//! Metrics mid-strip: tok/s, error rate, and completed activity sparklines.

use crate::monitor::theme::Theme;
use crate::store::Snapshot;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

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
        let spark_w = spark_width(inner.width);

        // Three equal columns on a single content row (bordered strip is height 3).
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(34),
                Constraint::Percentage(33),
                Constraint::Percentage(33),
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
                spark: sparkline_chars(&tok.tok_samples, spark_w),
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
                spark: sparkline_chars(&tok.error_samples, spark_w),
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
                spark: sparkline_chars(&tok.activity_samples, spark_w),
                value_style: self.theme.active,
                muted: self.theme.muted,
                spark_style: self.theme.header,
            },
        );
    }
}

struct MetricCell {
    label: &'static str,
    value: String,
    spark: String,
    value_style: ratatui::style::Style,
    muted: ratatui::style::Style,
    spark_style: ratatui::style::Style,
}

fn render_metric(area: Rect, buf: &mut Buffer, cell: MetricCell) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let line = Line::from(vec![
        Span::styled(format!(" {} ", cell.label), cell.muted),
        Span::styled(format!("{} ", cell.value), cell.value_style),
        Span::styled(cell.spark, cell.spark_style),
    ]);
    Paragraph::new(line).render(area, buf);
}

/// Derived view of store metric rings for the strip.
#[derive(Clone, Debug, Default)]
pub struct Metrics {
    pub avg_tok_s: f64,
    pub error_rate: f64,
    pub completed_ok: usize,
    pub completed_fail: usize,
    pub tok_samples: Vec<f64>,
    /// Per-sample error (1.0 fail / 0.0 ok) for the err sparkline.
    pub error_samples: Vec<f64>,
    /// Activity: 1.0 completed ok, 0.25 fail (visible blip), for done sparkline.
    pub activity_samples: Vec<f64>,
}

impl Metrics {
    pub fn from_snapshot(snapshot: &Snapshot) -> Self {
        let tok_samples = snapshot.metrics_tok_per_s.clone();
        let avg_tok_s = if tok_samples.is_empty() {
            0.0
        } else {
            tok_samples.iter().sum::<f64>() / tok_samples.len() as f64
        };

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
        // Keep fails visible on the activity line (0 would collapse to baseline).
        let activity_samples: Vec<f64> = completed
            .iter()
            .map(|&v| if v >= 0.5 { 1.0 } else { 0.25 })
            .collect();

        Self {
            avg_tok_s,
            error_rate,
            completed_ok,
            completed_fail,
            tok_samples,
            error_samples,
            activity_samples,
        }
    }
}

/// One-row block sparkline. Pads on the left when fewer samples than `width`.
pub fn sparkline_chars(values: &[f64], width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if values.is_empty() {
        // Same length as a data series so cold-start columns do not jump width.
        return "·".repeat(width);
    }
    let start = values.len().saturating_sub(width);
    let slice = &values[start..];
    let max = slice
        .iter()
        .copied()
        .fold(0.0_f64, f64::max)
        .max(f64::EPSILON);

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

/// Spark width scales with column width; keep a usable default for narrow cols.
fn spark_width(total_inner: u16) -> usize {
    // Three columns; leave room for " tok/s 12.3 " label (~12 chars).
    let per_col = (total_inner as usize / 3).saturating_sub(12);
    per_col.clamp(4, 24)
}

/// Whether the metrics strip should be drawn for this frame size.
pub fn should_show_metrics(area_width: u16, area_height: u16) -> bool {
    // Full dashboard needs header(3)+metrics(3)+body(min 6)+footer(3) ≈ 15;
    // require a bit more so body panels stay usable.
    area_width >= 40 && area_height >= 18
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
            metrics_tok_per_s: vec![10.0, 20.0, 30.0],
            metrics_completed: vec![1.0, 1.0, 0.0, 1.0], // 1 fail / 4
            ..Default::default()
        };
        let m = Metrics::from_snapshot(&snap);
        assert!((m.avg_tok_s - 20.0).abs() < 1e-9);
        assert_eq!(m.completed_ok, 3);
        assert_eq!(m.completed_fail, 1);
        assert!((m.error_rate - 0.25).abs() < 1e-9);
        assert_eq!(m.error_samples, vec![0.0, 0.0, 1.0, 0.0]);
        assert_eq!(m.activity_samples, vec![1.0, 1.0, 0.25, 1.0]);
    }

    #[test]
    fn should_show_metrics_thresholds() {
        assert!(!should_show_metrics(39, 40));
        assert!(!should_show_metrics(80, 17));
        assert!(should_show_metrics(80, 18));
        assert!(should_show_metrics(40, 24));
    }
}
