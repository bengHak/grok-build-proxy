package proxy

import (
	"encoding/json"
	"fmt"
	"log/slog"
	"strings"
)

const defaultMaxResponsesSSEStateBytes = 16 << 20

type responsesCompatMode uint8

const (
	responsesCompatOff responsesCompatMode = iota
	responsesCompatText
	responsesCompatFull
)

func parseResponsesCompatMode(value string) responsesCompatMode {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "off", "false", "0":
		return responsesCompatOff
	case "text", "legacy":
		return responsesCompatText
	default:
		return responsesCompatFull
	}
}

type responseStreamOutputEvent uint8

const (
	outputEventItemAdded responseStreamOutputEvent = iota
	outputEventItemDone
	outputEventTextDelta
	outputEventTextDone
	outputEventRefusalDelta
	outputEventRefusalDone
	outputEventFunctionArgumentsDelta
	outputEventFunctionArgumentsDone
	outputEventCustomInputDelta
	outputEventCustomInputDone
)

type responsesSSEAssembler struct {
	mode              responsesCompatMode
	outputs           map[int]*responseOutputState
	indexByItemID     map[string]int
	indexByCallID     map[string]int
	nextImplicitIndex int
	responseSnapshot  map[string]any
	terminalSeen      bool
	doneSeen          bool
	maxSequence       int64
	stateBytes        int
	stateErr          error
}

type responseOutputState struct {
	index           int
	itemID          string
	itemIDSynthetic bool
	callID          string
	kind            string

	addedItem map[string]any
	doneItem  map[string]any

	textDelta    strings.Builder
	textDone     string
	textDoneSeen bool

	refusalDelta    strings.Builder
	refusalDone     string
	refusalDoneSeen bool

	argumentsDelta    strings.Builder
	argumentsDone     string
	argumentsDoneSeen bool

	customInputDelta           strings.Builder
	customInputDone            string
	customInputDoneSeen        bool
	customInputDonePresent     bool
	customDoneItemInputPresent bool
}

func newResponsesSSEAssembler(mode responsesCompatMode) *responsesSSEAssembler {
	return &responsesSSEAssembler{
		mode:          mode,
		outputs:       make(map[int]*responseOutputState),
		indexByItemID: make(map[string]int),
		indexByCallID: make(map[string]int),
	}
}

func (s *responsesSSEAssembler) transformFrame(frame []byte) []byte {
	if s.mode == responsesCompatOff {
		return frame
	}
	eventName, data, ok := parseSSEFrame(frame)
	if !ok {
		return frame
	}
	if data == "[DONE]" {
		s.doneSeen = true
		return append(s.finishBeforeDone(), frame...)
	}

	var event map[string]any
	decoder := json.NewDecoder(strings.NewReader(data))
	decoder.UseNumber()
	if err := decoder.Decode(&event); err != nil {
		return frame
	}

	eventType := stringValue(event["type"])
	modified := false
	if eventType == "" && eventName != "" {
		eventType = eventName
		if strings.HasPrefix(eventName, "response.") {
			event["type"] = eventType
			modified = true
		}
	}
	if s.normalizeSequence(event, eventType) {
		modified = true
	}

	switch eventType {
	case "response.created", "response.in_progress", "response.queued":
		if response := jsonObject(event["response"]); response != nil {
			s.responseSnapshot = cloneJSONObject(response)
		}
	case "response.output_item.added":
		modified = s.captureOutputItem(event, false) || modified
	case "response.output_item.done":
		modified = s.captureOutputItem(event, true) || modified
	case "response.output_text.delta":
		modified = s.captureText(event, false) || modified
	case "response.output_text.done":
		modified = s.captureText(event, true) || modified
	case "response.refusal.delta":
		modified = s.captureRefusal(event, false) || modified
	case "response.refusal.done":
		modified = s.captureRefusal(event, true) || modified
	case "response.function_call_arguments.delta":
		if s.mode == responsesCompatFull {
			modified = s.captureFunctionArguments(event, false) || modified
		}
	case "response.function_call_arguments.done":
		if s.mode == responsesCompatFull {
			modified = s.captureFunctionArguments(event, true) || modified
		}
	case "response.custom_tool_call_input.delta":
		if s.mode == responsesCompatFull {
			modified = s.captureCustomToolInput(event, false) || modified
		}
	case "response.custom_tool_call_input.done":
		if s.mode == responsesCompatFull {
			modified = s.captureCustomToolInput(event, true) || modified
		}
	case "response.completed":
		return s.transformTerminal(eventName, event, "completed", modified, true)
	case "response.incomplete":
		return s.transformTerminal(eventName, event, "incomplete", modified, false)
	case "response.failed", "error":
		s.terminalSeen = true
	}

	if !modified {
		return frame
	}
	return encodeSSEEvent(firstNonEmptyString(eventName, eventType), event)
}

func (s *responsesSSEAssembler) finishBeforeDone() []byte {
	if s.terminalSeen || s.mode == responsesCompatOff {
		return nil
	}
	return s.synthesizeTerminal()
}

func (s *responsesSSEAssembler) finishAtEOF() []byte {
	if s.terminalSeen || s.doneSeen || s.mode == responsesCompatOff {
		return nil
	}
	return s.synthesizeTerminal()
}

func (s *responsesSSEAssembler) transformTerminal(eventName string, event map[string]any, status string, modified, strict bool) []byte {
	response := jsonObject(event["response"])
	if response == nil {
		s.terminalSeen = true
		if strict {
			return s.encodeError("proxy_invalid_terminal_response", "Responses Lite terminal event did not contain a response object")
		}
		if modified {
			return encodeSSEEvent(firstNonEmptyString(eventName, stringValue(event["type"])), event)
		}
		return encodeSSEEvent(eventName, event)
	}

	patched, err := s.mergeResponse(response, status, strict)
	if err != nil {
		s.terminalSeen = true
		return s.encodeError("proxy_incomplete_output", err.Error())
	}
	if strict && !responseHasUsableOutput(response) {
		s.terminalSeen = true
		return s.encodeError("proxy_missing_terminal_output", "Responses Lite completed without a usable final text or tool call")
	}
	s.terminalSeen = true
	if modified || patched {
		return encodeSSEEvent(firstNonEmptyString(eventName, stringValue(event["type"])), event)
	}
	return encodeSSEEvent(eventName, event)
}

func (s *responsesSSEAssembler) synthesizeTerminal() []byte {
	if s.stateErr != nil {
		s.terminalSeen = true
		return s.encodeError("proxy_stream_state_error", s.stateErr.Error())
	}
	if s.responseSnapshot == nil {
		s.terminalSeen = true
		return s.encodeError("proxy_missing_terminal_response", "Responses Lite stream ended without a terminal response snapshot")
	}
	response := cloneJSONObject(s.responseSnapshot)
	if response == nil {
		s.terminalSeen = true
		return s.encodeError("proxy_invalid_response_snapshot", "Responses Lite response snapshot could not be cloned")
	}
	patched, err := s.mergeResponse(response, "completed", true)
	if err != nil {
		s.terminalSeen = true
		return s.encodeError("proxy_incomplete_output", err.Error())
	}
	if !patched && !responseHasUsableOutput(response) {
		s.terminalSeen = true
		return s.encodeError("proxy_missing_terminal_output", "Responses Lite stream ended without a usable final text or tool call")
	}
	response["status"] = "completed"
	s.maxSequence++
	s.terminalSeen = true
	return encodeSSEEvent("response.completed", map[string]any{
		"type":            "response.completed",
		"sequence_number": s.maxSequence,
		"response":        response,
	})
}

func (s *responsesSSEAssembler) encodeError(kind, message string) []byte {
	slog.Warn(
		"responses lite normalization failed",
		"error_type", kind,
		"error", message,
		"response_id", stringValue(s.responseSnapshot["id"]),
		"output_states", len(s.outputs),
		"state_bytes", s.stateBytes,
		"done_seen", s.doneSeen,
	)
	return encodeSSEError(kind, message)
}

func (s *responsesSSEAssembler) captureOutputItem(event map[string]any, done bool) bool {
	item := jsonObject(event["item"])
	if item == nil {
		return false
	}
	kind := stringValue(item["type"])
	_, customInputPresent := item["input"].(string)
	eventKind := outputEventItemAdded
	if done {
		eventKind = outputEventItemDone
	}
	state := s.outputForEvent(event, item, kind, eventKind)
	if state == nil {
		return false
	}
	modified := s.normalizeOutputItemEvent(event, item, state, done)
	if done {
		state.doneItem = cloneJSONObject(item)
		if kind == "custom_tool_call" {
			state.customDoneItemInputPresent = customInputPresent
		}
	} else {
		state.addedItem = cloneJSONObject(item)
	}
	return modified
}

func (s *responsesSSEAssembler) captureText(event map[string]any, done bool) bool {
	eventKind := outputEventTextDelta
	if done {
		eventKind = outputEventTextDone
	}
	state := s.outputForEvent(event, nil, "message", eventKind)
	if state == nil {
		return false
	}
	modified := s.normalizeContentEvent(event, state, true)
	if done {
		text := stringValue(event["text"])
		if text == "" {
			text = state.textDelta.String()
			if text != "" {
				event["text"] = text
				modified = true
			}
		}
		state.textDone = text
		state.textDoneSeen = true
		s.addStateBytes(len(state.textDone))
		return modified
	}
	delta := stringValue(event["delta"])
	if delta != "" {
		state.textDelta.WriteString(delta)
		s.addStateBytes(len(delta))
	}
	return modified
}

func (s *responsesSSEAssembler) captureRefusal(event map[string]any, done bool) bool {
	eventKind := outputEventRefusalDelta
	if done {
		eventKind = outputEventRefusalDone
	}
	state := s.outputForEvent(event, nil, "message", eventKind)
	if state == nil {
		return false
	}
	modified := s.normalizeContentEvent(event, state, true)
	if done {
		refusal := stringValue(event["refusal"])
		if refusal == "" {
			refusal = state.refusalDelta.String()
			if refusal != "" {
				event["refusal"] = refusal
				modified = true
			}
		}
		state.refusalDone = refusal
		state.refusalDoneSeen = true
		s.addStateBytes(len(state.refusalDone))
		return modified
	}
	delta := stringValue(event["delta"])
	if delta != "" {
		state.refusalDelta.WriteString(delta)
		s.addStateBytes(len(delta))
	}
	return modified
}

func (s *responsesSSEAssembler) captureFunctionArguments(event map[string]any, done bool) bool {
	eventKind := outputEventFunctionArgumentsDelta
	if done {
		eventKind = outputEventFunctionArgumentsDone
	}
	state := s.outputForEvent(event, nil, "function_call", eventKind)
	if state == nil {
		return false
	}
	modified := s.normalizeContentEvent(event, state, false)
	if done {
		arguments := stringValue(event["arguments"])
		fallback := state.argumentsDelta.String()
		if (arguments == "" || !json.Valid([]byte(arguments))) && fallback != "" && json.Valid([]byte(fallback)) {
			arguments = fallback
			event["arguments"] = arguments
			modified = true
		}
		state.argumentsDone = arguments
		state.argumentsDoneSeen = true
		s.addStateBytes(len(state.argumentsDone))
		return modified
	}
	delta := stringValue(event["delta"])
	if delta != "" {
		state.argumentsDelta.WriteString(delta)
		s.addStateBytes(len(delta))
	}
	return modified
}

func (s *responsesSSEAssembler) captureCustomToolInput(event map[string]any, done bool) bool {
	eventKind := outputEventCustomInputDelta
	if done {
		eventKind = outputEventCustomInputDone
	}
	state := s.outputForEvent(event, nil, "custom_tool_call", eventKind)
	if state == nil {
		return false
	}
	modified := s.normalizeContentEvent(event, state, false)
	if done {
		input, present := event["input"].(string)
		fallback := state.customInputDelta.String()
		if (!present || input == "") && fallback != "" {
			input = fallback
			present = true
			event["input"] = input
			modified = true
		}
		state.customInputDone = input
		state.customInputDonePresent = present
		state.customInputDoneSeen = true
		s.addStateBytes(len(state.customInputDone))
		return modified
	}
	delta := stringValue(event["delta"])
	if delta != "" {
		state.customInputDelta.WriteString(delta)
		s.addStateBytes(len(delta))
	}
	return modified
}

func (s *responsesSSEAssembler) outputForEvent(event, item map[string]any, kind string, eventKind responseStreamOutputEvent) *responseOutputState {
	itemID := firstNonEmptyString(stringValue(event["item_id"]), itemString(item, "id"))
	callID := firstNonEmptyString(stringValue(event["call_id"]), itemString(item, "call_id"))
	kind = firstNonEmptyString(kind, itemString(item, "type"))
	explicitIndex, hasExplicitIndex := integerValue(event["output_index"])

	if state := s.mappedOutput(itemID, callID); state != nil {
		if hasExplicitIndex && state.index != int(explicitIndex) && s.outputs[int(explicitIndex)] == nil {
			state = s.reindexOutput(state, int(explicitIndex))
		}
		s.bindOutputState(state, itemID, callID, kind)
		return state
	}

	if hasExplicitIndex {
		index := int(explicitIndex)
		if state := s.outputs[index]; state != nil && state.acceptsEvent(itemID, callID, kind, eventKind) {
			s.bindOutputState(state, itemID, callID, kind)
			return state
		}
		if s.outputs[index] == nil {
			if state := s.uniqueOutputCandidate(itemID, callID, kind, eventKind); state != nil {
				state = s.reindexOutput(state, index)
				s.bindOutputState(state, itemID, callID, kind)
				return state
			}
			state := s.newOutputState(index)
			s.bindOutputState(state, itemID, callID, kind)
			return state
		}
	}

	if state := s.uniqueOutputCandidate(itemID, callID, kind, eventKind); state != nil {
		s.bindOutputState(state, itemID, callID, kind)
		return state
	}

	state := s.allocateOutputState()
	s.bindOutputState(state, itemID, callID, kind)
	return state
}

func (s *responsesSSEAssembler) mappedOutput(itemID, callID string) *responseOutputState {
	if itemID != "" {
		if index, exists := s.indexByItemID[itemID]; exists {
			return s.outputs[index]
		}
	}
	if callID != "" {
		if index, exists := s.indexByCallID[callID]; exists {
			return s.outputs[index]
		}
	}
	return nil
}

func (s *responsesSSEAssembler) uniqueOutputCandidate(itemID, callID, kind string, eventKind responseStreamOutputEvent) *responseOutputState {
	var candidate *responseOutputState
	for _, state := range s.outputs {
		if !state.acceptsEvent(itemID, callID, kind, eventKind) {
			continue
		}
		if candidate != nil {
			return nil
		}
		candidate = state
	}
	return candidate
}

func (s *responseOutputState) acceptsEvent(itemID, callID, kind string, eventKind responseStreamOutputEvent) bool {
	if kind != "" && s.kind != "" && kind != s.kind {
		return false
	}
	if itemID != "" && s.itemID != "" && itemID != s.itemID && !s.itemIDSynthetic {
		return false
	}
	if callID != "" && s.callID != "" && callID != s.callID {
		return false
	}
	switch eventKind {
	case outputEventItemAdded:
		return s.addedItem == nil && s.doneItem == nil
	case outputEventItemDone:
		return s.doneItem == nil
	case outputEventTextDelta:
		return s.doneItem == nil && !s.textDoneSeen
	case outputEventTextDone:
		return s.doneItem == nil && !s.textDoneSeen
	case outputEventRefusalDelta:
		return s.doneItem == nil && !s.refusalDoneSeen
	case outputEventRefusalDone:
		return s.doneItem == nil && !s.refusalDoneSeen
	case outputEventFunctionArgumentsDelta:
		return s.doneItem == nil && !s.argumentsDoneSeen
	case outputEventFunctionArgumentsDone:
		return s.doneItem == nil && !s.argumentsDoneSeen
	case outputEventCustomInputDelta:
		return s.doneItem == nil && !s.customInputDoneSeen
	case outputEventCustomInputDone:
		return s.doneItem == nil && !s.customInputDoneSeen
	default:
		return true
	}
}

func (s *responsesSSEAssembler) newOutputState(index int) *responseOutputState {
	state := &responseOutputState{index: index}
	s.outputs[index] = state
	return state
}

func (s *responsesSSEAssembler) allocateOutputState() *responseOutputState {
	for {
		index := s.nextImplicitIndex
		s.nextImplicitIndex++
		if s.outputs[index] == nil {
			return s.newOutputState(index)
		}
	}
}

func (s *responsesSSEAssembler) reindexOutput(state *responseOutputState, index int) *responseOutputState {
	if state == nil || state.index == index || s.outputs[index] != nil {
		return state
	}
	delete(s.outputs, state.index)
	state.index = index
	s.outputs[index] = state
	if state.itemID != "" {
		s.indexByItemID[state.itemID] = index
	}
	if state.callID != "" {
		s.indexByCallID[state.callID] = index
	}
	return state
}

func (s *responsesSSEAssembler) bindOutputState(state *responseOutputState, itemID, callID, kind string) {
	if state == nil {
		return
	}
	if itemID != "" {
		replacingItemID := state.itemID != "" && state.itemID != itemID
		if state.itemIDSynthetic && replacingItemID {
			delete(s.indexByItemID, state.itemID)
		}
		if state.itemID == "" || replacingItemID {
			state.itemIDSynthetic = false
		}
		state.itemID = itemID
		s.indexByItemID[itemID] = state.index
	}
	if callID != "" {
		state.callID = callID
		s.indexByCallID[callID] = state.index
	}
	if state.kind == "" && kind != "" {
		state.kind = kind
	}
}

func (s *responsesSSEAssembler) markSyntheticItemID(state *responseOutputState, itemID string) {
	if state == nil || itemID == "" || state.itemID != itemID {
		return
	}
	state.itemIDSynthetic = true
}

func (s *responsesSSEAssembler) normalizeSequence(event map[string]any, eventType string) bool {
	if !strings.HasPrefix(eventType, "response.") {
		return false
	}
	if sequence, ok := integerValue(event["sequence_number"]); ok {
		if sequence > s.maxSequence {
			s.maxSequence = sequence
		}
		return false
	}
	s.maxSequence++
	event["sequence_number"] = s.maxSequence
	return true
}

func (s *responsesSSEAssembler) normalizeOutputItemEvent(event, item map[string]any, state *responseOutputState, done bool) bool {
	modified := setIntegerDefault(event, "output_index", int64(state.index))
	kind := stringValue(item["type"])
	itemID := firstNonEmptyString(stringValue(item["id"]), state.itemID)
	callID := firstNonEmptyString(stringValue(item["call_id"]), state.callID)
	if itemID == "" {
		switch kind {
		case "function_call":
			itemID = syntheticToolItemID("fc_", callID)
		case "custom_tool_call":
			itemID = syntheticToolItemID("ct_", callID)
		case "message":
			itemID = s.syntheticMessageItemID(state.index)
		}
	}
	syntheticItemID := stringValue(item["id"]) == "" && state.itemID == "" && itemID != ""
	if itemID != "" && stringValue(item["id"]) == "" {
		item["id"] = itemID
		modified = true
	}
	s.bindOutputState(state, itemID, callID, kind)
	if syntheticItemID {
		s.markSyntheticItemID(state, itemID)
	}

	switch kind {
	case "message":
		if stringValue(item["role"]) == "" {
			item["role"] = "assistant"
			modified = true
		}
		if stringValue(item["status"]) == "" {
			if done {
				item["status"] = "completed"
			} else {
				item["status"] = "in_progress"
			}
			modified = true
		}
		if _, exists := item["content"]; !exists || item["content"] == nil {
			item["content"] = []any{}
			modified = true
		}
		if normalizeMessageContent(jsonArray(item["content"])) {
			modified = true
		}
	case "function_call":
		if callID != "" && stringValue(item["call_id"]) == "" {
			item["call_id"] = callID
			modified = true
		}
	case "custom_tool_call":
		if callID != "" && stringValue(item["call_id"]) == "" {
			item["call_id"] = callID
			modified = true
		}
		if _, exists := item["input"]; !exists {
			item["input"] = ""
			modified = true
		}
	}
	return modified
}

func (s *responsesSSEAssembler) normalizeContentEvent(event map[string]any, state *responseOutputState, contentIndexed bool) bool {
	modified := setIntegerDefault(event, "output_index", int64(state.index))
	itemID := firstNonEmptyString(stringValue(event["item_id"]), state.itemID)
	if itemID == "" {
		switch state.kind {
		case "function_call":
			itemID = syntheticToolItemID("fc_", state.callID)
		case "custom_tool_call":
			itemID = syntheticToolItemID("ct_", state.callID)
		default:
			itemID = s.syntheticMessageItemID(state.index)
		}
	}
	syntheticItemID := stringValue(event["item_id"]) == "" && state.itemID == "" && itemID != ""
	if itemID != "" && stringValue(event["item_id"]) == "" {
		event["item_id"] = itemID
		modified = true
	}
	s.bindOutputState(state, itemID, stringValue(event["call_id"]), state.kind)
	if syntheticItemID {
		s.markSyntheticItemID(state, itemID)
	}
	if contentIndexed && setIntegerDefault(event, "content_index", 0) {
		modified = true
	}
	return modified
}

func (s *responsesSSEAssembler) syntheticMessageItemID(index int) string {
	responseID := stringValue(s.responseSnapshot["id"])
	base := strings.TrimPrefix(responseID, "resp_")
	if base == "" {
		base = "grok_build_proxy"
	}
	return fmt.Sprintf("msg_%s_%d", base, index)
}

func setIntegerDefault(object map[string]any, key string, value int64) bool {
	if _, ok := integerValue(object[key]); ok {
		return false
	}
	object[key] = value
	return true
}

func normalizeMessageContent(content []any) bool {
	modified := false
	for _, raw := range content {
		part := jsonObject(raw)
		if stringValue(part["type"]) != "output_text" {
			continue
		}
		if _, exists := part["annotations"]; !exists || part["annotations"] == nil {
			part["annotations"] = []any{}
			modified = true
		}
	}
	return modified
}

func (s *responsesSSEAssembler) addStateBytes(size int) {
	if size <= 0 || s.stateErr != nil {
		return
	}
	s.stateBytes += size
	if s.stateBytes > defaultMaxResponsesSSEStateBytes {
		s.stateErr = fmt.Errorf("Responses Lite stream state exceeded %d bytes", defaultMaxResponsesSSEStateBytes)
	}
}
