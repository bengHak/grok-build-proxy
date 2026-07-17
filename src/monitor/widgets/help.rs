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
        let width = area.width.min(56);
        let height = area.height.min(14);
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
            Line::from("  Tab          switch panel (sessions ↔ active)"),
            Line::from("  Enter        open details"),
            Line::from("  Esc/Backspace return"),
            Line::from("  ?            toggle help"),
            Line::from("  q / Ctrl-C   stop proxy"),
            Line::from(""),
            Line::from(Span::styled(
                "  (failures panel & report export in later PRs)",
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
