//! Ratatui panel widgets for the serve monitor.

mod failures;
mod footer;
mod header;
mod help;
mod metrics;
mod session_detail;
mod sessions;

pub use failures::FailuresPanel;
pub use footer::Footer;
pub use header::Header;
pub use help::HelpOverlay;
pub use metrics::{MetricsStrip, fleet_avg_tok_s, should_show_metrics};
pub use session_detail::{SessionDetailPanel, TurnKind};
pub use sessions::{SessionsPanel, active_sessions};

/// Truncate a display string to `max` chars, appending `…` when needed.
pub(crate) fn truncate(s: &str, max: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn truncate_short_and_long() {
        assert_eq!(truncate("hi", 4), "hi");
        assert_eq!(truncate("hello", 4), "hel…");
        assert_eq!(truncate("x", 1), "x");
        assert_eq!(truncate("xy", 1), "…");
    }
}
