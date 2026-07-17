package proxy

import (
	"bytes"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"strings"
	"testing"
)

func TestResponsesLiteSSENormalizesMinimalCodexResponseContract(t *testing.T) {
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_minimal"}}`,
		``,
		`event: response.output_text.delta`,
		`data: {"type":"response.output_text.delta","delta":"안녕하세요! 무엇을 도와드릴까요?"}`,
		``,
		`event: response.output_item.done`,
		`data: {"type":"response.output_item.done","item":{"type":"message","role":"assistant","id":"msg_minimal","content":[{"type":"output_text","text":"안녕하세요! 무엇을 도와드릴까요?"}]}}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_minimal","usage":{"input_tokens":12,"input_tokens_details":null,"output_tokens":8,"output_tokens_details":null,"total_tokens":20}}}`,
		``,
		`data: [DONE]`,
		``,
	)

	body := newResponsesLiteSSEBodyWithOptions(
		io.NopCloser(strings.NewReader(stream)),
		responsesLiteSSEOptions{
			Mode:      responsesCompatFull,
			Model:     "gpt-5.6-sol",
			RequestID: "request-minimal",
		},
	)
	rawBytes, err := io.ReadAll(body)
	if err != nil {
		t.Fatal(err)
	}
	raw := string(rawBytes)

	created := findSSEEvent(t, raw, "response.created")
	assertStrictResponseEnvelope(t, jsonObject(created["response"]), "in_progress", "gpt-5.6-sol")

	completed := findSSEEvent(t, raw, "response.completed")
	response := jsonObject(completed["response"])
	assertStrictResponseEnvelope(t, response, "completed", "gpt-5.6-sol")
	if got := responseText(response); got != "안녕하세요! 무엇을 도와드릴까요?" {
		t.Fatalf("completed text = %q, stream=%s", got, raw)
	}

	usage := jsonObject(response["usage"])
	inputDetails := jsonObject(usage["input_tokens_details"])
	outputDetails := jsonObject(usage["output_tokens_details"])
	if cached, ok := integerValue(inputDetails["cached_tokens"]); !ok || cached != 0 {
		t.Fatalf("cached_tokens = %#v", inputDetails["cached_tokens"])
	}
	if reasoning, ok := integerValue(outputDetails["reasoning_tokens"]); !ok || reasoning != 0 {
		t.Fatalf("reasoning_tokens = %#v", outputDetails["reasoning_tokens"])
	}

	output := jsonArray(response["output"])
	if len(output) != 1 {
		t.Fatalf("output = %#v", output)
	}
	message := jsonObject(output[0])
	if message["status"] != "completed" || message["role"] != "assistant" {
		t.Fatalf("message = %#v", message)
	}
	content := jsonArray(message["content"])
	if len(content) != 1 {
		t.Fatalf("content = %#v", content)
	}
	if _, ok := jsonObject(content[0])["annotations"].([]any); !ok {
		t.Fatalf("annotations = %#v", jsonObject(content[0])["annotations"])
	}
}

func TestInjectVisibleTerminalFallbackBuildsFinalMessage(t *testing.T) {
	frame := []byte("event: response.completed\n" +
		`data: {"type":"response.completed","response":{"id":"resp_fallback","object":"response","created_at":1,"model":"gpt-5.6-sol","status":"completed","output":[]}}` +
		"\n\n")

	normalized, applied := injectVisibleTerminalFallback(frame, "visible answer", "")
	if !applied {
		t.Fatal("visible fallback was not applied")
	}
	_, data, ok := parseSSEFrame(normalized)
	if !ok {
		t.Fatalf("invalid normalized frame: %s", normalized)
	}
	var event map[string]any
	if err := json.Unmarshal([]byte(data), &event); err != nil {
		t.Fatal(err)
	}
	response := jsonObject(event["response"])
	if got := responseText(response); got != "visible answer" {
		t.Fatalf("fallback text = %q", got)
	}
	if !responseHasUsableOutput(response) {
		t.Fatalf("fallback output unusable: %#v", response["output"])
	}
}

func TestShouldNormalizeCodexSSEResponseFallsBackToRequestAccept(t *testing.T) {
	req, _ := http.NewRequest(http.MethodPost, "https://example.test/responses", nil)
	req.Header.Set(responsesLiteHeader, "true")
	req.Header.Set("Accept", "text/event-stream")
	resp := &http.Response{
		StatusCode: http.StatusOK,
		Header:     http.Header{"Content-Type": []string{"application/octet-stream"}},
		Body:       io.NopCloser(strings.NewReader("")),
	}
	if !shouldNormalizeCodexSSEResponse(req, resp) {
		t.Fatal("lite streaming response was not selected from request Accept header")
	}
}

func TestResponsesLiteSSETraceLogsShapeWithoutModelContent(t *testing.T) {
	const secretText = "private-answer-must-not-appear-in-proxy-log"
	stream := sseStream(
		`event: response.created`,
		`data: {"type":"response.created","response":{"id":"resp_trace"}}`,
		``,
		`event: response.output_text.delta`,
		`data: {"type":"response.output_text.delta","delta":"`+secretText+`"}`,
		``,
		`event: response.completed`,
		`data: {"type":"response.completed","response":{"id":"resp_trace","usage":{"input_tokens":1,"input_tokens_details":null,"output_tokens":1,"output_tokens_details":null,"total_tokens":2}}}`,
		``,
	)

	var logs bytes.Buffer
	logger := slog.New(slog.NewTextHandler(&logs, nil))
	body := newResponsesLiteSSEBodyWithOptions(
		io.NopCloser(strings.NewReader(stream)),
		responsesLiteSSEOptions{
			Mode:      responsesCompatFull,
			Model:     "gpt-5.6-sol",
			RequestID: "request-trace",
			Logger:    logger,
			Trace:     true,
		},
	)
	if _, err := io.ReadAll(body); err != nil {
		t.Fatal(err)
	}
	logText := logs.String()
	if !strings.Contains(logText, "response.completed") {
		t.Fatalf("trace missing terminal event: %s", logText)
	}
	if !strings.Contains(logText, "response.usage.input_tokens_details") {
		t.Fatalf("trace missing normalized usage fields: %s", logText)
	}
	if !strings.Contains(logText, "normalized_usable_output=true") {
		t.Fatalf("trace missing usable terminal summary: %s", logText)
	}
	if strings.Contains(logText, secretText) {
		t.Fatalf("trace leaked model content: %s", logText)
	}
}

func TestCodexRequestStringReadsNormalizedRequestBody(t *testing.T) {
	req, err := http.NewRequest(
		http.MethodPost,
		"https://chatgpt.com/backend-api/codex/responses",
		strings.NewReader(`{"model":"gpt-5.6-sol","input":"hello","stream":true}`),
	)
	if err != nil {
		t.Fatal(err)
	}
	if err := normalizeCodexHTTPRequest(req, DefaultCodexCompatibilityVersion); err != nil {
		t.Fatal(err)
	}
	if got := codexRequestString(req, "model"); got != "gpt-5.6-sol" {
		t.Fatalf("model = %q", got)
	}
}

func assertStrictResponseEnvelope(t *testing.T, response map[string]any, status, model string) {
	t.Helper()
	if response == nil {
		t.Fatal("response is nil")
	}
	if stringValue(response["id"]) == "" {
		t.Fatalf("missing response id: %#v", response)
	}
	if response["object"] != "response" {
		t.Fatalf("object = %#v", response["object"])
	}
	if response["model"] != model {
		t.Fatalf("model = %#v", response["model"])
	}
	if response["status"] != status {
		t.Fatalf("status = %#v", response["status"])
	}
	if createdAt, ok := integerValue(response["created_at"]); !ok || createdAt < 0 {
		t.Fatalf("created_at = %#v", response["created_at"])
	}
	if _, ok := response["output"].([]any); !ok {
		t.Fatalf("output = %#v", response["output"])
	}
}
