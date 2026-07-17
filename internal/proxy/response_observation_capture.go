package proxy

import (
	"bytes"
	"strings"
)

const observedSSELineLimit = 512

type responseObservationCapture struct {
	tail    *tailCapture
	tracker *sseFailureTracker
}

func newResponseObservationCapture(max int, eventStream bool) *responseObservationCapture {
	capture := &responseObservationCapture{tail: newTailCapture(max)}
	if eventStream {
		capture.tracker = &sseFailureTracker{}
	}
	return capture
}

func (c *responseObservationCapture) Write(p []byte) (int, error) {
	if c.tracker != nil {
		c.tracker.Write(p)
	}
	return c.tail.Write(p)
}

func (c *responseObservationCapture) Bytes() []byte { return c.tail.Bytes() }

func (c *responseObservationCapture) FailureHint() string {
	if c.tracker == nil {
		return ""
	}
	return c.tracker.failure
}

type sseFailureTracker struct {
	line    []byte
	discard bool
	failure string
}

func (t *sseFailureTracker) Write(p []byte) {
	for _, value := range p {
		if value == '\n' {
			t.finishLine()
			continue
		}
		if t.discard {
			continue
		}
		if len(t.line) == observedSSELineLimit {
			t.discard = true
			continue
		}
		t.line = append(t.line, value)
	}
}

func (t *sseFailureTracker) finishLine() {
	if !t.discard {
		line := strings.TrimSpace(string(bytes.TrimSuffix(t.line, []byte{'\r'})))
		if after, ok := strings.CutPrefix(line, "event:"); ok {
			kind := strings.TrimSpace(after)
			if kind == "response.failed" || kind == "response.incomplete" || kind == "error" || strings.HasPrefix(kind, "proxy_") {
				t.failure = summarizeUpstreamError([]byte(kind))
			}
		}
	}
	t.line = t.line[:0]
	t.discard = false
}
