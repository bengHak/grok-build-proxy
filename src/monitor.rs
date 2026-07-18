use crate::proxy::{Observer, RequestEvent, RequestEventKind};
use chrono::{DateTime, Utc};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{self, IsTerminal, Write},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct Request {
    pub id: String,
    pub session_id: String,
    pub requested_model: String,
    pub model: String,
    pub status: u16,
    pub error: String,
    pub output_tokens: u64,
    pub started_at: Instant,
    pub ended_at: Option<Instant>,
}
impl Request {
    pub fn duration(&self) -> Duration {
        self.ended_at
            .unwrap_or_else(Instant::now)
            .saturating_duration_since(self.started_at)
    }
    pub fn tokens_per_second(&self) -> f64 {
        let seconds = self.duration().as_secs_f64();
        if seconds > 0.0 {
            self.output_tokens as f64 / seconds
        } else {
            0.0
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct Session {
    pub id: String,
    pub requests: u64,
    pub active: u64,
    pub output_tokens: u64,
    pub last_model: String,
    pub errors: u64,
    pub updated_at: Option<DateTime<Utc>>,
    sample_seconds: f64,
}
impl Session {
    pub fn tokens_per_second(&self) -> f64 {
        if self.sample_seconds > 0.0 {
            self.output_tokens as f64 / self.sample_seconds
        } else {
            0.0
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub sessions: Vec<Session>,
    pub active: Vec<Request>,
    pub recent: Vec<Request>,
    pub errors: Vec<Request>,
}
#[derive(Default)]
struct State {
    sessions: HashMap<String, Session>,
    active: HashMap<String, Request>,
    recent: VecDeque<Request>,
    errors: VecDeque<Request>,
    completed: HashSet<String>,
}
#[derive(Clone, Default)]
pub struct Dashboard {
    inner: Arc<Mutex<State>>,
}
impl Dashboard {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn snapshot(&self) -> Snapshot {
        let state = self.inner.lock().unwrap();
        let mut sessions: Vec<_> = state.sessions.values().cloned().collect();
        sessions.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        let mut active: Vec<_> = state.active.values().cloned().collect();
        active.sort_by_key(|r| r.started_at);
        Snapshot {
            sessions,
            active,
            recent: state.recent.iter().cloned().collect(),
            errors: state.errors.iter().cloned().collect(),
        }
    }
    fn apply(&self, event: RequestEvent) {
        let mut state = self.inner.lock().unwrap();
        let request_id = sanitize(&event.request_id);
        let session_id = sanitize(&event.session_id);
        match event.kind {
            RequestEventKind::Started => {
                if state.active.contains_key(&event.request_id)
                    || state.completed.contains(&event.request_id)
                {
                    return;
                }
                state.active.insert(
                    event.request_id.clone(),
                    Request {
                        id: request_id,
                        session_id: session_id.clone(),
                        requested_model: sanitize(&event.requested_model),
                        model: sanitize(&event.model),
                        status: 0,
                        error: String::new(),
                        output_tokens: 0,
                        started_at: event.started_at,
                        ended_at: None,
                    },
                );
                let session = state
                    .sessions
                    .entry(event.session_id)
                    .or_insert_with(|| Session {
                        id: session_id,
                        ..Default::default()
                    });
                session.requests += 1;
                session.active += 1;
                session.last_model = sanitize(&event.model);
                session.updated_at = Some(Utc::now());
            }
            RequestEventKind::Completed | RequestEventKind::Failed => {
                if !state.completed.insert(event.request_id.clone()) {
                    return;
                }
                let mut request = state.active.remove(&event.request_id).unwrap_or(Request {
                    id: request_id,
                    session_id: session_id.clone(),
                    requested_model: sanitize(&event.requested_model),
                    model: sanitize(&event.model),
                    status: 0,
                    error: String::new(),
                    output_tokens: 0,
                    started_at: event.started_at,
                    ended_at: None,
                });
                request.status = event.status_code;
                request.error = sanitize(&event.error);
                request.output_tokens = event.output_tokens;
                request.ended_at = Some(Instant::now());
                let duration = request.duration().as_secs_f64();
                let failed = event.kind == RequestEventKind::Failed;
                state.recent.push_front(request.clone());
                state.recent.truncate(50);
                if failed {
                    state.errors.push_front(request.clone());
                    state.errors.truncate(50);
                }
                let session = state
                    .sessions
                    .entry(event.session_id)
                    .or_insert_with(|| Session {
                        id: session_id,
                        ..Default::default()
                    });
                session.active = session.active.saturating_sub(1);
                session.output_tokens += event.output_tokens;
                if event.output_tokens > 0 {
                    session.sample_seconds += duration
                }
                if failed {
                    session.errors += 1
                }
                session.updated_at = Some(Utc::now());
                if state.completed.len() > 200 {
                    state.completed.clear();
                }
            }
        }
    }
}
impl Observer for Dashboard {
    fn observe(&self, event: RequestEvent) {
        self.apply(event)
    }
}
pub fn is_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}
fn sanitize(value: &str) -> String {
    let mut out: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(256)
        .collect();
    out = out.trim().to_owned();
    if value.chars().count() > 256 {
        out.pop();
        out.push('…')
    }
    out
}

struct TerminalGuard;
impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
        Ok(Self)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), cursor::Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Dashboard,
    Help,
    Detail,
}
#[derive(Default)]
struct View {
    mode: Mode,
    selected: usize,
}
impl View {
    fn handle(&mut self, key: crossterm::event::KeyEvent, count: usize) -> bool {
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < count {
                    self.selected += 1
                }
            }
            KeyCode::Char('?') => {
                self.mode = if self.mode == Mode::Help {
                    Mode::Dashboard
                } else {
                    Mode::Help
                }
            }
            KeyCode::Enter if count > 0 => self.mode = Mode::Detail,
            KeyCode::Esc | KeyCode::Backspace => self.mode = Mode::Dashboard,
            _ => {}
        }
        false
    }
}
pub async fn run(dashboard: Arc<Dashboard>, address: &str, version: &str) -> io::Result<()> {
    let _guard = TerminalGuard::enter()?;
    let mut view = View::default();
    loop {
        let snapshot = dashboard.snapshot();
        render(&snapshot, address, version, &view)?;
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
        {
            let count = snapshot.sessions.len()
                + snapshot.active.len()
                + snapshot.recent.len()
                + snapshot.errors.len();
            if view.handle(key, count) {
                return Ok(());
            }
        }
        tokio::task::yield_now().await;
    }
}
fn render(snapshot: &Snapshot, address: &str, version: &str, view: &View) -> io::Result<()> {
    let mut out = io::stdout();
    execute!(
        out,
        cursor::MoveTo(0, 0),
        terminal::Clear(terminal::ClearType::All)
    )?;
    writeln!(out, "grok-build-proxy {version}  {address}\r")?;
    if view.mode == Mode::Help {
        writeln!(
            out,
            "Monitor help\r\n  ↑/k and ↓/j  move selection\r\n  Enter         open details\r\n  Esc/Backspace return\r\n  ?             toggle help\r\n  q/Ctrl-C      stop proxy\r"
        )?;
        return out.flush();
    }
    writeln!(
        out,
        "Sessions: {}  Active: {}  Recent: {}  Errors: {}\r",
        snapshot.sessions.len(),
        snapshot.active.len(),
        snapshot.recent.len(),
        snapshot.errors.len()
    )?;
    let mut rows = Vec::new();
    for s in &snapshot.sessions {
        rows.push(format!(
            "session {:<24} {:<18} requests {:>3} {:>6.1} tok/s",
            s.id,
            s.last_model,
            s.requests,
            s.tokens_per_second()
        ));
    }
    for r in &snapshot.active {
        rows.push(format!(
            "active  {:<24} {:<18} {:>6.1}s",
            r.id,
            r.model,
            r.duration().as_secs_f64()
        ));
    }
    for r in &snapshot.recent {
        rows.push(format!(
            "recent  {:<24} {:<18} HTTP {}",
            r.id, r.model, r.status
        ));
    }
    for r in &snapshot.errors {
        rows.push(format!("error   {:<24} {}", r.id, r.error));
    }
    if view.mode == Mode::Detail {
        if let Some(row) = rows.get(view.selected) {
            writeln!(out, "\rDetails\r\n  {row}\r")?
        }
    } else {
        for (index, row) in rows.iter().enumerate().take(12) {
            writeln!(
                out,
                "{} {}\r",
                if index == view.selected { ">" } else { " " },
                row
            )?;
        }
    }
    writeln!(out, "\r↑/k ↓/j navigate  Enter details  ? help  q quit\r")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(kind: RequestEventKind) -> RequestEvent {
        RequestEvent {
            kind,
            request_id: "req\n1".into(),
            session_id: "session".into(),
            requested_model: "alias".into(),
            model: "gpt".into(),
            status_code: 200,
            output_tokens: 20,
            error: String::new(),
            started_at: Instant::now() - Duration::from_secs(2),
        }
    }
    #[test]
    fn lifecycle_updates_bounded_state() {
        let d = Dashboard::new();
        d.observe(event(RequestEventKind::Started));
        assert_eq!(d.snapshot().active.len(), 1);
        d.observe(event(RequestEventKind::Completed));
        let s = d.snapshot();
        assert!(s.active.is_empty());
        assert_eq!(s.recent.len(), 1);
        assert_eq!(s.sessions[0].active, 0);
        assert_eq!(s.sessions[0].output_tokens, 20);
        assert!(!s.recent[0].id.contains('\n'));
    }
}
