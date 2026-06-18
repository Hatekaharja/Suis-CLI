//! A small centered yes/no confirmation overlay, shared by the `/implement`
//! confirmation and the begin-verification prompt. Rendering only — the key
//! handling lives in the input layer, and the keys themselves are taught by
//! the chat footer rather than repeated in the box.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};
use ratatui::Frame;

use crate::theme;
use crate::widgets::permission_prompt::centered_rect;

/// Render a confirmation box with a bold `title` and a `body` line.
pub fn render(frame: &mut Frame, area: Rect, title: &str, body: &str) {
    let popup = centered_rect(60, 25, area);
    frame.render_widget(Clear, popup);

    let lines = vec![
        Line::from(Span::styled(
            title.to_string(),
            Style::default()
                .fg(theme::WARN)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            body.to_string(),
            Style::default().fg(theme::TEXT),
        )),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::WARN))
        .style(Style::default().bg(theme::BG))
        .padding(Padding::horizontal(1));
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}
