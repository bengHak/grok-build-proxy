package proxy

import (
	"bytes"
	"log/slog"
	"strings"
	"testing"
)

func TestResponsesLiteSSEFallsBackToTextDeltaWhenDoneIsEmpty(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_text_done","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_text.delta`,
		`data: {"type":"response.output_text.delta","delta":"오이오이! 무엇을 도와드릴까요?"}`,
		``,
		`event: response.output_text.done`,
		`data: {"type":"response.output_text.done","text":""}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_text_done","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	if countSSEEvent(t, raw, "response.completed") != 1 {
		t.Fatalf("completed count mismatch: %s", raw)
	}
	response := findSSEEvent(t, raw, "response.completed")["response"].(map[string]any)
	if got := responseText(response); got != "오이오이! 무엇을 도와드릴까요?" {
		t.Fatalf("text = %q, stream=%s", got, raw)
	}
	done := findSSEEvent(t, raw, "response.output_text.done")
	if done["text"] != "오이오이! 무엇을 도와드릴까요?" {
		t.Fatalf("done text = %#v", done["text"])
	}
}

func TestResponsesLiteSSEFallsBackToRefusalDeltaWhenDoneIsEmpty(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_refusal_done","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.refusal.delta`,
		`data: {"type":"response.refusal.delta","delta":"요청을 처리할 수 없습니다."}`,
		``,
		`event: response.refusal.done`,
		`data: {"type":"response.refusal.done","refusal":""}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_refusal_done","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	response := findSSEEvent(t, raw, "response.completed")["response"].(map[string]any)
	if got := responseRefusal(response); got != "요청을 처리할 수 없습니다." {
		t.Fatalf("refusal = %q, stream=%s", got, raw)
	}
	done := findSSEEvent(t, raw, "response.refusal.done")
	if done["refusal"] != "요청을 처리할 수 없습니다." {
		t.Fatalf("done refusal = %#v", done["refusal"])
	}
}

func TestResponsesLiteSSEFallsBackToFunctionArgumentDeltasWhenDoneIsEmpty(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_plan_done","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_plan_done","name":"exit_plan_mode","arguments":""}}`,
		``,
		`event: response.function_call_arguments.delta`,
		`data: {"type":"response.function_call_arguments.delta","call_id":"call_plan_done","delta":"{\"plan\":\"ready\"}"}`,
		``,
		`event: response.function_call_arguments.done`,
		`data: {"type":"response.function_call_arguments.done","call_id":"call_plan_done","arguments":""}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_plan_done","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	calls := responseFunctionCalls(findSSEEvent(t, raw, "response.completed")["response"].(map[string]any))
	if len(calls) != 1 || calls[0]["arguments"] != `{"plan":"ready"}` {
		t.Fatalf("calls = %#v, stream=%s", calls, raw)
	}
	done := findSSEEvent(t, raw, "response.function_call_arguments.done")
	if done["arguments"] != `{"plan":"ready"}` {
		t.Fatalf("done arguments = %#v", done["arguments"])
	}
}

func TestResponsesLiteSSEUsesValidFunctionDeltaWhenDoneIsInvalid(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_goal_done","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_goal_done","name":"update_goal","arguments":""}}`,
		``,
		`event: response.function_call_arguments.delta`,
		`data: {"type":"response.function_call_arguments.delta","call_id":"call_goal_done","delta":"{\"completed\":true}"}`,
		``,
		`event: response.function_call_arguments.done`,
		`data: {"type":"response.function_call_arguments.done","call_id":"call_goal_done","arguments":"not-json"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_goal_done","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	calls := responseFunctionCalls(findSSEEvent(t, raw, "response.completed")["response"].(map[string]any))
	if len(calls) != 1 || calls[0]["arguments"] != `{"completed":true}` {
		t.Fatalf("calls = %#v, stream=%s", calls, raw)
	}
}

func TestResponsesLiteSSEStillRequiresFunctionCompletionBoundary(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_no_done","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_no_done","name":"write_file","arguments":""}}`,
		``,
		`event: response.function_call_arguments.delta`,
		`data: {"type":"response.function_call_arguments.delta","call_id":"call_no_done","delta":"{\"path\":\"plan.md\"}"}`,
		``,
		`data: [DONE]`,
		``,
	)

	raw := normalizeSSE(t, stream)
	if countSSEEvent(t, raw, "response.completed") != 0 {
		t.Fatalf("unconfirmed function call completed: %s", raw)
	}
	if body := findErrorPayload(t, raw); body["type"] != "proxy_incomplete_output" {
		t.Fatalf("error = %#v", body)
	}
}

func TestResponsesLiteSSEFallsBackToCustomInputDeltaWhenDoneIsEmpty(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_custom_done","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","item":{"type":"custom_tool_call","call_id":"call_shell_done","name":"shell","input":""}}`,
		``,
		`event: response.custom_tool_call_input.delta`,
		`data: {"type":"response.custom_tool_call_input.delta","call_id":"call_shell_done","delta":"go test ./..."}`,
		``,
		`event: response.custom_tool_call_input.done`,
		`data: {"type":"response.custom_tool_call_input.done","call_id":"call_shell_done","input":""}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_custom_done","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	calls := responseCustomToolCalls(findSSEEvent(t, raw, "response.completed")["response"].(map[string]any))
	if len(calls) != 1 || calls[0]["input"] != "go test ./..." {
		t.Fatalf("calls = %#v, stream=%s", calls, raw)
	}
	done := findSSEEvent(t, raw, "response.custom_tool_call_input.done")
	if done["input"] != "go test ./..." {
		t.Fatalf("done input = %#v", done["input"])
	}
}

func TestResponsesLiteSSEAllowsExplicitlyEmptyCustomInput(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_custom_empty","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","item":{"type":"custom_tool_call","call_id":"call_custom_empty","name":"noop","input":""}}`,
		``,
		`event: response.custom_tool_call_input.done`,
		`data: {"type":"response.custom_tool_call_input.done","call_id":"call_custom_empty","input":""}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_custom_empty","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	calls := responseCustomToolCalls(findSSEEvent(t, raw, "response.completed")["response"].(map[string]any))
	if len(calls) != 1 {
		t.Fatalf("calls = %#v, stream=%s", calls, raw)
	}
	if input, ok := calls[0]["input"].(string); !ok || input != "" {
		t.Fatalf("input = %#v", calls[0]["input"])
	}
}

func TestResponsesLiteSSEFailsClosedWhenCustomDoneOmitsInput(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_custom_missing","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","item":{"type":"custom_tool_call","call_id":"call_custom_missing","name":"shell","input":""}}`,
		``,
		`event: response.custom_tool_call_input.done`,
		`data: {"type":"response.custom_tool_call_input.done","call_id":"call_custom_missing"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_custom_missing","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	if countSSEEvent(t, raw, "response.completed") != 0 {
		t.Fatalf("missing custom input completed: %s", raw)
	}
	if body := findErrorPayload(t, raw); body["type"] != "proxy_incomplete_output" {
		t.Fatalf("error = %#v", body)
	}
}

func TestResponsesLiteSSELogsProxyNormalizationFailure(t *testing.T) {
	var logs bytes.Buffer
	previous := slog.Default()
	slog.SetDefault(slog.New(slog.NewTextHandler(&logs, nil)))
	defer slog.SetDefault(previous)

	raw := normalizeSSE(t, sseStream(`data: [DONE]`, ``))
	if body := findErrorPayload(t, raw); body["type"] != "proxy_missing_terminal_response" {
		t.Fatalf("error = %#v", body)
	}
	if !strings.Contains(logs.String(), "proxy_missing_terminal_response") || !strings.Contains(logs.String(), "output_states=0") {
		t.Fatalf("log = %q", logs.String())
	}
}

func responseRefusal(response map[string]any) string {
	var refusal strings.Builder
	for _, rawItem := range jsonArray(response["output"]) {
		item := jsonObject(rawItem)
		for _, rawPart := range jsonArray(item["content"]) {
			part := jsonObject(rawPart)
			if part["type"] == "refusal" {
				refusal.WriteString(stringValue(part["refusal"]))
			}
		}
	}
	return refusal.String()
}

func responseCustomToolCalls(response map[string]any) []map[string]any {
	var calls []map[string]any
	for _, rawItem := range jsonArray(response["output"]) {
		item := jsonObject(rawItem)
		if item["type"] == "custom_tool_call" {
			calls = append(calls, item)
		}
	}
	return calls
}
