package proxy

import (
	"encoding/json"
	"io"
	"net/http"
	"strings"
	"testing"
)

func TestResponsesLiteSSEBackfillsCompletedText(t *testing.T) {
	stream := strings.Join([]string{
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
	}, "\n")

	body := newResponsesLiteSSEBody(io.NopCloser(strings.NewReader(stream)))
	raw, err := io.ReadAll(body)
	if err != nil {
		t.Fatal(err)
	}
	completed := findSSEEvent(t, string(raw), "response.completed")
	response := completed["response"].(map[string]any)
	if got := responseText(response); got != "안녕" {
		t.Fatalf("completed text = %q, stream=%s", got, raw)
	}
	if countSSEEvent(t, string(raw), "response.completed") != 1 {
		t.Fatalf("completed event duplicated: %s", raw)
	}
}

func TestResponsesLiteSSEPreservesExistingCompletedText(t *testing.T) {
	stream := strings.Join([]string{
		`event: response.output_text.delta`,
		`data: {"type":"response.output_text.delta","sequence_number":1,"item_id":"msg_1","output_index":0,"content_index":0,"delta":"안녕"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","sequence_number":2,"response":{"id":"resp_1","object":"response","created_at":1,"status":"completed","model":"gpt-5.6-sol","output":[{"id":"msg_1","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"안녕","annotations":[],"logprobs":[]}]}]}}`,
		``,
	}, "\n")

	body := newResponsesLiteSSEBody(io.NopCloser(strings.NewReader(stream)))
	raw, err := io.ReadAll(body)
	if err != nil {
		t.Fatal(err)
	}
	completed := findSSEEvent(t, string(raw), "response.completed")
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
	stream := strings.Join([]string{
		`event: response.created`,
		`data: {"type":"response.created","sequence_number":0,"response":{"id":"resp_1","object":"response","created_at":1,"status":"in_progress","model":"gpt-5.6-sol","output":[]}}`,
		``,
		`event: response.output_text.delta`,
		`data: {"type":"response.output_text.delta","sequence_number":1,"item_id":"msg_1","output_index":0,"content_index":0,"delta":"안녕"}`,
		``,
		`data: [DONE]`,
		``,
	}, "\n")

	body := newResponsesLiteSSEBody(io.NopCloser(strings.NewReader(stream)))
	raw, err := io.ReadAll(body)
	if err != nil {
		t.Fatal(err)
	}
	text := string(raw)
	completedAt := strings.Index(text, `event: response.completed`)
	doneAt := strings.Index(text, `data: [DONE]`)
	if completedAt < 0 || doneAt < 0 || completedAt > doneAt {
		t.Fatalf("completed was not synthesized before DONE: %s", raw)
	}
	completed := findSSEEvent(t, text, "response.completed")
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
	output, _ := response["output"].([]any)
	var text strings.Builder
	for _, rawItem := range output {
		item, _ := rawItem.(map[string]any)
		content, _ := item["content"].([]any)
		for _, rawPart := range content {
			part, _ := rawPart.(map[string]any)
			if part["type"] == "output_text" {
				value, _ := part["text"].(string)
				text.WriteString(value)
			}
		}
	}
	return text.String()
}
