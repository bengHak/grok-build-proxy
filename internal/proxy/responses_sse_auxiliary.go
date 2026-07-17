package proxy

import (
	"encoding/json"
	"strings"
)

// outputEventAuxiliary deliberately sits outside the canonical event enum. The
// assembler's default acceptance path lets lifecycle events correlate with an
// existing output even after its text or item completion boundary has arrived.
const outputEventAuxiliary responseStreamOutputEvent = 255

type responsesAuxiliaryEventSpec struct {
	kind              string
	contentIndexed    bool
	summaryIndexed    bool
	annotationIndexed bool
	partialIndexed    bool
	outputPart        bool
	summaryPart       bool
}

func responsesAuxiliarySpec(eventType string) (responsesAuxiliaryEventSpec, bool) {
	switch eventType {
	case "response.content_part.added", "response.content_part.done":
		return responsesAuxiliaryEventSpec{
			kind:           "message",
			contentIndexed: true,
			outputPart:     true,
		}, true
	case "response.reasoning_summary_part.added", "response.reasoning_summary_part.done":
		return responsesAuxiliaryEventSpec{
			kind:           "reasoning",
			summaryIndexed: true,
			summaryPart:    true,
		}, true
	case "response.reasoning_summary_text.delta", "response.reasoning_summary_text.done":
		return responsesAuxiliaryEventSpec{
			kind:           "reasoning",
			summaryIndexed: true,
		}, true
	case "response.reasoning_text.delta", "response.reasoning_text.done":
		return responsesAuxiliaryEventSpec{
			kind:           "reasoning",
			contentIndexed: true,
		}, true
	case "response.output_text.annotation.added":
		return responsesAuxiliaryEventSpec{
			kind:              "message",
			contentIndexed:    true,
			annotationIndexed: true,
		}, true
	case "response.file_search_call.in_progress",
		"response.file_search_call.searching",
		"response.file_search_call.completed":
		return responsesAuxiliaryEventSpec{kind: "file_search_call"}, true
	case "response.web_search_call.in_progress",
		"response.web_search_call.searching",
		"response.web_search_call.completed":
		return responsesAuxiliaryEventSpec{kind: "web_search_call"}, true
	case "response.image_generation_call.in_progress",
		"response.image_generation_call.generating",
		"response.image_generation_call.completed":
		return responsesAuxiliaryEventSpec{kind: "image_generation_call"}, true
	case "response.image_generation_call.partial_image":
		return responsesAuxiliaryEventSpec{
			kind:           "image_generation_call",
			partialIndexed: true,
		}, true
	case "response.mcp_call_arguments.delta",
		"response.mcp_call_arguments.done",
		"response.mcp_call.in_progress",
		"response.mcp_call.completed",
		"response.mcp_call.failed":
		return responsesAuxiliaryEventSpec{kind: "mcp_call"}, true
	case "response.mcp_list_tools.in_progress",
		"response.mcp_list_tools.completed",
		"response.mcp_list_tools.failed":
		return responsesAuxiliaryEventSpec{kind: "mcp_list_tools"}, true
	case "response.code_interpreter_call.in_progress",
		"response.code_interpreter_call.interpreting",
		"response.code_interpreter_call.completed",
		"response.code_interpreter_call_code.delta",
		"response.code_interpreter_call_code.done":
		return responsesAuxiliaryEventSpec{kind: "code_interpreter_call"}, true
	default:
		return responsesAuxiliaryEventSpec{}, false
	}
}

// normalizeAuxiliaryFrame fills the strict Responses event envelope fields that
// the compact Responses Lite transport may omit. Grok Build deserializes every
// SSE frame into async-openai's fully populated event types, while Codex's own
// Lite client intentionally accepts a much smaller shape. This pass runs before
// the canonical assembler so both the live stream and final output share the
// same output identity.
func (s *responsesSSEAssembler) normalizeAuxiliaryFrame(frame []byte) []byte {
	if s == nil || s.mode == responsesCompatOff {
		return frame
	}

	eventName, data, ok := parseSSEFrame(frame)
	if !ok || data == "[DONE]" {
		return frame
	}

	var event map[string]any
	decoder := json.NewDecoder(strings.NewReader(data))
	decoder.UseNumber()
	if err := decoder.Decode(&event); err != nil {
		return frame
	}

	eventType := stringValue(event["type"])
	if eventType == "" {
		eventType = eventName
	}
	spec, ok := responsesAuxiliarySpec(eventType)
	if !ok {
		return frame
	}

	modified := false
	if stringValue(event["type"]) == "" {
		event["type"] = eventType
		modified = true
	}

	state := s.outputForEvent(event, nil, spec.kind, outputEventAuxiliary)
	if state == nil {
		return frame
	}
	if s.normalizeContentEvent(event, state, false) {
		modified = true
	}
	if spec.contentIndexed && setIntegerDefault(event, "content_index", 0) {
		modified = true
	}
	if spec.summaryIndexed && setIntegerDefault(event, "summary_index", 0) {
		modified = true
	}
	if spec.annotationIndexed && setIntegerDefault(event, "annotation_index", 0) {
		modified = true
	}
	if spec.partialIndexed && setIntegerDefault(event, "partial_image_index", 0) {
		modified = true
	}
	if spec.outputPart && normalizeAuxiliaryOutputPart(event, state, strings.HasSuffix(eventType, ".done")) {
		modified = true
	}
	if spec.summaryPart && normalizeAuxiliarySummaryPart(event) {
		modified = true
	}

	if !modified {
		return frame
	}
	return encodeSSEEvent(firstNonEmptyString(eventName, eventType), event)
}

func normalizeAuxiliaryOutputPart(event map[string]any, state *responseOutputState, done bool) bool {
	part := jsonObject(event["part"])
	if part == nil {
		text := responseStateText(state)
		refusal := responseStateRefusal(state)
		if done && refusal != "" && text == "" {
			part = map[string]any{
				"type":    "refusal",
				"refusal": refusal,
			}
		} else {
			if !done {
				text = ""
			}
			part = map[string]any{
				"type":        "output_text",
				"text":        text,
				"annotations": []any{},
			}
		}
		event["part"] = part
		return true
	}

	modified := false
	partType := stringValue(part["type"])
	if partType == "" {
		if _, exists := part["refusal"]; exists {
			partType = "refusal"
		} else {
			partType = "output_text"
		}
		part["type"] = partType
		modified = true
	}

	switch partType {
	case "output_text":
		text, textOK := part["text"].(string)
		fallback := responseStateText(state)
		if !textOK || (done && text == "" && fallback != "") {
			if done {
				text = fallback
			} else {
				text = ""
			}
			part["text"] = text
			modified = true
		}
		if _, ok := part["annotations"].([]any); !ok {
			part["annotations"] = []any{}
			modified = true
		}
		if logprobs, exists := part["logprobs"]; exists && logprobs != nil {
			if _, ok := logprobs.([]any); !ok {
				part["logprobs"] = nil
				modified = true
			}
		}
	case "refusal":
		refusal, refusalOK := part["refusal"].(string)
		fallback := responseStateRefusal(state)
		if !refusalOK || (done && refusal == "" && fallback != "") {
			if done {
				refusal = fallback
			} else {
				refusal = ""
			}
			part["refusal"] = refusal
			modified = true
		}
	case "reasoning_text":
		if _, ok := part["text"].(string); !ok {
			part["text"] = ""
			modified = true
		}
	}

	return modified
}

func normalizeAuxiliarySummaryPart(event map[string]any) bool {
	part := jsonObject(event["part"])
	if part == nil {
		event["part"] = map[string]any{
			"type": "summary_text",
			"text": "",
		}
		return true
	}

	modified := false
	if stringValue(part["type"]) == "" {
		part["type"] = "summary_text"
		modified = true
	}
	if stringValue(part["type"]) == "summary_text" {
		if _, ok := part["text"].(string); !ok {
			part["text"] = ""
			modified = true
		}
	}
	return modified
}

func responseStateText(state *responseOutputState) string {
	if state == nil {
		return ""
	}
	if state.textDoneSeen && state.textDone != "" {
		return state.textDone
	}
	return state.textDelta.String()
}

func responseStateRefusal(state *responseOutputState) string {
	if state == nil {
		return ""
	}
	if state.refusalDoneSeen && state.refusalDone != "" {
		return state.refusalDone
	}
	return state.refusalDelta.String()
}
