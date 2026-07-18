//! Interactive serve monitor (ratatui panels: header / metrics / sessions / session detail / failures / footer).

mod app;
mod theme;
mod widgets;

use app::{App, Focus, Mode};
use crossterm::{
    cursor,
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    text::Span,
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
use std::{
    io::{self, IsTerminal},
    path::Path,
    sync::Arc,
    time::Duration,
};
use theme::Theme;
use widgets::{
    FailuresPanel, Footer, Header, HelpOverlay, MetricsStrip, SessionDetailPanel, SessionsPanel,
    TurnKind, active_sessions, fleet_avg_tok_s, should_show_metrics, truncate,
};

/// Below this width, panels stack as a single focused "tab" instead of side-by-side.
const NARROW_WIDTH: u16 = 80;

use crate::report::{self, ReportMeta};
pub use crate::store::{Dashboard, FailureRecord, Request, Session, Snapshot};

/// Max chars shown for `error_message` in the failure detail modal (full value stays in store).
const DETAIL_MESSAGE_MAX: usize = 120;

pub fn is_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

struct TerminalGuard;
impl TerminalGuard {
    /// Enter alternate screen + raw mode. On partial failure, roll back so the
    /// terminal is never left in raw mode without a live guard.
    fn enter() -> io::Result<Self> {
        execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
        if let Err(e) = terminal::enable_raw_mode() {
            let _ = execute!(io::stdout(), cursor::Show, LeaveAlternateScreen);
            return Err(e);
        }
        Ok(Self)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), cursor::Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

pub async fn run(dashboard: Arc<Dashboard>, address: &str, version: &str) -> io::Result<()> {
    let _guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut app = App::new();
    let theme = Theme::default();

    loop {
        let snapshot = dashboard.snapshot();
        let active = active_sessions(&snapshot);
        let sessions_len = active.len();
        let failures_len = FailuresPanel::row_count(&snapshot, app.failure_filter);
        app.tick_toast();
        app.sync_selected_session(&active);
        let detail_len =
            SessionDetailPanel::row_count(&snapshot, app.selected_session_id.as_deref());
        app.clamp_selection(sessions_len, detail_len, failures_len);

        // 1 Hz fleet-average tok/s sample for the metrics sparkline.
        if app.should_sample_tok_s() {
            dashboard.push_tok_s_sample(fleet_avg_tok_s(&snapshot));
            app.mark_tok_sampled();
        }

        terminal.draw(|frame| {
            draw(
                frame.area(),
                frame.buffer_mut(),
                &snapshot,
                address,
                version,
                &app,
                theme,
            );
        })?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                // Crossterm may emit Press/Release/Repeat; accept Press + Repeat
                // so held j/k navigates under enhanced keyboard protocols.
                if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    continue;
                }
                let prev_focus = app.focus;
                if app.handle(key, sessions_len, detail_len, failures_len) {
                    return Ok(());
                }
                // After Tab into Sessions, restore selection to the pinned session.
                if app.focus == Focus::Sessions && prev_focus != Focus::Sessions {
                    app.restore_session_selection(&active);
                }
                // Sessions navigation updates the detail pin.
                if app.focus == Focus::Sessions {
                    app.pin_session_from_selection(&active);
                }
                // Report export: filtered failures → clipboard (y/Y) or file (w/W).
                if let Some(outcome) = try_export(key.code, &snapshot, &app, address, version) {
                    app.set_toast(outcome.toast());
                }
                // After Enter on Failures, pin stable request_id so live push_front
                // does not swap the overlay to a different row.
                if app.mode == Mode::Detail
                    && app.focus == Focus::Failures
                    && app.detail_request_id.is_none()
                {
                    let rows = FailuresPanel::ordered(&snapshot, app.failure_filter);
                    if let Some(f) = rows.get(app.selected) {
                        app.pin_failure_detail(f.request_id.clone());
                    }
                }
                // Filter cycle (`f`) may shrink the list; re-clamp with new filter length.
                let failures_len = FailuresPanel::row_count(&snapshot, app.failure_filter);
                let detail_len =
                    SessionDetailPanel::row_count(&snapshot, app.selected_session_id.as_deref());
                app.clamp_selection(sessions_len, detail_len, failures_len);
            }
        }
        tokio::task::yield_now().await;
    }
}

/// Handle y/Y (copy) and w/W (write). Returns `None` for unrelated keys
/// or when an overlay (Help/Detail) is open — export is Dashboard-only.
fn try_export(
    code: event::KeyCode,
    snapshot: &Snapshot,
    app: &App,
    address: &str,
    version: &str,
) -> Option<report::ExportOutcome> {
    try_export_to(code, snapshot, app, address, version, None)
}

fn try_export_to(
    code: event::KeyCode,
    snapshot: &Snapshot,
    app: &App,
    address: &str,
    version: &str,
    write_dir: Option<&Path>,
) -> Option<report::ExportOutcome> {
    use event::KeyCode;
    let json = match code {
        KeyCode::Char('y' | 'w') => false,
        KeyCode::Char('Y' | 'W') => true,
        _ => return None,
    };
    // Avoid accidental clipboard/file writes while reading help or detail.
    if app.mode != Mode::Dashboard {
        return None;
    }
    let copy = matches!(code, KeyCode::Char('y' | 'Y'));
    // Export the currently filtered set (group display order for readability).
    let records: Vec<FailureRecord> = FailuresPanel::ordered(snapshot, app.failure_filter)
        .into_iter()
        .cloned()
        .collect();
    let meta = ReportMeta::new(version, address, app.failure_filter.as_str());
    Some(if copy {
        report::export_copy(&records, &meta, json)
    } else if let Some(dir) = write_dir {
        report::export_write_to(&records, &meta, json, dir)
    } else {
        report::export_write(&records, &meta, json)
    })
}

fn draw(
    area: Rect,
    buf: &mut ratatui::buffer::Buffer,
    snapshot: &Snapshot,
    address: &str,
    version: &str,
    app: &App,
    theme: Theme,
) {
    // Clear full frame first.
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
            }
        }
    }

    let show_metrics = should_show_metrics(area.width, area.height);
    let narrow = area.width < NARROW_WIDTH;

    let chunks = if show_metrics {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // header
                Constraint::Length(3), // metrics sparklines
                Constraint::Min(5),    // body
                Constraint::Length(3), // footer
            ])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // header
                Constraint::Min(5),    // body
                Constraint::Length(3), // footer
            ])
            .split(area)
    };

    let (header_area, metrics_area, body_area, footer_area) = if show_metrics {
        (chunks[0], Some(chunks[1]), chunks[2], chunks[3])
    } else {
        (chunks[0], None, chunks[1], chunks[2])
    };

    Header {
        snapshot,
        address,
        version,
        uptime_secs: app.uptime_secs(),
        theme,
    }
    .render(header_area, buf);

    if let Some(metrics) = metrics_area {
        MetricsStrip { snapshot, theme }.render(metrics, buf);
    }

    if narrow {
        // Single focused panel (tab-like): Tab still cycles Sessions → Active → Failures.
        draw_focused_panel(body_area, buf, snapshot, app, theme);
    } else {
        // Body: top row sessions|session detail, bottom full-width failures.
        let body = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(body_area);

        let top = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(body[0]);

        SessionsPanel {
            snapshot,
            selected: app.selected,
            focused: app.focus == Focus::Sessions && app.mode == Mode::Dashboard,
            theme,
        }
        .render(top[0], buf);

        SessionDetailPanel {
            snapshot,
            session_id: app.selected_session_id.as_deref(),
            selected: app.selected,
            focused: app.focus == Focus::SessionDetail && app.mode == Mode::Dashboard,
            theme,
        }
        .render(top[1], buf);

        FailuresPanel {
            snapshot,
            selected: app.selected,
            focused: app.focus == Focus::Failures && app.mode == Mode::Dashboard,
            filter: app.failure_filter,
            theme,
        }
        .render(body[1], buf);
    }

    Footer {
        theme,
        toast: app.toast_message(),
    }
    .render(footer_area, buf);

    match app.mode {
        Mode::Help => HelpOverlay { theme }.render(area, buf),
        Mode::Detail => draw_detail(area, buf, snapshot, app, theme),
        Mode::Dashboard => {}
    }
}

/// Narrow terminal: only the focused panel fills the body (Tab cycles).
fn draw_focused_panel(
    area: Rect,
    buf: &mut ratatui::buffer::Buffer,
    snapshot: &Snapshot,
    app: &App,
    theme: Theme,
) {
    let dash = app.mode == Mode::Dashboard;
    match app.focus {
        Focus::Sessions => SessionsPanel {
            snapshot,
            selected: app.selected,
            focused: dash,
            theme,
        }
        .render(area, buf),
        Focus::SessionDetail => SessionDetailPanel {
            snapshot,
            session_id: app.selected_session_id.as_deref(),
            selected: app.selected,
            focused: dash,
            theme,
        }
        .render(area, buf),
        Focus::Failures => FailuresPanel {
            snapshot,
            selected: app.selected,
            focused: dash,
            filter: app.failure_filter,
            theme,
        }
        .render(area, buf),
    }
}

fn draw_detail(
    area: Rect,
    buf: &mut ratatui::buffer::Buffer,
    snapshot: &Snapshot,
    app: &App,
    theme: Theme,
) {
    let text = detail_text(snapshot, app);
    let width = area.width.min(76);
    // Size height from content so longer failure detail is not hard-capped at 18.
    // +2 for border rows; leave at least 2 rows of margin when possible.
    let content_lines = text.lines().count() as u16;
    let height = (content_lines.saturating_add(2))
        .max(5)
        .min(area.height.saturating_sub(2).max(5))
        .min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal = Rect {
        x,
        y,
        width,
        height,
    };

    Clear.render(modal, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.active)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .title(Span::styled(" detail ", theme.title));
    Paragraph::new(text).block(block).render(modal, buf);
}

fn detail_text(snapshot: &Snapshot, app: &App) -> String {
    match app.focus {
        Focus::Sessions => {
            let sessions = active_sessions(snapshot);
            if let Some(s) = sessions.get(app.selected) {
                let last = s.last_failure_kind.map(|k| k.as_str()).unwrap_or("-");
                format!(
                    "Session {}\n  model: {}\n  requests: {}  active: {}  errors: {}\n  tokens: {}  tok/s: {:.1}\n  last_failure: {last}",
                    s.id,
                    s.last_model,
                    s.requests,
                    s.active,
                    s.errors,
                    s.output_tokens,
                    s.tokens_per_second(),
                )
            } else {
                "No session selected".into()
            }
        }
        Focus::SessionDetail => {
            let rows = SessionDetailPanel::rows(snapshot, app.selected_session_id.as_deref());
            if let Some((kind, r)) = rows.get(app.selected) {
                let kind_label = match kind {
                    TurnKind::Active => "active",
                    TurnKind::Recent => "recent",
                };
                let err = if r.error.is_empty() {
                    r.error_type.as_str()
                } else {
                    r.error.as_str()
                };
                let fk = r.failure_kind.map(|k| k.as_str()).unwrap_or("-");
                format!(
                    "Turn {} ({kind_label})\n  session: {}\n  model: {} (requested {})\n  status: {}  attempt: {}\n  duration: {:.1}s  tokens: {}\n  failure: {fk}\n  error: {err}",
                    r.id,
                    r.session_id,
                    r.model,
                    r.requested_model,
                    r.status,
                    r.attempt,
                    r.duration().as_secs_f64(),
                    r.output_tokens,
                )
            } else {
                "No turn selected".into()
            }
        }
        Focus::Failures => failure_detail_text(snapshot, app),
    }
}

fn failure_detail_text(snapshot: &Snapshot, app: &App) -> String {
    // Prefer stable request_id so live push_front / ring eviction does not swap identity.
    let record = if let Some(id) = app.detail_request_id.as_ref() {
        match snapshot.failures.iter().find(|f| &f.request_id == id) {
            Some(f) => f,
            None => return format!("Failure {id}\n  (failure no longer in ring)"),
        }
    } else {
        // Fallback for tests / before pin: index into group-ordered list.
        match FailuresPanel::ordered(snapshot, app.failure_filter).get(app.selected) {
            Some(f) => *f,
            None => return "No failure selected".into(),
        }
    };

    let msg = if record.error_message.is_empty() {
        "-".to_owned()
    } else {
        truncate(&record.error_message, DETAIL_MESSAGE_MAX)
    };
    let err_type = if record.error_type.is_empty() {
        "-"
    } else {
        record.error_type.as_str()
    };
    let resp = if record.response_id.is_empty() {
        "-"
    } else {
        record.response_id.as_str()
    };

    format!(
        "Failure {}\n  ts: {}\n  kind: {}\n  session: {}\n  model: {} (requested {})\n  status: {}  attempt: {}\n  duration_ms: {}  session_fail#: {}\n  error_type: {}\n  message: {}\n  response_id: {}\n  mapped: {}  lite: {}  fast: {}\n  auth_retried: {}  outputs: {}  capture_bytes: {}",
        record.request_id,
        record.ts.to_rfc3339(),
        record.kind.as_str(),
        record.session_id,
        record.model,
        record.requested_model,
        record.status_code,
        record.attempt,
        record.duration_ms,
        record.session_failure_index,
        err_type,
        msg,
        resp,
        record.mapped,
        record.lite,
        record.fast,
        record.auth_retried,
        record.output_count,
        record.capture_bytes,
    )
}

/// Render into a TestBackend buffer (unit tests / snapshots).
#[cfg(test)]
pub fn render_test(
    width: u16,
    height: u16,
    snapshot: &Snapshot,
    address: &str,
    version: &str,
    app: &App,
) -> String {
    use ratatui::backend::TestBackend;
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    let theme = Theme::default();
    terminal
        .draw(|frame| {
            draw(
                frame.area(),
                frame.buffer_mut(),
                snapshot,
                address,
                version,
                app,
                theme,
            );
        })
        .expect("draw");
    buffer_to_string(terminal.backend().buffer())
}

#[cfg(test)]
fn buffer_to_string(buf: &ratatui::buffer::Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                out.push_str(cell.symbol());
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{FailureKind, Observer, RequestEvent, RequestEventKind};
    use crate::monitor::app::FailureFilter;
    use std::time::{Duration, Instant};

    fn base_event(kind: RequestEventKind) -> RequestEvent {
        RequestEvent {
            kind,
            request_id: "req-1".into(),
            session_id: "sess-abc".into(),
            requested_model: "alias".into(),
            model: "gpt-test".into(),
            status_code: 200,
            output_tokens: 40,
            error: String::new(),
            started_at: Instant::now() - Duration::from_secs(2),
            duration_ms: 2000,
            failure_kind: None,
            error_type: String::new(),
            response_id: String::new(),
            mapped: true,
            lite: true,
            fast: false,
            auth_retried: false,
            attempt: 1,
            output_count: 0,
            capture_bytes: 0,
        }
    }

    fn fixture_dashboard() -> Dashboard {
        let d = Dashboard::new();
        d.observe(base_event(RequestEventKind::Started));
        d.observe(base_event(RequestEventKind::Completed));
        let mut fail = base_event(RequestEventKind::Started);
        fail.request_id = "req-2".into();
        d.observe(fail);
        let mut failed = base_event(RequestEventKind::Failed);
        failed.request_id = "req-2".into();
        failed.failure_kind = Some(FailureKind::UpstreamHttp);
        failed.error_type = "upstream_http".into();
        failed.status_code = 502;
        d.observe(failed);

        // Second failure: ProxyAssemble for filter tests.
        let mut fail3 = base_event(RequestEventKind::Started);
        fail3.request_id = "req-3".into();
        d.observe(fail3);
        let mut failed3 = base_event(RequestEventKind::Failed);
        failed3.request_id = "req-3".into();
        failed3.failure_kind = Some(FailureKind::ProxyAssemble);
        failed3.error_type = "proxy_incomplete_output".into();
        failed3.status_code = 200;
        failed3.error = "could not assemble".into();
        failed3.output_count = 2;
        failed3.capture_bytes = 4096;
        d.observe(failed3);

        // Leave one in-flight turn for the active panel.
        let mut inflight = base_event(RequestEventKind::Started);
        inflight.request_id = "req-live".into();
        d.observe(inflight);
        d
    }

    #[test]
    fn render_header_sessions_failures_footer() {
        let snap = fixture_dashboard().snapshot();
        let mut app = App::new();
        let active = active_sessions(&snap);
        app.sync_selected_session(&active);
        let text = render_test(100, 28, &snap, "127.0.0.1:18765", "0.0.12", &app);
        assert!(
            text.contains("grok-build-proxy"),
            "header missing version banner:\n{text}"
        );
        assert!(
            text.contains("127.0.0.1:18765"),
            "header missing listen address:\n{text}"
        );
        assert!(
            text.contains("active↑1"),
            "header missing active count from store:\n{text}"
        );
        assert!(
            text.contains("err●2"),
            "header missing failure count from store:\n{text}"
        );
        assert!(
            text.contains("sessions"),
            "sessions panel title missing:\n{text}"
        );
        assert!(
            text.contains("session detail"),
            "session detail panel title missing:\n{text}"
        );
        assert!(
            text.contains("failures"),
            "failures panel title missing:\n{text}"
        );
        assert!(
            text.contains("[All]") && text.contains("2/2"),
            "failures filter title missing All 2/2:\n{text}"
        );
        assert!(text.contains("sess-abc"), "session id missing:\n{text}");
        assert!(
            text.contains("UpstreamHttp") || text.contains("upstream_http"),
            "failure kind/error_type missing in failures panel:\n{text}"
        );
        assert!(
            text.contains("ProxyAssemble") || text.contains("proxy_incomplete"),
            "ProxyAssemble failure missing in panel:\n{text}"
        );
        assert!(
            text.contains("req-live"),
            "active turn id from store missing:\n{text}"
        );
        assert!(
            text.contains("j/k") || text.contains("quit") || text.contains("filter"),
            "footer bindings missing:\n{text}"
        );
        assert!(
            text.contains(" y ") || text.contains("copy") || text.contains(" w "),
            "footer missing export bindings:\n{text}"
        );
        assert!(
            text.contains("metrics"),
            "metrics strip title missing on tall/wide terminal:\n{text}"
        );
        assert!(
            text.contains("tok/s") && text.contains("fail%") && text.contains("done"),
            "metrics strip labels missing:\n{text}"
        );
    }

    #[test]
    fn narrow_terminal_shows_only_focused_panel() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let snap = fixture_dashboard().snapshot();
        let mut app = App::new();
        let active = active_sessions(&snap);
        app.sync_selected_session(&active);
        // Width < 80 → single focused panel (sessions by default).
        let text = render_test(60, 24, &snap, "127.0.0.1:1", "0.0.12", &app);
        assert!(
            text.contains("sessions"),
            "narrow should show focused sessions panel:\n{text}"
        );
        assert!(
            !text.contains("session detail"),
            "narrow should hide non-focused session detail panel:\n{text}"
        );
        assert!(
            !text.contains("failures ["),
            "narrow should hide non-focused failures panel:\n{text}"
        );

        // Tab advances focus under the real key path; re-render at narrow width.
        app.handle(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            active.len(),
            SessionDetailPanel::row_count(&snap, app.selected_session_id.as_deref()),
            FailuresPanel::row_count(&snap, app.failure_filter),
        );
        assert_eq!(app.focus, Focus::SessionDetail);
        let text = render_test(60, 24, &snap, "127.0.0.1:1", "0.0.12", &app);
        assert!(
            text.contains("session detail"),
            "narrow session-detail focus should show session detail panel:\n{text}"
        );
        assert!(
            !text.contains("sessions"),
            "narrow session-detail focus should hide sessions title:\n{text}"
        );
        assert!(
            !text.contains("failures ["),
            "narrow session-detail focus should hide failures title:\n{text}"
        );

        app.focus = Focus::Failures;
        let text = render_test(60, 24, &snap, "127.0.0.1:1", "0.0.12", &app);
        assert!(
            text.contains("failures"),
            "narrow failures focus should show failures:\n{text}"
        );
        assert!(
            !text.contains("sessions"),
            "narrow failures focus should hide sessions title:\n{text}"
        );
    }

    #[test]
    fn short_terminal_hides_metrics_strip() {
        let snap = fixture_dashboard().snapshot();
        let app = App::new();
        // height 16 < 18 threshold → no metrics strip, but body still renders.
        let text = render_test(100, 16, &snap, "127.0.0.1:1", "0.0.12", &app);
        assert!(
            !text.contains(" metrics "),
            "metrics strip should hide when height is short:\n{text}"
        );
        assert!(
            text.contains("sessions") || text.contains("header"),
            "short layout should still draw core chrome:\n{text}"
        );
    }

    #[test]
    fn metrics_strip_reflects_store_samples() {
        let d = fixture_dashboard();
        let snap = d.snapshot();
        assert!(
            !snap.metrics_completed.is_empty(),
            "fixture should record completed samples"
        );
        // tok/s spark ring is 1 Hz fleet avg (not per-completion); push one sample.
        let avg = fleet_avg_tok_s(&snap);
        assert!(
            avg > 0.0,
            "fixture sessions should have a positive fleet avg"
        );
        d.push_tok_s_sample(avg);
        let snap = d.snapshot();
        assert!(
            !snap.metrics_tok_s.is_empty(),
            "push_tok_s_sample should fill metrics_tok_s"
        );
        // Fixture: 1 Completed + 2 Failed → fail% 67%, done 1ok/2f.
        assert_eq!(
            snap.metrics_completed.iter().filter(|&&v| v >= 0.5).count(),
            1
        );
        assert_eq!(
            snap.metrics_completed.iter().filter(|&&v| v < 0.5).count(),
            2
        );
        let app = App::new();
        let text = render_test(120, 28, &snap, "127.0.0.1:1", "0.0.12", &app);
        assert!(
            text.contains("fail%") && text.contains("67%"),
            "rolling fail% meter should show 67% for fixture (1 ok / 2 fail):\n{text}"
        );
        assert!(
            text.contains("1ok/2f"),
            "done activity should show 1ok/2f:\n{text}"
        );
        // Live fleet-average tok/s from session lifetime rates.
        assert!(
            text.contains("tok/s") && !text.contains("tok/s 0.0"),
            "tok/s should be non-zero from fixture session rates:\n{text}"
        );
    }

    #[test]
    fn metrics_strip_cold_start_empty_snapshot() {
        let snap = Snapshot::default();
        let app = App::new();
        // Must not panic; strip paints zero meters and empty sparklines.
        let text = render_test(100, 24, &snap, "127.0.0.1:1", "0.0.12", &app);
        assert!(
            text.contains("metrics"),
            "empty snapshot should still paint metrics strip:\n{text}"
        );
        assert!(
            text.contains("tok/s") && text.contains("0.0"),
            "cold-start tok/s should be 0.0:\n{text}"
        );
        assert!(
            text.contains("fail%") && text.contains("0%"),
            "cold-start fail% should be 0%:\n{text}"
        );
        assert!(
            text.contains("0ok/0f"),
            "cold-start done should be 0ok/0f:\n{text}"
        );
        assert!(
            text.contains('·'),
            "empty sparklines should paint middot placeholders:\n{text}"
        );
    }

    #[test]
    fn footer_shows_toast() {
        let snap = Snapshot::default();
        let mut app = App::new();
        app.set_toast("wrote 2 failures (md) → /tmp/x.md");
        let text = render_test(100, 20, &snap, "127.0.0.1:1", "0.0.12", &app);
        assert!(
            text.contains("wrote 2 failures") || text.contains("status"),
            "toast missing from footer:\n{text}"
        );
    }

    #[test]
    fn failures_filter_hides_non_matching() {
        let snap = fixture_dashboard().snapshot();
        assert_eq!(
            FailuresPanel::row_count(&snap, FailureFilter::ProxyAssemble),
            1
        );
        assert_eq!(FailuresPanel::row_count(&snap, FailureFilter::All), 2);

        let mut app = App::new();
        app.failure_filter = FailureFilter::ProxyAssemble;
        let text = render_test(100, 28, &snap, "127.0.0.1:18765", "0.0.12", &app);
        assert!(
            text.contains("failures [ProxyAssemble] 1/2"),
            "title should show filter and 1/2 counts:\n{text}"
        );
        assert!(
            !text.contains("UpstreamHttp"),
            "UpstreamHttp should be filtered out of list:\n{text}"
        );
    }

    #[test]
    fn detail_text_failure_fields_and_stable_id() {
        let snap = fixture_dashboard().snapshot();
        // Newest-first: req-3 ProxyAssemble, then req-2 UpstreamHttp.
        let mut app = App::new();
        app.focus = Focus::Failures;
        app.selected = 0;
        app.pin_failure_detail("req-3");
        let text = failure_detail_text(&snap, &app);
        assert!(text.starts_with("Failure req-3"), "header:\n{text}");
        assert!(text.contains("  ts:"), "ts label:\n{text}");
        assert!(text.contains("  kind: ProxyAssemble"), "kind:\n{text}");
        assert!(text.contains("  duration_ms:"), "duration_ms:\n{text}");
        assert!(text.contains("  session_fail#:"), "session_fail#:\n{text}");
        assert!(text.contains("  error_type:"), "error_type:\n{text}");
        assert!(text.contains("  auth_retried:"), "auth_retried:\n{text}");
        assert!(text.contains("  capture_bytes:"), "capture_bytes:\n{text}");
        assert!(
            text.contains("proxy_incomplete_output"),
            "etype val:\n{text}"
        );

        // Pinned id survives selection/index drift.
        app.selected = 99;
        let text2 = failure_detail_text(&snap, &app);
        assert!(
            text2.starts_with("Failure req-3"),
            "stable id ignored selected index:\n{text2}"
        );

        // Evicted / unknown id.
        app.pin_failure_detail("req-gone");
        let gone = failure_detail_text(&snap, &app);
        assert!(
            gone.contains("failure no longer in ring"),
            "missing eviction message:\n{gone}"
        );
    }

    #[test]
    fn detail_overlay_renders_failure_fields() {
        let snap = fixture_dashboard().snapshot();
        let mut app = App::new();
        app.focus = Focus::Failures;
        app.selected = 0;
        app.mode = Mode::Detail;
        app.pin_failure_detail("req-3");
        let text = render_test(100, 30, &snap, "127.0.0.1:18765", "0.0.12", &app);
        assert!(text.contains("detail"), "detail title missing:\n{text}");
        // Labels unique to the detail template (not the list row format).
        assert!(
            text.contains("duration_ms:"),
            "detail missing duration_ms:\n{text}"
        );
        assert!(
            text.contains("session_fail#:"),
            "detail missing session_fail#:\n{text}"
        );
        assert!(
            text.contains("capture_bytes:"),
            "detail missing capture_bytes:\n{text}"
        );
        assert!(
            text.contains("auth_retried:"),
            "detail missing auth_retried:\n{text}"
        );
        assert!(
            text.contains("Failure req-3"),
            "detail missing Failure header:\n{text}"
        );
    }

    #[test]
    fn detail_overlay_renders_turn_fields() {
        let snap = fixture_dashboard().snapshot();
        let mut app = App::new();
        let active = active_sessions(&snap);
        app.sync_selected_session(&active);
        app.focus = Focus::SessionDetail;
        app.selected = 0; // req-live (active first for pinned session)
        app.mode = Mode::Detail;
        let text = render_test(100, 28, &snap, "127.0.0.1:18765", "0.0.12", &app);
        assert!(text.contains("detail"), "detail title missing:\n{text}");
        assert!(
            text.contains("req-live"),
            "detail missing active turn id:\n{text}"
        );
        assert!(
            text.contains("sess-abc"),
            "detail missing session id:\n{text}"
        );
        assert!(text.contains("gpt-test"), "detail missing model:\n{text}");
        assert!(
            text.contains("active"),
            "detail missing kind label:\n{text}"
        );
    }

    #[test]
    fn detail_overlay_renders_session_fields() {
        let snap = fixture_dashboard().snapshot();
        let mut app = App::new();
        let active = active_sessions(&snap);
        app.sync_selected_session(&active);
        app.focus = Focus::Sessions;
        app.selected = 0;
        app.mode = Mode::Detail;
        let text = render_test(100, 28, &snap, "127.0.0.1:18765", "0.0.12", &app);
        assert!(
            text.contains("Session sess-abc"),
            "detail missing session header:\n{text}"
        );
        assert!(
            text.contains("last_failure:")
                || text.contains("UpstreamHttp")
                || text.contains("ProxyAssemble"),
            "detail missing last failure kind:\n{text}"
        );
        assert!(
            text.contains("requests:"),
            "detail missing requests field:\n{text}"
        );
    }

    #[test]
    fn help_overlay_renders() {
        let snap = Snapshot::default();
        let mut app = App::new();
        app.mode = Mode::Help;
        let text = render_test(80, 24, &snap, "127.0.0.1:1", "0.0.12", &app);
        assert!(text.contains("Monitor help"), "help text missing:\n{text}");
        assert!(
            text.contains("Shift-Tab") || text.contains("Tab"),
            "help missing panel switch keys:\n{text}"
        );
        assert!(
            text.contains("filter") || text.contains(" f "),
            "help missing filter key:\n{text}"
        );
        assert!(
            text.contains("copy") || text.contains(" y "),
            "help missing copy report key:\n{text}"
        );
        assert!(
            text.contains("write") || text.contains(" w "),
            "help missing write report key:\n{text}"
        );
    }

    #[test]
    fn estimated_retry_group_label_in_panel() {
        use chrono::{Duration, TimeZone, Utc};
        let t0 = Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap();
        // Newest-first ring order.
        let snap = Snapshot {
            failures: vec![
                FailureRecord {
                    ts: t0 + Duration::seconds(10),
                    request_id: "r2".into(),
                    session_id: "sess-retry".into(),
                    requested_model: "a".into(),
                    model: "m".into(),
                    status_code: 200,
                    duration_ms: 100,
                    kind: FailureKind::ProxyAssemble,
                    error_type: "proxy_incomplete_output".into(),
                    error_message: "x".into(),
                    response_id: String::new(),
                    mapped: true,
                    lite: false,
                    fast: false,
                    auth_retried: false,
                    attempt: 1,
                    output_count: 0,
                    capture_bytes: 0,
                    session_failure_index: 2,
                },
                FailureRecord {
                    ts: t0,
                    request_id: "r1".into(),
                    session_id: "sess-retry".into(),
                    requested_model: "a".into(),
                    model: "m".into(),
                    status_code: 200,
                    duration_ms: 100,
                    kind: FailureKind::ProxyAssemble,
                    error_type: "proxy_incomplete_output".into(),
                    error_message: "x".into(),
                    response_id: String::new(),
                    mapped: true,
                    lite: false,
                    fast: false,
                    auth_retried: false,
                    attempt: 1,
                    output_count: 0,
                    capture_bytes: 0,
                    session_failure_index: 1,
                },
            ],
            ..Default::default()
        };
        let app = App::new();
        let text = render_test(120, 28, &snap, "127.0.0.1:18765", "0.0.12", &app);
        assert!(
            text.contains("estimated"),
            "retry group should show estimated label:\n{text}"
        );
        assert!(
            text.contains("×2") || text.contains("x2") || text.contains("2 estimated"),
            "retry group size missing:\n{text}"
        );
    }

    #[test]
    fn try_export_keys_and_empty() {
        use crossterm::event::KeyCode;
        let snap = fixture_dashboard().snapshot();
        let app = App::new();
        let dir = tempfile::tempdir().unwrap();
        let out = try_export_to(
            KeyCode::Char('w'),
            &snap,
            &app,
            "127.0.0.1:1",
            "0.0.12",
            Some(dir.path()),
        )
        .expect("w is export");
        match out {
            report::ExportOutcome::Written { count, json, .. } => {
                assert_eq!(count, 2);
                assert!(!json);
            }
            report::ExportOutcome::Copied { .. } => panic!("w should write"),
            report::ExportOutcome::Empty => panic!("expected failures"),
            report::ExportOutcome::Error(e) => panic!("write failed: {e}"),
        }
        assert!(try_export(KeyCode::Char('j'), &snap, &app, "a", "v").is_none());

        let empty = Snapshot::default();
        let empty_out =
            try_export(KeyCode::Char('y'), &empty, &app, "a", "v").expect("y is export");
        assert!(matches!(empty_out, report::ExportOutcome::Empty));
    }

    #[test]
    fn try_export_respects_failure_filter() {
        use crossterm::event::KeyCode;
        use std::fs;
        let snap = fixture_dashboard().snapshot();
        // Fixture: req-3 ProxyAssemble + req-2 UpstreamHttp.
        let mut app = App::new();
        app.failure_filter = FailureFilter::ProxyAssemble;
        let dir = tempfile::tempdir().unwrap();
        let out = try_export_to(
            KeyCode::Char('w'),
            &snap,
            &app,
            "127.0.0.1:9",
            "0.0.12",
            Some(dir.path()),
        )
        .expect("w is export");
        match out {
            report::ExportOutcome::Written { count, path, .. } => {
                assert_eq!(count, 1, "only ProxyAssemble should export");
                let body = fs::read_to_string(&path).expect("read report");
                assert!(
                    body.contains("filter: ProxyAssemble"),
                    "meta filter label missing:\n{body}"
                );
                assert!(
                    body.contains("ProxyAssemble") || body.contains("proxy_incomplete"),
                    "ProxyAssemble content missing:\n{body}"
                );
                assert!(
                    !body.contains("UpstreamHttp"),
                    "filtered-out kind leaked into report:\n{body}"
                );
                assert!(
                    body.contains("| ProxyAssemble | 1 |"),
                    "summary should only count filtered kind:\n{body}"
                );
            }
            other => panic!("expected write, got {other:?}"),
        }
    }

    #[test]
    fn try_export_ignored_when_overlay_open() {
        use crossterm::event::KeyCode;
        let snap = fixture_dashboard().snapshot();
        let mut app = App::new();
        app.mode = Mode::Help;
        assert!(
            try_export(KeyCode::Char('w'), &snap, &app, "a", "v").is_none(),
            "export should no-op in Help"
        );
        app.mode = Mode::Detail;
        assert!(
            try_export(KeyCode::Char('y'), &snap, &app, "a", "v").is_none(),
            "export should no-op in Detail"
        );
        app.mode = Mode::Dashboard;
        let dir = tempfile::tempdir().unwrap();
        assert!(
            try_export_to(KeyCode::Char('w'), &snap, &app, "a", "v", Some(dir.path())).is_some(),
            "export should work on Dashboard"
        );
    }
}
