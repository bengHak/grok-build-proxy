//! Interactive serve monitor (plain crossterm UI; ratatui lands in a later PR).

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::{
    io::{self, IsTerminal, Write},
    sync::Arc,
    time::Duration,
};

pub use crate::store::{Dashboard, FailureRecord, Request, Session, Snapshot};

pub fn is_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
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
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                let count = snapshot.sessions.len()
                    + snapshot.active.len()
                    + snapshot.recent.len()
                    + snapshot.errors.len();
                if view.handle(key, count) {
                    return Ok(());
                }
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
        "Sessions: {}  Active: {}  Recent: {}  Errors: {}  Failures: {}\r",
        snapshot.sessions.len(),
        snapshot.active.len(),
        snapshot.recent.len(),
        snapshot.errors.len(),
        snapshot.failures.len()
    )?;
    let mut rows = Vec::new();
    for s in &snapshot.sessions {
        let err_tag = s
            .last_failure_kind
            .map(|k| format!(" last={k}"))
            .unwrap_or_default();
        rows.push(format!(
            "session {:<24} {:<18} requests {:>3} err {:>3} {:>6.1} tok/s{err_tag}",
            s.id,
            s.last_model,
            s.requests,
            s.errors,
            s.tokens_per_second()
        ));
    }
    for r in &snapshot.active {
        rows.push(format!(
            "active  {:<24} {:<18} {:>6.1}s a{}",
            r.id,
            r.model,
            r.duration().as_secs_f64(),
            r.attempt
        ));
    }
    for r in &snapshot.recent {
        let et = if r.error_type.is_empty() {
            String::new()
        } else {
            format!(" {}", r.error_type)
        };
        rows.push(format!(
            "recent  {:<24} {:<18} HTTP {}{et}",
            r.id, r.model, r.status
        ));
    }
    for r in &snapshot.errors {
        let label = if !r.error_type.is_empty() {
            r.error_type.as_str()
        } else if let Some(k) = r.failure_kind {
            k.as_str()
        } else {
            r.error.as_str()
        };
        rows.push(format!("error   {:<24} {}", r.id, label));
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
