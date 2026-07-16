package proxy

import (
	"encoding/json"
	"fmt"
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
	index  int
	itemID string
	callID string
	kind   string

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

	customInputDelta    strings.Builder
	customInputDone     string
	customInputDoneSeen bool
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
	if sequence, ok := integerValue(event["sequence_number"]); ok && sequence > s.maxSequence {
		s.maxSequence = sequence
	}

	switch eventType {
	case "response.created", "response.in_progress", "response.queued":
		if response := jsonObject(event["response"]); response != nil {
			s.responseSnapshot = cloneJSONObject(response)
		}
	case "response.output_item.added":
		s.captureOutputItem(event, false)
	case "response.output_item.done":
		s.captureOutputItem(event, true)
	case "response.output_text.delta":
		s.captureText(event, false)
	case "response.output_text.done":
		s.captureText(event, true)
	case "response.refusal.delta":
		s.captureRefusal(event, false)
	case "response.refusal.done":
		s.captureRefusal(event, true)
	case "response.function_call_arguments.delta":
		if s.mode == responsesCompatFull {
			s.captureFunctionArguments(event, false)
		}
	case "response.function_call_arguments.done":
		if s.mode == responsesCompatFull {
			s.captureFunctionArguments(event, true)
		}
	case "response.custom_tool_call_input.delta":
		if s.mode == responsesCompatFull {
			s.captureCustomToolInput(event, false)
		}
	case "response.custom_tool_call_input.done":
		if s.mode == responsesCompatFull {
			s.captureCustomToolInput(event, true)
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
			return encodeSSEError("proxy_invalid_terminal_response", "Responses Lite terminal event did not contain a response object")
		}
		if modified {
			return encodeSSEEvent(firstNonEmptyString(eventName, stringValue(event["type"])), event)
		}
		return encodeSSEEvent(eventName, event)
	}

	patched, err := s.mergeResponse(response, status, strict)
	if err != nil {
		s.terminalSeen = true
		return encodeSSEError("proxy_incomplete_output", err.Error())
	}
	if strict && !responseHasUsableOutput(response) {
		s.terminalSeen = true
		return encodeSSEError("proxy_missing_terminal_output", "Responses Lite completed without a usable final text or tool call")
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
		return encodeSSEError("proxy_stream_state_error", s.stateErr.Error())
	}
	if s.responseSnapshot == nil {
		s.terminalSeen = true
		return encodeSSEError("proxy_missing_terminal_response", "Responses Lite stream ended without a terminal response snapshot")
	}
	response := cloneJSONObject(s.responseSnapshot)
	if response == nil {
		s.terminalSeen = true
		return encodeSSEError("proxy_invalid_response_snapshot", "Responses Lite response snapshot could not be cloned")
	}
	patched, err := s.mergeResponse(response, "completed", true)
	if err != nil {
		s.terminalSeen = true
		return encodeSSEError("proxy_incomplete_output", err.Error())
	}
	if !patched && !responseHasUsableOutput(response) {
		s.terminalSeen = true
		return encodeSSEError("proxy_missing_terminal_output", "Responses Lite stream ended without a usable final text or tool call")
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

func (s *responsesSSEAssembler) captureOutputItem(event map[string]any, done bool) {
	item := jsonObject(event["item"])
	if item == nil {
		return
	}
	eventKind := outputEventItemAdded
	if done {
		eventKind = outputEventItemDone
	}
	state := s.outputForEvent(event, item, stringValue(item["type"]), eventKind)
	if state == nil {
		return
	}
	if done {
		state.doneItem = cloneJSONObject(item)
	} else {
		state.addedItem = cloneJSONObject(item)
	}
}

func (s *responsesSSEAssembler) captureText(event map[string]any, done bool) {
	eventKind := outputEventTextDelta
	if done {
		eventKind = outputEventTextDone
	}
	state := s.outputForEvent(event, nil, "message", eventKind)
	if state == nil {
		return
	}
	if done {
		state.textDone = stringValue(event["text"])
		state.textDoneSeen = true
		s.addStateBytes(len(state.textDone))
		return
	}
	delta := stringValue(event["delta"])
	if delta != "" {
		state.textDelta.WriteString(delta)
		s.addStateBytes(len(delta))
	}
}

func (s *responsesSSEAssembler) captureRefusal(event map[string]any, done bool) {
	eventKind := outputEventRefusalDelta
	if done {
		eventKind = outputEventRefusalDone
	}
	state := s.outputForEvent(event, nil, "message", eventKind)
	if state == nil {
		return
	}
	if done {
		state.refusalDone = stringValue(event["refusal"])
		state.refusalDoneSeen = true
		s.addStateBytes(len(state.refusalDone))
		return
	}
	delta := stringValue(event["delta"])
	if delta != "" {
		state.refusalDelta.WriteString(delta)
		s.addStateBytes(len(delta))
	}
}

func (s *responsesSSEAssembler) captureFunctionArguments(event map[string]any, done bool) {
	eventKind := outputEventFunctionArgumentsDelta
	if done {
		eventKind = outputEventFunctionArgumentsDone
	}
	state := s.outputForEvent(event, nil, "function_call", eventKind)
	if state == nil {
		return
	}
	if done {
		state.argumentsDone = stringValue(event["arguments"])
		state.argumentsDoneSeen = true
		s.addStateBytes(len(state.argumentsDone))
		return
	}
	delta := stringValue(event["delta"])
	if delta != "" {
		state.argumentsDelta.WriteString(delta)
		s.addStateBytes(len(delta))
	}
}

func (s *responsesSSEAssembler) captureCustomToolInput(event map[string]any, done bool) {
	eventKind := outputEventCustomInputDelta
	if done {
		eventKind = outputEventCustomInputDone
	}
	state := s.outputForEvent(event, nil, "custom_tool_call", eventKind)
	if state == nil {
		return
	}
	if done {
		state.customInputDone = stringValue(event["input"])
		state.customInputDoneSeen = true
		s.addStateBytes(len(state.customInputDone))
		return
	}
	delta := stringValue(event["delta"])
	if delta != "" {
		state.customInputDelta.WriteString(delta)
		s.addStateBytes(len(delta))
	}
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
	if itemID != "" && s.itemID != "" && itemID != s.itemID {
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

func (s *responsesSSEAssembler) addStateBytes(size int) {
	if size <= 0 || s.stateErr != nil {
		return
	}
	s.stateBytes += size
	if s.stateBytes > defaultMaxResponsesSSEStateBytes {
		s.stateErr = fmt.Errorf("Responses Lite stream state exceeded %d bytes", defaultMaxResponsesSSEStateBytes)
	}
}
