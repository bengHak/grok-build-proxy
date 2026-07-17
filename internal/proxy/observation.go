package proxy

import (
	"bufio"
	"bytes"
	"encoding/json"
	"fmt"
	"regexp"
	"strconv"
	"strings"
	"time"
)

// RequestEventType describes a proxy request lifecycle transition.
type RequestEventType uint8

const (
	RequestStarted RequestEventType = iota + 1
	RequestCompleted
	RequestFailed
)

// RequestEvent is emitted from the real responses handler. OutputTokens is set
// only when the upstream reports usage.
type RequestEvent struct {
	Type           RequestEventType
	RequestID      string
	SessionID      string
	RequestedModel string
	Model          string
	StartedAt      time.Time
	EndedAt        time.Time
	StatusCode     int
	OutputTokens   int64
	Error          string
}

// Observer receives request lifecycle events. Implementations must return
// quickly because calls happen on request goroutines.
type Observer interface {
	Observe(RequestEvent)
}

type observerFunc func(RequestEvent)

func (f observerFunc) Observe(event RequestEvent) { f(event) }

// tailCapture keeps the end of a response, where Responses API usage is
// reported, without buffering an arbitrarily large streamed response.
type tailCapture struct {
	buf   []byte
	max   int
	start int
}

func newTailCapture(max int) *tailCapture { return &tailCapture{max: max} }

func (c *tailCapture) Write(p []byte) (int, error) {
	written := len(p)
	if c.max <= 0 {
		return written, nil
	}
	if len(p) >= c.max {
		c.buf = append(c.buf[:0], p[len(p)-c.max:]...)
		c.start = 0
		return written, nil
	}
	if available := c.max - len(c.buf); available > 0 {
		if len(p) <= available {
			c.buf = append(c.buf, p...)
			return written, nil
		}
		c.buf = append(c.buf, p[:available]...)
		p = p[available:]
	}
	first := min(len(p), c.max-c.start)
	copy(c.buf[c.start:], p[:first])
	copy(c.buf, p[first:])
	c.start = (c.start + len(p)) % c.max
	return written, nil
}

func (c *tailCapture) Bytes() []byte {
	if c.start == 0 {
		return c.buf
	}
	data := make([]byte, 0, len(c.buf))
	data = append(data, c.buf[c.start:]...)
	return append(data, c.buf[:c.start]...)
}

var outputTokensPattern = regexp.MustCompile(`"output_tokens"\s*:\s*([0-9]+)`)

func observedOutputTokens(data []byte) int64 {
	// A non-streaming response can be decoded directly. The regex fallback also
	// handles the final response.completed SSE data frame.
	var payload struct {
		Usage struct {
			OutputTokens int64 `json:"output_tokens"`
		} `json:"usage"`
		Response struct {
			Usage struct {
				OutputTokens int64 `json:"output_tokens"`
			} `json:"usage"`
		} `json:"response"`
	}
	if json.Unmarshal(data, &payload) == nil {
		if payload.Usage.OutputTokens > 0 {
			return payload.Usage.OutputTokens
		}
		if payload.Response.Usage.OutputTokens > 0 {
			return payload.Response.Usage.OutputTokens
		}
	}
	matches := outputTokensPattern.FindAllSubmatch(data, -1)
	if len(matches) == 0 {
		return 0
	}
	value, err := strconv.ParseInt(string(matches[len(matches)-1][1]), 10, 64)
	if err != nil {
		return 0
	}
	return value
}

func requestFailure(status int, err error, response []byte) string {
	if err != nil {
		return err.Error()
	}
	if status >= 400 {
		return fmt.Sprintf("upstream returned HTTP %d", status)
	}
	return observedResponseFailure(response)
}

func observedResponseFailure(data []byte) string {
	if failure := responseFailureJSON("", data); failure != "" {
		return failure
	}
	reader := bufio.NewReader(bytes.NewReader(data))
	for {
		frame, err := readSSEFrame(reader)
		if len(frame) > 0 {
			eventName, payload, ok := parseSSEFrame(frame)
			if ok && payload != "[DONE]" {
				if failure := responseFailureJSON(eventName, []byte(payload)); failure != "" {
					return failure
				}
			}
		}
		if err != nil {
			return ""
		}
	}
}

func responseFailureJSON(eventName string, data []byte) string {
	var payload map[string]any
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	if decoder.Decode(&payload) != nil {
		return ""
	}
	eventType := firstNonEmptyString(stringValue(payload["type"]), eventName)
	response := jsonObject(payload["response"])
	if response == nil {
		response = payload
	}
	status := stringValue(response["status"])
	errorObject := jsonObject(payload["error"])
	if responseError := jsonObject(response["error"]); responseError != nil {
		errorObject = responseError
	}
	errorType := stringValue(errorObject["type"])
	message := firstNonEmptyString(stringValue(errorObject["message"]), stringValue(jsonObject(response["incomplete_details"])["reason"]))

	kind := ""
	switch {
	case eventType == "response.failed" || status == "failed":
		kind = "response.failed"
	case eventType == "response.incomplete" || status == "incomplete":
		kind = "response.incomplete"
	case strings.HasPrefix(eventType, "proxy_"):
		kind = eventType
	case strings.HasPrefix(errorType, "proxy_"):
		kind = errorType
	case eventType == "error":
		kind = "error"
	case errorObject != nil:
		kind = firstNonEmptyString(errorType, "error")
	}
	if kind == "" {
		return ""
	}
	kind = summarizeUpstreamError([]byte(kind))
	if message == "" {
		return kind
	}
	return kind + ": " + summarizeUpstreamError([]byte(message))
}
