package proxy

import (
	"encoding/json"
	"testing"

	"github.com/bengHak/grok-build-proxy/internal/catalog"
)

func TestTransformFullResponsesRequest(t *testing.T) {
	result, err := transformRequest([]byte(`{
      "model":"gpt-5.5",
      "instructions":"keep me",
      "input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}],
      "tools":[{"type":"function","name":"shell","parameters":{"type":"object"}}],
      "stream":true
    }`), catalog.New(""))
	if err != nil {
		t.Fatal(err)
	}
	if result.Lite {
		t.Fatal("gpt-5.5 should not use Responses Lite")
	}
	var body map[string]any
	if err := json.Unmarshal(result.Body, &body); err != nil {
		t.Fatal(err)
	}
	if body["instructions"] != "keep me" {
		t.Fatalf("instructions = %#v", body["instructions"])
	}
	if _, ok := body["tools"].([]any); !ok {
		t.Fatalf("tools were removed: %#v", body["tools"])
	}
	if body["parallel_tool_calls"] != true {
		t.Fatalf("parallel_tool_calls = %#v", body["parallel_tool_calls"])
	}
	if body["store"] != false {
		t.Fatalf("store = %#v", body["store"])
	}
}

func TestTransformResponsesLiteAndFastAlias(t *testing.T) {
	result, err := transformRequest([]byte(`{
      "model":"gpt-5.6-sol-fast",
      "instructions":"developer instructions",
      "input":"hello",
      "tools":[{"type":"function","name":"shell","parameters":{"type":"object"}}],
      "reasoning":{"effort":"high"}
    }`), catalog.New(""))
	if err != nil {
		t.Fatal(err)
	}
	if !result.Lite || !result.Fast || result.Model != "gpt-5.6-sol" {
		t.Fatalf("unexpected result metadata: %#v", result)
	}
	var body map[string]any
	if err := json.Unmarshal(result.Body, &body); err != nil {
		t.Fatal(err)
	}
	if body["model"] != "gpt-5.6-sol" || body["service_tier"] != "priority" {
		t.Fatalf("model/service_tier = %#v/%#v", body["model"], body["service_tier"])
	}
	if _, exists := body["instructions"]; exists {
		t.Fatal("instructions should be moved into input")
	}
	if _, exists := body["tools"]; exists {
		t.Fatal("tools should be moved into additional_tools")
	}
	if body["parallel_tool_calls"] != false {
		t.Fatalf("parallel_tool_calls = %#v", body["parallel_tool_calls"])
	}
	reasoning := body["reasoning"].(map[string]any)
	if reasoning["effort"] != "high" || reasoning["context"] != "all_turns" {
		t.Fatalf("reasoning = %#v", reasoning)
	}
	metadata := body["client_metadata"].(map[string]any)
	if metadata["ws_request_header_x_openai_internal_codex_responses_lite"] != "true" {
		t.Fatalf("client_metadata = %#v", metadata)
	}
	input := body["input"].([]any)
	if len(input) != 3 {
		t.Fatalf("input length = %d; %#v", len(input), input)
	}
	if input[0].(map[string]any)["type"] != "additional_tools" {
		t.Fatalf("first input item = %#v", input[0])
	}
	if input[1].(map[string]any)["role"] != "developer" {
		t.Fatalf("second input item = %#v", input[1])
	}
	if input[2].(map[string]any)["role"] != "user" {
		t.Fatalf("third input item = %#v", input[2])
	}
}

func TestTransformRequiresModel(t *testing.T) {
	if _, err := transformRequest([]byte(`{"input":"hello"}`), catalog.New("")); err == nil {
		t.Fatal("expected missing model error")
	}
}
