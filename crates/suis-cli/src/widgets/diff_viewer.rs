//! Rendering a unified diff with colored add/remove/context lines.
//!
//! The agent's `edit` tool returns a unified-style diff (produced by
//! `suis_agent::diff::unified`) as part of its result. This widget classifies
//! each line and renders it: removals red, additions green, file headers dim,
//! context default. The classifier is pure and unit-tested.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme;

/// The kind of a single diff line, which determines its color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    /// A `--- ` / `+++ ` file header.
    Header,
    /// An added line (`+`).
    Added,
    /// A removed line (`-`).
    Removed,
    /// A `⋯ N unchanged lines` marker standing in for elided context.
    Gap,
    /// An unchanged context line.
    Context,
}

impl DiffLineKind {
    /// Classify one line of unified-diff text.
    pub fn classify(line: &str) -> Self {
        if line.starts_with("+++") || line.starts_with("---") {
            DiffLineKind::Header
        } else if line.starts_with('+') {
            DiffLineKind::Added
        } else if line.starts_with('-') {
            DiffLineKind::Removed
        } else if line.starts_with('⋯') {
            DiffLineKind::Gap
        } else {
            DiffLineKind::Context
        }
    }

    /// The ratatui style for this line kind.
    pub fn style(self) -> Style {
        match self {
            DiffLineKind::Header => Style::default()
                .fg(theme::TEXT_FAINT)
                .add_modifier(Modifier::BOLD),
            DiffLineKind::Added => Style::default().fg(theme::ACCENT),
            DiffLineKind::Removed => Style::default().fg(theme::DANGER),
            DiffLineKind::Gap => Style::default()
                .fg(theme::TEXT_FAINT)
                .add_modifier(Modifier::ITALIC),
            DiffLineKind::Context => Style::default().fg(theme::TEXT_DIM),
        }
    }
}

/// Convert unified-diff text into styled ratatui lines.
pub fn styled_lines(diff: &str) -> Vec<Line<'static>> {
    diff.lines()
        .map(|line| {
            let kind = DiffLineKind::classify(line);
            Line::from(Span::styled(line.to_string(), kind.style()))
        })
        .collect()
}

/// Count the added and removed lines in a unified diff (excluding the `---`/
/// `+++` headers). Useful for a one-line summary.
pub fn change_counts(diff: &str) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for line in diff.lines() {
        match DiffLineKind::classify(line) {
            DiffLineKind::Added => added += 1,
            DiffLineKind::Removed => removed += 1,
            _ => {}
        }
    }
    (added, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "--- src/main.rs\n\
                          +++ src/main.rs\n\
                          -let x = 1;\n\
                          +let x = 2;\n\
                           let y = 3;\n";

    #[test]
    fn classifies_each_line_kind() {
        assert_eq!(DiffLineKind::classify("--- a"), DiffLineKind::Header);
        assert_eq!(DiffLineKind::classify("+++ a"), DiffLineKind::Header);
        assert_eq!(DiffLineKind::classify("+added"), DiffLineKind::Added);
        assert_eq!(DiffLineKind::classify("-removed"), DiffLineKind::Removed);
        assert_eq!(
            DiffLineKind::classify("⋯ 380 unchanged lines"),
            DiffLineKind::Gap
        );
        assert_eq!(DiffLineKind::classify(" context"), DiffLineKind::Context);
    }

    #[test]
    fn header_takes_precedence_over_plus_minus() {
        // A `---`/`+++` header must not be mistaken for a remove/add line.
        assert_eq!(DiffLineKind::classify("--- file"), DiffLineKind::Header);
        assert_eq!(DiffLineKind::classify("+++ file"), DiffLineKind::Header);
    }

    #[test]
    fn styled_lines_covers_every_input_line() {
        let lines = styled_lines(SAMPLE);
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn change_counts_ignores_headers() {
        let (added, removed) = change_counts(SAMPLE);
        assert_eq!(added, 1);
        assert_eq!(removed, 1);
    }
}
