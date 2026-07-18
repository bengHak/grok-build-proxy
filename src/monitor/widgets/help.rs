//! Help overlay listing key bindings.

use crate::monitor::theme::Theme;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

pub struct HelpOverlay {
    pub theme: Theme,
}

impl Widget for HelpOverlay {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Center a modal within the full frame.
        let width = area.width.min(66);
        let height = area.height.min(20);
        let x = area.x + (area.width.saturating_sub(width)) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 2;
        let modal = Rect {
            x,
            y,
            width,
            height,
        };

        Clear.render(modal, buf);

        let lines = vec![
            Line::from(Span::styled("Monitor help", self.theme.highlight)),
            Line::from(""),
            Line::from("  ↑/k  ↓/j     move selection"),
            Line::from("  Tab/Shift-Tab cycle panels (sessions → active → failures)"),
            Line::from("  Enter        open details"),
            Line::from("  f            cycle failure filter"),
            Line::from("               All / ProxyAssemble / Upstream / Auth / Stream"),
            Line::from("  y            copy filtered failure report (markdown)"),
            Line::from("  Y            copy filtered failure report (JSON)"),
            Line::from("  w            write report → ~/.grok/proxy-reports/*.md"),
            Line::from("  W            write report → ~/.grok/proxy-reports/*.json"),
            Line::from("  Esc/Backspace return"),
            Line::from("  ?            toggle help"),
            Line::from("  q/Q / Ctrl-C stop proxy"),
            Line::from(""),
            Line::from(Span::styled(
                "  Same session, consecutive fails ≤30s apart: estimated retry",
                self.theme.muted,
            )),
        ];

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.active)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(Span::styled(" help ", self.theme.title));

        Paragraph::new(lines).block(block).render(modal, buf);
    }
}
