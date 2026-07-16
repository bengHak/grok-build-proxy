package proxy

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/bengHak/grok-build-proxy/internal/auth"
	"github.com/bengHak/grok-build-proxy/internal/catalog"
)

type fakeCredentials struct {
	mu     sync.Mutex
	calls  []bool
	tokens []string
}

func (f *fakeCredentials) Get(_ context.Context, force bool) (auth.Credentials, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, force)
	index := len(f.calls) - 1
	if index >= len(f.tokens) {
		index = len(f.tokens) - 1
	}
	return auth.Credentials{AccessToken: f.tokens[index], AccountID: "account-123"}, nil
}

func TestHandlerProxiesStreamingLiteRequest(t *testing.T) {
	var gotBody map[string]any
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if got := r.Header.Get("Authorization"); got != "Bearer token-1" {
			t.Fatalf("Authorization = %q", got)
		}
		if got := r.Header.Get("ChatGPT-Account-ID"); got != "account-123" {
			t.Fatalf("ChatGPT-Account-ID = %q", got)
		}
		if got := r.Header.Get("X-OpenAI-Internal-Codex-Responses-Lite"); got != "true" {
			t.Fatalf("lite header = %q", got)
		}
		if got := r.Header.Get("session_id"); got != "session-abc" {
			t.Fatalf("session_id = %q", got)
		}
		if err := json.NewDecoder(r.Body).Decode(&gotBody); err != nil {
			t.Fatal(err)
		}
		w.Header().Set("Content-Type", "text/event-stream")
		w.WriteHeader(http.StatusOK)
		_, _ = io.WriteString(w, "event: response.output_text.delta\ndata: {\"delta\":\"hi\"}\n\n")
		if flusher, ok := w.(http.Flusher); ok {
			flusher.Flush()
		}
		_, _ = io.WriteString(w, "event: response.completed\ndata: {\"response\":{}}\n\n")
	}))
	defer upstream.Close()

	creds := &fakeCredentials{tokens: []string{"token-1"}}
	handler, err := New(Config{
		UpstreamURL: upstream.URL,
		Credentials: creds,
		Catalog:     catalog.New(""),
		HTTPClient:  upstream.Client(),
		Logger:      slog.New(slog.NewTextHandler(io.Discard, nil)),
		Version:     "test",
	})
	if err != nil {
		t.Fatal(err)
	}

	req := httptest.NewRequest(http.MethodPost, "/v1/responses", strings.NewReader(`{
      "model":"gpt-5.6-terra",
      "instructions":"be useful",
      "input":"hello",
      "tools":[{"type":"function","name":"shell","parameters":{"type":"object"}}]
    }`))
	req.Header.Set("x-grok-session-id", "session-abc")
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, req)
	resp := recorder.Result()
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(resp.Body)
		t.Fatalf("status = %d: %s", resp.StatusCode, body)
	}
	data, _ := io.ReadAll(resp.Body)
	if !strings.Contains(string(data), "response.completed") {
		t.Fatalf("stream body = %s", data)
	}
	if gotBody["model"] != "gpt-5.6-terra" {
		t.Fatalf("upstream model = %#v", gotBody["model"])
	}
	if _, exists := gotBody["tools"]; exists {
		t.Fatal("lite tools were not transformed")
	}
}

func TestHandlerRefreshesOnceOnUnauthorized(t *testing.T) {
	var calls int
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		if calls == 1 {
			if r.Header.Get("Authorization") != "Bearer stale" {
				t.Fatalf("first auth = %q", r.Header.Get("Authorization"))
			}
			w.WriteHeader(http.StatusUnauthorized)
			return
		}
		if r.Header.Get("Authorization") != "Bearer fresh" {
			t.Fatalf("second auth = %q", r.Header.Get("Authorization"))
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `{"id":"resp_1","object":"response"}`)
	}))
	defer upstream.Close()

	creds := &fakeCredentials{tokens: []string{"stale", "fresh"}}
	handler, err := New(Config{
		UpstreamURL: upstream.URL,
		Credentials: creds,
		Catalog:     catalog.New(""),
		HTTPClient:  upstream.Client(),
		Logger:      slog.New(slog.NewTextHandler(io.Discard, nil)),
		Version:     "test",
	})
	if err != nil {
		t.Fatal(err)
	}
	req := httptest.NewRequest(http.MethodPost, "/v1/responses", strings.NewReader(`{"model":"gpt-5.5","input":"hello","stream":false}`))
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, req)
	if recorder.Code != http.StatusOK {
		t.Fatalf("status = %d body=%s", recorder.Code, recorder.Body.String())
	}
	if calls != 2 {
		t.Fatalf("upstream calls = %d", calls)
	}
	if len(creds.calls) != 2 || creds.calls[0] || !creds.calls[1] {
		t.Fatalf("credential calls = %#v", creds.calls)
	}
}

func TestHandlerRequiresConfiguredClientToken(t *testing.T) {
	handler, err := New(Config{
		UpstreamURL: "http://example.test/responses",
		Credentials: &fakeCredentials{tokens: []string{"token"}},
		Catalog:     catalog.New(""),
		HTTPClient:  &http.Client{Timeout: time.Second},
		Logger:      slog.New(slog.NewTextHandler(io.Discard, nil)),
		ClientToken: "local-secret",
		Version:     "test",
	})
	if err != nil {
		t.Fatal(err)
	}
	for _, path := range []string{"/v1/models", "/readyz"} {
		req := httptest.NewRequest(http.MethodGet, path, nil)
		recorder := httptest.NewRecorder()
		handler.ServeHTTP(recorder, req)
		if recorder.Code != http.StatusUnauthorized {
			t.Fatalf("%s status = %d", path, recorder.Code)
		}
	}
}

func TestIsLoopbackListen(t *testing.T) {
	cases := map[string]bool{
		"127.0.0.1:18765": true,
		"[::1]:18765":     true,
		"localhost:18765": true,
		"0.0.0.0:18765":   false,
		":18765":          false,
	}
	for address, want := range cases {
		if got := IsLoopbackListen(address); got != want {
			t.Errorf("IsLoopbackListen(%q) = %v, want %v", address, got, want)
		}
	}
}
