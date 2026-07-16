package proxy

import (
	"bufio"
	"bytes"
	"encoding/json"
	"strconv"
	"strings"
)

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

func encodeSSEError(kind, message string) []byte {
	return encodeSSEEvent("error", map[string]any{
		"error": map[string]any{
			"type":    kind,
			"message": message,
		},
	})
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
	decoder := json.NewDecoder(bytes.NewReader(encoded))
	decoder.UseNumber()
	if err := decoder.Decode(&clone); err != nil {
		return nil
	}
	return clone
}

func jsonObject(value any) map[string]any {
	object, _ := value.(map[string]any)
	return object
}

func jsonArray(value any) []any {
	array, _ := value.([]any)
	return array
}

func stringValue(value any) string {
	text, _ := value.(string)
	return text
}

func firstNonEmptyString(values ...string) string {
	for _, value := range values {
		if value = strings.TrimSpace(value); value != "" {
			return value
		}
	}
	return ""
}
