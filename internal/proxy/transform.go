package proxy

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strings"

	"github.com/bengHak/grok-build-proxy/internal/catalog"
	"github.com/bengHak/grok-build-proxy/internal/modelmap"
)

type transformedRequest struct {
	Body           []byte
	RequestedModel string
	Model          string
	Mapped         bool
	Lite           bool
	Fast           bool
	Stream         bool
}

// transformRequest converts Grok Build's standard Responses API payload into
// the ChatGPT Codex wire shape. Most models are pass-through. Responses Lite
// models move tools and developer instructions into input items, matching the
// Codex client's current request format.
func transformRequest(raw []byte, models catalog.Catalog, mappings modelmap.Map) (transformedRequest, error) {
	if len(bytes.TrimSpace(raw)) == 0 {
		return transformedRequest{}, errors.New("request body is empty")
	}
	var body map[string]any
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.UseNumber()
	if err := dec.Decode(&body); err != nil {
		return transformedRequest{}, fmt.Errorf("invalid JSON request: %w", err)
	}
	var trailing any
	if err := dec.Decode(&trailing); !errors.Is(err, io.EOF) {
		if err == nil {
			return transformedRequest{}, errors.New("request body contains more than one JSON value")
		}
		return transformedRequest{}, fmt.Errorf("invalid trailing JSON data: %w", err)
	}

	requestedModel, _ := body["model"].(string)
	requestedModel = strings.TrimSpace(requestedModel)
	if requestedModel == "" {
		return transformedRequest{}, errors.New("model is required")
	}
	resolution := mappings.Resolve(requestedModel)
	model, _ := models.Lookup(resolution.Model)
	body["model"] = resolution.Model
	body["store"] = false

	stream := true
	if value, ok := body["stream"].(bool); ok {
		stream = value
	} else {
		body["stream"] = true
	}
	if resolution.Fast {
		if _, exists := body["service_tier"]; !exists {
			body["service_tier"] = "priority"
		}
	}

	if model.ResponsesLite {
		applyResponsesLite(body)
	} else if _, exists := body["parallel_tool_calls"]; !exists {
		body["parallel_tool_calls"] = true
	}

	encoded, err := json.Marshal(body)
	if err != nil {
		return transformedRequest{}, fmt.Errorf("encode transformed request: %w", err)
	}
	return transformedRequest{
		Body:           encoded,
		RequestedModel: requestedModel,
		Model:          resolution.Model,
		Mapped:         resolution.Mapped,
		Lite:           model.ResponsesLite,
		Fast:           resolution.Fast,
		Stream:         stream,
	}, nil
}

func applyResponsesLite(body map[string]any) {
	body["parallel_tool_calls"] = false

	clientMetadata := objectValue(body["client_metadata"])
	if clientMetadata == nil {
		clientMetadata = map[string]any{}
	}
	clientMetadata["ws_request_header_x_openai_internal_codex_responses_lite"] = "true"
	body["client_metadata"] = clientMetadata

	reasoning := objectValue(body["reasoning"])
	if reasoning == nil {
		reasoning = map[string]any{}
	}
	reasoning["context"] = "all_turns"
	body["reasoning"] = reasoning

	text := objectValue(body["text"])
	if text == nil {
		text = map[string]any{}
	}
	if _, exists := text["verbosity"]; !exists {
		text["verbosity"] = "low"
	}
	body["text"] = text

	input := normalizeInput(body["input"])
	prefix := make([]any, 0, 2)
	if tools, ok := body["tools"].([]any); ok && len(tools) > 0 {
		prefix = append(prefix, map[string]any{
			"type":  "additional_tools",
			"id":    nil,
			"role":  "developer",
			"tools": tools,
		})
	}
	delete(body, "tools")

	if instructions, ok := body["instructions"].(string); ok && strings.TrimSpace(instructions) != "" {
		prefix = append(prefix, map[string]any{
			"type": "message",
			"role": "developer",
			"content": []any{
				map[string]any{"type": "input_text", "text": instructions},
			},
		})
	}
	delete(body, "instructions")

	if len(prefix) > 0 {
		input = append(prefix, input...)
	}
	body["input"] = input
}

func normalizeInput(value any) []any {
	switch input := value.(type) {
	case []any:
		return input
	case string:
		if input == "" {
			return []any{}
		}
		return []any{map[string]any{
			"type": "message",
			"role": "user",
			"content": []any{
				map[string]any{"type": "input_text", "text": input},
			},
		}}
	case nil:
		return []any{}
	default:
		return []any{input}
	}
}

func objectValue(value any) map[string]any {
	object, _ := value.(map[string]any)
	return object
}
