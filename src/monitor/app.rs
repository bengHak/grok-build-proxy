//! View state: mode, panel focus, selection, failure filter, session pin.

use crate::events::FailureKind;
use crate::store::Session;
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
    SessionDetail,
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

/// Footer toast lifetime for export feedback.
const TOAST_SECS: u64 = 5;

#[derive(Clone, Debug, Default)]
pub struct App {
    pub mode: Mode,
    pub focus: Focus,
    pub selected: usize,
    pub failure_filter: FailureFilter,
    /// Pinned session for the session-detail panel (stable across list churn).
    pub selected_session_id: Option<String>,
    /// Stable identity for Failure detail overlay (avoids index shift on push_front).
    pub detail_request_id: Option<String>,
    /// Wall-clock start of the monitor loop (for uptime).
    pub started_at: Option<std::time::Instant>,
    /// Last time a fleet tok/s sample was pushed (1 Hz).
    pub last_tok_sample_at: Option<std::time::Instant>,
    /// Transient footer status (export path / clipboard result).
    pub toast: Option<String>,
    toast_set_at: Option<std::time::Instant>,
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

    /// Set a footer toast (auto-clears after a few seconds).
    pub fn set_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some(msg.into());
        self.toast_set_at = Some(std::time::Instant::now());
    }

    /// Active toast text, or `None` if expired / unset.
    pub fn toast_message(&self) -> Option<&str> {
        let set_at = self.toast_set_at?;
        if set_at.elapsed().as_secs() >= TOAST_SECS {
            return None;
        }
        self.toast.as_deref()
    }

    /// Drop expired toast so it does not linger in state forever.
    pub fn tick_toast(&mut self) {
        if self.toast.is_some()
            && self
                .toast_set_at
                .is_some_and(|t| t.elapsed().as_secs() >= TOAST_SECS)
        {
            self.toast = None;
            self.toast_set_at = None;
        }
    }

    /// Keep `selected_session_id` aligned with the active-session list.
    ///
    /// When focus is Sessions, pin follows `selected`. Otherwise, if the pin
    /// left the active set, re-pin to the clamped index or clear.
    pub fn sync_selected_session(&mut self, active: &[&Session]) {
        if active.is_empty() {
            self.selected_session_id = None;
            if self.focus == Focus::Sessions {
                self.selected = 0;
            }
            return;
        }
        match self.focus {
            Focus::Sessions => {
                if self.selected >= active.len() {
                    self.selected = active.len() - 1;
                }
                self.selected_session_id = Some(active[self.selected].id.clone());
            }
            Focus::SessionDetail | Focus::Failures => {
                let still = self
                    .selected_session_id
                    .as_ref()
                    .is_some_and(|id| active.iter().any(|s| s.id == *id));
                if !still {
                    let idx = self.selected.min(active.len() - 1);
                    self.selected_session_id = Some(active[idx].id.clone());
                }
            }
        }
    }

    /// Pin session id from the active list at `selected` (Sessions navigation).
    pub fn pin_session_from_selection(&mut self, active: &[&Session]) {
        if active.is_empty() {
            self.selected_session_id = None;
            return;
        }
        let idx = self.selected.min(active.len() - 1);
        self.selected_session_id = Some(active[idx].id.clone());
    }

    /// Restore Sessions `selected` to the pinned session when tabbing back.
    pub fn restore_session_selection(&mut self, active: &[&Session]) {
        if let Some(id) = &self.selected_session_id
            && let Some(i) = active.iter().position(|s| s.id == *id)
        {
            self.selected = i;
            return;
        }
        self.selected = 0;
        self.pin_session_from_selection(active);
    }

    /// True when at least one second has passed since the last tok/s sample.
    pub fn should_sample_tok_s(&self) -> bool {
        match self.last_tok_sample_at {
            None => true,
            Some(t) => t.elapsed().as_secs() >= 1,
        }
    }

    pub fn mark_tok_sampled(&mut self) {
        self.last_tok_sample_at = Some(std::time::Instant::now());
    }

    /// Clamp selection into the focused panel's row count.
    pub fn clamp_selection(&mut self, sessions_len: usize, detail_len: usize, failures_len: usize) {
        let count = match self.focus {
            Focus::Sessions => sessions_len,
            Focus::SessionDetail => detail_len,
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
        detail_len: usize,
        failures_len: usize,
    ) -> usize {
        match focus {
            Focus::Sessions => sessions_len,
            Focus::SessionDetail => detail_len,
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
    ///
    /// After navigation that changes Sessions selection, the caller should call
    /// [`Self::pin_session_from_selection`] with the current active-session list.
    pub fn handle(
        &mut self,
        key: KeyEvent,
        sessions_len: usize,
        detail_len: usize,
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
                        Self::focus_count(self.focus, sessions_len, detail_len, failures_len);
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
                        Focus::Sessions => Focus::SessionDetail,
                        Focus::SessionDetail => Focus::Failures,
                        Focus::Failures => Focus::Sessions,
                    };
                    self.selected = 0;
                }
            }
            KeyCode::BackTab => {
                if self.mode == Mode::Dashboard {
                    self.focus = match self.focus {
                        Focus::Sessions => Focus::Failures,
                        Focus::SessionDetail => Focus::Sessions,
                        Focus::Failures => Focus::SessionDetail,
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
                let count = Self::focus_count(self.focus, sessions_len, detail_len, failures_len);
                if self.selected + 1 < count {
                    self.selected += 1;
                }
            }
            _ => {}
        }
        self.clamp_selection(sessions_len, detail_len, failures_len);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::FailureKind;
    use crate::store::Session;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn sess(id: &str, active: u64) -> Session {
        Session {
            id: id.into(),
            active,
            ..Default::default()
        }
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
        assert_eq!(app.focus, Focus::SessionDetail);
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
        assert_eq!(app.focus, Focus::SessionDetail);
        app.handle(key(KeyCode::BackTab), 3, 5, 4);
        assert_eq!(app.focus, Focus::Sessions);
    }

    #[test]
    fn sync_pin_follows_sessions_selection() {
        let a = sess("a", 1);
        let b = sess("b", 1);
        let list = vec![&a, &b];
        let mut app = App::new();
        app.focus = Focus::Sessions;
        app.selected = 1;
        app.sync_selected_session(&list);
        assert_eq!(app.selected_session_id.as_deref(), Some("b"));
    }

    #[test]
    fn sync_clears_when_no_active_sessions() {
        let mut app = App::new();
        app.selected_session_id = Some("gone".into());
        app.sync_selected_session(&[]);
        assert!(app.selected_session_id.is_none());
    }

    #[test]
    fn sync_repins_when_pin_leaves_active_set() {
        let a = sess("a", 1);
        let list = vec![&a];
        let mut app = App::new();
        app.focus = Focus::SessionDetail;
        app.selected_session_id = Some("gone".into());
        app.sync_selected_session(&list);
        assert_eq!(app.selected_session_id.as_deref(), Some("a"));
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

    #[test]
    fn toast_set_and_read() {
        let mut app = App::new();
        assert!(app.toast_message().is_none());
        app.set_toast("copied 1 failure");
        assert_eq!(app.toast_message(), Some("copied 1 failure"));
        app.tick_toast();
        assert_eq!(app.toast_message(), Some("copied 1 failure"));
    }

    #[test]
    fn should_sample_tok_s_initially() {
        let app = App::new();
        assert!(app.should_sample_tok_s());
    }
}
