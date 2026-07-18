//! Help overlay listing key bindings.

use crate::monitor::theme::Theme;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};

pub struct HelpOverlay {
    pub theme: Theme,
}

impl Widget for HelpOverlay {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Center a modal within the full frame.
        let width = area.width.min(72);
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
            Line::from("  Tab/Shift-Tab cycle panels"),
            Line::from("               sessions → session detail → failures"),
            Line::from("  Enter        open details"),
            Line::from("  f            cycle failure filter"),
            Line::from("               All / ProxyAssemble / Upstream / Auth / Stream"),
            Line::from("  y            copy markdown report"),
            Line::from("  Y            copy JSON report"),
            Line::from("  w            write markdown report"),
            Line::from("  W            write JSON report"),
            Line::from("  Esc/Backspace return"),
            Line::from("  ?            toggle help"),
            Line::from("  q/Q / Ctrl-C stop proxy"),
            Line::from(Span::styled(
                "  ≤30s same-session failures: estimated retry",
                self.theme.muted,
            )),
        ];

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.active)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .title(Span::styled(" help ", self.theme.title));

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block)
            .render(modal, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn render(width: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal
            .draw(|frame| {
                HelpOverlay {
                    theme: Theme::default(),
                }
                .render(frame.area(), frame.buffer_mut())
            })
            .unwrap();
        let mut text = String::new();
        for y in 0..24 {
            for x in 0..width {
                text.push_str(terminal.backend().buffer().cell((x, y)).unwrap().symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn panel_order_is_visible_at_common_terminal_width() {
        let text = render(100);
        assert!(
            text.contains("sessions → session detail → failures"),
            "{text}"
        );
    }

    #[test]
    fn long_help_lines_wrap_on_narrow_terminals() {
        let text = render(50);
        for expected in ["Shift-Tab", "failures", "JSON", "estimated retry"] {
            assert!(text.contains(expected), "missing {expected}:\n{text}");
        }
    }
}
