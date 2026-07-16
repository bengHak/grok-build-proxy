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

type responsesSSEAssembler struct {
	mode             responsesCompatMode
	outputs          map[int]*responseOutputState
	indexByItemID    map[string]int
	responseSnapshot map[string]any
	terminalSeen     bool
	doneSeen         bool
	maxSequence      int64
	stateBytes       int
	stateErr         error
}

type responseOutputState struct {
	index  int
	itemID string
	kind   string

	addedItem map[string]any
	doneItem  map[string]any

	textDelta strings.Builder
	textDone  string

	refusalDelta strings.Builder
	refusalDone  string

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
	state := s.outputForEvent(event)
	if state == nil {
		return
	}
	if id := stringValue(item["id"]); id != "" {
		state.itemID = id
		s.indexByItemID[id] = state.index
	}
	if kind := stringValue(item["type"]); kind != "" {
		state.kind = kind
	}
	if done {
		state.doneItem = cloneJSONObject(item)
	} else {
		state.addedItem = cloneJSONObject(item)
	}
}

func (s *responsesSSEAssembler) captureText(event map[string]any, done bool) {
	state := s.outputForEvent(event)
	if state == nil {
		return
	}
	if state.kind == "" {
		state.kind = "message"
	}
	if done {
		state.textDone = stringValue(event["text"])
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
	state := s.outputForEvent(event)
	if state == nil {
		return
	}
	if state.kind == "" {
		state.kind = "message"
	}
	if done {
		state.refusalDone = stringValue(event["refusal"])
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
	state := s.outputForEvent(event)
	if state == nil {
		return
	}
	if state.kind == "" {
		state.kind = "function_call"
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
	state := s.outputForEvent(event)
	if state == nil {
		return
	}
	if state.kind == "" {
		state.kind = "custom_tool_call"
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

func (s *responsesSSEAssembler) outputForEvent(event map[string]any) *responseOutputState {
	itemID := stringValue(event["item_id"])
	index, hasIndex := integerValue(event["output_index"])
	if !hasIndex && itemID != "" {
		mapped, exists := s.indexByItemID[itemID]
		if exists {
			index = int64(mapped)
			hasIndex = true
		}
	}
	if !hasIndex {
		return nil
	}
	key := int(index)
	state := s.outputs[key]
	if state == nil {
		state = &responseOutputState{index: key}
		s.outputs[key] = state
	}
	if itemID != "" {
		state.itemID = itemID
		s.indexByItemID[itemID] = key
	}
	return state
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
