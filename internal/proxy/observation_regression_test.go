package proxy

import (
	"bytes"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/bengHak/grok-build-proxy/internal/catalog"
)

type eventRecorder []RequestEvent

func (r *eventRecorder) Observe(event RequestEvent) { *r = append(*r, event) }

func TestHandlerObservesEarlyRequestFailures(t *testing.T) {
	tests := []struct {
		name       string
		method     string
		body       string
		authorized bool
		wantStatus int
	}{
		{name: "unauthorized", method: http.MethodPost, body: `{}`, wantStatus: http.StatusUnauthorized},
		{name: "method", method: http.MethodGet, authorized: true, wantStatus: http.StatusMethodNotAllowed},
		{name: "body too large", method: http.MethodPost, body: strings.Repeat("x", 64), authorized: true, wantStatus: http.StatusRequestEntityTooLarge},
		{name: "malformed body", method: http.MethodPost, body: `{"model":`, authorized: true, wantStatus: http.StatusBadRequest},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			// Given
			var events eventRecorder
			handler, err := New(Config{
				UpstreamURL:  "http://example.test/responses",
				Credentials:  &fakeCredentials{tokens: []string{"token"}},
				Catalog:      catalog.New(""),
				Logger:       slog.New(slog.NewTextHandler(io.Discard, nil)),
				Observer:     &events,
				ClientToken:  "client-secret",
				MaxBodyBytes: 32,
			})
			if err != nil {
				t.Fatal(err)
			}
			request := httptest.NewRequest(test.method, "/v1/responses", strings.NewReader(test.body))
			if test.authorized {
				request.Header.Set("Authorization", "Bearer client-secret")
			}
			response := httptest.NewRecorder()

			// When
			handler.ServeHTTP(response, request)

			// Then
			if response.Code != test.wantStatus {
				t.Fatalf("status = %d", response.Code)
			}
			if len(events) != 2 || events[0].Type != RequestStarted || events[1].Type != RequestFailed {
				t.Fatalf("events = %#v", events)
			}
			if events[1].StatusCode != test.wantStatus || events[1].Error == "" {
				t.Fatalf("failure event = %#v", events[1])
			}
		})
	}
}

func TestHandlerUsesGeneratedRequestIDAsHeaderlessSession(t *testing.T) {
	// Given
	forwardedSession := ""
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		forwardedSession = r.Header.Get("session_id")
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `{"usage":{"output_tokens":1}}`)
	}))
	defer upstream.Close()
	var events eventRecorder
	handler, err := New(Config{
		UpstreamURL: upstream.URL,
		Credentials: &fakeCredentials{tokens: []string{"token"}},
		Catalog:     catalog.New(""),
		HTTPClient:  upstream.Client(),
		Logger:      slog.New(slog.NewTextHandler(io.Discard, nil)),
		Observer:    &events,
	})
	if err != nil {
		t.Fatal(err)
	}
	request := httptest.NewRequest(http.MethodPost, "/v1/responses", strings.NewReader(`{"model":"gpt-5.5","input":"hello","stream":false}`))
	response := httptest.NewRecorder()

	// When
	handler.ServeHTTP(response, request)

	// Then
	if len(events) != 2 {
		t.Fatalf("events = %#v", events)
	}
	if events[0].SessionID == "" || events[0].SessionID == "default" || events[0].SessionID != events[0].RequestID {
		t.Fatalf("start event = %#v", events[0])
	}
	if forwardedSession != events[0].SessionID {
		t.Fatalf("forwarded session = %q, observed = %q", forwardedSession, events[0].SessionID)
	}
}

func TestHandlerUsesCanonicalGrokRequestIDAsSession(t *testing.T) {
	// Given
	forwardedSession := ""
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		forwardedSession = r.Header.Get("session_id")
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `{"usage":{"output_tokens":1}}`)
	}))
	defer upstream.Close()
	var events eventRecorder
	handler, err := New(Config{
		UpstreamURL: upstream.URL,
		Credentials: &fakeCredentials{tokens: []string{"token"}},
		Catalog:     catalog.New(""),
		HTTPClient:  upstream.Client(),
		Logger:      slog.New(slog.NewTextHandler(io.Discard, nil)),
		Observer:    &events,
	})
	if err != nil {
		t.Fatal(err)
	}
	request := httptest.NewRequest(http.MethodPost, "/v1/responses", strings.NewReader(`{"model":"gpt-5.5","input":"hello","stream":false}`))
	request.Header.Set("x-grok-req-id", "canonical-request")
	response := httptest.NewRecorder()

	// When
	handler.ServeHTTP(response, request)

	// Then
	if len(events) != 2 || events[0].SessionID != "canonical-request" || forwardedSession != "canonical-request" {
		t.Fatalf("events=%#v forwarded=%q", events, forwardedSession)
	}
}

func TestHandlerCapturesOutputTokensWithoutObserver(t *testing.T) {
	// Given
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `{"usage":{"output_tokens":37}}`)
	}))
	defer upstream.Close()
	var logs bytes.Buffer
	handler, err := New(Config{
		UpstreamURL: upstream.URL,
		Credentials: &fakeCredentials{tokens: []string{"token"}},
		Catalog:     catalog.New(""),
		HTTPClient:  upstream.Client(),
		Logger:      slog.New(slog.NewTextHandler(&logs, nil)),
	})
	if err != nil {
		t.Fatal(err)
	}
	request := httptest.NewRequest(http.MethodPost, "/v1/responses", strings.NewReader(`{"model":"gpt-5.5","input":"hello","stream":false}`))
	response := httptest.NewRecorder()

	// When
	handler.ServeHTTP(response, request)

	// Then
	if !strings.Contains(logs.String(), "output_tokens=37") {
		t.Fatalf("logs = %q", logs.String())
	}
}

func TestHandlerMarksSemanticSSEFailures(t *testing.T) {
	tests := []struct {
		name string
		body string
	}{
		{"failed response", "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"message\":\"boom\"}}}\n\n"},
		{"incomplete response", "event: response.incomplete\ndata: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\"}}\n\n"},
		{"error event", "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"server_error\",\"message\":\"boom\"}}\n\n"},
		{"unnamed error payload", "data: {\"error\":{\"type\":\"server_error\",\"message\":\"boom\"}}\n\n"},
		{"proxy error", "event: error\ndata: {\"error\":{\"type\":\"proxy_missing_terminal_output\",\"message\":\"missing output\"}}\n\n"},
		{"oversized failed response", "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"message\":\"" + strings.Repeat("x", 300<<10) + "\"}}}\n\n"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			// Given
			upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				w.Header().Set("Content-Type", "text/event-stream")
				_, _ = io.WriteString(w, test.body)
			}))
			defer upstream.Close()
			var events eventRecorder
			handler, err := New(Config{
				UpstreamURL: upstream.URL,
				Credentials: &fakeCredentials{tokens: []string{"token"}},
				Catalog:     catalog.New(""),
				HTTPClient:  upstream.Client(),
				Logger:      slog.New(slog.NewTextHandler(io.Discard, nil)),
				Observer:    &events,
			})
			if err != nil {
				t.Fatal(err)
			}
			request := httptest.NewRequest(http.MethodPost, "/v1/responses", strings.NewReader(`{"model":"gpt-5.5","input":"hello"}`))
			response := httptest.NewRecorder()

			// When
			handler.ServeHTTP(response, request)

			// Then
			if len(events) != 2 || events[1].Type != RequestFailed || events[1].Error == "" {
				t.Fatalf("events = %#v", events)
			}
		})
	}
}

func TestObservedResponseFailureClassifiesTopLevelResponseStatus(t *testing.T) {
	tests := []struct {
		name string
		body string
		want string
	}{
		{name: "incomplete", body: `{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}`, want: "response.incomplete: max_output_tokens"},
		{name: "failed", body: `{"status":"failed","error":{"type":"server_error","message":"unavailable"}}`, want: "response.failed: unavailable"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if got := observedResponseFailure([]byte(test.body)); got != test.want {
				t.Fatalf("failure = %q, want %q", got, test.want)
			}
		})
	}
}

func TestObservedResponseFailureRedactsAndBoundsMessage(t *testing.T) {
	// Given
	secret := "Bearer secret-token sk-abcdefghijk"
	body := `{"status":"failed","error":{"message":"` + secret + strings.Repeat("x", 2_000) + `"}}`

	// When
	failure := observedResponseFailure([]byte(body))

	// Then
	if strings.Contains(failure, "secret-token") || strings.Contains(failure, "sk-abcdefghijk") {
		t.Fatalf("failure leaked a credential shape: %q", failure)
	}
	if len(failure) > len("response.failed: ")+1_027 {
		t.Fatalf("failure summary is unbounded: %d bytes", len(failure))
	}
}

func TestValidHeaderValueRejectsUnicodeControls(t *testing.T) {
	// Given
	value := "session\u009b2J"

	// When
	valid := validHeaderValue(value)

	// Then
	if valid {
		t.Fatalf("Unicode control accepted in %q", value)
	}
}

func TestTailCapturePreservesNewestBytesAcrossRingWrap(t *testing.T) {
	// Given
	capture := newTailCapture(5)

	// When
	for _, chunk := range []string{"abc", "de", "fg"} {
		if _, err := capture.Write([]byte(chunk)); err != nil {
			t.Fatal(err)
		}
	}

	// Then
	if got := string(capture.Bytes()); got != "cdefg" {
		t.Fatalf("tail = %q", got)
	}
}
