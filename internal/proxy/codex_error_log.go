package proxy

import (
	"bytes"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"regexp"
	"strings"
)

const maxLoggedUpstreamErrorBytes = 64 << 10

var secretPatterns = []*regexp.Regexp{
	regexp.MustCompile(`(?i)bearer\s+[a-z0-9._~+/=-]+`),
	regexp.MustCompile(`\bsk-[A-Za-z0-9_-]{8,}\b`),
	regexp.MustCompile(`\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b`),
	regexp.MustCompile(`(?i)(access_token|refresh_token|id_token)["'=:\s]+[^,}\s]+`),
}

func logUpstreamError(logger *slog.Logger, req *http.Request, resp *http.Response) {
	prefix, err := io.ReadAll(io.LimitReader(resp.Body, maxLoggedUpstreamErrorBytes+1))
	if err != nil {
		logger.Warn("upstream request rejected", "status", resp.StatusCode, "error", err)
		return
	}
	resp.Body = io.NopCloser(io.MultiReader(bytes.NewReader(prefix), resp.Body))
	summary := summarizeUpstreamError(prefix)
	logger.Warn("upstream request rejected",
		"status", resp.StatusCode,
		"request_id", req.Header.Get("x-client-request-id"),
		"upstream_error", summary,
	)
}

func summarizeUpstreamError(raw []byte) string {
	var payload struct {
		Error struct {
			Message string `json:"message"`
			Type    string `json:"type"`
			Code    any    `json:"code"`
		} `json:"error"`
	}
	summary := strings.TrimSpace(string(raw))
	if json.Unmarshal(raw, &payload) == nil && payload.Error.Message != "" {
		summary = payload.Error.Message
		if payload.Error.Type != "" {
			summary += " type=" + payload.Error.Type
		}
		if payload.Error.Code != nil {
			summary += " code=" + strings.TrimSpace(toString(payload.Error.Code))
		}
	}
	summary = strings.Join(strings.Fields(summary), " ")
	for _, pattern := range secretPatterns {
		summary = pattern.ReplaceAllString(summary, "[redacted]")
	}
	if len(summary) > 1024 {
		summary = summary[:1024] + "..."
	}
	return summary
}

func toString(value any) string {
	encoded, _ := json.Marshal(value)
	return strings.Trim(string(encoded), `"`)
}
