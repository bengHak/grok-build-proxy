package monitor

import (
	"bytes"
	"context"
	"errors"
	"io"
	"strings"
	"testing"
	"time"

	"github.com/bengHak/grok-build-proxy/internal/proxy"
	"github.com/mattn/go-runewidth"
)

func populatedDashboard(t *testing.T) (*Dashboard, time.Time) {
	t.Helper()
	dashboard := NewDashboard()
	now := time.Date(2026, 7, 17, 12, 0, 0, 0, time.UTC)
	dashboard.Observe(proxy.RequestEvent{Type: proxy.RequestStarted, RequestID: "active-request", SessionID: "session-a", Model: "gpt-5.6-sol", StartedAt: now.Add(-time.Second)})
	dashboard.Observe(proxy.RequestEvent{Type: proxy.RequestStarted, RequestID: "done-request", SessionID: "session-b", RequestedModel: "grok-build", Model: "gpt-5.5", StartedAt: now.Add(-2 * time.Second)})
	dashboard.Observe(proxy.RequestEvent{Type: proxy.RequestCompleted, RequestID: "done-request", SessionID: "session-b", RequestedModel: "grok-build", Model: "gpt-5.5", StartedAt: now.Add(-2 * time.Second), EndedAt: now, StatusCode: 200, OutputTokens: 50})
	dashboard.Observe(proxy.RequestEvent{Type: proxy.RequestStarted, RequestID: "failed-request", SessionID: "session-c", Model: "gpt-5.5", StartedAt: now.Add(-time.Second)})
	dashboard.Observe(proxy.RequestEvent{Type: proxy.RequestFailed, RequestID: "failed-request", SessionID: "session-c", Model: "gpt-5.5", StartedAt: now.Add(-time.Second), EndedAt: now, StatusCode: 502, Error: "network error"})
	return dashboard, now
}

func TestViewRendersAllDashboardRegionsAtResponsiveWidths(t *testing.T) {
	dashboard, now := populatedDashboard(t)
	for _, width := range []int{24, 48, 100} {
		view := View{Address: "127.0.0.1:18765", Version: "test", Now: func() time.Time { return now }}
		output := view.Render(dashboard.Snapshot(), width, 16)
		for _, heading := range []string{"Sessions", "Active requests", "Recent requests", "Output throughput", "Error events"} {
			if !strings.Contains(output, heading) {
				t.Errorf("width %d missing %q in:\n%s", width, heading, output)
			}
		}
		for _, line := range strings.Split(output, "\n") {
			if got := runewidth.StringWidth(line); got > width {
				t.Errorf("width %d rendered line of %d cells: %q", width, got, line)
			}
		}
	}
}

func TestFitUsesTerminalCellWidthForWideText(t *testing.T) {
	output := fit("세션🚀-long", 6)
	if width := runewidth.StringWidth(output); width > 6 {
		t.Fatalf("fit output uses %d cells: %q", width, output)
	}
}

func TestViewScrollsSelectedRowsIntoVisibleSectionWindow(t *testing.T) {
	snapshot := Snapshot{Sessions: []Session{{ID: "session-0"}, {ID: "session-1"}, {ID: "session-2"}, {ID: "session-3"}, {ID: "session-4"}}}
	view := View{Selection: 4}
	output := view.Render(snapshot, 40, 12)
	if !strings.Contains(output, "> session-4") {
		t.Fatalf("selected hidden row was not scrolled into view:\n%s", output)
	}
}

func TestViewHandlesHelpDetailSelectionAndTinyTerminal(t *testing.T) {
	dashboard, now := populatedDashboard(t)
	snapshot := dashboard.Snapshot()
	view := View{Now: func() time.Time { return now }}
	if view.HandleKey("down", snapshot) || view.Selection != 1 {
		t.Fatalf("down selection = %d", view.Selection)
	}
	view.HandleKey("enter", snapshot)
	if output := view.Render(snapshot, 32, 10); !strings.Contains(output, "Session detail") {
		t.Fatalf("detail output:\n%s", output)
	}
	view.HandleKey("escape", snapshot)
	view.Selection = len(snapshot.Sessions)
	view.HandleKey("enter", snapshot)
	if output := view.Render(snapshot, 40, 10); !strings.Contains(output, "Request detail") || !strings.Contains(output, "active-request") {
		t.Fatalf("request detail output:\n%s", output)
	}
	view.HandleKey("escape", snapshot)
	view.HandleKey("?", snapshot)
	if output := view.Render(snapshot, 20, 8); !strings.Contains(output, "Help") {
		t.Fatalf("help output:\n%s", output)
	}
	// Pathological dimensions must remain bounded and panic-free.
	if output := view.Render(snapshot, 1, 1); len(strings.Split(output, "\n")) != 1 {
		t.Fatalf("tiny output = %q", output)
	}
	if !view.HandleKey("q", snapshot) {
		t.Fatal("q did not request quit")
	}
}

type fakeTerminal struct {
	width, height int
	entered       bool
	restored      bool
}

func (t *fakeTerminal) Size() (int, int) { return t.width, t.height }
func (t *fakeTerminal) EnterRaw() (func(), error) {
	t.entered = true
	return func() { t.restored = true }, nil
}

type failFirstWriter struct {
	bytes.Buffer
	calls int
}

func (w *failFirstWriter) Write(p []byte) (int, error) {
	w.calls++
	if w.calls == 1 {
		_, _ = w.Buffer.Write(p[:1])
		return 1, io.ErrClosedPipe
	}
	return w.Buffer.Write(p)
}

func TestProgramCleansUpAfterPartialAlternateScreenWrite(t *testing.T) {
	terminal := &fakeTerminal{width: 60, height: 16}
	output := &failFirstWriter{}
	program := Program{Dashboard: NewDashboard(), Input: strings.NewReader(""), Output: output, Terminal: terminal}
	if err := program.Run(context.Background()); !errors.Is(err, io.ErrClosedPipe) {
		t.Fatalf("error = %v", err)
	}
	if !terminal.restored || !strings.Contains(output.String(), "\x1b[?25h") || !strings.Contains(output.String(), "\x1b[?1049l") {
		t.Fatalf("partial-write cleanup missing: restored=%v output=%q", terminal.restored, output.String())
	}
}

func TestProgramQuitRestoresTerminalAndCursor(t *testing.T) {
	dashboard, _ := populatedDashboard(t)
	terminal := &fakeTerminal{width: 60, height: 16}
	var output bytes.Buffer
	program := Program{
		Dashboard: dashboard,
		Input:     strings.NewReader("?\x1b[Bq"),
		Output:    &output,
		Terminal:  terminal,
		Address:   "127.0.0.1:18765",
		Version:   "test",
		Refresh:   time.Hour,
	}
	if err := program.Run(context.Background()); err != nil {
		t.Fatal(err)
	}
	if !terminal.entered || !terminal.restored {
		t.Fatalf("terminal lifecycle entered=%v restored=%v", terminal.entered, terminal.restored)
	}
	text := output.String()
	if !strings.Contains(text, "Sessions") || !strings.Contains(text, "Help") {
		t.Fatalf("program did not render dashboard and help:\n%s", text)
	}
	if !strings.Contains(text, "\x1b[?25h") || !strings.Contains(text, "\x1b[?1049l") {
		t.Fatalf("terminal cleanup sequence missing: %q", text)
	}
}
