package proxy

import (
	"bytes"
	"io"
	"log/slog"
	"strings"
	"testing"
)

func TestResponsesLiteSSEUsesConfiguredLoggerForNormalizationFailure(t *testing.T) {
	// Given
	var configuredLogs, defaultLogs bytes.Buffer
	previous := slog.Default()
	slog.SetDefault(slog.New(slog.NewTextHandler(&defaultLogs, nil)))
	defer slog.SetDefault(previous)
	body := newResponsesLiteSSEBodyWithOptions(
		io.NopCloser(strings.NewReader("data: [DONE]\n\n")),
		responsesLiteSSEOptions{
			Mode:   responsesCompatFull,
			Logger: slog.New(slog.NewTextHandler(&configuredLogs, nil)),
		},
	)

	// When
	if _, err := io.ReadAll(body); err != nil {
		t.Fatal(err)
	}

	// Then
	if !strings.Contains(configuredLogs.String(), "proxy_missing_terminal_response") {
		t.Fatalf("configured logs = %q", configuredLogs.String())
	}
	if strings.Contains(defaultLogs.String(), "proxy_missing_terminal_response") {
		t.Fatalf("default logger received normalization failure: %q", defaultLogs.String())
	}
}
