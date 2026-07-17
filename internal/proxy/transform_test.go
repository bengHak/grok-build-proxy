package proxy

import (
	"encoding/json"
	"testing"

	"github.com/bengHak/grok-build-proxy/internal/catalog"
	"github.com/bengHak/grok-build-proxy/internal/modelmap"
)

func TestTransformFullResponsesRequest(t *testing.T) {
	result, err := transformRequest([]byte(`{
      "model":"gpt-5.5",
      "instructions":"keep me",
      "input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}],
      "tools":[{"type":"function","name":"shell","parameters":{"type":"object"}}],
      "stream":true
    }`), catalog.New(""), modelmap.Map{})
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
    }`), catalog.New(""), modelmap.Map{})
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

func TestTransformPreservesReasoningEffort(t *testing.T) {
	mappings, err := modelmap.Parse("grok-build=gpt-5.5")
	if err != nil {
		t.Fatal(err)
	}

	tests := []struct {
		name        string
		model       string
		wantModel   string
		wantContext bool
		wantMapped  bool
		wantFast    bool
	}{
		{name: "Responses Lite", model: "gpt-5.6-sol", wantModel: "gpt-5.6-sol", wantContext: true},
		{name: "non-Lite Responses", model: "gpt-5.5", wantModel: "gpt-5.5"},
		{name: "model map", model: "grok-build", wantModel: "gpt-5.5", wantMapped: true},
		{name: "fast alias", model: "gpt-5.6-terra-fast", wantModel: "gpt-5.6-terra", wantContext: true, wantFast: true},
	}
	for _, effort := range []string{"low", "medium", "high", "xhigh"} {
		for _, test := range tests {
			t.Run(effort+"/"+test.name, func(t *testing.T) {
				raw, err := json.Marshal(map[string]any{
					"model":     test.model,
					"input":     "hello",
					"reasoning": map[string]any{"effort": effort},
				})
				if err != nil {
					t.Fatal(err)
				}
				result, err := transformRequest(raw, catalog.New(""), mappings)
				if err != nil {
					t.Fatal(err)
				}
				if result.Model != test.wantModel || result.Mapped != test.wantMapped || result.Fast != test.wantFast {
					t.Fatalf("unexpected result metadata: %#v", result)
				}

				var body map[string]any
				if err := json.Unmarshal(result.Body, &body); err != nil {
					t.Fatal(err)
				}
				if body["model"] != test.wantModel {
					t.Fatalf("model = %#v, want %q", body["model"], test.wantModel)
				}
				reasoning, ok := body["reasoning"].(map[string]any)
				if !ok {
					t.Fatalf("reasoning = %#v", body["reasoning"])
				}
				if reasoning["effort"] != effort {
					t.Fatalf("reasoning.effort = %#v, want %q", reasoning["effort"], effort)
				}
				if test.wantContext {
					if reasoning["context"] != "all_turns" || len(reasoning) != 2 {
						t.Fatalf("reasoning = %#v, want effort plus all_turns context", reasoning)
					}
				} else if len(reasoning) != 1 {
					t.Fatalf("reasoning changed for non-Lite request: %#v", reasoning)
				}
				if test.wantFast {
					if body["service_tier"] != "priority" {
						t.Fatalf("service_tier = %#v, want priority", body["service_tier"])
					}
				} else if _, exists := body["service_tier"]; exists {
					t.Fatalf("unexpected service_tier = %#v", body["service_tier"])
				}
			})
		}
	}
}

func TestTransformRequiresModel(t *testing.T) {
	if _, err := transformRequest([]byte(`{"input":"hello"}`), catalog.New(""), modelmap.Map{}); err == nil {
		t.Fatal("expected missing model error")
	}
}

func TestTransformAppliesConfiguredModelMapping(t *testing.T) {
	mappings, err := modelmap.Parse("grok-build=gpt-5.6-terra,grok-4.5=gpt-5.6-sol")
	if err != nil {
		t.Fatal(err)
	}
	result, err := transformRequest([]byte(`{
      "model":"grok-4.5-fast",
      "input":"review this repository"
    }`), catalog.New(""), mappings)
	if err != nil {
		t.Fatal(err)
	}
	if result.RequestedModel != "grok-4.5-fast" || result.Model != "gpt-5.6-sol" || !result.Mapped || !result.Fast || !result.Lite {
		t.Fatalf("unexpected result metadata: %#v", result)
	}
	var body map[string]any
	if err := json.Unmarshal(result.Body, &body); err != nil {
		t.Fatal(err)
	}
	if body["model"] != "gpt-5.6-sol" {
		t.Fatalf("model = %#v", body["model"])
	}
	if body["service_tier"] != "priority" {
		t.Fatalf("service_tier = %#v", body["service_tier"])
	}
}

func TestTransformMappingCanSelectFastTarget(t *testing.T) {
	mappings, err := modelmap.Parse("grok-build=gpt-5.6-terra-fast")
	if err != nil {
		t.Fatal(err)
	}
	result, err := transformRequest([]byte(`{"model":"grok-build","input":"hello"}`), catalog.New(""), mappings)
	if err != nil {
		t.Fatal(err)
	}
	if result.Model != "gpt-5.6-terra" || !result.Mapped || !result.Fast {
		t.Fatalf("unexpected result metadata: %#v", result)
	}
}
