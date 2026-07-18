//! View state: mode, panel focus, selection.

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
}

#[derive(Clone, Debug, Default)]
pub struct App {
    pub mode: Mode,
    pub focus: Focus,
    pub selected: usize,
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
    pub fn clamp_selection(&mut self, sessions_len: usize, active_len: usize) {
        let count = match self.focus {
            Focus::Sessions => sessions_len,
            Focus::Active => active_len,
        };
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
    }

    /// Handle a key. Returns `true` when the monitor should quit.
    pub fn handle(&mut self, key: KeyEvent, sessions_len: usize, active_len: usize) -> bool {
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
                }
            }
            KeyCode::Enter => {
                if self.mode == Mode::Dashboard {
                    let count = match self.focus {
                        Focus::Sessions => sessions_len,
                        Focus::Active => active_len,
                    };
                    if count > 0 {
                        self.mode = Mode::Detail;
                    }
                }
            }
            KeyCode::Tab => {
                if self.mode == Mode::Dashboard {
                    self.focus = match self.focus {
                        Focus::Sessions => Focus::Active,
                        Focus::Active => Focus::Sessions,
                    };
                    self.selected = 0;
                }
            }
            KeyCode::BackTab => {
                if self.mode == Mode::Dashboard {
                    self.focus = match self.focus {
                        Focus::Sessions => Focus::Active,
                        Focus::Active => Focus::Sessions,
                    };
                    self.selected = 0;
                }
            }
            KeyCode::Up | KeyCode::Char('k') if self.mode == Mode::Dashboard => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if self.mode == Mode::Dashboard => {
                let count = match self.focus {
                    Focus::Sessions => sessions_len,
                    Focus::Active => active_len,
                };
                if self.selected + 1 < count {
                    self.selected += 1;
                }
            }
            _ => {}
        }
        self.clamp_selection(sessions_len, active_len);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn j_k_navigate_within_focus() {
        let mut app = App::new();
        assert_eq!(app.focus, Focus::Sessions);
        app.handle(key(KeyCode::Char('j')), 3, 2);
        assert_eq!(app.selected, 1);
        app.handle(key(KeyCode::Char('j')), 3, 2);
        assert_eq!(app.selected, 2);
        app.handle(key(KeyCode::Char('j')), 3, 2);
        assert_eq!(app.selected, 2);
        app.handle(key(KeyCode::Char('k')), 3, 2);
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn tab_switches_focus_and_resets_selection() {
        let mut app = App::new();
        app.selected = 2;
        app.handle(key(KeyCode::Tab), 3, 5);
        assert_eq!(app.focus, Focus::Active);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn quit_keys() {
        let mut app = App::new();
        assert!(app.handle(key(KeyCode::Char('q')), 0, 0));
        let mut app = App::new();
        assert!(app.handle(key(KeyCode::Char('Q')), 0, 0));
        let mut app = App::new();
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(app.handle(ctrl_c, 0, 0));
    }

    #[test]
    fn help_and_detail_toggle() {
        let mut app = App::new();
        app.handle(key(KeyCode::Char('?')), 1, 0);
        assert_eq!(app.mode, Mode::Help);
        app.handle(key(KeyCode::Char('?')), 1, 0);
        assert_eq!(app.mode, Mode::Dashboard);
        app.handle(key(KeyCode::Enter), 1, 0);
        assert_eq!(app.mode, Mode::Detail);
        app.handle(key(KeyCode::Esc), 1, 0);
        assert_eq!(app.mode, Mode::Dashboard);
    }
}
