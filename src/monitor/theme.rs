//! abtop-inspired palette for the serve monitor (single default theme).

use ratatui::style::{Color, Modifier, Style};

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
            assemble: Style::default().fg(Color::LightRed),
        }
    }
}
