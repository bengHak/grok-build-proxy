package proxy

import (
	"encoding/json"
	"io"
	"net/http"
	"strings"
	"testing"
)

func TestNormalizeCodexHTTPRequest(t *testing.T) {
	body := `{
      "model":"gpt-5.6-sol",
      "input":[{"id":null,"type":"message","role":"user","content":[{"type":"input_image","image_url":"data:image/png;base64,AA==","detail":"high"}]}],
      "client_metadata":{"ws_request_header_x_openai_internal_codex_responses_lite":"true","keep":"yes"},
      "temperature":0.2,
      "max_output_tokens":100,
      "stream":true
    }`
	req, err := http.NewRequest(http.MethodPost, "https://chatgpt.com/backend-api/codex/responses", strings.NewReader(body))
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("OpenAI-Beta", "responses=experimental")
	req.Header.Set("session_id", "session-1")
	req.Header.Set("x-client-request-id", "session-1")
	req.Header.Set("x-codex-window-id", "session-1:0")
	req.Header.Set("X-OpenAI-Internal-Codex-Responses-Lite", "true")

	if err := normalizeCodexHTTPRequest(req, "0.144.0"); err != nil {
		t.Fatal(err)
	}
	if got := req.Header.Get("session-id"); got != "session-1" {
		t.Fatalf("session-id = %q", got)
	}
	if got := req.Header.Get("thread-id"); got != "session-1" {
		t.Fatalf("thread-id = %q", got)
	}
	if got := req.Header.Get("x-session-affinity"); got != "session-1" {
		t.Fatalf("x-session-affinity = %q", got)
	}
	if got := req.Header.Get("version"); got != "0.144.0" {
		t.Fatalf("version = %q", got)
	}
	if got := req.Header.Get("OpenAI-Beta"); got != "" {
		t.Fatalf("obsolete beta header retained: %q", got)
	}
	if got := req.Header.Get("session_id"); got != "" {
		t.Fatalf("legacy session header retained: %q", got)
	}

	raw, err := io.ReadAll(req.Body)
	if err != nil {
		t.Fatal(err)
	}
	var transformed map[string]any
	if err := json.Unmarshal(raw, &transformed); err != nil {
		t.Fatal(err)
	}
	if transformed["tool_choice"] != "auto" {
		t.Fatalf("tool_choice = %#v", transformed["tool_choice"])
	}
	if transformed["parallel_tool_calls"] != false {
		t.Fatalf("parallel_tool_calls = %#v", transformed["parallel_tool_calls"])
	}
	if transformed["prompt_cache_key"] != "session-1" {
		t.Fatalf("prompt_cache_key = %#v", transformed["prompt_cache_key"])
	}
	if _, exists := transformed["temperature"]; exists {
		t.Fatal("temperature was not removed")
	}
	if _, exists := transformed["max_output_tokens"]; exists {
		t.Fatal("max_output_tokens was not removed")
	}
	reasoning, _ := transformed["reasoning"].(map[string]any)
	if reasoning["context"] != "all_turns" {
		t.Fatalf("reasoning.context = %#v", reasoning["context"])
	}
	include, _ := transformed["include"].([]any)
	if len(include) != 1 || include[0] != "reasoning.encrypted_content" {
		t.Fatalf("include = %#v", include)
	}
	metadata, _ := transformed["client_metadata"].(map[string]any)
	if _, exists := metadata[staleWSMetadataKey]; exists {
		t.Fatal("websocket-only metadata was retained")
	}
	if metadata["session_id"] != "session-1" || metadata["thread_id"] != "session-1" {
		t.Fatalf("client metadata = %#v", metadata)
	}
	input, _ := transformed["input"].([]any)
	if len(input) != 2 {
		t.Fatalf("input length = %d: %#v", len(input), input)
	}
	additional, _ := input[0].(map[string]any)
	if additional["type"] != "additional_tools" {
		t.Fatalf("first input = %#v", additional)
	}
	if _, exists := additional["id"]; exists {
		t.Fatal("additional_tools id was retained")
	}
	message, _ := input[1].(map[string]any)
	if _, exists := message["id"]; exists {
		t.Fatal("input item id was retained")
	}
	content, _ := message["content"].([]any)
	image, _ := content[0].(map[string]any)
	if _, exists := image["detail"]; exists {
		t.Fatal("input image detail was retained")
	}
}

func TestSummarizeUpstreamErrorRedactsSecrets(t *testing.T) {
	raw := []byte(`{"error":{"message":"Bearer abc.def sk-secret123 access_token=secret eyJabc.def.ghi","type":"invalid_request_error","code":"bad"}}`)
	summary := summarizeUpstreamError(raw)
	for _, secret := range []string{"abc.def", "sk-secret123", "secret", "eyJabc.def.ghi"} {
		if strings.Contains(summary, secret) {
			t.Fatalf("summary leaked %q: %s", secret, summary)
		}
	}
	if !strings.Contains(summary, "type=invalid_request_error") || !strings.Contains(summary, "code=bad") {
		t.Fatalf("summary lost useful fields: %s", summary)
	}
}

func TestNormalizeCodexBodyConvertsSystemMessagesToDeveloper(t *testing.T) {
	body := map[string]any{
		"model": "gpt-5.6-sol",
		"input": []any{
			map[string]any{
				"type": "message",
				"role": "system",
				"content": []any{map[string]any{
					"type": "input_text",
					"text": "You are a coding agent.",
				}},
			},
			map[string]any{
				"type": "message",
				"role": "user",
				"content": []any{map[string]any{
					"type": "input_text",
					"text": "Hello",
				}},
			},
		},
	}

	if err := normalizeCodexBody(body, codexIdentity{}, true); err != nil {
		t.Fatal(err)
	}
	input, _ := body["input"].([]any)
	if len(input) != 3 {
		t.Fatalf("input length = %d: %#v", len(input), input)
	}
	message, _ := input[1].(map[string]any)
	if message["role"] != "developer" {
		t.Fatalf("system role was not normalized: %#v", message)
	}
	for _, item := range input {
		object, _ := item.(map[string]any)
		if object["role"] == "system" {
			t.Fatalf("system role leaked upstream: %#v", object)
		}
	}
}
