package proxy

import (
	"strings"
	"testing"
)

func TestResponsesLiteSSEDoesNotForwardCodexKeepalive(t *testing.T) {
	// Given a Codex stream containing the transport-only heartbeat seen by Grok.
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_keepalive","object":"response","created_at":1,"status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`data: {"type":"keepalive"}`,
		``,
		`event: response.output_text.delta`,
		`data: {"type":"response.output_text.delta","sequence_number":1,"item_id":"msg_keepalive","output_index":0,"content_index":0,"delta":"ok"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","sequence_number":2,"response":{"id":"resp_keepalive","object":"response","created_at":1,"status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	// When the Responses Lite compatibility body is consumed.
	raw := normalizeSSE(t, stream)

	// Then the heartbeat remains an SSE comment and never reaches the JSON decoder.
	for _, event := range decodeSSEEvents(t, raw) {
		if event["type"] == "keepalive" {
			t.Fatalf("strict Responses client received unsupported keepalive event: %s", raw)
		}
	}
	if !strings.Contains(raw, ": keepalive\n\n") {
		t.Fatalf("keepalive heartbeat was not preserved as an SSE comment: %s", raw)
	}
	findSSEEvent(t, raw, "response.completed")
}
