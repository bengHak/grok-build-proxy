package proxy

import (
	"bufio"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net/http"
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
	return headerContainsToken(resp.Header.Get("Content-Type"), "text/event-stream") ||
		headerContainsToken(req.Header.Get("Accept"), "text/event-stream")
}

func headerContainsToken(value, token string) bool {
	return strings.Contains(strings.ToLower(strings.TrimSpace(value)), strings.ToLower(token))
}

func newResponsesLiteSSEBody(source io.ReadCloser) io.ReadCloser {
	return newResponsesLiteSSEBodyWithMode(source, responsesCompatFull)
}

func newResponsesLiteSSEBodyWithMode(source io.ReadCloser, mode responsesCompatMode) io.ReadCloser {
	return newResponsesLiteSSEBodyWithOptions(source, responsesLiteSSEOptions{Mode: mode})
}

type responsesLiteSSEOptions struct {
	Mode      responsesCompatMode
	Model     string
	RequestID string
	Logger    *slog.Logger
	Trace     bool
}

func newResponsesLiteSSEBodyWithOptions(source io.ReadCloser, options responsesLiteSSEOptions) io.ReadCloser {
	if options.Logger == nil {
		options.Logger = slog.Default()
	}
	return &responsesLiteSSEBody{
		source:    source,
		reader:    bufio.NewReader(source),
		state:     newResponsesSSEAssembler(options.Mode),
		envelope:  newResponsesResponseNormalizer(options.Model, options.RequestID),
		logger:    options.Logger,
		trace:     options.Trace,
		requestID: options.RequestID,
	}
}

type responsesLiteSSEBody struct {
	source         io.ReadCloser
	reader         *bufio.Reader
	state          *responsesSSEAssembler
	envelope       *responsesResponseNormalizer
	logger         *slog.Logger
	trace          bool
	requestID      string
	eventIndex     int
	visibleText    strings.Builder
	visibleRefusal strings.Builder
	pending        []byte
	finished       bool
	terminalErr    error
}

func (b *responsesLiteSSEBody) Read(p []byte) (int, error) {
	if len(p) == 0 {
		return 0, nil
	}
	for len(b.pending) == 0 && !b.finished {
		frame, err := readSSEFrame(b.reader)
		if len(frame) > 0 {
			if eventType, drop := codexPrivateSSEEventType(frame); drop {
				b.eventIndex++
				if b.trace {
					logResponsesSSEFrame(
						b.logger,
						b.requestID,
						b.eventIndex,
						nil,
						nil,
						responsesResponseNormalizationReport{
							EventType: eventType,
							Filled:    []string{"private_event.dropped"},
						},
					)
				}
			} else {
				rawFrame := append([]byte(nil), frame...)
				captureVisibleSSEContent(rawFrame, &b.visibleText, &b.visibleRefusal)

				report := responsesResponseNormalizationReport{}
				if b.envelope != nil {
					frame, report = b.envelope.normalizeFrame(frame)
				}
				frame = b.state.normalizeAuxiliaryFrame(frame)
				terminalCandidate := completedTerminalFrame(frame)
				normalized := b.state.transformFrame(frame)
				var fallbackApplied bool
				normalized, fallbackApplied = repairVisibleTerminalOutput(
					normalized,
					terminalCandidate,
					b.visibleText.String(),
					b.visibleRefusal.String(),
				)
				report.VisibleFallback = fallbackApplied
				b.eventIndex++
				if b.trace {
					logResponsesSSEFrame(b.logger, b.requestID, b.eventIndex, rawFrame, normalized, report)
				}
				b.pending = append(b.pending, normalized...)
			}
		}
		if err != nil {
			b.finished = true
			if errors.Is(err, io.EOF) {
				terminal := b.state.finishAtEOF()
				if len(terminal) > 0 {
					terminal, _ = repairVisibleTerminalOutput(
						terminal,
						nil,
						b.visibleText.String(),
						b.visibleRefusal.String(),
					)
				}
				if len(terminal) > 0 && b.trace {
					b.eventIndex++
					logResponsesSSEFrame(
						b.logger,
						b.requestID,
						b.eventIndex,
						nil,
						terminal,
						responsesResponseNormalizationReport{EventType: "proxy.synthetic_terminal"},
					)
				}
				b.pending = append(b.pending, terminal...)
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

// codexPrivateSSEEventType identifies transport-only Codex events that are not
// part of the public Responses API event enum used by Grok Build. Codex's own
// client consumes response.metadata out of band for turn state, model and
// moderation metadata. Passing it through makes async-openai reject the stream
// before the following Plan/function-call event can be processed.
func codexPrivateSSEEventType(frame []byte) (string, bool) {
	eventName, data, ok := parseSSEFrame(frame)
	if eventName == "response.metadata" {
		return eventName, true
	}
	if !ok || data == "[DONE]" {
		return "", false
	}

	var event map[string]any
	decoder := json.NewDecoder(strings.NewReader(data))
	decoder.UseNumber()
	if err := decoder.Decode(&event); err != nil {
		return "", false
	}
	if stringValue(event["type"]) == "response.metadata" {
		return "response.metadata", true
	}
	return "", false
}

func (b *responsesLiteSSEBody) Close() error {
	return b.source.Close()
}
