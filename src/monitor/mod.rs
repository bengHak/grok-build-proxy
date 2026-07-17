//! Interactive serve monitor (ratatui panels: header / sessions / active / failures / footer).

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
    sync::Arc,
    time::Duration,
};
use theme::Theme;
use widgets::{ActivePanel, FailuresPanel, Footer, Header, HelpOverlay, SessionsPanel, TurnKind};

pub use crate::store::{Dashboard, FailureRecord, Request, Session, Snapshot};

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
        let sessions_len = snapshot.sessions.len();
        let active_len = ActivePanel::row_count(&snapshot);
        let failures_len = FailuresPanel::row_count(&snapshot, app.failure_filter);
        app.clamp_selection(sessions_len, active_len, failures_len);

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
                if app.handle(key, sessions_len, active_len, failures_len) {
                    return Ok(());
                }
                // Filter cycle (`f`) may shrink the list; re-clamp with new filter.
                let failures_len = FailuresPanel::row_count(&snapshot, app.failure_filter);
                app.clamp_selection(sessions_len, active_len, failures_len);
            }
        }
        tokio::task::yield_now().await;
    }
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

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(5),    // body (sessions|active + failures)
            Constraint::Length(3), // footer
        ])
        .split(area);

    Header {
        snapshot,
        address,
        version,
        uptime_secs: app.uptime_secs(),
        theme,
    }
    .render(chunks[0], buf);

    // Body: top row sessions|active, bottom full-width failures.
    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(chunks[1]);

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

    ActivePanel {
        snapshot,
        selected: app.selected,
        focused: app.focus == Focus::Active && app.mode == Mode::Dashboard,
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

    Footer { theme }.render(chunks[2], buf);

    match app.mode {
        Mode::Help => HelpOverlay { theme }.render(area, buf),
        Mode::Detail => draw_detail(area, buf, snapshot, app, theme),
        Mode::Dashboard => {}
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
    let height = area
        .height
        .min(if app.focus == Focus::Failures { 18 } else { 12 });
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
            if let Some(s) = snapshot.sessions.get(app.selected) {
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
        Focus::Active => {
            let rows = ActivePanel::rows(snapshot);
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
        Focus::Failures => {
            let rows = FailuresPanel::filtered(snapshot, app.failure_filter);
            if let Some(f) = rows.get(app.selected) {
                format!(
                    "Failure {}\n  ts: {}\n  kind: {}\n  session: {}\n  model: {} (requested {})\n  status: {}  attempt: {}\n  duration_ms: {}  session_fail#: {}\n  error_type: {}\n  message: {}\n  response_id: {}\n  mapped: {}  lite: {}  fast: {}\n  auth_retried: {}  outputs: {}  capture_bytes: {}",
                    f.request_id,
                    f.ts.to_rfc3339(),
                    f.kind.as_str(),
                    f.session_id,
                    f.model,
                    f.requested_model,
                    f.status_code,
                    f.attempt,
                    f.duration_ms,
                    f.session_failure_index,
                    if f.error_type.is_empty() {
                        "-"
                    } else {
                        f.error_type.as_str()
                    },
                    if f.error_message.is_empty() {
                        "-"
                    } else {
                        f.error_message.as_str()
                    },
                    if f.response_id.is_empty() {
                        "-"
                    } else {
                        f.response_id.as_str()
                    },
                    f.mapped,
                    f.lite,
                    f.fast,
                    f.auth_retried,
                    f.output_count,
                    f.capture_bytes,
                )
            } else {
                "No failure selected".into()
            }
        }
    }
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
        let app = App::new();
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
            text.contains("active / recent"),
            "active panel title missing:\n{text}"
        );
        assert!(
            text.contains("failures"),
            "failures panel title missing:\n{text}"
        );
        assert!(
            text.contains("All") || text.contains("[All]"),
            "failures filter label missing:\n{text}"
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
    }

    #[test]
    fn failures_filter_hides_non_matching() {
        let snap = fixture_dashboard().snapshot();
        let mut app = App::new();
        app.failure_filter = FailureFilter::ProxyAssemble;
        let text = render_test(100, 28, &snap, "127.0.0.1:18765", "0.0.12", &app);
        assert!(
            text.contains("ProxyAssemble") || text.contains("proxy_incomplete"),
            "filtered ProxyAssemble missing:\n{text}"
        );
        // Title should show filter name.
        assert!(
            text.contains("ProxyAssemble"),
            "filter name in title missing:\n{text}"
        );
        // UpstreamHttp row should not appear when filter is ProxyAssemble.
        // Kind label in the list uses as_str(); filter title also contains ProxyAssemble.
        let without_title = text.replacen("ProxyAssemble", "", 1);
        assert!(
            !without_title.contains("UpstreamHttp"),
            "UpstreamHttp should be filtered out:\n{text}"
        );
    }

    #[test]
    fn detail_overlay_renders_failure_fields() {
        let snap = fixture_dashboard().snapshot();
        let mut app = App::new();
        app.focus = Focus::Failures;
        app.selected = 0;
        app.mode = Mode::Detail;
        let text = render_test(100, 28, &snap, "127.0.0.1:18765", "0.0.12", &app);
        assert!(text.contains("detail"), "detail title missing:\n{text}");
        assert!(
            text.contains("Failure"),
            "detail missing failure header:\n{text}"
        );
        assert!(
            text.contains("kind:")
                || text.contains("UpstreamHttp")
                || text.contains("ProxyAssemble"),
            "detail missing kind:\n{text}"
        );
        assert!(
            text.contains("sess-abc"),
            "detail missing session id:\n{text}"
        );
        assert!(
            text.contains("attempt:") || text.contains("attempt"),
            "detail missing attempt:\n{text}"
        );
        assert!(
            text.contains("error_type")
                || text.contains("upstream_http")
                || text.contains("proxy_incomplete"),
            "detail missing error_type:\n{text}"
        );
    }

    #[test]
    fn detail_overlay_renders_turn_fields() {
        let snap = fixture_dashboard().snapshot();
        let mut app = App::new();
        app.focus = Focus::Active;
        app.selected = 0; // req-live (active first)
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
        let text = render_test(80, 22, &snap, "127.0.0.1:1", "0.0.12", &app);
        assert!(text.contains("Monitor help"), "help text missing:\n{text}");
        assert!(
            text.contains("Shift-Tab") || text.contains("Tab"),
            "help missing panel switch keys:\n{text}"
        );
        assert!(
            text.contains("filter") || text.contains(" f "),
            "help missing filter key:\n{text}"
        );
    }
}
