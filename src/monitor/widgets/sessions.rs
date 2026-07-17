//! Left panel: session list (id, model, requests, errors, tok/s).

use crate::monitor::theme::Theme;
use crate::store::Snapshot;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, StatefulWidget, Widget},
};

pub struct SessionsPanel<'a> {
    pub snapshot: &'a Snapshot,
    pub selected: usize,
    pub focused: bool,
    pub theme: Theme,
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

        let items: Vec<ListItem> = self
            .snapshot
            .sessions
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
        if self.focused && !self.snapshot.sessions.is_empty() {
            state.select(Some(self.selected.min(self.snapshot.sessions.len() - 1)));
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

fn truncate(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_owned()
    } else if max <= 1 {
        "…".to_owned()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}
