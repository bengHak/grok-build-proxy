package monitor

import (
	"fmt"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/mattn/go-runewidth"
)

type Mode uint8

const (
	ModeDashboard Mode = iota
	ModeHelp
	ModeDetail
)

type View struct {
	Mode      Mode
	Selection int
	Address   string
	Version   string
	Now       func() time.Time
}

// HandleKey applies a complete key name and reports whether the program should
// quit. Supported names are also used by Program's input decoder.
func (v *View) HandleKey(key string, snapshot Snapshot) bool {
	switch key {
	case "q", "ctrl+c":
		return true
	case "?":
		if v.Mode == ModeHelp {
			v.Mode = ModeDashboard
		} else {
			v.Mode = ModeHelp
		}
	case "escape", "backspace":
		v.Mode = ModeDashboard
	case "enter":
		if v.Mode == ModeDashboard && selectableCount(snapshot) > 0 {
			v.Mode = ModeDetail
		}
	case "up", "k":
		if v.Mode == ModeDashboard && v.Selection > 0 {
			v.Selection--
		}
	case "down", "j":
		if v.Mode == ModeDashboard && v.Selection+1 < selectableCount(snapshot) {
			v.Selection++
		}
	}
	return false
}

func (v *View) Render(snapshot Snapshot, width, height int) string {
	if width < 1 {
		width = 1
	}
	if height < 1 {
		height = 1
	}
	if count := selectableCount(snapshot); count == 0 {
		v.Selection = 0
	} else if v.Selection >= count {
		v.Selection = count - 1
	}
	now := time.Now()
	if v.Now != nil {
		now = v.Now()
	}

	var lines []string
	lines = append(lines, fit("grok-build-proxy monitor  "+v.Address+"  "+v.Version, width))
	switch v.Mode {
	case ModeHelp:
		lines = append(lines,
			fit("Help", width),
			fit("↑/k ↓/j  move selection", width),
			fit("Enter    session/request details", width),
			fit("? Esc    close overlay", width),
			fit("q Ctrl-C quit and stop server", width),
		)
	case ModeDetail:
		lines = append(lines, renderDetail(selected(snapshot, v.Selection), now, width)...)
	default:
		lines = append(lines, renderDashboard(snapshot, v.Selection, now, width, height)...)
	}
	lines = append(lines, fit("? help  Enter details  ↑/↓ move  q quit", width))
	if len(lines) > height {
		lines = lines[:height]
	}
	return strings.Join(lines, "\n")
}

func renderDashboard(snapshot Snapshot, selection int, now time.Time, width, height int) []string {
	itemsBudget := height - 8 // title, five sections, throughput value, footer
	if itemsBudget < 0 {
		itemsBudget = 0
	}
	perSection := 1
	if itemsBudget >= 8 {
		perSection = 2
	}
	if itemsBudget == 0 {
		perSection = 0
	}
	var lines []string
	lines = append(lines, fit("Sessions", width))
	sessionSelection := selection
	if sessionSelection < 0 || sessionSelection >= len(snapshot.Sessions) {
		sessionSelection = -1
	}
	start, end := visibleRange(len(snapshot.Sessions), perSection, sessionSelection)
	for i := start; i < end; i++ {
		s := snapshot.Sessions[i]
		prefix := marker(i, selection)
		lines = append(lines, fit(fmt.Sprintf("%s %s  %s  %d active / %d total", prefix, short(s.ID, 16), s.Model, s.Active, s.Requests), width))
	}
	activeBase := len(snapshot.Sessions)
	lines = append(lines, fit("Active requests", width))
	activeSelection := selection - activeBase
	if activeSelection < 0 || activeSelection >= len(snapshot.Active) {
		activeSelection = -1
	}
	start, end = visibleRange(len(snapshot.Active), perSection, activeSelection)
	for i := start; i < end; i++ {
		r := snapshot.Active[i]
		lines = append(lines, fit(requestLine(marker(activeBase+i, selection), r, now), width))
	}
	recentBase := activeBase + len(snapshot.Active)
	lines = append(lines, fit("Recent requests", width))
	recentSelection := selection - recentBase
	if recentSelection < 0 || recentSelection >= len(snapshot.Recent) {
		recentSelection = -1
	}
	start, end = visibleRange(len(snapshot.Recent), perSection, recentSelection)
	for i := start; i < end; i++ {
		r := snapshot.Recent[i]
		lines = append(lines, fit(requestLine(marker(recentBase+i, selection), r, now), width))
	}
	lines = append(lines, fit("Output throughput", width))
	if len(snapshot.Sessions) == 0 {
		lines = append(lines, fit("  waiting for usage samples", width))
	} else {
		s := snapshot.Sessions[0]
		lines = append(lines, fit(fmt.Sprintf("  %s  %.1f tok/s  %d output tokens", short(s.ID, 16), s.TokensPerSecond(), s.OutputTokens), width))
	}
	lines = append(lines, fit("Error events", width))
	errorBase := recentBase + len(snapshot.Recent)
	errorSelection := selection - errorBase
	if errorSelection < 0 || errorSelection >= len(snapshot.Errors) {
		errorSelection = -1
	}
	start, end = visibleRange(len(snapshot.Errors), perSection, errorSelection)
	for i := start; i < end; i++ {
		r := snapshot.Errors[i]
		lines = append(lines, fit(fmt.Sprintf("%s %s  HTTP %d  %s", marker(errorBase+i, selection), short(r.ID, 12), r.StatusCode, r.Error), width))
	}
	return lines
}

type selectedItem struct {
	kind    string
	session *Session
	request *Request
}

func selected(snapshot Snapshot, target int) selectedItem {
	index := 0
	for i := range snapshot.Sessions {
		if index == target {
			return selectedItem{kind: "Session detail", session: &snapshot.Sessions[i]}
		}
		index++
	}
	for _, group := range [][]Request{snapshot.Active, snapshot.Recent, snapshot.Errors} {
		for i := range group {
			if index == target {
				return selectedItem{kind: "Request detail", request: &group[i]}
			}
			index++
		}
	}
	return selectedItem{kind: "Detail"}
}

func renderDetail(item selectedItem, now time.Time, width int) []string {
	lines := []string{fit(item.kind, width)}
	if item.session != nil {
		s := item.session
		lines = append(lines,
			fit("Session: "+s.ID, width),
			fit("Model: "+s.Model, width),
			fit(fmt.Sprintf("Requests: %d total, %d active", s.Requests, s.Active), width),
			fit(fmt.Sprintf("Output: %d tokens, %.1f tok/s", s.OutputTokens, s.TokensPerSecond()), width),
		)
	} else if item.request != nil {
		r := item.request
		lines = append(lines,
			fit("Request: "+r.ID, width),
			fit("Session: "+r.SessionID, width),
			fit("Model: "+r.RequestedModel+" → "+r.Model, width),
			fit(fmt.Sprintf("Status: %s / HTTP %d", r.Status, r.StatusCode), width),
			fit(fmt.Sprintf("Elapsed: %s / Output: %d / %.1f tok/s", durationText(r.Duration(now)), r.OutputTokens, r.TokensPerSecond()), width),
		)
		if r.Error != "" {
			lines = append(lines, fit("Error: "+r.Error, width))
		}
	} else {
		lines = append(lines, fit("No item selected", width))
	}
	lines = append(lines, fit("Esc/Backspace return", width))
	return lines
}

func visibleRange(length, limit, selected int) (int, int) {
	if length == 0 || limit <= 0 {
		return 0, 0
	}
	start := 0
	if selected >= limit {
		start = selected - limit + 1
	}
	if start+limit > length {
		start = length - limit
		if start < 0 {
			start = 0
		}
	}
	end := start + limit
	if end > length {
		end = length
	}
	return start, end
}

func selectableCount(snapshot Snapshot) int {
	return len(snapshot.Sessions) + len(snapshot.Active) + len(snapshot.Recent) + len(snapshot.Errors)
}

func marker(index, selected int) string {
	if index == selected {
		return ">"
	}
	return " "
}

func requestLine(prefix string, request Request, now time.Time) string {
	return fmt.Sprintf("%s %s  %s  %s  %s", prefix, short(request.ID, 12), request.Model, request.Status, durationText(request.Duration(now)))
}

func durationText(value time.Duration) string {
	if value < 0 {
		value = 0
	}
	if value < time.Second {
		return fmt.Sprintf("%dms", value.Milliseconds())
	}
	return fmt.Sprintf("%.1fs", value.Seconds())
}

func short(value string, max int) string {
	if utf8.RuneCountInString(value) <= max {
		return value
	}
	runes := []rune(value)
	if max <= 1 {
		return string(runes[:max])
	}
	return string(runes[:max-1]) + "…"
}

func fit(value string, width int) string {
	value = strings.ReplaceAll(value, "\n", " ")
	if width <= 0 {
		return ""
	}
	if runewidth.StringWidth(value) <= width {
		return value
	}
	if width == 1 {
		return runewidth.Truncate(value, 1, "")
	}
	return runewidth.Truncate(value, width-1, "") + "…"
}
