//! View state: mode, panel focus, selection, failure filter.

use crate::events::FailureKind;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Dashboard,
    Help,
    Detail,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Sessions,
    Active,
    Failures,
}

/// Cycles with `f`: All → ProxyAssemble → Upstream → Auth → Stream → All.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FailureFilter {
    #[default]
    All,
    ProxyAssemble,
    /// UpstreamHttp + UpstreamConnect.
    Upstream,
    /// AuthRetryFailed.
    Auth,
    /// StreamIo + StreamTerminalFailed.
    Stream,
}

impl FailureFilter {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::ProxyAssemble,
            Self::ProxyAssemble => Self::Upstream,
            Self::Upstream => Self::Auth,
            Self::Auth => Self::Stream,
            Self::Stream => Self::All,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::ProxyAssemble => "ProxyAssemble",
            Self::Upstream => "Upstream",
            Self::Auth => "Auth",
            Self::Stream => "Stream",
        }
    }

    pub fn matches(self, kind: FailureKind) -> bool {
        match self {
            Self::All => true,
            Self::ProxyAssemble => kind == FailureKind::ProxyAssemble,
            Self::Upstream => matches!(
                kind,
                FailureKind::UpstreamHttp | FailureKind::UpstreamConnect
            ),
            Self::Auth => kind == FailureKind::AuthRetryFailed,
            Self::Stream => matches!(
                kind,
                FailureKind::StreamIo | FailureKind::StreamTerminalFailed
            ),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct App {
    pub mode: Mode,
    pub focus: Focus,
    pub selected: usize,
    pub failure_filter: FailureFilter,
    /// Stable identity for Failure detail overlay (avoids index shift on push_front).
    pub detail_request_id: Option<String>,
    /// Wall-clock start of the monitor loop (for uptime).
    pub started_at: Option<std::time::Instant>,
}

impl App {
    pub fn new() -> Self {
        Self {
            started_at: Some(std::time::Instant::now()),
            ..Default::default()
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.map(|t| t.elapsed().as_secs()).unwrap_or(0)
    }

    /// Clamp selection into the focused panel's row count.
    pub fn clamp_selection(&mut self, sessions_len: usize, active_len: usize, failures_len: usize) {
        let count = match self.focus {
            Focus::Sessions => sessions_len,
            Focus::Active => active_len,
            Focus::Failures => failures_len,
        };
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
    }

    fn focus_count(
        focus: Focus,
        sessions_len: usize,
        active_len: usize,
        failures_len: usize,
    ) -> usize {
        match focus {
            Focus::Sessions => sessions_len,
            Focus::Active => active_len,
            Focus::Failures => failures_len,
        }
    }

    /// Pin failure detail identity after Enter (call with the selected row's request_id).
    pub fn pin_failure_detail(&mut self, request_id: impl Into<String>) {
        self.detail_request_id = Some(request_id.into());
    }

    /// Handle a key. Returns `true` when the monitor should quit.
    ///
    /// On `f` with Failures focused, selection is reset to 0 and the in-handle clamp
    /// is skipped so callers must re-clamp with the **post-filter** `failures_len`
    /// (see `run` loop). Other keys clamp with the lengths passed here.
    pub fn handle(
        &mut self,
        key: KeyEvent,
        sessions_len: usize,
        active_len: usize,
        failures_len: usize,
    ) -> bool {
        match key.code {
            KeyCode::Char('q' | 'Q') => return true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Char('?') => {
                self.mode = if self.mode == Mode::Help {
                    Mode::Dashboard
                } else {
                    Mode::Help
                };
            }
            KeyCode::Esc | KeyCode::Backspace => {
                if self.mode != Mode::Dashboard {
                    self.mode = Mode::Dashboard;
                    self.detail_request_id = None;
                }
            }
            KeyCode::Enter => {
                if self.mode == Mode::Dashboard {
                    let count =
                        Self::focus_count(self.focus, sessions_len, active_len, failures_len);
                    if count > 0 {
                        self.mode = Mode::Detail;
                        // Failures: caller pins request_id via `pin_failure_detail`.
                        // Clear any stale pin from a previous overlay session.
                        self.detail_request_id = None;
                    }
                }
            }
            KeyCode::Tab => {
                if self.mode == Mode::Dashboard {
                    self.focus = match self.focus {
                        Focus::Sessions => Focus::Active,
                        Focus::Active => Focus::Failures,
                        Focus::Failures => Focus::Sessions,
                    };
                    self.selected = 0;
                }
            }
            KeyCode::BackTab => {
                if self.mode == Mode::Dashboard {
                    self.focus = match self.focus {
                        Focus::Sessions => Focus::Failures,
                        Focus::Active => Focus::Sessions,
                        Focus::Failures => Focus::Active,
                    };
                    self.selected = 0;
                }
            }
            KeyCode::Char('f' | 'F') if self.mode == Mode::Dashboard => {
                self.failure_filter = self.failure_filter.next();
                // Selection under Failures must not be clamped against the *pre-filter*
                // failures_len. Reset to 0 and return so the caller re-clamps with the
                // new filtered length.
                if self.focus == Focus::Failures {
                    self.selected = 0;
                    return false;
                }
            }
            KeyCode::Up | KeyCode::Char('k') if self.mode == Mode::Dashboard => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if self.mode == Mode::Dashboard => {
                let count = Self::focus_count(self.focus, sessions_len, active_len, failures_len);
                if self.selected + 1 < count {
                    self.selected += 1;
                }
            }
            _ => {}
        }
        self.clamp_selection(sessions_len, active_len, failures_len);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::FailureKind;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn j_k_navigate_within_focus() {
        let mut app = App::new();
        assert_eq!(app.focus, Focus::Sessions);
        app.handle(key(KeyCode::Char('j')), 3, 2, 0);
        assert_eq!(app.selected, 1);
        app.handle(key(KeyCode::Char('j')), 3, 2, 0);
        assert_eq!(app.selected, 2);
        app.handle(key(KeyCode::Char('j')), 3, 2, 0);
        assert_eq!(app.selected, 2);
        app.handle(key(KeyCode::Char('k')), 3, 2, 0);
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn tab_cycles_three_panels() {
        let mut app = App::new();
        app.selected = 2;
        app.handle(key(KeyCode::Tab), 3, 5, 4);
        assert_eq!(app.focus, Focus::Active);
        assert_eq!(app.selected, 0);
        app.handle(key(KeyCode::Tab), 3, 5, 4);
        assert_eq!(app.focus, Focus::Failures);
        assert_eq!(app.selected, 0);
        app.handle(key(KeyCode::Tab), 3, 5, 4);
        assert_eq!(app.focus, Focus::Sessions);
    }

    #[test]
    fn backtab_cycles_reverse() {
        let mut app = App::new();
        app.handle(key(KeyCode::BackTab), 3, 5, 4);
        assert_eq!(app.focus, Focus::Failures);
        app.handle(key(KeyCode::BackTab), 3, 5, 4);
        assert_eq!(app.focus, Focus::Active);
        app.handle(key(KeyCode::BackTab), 3, 5, 4);
        assert_eq!(app.focus, Focus::Sessions);
    }

    #[test]
    fn filter_cycles_and_matches() {
        let mut app = App::new();
        assert_eq!(app.failure_filter, FailureFilter::All);
        app.handle(key(KeyCode::Char('f')), 0, 0, 0);
        assert_eq!(app.failure_filter, FailureFilter::ProxyAssemble);
        app.handle(key(KeyCode::Char('f')), 0, 0, 0);
        assert_eq!(app.failure_filter, FailureFilter::Upstream);
        app.handle(key(KeyCode::Char('f')), 0, 0, 0);
        assert_eq!(app.failure_filter, FailureFilter::Auth);
        app.handle(key(KeyCode::Char('f')), 0, 0, 0);
        assert_eq!(app.failure_filter, FailureFilter::Stream);
        app.handle(key(KeyCode::Char('f')), 0, 0, 0);
        assert_eq!(app.failure_filter, FailureFilter::All);

        assert!(FailureFilter::Upstream.matches(FailureKind::UpstreamHttp));
        assert!(FailureFilter::Upstream.matches(FailureKind::UpstreamConnect));
        assert!(!FailureFilter::Upstream.matches(FailureKind::ProxyAssemble));
        assert!(FailureFilter::Stream.matches(FailureKind::StreamIo));
        assert!(FailureFilter::Stream.matches(FailureKind::StreamTerminalFailed));
        assert!(FailureFilter::Auth.matches(FailureKind::AuthRetryFailed));
        assert!(!FailureFilter::Auth.matches(FailureKind::UpstreamHttp));
        // ClientRejected / Unknown only under All.
        assert!(FailureFilter::All.matches(FailureKind::ClientRejected));
        assert!(FailureFilter::All.matches(FailureKind::Unknown));
        assert!(!FailureFilter::ProxyAssemble.matches(FailureKind::ClientRejected));
        assert!(!FailureFilter::Upstream.matches(FailureKind::Unknown));
        assert!(!FailureFilter::Auth.matches(FailureKind::ClientRejected));
        assert!(!FailureFilter::Stream.matches(FailureKind::Unknown));
    }

    #[test]
    fn filter_resets_selection_when_failures_focused() {
        let mut app = App::new();
        app.focus = Focus::Failures;
        app.selected = 3;
        app.handle(key(KeyCode::Char('f')), 0, 0, 5);
        assert_eq!(app.selected, 0);
        assert_eq!(app.failure_filter, FailureFilter::ProxyAssemble);
    }

    #[test]
    fn empty_failures_clamps_and_blocks_detail() {
        let mut app = App::new();
        app.focus = Focus::Failures;
        app.selected = 2;
        app.clamp_selection(0, 0, 0);
        assert_eq!(app.selected, 0);
        app.handle(key(KeyCode::Enter), 0, 0, 0);
        assert_eq!(app.mode, Mode::Dashboard);
        assert!(app.detail_request_id.is_none());
    }

    #[test]
    fn quit_keys() {
        let mut app = App::new();
        assert!(app.handle(key(KeyCode::Char('q')), 0, 0, 0));
        let mut app = App::new();
        assert!(app.handle(key(KeyCode::Char('Q')), 0, 0, 0));
        let mut app = App::new();
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(app.handle(ctrl_c, 0, 0, 0));
    }

    #[test]
    fn help_and_detail_toggle() {
        let mut app = App::new();
        app.handle(key(KeyCode::Char('?')), 1, 0, 0);
        assert_eq!(app.mode, Mode::Help);
        app.handle(key(KeyCode::Char('?')), 1, 0, 0);
        assert_eq!(app.mode, Mode::Dashboard);
        app.handle(key(KeyCode::Enter), 1, 0, 0);
        assert_eq!(app.mode, Mode::Detail);
        app.handle(key(KeyCode::Esc), 1, 0, 0);
        assert_eq!(app.mode, Mode::Dashboard);
        assert!(app.detail_request_id.is_none());
    }

    #[test]
    fn enter_opens_detail_on_failures() {
        let mut app = App::new();
        app.focus = Focus::Failures;
        app.handle(key(KeyCode::Enter), 0, 0, 2);
        assert_eq!(app.mode, Mode::Detail);
        app.pin_failure_detail("req-x");
        assert_eq!(app.detail_request_id.as_deref(), Some("req-x"));
        app.handle(key(KeyCode::Esc), 0, 0, 2);
        assert!(app.detail_request_id.is_none());

        let mut app = App::new();
        app.focus = Focus::Failures;
        app.handle(key(KeyCode::Enter), 0, 0, 0);
        assert_eq!(app.mode, Mode::Dashboard);
    }
}
