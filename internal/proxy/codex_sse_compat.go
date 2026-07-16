package proxy

import (
	"bufio"
	"bytes"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"sort"
	"strconv"
	"strings"
)

const responsesLiteHeader = "X-OpenAI-Internal-Codex-Responses-Lite"

func shouldNormalizeCodexSSEResponse(req *http.Request, resp *http.Response) bool {
	if req == nil || resp == nil || resp.Body == nil || resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return false
	}
	if !strings.EqualFold(strings.TrimSpace(req.Header.Get(responsesLiteHeader)), "true") {
		return false
	}
	return strings.Contains(strings.ToLower(resp.Header.Get("Content-Type")), "text/event-stream")
}

func newResponsesLiteSSEBody(source io.ReadCloser) io.ReadCloser {
	return &responsesLiteSSEBody{
		source: source,
		reader: bufio.NewReader(source),
		state:  responsesLiteSSEState{outputs: map[int]*streamedOutputText{}},
	}
}

type responsesLiteSSEBody struct {
	source      io.ReadCloser
	reader      *bufio.Reader
	state       responsesLiteSSEState
	pending     []byte
	finished    bool
	terminalErr error
}

func (b *responsesLiteSSEBody) Read(p []byte) (int, error) {
	for len(b.pending) == 0 && !b.finished {
		frame, err := readSSEFrame(b.reader)
		if len(frame) > 0 {
			b.pending = append(b.pending, b.state.transformFrame(frame)...)
		}
		if err != nil {
			b.finished = true
			if errors.Is(err, io.EOF) {
				b.pending = append(b.pending, b.state.synthesizeCompleted()...)
			} else {
				b.terminalErr = err
			}
		}
	}

	if len(b.pending) > 0 {
		n := copy(p, b.pending)
		b.pending = b.pending[n:]
		return n, nil
	}
	if b.terminalErr != nil {
		err := b.terminalErr
		b.terminalErr = nil
		return 0, err
	}
	return 0, io.EOF
}

func (b *responsesLiteSSEBody) Close() error {
	return b.source.Close()
}

func readSSEFrame(reader *bufio.Reader) ([]byte, error) {
	var frame bytes.Buffer
	for {
		line, err := reader.ReadString('\n')
		if line != "" {
			frame.WriteString(line)
		}
		if line == "\n" || line == "\r\n" {
			return frame.Bytes(), err
		}
		if err != nil {
			return frame.Bytes(), err
		}
	}
}

type streamedOutputText struct {
	itemID   string
	delta    strings.Builder
	doneText string
}

func (s *streamedOutputText) text() string {
	if s.doneText != "" {
		return s.doneText
	}
	return s.delta.String()
}

type responsesLiteSSEState struct {
	outputs          map[int]*streamedOutputText
	responseSnapshot map[string]any
	terminalSeen     bool
	maxSequence      int64
}

func (s *responsesLiteSSEState) transformFrame(frame []byte) []byte {
	eventName, data, ok := parseSSEFrame(frame)
	if !ok {
		return frame
	}
	if data == "[DONE]" {
		completed := s.synthesizeCompleted()
		return append(completed, frame...)
	}

	var event map[string]any
	if err := json.Unmarshal([]byte(data), &event); err != nil {
		return frame
	}

	eventType, _ := event["type"].(string)
	modified := false
	if eventType == "" && strings.HasPrefix(eventName, "response.") {
		eventType = eventName
		event["type"] = eventType
		modified = true
	}
	if sequence, ok := integerValue(event["sequence_number"]); ok && sequence > s.maxSequence {
		s.maxSequence = sequence
	}

	switch eventType {
	case "response.created", "response.in_progress", "response.queued":
		if response, ok := event["response"].(map[string]any); ok {
			s.responseSnapshot = cloneJSONObject(response)
		}
	case "response.output_text.delta":
		s.captureTextDelta(event)
	case "response.output_text.done":
		s.captureTextDone(event)
	case "response.completed", "response.incomplete":
		if response, ok := event["response"].(map[string]any); ok {
			status := "completed"
			if eventType == "response.incomplete" {
				status = "incomplete"
			}
			if s.patchResponse(response, status) {
				modified = true
			}
		}
		s.terminalSeen = true
	case "response.failed", "error":
		s.terminalSeen = true
	}

	if !modified {
		return frame
	}
	return encodeSSEEvent(firstNonEmpty(eventName, eventType), event)
}

func (s *responsesLiteSSEState) captureTextDelta(event map[string]any) {
	delta, _ := event["delta"].(string)
	if delta == "" {
		return
	}
	outputIndex, _ := integerValue(event["output_index"])
	entry := s.output(outputIndex)
	if itemID, _ := event["item_id"].(string); itemID != "" {
		entry.itemID = itemID
	}
	entry.delta.WriteString(delta)
}

func (s *responsesLiteSSEState) captureTextDone(event map[string]any) {
	text, _ := event["text"].(string)
	if text == "" {
		return
	}
	outputIndex, _ := integerValue(event["output_index"])
	entry := s.output(outputIndex)
	if itemID, _ := event["item_id"].(string); itemID != "" {
		entry.itemID = itemID
	}
	entry.doneText = text
}

func (s *responsesLiteSSEState) output(index int64) *streamedOutputText {
	key := int(index)
	entry := s.outputs[key]
	if entry == nil {
		entry = &streamedOutputText{}
		s.outputs[key] = entry
	}
	return entry
}

func (s *responsesLiteSSEState) synthesizeCompleted() []byte {
	if s.terminalSeen || s.responseSnapshot == nil || !s.hasStreamedText() {
		return nil
	}
	response := cloneJSONObject(s.responseSnapshot)
	if response == nil || !s.patchResponse(response, "completed") {
		return nil
	}
	s.terminalSeen = true
	s.maxSequence++
	return encodeSSEEvent("response.completed", map[string]any{
		"type":            "response.completed",
		"sequence_number": s.maxSequence,
		"response":        response,
	})
}

func (s *responsesLiteSSEState) hasStreamedText() bool {
	for _, output := range s.outputs {
		if output.text() != "" {
			return true
		}
	}
	return false
}

func (s *responsesLiteSSEState) patchResponse(response map[string]any, status string) bool {
	if responseHasVisibleText(response) || !s.hasStreamedText() {
		return false
	}

	output, _ := response["output"].([]any)
	indexes := make([]int, 0, len(s.outputs))
	for index := range s.outputs {
		indexes = append(indexes, index)
	}
	sort.Ints(indexes)

	patched := false
	for _, index := range indexes {
		streamed := s.outputs[index]
		text := streamed.text()
		if text == "" {
			continue
		}
		output = injectOutputText(output, index, streamed.itemID, responseID(response), text)
		patched = true
	}
	if !patched {
		return false
	}
	response["output"] = output
	response["status"] = status
	return true
}

func injectOutputText(output []any, outputIndex int, itemID, responseID, text string) []any {
	var message map[string]any
	if outputIndex >= 0 && outputIndex < len(output) {
		message, _ = output[outputIndex].(map[string]any)
		if message != nil && message["type"] != "message" {
			message = nil
		}
	}
	if message == nil && itemID != "" {
		for _, raw := range output {
			candidate, _ := raw.(map[string]any)
			if candidate["type"] == "message" && candidate["id"] == itemID {
				message = candidate
				break
			}
		}
	}

	content := map[string]any{
		"type":        "output_text",
		"text":        text,
		"annotations": []any{},
		"logprobs":    []any{},
	}
	if message != nil {
		parts, _ := message["content"].([]any)
		message["content"] = append(parts, content)
		message["role"] = "assistant"
		message["status"] = "completed"
		return output
	}

	if itemID == "" {
		itemID = "msg_" + strings.TrimPrefix(responseID, "resp_")
		if itemID == "msg_" {
			itemID = "msg_grok_build_proxy"
		}
	}
	return append(output, map[string]any{
		"id":      itemID,
		"type":    "message",
		"status":  "completed",
		"role":    "assistant",
		"content": []any{content},
	})
}

func responseHasVisibleText(response map[string]any) bool {
	output, _ := response["output"].([]any)
	for _, rawItem := range output {
		item, _ := rawItem.(map[string]any)
		if item["type"] != "message" {
			continue
		}
		content, _ := item["content"].([]any)
		for _, rawPart := range content {
			part, _ := rawPart.(map[string]any)
			typeName, _ := part["type"].(string)
			if typeName != "output_text" && typeName != "refusal" {
				continue
			}
			text, _ := part["text"].(string)
			if text == "" {
				text, _ = part["refusal"].(string)
			}
			if text != "" {
				return true
			}
		}
	}
	return false
}

func responseID(response map[string]any) string {
	id, _ := response["id"].(string)
	return id
}

func parseSSEFrame(frame []byte) (eventName, data string, ok bool) {
	var dataLines []string
	for _, rawLine := range strings.Split(string(frame), "\n") {
		line := strings.TrimSuffix(rawLine, "\r")
		switch {
		case strings.HasPrefix(line, "event:"):
			eventName = strings.TrimSpace(strings.TrimPrefix(line, "event:"))
		case strings.HasPrefix(line, "data:"):
			value := strings.TrimPrefix(line, "data:")
			value = strings.TrimPrefix(value, " ")
			dataLines = append(dataLines, value)
		}
	}
	if len(dataLines) == 0 {
		return eventName, "", false
	}
	return eventName, strings.Join(dataLines, "\n"), true
}

func encodeSSEEvent(eventName string, event map[string]any) []byte {
	encoded, err := json.Marshal(event)
	if err != nil {
		return nil
	}
	var frame bytes.Buffer
	if eventName != "" {
		frame.WriteString("event: ")
		frame.WriteString(eventName)
		frame.WriteByte('\n')
	}
	frame.WriteString("data: ")
	frame.Write(encoded)
	frame.WriteString("\n\n")
	return frame.Bytes()
}

func integerValue(value any) (int64, bool) {
	switch typed := value.(type) {
	case float64:
		return int64(typed), true
	case json.Number:
		parsed, err := typed.Int64()
		return parsed, err == nil
	case int:
		return int64(typed), true
	case int64:
		return typed, true
	case string:
		parsed, err := strconv.ParseInt(typed, 10, 64)
		return parsed, err == nil
	default:
		return 0, false
	}
}

func cloneJSONObject(value map[string]any) map[string]any {
	if value == nil {
		return nil
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		return nil
	}
	var clone map[string]any
	if err := json.Unmarshal(encoded, &clone); err != nil {
		return nil
	}
	return clone
}
