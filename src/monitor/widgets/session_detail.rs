//! Right panel: inspector for the session selected (pinned) on the left.
//!
//! Layout (top → bottom):
//! 1. Session summary — identity, counters, rate, last failure
//! 2. Turns for that session — in-flight first, then recent history
//!
//! j/k navigates the turn list only; the summary always follows the pin.

use super::truncate;
use crate::monitor::theme::Theme;
use crate::monitor::widgets::metrics::{format_cache_read_value, format_token_count};
use crate::store::{Request, Session, Snapshot};

/// Session summary `tokens` line: output, absolute cache reads, and lifetime tok/s.
///
/// Cache reads use the same absolute+ratio formatting as the fleet metrics strip so a
/// zero-cache session is visible without leaving the inspector.
pub fn format_session_tokens_line(session: &Session) -> String {
    let cache = format_cache_read_value(session.cached_input_tokens, session.cache_read_ratio());
    format!(
        "{} out · cache {} · {:.1} t/s",
        format_token_count(session.output_tokens),
        cache,
        session.tokens_per_second()
    )
}
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, StatefulWidget, Widget},
};

/// Combined active-then-recent row for selection.
#[derive(Clone, Copy, Debug)]
pub enum TurnKind {
    Active,
    Recent,
}

pub struct SessionDetailPanel<'a> {
    pub snapshot: &'a Snapshot,
    /// Pinned session id from the sessions panel; `None` → empty state.
    pub session_id: Option<&'a str>,
    pub selected: usize,
    pub focused: bool,
    pub theme: Theme,
}

impl SessionDetailPanel<'_> {
    pub fn row_count(snapshot: &Snapshot, session_id: Option<&str>) -> usize {
        Self::rows(snapshot, session_id).len()
    }

    /// Resolve the pinned session from the full store (not only active-filtered).
    pub fn session<'s>(snapshot: &'s Snapshot, session_id: Option<&str>) -> Option<&'s Session> {
        let sid = session_id?;
        snapshot.sessions.iter().find(|s| s.id == sid)
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

    /// Failures belonging to the pinned session (newest first, store order).
    pub fn session_failures<'s>(
        snapshot: &'s Snapshot,
        session_id: Option<&str>,
    ) -> Vec<&'s crate::store::FailureRecord> {
        let Some(sid) = session_id else {
            return Vec::new();
        };
        snapshot
            .failures
            .iter()
            .filter(|f| f.session_id == sid)
            .collect()
    }
}

impl Widget for SessionDetailPanel<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let title_style = if self.focused {
            self.theme.highlight
        } else {
            self.theme.title
        };
        let session = Self::session(self.snapshot, self.session_id);
        let title = match session {
            Some(s) => format!(" session detail · {} ", truncate(&s.id, 14)),
            None if self.session_id.is_some() => " session detail · ? ".to_owned(),
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

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        match (self.session_id, session) {
            (None, _) => {
                Paragraph::new(vec![
                    Line::from(Span::styled("no session selected", self.theme.muted)),
                    Line::from(Span::styled(
                        "pick one in sessions (left)",
                        self.theme.muted,
                    )),
                ])
                .render(inner, buf);
            }
            (Some(sid), None) => {
                Paragraph::new(vec![
                    Line::from(Span::styled(
                        format!("session {} not in store", truncate(sid, 20)),
                        self.theme.fail,
                    )),
                    Line::from(Span::styled(
                        "(finished and dropped, or unknown id)",
                        self.theme.muted,
                    )),
                ])
                .render(inner, buf);
            }
            (Some(_), Some(session)) => {
                self.render_inspector(inner, buf, session);
            }
        }
    }
}

impl SessionDetailPanel<'_> {
    fn render_inspector(self, area: Rect, buf: &mut Buffer, session: &Session) {
        // Six summary lines; preserve at least a turns header + one list/empty row.
        let summary_h = 6u16.min(area.height.saturating_sub(2).max(1));
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(summary_h), Constraint::Min(1)])
            .split(area);
        self.render_summary(chunks[0], buf, session);
        self.render_turns(chunks[1], buf);
    }

    fn render_summary(&self, area: Rect, buf: &mut Buffer, session: &Session) {
        let last = session
            .last_failure_kind
            .map(|kind| kind.as_str())
            .unwrap_or("-");
        let updated = session
            .updated_at
            .map(|time| time.format("%H:%M:%S").to_string())
            .unwrap_or_else(|| "-".into());
        let fail_n = Self::session_failures(self.snapshot, self.session_id).len();
        let model = if session.last_model.is_empty() {
            "-"
        } else {
            session.last_model.as_str()
        };
        let cwd = if session.cwd.is_empty() {
            "-"
        } else {
            session.cwd.as_str()
        };
        let prompt = if session.last_prompt.is_empty() {
            "-"
        } else {
            session.last_prompt.as_str()
        };
        let value_width = area.width.saturating_sub(8) as usize;
        let identity = format!(
            "{} · model {}",
            truncate(&session.id, 16),
            truncate(model, value_width.saturating_sub(12))
        );
        let lines = vec![
            Line::from(vec![
                Span::styled("id      ", self.theme.muted),
                Span::styled(truncate(&identity, value_width), self.theme.header),
            ]),
            Line::from(vec![
                Span::styled("reqs    ", self.theme.muted),
                Span::styled(
                    format!(
                        "{:<5} active {:<4} errs {}",
                        session.requests, session.active, session.errors
                    ),
                    if session.errors > 0 {
                        self.theme.fail
                    } else if session.active > 0 {
                        self.theme.active
                    } else {
                        self.theme.header
                    },
                ),
            ]),
            Line::from(vec![
                Span::styled("tokens  ", self.theme.muted),
                Span::styled(format_session_tokens_line(session), self.theme.header),
            ]),
            Line::from(vec![
                Span::styled("last    ", self.theme.muted),
                Span::styled(
                    last,
                    if session.last_failure_kind.is_some() {
                        self.theme.fail
                    } else {
                        self.theme.ok
                    },
                ),
                Span::styled(format!(" ring {fail_n} · upd {updated}"), self.theme.muted),
            ]),
            Line::from(vec![
                Span::styled("cwd     ", self.theme.muted),
                Span::styled(truncate(cwd, value_width), self.theme.header),
            ]),
            Line::from(vec![
                Span::styled("prompt  ", self.theme.muted),
                Span::styled(truncate(prompt, value_width), self.theme.header),
            ]),
        ];
        Paragraph::new(lines).render(area, buf);
    }

    fn render_turns(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }

        // Sub-header + list.
        let header_h = 1u16.min(area.height);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(header_h), Constraint::Min(0)])
            .split(area);

        let rows = Self::rows(self.snapshot, self.session_id);
        let active_n = rows
            .iter()
            .filter(|(k, _)| matches!(k, TurnKind::Active))
            .count();
        let recent_n = rows.len().saturating_sub(active_n);

        Paragraph::new(Line::from(Span::styled(
            format!("turns  active {active_n} · recent {recent_n}"),
            self.theme.title,
        )))
        .render(chunks[0], buf);

        if chunks[1].height == 0 {
            return;
        }

        if rows.is_empty() {
            Paragraph::new(Line::from(Span::styled(
                "  (no turns in active/recent for this session)",
                self.theme.muted,
            )))
            .render(chunks[1], buf);
            return;
        }

        let items: Vec<ListItem> = rows
            .iter()
            .map(|(kind, r)| {
                let (label, style) = match kind {
                    TurnKind::Active => (
                        format!(
                            "▶ {:<12} {:<12} {:>5.1}s a{}",
                            truncate(&r.id, 12),
                            truncate(&r.model, 12),
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
                            format!(
                                " ok {}  {}tok {:>5.1}s",
                                r.status,
                                r.output_tokens,
                                r.duration().as_secs_f64()
                            )
                        };
                        let style = if failed {
                            self.theme.fail
                        } else {
                            self.theme.ok
                        };
                        (
                            format!(
                                "  {:<12} {:<12}{status_txt}",
                                truncate(&r.id, 12),
                                truncate(&r.model, 12),
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
                .highlight_style(self.theme.selected)
                .highlight_symbol("> "),
            chunks[1],
            buf,
            &mut state,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::FailureKind;
    use crate::store::{FailureRecord, Request, Session};
    use chrono::Utc;
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
            usage: None,
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

    fn session(id: &str, active: u64) -> Session {
        Session {
            id: id.into(),
            active,
            requests: 3,
            output_tokens: 40,
            last_model: "gpt".into(),
            last_prompt: "inspect this".into(),
            cwd: "/tmp/project".into(),
            errors: 1,
            last_failure_kind: Some(FailureKind::UpstreamHttp),
            updated_at: Some(Utc::now()),
            sample_seconds: 2.0,
            ..Default::default()
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

    #[test]
    fn session_lookup_uses_full_store() {
        let snap = Snapshot {
            sessions: vec![session("idle", 0), session("live", 2)],
            ..Default::default()
        };
        assert_eq!(
            SessionDetailPanel::session(&snap, Some("idle")).map(|s| s.id.as_str()),
            Some("idle")
        );
        assert!(SessionDetailPanel::session(&snap, None).is_none());
        assert!(SessionDetailPanel::session(&snap, Some("gone")).is_none());
    }

    #[test]
    fn session_tokens_line_shows_absolute_cache_reads() {
        let hot = Session {
            id: "hot".into(),
            output_tokens: 40,
            input_tokens: 1_010,
            cached_input_tokens: 900,
            usage_requests: 2,
            sample_seconds: 2.0,
            ..Default::default()
        };
        let line = format_session_tokens_line(&hot);
        assert!(
            line.contains("900") && line.contains("cache") && line.contains("89%"),
            "nonzero cache reads missing absolute count: {line}"
        );
        assert!(
            line.contains("cache 900·89%"),
            "session tokens line should use compact count·ratio: {line}"
        );
        assert!(line.contains("40 out"), "output count missing: {line}");

        let cold = Session {
            id: "cold".into(),
            output_tokens: 10,
            input_tokens: 500,
            cached_input_tokens: 0,
            usage_requests: 4,
            sample_seconds: 1.0,
            ..Default::default()
        };
        let line = format_session_tokens_line(&cold);
        assert!(
            line.contains("cache 0·0%"),
            "zero cache reads must stay visible: {line}"
        );

        let unknown = Session {
            id: "new".into(),
            output_tokens: 0,
            ..Default::default()
        };
        let line = format_session_tokens_line(&unknown);
        assert!(
            line.contains("cache n/a"),
            "no usage observations → n/a: {line}"
        );
    }

    #[test]
    fn session_failures_filtered() {
        let f1 = FailureRecord {
            ts: Utc::now(),
            request_id: "r1".into(),
            session_id: "s1".into(),
            requested_model: "a".into(),
            model: "m".into(),
            status_code: 502,
            duration_ms: 10,
            kind: FailureKind::UpstreamHttp,
            error_type: "upstream_http".into(),
            error_message: String::new(),
            response_id: String::new(),
            mapped: false,
            lite: false,
            fast: false,
            auth_retried: false,
            attempt: 1,
            output_count: 0,
            capture_bytes: 0,
            session_failure_index: 1,
        };
        let mut f2 = f1.clone();
        f2.request_id = "r2".into();
        f2.session_id = "s2".into();
        let snap = Snapshot {
            failures: vec![f1, f2],
            ..Default::default()
        };
        let list = SessionDetailPanel::session_failures(&snap, Some("s1"));
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].request_id, "r1");
    }
}
