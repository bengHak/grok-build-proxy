package proxy

import (
	"bufio"
	"errors"
	"io"
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
	return strings.Contains(strings.ToLower(resp.Header.Get("Content-Type")), "text/event-stream")
}

func newResponsesLiteSSEBody(source io.ReadCloser) io.ReadCloser {
	return newResponsesLiteSSEBodyWithMode(source, responsesCompatFull)
}

func newResponsesLiteSSEBodyWithMode(source io.ReadCloser, mode responsesCompatMode) io.ReadCloser {
	return &responsesLiteSSEBody{
		source: source,
		reader: bufio.NewReader(source),
		state:  newResponsesSSEAssembler(mode),
	}
}

type responsesLiteSSEBody struct {
	source      io.ReadCloser
	reader      *bufio.Reader
	state       *responsesSSEAssembler
	pending     []byte
	finished    bool
	terminalErr error
}

func (b *responsesLiteSSEBody) Read(p []byte) (int, error) {
	if len(p) == 0 {
		return 0, nil
	}
	for len(b.pending) == 0 && !b.finished {
		frame, err := readSSEFrame(b.reader)
		if len(frame) > 0 {
			b.pending = append(b.pending, b.state.transformFrame(frame)...)
		}
		if err != nil {
			b.finished = true
			if errors.Is(err, io.EOF) {
				b.pending = append(b.pending, b.state.finishAtEOF()...)
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
