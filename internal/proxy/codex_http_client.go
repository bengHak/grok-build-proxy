package proxy

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"os"
	"strings"
	"time"
)

const DefaultCodexCompatibilityVersion = "0.144.0"

type codexCompatTransport struct {
	base            http.RoundTripper
	logger          *slog.Logger
	compatVersion   string
	responsesCompat responsesCompatMode
	traceSSE        bool
}

// NewCodexHTTPClient returns an HTTP client that normalizes Grok Build's
// outgoing Responses request to the current ChatGPT Codex HTTP contract.
func NewCodexHTTPClient(logger *slog.Logger, compatVersion string) *http.Client {
	if logger == nil {
		logger = slog.Default()
	}
	compatVersion = strings.TrimSpace(compatVersion)
	if compatVersion == "" {
		compatVersion = DefaultCodexCompatibilityVersion
	}
	base := &http.Transport{
		Proxy:                 http.ProxyFromEnvironment,
		MaxIdleConns:          100,
		MaxIdleConnsPerHost:   20,
		IdleConnTimeout:       90 * time.Second,
		ResponseHeaderTimeout: 90 * time.Second,
		ForceAttemptHTTP2:     true,
	}
	return &http.Client{Transport: &codexCompatTransport{
		base:            base,
		logger:          logger,
		compatVersion:   compatVersion,
		responsesCompat: parseResponsesCompatMode(os.Getenv("GROK_BUILD_PROXY_RESPONSES_COMPAT")),
		traceSSE:        envEnabled("GROK_BUILD_PROXY_SSE_TRACE"),
	}}
}

func (t *codexCompatTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	if err := normalizeCodexHTTPRequest(req, t.compatVersion); err != nil {
		return nil, fmt.Errorf("normalize Codex request: %w", err)
	}

	model := codexRequestString(req, "model")
	requestID := firstNonEmpty(
		req.Header.Get("x-client-request-id"),
		req.Header.Get("session-id"),
		req.Header.Get("thread-id"),
	)
	lite := strings.EqualFold(strings.TrimSpace(req.Header.Get(responsesLiteHeader)), "true")
	acceptsSSE := headerContainsToken(req.Header.Get("Accept"), "text/event-stream")
	if lite && acceptsSSE {
		// Responses Lite is a text stream. Avoid an opaque compressed response
		// bypassing the compatibility reader when an intermediary changes headers.
		req.Header.Set("Accept-Encoding", "identity")
	}

	resp, err := t.base.RoundTrip(req)
	if err != nil {
		return nil, err
	}

	normalize := t.responsesCompat != responsesCompatOff && shouldNormalizeCodexSSEResponse(req, resp)
	if lite {
		t.logger.Info(
			"codex responses compatibility",
			"request_id", requestID,
			"model", model,
			"responses_lite", true,
			"compat_mode", t.responsesCompat.String(),
			"request_accept", req.Header.Get("Accept"),
			"upstream_content_type", resp.Header.Get("Content-Type"),
			"upstream_content_encoding", resp.Header.Get("Content-Encoding"),
			"normalizer_applied", normalize,
		)
	}
	if normalize {
		resp.Body = newResponsesLiteSSEBodyWithOptions(resp.Body, responsesLiteSSEOptions{
			Mode:      t.responsesCompat,
			Model:     model,
			RequestID: requestID,
			Logger:    t.logger,
			Trace:     t.traceSSE,
		})
		resp.ContentLength = -1
		resp.Header.Del("Content-Length")
		resp.Header.Del("Content-Encoding")
		resp.Header.Set("Content-Type", "text/event-stream; charset=utf-8")
	}
	if resp.StatusCode >= 400 {
		logUpstreamError(t.logger, req, resp)
	}
	return resp, nil
}

func normalizeCodexHTTPRequest(req *http.Request, compatVersion string) error {
	sessionID := firstNonEmpty(
		req.Header.Get("session-id"),
		req.Header.Get("session_id"),
		req.Header.Get("x-client-request-id"),
	)
	threadID := firstNonEmpty(req.Header.Get("thread-id"), sessionID)
	windowID := firstNonEmpty(req.Header.Get("x-codex-window-id"), sessionID+":0")
	clientRequestID := firstNonEmpty(req.Header.Get("x-client-request-id"), sessionID)

	req.Header.Del("OpenAI-Beta")
	req.Header.Del("session_id")
	if sessionID != "" {
		req.Header.Set("session-id", sessionID)
		req.Header.Set("x-session-affinity", sessionID)
	}
	if clientRequestID != "" {
		req.Header.Set("x-client-request-id", clientRequestID)
	}
	if threadID != "" {
		req.Header.Set("thread-id", threadID)
	}
	if windowID != "" {
		req.Header.Set("x-codex-window-id", windowID)
	}
	req.Header.Set("version", compatVersion)

	if req.Body == nil {
		return nil
	}
	raw, err := io.ReadAll(req.Body)
	if err != nil {
		return err
	}
	_ = req.Body.Close()
	var body map[string]any
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	if err := decoder.Decode(&body); err != nil {
		return err
	}
	lite := strings.EqualFold(req.Header.Get("X-OpenAI-Internal-Codex-Responses-Lite"), "true")
	if err := normalizeCodexBody(body, codexIdentity{sessionID, threadID, windowID}, lite); err != nil {
		return err
	}
	encoded, err := json.Marshal(body)
	if err != nil {
		return err
	}
	req.Body = io.NopCloser(bytes.NewReader(encoded))
	req.ContentLength = int64(len(encoded))
	req.GetBody = func() (io.ReadCloser, error) {
		return io.NopCloser(bytes.NewReader(encoded)), nil
	}
	return nil
}

func codexRequestString(req *http.Request, key string) string {
	if req == nil || req.GetBody == nil || strings.TrimSpace(key) == "" {
		return ""
	}
	body, err := req.GetBody()
	if err != nil {
		return ""
	}
	defer body.Close()
	var payload map[string]any
	decoder := json.NewDecoder(body)
	decoder.UseNumber()
	if err := decoder.Decode(&payload); err != nil {
		return ""
	}
	return strings.TrimSpace(stringValue(payload[key]))
}

func envEnabled(name string) bool {
	switch strings.ToLower(strings.TrimSpace(os.Getenv(name))) {
	case "1", "true", "yes", "on", "debug", "summary":
		return true
	default:
		return false
	}
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if value = strings.TrimSpace(value); value != "" {
			return value
		}
	}
	return ""
}
