package proxy

import "testing"

func TestResponsesLiteSSEBackfillsIndexlessTextDelta(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_text","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_text.delta`,
		`data: {"type":"response.output_text.delta","delta":"네, 듣고 있어요!"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_text","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`data: [DONE]`,
		``,
	)

	raw := normalizeSSE(t, stream)
	if countSSEEvent(t, raw, "response.completed") != 1 {
		t.Fatalf("completed count mismatch: %s", raw)
	}
	response := findSSEEvent(t, raw, "response.completed")["response"].(map[string]any)
	if got := responseText(response); got != "네, 듣고 있어요!" {
		t.Fatalf("text = %q, stream=%s", got, raw)
	}
}

func TestResponsesLiteSSECapturesIndexlessMessageDone(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_message","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.done`,
		`data: {"type":"response.output_item.done","item":{"type":"message","role":"assistant","id":"msg_message","content":[{"type":"output_text","text":"완료했습니다."}]}}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_message","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	response := findSSEEvent(t, normalizeSSE(t, stream), "response.completed")["response"].(map[string]any)
	if got := responseText(response); got != "완료했습니다." {
		t.Fatalf("text = %q", got)
	}
}

func TestResponsesLiteSSECapturesIndexlessPlanCall(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_plan","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.done`,
		`data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_plan","name":"exit_plan_mode","arguments":"{\"plan\":\"ready\"}"}}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_plan","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	response := findSSEEvent(t, normalizeSSE(t, stream), "response.completed")["response"].(map[string]any)
	calls := responseFunctionCalls(response)
	if len(calls) != 1 {
		t.Fatalf("calls = %#v", calls)
	}
	call := calls[0]
	if call["id"] != "fc_call_plan" || call["call_id"] != "call_plan" || call["name"] != "exit_plan_mode" {
		t.Fatalf("identity = %#v", call)
	}
	if call["arguments"] != `{"plan":"ready"}` || call["status"] != "completed" {
		t.Fatalf("call = %#v", call)
	}
}

func TestResponsesLiteSSEPreservesIndexlessMixedOutputOrder(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_mix","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_text.delta`,
		`data: {"type":"response.output_text.delta","delta":"계획을 준비했습니다."}`,
		``,
		`event: response.output_item.done`,
		`data: {"type":"response.output_item.done","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"계획을 준비했습니다."}]}}`,
		``,
		`event: response.output_item.done`,
		`data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_exit","name":"exit_plan_mode","arguments":"{}"}}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_mix","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	response := findSSEEvent(t, normalizeSSE(t, stream), "response.completed")["response"].(map[string]any)
	output := jsonArray(response["output"])
	if len(output) != 2 {
		t.Fatalf("output = %#v", output)
	}
	if jsonObject(output[0])["type"] != "message" || jsonObject(output[1])["type"] != "function_call" {
		t.Fatalf("order = %#v", output)
	}
}

func TestResponsesLiteSSEKeepsIndexlessGoalCallsDistinct(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_goal","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.done`,
		`data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_read","name":"read_file","arguments":"{\"path\":\"README.md\"}"}}`,
		``,
		`event: response.output_item.done`,
		`data: {"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_goal","name":"update_goal","arguments":"{\"completed\":true}"}}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_goal","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	response := findSSEEvent(t, normalizeSSE(t, stream), "response.completed")["response"].(map[string]any)
	calls := responseFunctionCalls(response)
	if len(calls) != 2 || calls[0]["call_id"] != "call_read" || calls[1]["call_id"] != "call_goal" {
		t.Fatalf("calls = %#v", calls)
	}
}

func TestResponsesLiteSSEMapsIndexlessArgumentsByCallID(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_args","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_question","name":"ask_user_question","arguments":""}}`,
		``,
		`event: response.function_call_arguments.delta`,
		`data: {"type":"response.function_call_arguments.delta","call_id":"call_question","delta":"{\"question\":"}`,
		``,
		`event: response.function_call_arguments.done`,
		`data: {"type":"response.function_call_arguments.done","call_id":"call_question","arguments":"{\"question\":\"Proceed?\"}"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_args","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	response := findSSEEvent(t, normalizeSSE(t, stream), "response.completed")["response"].(map[string]any)
	calls := responseFunctionCalls(response)
	if len(calls) != 1 || calls[0]["arguments"] != `{"question":"Proceed?"}` {
		t.Fatalf("calls = %#v", calls)
	}
}

func TestResponsesLiteSSECapturesIndexlessCustomToolCall(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_custom","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.done`,
		`data: {"type":"response.output_item.done","item":{"type":"custom_tool_call","call_id":"call_shell","name":"shell","input":"go test ./..."}}`,
		``,
		`data: [DONE]`,
		``,
	)

	response := findSSEEvent(t, normalizeSSE(t, stream), "response.completed")["response"].(map[string]any)
	output := jsonArray(response["output"])
	if len(output) != 1 {
		t.Fatalf("output = %#v", output)
	}
	item := jsonObject(output[0])
	if item["id"] != "ct_call_shell" || item["input"] != "go test ./..." {
		t.Fatalf("item = %#v", item)
	}
}

func TestResponsesLiteSSENormalizesIndexlessTextEnvelope(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_wire","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_text.delta`,
		`data: {"type":"response.output_text.delta","delta":"hello"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_wire","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	delta := findSSEEvent(t, raw, "response.output_text.delta")
	if _, ok := integerValue(delta["sequence_number"]); !ok {
		t.Fatalf("missing sequence number: %#v", delta)
	}
	if delta["item_id"] != "msg_wire_0" {
		t.Fatalf("item_id = %#v", delta["item_id"])
	}
	if index, ok := integerValue(delta["output_index"]); !ok || index != 0 {
		t.Fatalf("output_index = %#v", delta["output_index"])
	}
	if index, ok := integerValue(delta["content_index"]); !ok || index != 0 {
		t.Fatalf("content_index = %#v", delta["content_index"])
	}
}

func TestResponsesLiteSSENormalizesIndexlessToolEnvelope(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_tool_wire","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_wire","name":"update_goal","arguments":""}}`,
		``,
		`event: response.function_call_arguments.delta`,
		`data: {"type":"response.function_call_arguments.delta","delta":"{\"completed\":"}`,
		``,
		`event: response.function_call_arguments.done`,
		`data: {"type":"response.function_call_arguments.done","arguments":"{\"completed\":true}"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_tool_wire","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	added := findSSEEvent(t, raw, "response.output_item.added")
	item := jsonObject(added["item"])
	if item["id"] != "fc_call_wire" {
		t.Fatalf("item = %#v", item)
	}
	if index, ok := integerValue(added["output_index"]); !ok || index != 0 {
		t.Fatalf("added output_index = %#v", added["output_index"])
	}

	for _, eventType := range []string{
		"response.function_call_arguments.delta",
		"response.function_call_arguments.done",
	} {
		event := findSSEEvent(t, raw, eventType)
		if event["item_id"] != "fc_call_wire" {
			t.Fatalf("%s item_id = %#v", eventType, event["item_id"])
		}
		if index, ok := integerValue(event["output_index"]); !ok || index != 0 {
			t.Fatalf("%s output_index = %#v", eventType, event["output_index"])
		}
		if _, ok := integerValue(event["sequence_number"]); !ok {
			t.Fatalf("%s missing sequence number: %#v", eventType, event)
		}
	}
}

func TestResponsesLiteSSENormalizesMessageDoneContent(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_message_wire","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.done`,
		`data: {"type":"response.output_item.done","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_message_wire","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	done := findSSEEvent(t, raw, "response.output_item.done")
	item := jsonObject(done["item"])
	if item["id"] != "msg_message_wire_0" || item["status"] != "completed" {
		t.Fatalf("item = %#v", item)
	}
	content := jsonArray(item["content"])
	if len(content) != 1 {
		t.Fatalf("content = %#v", content)
	}
	if _, ok := jsonObject(content[0])["annotations"].([]any); !ok {
		t.Fatalf("annotations = %#v", jsonObject(content[0])["annotations"])
	}
}
