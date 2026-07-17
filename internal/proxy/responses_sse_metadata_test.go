package proxy

import (
	"bytes"
	"io"
	"log/slog"
	"strings"
	"testing"
)

func TestResponsesLiteSSEDropsCodexResponseMetadata(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_metadata","object":"response","created_at":1,"status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.metadata`,
		`data: {"type":"response.metadata","sequence_number":1,"headers":{"x-codex-turn-state":"opaque-state"},"metadata":{"openai_verification_recommendation":[]}}`,
		``,
		`event: response.output_text.delta`,
		`data: {"type":"response.output_text.delta","sequence_number":2,"item_id":"msg_metadata","output_index":0,"content_index":0,"delta":"안녕"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","sequence_number":3,"response":{"id":"resp_metadata","object":"response","created_at":1,"status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	if strings.Contains(raw, `response.metadata`) || strings.Contains(raw, `opaque-state`) {
		t.Fatalf("private metadata leaked downstream: %s", raw)
	}
	completed := findSSEEvent(t, raw, "response.completed")
	if got := responseText(completed["response"].(map[string]any)); got != "안녕" {
		t.Fatalf("completed text = %q, stream=%s", got, raw)
	}
}

func TestResponsesLiteSSEDropsMetadataByEventNameOrJSONType(t *testing.T) {
	tests := []struct {
		name   string
		frame  string
		marker string
	}{
		{
			name:   "event name without JSON type",
			frame:  sseStream(`event: response.metadata`, `data: {"sequence_number":1,"private":"event-only"}`, ``),
			marker: "event-only",
		},
		{
			name:   "JSON type without event name",
			frame:  sseStream(`data: {"type":"response.metadata","sequence_number":1,"private":"type-only"}`, ``),
			marker: "type-only",
		},
		{
			name:   "event name with malformed payload",
			frame:  sseStream(`event: response.metadata`, `data: malformed-private-payload`, ``),
			marker: "malformed-private-payload",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			stream := sseStream(
				`event: response.created`,
				`data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_metadata_shape","object":"response","created_at":1,"status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
				``,
				tt.frame,
				`event: response.output_text.delta`,
				`data: {"type":"response.output_text.delta","sequence_number":2,"item_id":"msg_metadata_shape","output_index":0,"content_index":0,"delta":"ok"}`,
				``,
				`event: response.completed`,
				`data: {"type":"response.completed","sequence_number":3,"response":{"id":"resp_metadata_shape","object":"response","created_at":1,"status":"completed","model":"gpt-5.6-sol","output":[]}}`,
				``,
			)

			raw := normalizeSSE(t, stream)
			if strings.Contains(raw, `response.metadata`) || strings.Contains(raw, tt.marker) {
				t.Fatalf("private metadata leaked downstream: %s", raw)
			}
			findSSEEvent(t, raw, "response.completed")
		})
	}
}

func TestResponsesLiteSSEDroppedMetadataDoesNotMutateVisibleFallback(t *testing.T) {
	const privateText = "private-metadata-must-not-become-output"
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_metadata_conflict","object":"response","created_at":1,"status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.metadata`,
		`data: {"type":"response.output_text.delta","sequence_number":1,"item_id":"private_item","output_index":0,"content_index":0,"delta":"`+privateText+`"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","sequence_number":2,"response":{"id":"resp_metadata_conflict","object":"response","created_at":1,"status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	if strings.Contains(raw, privateText) {
		t.Fatalf("private metadata became downstream output: %s", raw)
	}
}

func TestResponsesLiteSSEDroppedMetadataTraceDoesNotInspectPayload(t *testing.T) {
	const privateValue = "private-status-must-not-appear-in-trace"
	stream := sseStream(
		`event: response.metadata`,
		`data: {"type":"response.metadata","response":{"status":"`+privateValue+`"}}`,
		``,
	)

	var logs bytes.Buffer
	body := newResponsesLiteSSEBodyWithOptions(
		io.NopCloser(strings.NewReader(stream)),
		responsesLiteSSEOptions{
			Mode:      responsesCompatFull,
			RequestID: "request-private-metadata",
			Logger:    slog.New(slog.NewTextHandler(&logs, nil)),
			Trace:     true,
		},
	)
	if _, err := io.ReadAll(body); err != nil {
		t.Fatal(err)
	}
	logText := logs.String()
	if !strings.Contains(logText, "private_event.dropped") {
		t.Fatalf("trace missing dropped event marker: %s", logText)
	}
	if strings.Contains(logText, privateValue) {
		t.Fatalf("trace inspected private metadata payload: %s", logText)
	}
}

func TestResponsesLiteSSEDropsMetadataBeforePlanFunctionCall(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_plan","object":"response","created_at":1,"status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.metadata`,
		`data: {"type":"response.metadata","sequence_number":1,"headers":{"x-codex-turn-state":"plan-turn-state"}}`,
		``,
		`event: response.output_item.done`,
		`data: {"type":"response.output_item.done","sequence_number":2,"output_index":0,"item":{"id":"fc_plan","type":"function_call","call_id":"call_plan","name":"exit_plan_mode","arguments":"{\"plan\":\"ship it\"}","status":"completed"}}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","sequence_number":3,"response":{"id":"resp_plan","object":"response","created_at":1,"status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	if strings.Contains(raw, `response.metadata`) || strings.Contains(raw, `plan-turn-state`) {
		t.Fatalf("private metadata leaked downstream: %s", raw)
	}
	completed := findSSEEvent(t, raw, "response.completed")
	calls := responseFunctionCalls(completed["response"].(map[string]any))
	if len(calls) != 1 {
		t.Fatalf("function calls = %#v, stream=%s", calls, raw)
	}
	if calls[0]["name"] != "exit_plan_mode" || calls[0]["call_id"] != "call_plan" {
		t.Fatalf("function call = %#v", calls[0])
	}
	if calls[0]["arguments"] != `{"plan":"ship it"}` {
		t.Fatalf("arguments = %#v", calls[0]["arguments"])
	}
}
