//! Right panel: active + recent turns (id, model, duration/status).

use super::truncate;
use crate::monitor::theme::Theme;
use crate::store::{Request, Snapshot};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, StatefulWidget, Widget},
};

/// Combined active-then-recent row for selection.
#[derive(Clone, Copy, Debug)]
pub enum TurnKind {
    Active,
    Recent,
}

pub struct ActivePanel<'a> {
    pub snapshot: &'a Snapshot,
    pub selected: usize,
    pub focused: bool,
    pub theme: Theme,
}

impl ActivePanel<'_> {
    /// Flattened active + recent list length.
    pub fn row_count(snapshot: &Snapshot) -> usize {
        snapshot.active.len() + snapshot.recent.len()
    }

    pub fn rows(snapshot: &Snapshot) -> Vec<(TurnKind, &Request)> {
        let mut out = Vec::with_capacity(Self::row_count(snapshot));
        for r in &snapshot.active {
            out.push((TurnKind::Active, r));
        }
        for r in &snapshot.recent {
            out.push((TurnKind::Recent, r));
        }
        out
    }
}

impl Widget for ActivePanel<'_> {
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
            .title(Span::styled(" active / recent ", title_style));

        let rows = Self::rows(self.snapshot);
        let items: Vec<ListItem> = rows
            .iter()
            .map(|(kind, r)| {
                let (label, style) = match kind {
                    TurnKind::Active => (
                        format!(
                            "▶ {:<14} {:<14} {:>5.1}s a{}",
                            truncate(&r.id, 14),
                            truncate(&r.model, 14),
                            r.duration().as_secs_f64(),
                            r.attempt
                        ),
                        self.theme.active,
                    ),
                    TurnKind::Recent => {
                        let failed = r.status == 0
                            || !(200..300).contains(&r.status)
                            || r.failure_kind.is_some()
                            || !r.error_type.is_empty();
                        let status_txt = if failed {
                            if !r.error_type.is_empty() {
                                format!(" {}", r.error_type)
                            } else if let Some(k) = r.failure_kind {
                                format!(" {k}")
                            } else if r.status == 0 {
                                " fail".into()
                            } else {
                                format!(" HTTP {}", r.status)
                            }
                        } else {
                            format!(" HTTP {}", r.status)
                        };
                        let style = if failed {
                            self.theme.fail
                        } else {
                            self.theme.ok
                        };
                        (
                            format!(
                                "  {:<14} {:<14}{status_txt}",
                                truncate(&r.id, 14),
                                truncate(&r.model, 14),
                            ),
                            style,
                        )
                    }
                };
                ListItem::new(Line::from(Span::styled(label, style)))
            })
            .collect();

        let mut state = ListState::default();
        if self.focused && !rows.is_empty() {
            state.select(Some(self.selected.min(rows.len() - 1)));
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
