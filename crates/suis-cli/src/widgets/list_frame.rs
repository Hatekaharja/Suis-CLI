//! The shared styling grammar for the full-screen list views.
//!
//! Model select, providers, and plan select all draw the same surface: a
//! one-line title, a bordered list with a `> ` cursor over a bright selected
//! row, accent count badges, and a faint parenthesised empty state. This
//! module is that grammar in one place, so the three screens are visually
//! interchangeable.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};
use ratatui::Frame;

use crate::theme;

/// The bordered, padded block every full-screen list sits in.
pub fn block() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .padding(Padding::horizontal(1))
}

/// Render the one-line screen title above a list.
pub fn render_title(frame: &mut Frame, area: Rect, title: &str) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title.to_string(),
            Style::default()
                .fg(theme::INFO)
                .add_modifier(Modifier::BOLD),
        ))),
        area,
    );
}

/// The `(style, marker)` for a list row: bright and bold behind a `> ` cursor
/// when selected, dim behind aligning spaces otherwise.
pub fn row_style(selected: bool) -> (Style, &'static str) {
    if selected {
        (
            Style::default()
                .fg(theme::TEXT_BRIGHT)
                .add_modifier(Modifier::BOLD),
            "> ",
        )
    } else {
        (Style::default().fg(theme::TEXT_DIM), "  ")
    }
}

/// A count badge like `[3/5 steps]`, in the shared accent.
pub fn badge(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(theme::ACCENT))
}

/// The one-line empty-state row: `  (text)`, faint.
pub fn empty_line(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("  ({text})"),
        Style::default().fg(theme::TEXT_FAINT),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_row_is_bright_bold_behind_the_cursor() {
        let (style, marker) = row_style(true);
        assert_eq!(marker, "> ");
        assert_eq!(style.fg, Some(theme::TEXT_BRIGHT));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn unselected_row_is_dim_and_aligned() {
        let (style, marker) = row_style(false);
        assert_eq!(marker, "  ");
        assert_eq!(style.fg, Some(theme::TEXT_DIM));
    }

    #[test]
    fn badges_use_the_accent() {
        assert_eq!(badge("[3/5 steps]").style.fg, Some(theme::ACCENT));
    }

    #[test]
    fn empty_state_is_faint_and_parenthesised() {
        let line = empty_line("no plans");
        assert_eq!(line.spans[0].content, "  (no plans)");
        assert_eq!(line.spans[0].style.fg, Some(theme::TEXT_FAINT));
    }
}
