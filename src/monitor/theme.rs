//! abtop-inspired palette for the serve monitor (single default theme).
//!
//! Uses portable named ANSI colors so 16-/256-color terminals (SSH, older
//! Terminal profiles, multiplexers) stay readable without truecolor.

use ratatui::style::{Color, Modifier, Style};

/// Single default palette for v1 (no `t` cycle unless a second theme is added).
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
            border: Style::default().fg(Color::DarkGray),
            title: Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            header: Style::default().fg(Color::White),
            footer: Style::default().fg(Color::DarkGray),
            // DarkGray bg is portable; bold white keeps selection legible.
            selected: Style::default()
                .bg(Color::DarkGray)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            active: Style::default().fg(Color::Cyan),
            ok: Style::default().fg(Color::Green),
            fail: Style::default().fg(Color::Red),
            muted: Style::default().fg(Color::DarkGray),
            highlight: Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            auth: Style::default().fg(Color::Yellow),
            stream: Style::default().fg(Color::Magenta),
            // LightRed stands out from plain Red fail rows for ProxyAssemble.
            assemble: Style::default().fg(Color::LightRed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_uses_named_ansi() {
        let t = Theme::default();
        assert_eq!(t.selected.bg, Some(Color::DarkGray));
        assert_eq!(t.ok.fg, Some(Color::Green));
        assert_eq!(t.fail.fg, Some(Color::Red));
        assert_eq!(t.title.fg, Some(Color::Cyan));
        assert_eq!(t.assemble.fg, Some(Color::LightRed));
    }
}
