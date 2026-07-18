//! Right panel: turns for the selected (pinned) session — active then recent.

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

pub struct SessionDetailPanel<'a> {
    pub snapshot: &'a Snapshot,
    /// Pinned session id from the sessions panel; `None` → empty list.
    pub session_id: Option<&'a str>,
    pub selected: usize,
    pub focused: bool,
    pub theme: Theme,
}

impl SessionDetailPanel<'_> {
    pub fn row_count(snapshot: &Snapshot, session_id: Option<&str>) -> usize {
        Self::rows(snapshot, session_id).len()
    }

    /// Active turns for `session_id`, then matching recent (store order).
    pub fn rows<'s>(
        snapshot: &'s Snapshot,
        session_id: Option<&str>,
    ) -> Vec<(TurnKind, &'s Request)> {
        let Some(sid) = session_id else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for r in &snapshot.active {
            if r.session_id == sid {
                out.push((TurnKind::Active, r));
            }
        }
        for r in &snapshot.recent {
            if r.session_id == sid {
                out.push((TurnKind::Recent, r));
            }
        }
        out
    }
}

impl Widget for SessionDetailPanel<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let title_style = if self.focused {
            self.theme.highlight
        } else {
            self.theme.title
        };
        let title = match self.session_id.and_then(|key| {
            self.snapshot
                .sessions
                .iter()
                .find(|session| session.id == key)
        }) {
            Some(session) => format!(" session detail {} ", truncate(&session.id, 12)),
            None => " session detail ".to_owned(),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(if self.focused {
                self.theme.active
            } else {
                self.theme.border
            })
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(Span::styled(title, title_style));

        let rows = Self::rows(self.snapshot, self.session_id);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Request;
    use std::time::Instant;

    fn req(id: &str, session: &str) -> Request {
        Request {
            id: id.into(),
            session_id: session.into(),
            requested_model: "m".into(),
            model: "m".into(),
            status: 200,
            error: String::new(),
            error_type: String::new(),
            failure_kind: None,
            output_tokens: 0,
            started_at: Instant::now(),
            ended_at: None,
            duration_ms: 0,
            response_id: String::new(),
            mapped: false,
            lite: false,
            fast: false,
            auth_retried: false,
            attempt: 1,
            output_count: 0,
            capture_bytes: 0,
        }
    }

    #[test]
    fn rows_scoped_to_session() {
        let snap = Snapshot {
            active: vec![req("a1", "s1"), req("a2", "s2")],
            recent: vec![req("r1", "s1"), req("r2", "s2"), req("r3", "s1")],
            ..Default::default()
        };
        let rows = SessionDetailPanel::rows(&snap, Some("s1"));
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].1.id, "a1");
        assert_eq!(rows[1].1.id, "r1");
        assert_eq!(rows[2].1.id, "r3");
        assert!(SessionDetailPanel::rows(&snap, None).is_empty());
        assert!(SessionDetailPanel::rows(&snap, Some("missing")).is_empty());
    }

    #[test]
    fn rows_join_on_full_session_key() {
        let prefix = "x".repeat(256);
        let first = format!("{prefix}a");
        let second = format!("{prefix}b");
        let snap = Snapshot {
            active: vec![req("a1", &first), req("a2", &second)],
            ..Default::default()
        };
        let rows = SessionDetailPanel::rows(&snap, Some(&first));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.id, "a1");
    }
}
