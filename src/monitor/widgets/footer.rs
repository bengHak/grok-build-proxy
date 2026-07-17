//! Bottom keybinding bar.

use crate::monitor::theme::Theme;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

pub struct Footer {
    pub theme: Theme,
}

impl Widget for Footer {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let line = Line::from(vec![
            Span::styled(" j/k ", self.theme.highlight),
            Span::styled("move ", self.theme.footer),
            Span::styled(" Tab ", self.theme.highlight),
            Span::styled("panel ", self.theme.footer),
            Span::styled(" Enter ", self.theme.highlight),
            Span::styled("detail ", self.theme.footer),
            Span::styled(" f ", self.theme.highlight),
            Span::styled("filter ", self.theme.footer),
            Span::styled(" ? ", self.theme.highlight),
            Span::styled("help ", self.theme.footer),
            Span::styled(" Esc ", self.theme.highlight),
            Span::styled("back ", self.theme.footer),
            Span::styled(" q ", self.theme.highlight),
            Span::styled("quit", self.theme.footer),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(Span::styled(" keys ", self.theme.title));

        Paragraph::new(line).block(block).render(area, buf);
    }
}
