//! Full-width failures panel: time, kind, session, error_type, status, attempt.

use super::truncate;
use crate::events::FailureKind;
use crate::monitor::app::FailureFilter;
use crate::monitor::theme::Theme;
use crate::store::{FailureRecord, Snapshot};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, StatefulWidget, Widget},
};

pub struct FailuresPanel<'a> {
    pub snapshot: &'a Snapshot,
    pub selected: usize,
    pub focused: bool,
    pub filter: FailureFilter,
    pub theme: Theme,
}

impl FailuresPanel<'_> {
    /// Failures matching the current filter (newest first — snapshot order).
    pub fn filtered(snapshot: &Snapshot, filter: FailureFilter) -> Vec<&FailureRecord> {
        snapshot
            .failures
            .iter()
            .filter(|f| filter.matches(f.kind))
            .collect()
    }

    /// Count without allocating a filtered vector (called every tick).
    pub fn row_count(snapshot: &Snapshot, filter: FailureFilter) -> usize {
        snapshot
            .failures
            .iter()
            .filter(|f| filter.matches(f.kind))
            .count()
    }
}

/// Style for a FailureKind row (and title accents).
pub fn kind_style(kind: FailureKind, theme: Theme) -> Style {
    match kind {
        FailureKind::ProxyAssemble => theme.assemble,
        FailureKind::UpstreamHttp | FailureKind::UpstreamConnect => theme.fail,
        FailureKind::AuthRetryFailed => theme.auth,
        FailureKind::StreamIo | FailureKind::StreamTerminalFailed => theme.stream,
        FailureKind::ClientRejected => theme.muted,
        FailureKind::Unknown => theme.muted,
    }
}

impl Widget for FailuresPanel<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let rows = Self::filtered(self.snapshot, self.filter);
        let title_style = if self.focused {
            self.theme.highlight
        } else {
            self.theme.title
        };
        let title = format!(
            " failures [{}] {}/{} ",
            self.filter.as_str(),
            rows.len(),
            self.snapshot.failures.len()
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(if self.focused {
                self.theme.active
            } else {
                self.theme.border
            })
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(Span::styled(title, title_style));

        let items: Vec<ListItem> = rows
            .iter()
            .map(|f| {
                let ts = f.ts.format("%H:%M:%S").to_string();
                let kind = f.kind.as_str();
                let err = if f.error_type.is_empty() {
                    "-"
                } else {
                    f.error_type.as_str()
                };
                let label = format!(
                    "{ts}  {:<18} {:<12} {:<22} {:>3} a{}",
                    truncate(kind, 18),
                    truncate(&f.session_id, 12),
                    truncate(err, 22),
                    f.status_code,
                    f.attempt
                );
                ListItem::new(Line::from(Span::styled(
                    label,
                    kind_style(f.kind, self.theme),
                )))
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
    use crate::events::FailureKind;
    use chrono::Utc;

    fn rec(kind: FailureKind) -> FailureRecord {
        FailureRecord {
            ts: Utc::now(),
            request_id: "r".into(),
            session_id: "s".into(),
            requested_model: "a".into(),
            model: "m".into(),
            status_code: 502,
            duration_ms: 10,
            kind,
            error_type: "t".into(),
            error_message: "e".into(),
            response_id: String::new(),
            mapped: true,
            lite: false,
            fast: false,
            auth_retried: false,
            attempt: 1,
            output_count: 0,
            capture_bytes: 0,
            session_failure_index: 1,
        }
    }

    #[test]
    fn filter_selects_kinds() {
        let snap = Snapshot {
            failures: vec![
                rec(FailureKind::ProxyAssemble),
                rec(FailureKind::UpstreamHttp),
                rec(FailureKind::UpstreamConnect),
                rec(FailureKind::StreamIo),
                rec(FailureKind::StreamTerminalFailed),
                rec(FailureKind::AuthRetryFailed),
                rec(FailureKind::ClientRejected),
                rec(FailureKind::Unknown),
            ],
            ..Default::default()
        };
        assert_eq!(FailuresPanel::row_count(&snap, FailureFilter::All), 8);
        assert_eq!(
            FailuresPanel::row_count(&snap, FailureFilter::ProxyAssemble),
            1
        );
        // UpstreamHttp + UpstreamConnect
        assert_eq!(FailuresPanel::row_count(&snap, FailureFilter::Upstream), 2);
        assert_eq!(FailuresPanel::row_count(&snap, FailureFilter::Auth), 1);
        // StreamIo + StreamTerminalFailed
        assert_eq!(FailuresPanel::row_count(&snap, FailureFilter::Stream), 2);
        // ClientRejected / Unknown only under All
        for filter in [
            FailureFilter::ProxyAssemble,
            FailureFilter::Upstream,
            FailureFilter::Auth,
            FailureFilter::Stream,
        ] {
            let kinds: Vec<_> = FailuresPanel::filtered(&snap, filter)
                .iter()
                .map(|f| f.kind)
                .collect();
            assert!(
                !kinds.contains(&FailureKind::ClientRejected),
                "{filter:?} should not include ClientRejected"
            );
            assert!(
                !kinds.contains(&FailureKind::Unknown),
                "{filter:?} should not include Unknown"
            );
        }
    }
}
