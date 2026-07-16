package proxy

import "testing"

func TestResponsesLiteSSEBackfillsFunctionCall(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_2","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","sequence_number":1,"output_index":0,"item":{"id":"fc_1","type":"function_call","status":"in_progress","call_id":"call_1","name":"exit_plan_mode","arguments":""}}`,
		``,
		`event: response.function_call_arguments.delta`,
		`data: {"type":"response.function_call_arguments.delta","sequence_number":2,"item_id":"fc_1","output_index":0,"delta":"{\"plan\":"}`,
		``,
		`event: response.function_call_arguments.delta`,
		`data: {"type":"response.function_call_arguments.delta","sequence_number":3,"item_id":"fc_1","output_index":0,"delta":"\"ready\"}"}`,
		``,
		`event: response.function_call_arguments.done`,
		`data: {"type":"response.function_call_arguments.done","sequence_number":4,"item_id":"fc_1","output_index":0,"arguments":"{\"plan\":\"ready\"}"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","sequence_number":5,"response":{"id":"resp_2","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	response := findSSEEvent(t, raw, "response.completed")["response"].(map[string]any)
	calls := responseFunctionCalls(response)
	if len(calls) != 1 {
		t.Fatalf("function calls = %#v, stream=%s", calls, raw)
	}
	call := calls[0]
	if call["id"] != "fc_1" || call["call_id"] != "call_1" || call["name"] != "exit_plan_mode" {
		t.Fatalf("function call identity = %#v", call)
	}
	if call["arguments"] != `{"plan":"ready"}` {
		t.Fatalf("arguments = %#v", call["arguments"])
	}
}

func TestResponsesLiteSSEPreservesExistingFunctionCallWithoutDuplication(t *testing.T) {
	stream := sseStream(
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","sequence_number":1,"output_index":0,"item":{"id":"fc_1","type":"function_call","status":"in_progress","call_id":"call_1","name":"update_goal","arguments":""}}`,
		``,
		`event: response.function_call_arguments.done`,
		`data: {"type":"response.function_call_arguments.done","sequence_number":2,"item_id":"fc_1","output_index":0,"arguments":"{\"completed\":true}"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","sequence_number":3,"response":{"id":"resp_3","object":"response","status":"completed","model":"gpt-5.6-sol","output":[{"id":"fc_1","type":"function_call","status":"completed","call_id":"call_1","name":"update_goal","arguments":"{\"completed\":true}"}]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	response := findSSEEvent(t, raw, "response.completed")["response"].(map[string]any)
	calls := responseFunctionCalls(response)
	if len(calls) != 1 {
		t.Fatalf("function call duplicated: %#v", calls)
	}
}

func TestResponsesLiteSSESynthesizesFunctionCallBeforeDone(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_4","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","sequence_number":1,"output_index":0,"item":{"id":"fc_4","type":"function_call","status":"in_progress","call_id":"call_4","name":"read_file","arguments":""}}`,
		``,
		`event: response.function_call_arguments.done`,
		`data: {"type":"response.function_call_arguments.done","sequence_number":2,"item_id":"fc_4","output_index":0,"arguments":"{\"path\":\"README.md\"}"}`,
		``,
		`data: [DONE]`,
		``,
	)

	raw := normalizeSSE(t, stream)
	response := findSSEEvent(t, raw, "response.completed")["response"].(map[string]any)
	if len(responseFunctionCalls(response)) != 1 {
		t.Fatalf("missing synthesized function call: %s", raw)
	}
}

func TestResponsesLiteSSEFailsClosedForIncompleteFunctionCall(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_5","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","sequence_number":1,"output_index":0,"item":{"id":"fc_5","type":"function_call","status":"in_progress","call_id":"call_5","name":"write_file","arguments":""}}`,
		``,
		`event: response.function_call_arguments.delta`,
		`data: {"type":"response.function_call_arguments.delta","sequence_number":2,"item_id":"fc_5","output_index":0,"delta":"{\"path\":\"plan.md\""}`,
		``,
		`data: [DONE]`,
		``,
	)

	raw := normalizeSSE(t, stream)
	if countSSEEvent(t, raw, "response.completed") != 0 {
		t.Fatalf("incomplete call was completed: %s", raw)
	}
	errorEvent := findErrorPayload(t, raw)
	if errorEvent["type"] != "proxy_incomplete_output" {
		t.Fatalf("error = %#v", errorEvent)
	}
}

func TestResponsesLiteSSEFailsClosedForInvalidFunctionArguments(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_6","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","sequence_number":1,"output_index":0,"item":{"id":"fc_6","type":"function_call","status":"in_progress","call_id":"call_6","name":"shell","arguments":""}}`,
		``,
		`event: response.function_call_arguments.done`,
		`data: {"type":"response.function_call_arguments.done","sequence_number":2,"item_id":"fc_6","output_index":0,"arguments":"not-json"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","sequence_number":3,"response":{"id":"resp_6","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	if countSSEEvent(t, raw, "response.completed") != 0 {
		t.Fatalf("invalid call was completed: %s", raw)
	}
	findErrorPayload(t, raw)
}

func TestResponsesLiteSSEMergesMixedTextAndFunctionCall(t *testing.T) {
	stream := sseStream(
		`event: response.output_text.delta`,
		`data: {"type":"response.output_text.delta","sequence_number":1,"item_id":"msg_1","output_index":0,"delta":"계획을 준비했습니다."}`,
		``,
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","sequence_number":2,"output_index":1,"item":{"id":"fc_1","type":"function_call","status":"in_progress","call_id":"call_1","name":"exit_plan_mode","arguments":""}}`,
		``,
		`event: response.function_call_arguments.done`,
		`data: {"type":"response.function_call_arguments.done","sequence_number":3,"item_id":"fc_1","output_index":1,"arguments":"{}"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","sequence_number":4,"response":{"id":"resp_mix","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	response := findSSEEvent(t, raw, "response.completed")["response"].(map[string]any)
	if responseText(response) != "계획을 준비했습니다." || len(responseFunctionCalls(response)) != 1 {
		t.Fatalf("mixed output = %#v", response["output"])
	}
}

func TestResponsesLiteSSESupportsInterleavedFunctionCalls(t *testing.T) {
	stream := sseStream(
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","sequence_number":1,"output_index":0,"item":{"id":"fc_a","type":"function_call","status":"in_progress","call_id":"call_a","name":"read_file","arguments":""}}`,
		``,
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","sequence_number":2,"output_index":1,"item":{"id":"fc_b","type":"function_call","status":"in_progress","call_id":"call_b","name":"grep","arguments":""}}`,
		``,
		`event: response.function_call_arguments.delta`,
		`data: {"type":"response.function_call_arguments.delta","sequence_number":3,"item_id":"fc_a","output_index":0,"delta":"{\"path\":"}`,
		``,
		`event: response.function_call_arguments.delta`,
		`data: {"type":"response.function_call_arguments.delta","sequence_number":4,"item_id":"fc_b","output_index":1,"delta":"{\"query\":"}`,
		``,
		`event: response.function_call_arguments.done`,
		`data: {"type":"response.function_call_arguments.done","sequence_number":5,"item_id":"fc_b","output_index":1,"arguments":"{\"query\":\"TODO\"}"}`,
		``,
		`event: response.function_call_arguments.done`,
		`data: {"type":"response.function_call_arguments.done","sequence_number":6,"item_id":"fc_a","output_index":0,"arguments":"{\"path\":\"README.md\"}"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","sequence_number":7,"response":{"id":"resp_multi","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	response := findSSEEvent(t, raw, "response.completed")["response"].(map[string]any)
	calls := responseFunctionCalls(response)
	if len(calls) != 2 || calls[0]["name"] != "read_file" || calls[1]["name"] != "grep" {
		t.Fatalf("calls = %#v", calls)
	}
}

func TestResponsesLiteSSEUsesOutputItemDoneAsCanonicalFunctionCall(t *testing.T) {
	stream := sseStream(
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","sequence_number":1,"output_index":0,"item":{"id":"fc_done","type":"function_call","status":"in_progress","call_id":"call_done","name":"ask_user_question","arguments":""}}`,
		``,
		`event: response.output_item.done`,
		`data: {"type":"response.output_item.done","sequence_number":2,"output_index":0,"item":{"id":"fc_done","type":"function_call","status":"completed","call_id":"call_done","name":"ask_user_question","arguments":"{\"question\":\"Proceed?\"}"}}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","sequence_number":3,"response":{"id":"resp_done","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	calls := responseFunctionCalls(findSSEEvent(t, raw, "response.completed")["response"].(map[string]any))
	if len(calls) != 1 || calls[0]["arguments"] != `{"question":"Proceed?"}` {
		t.Fatalf("canonical done call = %#v", calls)
	}
}

func TestResponsesLiteSSEBackfillsCustomToolCall(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_custom","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","sequence_number":1,"output_index":0,"item":{"id":"ct_1","type":"custom_tool_call","status":"in_progress","call_id":"call_custom","name":"shell","input":""}}`,
		``,
		`event: response.custom_tool_call_input.done`,
		`data: {"type":"response.custom_tool_call_input.done","sequence_number":2,"item_id":"ct_1","output_index":0,"input":"pytest -q"}`,
		``,
		`data: [DONE]`,
		``,
	)

	raw := normalizeSSE(t, stream)
	response := findSSEEvent(t, raw, "response.completed")["response"].(map[string]any)
	output := jsonArray(response["output"])
	if len(output) != 1 {
		t.Fatalf("custom output = %#v", output)
	}
	item := jsonObject(output[0])
	if item["type"] != "custom_tool_call" || item["input"] != "pytest -q" {
		t.Fatalf("custom tool call = %#v", item)
	}
}
