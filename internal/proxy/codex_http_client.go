package proxy

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"strings"
	"time"
)

const DefaultCodexCompatibilityVersion = "0.144.0"

type codexCompatTransport struct {
	base          http.RoundTripper
	logger        *slog.Logger
	compatVersion string
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
		base:          base,
		logger:        logger,
		compatVersion: compatVersion,
	}}
}

func (t *codexCompatTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	if err := normalizeCodexHTTPRequest(req, t.compatVersion); err != nil {
		return nil, fmt.Errorf("normalize Codex request: %w", err)
	}
	resp, err := t.base.RoundTrip(req)
	if err != nil {
		return nil, err
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

	req.Header.Del("OpenAI-Beta")
	req.Header.Del("session_id")
	if sessionID != "" {
		req.Header.Set("session-id", sessionID)
		req.Header.Set("x-session-affinity", sessionID)
		req.Header.Set("x-client-request-id", sessionID)
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

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if value = strings.TrimSpace(value); value != "" {
			return value
		}
	}
	return ""
}
