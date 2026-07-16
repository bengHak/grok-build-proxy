package proxy

import "testing"

func TestResponsesLiteSSERebindsSyntheticMessageID(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_rebind","object":"response","status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_text.delta`,
		`data: {"type":"response.output_text.delta","delta":"hel"}`,
		``,
		`event: response.output_text.delta`,
		`data: {"type":"response.output_text.delta","delta":"lo"}`,
		``,
		`event: response.output_item.done`,
		`data: {"type":"response.output_item.done","item":{"id":"msg_real","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"hello","annotations":[]}]}}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_rebind","object":"response","status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
	)

	response := findSSEEvent(t, normalizeSSE(t, stream), "response.completed")["response"].(map[string]any)
	output := jsonArray(response["output"])
	if len(output) != 1 {
		t.Fatalf("output = %#v", output)
	}
	item := jsonObject(output[0])
	if item["id"] != "msg_real" || responseText(response) != "hello" {
		t.Fatalf("item = %#v", item)
	}
}
