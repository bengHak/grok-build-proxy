package proxy

import (
	"errors"
	"strings"
)

type codexIdentity struct {
	sessionID string
	threadID  string
	windowID  string
}

var allowedCodexFields = map[string]struct{}{
	"client_metadata": {}, "include": {}, "input": {}, "instructions": {},
	"model": {}, "parallel_tool_calls": {}, "prompt_cache_key": {},
	"reasoning": {}, "service_tier": {}, "store": {}, "stream": {},
	"stream_options": {}, "text": {}, "tool_choice": {}, "tools": {},
}

func normalizeCodexBody(body map[string]any, identity codexIdentity, lite bool) error {
	body["store"] = false
	body["tool_choice"] = "auto"
	if identity.sessionID != "" {
		body["prompt_cache_key"] = identity.sessionID
	}
	input := normalizeCompatInput(body["input"])
	stripCompatInputIDs(input)
	stripCompatImageDetail(input)
	if err := ensureCompatEncryptedReasoning(body); err != nil {
		return err
	}
	if err := normalizeCompatMetadata(body, identity); err != nil {
		return err
	}
	if lite {
		if err := normalizeResponsesLiteBody(body, input); err != nil {
			return err
		}
	} else {
		body["input"] = input
		if _, exists := body["parallel_tool_calls"]; !exists {
			body["parallel_tool_calls"] = true
		}
	}
	for key := range body {
		if _, keep := allowedCodexFields[key]; !keep || body[key] == nil {
			delete(body, key)
		}
	}
	return nil
}

func normalizeResponsesLiteBody(body map[string]any, input []any) error {
	body["parallel_tool_calls"] = false
	reasoning := compatObject(body["reasoning"])
	if reasoning == nil {
		reasoning = map[string]any{}
	}
	reasoning["context"] = "all_turns"
	body["reasoning"] = reasoning
	text := compatObject(body["text"])
	if text == nil {
		text = map[string]any{}
	}
	if _, exists := text["verbosity"]; !exists {
		text["verbosity"] = "low"
	}
	body["text"] = text

	tools := []any{}
	if raw, exists := body["tools"]; exists && raw != nil {
		var ok bool
		tools, ok = raw.([]any)
		if !ok {
			return errors.New("Responses Lite requires tools to be an array")
		}
	}
	filtered := make([]any, 0, len(input))
	for _, item := range input {
		object, ok := item.(map[string]any)
		if ok && object["type"] == "additional_tools" {
			if existing, ok := object["tools"].([]any); ok && len(existing) > 0 {
				tools = existing
			}
			continue
		}
		filtered = append(filtered, item)
	}
	prefix := []any{map[string]any{
		"type": "additional_tools", "role": "developer", "tools": tools,
	}}
	if raw, exists := body["instructions"]; exists && raw != nil {
		instructions, ok := raw.(string)
		if !ok {
			return errors.New("Responses Lite requires instructions to be a string")
		}
		if strings.TrimSpace(instructions) != "" {
			prefix = append(prefix, compatDeveloperMessage(instructions))
		}
	}
	body["instructions"] = ""
	body["input"] = append(prefix, filtered...)
	delete(body, "tools")
	return nil
}

func compatDeveloperMessage(text string) map[string]any {
	return map[string]any{
		"type": "message", "role": "developer",
		"content": []any{map[string]any{"type": "input_text", "text": text}},
	}
}
