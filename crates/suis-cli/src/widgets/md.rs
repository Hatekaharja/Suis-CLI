//! Line-level markdown styling for agent prose.
//!
//! Deliberately small and hand-rolled: fenced code blocks, inline `` `code` ``
//! spans, `**bold**`, headings, and `-`/`*`/numbered bullets. Nothing else —
//! unrecognized markdown renders as plain text, so degradation is always safe.
//! The styler only produces spans; wrapping stays with the caller.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme;

/// Map a message body to one styled [`Line`] per input line.
///
/// Fence state is tracked across lines, so an unterminated fence (a message
/// still streaming, or a model that forgot to close one) styles the remainder
/// as code and never leaks past this message — every call starts outside a
/// fence.
pub fn style_lines(text: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for raw in text.lines() {
        if raw.trim_start().starts_with("```") {
            // The fence itself stays visible but dimmed.
            in_fence = !in_fence;
            out.push(Line::from(Span::styled(
                raw.to_string(),
                Style::default().fg(theme::TEXT_FAINT),
            )));
        } else if in_fence {
            out.push(Line::from(Span::styled(raw.to_string(), code_style())));
        } else {
            out.push(prose_line(raw));
        }
    }
    out
}

/// Style one line outside any fence: a heading, a bullet, or inline prose.
fn prose_line(raw: &str) -> Line<'static> {
    if is_heading(raw) {
        return Line::from(Span::styled(
            raw.to_string(),
            Style::default()
                .fg(theme::TEXT_BRIGHT)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some((marker, rest)) = split_bullet(raw) {
        let mut spans = vec![Span::styled(marker, Style::default().fg(theme::ACCENT))];
        spans.extend(inline_spans(rest));
        return Line::from(spans);
    }
    Line::from(inline_spans(raw))
}

/// `#` to `######` followed by a space.
fn is_heading(raw: &str) -> bool {
    let hashes = raw.chars().take_while(|&c| c == '#').count();
    (1..=6).contains(&hashes) && raw[hashes..].starts_with(' ')
}

/// Split a `-`/`*`/`1.` bullet into its marker (with leading indent and the
/// trailing space) and the rest of the line.
fn split_bullet(raw: &str) -> Option<(String, &str)> {
    let indent = raw.len() - raw.trim_start().len();
    let trimmed = &raw[indent..];
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        return Some((raw[..indent + 2].to_string(), rest));
    }
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        if let Some(rest) = trimmed[digits..].strip_prefix(". ") {
            return Some((raw[..indent + digits + 2].to_string(), rest));
        }
    }
    None
}

/// Split a prose line into spans around inline `` `code` `` (the backticks are
/// dropped), parsing `**bold**` in the non-code stretches. An unmatched
/// backtick is left as literal text.
fn inline_spans(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else {
            break;
        };
        bold_spans(&rest[..start], &mut spans);
        spans.push(Span::styled(after[..end].to_string(), code_style()));
        rest = &after[end + 1..];
    }
    bold_spans(rest, &mut spans);
    spans
}

/// Push spans for a code-free stretch, bolding `**…**` pairs (the asterisks
/// are dropped). An unmatched `**` is left as literal text.
fn bold_spans(text: &str, spans: &mut Vec<Span<'static>>) {
    let mut rest = text;
    while let Some(start) = rest.find("**") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("**") else { break };
        if start > 0 {
            spans.push(plain(&rest[..start]));
        }
        spans.push(Span::styled(
            after[..end].to_string(),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        ));
        rest = &after[end + 2..];
    }
    if !rest.is_empty() {
        spans.push(plain(rest));
    }
}

fn plain(text: &str) -> Span<'static> {
    Span::styled(text.to_string(), Style::default().fg(theme::TEXT))
}

fn code_style() -> Style {
    Style::default().fg(theme::CODE_FG).bg(theme::CODE_BG)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rendered text of one line, spans concatenated.
    fn text_of(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn plain_text_passes_through_identical() {
        let lines = style_lines("just some prose\nand a second line");
        assert_eq!(lines.len(), 2);
        assert_eq!(text_of(&lines[0]), "just some prose");
        assert_eq!(text_of(&lines[1]), "and a second line");
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].style.fg, Some(theme::TEXT));
    }

    #[test]
    fn fenced_block_styles_code_and_dims_the_fences() {
        let lines = style_lines("```rust\nlet x = 1;\n```\nafter");
        assert_eq!(lines.len(), 4);
        // The fences themselves are kept, dimmed.
        assert_eq!(lines[0].spans[0].style.fg, Some(theme::TEXT_FAINT));
        assert_eq!(lines[2].spans[0].style.fg, Some(theme::TEXT_FAINT));
        // The body carries the code style.
        assert_eq!(lines[1].spans[0].style.bg, Some(theme::CODE_BG));
        assert_eq!(lines[1].spans[0].style.fg, Some(theme::CODE_FG));
        // Styling does not leak past the closing fence.
        assert_eq!(lines[3].spans[0].style.fg, Some(theme::TEXT));
    }

    #[test]
    fn unterminated_fence_styles_to_the_end() {
        let lines = style_lines("```\nstill code\nmore code");
        assert_eq!(lines[1].spans[0].style.bg, Some(theme::CODE_BG));
        assert_eq!(lines[2].spans[0].style.bg, Some(theme::CODE_BG));
    }

    #[test]
    fn markdown_inside_a_fence_is_not_interpreted() {
        let lines = style_lines("```\n# not a heading\n```");
        assert_eq!(text_of(&lines[1]), "# not a heading");
        assert_eq!(lines[1].spans[0].style.bg, Some(theme::CODE_BG));
    }

    #[test]
    fn inline_code_splits_spans_and_drops_the_ticks() {
        let lines = style_lines("run `cargo test` now");
        let line = &lines[0];
        assert_eq!(text_of(line), "run cargo test now");
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[1].content, "cargo test");
        assert_eq!(line.spans[1].style.bg, Some(theme::CODE_BG));
        assert_eq!(line.spans[0].style.fg, Some(theme::TEXT));
    }

    #[test]
    fn unmatched_backtick_stays_literal() {
        let lines = style_lines("a stray ` backtick");
        assert_eq!(text_of(&lines[0]), "a stray ` backtick");
    }

    #[test]
    fn bold_splits_spans_and_drops_the_asterisks() {
        let lines = style_lines("this is **important** stuff");
        let line = &lines[0];
        assert_eq!(text_of(line), "this is important stuff");
        let bold = &line.spans[1];
        assert_eq!(bold.content, "important");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn heading_is_bold_bright() {
        let lines = style_lines("## Setup");
        let span = &lines[0].spans[0];
        assert_eq!(span.style.fg, Some(theme::TEXT_BRIGHT));
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
        // Not-a-heading lookalikes pass through as prose.
        let lines = style_lines("#hashtag");
        assert_eq!(lines[0].spans[0].style.fg, Some(theme::TEXT));
    }

    #[test]
    fn bullets_style_the_marker_and_parse_the_rest() {
        for input in [
            "- item with `code`",
            "* item with `code`",
            "3. item with `code`",
        ] {
            let lines = style_lines(input);
            let line = &lines[0];
            assert_eq!(line.spans[0].style.fg, Some(theme::ACCENT), "{input}");
            assert!(
                line.spans
                    .iter()
                    .any(|s| s.content == "code" && s.style.bg == Some(theme::CODE_BG)),
                "{input}"
            );
        }
        // Indented bullets keep their indent inside the marker span.
        let lines = style_lines("  - nested");
        assert_eq!(lines[0].spans[0].content, "  - ");
    }
}
