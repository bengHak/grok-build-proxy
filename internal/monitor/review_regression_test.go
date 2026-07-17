package monitor

import (
	"fmt"
	"strings"
	"testing"
	"time"
	"unicode/utf8"

	"github.com/bengHak/grok-build-proxy/internal/proxy"
)

func TestStateSanitizesAndBoundsObservedText(t *testing.T) {
	// Given
	state := NewState()
	unsafeModel := "gpt-5.5\x1b[2J" + strings.Repeat("x", 1_000)

	// When
	state.Apply(proxy.RequestEvent{
		Type:      proxy.RequestStarted,
		RequestID: "request\x1b[H",
		SessionID: "session\nname",
		Model:     unsafeModel,
		StartedAt: time.Now(),
	})
	snapshot := state.Snapshot()

	// Then
	if len(snapshot.Active) != 1 || len(snapshot.Sessions) != 1 {
		t.Fatalf("snapshot = %#v", snapshot)
	}
	for _, value := range []string{snapshot.Active[0].ID, snapshot.Active[0].SessionID, snapshot.Active[0].Model, snapshot.Sessions[0].ID} {
		if strings.ContainsAny(value, "\x1b\n\r") {
			t.Fatalf("unsafe control retained in %q", value)
		}
		if utf8.RuneCountInString(value) > 256 {
			t.Fatalf("unbounded observed text: %d runes", utf8.RuneCountInString(value))
		}
	}
}

func TestStateBoundsActiveRequestTracking(t *testing.T) {
	// Given
	state := NewState()
	now := time.Now()

	// When
	for i := 0; i < 500; i++ {
		state.Apply(proxy.RequestEvent{
			Type:      proxy.RequestStarted,
			RequestID: strings.Repeat("x", i+1),
			SessionID: strings.Repeat("s", i+1),
			StartedAt: now,
		})
	}

	// Then
	if snapshot := state.Snapshot(); len(snapshot.Active) > 200 || len(snapshot.Sessions) > 200 {
		t.Fatalf("active=%d sessions=%d", len(snapshot.Active), len(snapshot.Sessions))
	}
}

func TestStateSortsEqualTimestampsByStableID(t *testing.T) {
	// Given
	state := NewState()
	now := time.Now()
	state.Apply(proxy.RequestEvent{Type: proxy.RequestStarted, RequestID: "request-b", SessionID: "session-b", StartedAt: now})
	state.Apply(proxy.RequestEvent{Type: proxy.RequestStarted, RequestID: "request-a", SessionID: "session-a", StartedAt: now})

	// When
	snapshot := state.Snapshot()

	// Then
	if snapshot.Sessions[0].ID != "session-a" || snapshot.Active[0].ID != "request-a" {
		t.Fatalf("sessions=%#v active=%#v", snapshot.Sessions, snapshot.Active)
	}
}

func TestStatePrunesEqualTimestampSessionsDeterministically(t *testing.T) {
	// Given
	now := time.Now()

	// When / Then
	for attempt := 0; attempt < 20; attempt++ {
		state := NewState()
		for i := 0; i <= historyLimit; i++ {
			id := fmt.Sprintf("session-%02d", i)
			state.Apply(proxy.RequestEvent{Type: proxy.RequestCompleted, RequestID: "request-" + id, SessionID: id, StartedAt: now, EndedAt: now})
		}
		snapshot := state.Snapshot()
		if len(snapshot.Sessions) != historyLimit {
			t.Fatalf("sessions = %d", len(snapshot.Sessions))
		}
		for _, session := range snapshot.Sessions {
			if session.ID == "session-00" {
				t.Fatalf("nondeterministic tie retained session-00 on attempt %d", attempt)
			}
		}
	}
}

func TestStateDoesNotRegressSessionRecency(t *testing.T) {
	// Given
	state := NewState()
	newer := time.Date(2026, 7, 17, 12, 1, 0, 0, time.UTC)
	older := newer.Add(-time.Minute)

	// When
	state.Apply(proxy.RequestEvent{Type: proxy.RequestCompleted, RequestID: "newer", SessionID: "session", Model: "new-model", StartedAt: newer, EndedAt: newer})
	state.Apply(proxy.RequestEvent{Type: proxy.RequestCompleted, RequestID: "older", SessionID: "session", Model: "old-model", StartedAt: older, EndedAt: older})

	// Then
	session := state.Snapshot().Sessions[0]
	if session.LastSeen != newer || session.Model != "new-model" {
		t.Fatalf("session recency regressed: %#v", session)
	}
}

func TestStateKeepsCollidingDisplayIdentitiesDistinct(t *testing.T) {
	// Given
	state := NewState()
	now := time.Now()

	// When
	state.Apply(proxy.RequestEvent{Type: proxy.RequestStarted, RequestID: "request\u200bsame", SessionID: "session\u200bsame", StartedAt: now})
	state.Apply(proxy.RequestEvent{Type: proxy.RequestStarted, RequestID: "request\u200csame", SessionID: "session\u200csame", StartedAt: now})

	// Then
	if snapshot := state.Snapshot(); len(snapshot.Active) != 2 || len(snapshot.Sessions) != 2 {
		t.Fatalf("sanitized identities collapsed: active=%d sessions=%d", len(snapshot.Active), len(snapshot.Sessions))
	}
}

func TestViewKeepsDetailIdentityAcrossLiveUpdates(t *testing.T) {
	// Given
	state := NewState()
	base := time.Date(2026, 7, 17, 12, 0, 0, 0, time.UTC)
	state.Apply(proxy.RequestEvent{Type: proxy.RequestStarted, RequestID: "old-request", SessionID: "old-session", StartedAt: base})
	view := View{}
	snapshot := state.Snapshot()
	view.HandleKey("enter", snapshot)
	before := view.Render(snapshot, 60, 12)

	// When
	state.Apply(proxy.RequestEvent{Type: proxy.RequestStarted, RequestID: "new-request", SessionID: "new-session", StartedAt: base.Add(time.Second)})
	after := view.Render(state.Snapshot(), 60, 12)

	// Then
	if !strings.Contains(before, "Session: old-session") || !strings.Contains(after, "Session: old-session") {
		t.Fatalf("before:\n%s\nafter:\n%s", before, after)
	}
}

func TestViewKeepsDashboardSelectionIdentityAcrossLiveUpdates(t *testing.T) {
	// Given
	state := NewState()
	base := time.Date(2026, 7, 17, 12, 0, 0, 0, time.UTC)
	state.Apply(proxy.RequestEvent{Type: proxy.RequestStarted, RequestID: "old-request", SessionID: "old-session", StartedAt: base})
	view := View{}
	_ = view.Render(state.Snapshot(), 80, 12)

	// When
	state.Apply(proxy.RequestEvent{Type: proxy.RequestStarted, RequestID: "new-request", SessionID: "new-session", StartedAt: base.Add(time.Second)})
	output := view.Render(state.Snapshot(), 80, 12)

	// Then
	if !strings.Contains(output, "> old-session") {
		t.Fatalf("dashboard selection moved to a different identity:\n%s", output)
	}
}

func TestViewReturnsToSameRequestGroupAfterDetails(t *testing.T) {
	// Given
	request := Request{ID: "failed-request", Error: "boom"}
	snapshot := Snapshot{Recent: []Request{request}, Errors: []Request{request}}
	view := View{Selection: 1}
	view.HandleKey("enter", snapshot)

	// When
	view.HandleKey("escape", snapshot)

	// Then
	if view.Selection != 1 {
		t.Fatalf("selection returned to duplicate recent row: %d", view.Selection)
	}
}

func TestViewFollowsActiveRequestIntoRecentDetails(t *testing.T) {
	// Given
	state := NewState()
	started := time.Now()
	start := proxy.RequestEvent{Type: proxy.RequestStarted, RequestID: "request", SessionID: "session", Model: "model", StartedAt: started}
	state.Apply(start)
	view := View{Selection: 1}
	view.HandleKey("enter", state.Snapshot())

	// When
	start.Type = proxy.RequestCompleted
	start.EndedAt = started.Add(time.Second)
	start.StatusCode = 200
	state.Apply(start)
	output := view.Render(state.Snapshot(), 60, 12)

	// Then
	if !strings.Contains(output, "Request: request") || !strings.Contains(output, "Status: complete") {
		t.Fatalf("completed request detail was lost:\n%s", output)
	}
}

func TestHelpListsBackspaceNavigation(t *testing.T) {
	// Given
	view := View{Mode: ModeHelp}

	// When
	output := view.Render(Snapshot{}, 80, 12)

	// Then
	if !strings.Contains(output, "Backspace") {
		t.Fatalf("help omits Backspace:\n%s", output)
	}
}

func TestViewKeepsFooterVisibleInShortTerminal(t *testing.T) {
	// Given
	view := View{}
	snapshot := Snapshot{}

	// When
	output := view.Render(snapshot, 40, 3)

	// Then
	if !strings.Contains(output, "? help") {
		t.Fatalf("footer missing:\n%s", output)
	}
}

func TestViewKeepsSelectedErrorVisibleInShortTerminal(t *testing.T) {
	// Given
	view := View{Selection: 3}
	snapshot := Snapshot{
		Sessions: []Session{{ID: "session"}},
		Active:   []Request{{ID: "active"}},
		Recent:   []Request{{ID: "recent"}},
		Errors:   []Request{{ID: "selected-error", StatusCode: 502, Error: "upstream failed"}},
	}

	// When
	output := view.Render(snapshot, 60, 10)

	// Then
	if !strings.Contains(output, "> selected-er…") {
		t.Fatalf("selected error missing:\n%s", output)
	}
	if !strings.Contains(output, "? help") {
		t.Fatalf("footer missing:\n%s", output)
	}
}
