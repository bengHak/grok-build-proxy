package monitor_test

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/bengHak/grok-build-proxy/internal/auth"
	"github.com/bengHak/grok-build-proxy/internal/catalog"
	"github.com/bengHak/grok-build-proxy/internal/modelmap"
	"github.com/bengHak/grok-build-proxy/internal/monitor"
	"github.com/bengHak/grok-build-proxy/internal/proxy"
)

type integrationCredentials struct{}

func (integrationCredentials) Get(context.Context, bool) (auth.Credentials, error) {
	return auth.Credentials{AccessToken: "test-token", AccountID: "account"}, nil
}

func TestRealProxyHandlerFeedsDashboardAndPreservesHTTPRoutes(t *testing.T) {
	var calls atomic.Int32
	entered := make(chan struct{})
	release := make(chan struct{})
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer test-token" {
			t.Errorf("authorization = %q", r.Header.Get("Authorization"))
		}
		var body map[string]any
		if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
			t.Error(err)
		}
		w.Header().Set("Content-Type", "application/json")
		if calls.Add(1) == 1 {
			close(entered)
			<-release
			_, _ = io.WriteString(w, `{"id":"response-1","object":"response","usage":{"input_tokens":20,"output_tokens":120}}`)
			return
		}
		w.WriteHeader(http.StatusInternalServerError)
		_, _ = io.WriteString(w, `{"error":{"message":"upstream broke"}}`)
	}))
	defer upstream.Close()

	dashboard := monitor.NewDashboard()
	handler, err := proxy.New(proxy.Config{
		UpstreamURL: upstream.URL,
		Credentials: integrationCredentials{},
		Catalog:     catalog.New("gpt-5.5"),
		ModelMap:    modelmap.Map{},
		HTTPClient:  upstream.Client(),
		Logger:      slog.New(slog.NewTextHandler(io.Discard, nil)),
		Observer:    dashboard,
		Version:     "test-version",
	})
	if err != nil {
		t.Fatal(err)
	}

	for _, route := range []string{"/healthz", "/readyz", "/v1/models"} {
		recorder := httptest.NewRecorder()
		handler.ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, route, nil))
		if recorder.Code != http.StatusOK {
			t.Fatalf("%s status = %d body=%s", route, recorder.Code, recorder.Body.String())
		}
	}

	request := func(session string) *httptest.ResponseRecorder {
		recorder := httptest.NewRecorder()
		req := httptest.NewRequest(http.MethodPost, "/v1/responses", strings.NewReader(`{"model":"gpt-5.5","input":"hello","stream":false}`))
		req.Header.Set("x-grok-session-id", session)
		handler.ServeHTTP(recorder, req)
		return recorder
	}
	successDone := make(chan *httptest.ResponseRecorder, 1)
	go func() { successDone <- request("session-success") }()
	select {
	case <-entered:
	case <-time.After(2 * time.Second):
		t.Fatal("upstream request did not start")
	}
	activeSnapshot := dashboard.Snapshot()
	if len(activeSnapshot.Active) != 1 || activeSnapshot.Active[0].SessionID != "session-success" {
		t.Fatalf("active request was not visible: %#v", activeSnapshot.Active)
	}
	close(release)
	success := <-successDone
	if success.Code != http.StatusOK || !strings.Contains(success.Body.String(), `"response-1"`) {
		t.Fatalf("success response status=%d body=%s", success.Code, success.Body.String())
	}
	failure := request("")
	if failure.Code != http.StatusInternalServerError || !strings.Contains(failure.Body.String(), "upstream broke") {
		t.Fatalf("failure response status=%d body=%s", failure.Code, failure.Body.String())
	}

	snapshot := dashboard.Snapshot()
	if len(snapshot.Active) != 0 || len(snapshot.Recent) != 2 || len(snapshot.Errors) != 1 || len(snapshot.Sessions) != 2 {
		t.Fatalf("dashboard state: active=%d recent=%d errors=%d sessions=%d", len(snapshot.Active), len(snapshot.Recent), len(snapshot.Errors), len(snapshot.Sessions))
	}
	var completed monitor.Request
	for _, item := range snapshot.Recent {
		if item.SessionID == "session-success" {
			completed = item
		}
	}
	if completed.Status != "complete" || completed.StatusCode != 200 || completed.OutputTokens != 120 || completed.TokensPerSecond() <= 0 {
		t.Fatalf("completed request = %#v rate=%.2f", completed, completed.TokensPerSecond())
	}
	if snapshot.Errors[0].SessionID == "" || snapshot.Errors[0].SessionID == "default" || snapshot.Errors[0].SessionID != snapshot.Errors[0].ID || snapshot.Errors[0].StatusCode != 500 || snapshot.Errors[0].Error != "upstream returned HTTP 500" {
		t.Fatalf("error event = %#v", snapshot.Errors[0])
	}
}
