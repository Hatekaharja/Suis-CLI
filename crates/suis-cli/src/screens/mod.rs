//! Full-screen views. Each screen owns its layout and delegates pieces to
//! [`crate::widgets`]. The active screen is chosen by
//! [`AppState::screen`](crate::app::state::Screen).

pub mod chat;
pub mod diff_screen;
pub mod model_select;
pub mod permissions;
pub mod plan_select;
pub mod project_init;
pub mod provider_form;
pub mod providers;

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};
use ratatui::Frame;

use crate::theme;
use crate::widgets::footer;

/// Render the fatal-error screen (e.g. no providers discovered).
pub fn render_error(frame: &mut Frame, area: Rect, message: &str) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let lines = vec![
        Line::from(Span::styled(
            "Suis cannot start",
            Style::default()
                .fg(theme::DANGER)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(theme::TEXT),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Start a local provider (e.g. `ollama serve`) and relaunch.",
            Style::default().fg(theme::TEXT_FAINT),
        )),
    ];
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .padding(Padding::horizontal(1));
    let paragraph = Paragraph::new(lines)
        .block(block)
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, rows[0]);

    footer::render(frame, rows[1], &[("q/Esc", "quit")]);
}
