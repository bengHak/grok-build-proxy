package proxy

import (
	"encoding/json"
	"fmt"
	"sort"
	"strings"
)

func (s *responsesSSEAssembler) mergeResponse(response map[string]any, status string, strict bool) (bool, error) {
	if s.stateErr != nil {
		return false, s.stateErr
	}
	output := append([]any(nil), jsonArray(response["output"])...)
	indexes := make([]int, 0, len(s.outputs))
	for index := range s.outputs {
		indexes = append(indexes, index)
	}
	sort.Ints(indexes)

	modified := false
	for _, index := range indexes {
		state := s.outputs[index]
		existingIndex := findExistingOutputItem(output, state)
		if existingIndex >= 0 && outputItemSatisfiesState(jsonObject(output[existingIndex]), state) {
			continue
		}

		canonical, complete, executable, err := state.canonicalItem(stringValue(response["id"]), s.mode)
		if err != nil {
			return false, err
		}
		if !complete {
			if strict && executable {
				return false, fmt.Errorf("Responses Lite stream ended with an incomplete %s at output_index %d", firstNonEmptyString(state.kind, "tool call"), state.index)
			}
			continue
		}
		if existingIndex >= 0 {
			merged := mergeOutputItem(jsonObject(output[existingIndex]), canonical)
			if !jsonObjectsEqual(jsonObject(output[existingIndex]), merged) {
				output[existingIndex] = merged
				modified = true
			}
		} else {
			output = insertOutputItem(output, state.index, canonical)
			modified = true
		}
	}

	if strict {
		for index, raw := range output {
			item := jsonObject(raw)
			if item == nil {
				continue
			}
			switch stringValue(item["type"]) {
			case "function_call":
				if err := validateFunctionCall(item); err != nil {
					return false, fmt.Errorf("invalid function_call at output index %d: %w", index, err)
				}
			case "custom_tool_call":
				if err := validateCustomToolCall(item); err != nil {
					return false, fmt.Errorf("invalid custom_tool_call at output index %d: %w", index, err)
				}
			}
		}
	}

	if modified {
		response["output"] = output
	}
	if status != "" && stringValue(response["status"]) != status {
		response["status"] = status
		modified = true
	}
	return modified, nil
}

func (s *responseOutputState) canonicalItem(responseID string, mode responsesCompatMode) (map[string]any, bool, bool, error) {
	base := cloneJSONObject(s.doneItem)
	if base == nil {
		base = cloneJSONObject(s.addedItem)
	}
	kind := firstNonEmptyString(s.kind, stringValue(base["type"]))
	if kind == "" {
		if s.text() != "" || s.refusal() != "" {
			kind = "message"
		}
	}

	switch kind {
	case "message":
		item := buildMessageItem(base, s.itemID, responseID, s.text(), s.refusal())
		return item, messageHasVisibleContent(item), false, nil
	case "function_call":
		if mode != responsesCompatFull {
			return nil, false, true, nil
		}
		item, complete := s.buildFunctionCall(base)
		if !complete {
			return nil, false, true, nil
		}
		if err := validateFunctionCall(item); err != nil {
			return nil, false, true, err
		}
		return item, true, true, nil
	case "custom_tool_call":
		if mode != responsesCompatFull {
			return nil, false, true, nil
		}
		item, complete := s.buildCustomToolCall(base)
		if !complete {
			return nil, false, true, nil
		}
		if err := validateCustomToolCall(item); err != nil {
			return nil, false, true, err
		}
		return item, true, true, nil
	default:
		if s.doneItem != nil {
			return cloneJSONObject(s.doneItem), true, false, nil
		}
		return nil, false, false, nil
	}
}

func (s *responseOutputState) buildFunctionCall(base map[string]any) (map[string]any, bool) {
	item := cloneJSONObject(base)
	if item == nil {
		item = map[string]any{}
	}
	item["type"] = "function_call"
	callID := firstNonEmptyString(stringValue(item["call_id"]), s.callID, itemString(s.doneItem, "call_id"), itemString(s.addedItem, "call_id"))
	item["call_id"] = callID
	item["id"] = firstNonEmptyString(stringValue(item["id"]), s.itemID, syntheticToolItemID("fc_", callID))
	item["name"] = firstNonEmptyString(stringValue(item["name"]), itemString(s.doneItem, "name"), itemString(s.addedItem, "name"))

	arguments := stringValue(item["arguments"])
	argumentsDone := arguments != "" && s.doneItem != nil
	if !argumentsDone && s.argumentsDoneSeen {
		arguments = s.argumentsDone
		argumentsDone = true
	}
	if !argumentsDone {
		return item, false
	}
	item["arguments"] = arguments
	item["status"] = "completed"
	return item, true
}

func (s *responseOutputState) buildCustomToolCall(base map[string]any) (map[string]any, bool) {
	item := cloneJSONObject(base)
	if item == nil {
		item = map[string]any{}
	}
	item["type"] = "custom_tool_call"
	callID := firstNonEmptyString(stringValue(item["call_id"]), s.callID, itemString(s.doneItem, "call_id"), itemString(s.addedItem, "call_id"))
	item["call_id"] = callID
	item["id"] = firstNonEmptyString(stringValue(item["id"]), s.itemID, syntheticToolItemID("ct_", callID))
	item["name"] = firstNonEmptyString(stringValue(item["name"]), itemString(s.doneItem, "name"), itemString(s.addedItem, "name"))

	input := stringValue(item["input"])
	inputDone := s.doneItem != nil && item["input"] != nil
	if !inputDone && s.customInputDoneSeen {
		input = s.customInputDone
		inputDone = true
	}
	if !inputDone {
		return item, false
	}
	item["input"] = input
	item["status"] = "completed"
	return item, true
}

func syntheticToolItemID(prefix, callID string) string {
	if callID == "" {
		return ""
	}
	return prefix + callID
}

func (s *responseOutputState) text() string {
	if s.textDoneSeen {
		return s.textDone
	}
	return s.textDelta.String()
}

func (s *responseOutputState) refusal() string {
	if s.refusalDoneSeen {
		return s.refusalDone
	}
	return s.refusalDelta.String()
}

func buildMessageItem(base map[string]any, itemID, responseID, text, refusal string) map[string]any {
	item := cloneJSONObject(base)
	if item == nil {
		item = map[string]any{}
	}
	item["type"] = "message"
	item["role"] = "assistant"
	item["status"] = "completed"
	if stringValue(item["id"]) == "" {
		if itemID == "" {
			itemID = "msg_" + strings.TrimPrefix(responseID, "resp_")
			if itemID == "msg_" {
				itemID = "msg_grok_build_proxy"
			}
		}
		item["id"] = itemID
	}
	content := append([]any(nil), jsonArray(item["content"])...)
	if text != "" {
		content = mergeMessageContent(content, "output_text", text)
	}
	if refusal != "" {
		content = mergeMessageContent(content, "refusal", refusal)
	}
	item["content"] = content
	return item
}

func mergeMessageContent(content []any, kind, value string) []any {
	field := "text"
	if kind == "refusal" {
		field = "refusal"
	}
	for _, raw := range content {
		part := jsonObject(raw)
		if stringValue(part["type"]) != kind {
			continue
		}
		if stringValue(part[field]) == "" {
			part[field] = value
		}
		return content
	}
	part := map[string]any{"type": kind, field: value}
	if kind == "output_text" {
		part["annotations"] = []any{}
		part["logprobs"] = []any{}
	}
	return append(content, part)
}

func findExistingOutputItem(output []any, state *responseOutputState) int {
	if state.itemID != "" {
		for index, raw := range output {
			if stringValue(jsonObject(raw)["id"]) == state.itemID {
				return index
			}
		}
	}
	if state.callID != "" {
		for index, raw := range output {
			item := jsonObject(raw)
			if stringValue(item["call_id"]) == state.callID {
				return index
			}
		}
	}
	if state.index >= 0 && state.index < len(output) {
		item := jsonObject(output[state.index])
		if item != nil {
			itemType := stringValue(item["type"])
			if state.kind == "" || itemType == state.kind || (state.kind == "message" && itemType == "message") {
				return state.index
			}
		}
	}
	return -1
}

func outputItemSatisfiesState(item map[string]any, state *responseOutputState) bool {
	if item == nil {
		return false
	}
	switch stringValue(item["type"]) {
	case "message":
		return messageHasVisibleContent(item)
	case "function_call":
		return validateFunctionCall(item) == nil
	case "custom_tool_call":
		return validateCustomToolCall(item) == nil
	default:
		return stringValue(item["status"]) == "completed" && state.doneItem != nil
	}
}

func mergeOutputItem(existing, canonical map[string]any) map[string]any {
	if existing == nil {
		return canonical
	}
	if canonical == nil {
		return existing
	}
	if stringValue(existing["type"]) == "message" && stringValue(canonical["type"]) == "message" {
		result := cloneJSONObject(existing)
		content := append([]any(nil), jsonArray(result["content"])...)
		for _, raw := range jsonArray(canonical["content"]) {
			part := jsonObject(raw)
			kind := stringValue(part["type"])
			switch kind {
			case "output_text":
				content = mergeMessageContent(content, kind, stringValue(part["text"]))
			case "refusal":
				content = mergeMessageContent(content, kind, stringValue(part["refusal"]))
			}
		}
		result["content"] = content
		if stringValue(result["role"]) == "" {
			result["role"] = "assistant"
		}
		if stringValue(result["status"]) == "" {
			result["status"] = "completed"
		}
		return result
	}

	result := cloneJSONObject(canonical)
	for key, value := range existing {
		if key == "arguments" && stringValue(canonical["type"]) == "function_call" && !json.Valid([]byte(stringValue(value))) {
			continue
		}
		if value != nil && !(stringValue(value) == "" && stringValue(canonical[key]) != "") {
			result[key] = value
		}
	}
	return result
}

func insertOutputItem(output []any, index int, item map[string]any) []any {
	if index < 0 || index >= len(output) {
		return append(output, item)
	}
	output = append(output, nil)
	copy(output[index+1:], output[index:])
	output[index] = item
	return output
}
