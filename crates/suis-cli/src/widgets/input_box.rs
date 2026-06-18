//! The chat input box.
//!
//! Rendering only; the buffer and cursor live in the app state. The box grows
//! with its content — Alt+Enter newlines and wrapped long lines alike — up to
//! [`MAX_CONTENT_ROWS`], then scrolls internally to keep the cursor visible.
//! While the agent works the prompt dims and the cursor hides; the working
//! spinner itself lives in the transcript (see `message_list`). The border
//! carries the runtime-mode label (`┤ PLAN ├` etc.) so the active mode is
//! always visible; Shift+Tab cycles it.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};
use ratatui::Frame;

use suis_agent::Mode;

use crate::theme;
use crate::widgets::context_gauge;
use crate::widgets::context_gauge::ContextGauge;

/// The box stops growing at this many content rows and scrolls internally.
const MAX_CONTENT_ROWS: u16 = 6;

/// Columns consumed around the text: borders (2), horizontal padding (2), and
/// the two-char `› ` prompt / continuation indent.
const CHROME_COLS: u16 = 6;

/// The accent colour for a mode's label: analysis blue for Plan, the signature
/// green for Agent, muted for Chat.
fn mode_color(mode: Mode) -> Color {
    match mode {
        Mode::Plan => theme::INFO,
        Mode::Agent => theme::ACCENT,
        Mode::Chat => theme::TEXT_DIM,
    }
}

/// The columns available to buffer text inside a box `area_width` wide.
fn text_width(area_width: u16) -> usize {
    area_width.saturating_sub(CHROME_COLS).max(1) as usize
}

/// The height (including borders) the box needs for `buffer` at `area_width`,
/// measured from wrapped content and clamped to [`MAX_CONTENT_ROWS`].
pub fn required_height(buffer: &str, area_width: u16) -> u16 {
    let rows = wrapped_rows(buffer, text_width(area_width)).len() as u16;
    rows.clamp(1, MAX_CONTENT_ROWS) + 2
}

/// Split `buffer` into display rows: logical lines on `\n`, each hard-wrapped
/// at `width` characters. An empty logical line still occupies one row.
fn wrapped_rows(buffer: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for line in buffer.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            rows.push(String::new());
            continue;
        }
        let mut start = 0;
        while start < chars.len() {
            let end = (start + width).min(chars.len());
            rows.push(chars[start..end].iter().collect());
            start = end;
        }
    }
    rows
}

/// The display (row, column) of the character `cursor` within `buffer` under
/// the same wrapping as [`wrapped_rows`]. A cursor at the exact end of a full
/// row sits one past its last column.
fn cursor_pos(buffer: &str, width: usize, cursor: usize) -> (usize, usize) {
    let mut row = 0;
    let mut consumed = 0;
    for line in buffer.split('\n') {
        let len = line.chars().count();
        let line_rows = if len == 0 { 1 } else { len.div_ceil(width) };
        if cursor <= consumed + len {
            let offset = cursor - consumed;
            let r = (offset / width).min(line_rows - 1);
            return (row + r, offset - r * width);
        }
        consumed += len + 1; // the '\n'
        row += line_rows;
    }
    // Defensive: a cursor beyond the buffer lands after the last row.
    (row.saturating_sub(1), 0)
}

/// Render the input box with the current `buffer` and character `cursor`.
/// `busy` means the agent is working (or compacting): the prompt dims and the
/// terminal cursor is hidden. The border's left title is `mode`'s label; the
/// right title carries the implementation `target` (plan · step), the live
/// context size `ctx_tokens` (tokens currently in the agent's context), and the
/// context-pressure gauge `ctx`.
#[allow(clippy::too_many_arguments)] // a render fn fed straight from app state
pub fn render(
    frame: &mut Frame,
    area: Rect,
    buffer: &str,
    cursor: usize,
    busy: bool,
    mode: Mode,
    target: Option<&str>,
    ctx: Option<ContextGauge>,
    ctx_tokens: Option<usize>,
) {
    let prompt_style = if busy {
        Style::default().fg(theme::TEXT_FAINT)
    } else {
        Style::default().fg(mode_color(mode))
    };

    let width = text_width(area.width);
    let rows = wrapped_rows(buffer, width);
    let (cur_row, cur_col) = cursor_pos(buffer, width, cursor);

    // Scroll the content window so the end stays in view, then pull it back
    // up if the cursor moved above it.
    let visible = area.height.saturating_sub(2).max(1) as usize;
    let top = rows.len().saturating_sub(visible).min(cur_row);

    let last_visible = (top + visible).min(rows.len()) - 1;
    let lines: Vec<Line> = rows[top..=last_visible]
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let global = top + i;
            let prefix = if global == 0 { "› " } else { "  " };
            Line::from(vec![
                Span::styled(prefix, prompt_style),
                Span::styled(row.clone(), Style::default().fg(theme::TEXT)),
            ])
        })
        .collect();

    let title = Line::from(vec![
        Span::styled("┤ ", Style::default().fg(theme::BORDER)),
        Span::styled(
            mode.label(),
            Style::default()
                .fg(mode_color(mode))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ├", Style::default().fg(theme::BORDER)),
    ]);

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(prompt_style)
        .title(title)
        .padding(Padding::horizontal(1));
    if let Some(right) = right_title(target, ctx, ctx_tokens) {
        block = block.title(right.right_aligned());
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);

    // Place the terminal cursor at the caret when idle. The offset accounts
    // for the border (1), the block's horizontal padding (1), and the two-char
    // prompt/indent.
    if !busy {
        let cursor_x = area.x + 1 + 1 + 2 + cur_col as u16;
        let cursor_y = area.y + 1 + (cur_row - top) as u16;
        frame.set_cursor_position((
            cursor_x.min(area.x + area.width.saturating_sub(3)),
            cursor_y,
        ));
    }
}

/// Compose the input box's right-aligned border title from the implementation
/// `target`, the live context size `ctx_tokens`, and the context `ctx` gauge —
/// `┤ target · 8.2k · ctx 62% ├` when all are present, any subset otherwise, and
/// nothing when none is. The token figure and the gauge both describe the live
/// agent context: the absolute size and the same as a % of the window. The gauge
/// turns to the warning colour once usage crosses its threshold.
fn right_title(
    target: Option<&str>,
    ctx: Option<ContextGauge>,
    ctx_tokens: Option<usize>,
) -> Option<Line<'static>> {
    if target.is_none() && ctx.is_none() && ctx_tokens.is_none() {
        return None;
    }
    let mut spans = vec![Span::styled("┤ ", Style::default().fg(theme::BORDER))];
    let mut first = true;
    let mut sep = |spans: &mut Vec<Span<'static>>| {
        if !first {
            spans.push(Span::styled(" · ", Style::default().fg(theme::TEXT_FAINT)));
        }
        first = false;
    };
    if let Some(target) = target {
        sep(&mut spans);
        spans.push(Span::styled(
            target.to_string(),
            Style::default().fg(theme::WARN),
        ));
    }
    // The live context size — tokens currently in the agent's context, the
    // gauge's numerator — e.g. `8.2k`. Resets per turn / per task, unlike the
    // footer's cumulative session total.
    if let Some(tokens) = ctx_tokens {
        sep(&mut spans);
        spans.push(Span::styled(
            context_gauge::fmt_tokens(tokens),
            Style::default().fg(theme::TEXT_DIM),
        ));
    }
    if let Some(ctx) = ctx {
        sep(&mut spans);
        let color = if ctx.is_warning() {
            theme::WARN
        } else {
            theme::TEXT_DIM
        };
        // The percentage of the model's window the live context fills — the
        // pressure gauge that drives pruning, e.g. `ctx 35%`.
        spans.push(Span::styled(ctx.label(), Style::default().fg(color)));
    }
    spans.push(Span::styled(" ├", Style::default().fg(theme::BORDER)));
    Some(Line::from(spans))
}

#[cfg(test)]
mod tests {
    use super::*;

    // At area width 46, text_width is 40.

    #[test]
    fn height_is_three_for_a_single_line() {
        assert_eq!(required_height("", 46), 3);
        assert_eq!(required_height("hello", 46), 3);
    }

    #[test]
    fn height_grows_per_newline_and_clamps_at_the_max() {
        assert_eq!(required_height("a\nb", 46), 4);
        assert_eq!(required_height("a\nb\nc", 46), 5);
        let ten_lines = ["x"; 10].join("\n");
        assert_eq!(required_height(&ten_lines, 46), MAX_CONTENT_ROWS + 2);
    }

    #[test]
    fn wrapped_long_lines_also_grow_the_box() {
        // 100 chars at width 40 wrap to 3 rows.
        let long = "x".repeat(100);
        assert_eq!(required_height(&long, 46), 5);
        assert_eq!(wrapped_rows(&long, 40).len(), 3);
    }

    #[test]
    fn empty_logical_lines_keep_their_row() {
        assert_eq!(wrapped_rows("a\n\nb", 40), vec!["a", "", "b"]);
    }

    #[test]
    fn cursor_lands_on_the_correct_row_and_column() {
        // "line one\nline two" with the cursor at the very end.
        let buffer = "line one\nline two";
        assert_eq!(cursor_pos(buffer, 40, buffer.chars().count()), (1, 8));
        // Right after the Alt+Enter newline: start of row 1.
        assert_eq!(cursor_pos(buffer, 40, 9), (1, 0));
        // End of the first line, before the newline.
        assert_eq!(cursor_pos(buffer, 40, 8), (0, 8));
        // Mid-first-line.
        assert_eq!(cursor_pos(buffer, 40, 4), (0, 4));
    }

    fn title_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn right_title_orders_target_ctx_tokens_then_gauge() {
        let ctx = ContextGauge::new(7_440, 12_000, false);
        let line = right_title(Some("plan · step"), Some(ctx), Some(7_440)).expect("title");
        assert_eq!(title_text(&line), "┤ plan · step · 7.4k · ctx 62% ├");
    }

    #[test]
    fn right_title_shows_ctx_tokens_alone() {
        let line = right_title(None, None, Some(7_440)).expect("title");
        assert_eq!(title_text(&line), "┤ 7.4k ├");
    }

    #[test]
    fn right_title_is_absent_with_nothing_to_show() {
        assert!(right_title(None, None, None).is_none());
    }

    #[test]
    fn cursor_follows_wrapping() {
        let long = "x".repeat(100);
        assert_eq!(cursor_pos(&long, 40, 0), (0, 0));
        assert_eq!(cursor_pos(&long, 40, 40), (1, 0));
        assert_eq!(cursor_pos(&long, 40, 95), (2, 15));
        // The end of an exactly-full row sits one past its last column.
        assert_eq!(cursor_pos(&"y".repeat(40), 40, 40), (0, 40));
    }
}
