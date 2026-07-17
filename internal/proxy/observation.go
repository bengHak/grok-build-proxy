package proxy

import (
	"encoding/json"
	"fmt"
	"regexp"
	"strconv"
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
	buf []byte
	max int
}

func newTailCapture(max int) *tailCapture { return &tailCapture{max: max} }

func (c *tailCapture) Write(p []byte) (int, error) {
	if c.max <= 0 {
		return len(p), nil
	}
	if len(p) >= c.max {
		c.buf = append(c.buf[:0], p[len(p)-c.max:]...)
		return len(p), nil
	}
	if overflow := len(c.buf) + len(p) - c.max; overflow > 0 {
		copy(c.buf, c.buf[overflow:])
		c.buf = c.buf[:len(c.buf)-overflow]
	}
	c.buf = append(c.buf, p...)
	return len(p), nil
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

func requestFailure(status int, err error) string {
	if err != nil {
		return err.Error()
	}
	if status >= 400 {
		return fmt.Sprintf("upstream returned HTTP %d", status)
	}
	return ""
}
