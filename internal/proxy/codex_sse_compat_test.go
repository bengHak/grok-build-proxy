package proxy

import (
	"encoding/json"
	"io"
	"net/http"
	"strings"
	"testing"
)

func TestResponsesLiteSSEBackfillsCompletedText(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_1","object":"response","created_at":1,"status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_text.delta`,
		`data: {"type":"response.output_text.delta","sequence_number":1,"item_id":"msg_1","output_index":0,"content_index":0,"delta":"안녕"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","sequence_number":2,"response":{"id":"resp_1","object":"response","created_at":1,"status":"completed","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`data: [DONE]`,
		``,
	)

	raw := normalizeSSE(t, stream)
	completed := findSSEEvent(t, raw, "response.completed")
	response := completed["response"].(map[string]any)
	if got := responseText(response); got != "안녕" {
		t.Fatalf("completed text = %q, stream=%s", got, raw)
	}
	if countSSEEvent(t, raw, "response.completed") != 1 {
		t.Fatalf("completed event duplicated: %s", raw)
	}
}

func TestResponsesLiteSSEPreservesExistingCompletedText(t *testing.T) {
	stream := sseStream(
		`event: response.output_text.delta`,
		`data: {"type":"response.output_text.delta","sequence_number":1,"item_id":"msg_1","output_index":0,"content_index":0,"delta":"안녕"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","sequence_number":2,"response":{"id":"resp_1","object":"response","created_at":1,"status":"completed","model":"gpt-5.6-sol","output":[{"id":"msg_1","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"안녕","annotations":[],"logprobs":[]}] }]}}`,
		``,
	)

	raw := normalizeSSE(t, stream)
	completed := findSSEEvent(t, raw, "response.completed")
	response := completed["response"].(map[string]any)
	if got := responseText(response); got != "안녕" {
		t.Fatalf("completed text = %q", got)
	}
	output := response["output"].([]any)
	content := output[0].(map[string]any)["content"].([]any)
	if len(content) != 1 {
		t.Fatalf("content duplicated: %#v", content)
	}
}

func TestResponsesLiteSSESynthesizesCompletedBeforeDone(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_1","object":"response","created_at":1,"status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_text.delta`,
		`data: {"type":"response.output_text.delta","sequence_number":1,"item_id":"msg_1","output_index":0,"content_index":0,"delta":"안녕"}`,
		``,
		`data: [DONE]`,
		``,
	)

	raw := normalizeSSE(t, stream)
	completedAt := strings.Index(raw, `event: response.completed`)
	doneAt := strings.Index(raw, `data: [DONE]`)
	if completedAt < 0 || doneAt < 0 || completedAt > doneAt {
		t.Fatalf("completed was not synthesized before DONE: %s", raw)
	}
	completed := findSSEEvent(t, raw, "response.completed")
	if got := responseText(completed["response"].(map[string]any)); got != "안녕" {
		t.Fatalf("synthesized text = %q", got)
	}
}

func TestShouldNormalizeCodexSSEResponse(t *testing.T) {
	req, _ := http.NewRequest(http.MethodPost, "https://example.test/responses", nil)
	req.Header.Set(responsesLiteHeader, "true")
	resp := &http.Response{
		StatusCode: http.StatusOK,
		Header:     http.Header{"Content-Type": []string{"text/event-stream; charset=utf-8"}},
		Body:       io.NopCloser(strings.NewReader("")),
	}
	if !shouldNormalizeCodexSSEResponse(req, resp) {
		t.Fatal("lite SSE response was not selected")
	}
	req.Header.Del(responsesLiteHeader)
	if shouldNormalizeCodexSSEResponse(req, resp) {
		t.Fatal("non-lite response was selected")
	}
}

func normalizeSSE(t *testing.T, stream string) string {
	t.Helper()
	body := newResponsesLiteSSEBody(io.NopCloser(strings.NewReader(stream)))
	raw, err := io.ReadAll(body)
	if err != nil {
		t.Fatal(err)
	}
	return string(raw)
}

func sseStream(lines ...string) string {
	return strings.Join(lines, "\n")
}

func findSSEEvent(t *testing.T, stream, eventType string) map[string]any {
	t.Helper()
	for _, event := range decodeSSEEvents(t, stream) {
		if event["type"] == eventType {
			return event
		}
	}
	t.Fatalf("event %q not found in %s", eventType, stream)
	return nil
}

func findErrorPayload(t *testing.T, stream string) map[string]any {
	t.Helper()
	for _, event := range decodeSSEEvents(t, stream) {
		if body, ok := event["error"].(map[string]any); ok {
			return body
		}
	}
	t.Fatalf("error payload not found in %s", stream)
	return nil
}

func countSSEEvent(t *testing.T, stream, eventType string) int {
	t.Helper()
	count := 0
	for _, event := range decodeSSEEvents(t, stream) {
		if event["type"] == eventType {
			count++
		}
	}
	return count
}

func decodeSSEEvents(t *testing.T, stream string) []map[string]any {
	t.Helper()
	var events []map[string]any
	stream = strings.ReplaceAll(stream, "\r\n", "\n")
	for _, frame := range strings.Split(stream, "\n\n") {
		_, data, ok := parseSSEFrame([]byte(frame))
		if !ok || data == "[DONE]" {
			continue
		}
		var event map[string]any
		if err := json.Unmarshal([]byte(data), &event); err != nil {
			t.Fatalf("decode SSE event: %v: %q", err, data)
		}
		events = append(events, event)
	}
	return events
}

func responseText(response map[string]any) string {
	var text strings.Builder
	for _, rawItem := range jsonArray(response["output"]) {
		item := jsonObject(rawItem)
		for _, rawPart := range jsonArray(item["content"]) {
			part := jsonObject(rawPart)
			if part["type"] == "output_text" {
				text.WriteString(stringValue(part["text"]))
			}
		}
	}
	return text.String()
}

func responseFunctionCalls(response map[string]any) []map[string]any {
	var calls []map[string]any
	for _, rawItem := range jsonArray(response["output"]) {
		item := jsonObject(rawItem)
		if item["type"] == "function_call" {
			calls = append(calls, item)
		}
	}
	return calls
}
