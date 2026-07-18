//! Metrics mid-strip: tok/s, error rate, completed outcomes, and cache-read counts.

use crate::monitor::theme::Theme;
use crate::store::{Session, Snapshot};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

/// Terminal display width for metric layout.
///
/// Metric strings use ASCII plus single-width Unicode (middle dot, block sparklines).
/// Using UTF-8 byte length under-counts spark room when `·` (2 bytes, 1 column) appears.
pub fn display_width(s: &str) -> usize {
    s.chars().count()
}

/// Compact absolute token count for tight metric cells (`900`, `1.2k`, `3.4M`).
///
/// One-decimal rounding that would print as `1000.0k` / `1000.0M` is promoted to the
/// next unit so the string never grows longer at unit boundaries.
pub fn format_token_count(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    let k = (n as f64 / 1_000.0 * 10.0).round() / 10.0;
    if k < 1000.0 {
        return format!("{k:.1}k");
    }
    let m = (n as f64 / 1_000_000.0 * 10.0).round() / 10.0;
    if m < 1000.0 {
        return format!("{m:.1}M");
    }
    let g = (n as f64 / 1_000_000_000.0 * 10.0).round() / 10.0;
    format!("{g:.1}G")
}

/// Fleet/session cache cell: absolute cache-read tokens plus optional ratio.
///
/// - No usage observations yet → `n/a`
/// - Otherwise → `{count}·{pct}%` (no spaces) so a 64-col strip keeps count and ratio
///   inside the cache column; zero reads stay visible as `0·0%`
pub fn format_cache_read_value(cached_input_tokens: u64, ratio: Option<f64>) -> String {
    match ratio {
        None => "n/a".into(),
        Some(ratio) => format!(
            "{}·{:.0}%",
            format_token_count(cached_input_tokens),
            ratio * 100.0
        ),
    }
}

/// Mean of per-session lifetime tok/s over sessions that have a defined rate.
///
/// Sessions with `sample_seconds == 0` (no completed output yet) are excluded.
/// Used for the live metrics/header number; the sparkline uses 1 Hz samples of
/// this same value pushed by the monitor loop.
pub fn fleet_avg_tok_s(snapshot: &Snapshot) -> f64 {
    fleet_avg_tok_s_from_sessions(&snapshot.sessions)
}

/// Testable core of [`fleet_avg_tok_s`].
pub fn fleet_avg_tok_s_from_sessions(sessions: &[Session]) -> f64 {
    let mut total = 0.0;
    let mut n = 0usize;
    for s in sessions {
        let rate = s.tokens_per_second();
        if rate > 0.0 {
            total += rate;
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { total / n as f64 }
}

/// Unicode block levels for a one-row sparkline (▁…█).
const SPARK_LEVELS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Mid-strip showing rolling store metrics as compact sparklines + meters.
pub struct MetricsStrip<'a> {
    pub snapshot: &'a Snapshot,
    pub theme: Theme,
}

impl Widget for MetricsStrip<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(Span::styled(" metrics ", self.theme.title));

        let inner = block.inner(area);
        block.render(area, buf);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let tok = Metrics::from_snapshot(self.snapshot);

        // Cache column is wider: absolute count + ratio (`12.3k·100%`) must fit at the
        // documented 64-col strip minimum (inner width 62 after borders).
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(20),
                Constraint::Percentage(18),
                Constraint::Percentage(24),
                Constraint::Percentage(38),
            ])
            .split(inner);

        let err_style = if tok.error_rate > 0.25 {
            self.theme.fail
        } else if tok.error_rate > 0.05 {
            self.theme.auth
        } else {
            self.theme.ok
        };
        render_metric(
            cols[0],
            buf,
            MetricCell {
                label: "tok/s",
                value: format!("{:.1}", tok.avg_tok_s),
                spark_values: &tok.tok_samples,
                fixed_max: None,
                value_style: self.theme.ok,
                muted: self.theme.muted,
                spark_style: self.theme.header,
            },
        );
        // Label "fail%" (not "err") so it is distinct from header err●N (failure ring count).
        render_metric(
            cols[1],
            buf,
            MetricCell {
                label: "fail%",
                value: format!("{:.0}%", tok.error_rate * 100.0),
                spark_values: &tok.error_samples,
                fixed_max: Some(1.0),
                value_style: err_style,
                muted: self.theme.muted,
                spark_style: self.theme.header,
            },
        );
        render_metric(
            cols[2],
            buf,
            MetricCell {
                label: "done",
                value: format!("{}ok/{}f", tok.completed_ok, tok.completed_fail),
                spark_values: &tok.outcome_samples,
                fixed_max: Some(1.0),
                value_style: self.theme.active,
                muted: self.theme.muted,
                spark_style: self.theme.header,
            },
        );
        render_metric(
            cols[3],
            buf,
            MetricCell {
                label: "cache",
                value: tok.cache_cell_value(),
                spark_values: &[],
                fixed_max: Some(1.0),
                value_style: self.theme.ok,
                muted: self.theme.muted,
                spark_style: self.theme.header,
            },
        );
    }
}

struct MetricCell<'a> {
    label: &'static str,
    value: String,
    spark_values: &'a [f64],
    fixed_max: Option<f64>,
    value_style: ratatui::style::Style,
    muted: ratatui::style::Style,
    spark_style: ratatui::style::Style,
}

fn render_metric(area: Rect, buf: &mut Buffer, cell: MetricCell<'_>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let label = format!(" {} ", cell.label);
    let value = format!("{} ", cell.value);
    // Display columns, not UTF-8 bytes — `·` is 2 bytes but 1 terminal cell.
    let spark_width =
        (area.width as usize).saturating_sub(display_width(&label) + display_width(&value));
    let spark = match cell.fixed_max {
        Some(max) => sparkline_chars_scaled(cell.spark_values, spark_width, Some(max)),
        None => sparkline_chars(cell.spark_values, spark_width),
    };
    let line = Line::from(vec![
        Span::styled(label, cell.muted),
        Span::styled(value, cell.value_style),
        Span::styled(spark, cell.spark_style),
    ]);
    Paragraph::new(line).render(area, buf);
}

/// Derived view of store metric rings for the strip.
#[derive(Clone, Debug, Default)]
pub struct Metrics {
    pub avg_tok_s: f64,
    /// Fleet total cache-read tokens from terminal usage aggregates.
    pub cached_input_tokens: u64,
    pub cache_read_ratio: Option<f64>,
    pub error_rate: f64,
    pub completed_ok: usize,
    pub completed_fail: usize,
    pub tok_samples: Vec<f64>,
    /// Per-sample error (1.0 fail / 0.0 ok) for the err sparkline.
    pub error_samples: Vec<f64>,
    /// Outcomes: 1.0 completed ok, 0.25 fail (visible blip), for done sparkline.
    pub outcome_samples: Vec<f64>,
}

impl Metrics {
    pub fn from_snapshot(snapshot: &Snapshot) -> Self {
        // Live number = fleet mean; sparkline = 1 Hz history ring.
        let avg_tok_s = fleet_avg_tok_s(snapshot);
        let tok_samples = snapshot.metrics_tok_s.clone();

        let completed = &snapshot.metrics_completed;
        let completed_ok = completed.iter().filter(|&&v| v >= 0.5).count();
        let completed_fail = completed.len().saturating_sub(completed_ok);
        let error_rate = if completed.is_empty() {
            0.0
        } else {
            completed_fail as f64 / completed.len() as f64
        };

        let error_samples: Vec<f64> = completed
            .iter()
            .map(|&v| if v >= 0.5 { 0.0 } else { 1.0 })
            .collect();
        // Keep failures visible in the outcome history (0 would collapse to baseline).
        let outcome_samples: Vec<f64> = completed
            .iter()
            .map(|&v| if v >= 0.5 { 1.0 } else { 0.25 })
            .collect();

        Self {
            avg_tok_s,
            cached_input_tokens: snapshot.cached_input_tokens,
            cache_read_ratio: snapshot.cache_read_ratio(),
            error_rate,
            completed_ok,
            completed_fail,
            tok_samples,
            error_samples,
            outcome_samples,
        }
    }

    /// Value rendered in the metrics strip `cache` cell.
    pub fn cache_cell_value(&self) -> String {
        format_cache_read_value(self.cached_input_tokens, self.cache_read_ratio)
    }
}

/// One-row block sparkline. Pads on the left when fewer samples than `width`.
pub fn sparkline_chars(values: &[f64], width: usize) -> String {
    sparkline_chars_scaled(values, width, None)
}

fn sparkline_chars_scaled(values: &[f64], width: usize, fixed_max: Option<f64>) -> String {
    if width == 0 {
        return String::new();
    }
    if values.is_empty() {
        // Same length as a data series so cold-start columns do not jump width.
        return "·".repeat(width);
    }
    let start = values.len().saturating_sub(width);
    let slice = &values[start..];
    let max = fixed_max.unwrap_or_else(|| slice.iter().copied().fold(0.0_f64, f64::max));
    let max = max.max(f64::EPSILON);

    let mut out = String::with_capacity(width);
    for _ in 0..width.saturating_sub(slice.len()) {
        out.push(' ');
    }
    for &v in slice {
        if v <= 0.0 {
            out.push(SPARK_LEVELS[0]);
            continue;
        }
        let t = (v / max).clamp(0.0, 1.0);
        let idx =
            ((t * (SPARK_LEVELS.len() - 1) as f64).round() as usize).min(SPARK_LEVELS.len() - 1);
        out.push(SPARK_LEVELS[idx]);
    }
    out
}

/// Whether the metrics strip should be drawn for this frame size.
pub fn should_show_metrics(area_width: u16, area_height: u16) -> bool {
    // Full dashboard needs header(3)+metrics(3)+body(min 6)+footer(3) ≈ 15;
    // require a bit more so body panels stay usable. At least 64 columns keeps the
    // widened cache column (absolute count + ratio) fully visible after borders.
    area_width >= 64 && area_height >= 18
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Snapshot;

    #[test]
    fn sparkline_empty_and_scaled() {
        assert_eq!(sparkline_chars(&[], 0), "");
        let empty = sparkline_chars(&[], 5);
        assert_eq!(empty.chars().count(), 5);
        assert!(empty.chars().all(|c| c == '·'));
        // Empty series uses full width (not capped) so it matches data-path length.
        assert_eq!(sparkline_chars(&[], 20).chars().count(), 20);

        let s = sparkline_chars(&[0.0, 1.0, 2.0, 4.0], 4);
        assert_eq!(s.chars().count(), 4);
        // Max maps to █; zero maps to ▁.
        assert!(s.ends_with('█'), "max should be full block: {s}");
        assert!(s.starts_with('▁'), "zero should be baseline: {s}");

        let failures = sparkline_chars_scaled(&[0.25, 0.25], 2, Some(1.0));
        let successes = sparkline_chars_scaled(&[1.0, 1.0], 2, Some(1.0));
        assert_ne!(failures, successes, "fixed scale must distinguish outcomes");
        assert!(successes.chars().all(|c| c == '█'));
    }

    #[test]
    fn sparkline_pads_and_truncates() {
        let short = sparkline_chars(&[1.0, 2.0], 5);
        assert_eq!(short.chars().count(), 5);
        assert!(short.starts_with("   "), "left pad: {short:?}");

        let long = sparkline_chars(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 3);
        assert_eq!(long.chars().count(), 3);
        // Last three: 4,5,6 — max is 6 → █ at end.
        assert!(long.ends_with('█'), "{long}");
    }

    #[test]
    fn metrics_from_completed_samples() {
        let snap = Snapshot {
            metrics_tok_s: vec![10.0, 20.0, 30.0],
            metrics_completed: vec![1.0, 1.0, 0.0, 1.0], // 1 fail / 4
            sessions: vec![
                Session {
                    id: "a".into(),
                    output_tokens: 20,
                    sample_seconds: 2.0, // 10 tok/s
                    ..Default::default()
                },
                Session {
                    id: "b".into(),
                    output_tokens: 30,
                    sample_seconds: 1.0, // 30 tok/s
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let m = Metrics::from_snapshot(&snap);
        // Live avg = mean of session rates (10 + 30) / 2 = 20; spark ring is separate.
        assert!((m.avg_tok_s - 20.0).abs() < 1e-9);
        assert_eq!(m.tok_samples, vec![10.0, 20.0, 30.0]);
        assert_eq!(m.cache_read_ratio, None);
        assert_eq!(m.completed_ok, 3);
        assert_eq!(m.completed_fail, 1);
        assert!((m.error_rate - 0.25).abs() < 1e-9);
        assert_eq!(m.error_samples, vec![0.0, 0.0, 1.0, 0.0]);
        assert_eq!(m.outcome_samples, vec![1.0, 1.0, 0.25, 1.0]);
    }

    #[test]
    fn fleet_avg_excludes_zero_rate_sessions() {
        assert!((fleet_avg_tok_s_from_sessions(&[]) - 0.0).abs() < 1e-9);
        let sessions = vec![
            Session {
                id: "cold".into(),
                ..Default::default()
            },
            Session {
                id: "hot".into(),
                output_tokens: 100,
                sample_seconds: 4.0, // 25 tok/s
                ..Default::default()
            },
        ];
        assert!((fleet_avg_tok_s_from_sessions(&sessions) - 25.0).abs() < 1e-9);
    }

    #[test]
    fn metrics_uses_weighted_cache_ratio() {
        let snap = Snapshot {
            input_tokens: 1_010,
            cached_input_tokens: 900,
            usage_requests: 2,
            ..Default::default()
        };
        let m = Metrics::from_snapshot(&snap);
        assert_eq!(m.cached_input_tokens, 900);
        assert!((m.cache_read_ratio.unwrap() - 900.0 / 1_010.0).abs() < 1e-12);
        assert_eq!(m.cache_cell_value(), "900·89%");
    }

    #[test]
    fn metrics_cache_cell_shows_absolute_zero_reads() {
        let snap = Snapshot {
            input_tokens: 500,
            cached_input_tokens: 0,
            usage_requests: 3,
            ..Default::default()
        };
        let m = Metrics::from_snapshot(&snap);
        assert_eq!(m.cached_input_tokens, 0);
        assert!((m.cache_read_ratio.unwrap() - 0.0).abs() < 1e-12);
        let cell = m.cache_cell_value();
        assert!(
            cell.starts_with('0'),
            "zero cache reads must show absolute 0, got {cell}"
        );
        assert_eq!(cell, "0·0%");
    }

    #[test]
    fn metrics_cache_cell_n_a_without_usage_observations() {
        let m = Metrics::from_snapshot(&Snapshot::default());
        assert_eq!(m.cached_input_tokens, 0);
        assert_eq!(m.cache_read_ratio, None);
        assert_eq!(m.cache_cell_value(), "n/a");
    }

    #[test]
    fn format_token_count_compacts_large_values() {
        assert_eq!(format_token_count(0), "0");
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(1_234), "1.2k");
        assert_eq!(format_token_count(12_300), "12.3k");
        assert_eq!(format_token_count(2_500_000), "2.5M");
        assert_eq!(format_cache_read_value(12_300, Some(0.91)), "12.3k·91%");
    }

    #[test]
    fn format_token_count_promotes_at_unit_boundaries() {
        // One-decimal k that would print 1000.0k must become 1.0M (shorter, not longer).
        assert_eq!(format_token_count(999_949), "999.9k");
        assert_eq!(format_token_count(999_950), "1.0M");
        assert_eq!(format_token_count(999_999), "1.0M");
        assert_eq!(format_token_count(1_000_000), "1.0M");
        // Same promotion at the M → G boundary.
        assert_eq!(format_token_count(999_949_999), "999.9M");
        assert_eq!(format_token_count(999_950_000), "1.0G");
        assert_eq!(format_token_count(1_000_000_000), "1.0G");
        // Compact cache cell stays short at the awkward boundary.
        assert_eq!(format_cache_read_value(999_999, Some(0.5)), "1.0M·50%");
        assert!(
            display_width(&format_token_count(999_999)) < display_width("1000.0k"),
            "promoted form must be shorter than unpromoted 1000.0k"
        );
    }

    #[test]
    fn display_width_counts_columns_not_bytes() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("·"), 1);
        assert_eq!("·".len(), 2, "middle dot is multi-byte UTF-8");
        assert_eq!(display_width("900·89%"), 7);
        assert!(
            display_width("900·89%") < "900·89%".len(),
            "display width must not use byte length for spark budget"
        );
    }

    #[test]
    fn cache_cell_fits_minimum_64_col_strip_column() {
        // Documented minimum: outer width 64 → bordered inner 62 → cache column 38% ≈ 23.
        // Guarantee label + value (with trailing space) still fit without clipping.
        let cases = [
            format_cache_read_value(900, Some(0.89)),
            format_cache_read_value(0, Some(0.0)),
            format_cache_read_value(12_300, Some(0.91)),
            format_cache_read_value(999_999, Some(1.0)),
            format_cache_read_value(0, None),
        ];
        let cache_col = (62.0_f64 * 0.38).floor() as usize; // matches Percentage(38) floor share
        for value in cases {
            let label = format!(" {} ", "cache");
            let painted = format!("{value} ");
            let needed = display_width(&label) + display_width(&painted);
            assert!(
                needed <= cache_col,
                "cache cell {value:?} needs {needed} cols > cache column {cache_col}"
            );
        }
    }

    #[test]
    fn should_show_metrics_thresholds() {
        assert!(!should_show_metrics(63, 40));
        assert!(!should_show_metrics(80, 17));
        assert!(should_show_metrics(80, 18));
        assert!(should_show_metrics(64, 24));
    }
}
