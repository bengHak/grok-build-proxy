//! Left panel: active sessions only (id, model, requests, errors, tok/s).

use super::truncate;
use crate::monitor::theme::Theme;
use crate::store::{Session, Snapshot};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, StatefulWidget, Widget},
};

/// Sessions with at least one in-flight request (`active > 0`), store order preserved.
pub fn active_sessions(snapshot: &Snapshot) -> Vec<&Session> {
    snapshot.sessions.iter().filter(|s| s.active > 0).collect()
}

pub struct SessionsPanel<'a> {
    pub snapshot: &'a Snapshot,
    pub selected: usize,
    pub focused: bool,
    pub theme: Theme,
}

impl SessionsPanel<'_> {
    pub fn row_count(snapshot: &Snapshot) -> usize {
        active_sessions(snapshot).len()
    }
}

impl Widget for SessionsPanel<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let title_style = if self.focused {
            self.theme.highlight
        } else {
            self.theme.title
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(if self.focused {
                self.theme.active
            } else {
                self.theme.border
            })
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(Span::styled(" sessions ", title_style));

        let sessions = active_sessions(self.snapshot);
        let items: Vec<ListItem> = sessions
            .iter()
            .map(|s| {
                let err_tag = s
                    .last_failure_kind
                    .map(|k| format!(" last={k}"))
                    .unwrap_or_default();
                let style = if s.errors > 0 {
                    self.theme.fail
                } else if s.active > 0 {
                    self.theme.active
                } else {
                    Style::default()
                };
                let line = Line::from(Span::styled(
                    format!(
                        "{:<16} {:<14} r{:>3} e{:>3} {:>5.1}t/s{err_tag}",
                        truncate(&s.id, 16),
                        truncate(&s.last_model, 14),
                        s.requests,
                        s.errors,
                        s.tokens_per_second()
                    ),
                    style,
                ));
                ListItem::new(line)
            })
            .collect();

        let mut state = ListState::default();
        if self.focused && !sessions.is_empty() {
            state.select(Some(self.selected.min(sessions.len() - 1)));
        }

        StatefulWidget::render(
            List::new(items)
                .block(block)
                .highlight_style(self.theme.selected)
                .highlight_symbol("> "),
            area,
            buf,
            &mut state,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Session;

    #[test]
    fn active_sessions_filters_idle() {
        let snap = Snapshot {
            sessions: vec![
                Session {
                    id: "idle".into(),
                    active: 0,
                    requests: 3,
                    ..Default::default()
                },
                Session {
                    id: "live".into(),
                    active: 2,
                    requests: 5,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let list = active_sessions(&snap);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "live");
        assert_eq!(SessionsPanel::row_count(&snap), 1);
    }
}
