//! A transient, dismissible advisory popup.
//!
//! Unlike the permission and confirm dialogs this carries no choices: it
//! surfaces a single advisory message (e.g. an API key being sent over plaintext
//! http) that the user acknowledges with any key. The message is owned by
//! [`AppState::notice`](crate::app::state::AppState); rendering is stateless.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};
use ratatui::Frame;

use crate::theme;
use crate::widgets::permission_prompt::centered_rect;

/// Render `message` as a centered, bordered warning box over `area`.
pub fn render(frame: &mut Frame, area: Rect, message: &str) {
    let popup = centered_rect(60, 40, area);
    frame.render_widget(Clear, popup);

    let lines = vec![
        Line::from(Span::styled(
            "Warning",
            Style::default()
                .fg(theme::WARN)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(theme::TEXT_BRIGHT),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "press any key to dismiss",
            Style::default().fg(theme::TEXT_FAINT),
        )),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::WARN))
        .style(Style::default().bg(theme::BG))
        .padding(Padding::horizontal(1));
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        popup,
    );
}
