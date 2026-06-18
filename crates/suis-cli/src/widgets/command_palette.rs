//! The live slash-command palette.
//!
//! A popup anchored above the input box listing every command matching the
//! typed prefix — name in accent, description dim, the selected row in the
//! shared bright-row style. Suggestion-only chrome: key handling lives in the
//! input layer, and accepting a row goes through the same submit path as a
//! blind-typed command.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};
use ratatui::Frame;

use crate::theme;

/// The widest the popup grows; enough for the longest name + description.
const MAX_WIDTH: u16 = 72;

/// Render the palette over the bottom edge of `above` (the transcript area),
/// listing `matches` with `selected` highlighted.
pub fn render(
    frame: &mut Frame,
    above: Rect,
    matches: &[(&'static str, &'static str)],
    selected: usize,
) {
    if matches.is_empty() || above.height == 0 {
        return;
    }
    let height = (matches.len() as u16 + 2).min(above.height);
    let popup = Rect {
        x: above.x,
        y: above.y + above.height - height,
        width: above.width.min(MAX_WIDTH),
        height,
    };
    frame.render_widget(Clear, popup);

    let selected = selected.min(matches.len() - 1);
    let name_width = matches
        .iter()
        .map(|(name, _)| name.len())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = matches
        .iter()
        .enumerate()
        .map(|(i, (name, description))| {
            let is_selected = i == selected;
            let marker = if is_selected { "> " } else { "  " };
            let name_style = if is_selected {
                Style::default()
                    .fg(theme::TEXT_BRIGHT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::ACCENT)
            };
            Line::from(vec![
                Span::styled(marker, name_style),
                Span::styled(format!("/{name:<name_width$}"), name_style),
                Span::styled("  ", Style::default()),
                Span::styled(*description, Style::default().fg(theme::TEXT_DIM)),
            ])
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(Style::default().bg(theme::BG))
        .padding(Padding::horizontal(1));
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}
