package proxy

import (
	"io"
	"strings"
	"testing"
)

func TestResponsesLiteSSEPreservesErrorTerminalWithoutSynthesis(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_error","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: error`,
		`data: {"error":{"type":"server_error","message":"boom"}}`,
		``,
		`data: [DONE]`,
		``,
	)

	raw := normalizeSSE(t, stream)
	if countSSEEvent(t, raw, "response.completed") != 0 {
		t.Fatalf("error stream synthesized completion: %s", raw)
	}
	if body := findErrorPayload(t, raw); body["message"] != "boom" {
		t.Fatalf("error payload = %#v", body)
	}
}

func TestParseSSEFrameJoinsMultilineDataAndCRLF(t *testing.T) {
	frame := []byte("event: response.test\r\ndata: {\"a\":\r\ndata: 1}\r\n\r\n")
	event, data, ok := parseSSEFrame(frame)
	if !ok || event != "response.test" || data != "{\"a\":\n1}" {
		t.Fatalf("event=%q data=%q ok=%v", event, data, ok)
	}
}

func TestResponsesLiteSSEDoesNotUpgradeIncompleteResponse(t *testing.T) {
	stream := sseStream(
		`event: response.output_text.delta`,
		`data: {"type":"response.output_text.delta","sequence_number":1,"item_id":"msg_1","output_index":0,"delta":"partial"}`,
		``,
		`event: response.incomplete`,
		`data: {"type":"response.incomplete","sequence_number":2,"response":{"id":"resp_partial","object":"response","status":"incomplete","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`data: [DONE]`,
		``,
	)

	raw := normalizeSSE(t, stream)
	if countSSEEvent(t, raw, "response.completed") != 0 {
		t.Fatalf("incomplete response was upgraded: %s", raw)
	}
	response := findSSEEvent(t, raw, "response.incomplete")["response"].(map[string]any)
	if response["status"] != "incomplete" {
		t.Fatalf("status = %#v", response["status"])
	}
}

func TestResponsesLiteSSEPreservesFailedTerminal(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_failed","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.failed`,
		`data: {"type":"response.failed","sequence_number":1,"response":{"id":"resp_failed","object":"response","status":"failed","model":"gpt-5.6-sol","output":[],"error":{"code":"server_error","message":"boom"}}}`,
		``,
		`data: [DONE]`,
		``,
	)

	raw := normalizeSSE(t, stream)
	if countSSEEvent(t, raw, "response.failed") != 1 || countSSEEvent(t, raw, "response.completed") != 0 {
		t.Fatalf("failed terminal changed: %s", raw)
	}
}

func TestResponsesLiteSSETextModeDoesNotSynthesizeFunctionCalls(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_text_mode","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_item.added`,
		`data: {"type":"response.output_item.added","sequence_number":1,"output_index":0,"item":{"id":"fc_1","type":"function_call","status":"in_progress","call_id":"call_1","name":"shell","arguments":""}}`,
		``,
		`event: response.function_call_arguments.done`,
		`data: {"type":"response.function_call_arguments.done","sequence_number":2,"item_id":"fc_1","output_index":0,"arguments":"{\"command\":\"true\"}"}`,
		``,
		`data: [DONE]`,
		``,
	)

	body := newResponsesLiteSSEBodyWithMode(io.NopCloser(strings.NewReader(stream)), responsesCompatText)
	data, err := io.ReadAll(body)
	if err != nil {
		t.Fatal(err)
	}
	raw := string(data)
	if countSSEEvent(t, raw, "response.completed") != 0 {
		t.Fatalf("text mode reconstructed a function call: %s", raw)
	}
	findErrorPayload(t, raw)
}
