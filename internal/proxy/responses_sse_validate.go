package proxy

import (
	"encoding/json"
	"errors"
)

func validateFunctionCall(item map[string]any) error {
	if stringValue(item["id"]) == "" {
		return errors.New("missing item id")
	}
	if stringValue(item["call_id"]) == "" {
		return errors.New("missing call_id")
	}
	if stringValue(item["name"]) == "" {
		return errors.New("missing function name")
	}
	arguments, ok := item["arguments"].(string)
	if !ok || !json.Valid([]byte(arguments)) {
		return errors.New("arguments are not valid JSON")
	}
	return nil
}

func validateCustomToolCall(item map[string]any) error {
	if stringValue(item["id"]) == "" {
		return errors.New("missing item id")
	}
	if stringValue(item["call_id"]) == "" {
		return errors.New("missing call_id")
	}
	if stringValue(item["name"]) == "" {
		return errors.New("missing tool name")
	}
	if _, ok := item["input"].(string); !ok {
		return errors.New("missing completed input")
	}
	return nil
}

func messageHasVisibleContent(item map[string]any) bool {
	if item == nil || stringValue(item["type"]) != "message" {
		return false
	}
	for _, raw := range jsonArray(item["content"]) {
		part := jsonObject(raw)
		switch stringValue(part["type"]) {
		case "output_text":
			if stringValue(part["text"]) != "" {
				return true
			}
		case "refusal":
			if stringValue(part["refusal"]) != "" {
				return true
			}
		}
	}
	return false
}

func responseHasUsableOutput(response map[string]any) bool {
	for _, raw := range jsonArray(response["output"]) {
		item := jsonObject(raw)
		switch stringValue(item["type"]) {
		case "message":
			if messageHasVisibleContent(item) {
				return true
			}
		case "function_call":
			if validateFunctionCall(item) == nil {
				return true
			}
		case "custom_tool_call":
			if validateCustomToolCall(item) == nil {
				return true
			}
		}
	}
	return false
}

func itemString(item map[string]any, key string) string {
	if item == nil {
		return ""
	}
	return stringValue(item[key])
}

func jsonObjectsEqual(left, right map[string]any) bool {
	leftJSON, leftErr := json.Marshal(left)
	rightJSON, rightErr := json.Marshal(right)
	return leftErr == nil && rightErr == nil && string(leftJSON) == string(rightJSON)
}
