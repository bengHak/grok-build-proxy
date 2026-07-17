package monitor

import (
	"fmt"
	"testing"
	"time"

	"github.com/bengHak/grok-build-proxy/internal/proxy"
)

func TestStateTracksConcurrentSessionsCompletionFailureAndThroughput(t *testing.T) {
	state := NewState()
	base := time.Date(2026, 7, 17, 12, 0, 0, 0, time.UTC)
	startA := proxy.RequestEvent{Type: proxy.RequestStarted, RequestID: "req-a", SessionID: "session-a", RequestedModel: "grok-build", Model: "gpt-5.6-sol", StartedAt: base}
	startB := proxy.RequestEvent{Type: proxy.RequestStarted, RequestID: "req-b", SessionID: "session-b", Model: "gpt-5.5", StartedAt: base.Add(time.Second)}
	state.Apply(startA)
	state.Apply(startB)

	snapshot := state.Snapshot()
	if len(snapshot.Active) != 2 || len(snapshot.Sessions) != 2 {
		t.Fatalf("after starts: active=%d sessions=%d", len(snapshot.Active), len(snapshot.Sessions))
	}

	state.Apply(proxy.RequestEvent{Type: proxy.RequestCompleted, RequestID: "req-a", SessionID: "session-a", RequestedModel: "grok-build", Model: "gpt-5.6-sol", StartedAt: base, EndedAt: base.Add(2 * time.Second), StatusCode: 200, OutputTokens: 80})
	state.Apply(proxy.RequestEvent{Type: proxy.RequestFailed, RequestID: "req-b", SessionID: "session-b", Model: "gpt-5.5", StartedAt: base.Add(time.Second), EndedAt: base.Add(3 * time.Second), StatusCode: 502, Error: "upstream unavailable"})

	snapshot = state.Snapshot()
	if len(snapshot.Active) != 0 || len(snapshot.Recent) != 2 || len(snapshot.Errors) != 1 {
		t.Fatalf("terminal state: active=%d recent=%d errors=%d", len(snapshot.Active), len(snapshot.Recent), len(snapshot.Errors))
	}
	if got := snapshot.Errors[0]; got.ID != "req-b" || got.Status != "failed" || got.Error != "upstream unavailable" {
		t.Fatalf("error request = %#v", got)
	}
	var sessionA Session
	for _, session := range snapshot.Sessions {
		if session.ID == "session-a" {
			sessionA = session
		}
	}
	if sessionA.Requests != 1 || sessionA.Active != 0 || sessionA.OutputTokens != 80 {
		t.Fatalf("session-a = %#v", sessionA)
	}
	if got := sessionA.TokensPerSecond(); got != 40 {
		t.Fatalf("session throughput = %.2f, want 40", got)
	}
	if got := snapshot.Recent[1].TokensPerSecond(); got != 40 {
		t.Fatalf("request throughput = %.2f, want 40", got)
	}
}

func TestStateBoundsCompletedRequestAndSessionHistory(t *testing.T) {
	state := NewState()
	base := time.Now()
	for i := 0; i < 300; i++ {
		id := fmt.Sprintf("request-%d", i)
		session := fmt.Sprintf("session-%d", i)
		start := proxy.RequestEvent{Type: proxy.RequestStarted, RequestID: id, SessionID: session, StartedAt: base.Add(time.Duration(i) * time.Second)}
		state.Apply(start)
		state.Apply(proxy.RequestEvent{Type: proxy.RequestCompleted, RequestID: id, SessionID: session, StartedAt: start.StartedAt, EndedAt: start.StartedAt.Add(time.Second), StatusCode: 200})
	}
	snapshot := state.Snapshot()
	if len(snapshot.Sessions) > historyLimit || len(snapshot.Recent) > historyLimit || len(state.finished) > dedupLimit {
		t.Fatalf("unbounded history: sessions=%d recent=%d dedup=%d", len(snapshot.Sessions), len(snapshot.Recent), len(state.finished))
	}
}

func TestStateIgnoresDuplicateLifecycleEvents(t *testing.T) {
	state := NewState()
	now := time.Now()
	start := proxy.RequestEvent{Type: proxy.RequestStarted, RequestID: "req", SessionID: "session", StartedAt: now}
	done := proxy.RequestEvent{Type: proxy.RequestCompleted, RequestID: "req", SessionID: "session", StartedAt: now, EndedAt: now.Add(time.Second), StatusCode: 200, OutputTokens: 10}
	state.Apply(start)
	state.Apply(start)
	state.Apply(done)
	state.Apply(done)

	snapshot := state.Snapshot()
	if len(snapshot.Recent) != 1 || snapshot.Sessions[0].Requests != 1 || snapshot.Sessions[0].OutputTokens != 10 {
		t.Fatalf("duplicate events changed state: %#v", snapshot)
	}
}
