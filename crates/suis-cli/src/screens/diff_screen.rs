//! Full-screen diff viewer with approve/reject/skip controls.
//!
//! The `edit` tool returns a unified diff in its result, which the chat screen
//! colors inline (see [`crate::widgets::diff_viewer`]). This screen presents a
//! single edit's diff on its own with explicit controls.
//!
//! Note on wiring: the current agent applies edits inside the tool call (the
//! write happens before the result is returned), so there is no pre-write hook
//! for the UI to gate. The decision mapping and renderer here are complete and
//! unit-tested, ready to drive interactive apply/reject once the agent emits a
//! diff-approval event (Project 5). Until then the viewer is display-only.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};
use ratatui::Frame;

use crate::theme;
use crate::widgets::{diff_viewer, footer};

/// What the user chose to do with a proposed edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffDecision {
    /// Apply the edit and continue.
    Apply,
    /// Reject the edit; the agent is told it was not applied.
    Reject,
    /// Skip this file but continue with any others.
    Skip,
}

impl DiffDecision {
    /// Map a keypress to a decision (`a`/`r`/`s`, case-insensitive).
    pub fn from_key(key: char) -> Option<Self> {
        match key.to_ascii_lowercase() {
            'a' => Some(DiffDecision::Apply),
            'r' => Some(DiffDecision::Reject),
            's' => Some(DiffDecision::Skip),
            _ => None,
        }
    }
}

/// A diff awaiting a decision: the file label and the unified-diff text.
#[derive(Debug, Clone)]
pub struct DiffView {
    /// File path / label shown in the header.
    pub label: String,
    /// The unified-diff body.
    pub diff: String,
}

/// Render the diff screen.
pub fn render(frame: &mut Frame, area: Rect, view: &DiffView) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // diff body
            Constraint::Length(1), // footer
        ])
        .split(area);

    let (added, removed) = diff_viewer::change_counts(&view.diff);
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            format!("Edit: {}", view.label),
            Style::default()
                .fg(theme::INFO)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  (+{added} -{removed})"),
            Style::default().fg(theme::TEXT_FAINT),
        ),
    ]));
    frame.render_widget(header, rows[0]);

    let body = Paragraph::new(diff_viewer::styled_lines(&view.diff)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::BORDER))
            .padding(Padding::horizontal(1)),
    );
    frame.render_widget(body, rows[1]);

    footer::render(
        frame,
        rows[2],
        &[
            ("A", "apply"),
            ("R", "reject"),
            ("S", "skip"),
            ("Esc", "back"),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_keys_to_decisions() {
        assert_eq!(DiffDecision::from_key('a'), Some(DiffDecision::Apply));
        assert_eq!(DiffDecision::from_key('A'), Some(DiffDecision::Apply));
        assert_eq!(DiffDecision::from_key('r'), Some(DiffDecision::Reject));
        assert_eq!(DiffDecision::from_key('s'), Some(DiffDecision::Skip));
        assert_eq!(DiffDecision::from_key('x'), None);
    }
}
