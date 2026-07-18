//! Full-width failures panel: time, kind, session, error_type, status, attempt.
//! Same-session failures within 30s are shown as estimated client-retry groups.

use super::truncate;
use crate::events::FailureKind;
use crate::monitor::app::FailureFilter;
use crate::monitor::theme::Theme;
use crate::store::{FailureRecord, Snapshot};
use chrono::{DateTime, Utc};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, StatefulWidget, Widget},
};
use std::collections::HashMap;

/// Failures with the same `session_id` within this gap form an estimated retry group.
pub const RETRY_GROUP_WINDOW_SECS: i64 = 30;

pub struct FailuresPanel<'a> {
    pub snapshot: &'a Snapshot,
    pub selected: usize,
    pub focused: bool,
    pub filter: FailureFilter,
    pub theme: Theme,
}

/// One estimated client-retry group (same session, ≤30s between consecutive fails).
#[derive(Clone, Debug)]
pub struct FailureGroup<'a> {
    pub session_id: &'a str,
    /// Newest-first within the group.
    pub members: Vec<&'a FailureRecord>,
}

impl FailureGroup<'_> {
    pub fn estimated(&self) -> bool {
        self.members.len() > 1
    }

    pub fn span_secs(&self) -> i64 {
        let (Some(newest), Some(oldest)) = (
            self.members.iter().map(|m| m.ts).max(),
            self.members.iter().map(|m| m.ts).min(),
        ) else {
            return 0;
        };
        newest.signed_duration_since(oldest).num_seconds().max(0)
    }

    pub fn kind_summary(&self) -> String {
        let mut counts: HashMap<FailureKind, usize> = HashMap::new();
        for m in &self.members {
            *counts.entry(m.kind).or_insert(0) += 1;
        }
        let mut parts: Vec<(FailureKind, usize)> = counts.into_iter().collect();
        parts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.as_str().cmp(b.0.as_str())));
        parts
            .into_iter()
            .map(|(k, n)| format!("{}×{n}", k.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    }
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

    /// Group filtered failures by session_id with ≤30s proximity, newest group first.
    /// Flattened member order is the panel selection order.
    pub fn groups(snapshot: &Snapshot, filter: FailureFilter) -> Vec<FailureGroup<'_>> {
        group_failures(&Self::filtered(snapshot, filter))
    }

    /// Selectable rows in display order (group-aware, newest group first).
    pub fn ordered(snapshot: &Snapshot, filter: FailureFilter) -> Vec<&FailureRecord> {
        Self::groups(snapshot, filter)
            .into_iter()
            .flat_map(|g| g.members)
            .collect()
    }
}

/// Cluster filtered failures (any order) into estimated retry groups.
pub fn group_failures<'a>(failures: &[&'a FailureRecord]) -> Vec<FailureGroup<'a>> {
    let mut by_session: HashMap<&str, Vec<&FailureRecord>> = HashMap::new();
    for f in failures {
        by_session.entry(f.session_id.as_str()).or_default().push(f);
    }

    let mut groups: Vec<FailureGroup<'a>> = Vec::new();
    for (session_id, mut recs) in by_session {
        recs.sort_by(|a, b| {
            a.ts.cmp(&b.ts)
                .then_with(|| a.request_id.cmp(&b.request_id))
        });
        let mut current: Vec<&FailureRecord> = Vec::new();
        for f in recs {
            if let Some(last) = current.last() {
                let gap = f.ts.signed_duration_since(last.ts).num_seconds();
                if gap <= RETRY_GROUP_WINDOW_SECS {
                    current.push(f);
                    continue;
                }
                groups.push(FailureGroup {
                    session_id,
                    members: std::mem::take(&mut current),
                });
            }
            current.push(f);
        }
        if !current.is_empty() {
            groups.push(FailureGroup {
                session_id,
                members: current,
            });
        }
    }

    // Newest group first (by max ts).
    groups.sort_by(|a, b| {
        let ta = group_newest_ts(a);
        let tb = group_newest_ts(b);
        tb.cmp(&ta).then_with(|| a.session_id.cmp(b.session_id))
    });

    // Within each group: newest first for display.
    for g in &mut groups {
        g.members.sort_by(|a, b| {
            b.ts.cmp(&a.ts)
                .then_with(|| b.request_id.cmp(&a.request_id))
        });
    }
    groups
}

fn group_newest_ts(g: &FailureGroup<'_>) -> DateTime<Utc> {
    g.members
        .iter()
        .map(|m| m.ts)
        .max()
        .unwrap_or_else(Utc::now)
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
        let groups = Self::groups(self.snapshot, self.filter);
        let rows: Vec<&FailureRecord> = groups
            .iter()
            .flat_map(|g| g.members.iter().copied())
            .collect();
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

        let items: Vec<ListItem> = groups
            .iter()
            .flat_map(|g| {
                let estimated = g.estimated();
                let n = g.members.len();
                g.members.iter().enumerate().map(move |(i, f)| {
                    let ts = f.ts.format("%H:%M:%S").to_string();
                    let kind = f.kind.as_str();
                    let err = if f.error_type.is_empty() {
                        "-"
                    } else {
                        f.error_type.as_str()
                    };
                    let prefix = if estimated && i > 0 { "  ↳ " } else { "" };
                    let suffix = if estimated && i == 0 {
                        // Label estimated client-retry clusters (same session, ≤30s).
                        format!(
                            "  [×{n} estimated {}s · {}]",
                            g.span_secs(),
                            g.kind_summary()
                        )
                    } else {
                        String::new()
                    };
                    let label = format!(
                        "{prefix}{ts}  {:<18} {:<12} {:<22} {:>3} a{}{suffix}",
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
    use chrono::{Duration, TimeZone};

    fn rec_at(kind: FailureKind, session: &str, req: &str, ts: DateTime<Utc>) -> FailureRecord {
        FailureRecord {
            ts,
            request_id: req.into(),
            session_id: session.into(),
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

    fn rec(kind: FailureKind) -> FailureRecord {
        rec_at(kind, "s", "r", Utc::now())
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

    #[test]
    fn groups_same_session_within_30s() {
        let t0 = Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap();
        let failures = [
            rec_at(FailureKind::ProxyAssemble, "abc", "r1", t0),
            rec_at(
                FailureKind::ProxyAssemble,
                "abc",
                "r2",
                t0 + Duration::seconds(8),
            ),
            rec_at(
                FailureKind::UpstreamHttp,
                "abc",
                "r3",
                t0 + Duration::seconds(15),
            ),
            // Same session but >30s after previous chain end → new group
            rec_at(
                FailureKind::StreamIo,
                "abc",
                "r4",
                t0 + Duration::seconds(50),
            ),
            // Different session
            rec_at(
                FailureKind::UpstreamHttp,
                "other",
                "r5",
                t0 + Duration::seconds(1),
            ),
        ];
        let refs: Vec<&FailureRecord> = failures.iter().collect();
        let groups = group_failures(&refs);
        // abc: [r1,r2,r3] + [r4]; other: [r5] → 3 groups
        assert_eq!(groups.len(), 3);
        let multi = groups
            .iter()
            .find(|g| g.session_id == "abc" && g.members.len() == 3)
            .expect("3-member abc group");
        assert!(multi.estimated());
        assert_eq!(multi.span_secs(), 15);
        assert!(multi.kind_summary().contains("ProxyAssemble×2"));
        assert!(multi.kind_summary().contains("UpstreamHttp×1"));

        let late = groups
            .iter()
            .find(|g| g.session_id == "abc" && g.members.len() == 1)
            .expect("solo late abc");
        assert!(!late.estimated());
        assert_eq!(late.members[0].request_id, "r4");
    }

    #[test]
    fn gap_over_30s_splits_group() {
        let t0 = Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap();
        let failures = [
            rec_at(FailureKind::ProxyAssemble, "s", "a", t0),
            rec_at(
                FailureKind::ProxyAssemble,
                "s",
                "b",
                t0 + Duration::seconds(31),
            ),
        ];
        let refs: Vec<&FailureRecord> = failures.iter().collect();
        let groups = group_failures(&refs);
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|g| !g.estimated()));
    }

    #[test]
    fn gap_exactly_30s_forms_estimated_group() {
        let t0 = Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap();
        let failures = [
            rec_at(FailureKind::ProxyAssemble, "s", "a", t0),
            rec_at(
                FailureKind::UpstreamHttp,
                "s",
                "b",
                t0 + Duration::seconds(RETRY_GROUP_WINDOW_SECS),
            ),
        ];
        let refs: Vec<&FailureRecord> = failures.iter().collect();
        let groups = group_failures(&refs);
        assert_eq!(groups.len(), 1, "gap == 30s is inclusive");
        assert!(groups[0].estimated());
        assert_eq!(groups[0].span_secs(), RETRY_GROUP_WINDOW_SECS);
    }

    #[test]
    fn chained_gaps_extend_group_beyond_30s_span() {
        // Consecutive gaps ≤30s chain: t=0,25,50 → one group with span 50s.
        let t0 = Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap();
        let failures = [
            rec_at(FailureKind::ProxyAssemble, "s", "a", t0),
            rec_at(
                FailureKind::ProxyAssemble,
                "s",
                "b",
                t0 + Duration::seconds(25),
            ),
            rec_at(
                FailureKind::UpstreamHttp,
                "s",
                "c",
                t0 + Duration::seconds(50),
            ),
        ];
        let refs: Vec<&FailureRecord> = failures.iter().collect();
        let groups = group_failures(&refs);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members.len(), 3);
        assert_eq!(groups[0].span_secs(), 50);
    }

    #[test]
    fn ordered_newest_group_first() {
        let t0 = Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap();
        let snap = Snapshot {
            failures: vec![
                // Newest first in ring (as store keeps them)
                rec_at(
                    FailureKind::UpstreamHttp,
                    "s2",
                    "new",
                    t0 + Duration::seconds(100),
                ),
                rec_at(
                    FailureKind::ProxyAssemble,
                    "s1",
                    "mid",
                    t0 + Duration::seconds(5),
                ),
                rec_at(FailureKind::ProxyAssemble, "s1", "old", t0),
            ],
            ..Default::default()
        };
        let ordered = FailuresPanel::ordered(&snap, FailureFilter::All);
        assert_eq!(ordered[0].request_id, "new");
        // s1 group members newest-first
        assert_eq!(ordered[1].request_id, "mid");
        assert_eq!(ordered[2].request_id, "old");
    }
}
