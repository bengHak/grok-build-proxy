//! Responsive column density for session-centric list rows (no global Active/Recent panes).

/// Terminal width tiers for collapsing list columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutTier {
    /// < 80 cols: emergency / narrow single-panel mode (caller handles).
    Emergency,
    /// 80–99: compact rows.
    Narrow,
    /// 100–129: medium density.
    Medium,
    /// ≥ 130: expanded columns.
    Wide,
}

impl LayoutTier {
    pub fn for_width(width: u16) -> Self {
        match width {
            0..=79 => Self::Emergency,
            80..=99 => Self::Narrow,
            100..=129 => Self::Medium,
            _ => Self::Wide,
        }
    }

    pub fn session_id_width(self) -> usize {
        match self {
            Self::Emergency | Self::Narrow => 12,
            Self::Medium => 16,
            Self::Wide => 20,
        }
    }

    pub fn model_width(self) -> usize {
        match self {
            Self::Emergency => 10,
            Self::Narrow => 12,
            Self::Medium => 16,
            Self::Wide => 22,
        }
    }

    pub fn show_provider(self) -> bool {
        !matches!(self, Self::Emergency)
    }

    pub fn failure_error_width(self) -> usize {
        match self {
            Self::Emergency => 12,
            Self::Narrow => 18,
            Self::Medium => 28,
            Self::Wide => 40,
        }
    }
}

/// Unicode block sparkline for per-session output token buckets.
pub fn token_bucket_sparkline(buckets: &[u64], width: usize) -> String {
    if width == 0 || buckets.is_empty() {
        return String::new();
    }
    const LEVELS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = buckets.iter().copied().max().unwrap_or(0).max(1);
    let take = buckets.len().min(width);
    let start = buckets.len().saturating_sub(take);
    buckets[start..]
        .iter()
        .map(|&v| {
            let idx = ((v as f64 / max as f64) * (LEVELS.len() - 1) as f64).round() as usize;
            LEVELS[idx.min(LEVELS.len() - 1)]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_cover_common_widths() {
        assert_eq!(LayoutTier::for_width(60), LayoutTier::Emergency);
        assert_eq!(LayoutTier::for_width(90), LayoutTier::Narrow);
        assert_eq!(LayoutTier::for_width(110), LayoutTier::Medium);
        assert_eq!(LayoutTier::for_width(140), LayoutTier::Wide);
    }

    #[test]
    fn sparkline_renders_blocks() {
        let s = token_bucket_sparkline(&[0, 10, 50, 100], 4);
        assert_eq!(s.chars().count(), 4);
        assert!(s.contains('█') || s.contains('▇'));
    }
}
