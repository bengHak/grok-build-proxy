//! Top status bar: version, listen address, uptime, active, errors, tok/s.

use crate::monitor::theme::Theme;
use crate::monitor::widgets::metrics::fleet_avg_tok_s;
use crate::store::Snapshot;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

pub struct Header<'a> {
    pub snapshot: &'a Snapshot,
    pub address: &'a str,
    pub version: &'a str,
    pub uptime_secs: u64,
    pub theme: Theme,
}

impl Widget for Header<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let active = self.snapshot.active.len();
        // Canonical failure ring (cap 200); prefer over legacy `errors` (cap 50).
        let errors = self.snapshot.failures.len();
        let tok_s = fleet_avg_tok_s(self.snapshot);
        let uptime = format_uptime(self.uptime_secs);

        let line = Line::from(vec![
            Span::styled(
                format!(" grok-build-proxy v{} ", self.version),
                self.theme.highlight,
            ),
            Span::styled(format!(" {} ", self.address), self.theme.header),
            Span::styled(format!(" up {uptime} "), self.theme.muted),
            Span::styled(format!(" active↑{active} "), self.theme.active),
            Span::styled(format!(" err●{errors} "), self.theme.fail),
            Span::styled(format!(" {tok_s:.1} tok/s "), self.theme.ok),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(Span::styled(" header ", self.theme.title));

        Paragraph::new(line)
            .style(Style::default())
            .block(block)
            .render(area, buf);
    }
}

fn format_uptime(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::format_uptime;

    #[test]
    fn uptime_formats() {
        assert_eq!(format_uptime(5), "5s");
        assert_eq!(format_uptime(65), "1m05s");
        assert_eq!(format_uptime(3661), "1h01m");
    }
}
