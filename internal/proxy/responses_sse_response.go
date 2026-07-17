package proxy

import (
	"bufio"
	"bytes"
	"encoding/json"
	"log/slog"
	"sort"
	"strings"
	"time"
)

type responsesResponseNormalizer struct {
	model      string
	responseID string
	createdAt  int64
}

type responsesResponseNormalizationReport struct {
	EventType       string
	Filled          []string
	VisibleFallback bool
}

func newResponsesResponseNormalizer(model, requestID string) *responsesResponseNormalizer {
	requestID = strings.TrimSpace(requestID)
	responseID := ""
	if requestID != "" {
		responseID = "resp_" + requestID
	}
	return &responsesResponseNormalizer{
		model:      strings.TrimSpace(model),
		responseID: responseID,
		createdAt:  time.Now().Unix(),
	}
}

func (m responsesCompatMode) String() string {
	switch m {
	case responsesCompatOff:
		return "off"
	case responsesCompatText:
		return "text"
	case responsesCompatFull:
		return "full"
	default:
		return "unknown"
	}
}

func (n *responsesResponseNormalizer) normalizeFrame(frame []byte) ([]byte, responsesResponseNormalizationReport) {
	report := responsesResponseNormalizationReport{}
	eventName, data, ok := parseSSEFrame(frame)
	if !ok || data == "[DONE]" {
		return frame, report
	}

	var event map[string]any
	decoder := json.NewDecoder(strings.NewReader(data))
	decoder.UseNumber()
	if err := decoder.Decode(&event); err != nil {
		return frame, report
	}

	eventType := firstNonEmptyString(stringValue(event["type"]), eventName)
	report.EventType = eventType
	status, isResponseEvent := responseStatusForEvent(eventType)
	if !isResponseEvent {
		return frame, report
	}

	response := jsonObject(event["response"])
	if response == nil {
		return frame, report
	}
	report.Filled = n.normalizeResponse(response, status)
	if len(report.Filled) == 0 {
		return frame, report
	}
	sort.Strings(report.Filled)
	return encodeSSEEvent(firstNonEmptyString(eventName, eventType), event), report
}

func responseStatusForEvent(eventType string) (string, bool) {
	switch eventType {
	case "response.created", "response.in_progress":
		return "in_progress", true
	case "response.queued":
		return "queued", true
	case "response.completed":
		return "completed", true
	case "response.incomplete":
		return "incomplete", true
	case "response.failed":
		return "failed", true
	default:
		return "", false
	}
}

func (n *responsesResponseNormalizer) normalizeResponse(response map[string]any, eventStatus string) []string {
	var filled []string

	if id := strings.TrimSpace(stringValue(response["id"])); id != "" {
		n.responseID = id
	} else {
		if n.responseID == "" {
			n.responseID = "resp_grok_build_proxy"
		}
		response["id"] = n.responseID
		filled = append(filled, "response.id")
	}

	if model := strings.TrimSpace(stringValue(response["model"])); model != "" {
		n.model = model
	} else {
		if n.model == "" {
			n.model = "unknown"
		}
		response["model"] = n.model
		filled = append(filled, "response.model")
	}

	if stringValue(response["object"]) != "response" {
		response["object"] = "response"
		filled = append(filled, "response.object")
	}

	if createdAt, ok := strictUnsignedInteger(response["created_at"]); ok {
		n.createdAt = createdAt
	} else {
		if n.createdAt < 0 {
			n.createdAt = 0
		}
		response["created_at"] = n.createdAt
		filled = append(filled, "response.created_at")
	}

	if _, ok := response["output"].([]any); !ok {
		response["output"] = []any{}
		filled = append(filled, "response.output")
	}

	status := strings.TrimSpace(stringValue(response["status"]))
	if eventStatus == "" {
		eventStatus = "in_progress"
	}
	if !validResponseStatus(status) || status != eventStatus {
		response["status"] = eventStatus
		filled = append(filled, "response.status")
	}

	if usage, exists := response["usage"]; exists && usage != nil {
		if usageObject := jsonObject(usage); usageObject != nil {
			filled = append(filled, normalizeResponseUsage(usageObject)...)
		} else {
			response["usage"] = nil
			filled = append(filled, "response.usage")
		}
	}

	return filled
}

func validResponseStatus(status string) bool {
	switch status {
	case "completed", "failed", "in_progress", "cancelled", "queued", "incomplete":
		return true
	default:
		return false
	}
}

func normalizeResponseUsage(usage map[string]any) []string {
	var filled []string
	inputTokens, changed := normalizeUnsignedIntegerField(usage, "input_tokens", 0)
	if changed {
		filled = append(filled, "response.usage.input_tokens")
	}
	outputTokens, changed := normalizeUnsignedIntegerField(usage, "output_tokens", 0)
	if changed {
		filled = append(filled, "response.usage.output_tokens")
	}
	if _, changed := normalizeUnsignedIntegerField(usage, "total_tokens", inputTokens+outputTokens); changed {
		filled = append(filled, "response.usage.total_tokens")
	}

	inputDetails := jsonObject(usage["input_tokens_details"])
	if inputDetails == nil {
		inputDetails = map[string]any{}
		usage["input_tokens_details"] = inputDetails
		filled = append(filled, "response.usage.input_tokens_details")
	}
	if _, changed := normalizeUnsignedIntegerField(inputDetails, "cached_tokens", 0); changed {
		filled = append(filled, "response.usage.input_tokens_details.cached_tokens")
	}

	outputDetails := jsonObject(usage["output_tokens_details"])
	if outputDetails == nil {
		outputDetails = map[string]any{}
		usage["output_tokens_details"] = outputDetails
		filled = append(filled, "response.usage.output_tokens_details")
	}
	if _, changed := normalizeUnsignedIntegerField(outputDetails, "reasoning_tokens", 0); changed {
		filled = append(filled, "response.usage.output_tokens_details.reasoning_tokens")
	}

	return filled
}

func normalizeUnsignedIntegerField(object map[string]any, key string, fallback int64) (int64, bool) {
	value, exists := object[key]
	if !exists {
		object[key] = fallback
		return fallback, true
	}
	integer, ok := strictUnsignedInteger(value)
	if !ok {
		object[key] = fallback
		return fallback, true
	}
	return integer, false
}

func strictUnsignedInteger(value any) (int64, bool) {
	integer, ok := integerValue(value)
	if !ok || integer < 0 {
		return 0, false
	}
	switch value.(type) {
	case json.Number, float64, int, int64:
		return integer, true
	default:
		return 0, false
	}
}

func captureVisibleSSEContent(frame []byte, text, refusal *strings.Builder) {
	_, data, ok := parseSSEFrame(frame)
	if !ok || data == "[DONE]" {
		return
	}
	var event map[string]any
	decoder := json.NewDecoder(strings.NewReader(data))
	decoder.UseNumber()
	if err := decoder.Decode(&event); err != nil {
		return
	}
	switch stringValue(event["type"]) {
	case "response.output_text.delta":
		if delta := stringValue(event["delta"]); delta != "" {
			text.WriteString(delta)
		}
	case "response.output_text.done":
		if text.Len() == 0 {
			text.WriteString(stringValue(event["text"]))
		}
	case "response.refusal.delta":
		if delta := stringValue(event["delta"]); delta != "" {
			refusal.WriteString(delta)
		}
	case "response.refusal.done":
		if refusal.Len() == 0 {
			refusal.WriteString(stringValue(event["refusal"]))
		}
	}
}

func injectVisibleTerminalFallback(frame []byte, text, refusal string) ([]byte, bool) {
	if text == "" && refusal == "" {
		return frame, false
	}
	eventName, data, ok := parseSSEFrame(frame)
	if !ok || data == "[DONE]" {
		return frame, false
	}
	var event map[string]any
	decoder := json.NewDecoder(strings.NewReader(data))
	decoder.UseNumber()
	if err := decoder.Decode(&event); err != nil {
		return frame, false
	}
	eventType := firstNonEmptyString(stringValue(event["type"]), eventName)
	if eventType != "response.completed" {
		return frame, false
	}
	response := jsonObject(event["response"])
	if response == nil || responseHasUsableOutput(response) {
		return frame, false
	}
	message := buildMessageItem(nil, "", stringValue(response["id"]), text, refusal)
	response["output"] = append(jsonArray(response["output"]), message)
	return encodeSSEEvent(firstNonEmptyString(eventName, eventType), event), true
}

func logResponsesSSEFrame(
	logger *slog.Logger,
	requestID string,
	eventIndex int,
	rawFrame, normalized []byte,
	report responsesResponseNormalizationReport,
) {
	if logger == nil {
		logger = slog.Default()
	}

	rawSummary := summarizeSSEFrame(rawFrame)
	normalizedSummary := summarizeSSEData(normalized)
	resultTypes := sseEventTypes(normalized)
	eventType := firstNonEmptyString(report.EventType, rawSummary.EventType)
	logger.Info(
		"responses lite sse event",
		"request_id", requestID,
		"event_index", eventIndex,
		"event_type", eventType,
		"result_event_types", strings.Join(resultTypes, ","),
		"normalized", len(report.Filled) > 0 || report.VisibleFallback || !bytes.Equal(rawFrame, normalized),
		"filled_fields", strings.Join(report.Filled, ","),
		"visible_fallback", report.VisibleFallback,
		"top_level_keys", strings.Join(rawSummary.TopLevelKeys, ","),
		"response_keys", strings.Join(rawSummary.ResponseKeys, ","),
		"item_type", rawSummary.ItemType,
		"response_status", rawSummary.ResponseStatus,
		"raw_output_items", rawSummary.OutputItems,
		"normalized_output_items", normalizedSummary.OutputItems,
		"normalized_visible_text_bytes", normalizedSummary.VisibleTextBytes,
		"normalized_tool_calls", normalizedSummary.ToolCalls,
		"normalized_usable_output", normalizedSummary.UsableOutput,
		"raw_bytes", len(rawFrame),
		"normalized_bytes", len(normalized),
	)
}

type responsesSSEFrameSummary struct {
	EventType        string
	TopLevelKeys     []string
	ResponseKeys     []string
	ItemType         string
	ResponseStatus   string
	OutputItems      int
	VisibleTextBytes int
	ToolCalls        int
	UsableOutput     bool
}

func summarizeSSEFrame(frame []byte) responsesSSEFrameSummary {
	summary := responsesSSEFrameSummary{OutputItems: -1}
	if len(frame) == 0 {
		return summary
	}
	eventName, data, ok := parseSSEFrame(frame)
	if !ok {
		summary.EventType = eventName
		return summary
	}
	if data == "[DONE]" {
		summary.EventType = "[DONE]"
		return summary
	}
	var event map[string]any
	decoder := json.NewDecoder(strings.NewReader(data))
	decoder.UseNumber()
	if err := decoder.Decode(&event); err != nil {
		summary.EventType = firstNonEmptyString(eventName, "invalid_json")
		return summary
	}
	summary.EventType = firstNonEmptyString(stringValue(event["type"]), eventName)
	summary.TopLevelKeys = sortedObjectKeys(event)
	if response := jsonObject(event["response"]); response != nil {
		summary.ResponseKeys = sortedObjectKeys(response)
		summary.ResponseStatus = stringValue(response["status"])
		if output, ok := response["output"].([]any); ok {
			summary.OutputItems = len(output)
		}
		summary.VisibleTextBytes, summary.ToolCalls = summarizeResponseOutput(response)
		summary.UsableOutput = responseHasUsableOutput(response)
	}
	if item := jsonObject(event["item"]); item != nil {
		summary.ItemType = stringValue(item["type"])
	}
	return summary
}

func summarizeSSEData(data []byte) responsesSSEFrameSummary {
	if len(data) == 0 {
		return responsesSSEFrameSummary{OutputItems: -1}
	}
	reader := bufio.NewReader(bytes.NewReader(data))
	last := responsesSSEFrameSummary{OutputItems: -1}
	terminal := responsesSSEFrameSummary{OutputItems: -1}
	for {
		frame, err := readSSEFrame(reader)
		if len(frame) > 0 {
			summary := summarizeSSEFrame(frame)
			last = summary
			if summary.EventType == "response.completed" {
				terminal = summary
			}
		}
		if err != nil {
			break
		}
	}
	if terminal.EventType != "" {
		return terminal
	}
	return last
}

func summarizeResponseOutput(response map[string]any) (visibleBytes, toolCalls int) {
	for _, rawItem := range jsonArray(response["output"]) {
		item := jsonObject(rawItem)
		switch stringValue(item["type"]) {
		case "message":
			for _, rawPart := range jsonArray(item["content"]) {
				part := jsonObject(rawPart)
				switch stringValue(part["type"]) {
				case "output_text":
					visibleBytes += len(stringValue(part["text"]))
				case "refusal":
					visibleBytes += len(stringValue(part["refusal"]))
				}
			}
		case "function_call", "custom_tool_call":
			toolCalls++
		}
	}
	return visibleBytes, toolCalls
}

func sseEventTypes(data []byte) []string {
	if len(data) == 0 {
		return nil
	}
	reader := bufio.NewReader(bytes.NewReader(data))
	var types []string
	for {
		frame, err := readSSEFrame(reader)
		if len(frame) > 0 {
			summary := summarizeSSEFrame(frame)
			if summary.EventType != "" {
				types = append(types, summary.EventType)
			}
		}
		if err != nil {
			break
		}
	}
	return types
}

func sortedObjectKeys(object map[string]any) []string {
	if len(object) == 0 {
		return nil
	}
	keys := make([]string, 0, len(object))
	for key := range object {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	return keys
}
