//! The one-row, context-sensitive key-hint footer.
//!
//! Every screen reserves its bottom row for this widget and passes the keys
//! that currently apply as `(key, label)` pairs — so the hints are stated
//! once, in one place, in one style, and always tell the truth for the active
//! screen or overlay. A pair with an empty key renders as a plain note.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::theme;

/// Build the footer line for `hints`, dropping whole pairs from the right when
/// `width` is too narrow — the footer never wraps to a second row.
pub fn line(hints: &[(&str, &str)], width: u16) -> Line<'static> {
    let width = width as usize;
    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    let mut used = 1usize;
    let mut first = true;
    for (key, label) in hints {
        let sep = if first { 0 } else { 3 }; // " · "
        let pair = key.chars().count()
            + label.chars().count()
            + if key.is_empty() || label.is_empty() {
                0
            } else {
                1
            };
        if used + sep + pair > width {
            break;
        }
        used += sep + pair;
        if !first {
            spans.push(Span::styled(" · ", Style::default().fg(theme::TEXT_FAINT)));
        }
        first = false;
        if !key.is_empty() {
            spans.push(Span::styled(
                key.to_string(),
                Style::default()
                    .fg(theme::TEXT_DIM)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if !label.is_empty() {
            let label = if key.is_empty() {
                label.to_string()
            } else {
                format!(" {label}")
            };
            spans.push(Span::styled(label, Style::default().fg(theme::TEXT_FAINT)));
        }
    }
    Line::from(spans)
}

/// Render the footer into `area` (expected to be one row high).
pub fn render(frame: &mut Frame, area: Rect, hints: &[(&str, &str)]) {
    frame.render_widget(Paragraph::new(line(hints, area.width)), area);
}

/// Columns of hint space to preserve when also showing the session usage; below
/// this the usage is dropped so the hints keep their room (the terminal is
/// "too narrow" for both).
const MIN_HINT_COLS: u16 = 24;

/// Render the key-hint footer plus — when `usage` is set and the input column
/// is wide enough — the active provider's running session token total. The
/// total is anchored at the right edge of the input column (`col_width` from
/// the footer's left), so it sits below the input box and left of any docked
/// task panel. A blank trailing column keeps it off the panel border.
pub fn render_with_usage(
    frame: &mut Frame,
    area: Rect,
    hints: &[(&str, &str)],
    col_width: u16,
    usage: Option<usize>,
) {
    let label = usage.map(session_label);
    // Width of the usage cell: the label plus one blank trailing column.
    let cell = label.as_ref().map(|s| s.chars().count() as u16 + 1);

    match (label, cell) {
        (Some(label), Some(cell)) if col_width >= cell + MIN_HINT_COLS => {
            let hint_area = Rect {
                width: col_width - cell,
                ..area
            };
            frame.render_widget(Paragraph::new(line(hints, hint_area.width)), hint_area);

            let usage_area = Rect {
                x: area.x + col_width - cell,
                width: cell,
                ..area
            };
            // Left-aligned within the cell leaves the trailing column blank.
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    label,
                    Style::default().fg(theme::TEXT_DIM),
                ))),
                usage_area,
            );
        }
        // Too narrow (or no usage): the hints get the whole row.
        _ => frame.render_widget(Paragraph::new(line(hints, area.width)), area),
    }
}

/// The footer's session-total label, e.g. `session 48k`.
fn session_label(total: usize) -> String {
    format!(
        "session {}",
        crate::widgets::context_gauge::fmt_tokens(total)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const HINTS: &[(&str, &str)] = &[
        ("Enter", "send"),
        ("Alt+Enter", "newline"),
        ("↑", "history"),
        ("PgUp/PgDn", "scroll"),
    ];

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn wide_footer_lists_every_pair() {
        let rendered = text(&line(HINTS, 80));
        assert_eq!(
            rendered,
            " Enter send · Alt+Enter newline · ↑ history · PgUp/PgDn scroll"
        );
    }

    #[test]
    fn narrow_footer_drops_whole_pairs_from_the_right() {
        // " Enter send · Alt+Enter newline" is 31 cols; the next pair (with
        // its separator) needs 12 more.
        let rendered = text(&line(HINTS, 40));
        assert_eq!(rendered, " Enter send · Alt+Enter newline");
        // Never a partial pair or a dangling separator.
        let tiny = text(&line(HINTS, 12));
        assert_eq!(tiny, " Enter send");
    }

    #[test]
    fn keyless_pair_renders_as_a_plain_note() {
        let rendered = text(&line(
            &[("Esc", "back"), ("", "changes apply next launch")],
            60,
        ));
        assert_eq!(rendered, " Esc back · changes apply next launch");
    }

    #[test]
    fn keys_are_bold_and_labels_faint() {
        let l = line(&[("Enter", "send")], 40);
        let key = &l.spans[1];
        assert_eq!(key.content, "Enter");
        assert!(key.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(l.spans[2].style.fg, Some(theme::TEXT_FAINT));
    }
}
