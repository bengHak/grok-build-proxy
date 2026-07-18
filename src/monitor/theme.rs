//! abtop-inspired palette for the serve monitor (single default theme).

use ratatui::style::{Color, Modifier, Style};

/// Terminal-safe cyan / amber / rose accents on a dark base.
/// Kept as one palette for v1 (no `t` cycle unless a second theme is added later).
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub border: Style,
    pub title: Style,
    pub header: Style,
    pub footer: Style,
    pub selected: Style,
    pub active: Style,
    pub ok: Style,
    pub fail: Style,
    pub muted: Style,
    pub highlight: Style,
    /// AuthRetryFailed rows.
    pub auth: Style,
    /// StreamIo / StreamTerminalFailed rows.
    pub stream: Style,
    /// ProxyAssemble rows.
    pub assemble: Style,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            // Soft slate border — less harsh than pure DarkGray on dark terminals.
            border: Style::default().fg(Color::Rgb(90, 98, 120)),
            title: Style::default()
                .fg(Color::Rgb(120, 210, 230))
                .add_modifier(Modifier::BOLD),
            header: Style::default().fg(Color::Rgb(220, 225, 235)),
            footer: Style::default().fg(Color::Rgb(110, 118, 140)),
            selected: Style::default()
                .bg(Color::Rgb(45, 55, 85))
                .fg(Color::Rgb(240, 244, 255))
                .add_modifier(Modifier::BOLD),
            active: Style::default().fg(Color::Rgb(90, 200, 220)),
            ok: Style::default().fg(Color::Rgb(110, 210, 140)),
            fail: Style::default().fg(Color::Rgb(230, 100, 110)),
            muted: Style::default().fg(Color::Rgb(100, 108, 128)),
            highlight: Style::default()
                .fg(Color::Rgb(240, 200, 90))
                .add_modifier(Modifier::BOLD),
            auth: Style::default().fg(Color::Rgb(240, 190, 80)),
            stream: Style::default().fg(Color::Rgb(190, 140, 230)),
            assemble: Style::default().fg(Color::Rgb(240, 140, 120)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_selected_has_dark_bg() {
        let t = Theme::default();
        assert_eq!(t.selected.bg, Some(Color::Rgb(45, 55, 85)));
        assert_eq!(t.ok.fg, Some(Color::Rgb(110, 210, 140)));
        assert_eq!(t.fail.fg, Some(Color::Rgb(230, 100, 110)));
    }
}
