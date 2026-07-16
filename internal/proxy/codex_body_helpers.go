package proxy

import (
	"errors"
	"strings"
)

const staleWSMetadataKey = "ws_request_header_x_openai_internal_codex_responses_lite"

func normalizeCompatInput(value any) []any {
	switch input := value.(type) {
	case []any:
		return input
	case string:
		if input == "" {
			return []any{}
		}
		return []any{map[string]any{
			"type": "message", "role": "user",
			"content": []any{map[string]any{"type": "input_text", "text": input}},
		}}
	case nil:
		return []any{}
	default:
		return []any{input}
	}
}

// normalizeCompatMessageRoles adapts public Responses API roles to the
// ChatGPT Codex endpoint, which rejects role=system input messages.
func normalizeCompatMessageRoles(input []any) {
	for _, item := range input {
		message, ok := item.(map[string]any)
		if !ok {
			continue
		}
		messageType, _ := message["type"].(string)
		if messageType != "" && messageType != "message" {
			continue
		}
		role, _ := message["role"].(string)
		if strings.EqualFold(strings.TrimSpace(role), "system") {
			message["role"] = "developer"
		}
	}
}

func ensureCompatEncryptedReasoning(body map[string]any) error {
	include := []any{}
	if raw, exists := body["include"]; exists && raw != nil {
		values, ok := raw.([]any)
		if !ok {
			return errors.New("include must be an array")
		}
		include = append(include, values...)
	}
	for _, value := range include {
		if value == "reasoning.encrypted_content" {
			body["include"] = include
			return nil
		}
	}
	body["include"] = append(include, "reasoning.encrypted_content")
	return nil
}

func normalizeCompatMetadata(body map[string]any, identity codexIdentity) error {
	metadata := map[string]any{}
	if raw, exists := body["client_metadata"]; exists && raw != nil {
		source, ok := raw.(map[string]any)
		if !ok {
			return errors.New("client_metadata must be an object")
		}
		for key, value := range source {
			if key == staleWSMetadataKey {
				continue
			}
			if text, ok := value.(string); ok {
				metadata[key] = text
			}
		}
	}
	if identity.sessionID != "" {
		metadata["session_id"] = identity.sessionID
	}
	if identity.threadID != "" {
		metadata["thread_id"] = identity.threadID
	}
	if identity.windowID != "" {
		metadata["x-codex-window-id"] = identity.windowID
	}
	if len(metadata) == 0 {
		delete(body, "client_metadata")
	} else {
		body["client_metadata"] = metadata
	}
	return nil
}

func stripCompatInputIDs(input []any) {
	for _, item := range input {
		if object, ok := item.(map[string]any); ok {
			delete(object, "id")
		}
	}
}

func stripCompatImageDetail(value any) {
	switch typed := value.(type) {
	case []any:
		for _, item := range typed {
			stripCompatImageDetail(item)
		}
	case map[string]any:
		if typed["type"] == "input_image" {
			delete(typed, "detail")
		}
		for _, item := range typed {
			stripCompatImageDetail(item)
		}
	}
}

func compatObject(value any) map[string]any {
	object, _ := value.(map[string]any)
	return object
}
