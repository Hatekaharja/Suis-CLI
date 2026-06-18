//! The scrollable chat transcript and its display model.
//!
//! [`ChatMessage`] is the UI's view of one turn (user, agent, system notice, or
//! tool output). The agent's streamed text accumulates into the trailing
//! agent message, which shows a cursor glyph while `streaming` is true.

use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Padding, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};
use ratatui::Frame;

use crate::theme;
use crate::widgets::{diff_viewer, md};

/// Who produced a chat message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgRole {
    /// The human user.
    User,
    /// The assistant/agent.
    Agent,
    /// A local system notice (command output, errors, help text).
    System,
    /// A tool result echoed into the transcript.
    Tool,
}

impl MsgRole {
    fn label(self) -> &'static str {
        match self {
            MsgRole::User => "You",
            MsgRole::Agent => "Suis",
            MsgRole::System => "System",
            MsgRole::Tool => "Tool",
        }
    }

    fn color(self) -> Color {
        match self {
            MsgRole::User => theme::ACCENT,
            MsgRole::Agent => theme::INFO,
            MsgRole::System => theme::TEXT_FAINT,
            MsgRole::Tool => theme::TOOL,
        }
    }
}

/// Where a tool activity card is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    /// The call is executing; the card shows `⚙ name subject …`.
    Running,
    /// Finished successfully; the card collapses to a summary line.
    Ok,
    /// Finished with an error; the card renders in danger styling.
    Error,
}

/// Display model for one tool invocation, rendered as a single in-place card:
/// a one-line summary plus a collapsed (or expanded) body. The full output is
/// always retained here — collapsing is strictly a render-time choice; the
/// result fed back to the model is untouched.
#[derive(Debug, Clone)]
pub struct ToolCard {
    /// The tool's name (`read`, `bash`, …).
    pub name: String,
    /// One-line subject derived from the call args (path, command, pattern).
    pub subject: String,
    /// Lifecycle state.
    pub status: ToolStatus,
    /// The complete result content, set on completion.
    pub output: String,
    /// Whether the body renders in full (`Ctrl+O`) instead of collapsed.
    pub expanded: bool,
    /// The exact call the agent made — the tool name and its JSON arguments,
    /// pretty-printed. Captured at call start so the UI can show precisely what
    /// was sent, independent of how the result is summarised.
    pub raw_call: String,
    /// Whether to render [`Self::raw_call`] on the card. Toggled by `/developer`
    /// across every card at once; off by default so the transcript stays clean.
    pub show_raw: bool,
}

impl ToolCard {
    /// Whether the output carries a unified diff (an `edit` result), which is
    /// exempt from collapsing and renders with diff colouring. Keyed on the tool
    /// name — `edit` is the only producer of diffs — so a `read` of a file that
    /// happens to contain a `--- ` line is not mistaken for a diff and still
    /// collapses normally.
    fn is_diff(&self) -> bool {
        self.name == "edit" && self.output.contains("\n--- ")
    }
}

/// Display model for the model's streamed reasoning ("thinking"), rendered as a
/// collapsible block: a one-line header — `▸ Thinking…` while it streams,
/// `▸ Thought for 12s` once done — with the full reasoning shown only when the
/// header is clicked (or toggled by key) open. Reasoning is display-only; it is
/// never fed back to the model.
#[derive(Debug, Clone)]
pub struct ThinkingCard {
    /// The accumulated reasoning text.
    pub text: String,
    /// Whether reasoning is still streaming in.
    pub streaming: bool,
    /// Whether the body renders in full instead of just the header.
    pub expanded: bool,
    /// Seconds the model spent thinking, set when the block is finalized.
    pub elapsed_secs: u64,
}

/// One rendered message in the transcript.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// The author.
    pub role: MsgRole,
    /// The text body (may grow while streaming).
    pub text: String,
    /// Whether the agent is still streaming into this message.
    pub streaming: bool,
    /// Set for tool activity: the message renders as a card instead of prose.
    pub tool: Option<ToolCard>,
    /// Set for model reasoning: the message renders as a collapsible thinking
    /// block instead of prose.
    pub thinking: Option<ThinkingCard>,
    /// Pre-styled body lines (aligned command output like `/help`); when set
    /// they render in place of the flat-styled `text`.
    pub styled: Option<Vec<Line<'static>>>,
}

impl ChatMessage {
    /// A finished message.
    pub fn new(role: MsgRole, text: impl Into<String>) -> Self {
        ChatMessage {
            role,
            text: text.into(),
            streaming: false,
            tool: None,
            thinking: None,
            styled: None,
        }
    }

    /// An empty agent message that text will stream into.
    pub fn streaming_agent() -> Self {
        ChatMessage {
            role: MsgRole::Agent,
            text: String::new(),
            streaming: true,
            tool: None,
            thinking: None,
            styled: None,
        }
    }

    /// A running tool card, updated in place when its result arrives.
    pub fn tool_running(name: impl Into<String>, subject: impl Into<String>) -> Self {
        ChatMessage {
            role: MsgRole::Tool,
            text: String::new(),
            streaming: false,
            tool: Some(ToolCard {
                name: name.into(),
                subject: subject.into(),
                status: ToolStatus::Running,
                output: String::new(),
                expanded: false,
                raw_call: String::new(),
                show_raw: false,
            }),
            thinking: None,
            styled: None,
        }
    }

    /// A streaming thinking block, seeded with the first reasoning chunk and
    /// updated in place as more arrives.
    pub fn thinking_streaming(chunk: impl Into<String>) -> Self {
        ChatMessage {
            role: MsgRole::Agent,
            text: String::new(),
            streaming: false,
            tool: None,
            thinking: Some(ThinkingCard {
                text: chunk.into(),
                streaming: true,
                expanded: false,
                elapsed_secs: 0,
            }),
            styled: None,
        }
    }

    /// A system notice carrying pre-styled lines. The plain text is derived
    /// from the lines, so text-based logic (and tests) keep working.
    pub fn system_styled(lines: Vec<Line<'static>>) -> Self {
        let text = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        ChatMessage {
            role: MsgRole::System,
            text,
            streaming: false,
            tool: None,
            thinking: None,
            styled: Some(lines),
        }
    }
}

/// Build the styled lines for a transcript, wrapped to `width` columns. Public
/// so the line count can drive scroll math in the chat screen.
///
/// User messages are drawn as a filled, full-width card (with a green accent
/// edge) so they stand apart from the assistant's plain prose. A blank spacer
/// separates consecutive messages.
pub fn lines(messages: &[ChatMessage], width: u16) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for (idx, msg) in messages.iter().enumerate() {
        // Vertical padding between one message and the next.
        if idx > 0 {
            out.push(Line::from(""));
        }
        out.extend(message_block(msg, width));
    }
    out
}

/// Serialize the whole transcript to plain text for the clipboard (`/developer`
/// Ctrl+Y). Unlike the rendered view this is the *full* raw history: every tool
/// card's name, its exact call (name + JSON args), and its complete output/diff
/// are included even when collapsed on screen, and thinking blocks are written
/// out in full — so a pasted dump captures exactly what happened, tool calls and
/// all. Blocks are separated by a blank line.
pub fn transcript_text(messages: &[ChatMessage]) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for msg in messages {
        blocks.push(message_text(msg));
    }
    blocks.join("\n\n")
}

/// One message's plain-text block for [`transcript_text`].
fn message_text(msg: &ChatMessage) -> String {
    if let Some(card) = &msg.tool {
        let status = match card.status {
            ToolStatus::Running => "running",
            ToolStatus::Ok => "ok",
            ToolStatus::Error => "error",
        };
        let subject = if card.subject.is_empty() {
            String::new()
        } else {
            format!(" {}", card.subject)
        };
        let mut out = format!("[tool] {}{subject} ({status})", card.name);
        if !card.raw_call.is_empty() {
            out.push_str("\n  raw call:");
            for line in card.raw_call.lines() {
                out.push_str(&format!("\n    {line}"));
            }
        }
        if !card.output.is_empty() {
            out.push_str("\n  output:");
            for line in card.output.lines() {
                out.push_str(&format!("\n    {line}"));
            }
        }
        return out;
    }
    if let Some(card) = &msg.thinking {
        let mut out = format!("[thinking {}s]", card.elapsed_secs);
        for line in card.text.lines() {
            out.push_str(&format!("\n  {line}"));
        }
        return out;
    }
    format!("{}: {}", msg.role.label(), msg.text)
}

/// Build the rendered lines for one message, without the inter-message spacer.
/// Shared by [`lines`] (which concatenates the blocks) and [`click_regions`]
/// (which measures them), so what is drawn and what is hit-tested never drift.
fn message_block(msg: &ChatMessage, width: u16) -> Vec<Line<'static>> {
    if msg.role == MsgRole::User {
        return user_card(&msg.text, width);
    }
    if let Some(card) = &msg.thinking {
        return thinking_card_lines(card);
    }
    if let Some(card) = &msg.tool {
        return tool_card_lines(card);
    }

    let header = Span::styled(
        format!("{}: ", msg.role.label()),
        Style::default()
            .fg(msg.role.color())
            .add_modifier(Modifier::BOLD),
    );

    // Agent prose renders through the markdown styler; pre-styled system
    // notices carry their own lines; everything else is flat, except a
    // (legacy) tool message carrying a diff.
    let is_diff = msg.role == MsgRole::Tool && msg.text.contains("\n--- ");
    let body: Vec<Line> = if msg.role == MsgRole::Agent {
        md::style_lines(&msg.text)
    } else if let Some(styled) = &msg.styled {
        styled.clone()
    } else {
        msg.text
            .lines()
            .map(|body| {
                let style = if is_diff {
                    diff_viewer::DiffLineKind::classify(body).style()
                } else {
                    Style::default().fg(line_color(msg.role))
                };
                Line::from(Span::styled(body.to_string(), style))
            })
            .collect()
    };

    if body.is_empty() {
        let mut line = vec![header];
        if msg.streaming {
            line.push(cursor_span());
        }
        return vec![Line::from(line)];
    }

    let mut out = Vec::with_capacity(body.len());
    let last = body.len() - 1;
    for (i, line) in body.into_iter().enumerate() {
        let mut spans = Vec::new();
        if i == 0 {
            spans.push(header.clone());
        } else {
            spans.push(Span::raw("  "));
        }
        spans.extend(line.spans);
        // Streaming cursor trails the final line of the active agent message.
        if msg.streaming && i == last {
            spans.push(cursor_span());
        }
        out.push(Line::from(spans));
    }
    out
}

/// Render one thinking block: a clickable header (`▸ Thinking…` while it
/// streams, `▸ Thought for 12s` once finalized) and, only when expanded, the
/// reasoning text dimmed and italic beneath it.
fn thinking_card_lines(card: &ThinkingCard) -> Vec<Line<'static>> {
    let chevron = if card.expanded { "▾" } else { "▸" };
    let label = if card.streaming {
        "Thinking…".to_string()
    } else {
        format!("Thought for {}s", card.elapsed_secs)
    };
    let mut header = vec![
        Span::styled(
            format!("{chevron} "),
            Style::default().fg(theme::TEXT_FAINT),
        ),
        Span::styled(
            label,
            Style::default()
                .fg(theme::TEXT_DIM)
                .add_modifier(Modifier::ITALIC),
        ),
    ];
    if card.streaming {
        header.push(cursor_span());
    }
    let mut out = vec![Line::from(header)];
    if card.expanded {
        for line in card.text.lines() {
            out.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    line.to_string(),
                    Style::default()
                        .fg(theme::TEXT_FAINT)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
    }
    out
}

/// Render one tool activity card: the status line, then a body that depends on
/// the card's state — nothing while running, the full error text on failure,
/// every line of a diff, and otherwise the head (or, expanded, all) of the
/// output.
fn tool_card_lines(card: &ToolCard) -> Vec<Line<'static>> {
    let (glyph, glyph_color) = match card.status {
        ToolStatus::Running => ("⚙", theme::TOOL),
        ToolStatus::Ok => ("✓", theme::ACCENT),
        ToolStatus::Error => ("✗", theme::DANGER),
    };
    let mut spans = Vec::new();
    // A completed, collapsible card (`Ok`, with collapsible output) leads with a
    // chevron marking its open/closed state — the click/keyboard toggle target.
    if card.status == ToolStatus::Ok && !card.is_diff() {
        let chevron = if card.expanded { "▾ " } else { "▸ " };
        spans.push(Span::styled(
            chevron,
            Style::default().fg(theme::TEXT_FAINT),
        ));
    }
    spans.push(Span::styled(
        format!("{glyph} "),
        Style::default().fg(glyph_color),
    ));
    spans.push(Span::styled(
        card.name.clone(),
        Style::default()
            .fg(theme::TOOL)
            .add_modifier(Modifier::BOLD),
    ));
    if !card.subject.is_empty() {
        spans.push(Span::styled(
            format!(" {}", card.subject),
            Style::default().fg(theme::TEXT_DIM),
        ));
    }
    match card.status {
        ToolStatus::Running => {
            spans.push(Span::styled(" …", Style::default().fg(theme::TEXT_FAINT)))
        }
        // An edit's diff reads as a `+added −removed` summary; other tools count
        // their output lines.
        ToolStatus::Ok if card.is_diff() => {
            let (added, removed) = diff_viewer::change_counts(&card.output);
            spans.push(Span::styled(
                format!(" (+{added} −{removed})"),
                Style::default().fg(theme::TEXT_FAINT),
            ));
        }
        ToolStatus::Ok => {
            let output_lines = card.output.lines().count();
            spans.push(Span::styled(
                format!(
                    " ({output_lines} line{})",
                    if output_lines == 1 { "" } else { "s" }
                ),
                Style::default().fg(theme::TEXT_FAINT),
            ));
        }
        ToolStatus::Error => {}
    }
    let mut out = vec![Line::from(spans)];

    let body = |line: &str, style: Style| {
        Line::from(vec![Span::raw("  "), Span::styled(line.to_string(), style)])
    };
    match card.status {
        ToolStatus::Running => {
            // Nothing has come back yet, but the raw call is already known: in
            // developer mode show what is executing right now.
            append_raw_call(&mut out, card);
        }
        ToolStatus::Error => {
            // The raw call sits above the failure, so a bad argument is visible
            // next to the error it caused.
            append_raw_call(&mut out, card);
            // Errors stay visible in full.
            for line in card.output.lines() {
                out.push(body(line, Style::default().fg(theme::DANGER)));
            }
        }
        ToolStatus::Ok if card.is_diff() => {
            // The raw call sits above the diff, so a truncated/empty new_string
            // is visible next to the change it produced.
            append_raw_call(&mut out, card);
            // Diffs are exempt from collapsing: every line, diff-coloured.
            for line in card.output.lines() {
                out.push(body(
                    line,
                    diff_viewer::DiffLineKind::classify(line).style(),
                ));
            }
        }
        ToolStatus::Ok => {
            // Collapsed by default: the summary line alone. Expanding (a click on
            // the header, or the keyboard toggle) reveals the raw call (developer
            // mode) followed by every output line.
            if card.expanded {
                append_raw_call(&mut out, card);
                for line in card.output.lines() {
                    out.push(body(line, Style::default().fg(theme::TEXT_DIM)));
                }
            }
        }
    }
    out
}

/// Append the card's raw call — the exact tool name and JSON arguments the agent
/// sent — as a faint, labelled block. A no-op unless developer mode armed the
/// card ([`ToolCard::show_raw`]) and a call was captured. Indented one step
/// deeper than the card body so it reads as metadata, not output.
fn append_raw_call(out: &mut Vec<Line<'static>>, card: &ToolCard) {
    if !card.show_raw || card.raw_call.is_empty() {
        return;
    }
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "raw call:",
            Style::default()
                .fg(theme::TEXT_FAINT)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    for line in card.raw_call.lines() {
        out.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(line.to_string(), Style::default().fg(theme::TEXT_FAINT)),
        ]));
    }
}

/// Whether `msg` renders a header the user can click (or key-toggle) to
/// expand/collapse: a thinking block, or a completed, collapsible tool card.
/// Diffs and errors render in full and carry no toggle.
fn is_toggleable(msg: &ChatMessage) -> bool {
    if msg.thinking.is_some() {
        return true;
    }
    matches!(&msg.tool, Some(card) if card.status == ToolStatus::Ok && !card.is_diff())
}

/// A clickable toggle target, in transcript content rows (before the scroll
/// offset is applied): rows `start..end` cover the message's collapsible header,
/// and `index` is its position in the message slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClickRegion {
    /// First content row (inclusive) the header occupies.
    pub start: u16,
    /// One past the last content row the header occupies.
    pub end: u16,
    /// Index of the owning message.
    pub index: usize,
}

/// Map every collapsible header to its content-row range, measured against
/// `width` exactly as [`render`] wraps the transcript, so a click row resolves
/// to the right card. The ranges are built in the same pass order as [`lines`],
/// including the one-row spacer between messages, so the offsets line up with
/// what is drawn.
pub fn click_regions(messages: &[ChatMessage], width: u16) -> Vec<ClickRegion> {
    let mut regions = Vec::new();
    if width == 0 {
        return regions;
    }
    let mut row: u16 = 0;
    for (idx, msg) in messages.iter().enumerate() {
        if idx > 0 {
            row = row.saturating_add(1); // inter-message spacer
        }
        let block = message_block(msg, width);
        if is_toggleable(msg) && !block.is_empty() {
            // The clickable header is the block's first line; its wrapped height
            // (a long subject may wrap) is the click target.
            let header_rows = paragraph_rows(&block[..1], width);
            regions.push(ClickRegion {
                start: row,
                end: row.saturating_add(header_rows),
                index: idx,
            });
        }
        row = row.saturating_add(paragraph_rows(&block, width));
    }
    regions
}

/// The number of terminal rows `lines` wrap to at `width`, matching the
/// `Paragraph` the transcript is rendered through.
fn paragraph_rows(lines: &[Line<'static>], width: u16) -> u16 {
    Paragraph::new(lines.to_vec())
        .wrap(Wrap { trim: false })
        .line_count(width)
        .try_into()
        .unwrap_or(u16::MAX)
}

/// Render a user message as a filled card spanning `width` columns: a green
/// accent bar down the left edge, a `You` label, then the wrapped body, with a
/// blank padding row above and below. Every cell carries the card background so
/// the box reads as one solid block.
fn user_card(text: &str, width: u16) -> Vec<Line<'static>> {
    let w = width as usize;
    if w == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    // Top padding row (full-width background).
    lines.push(card_row(None, theme::ACCENT, Modifier::empty(), w));
    lines.push(card_row(Some("You"), theme::ACCENT, Modifier::BOLD, w));
    // The body wraps to the width inside the accent bar and side gutters.
    let body_width = w.saturating_sub(2).max(1);
    for segment in wrap_text(text, body_width) {
        lines.push(card_row(Some(&segment), theme::TEXT, Modifier::empty(), w));
    }
    // Bottom padding row.
    lines.push(card_row(None, theme::ACCENT, Modifier::empty(), w));
    lines
}

/// One row of a user card: the accent bar, a left gutter space, `content` (or
/// blank), then right-padding — all on the card background, totalling `w` cells.
fn card_row(content: Option<&str>, fg: Color, modifier: Modifier, w: usize) -> Line<'static> {
    let bar = Span::styled(
        "▌",
        Style::default().fg(theme::ACCENT).bg(theme::USER_SURFACE),
    );
    // Columns available after the 1-col accent bar.
    let inner = w.saturating_sub(1);
    let mut text = String::from(" ");
    if let Some(content) = content {
        text.push_str(content);
    }
    let used = text.chars().count();
    if used < inner {
        text.push_str(&" ".repeat(inner - used));
    }
    Line::from(vec![
        bar,
        Span::styled(
            text,
            Style::default()
                .fg(fg)
                .bg(theme::USER_SURFACE)
                .add_modifier(modifier),
        ),
    ])
}

/// Greedy word-wrap `text` to `width` columns, hard-breaking any word longer
/// than the width. Existing newlines are preserved as line breaks.
pub(crate) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let before = out.len();
        let mut current = String::new();
        let mut len = 0usize;
        for word in raw.split_whitespace() {
            let wlen = word.chars().count();
            if wlen > width {
                // A single over-long word: flush, then break it across rows.
                if len > 0 {
                    out.push(std::mem::take(&mut current));
                    len = 0;
                }
                let chars: Vec<char> = word.chars().collect();
                for chunk in chars.chunks(width) {
                    out.push(chunk.iter().collect());
                }
                continue;
            }
            let extra = if len == 0 { wlen } else { wlen + 1 };
            if len + extra > width {
                out.push(std::mem::take(&mut current));
                current.push_str(word);
                len = wlen;
            } else {
                if len > 0 {
                    current.push(' ');
                    len += 1;
                }
                current.push_str(word);
                len += wlen;
            }
        }
        if len > 0 {
            out.push(current);
        }
        // Preserve a genuinely blank input line as one empty row.
        if out.len() == before {
            out.push(String::new());
        }
    }
    out
}

fn line_color(role: MsgRole) -> Color {
    match role {
        MsgRole::User => theme::TEXT,
        MsgRole::Agent => theme::TEXT,
        MsgRole::System => theme::TEXT_FAINT,
        MsgRole::Tool => theme::TOOL,
    }
}

fn cursor_span() -> Span<'static> {
    Span::styled(
        "▊",
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::SLOW_BLINK),
    )
}

/// Append the working-status line (spinner · tool · elapsed) below the last
/// message, separated by the usual blank spacer.
fn append_status(out: &mut Vec<Line<'static>>, status: Option<&str>) {
    if let Some(status) = status {
        if !out.is_empty() {
            out.push(Line::from(""));
        }
        out.push(Line::from(Span::styled(
            status.to_string(),
            Style::default().fg(theme::TEXT_DIM),
        )));
    }
}

/// The number of rows the transcript occupies once wrapped to `width` columns.
/// `width` is the inner text width (i.e. excluding the surrounding border).
/// Drives the scroll math that keeps the view pinned to the bottom.
pub fn content_height(messages: &[ChatMessage], width: u16, status: Option<&str>) -> u16 {
    if width == 0 {
        return 0;
    }
    let mut content = lines(messages, width);
    append_status(&mut content, status);
    let paragraph = Paragraph::new(content).wrap(Wrap { trim: false });
    paragraph.line_count(width).try_into().unwrap_or(u16::MAX)
}

/// The pinned cue that content is arriving below a scrolled-up view.
const NEW_OUTPUT_NOTICE: &str = "· · · new output below · · ·";

/// Render the transcript into `area`, scrolled by `scroll` lines from the top.
/// The framing block carries the theme border and a little horizontal padding
/// so messages don't touch the edges; `lines` is built against the resulting
/// inner width so user cards span it exactly.
///
/// When the view is `detached` from the bottom, a scrollbar marks the position
/// on the right edge — and, while content is still `streaming` in, a pinned
/// notice on the bottom border points at the live edge. A bottom-pinned view
/// renders neither: zero idle visual noise.
///
/// A `Some` `status` (the busy hint: spinner · tool · elapsed) renders as the
/// transcript's trailing line, marking where the agent is working.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    messages: &[ChatMessage],
    scroll: u16,
    detached: bool,
    streaming: bool,
    status: Option<&str>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .padding(Padding::horizontal(1))
        .title(Span::styled(
            "Suis",
            Style::default()
                .fg(theme::TEXT_DIM)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    let mut content = lines(messages, inner.width);
    append_status(&mut content, status);
    let paragraph = Paragraph::new(content)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(paragraph, area);

    if !detached {
        return;
    }

    // The scrollbar tracks the scrollable range (top offsets), drawn over the
    // right border between the corners.
    let max_scroll = content_height(messages, inner.width, status).saturating_sub(inner.height);
    if max_scroll > 0 && area.height > 2 {
        let mut state =
            ScrollbarState::new(max_scroll as usize).position(scroll.min(max_scroll) as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("│"))
                .thumb_symbol("█"),
            area.inner(Margin::new(0, 1)),
            &mut state,
        );
    }

    if streaming && area.height >= 2 {
        let width = NEW_OUTPUT_NOTICE.chars().count() as u16;
        if area.width > width + 2 {
            let rect = Rect {
                x: area.x + (area.width - width) / 2,
                y: area.y + area.height - 1,
                width,
                height: 1,
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    NEW_OUTPUT_NOTICE,
                    Style::default().fg(theme::WARN),
                ))),
                rect,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u16 = 40;

    #[test]
    fn multiline_agent_body_indents_continuation() {
        let msgs = vec![ChatMessage::new(MsgRole::Agent, "line one\nline two")];
        let rendered = lines(&msgs, W);
        assert_eq!(rendered.len(), 2);
    }

    #[test]
    fn empty_streaming_message_still_renders_a_line() {
        let msgs = vec![ChatMessage::streaming_agent()];
        let rendered = lines(&msgs, W);
        assert_eq!(rendered.len(), 1);
    }

    #[test]
    fn user_message_renders_as_a_padded_card() {
        // Top pad + "You" label + one body row + bottom pad = 4 rows.
        let msgs = vec![ChatMessage::new(MsgRole::User, "hello")];
        let rendered = lines(&msgs, W);
        assert_eq!(rendered.len(), 4);
    }

    #[test]
    fn card_rows_span_the_full_width() {
        let msgs = vec![ChatMessage::new(MsgRole::User, "hi")];
        let rendered = lines(&msgs, W);
        for line in &rendered {
            let cols: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            assert_eq!(cols, W as usize, "card row should fill the width");
        }
    }

    #[test]
    fn a_blank_spacer_separates_messages() {
        // user card (4 rows) + spacer (1) + agent line (1) = 6.
        let msgs = vec![
            ChatMessage::new(MsgRole::User, "hello"),
            ChatMessage::new(MsgRole::Agent, "hi there"),
        ];
        let rendered = lines(&msgs, W);
        assert_eq!(rendered.len(), 6);
    }

    /// A completed tool card with `n` output lines.
    fn completed_card(n: usize, status: ToolStatus) -> ChatMessage {
        let mut msg = ChatMessage::tool_running("read", "src/main.rs");
        let card = msg.tool.as_mut().unwrap();
        card.status = status;
        card.output = (1..=n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        msg
    }

    #[test]
    fn running_card_is_a_single_line() {
        let msgs = vec![ChatMessage::tool_running("read", "src/main.rs")];
        let rendered = lines(&msgs, W);
        assert_eq!(rendered.len(), 1);
        let text: String = rendered[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("⚙"));
        assert!(text.contains("read"));
        assert!(text.contains("src/main.rs"));
    }

    #[test]
    fn collapsed_card_shows_only_the_summary_line() {
        let msgs = vec![completed_card(10, ToolStatus::Ok)];
        let rendered = lines(&msgs, W);
        // Collapsed by default: the summary line alone, no body.
        assert_eq!(rendered.len(), 1);
        let summary: String = rendered[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            summary.contains("▸"),
            "a collapsed card shows a closed chevron"
        );
        assert!(summary.contains("✓"));
        assert!(summary.contains("(10 lines)"));
    }

    #[test]
    fn expanded_card_renders_the_full_body() {
        let mut msg = completed_card(10, ToolStatus::Ok);
        msg.tool.as_mut().unwrap().expanded = true;
        let rendered = lines(&[msg], W);
        assert_eq!(rendered.len(), 11);
    }

    /// A thinking block carrying `text`, finalized after `secs` unless still
    /// `streaming`.
    fn thinking_card(text: &str, streaming: bool, secs: u64) -> ChatMessage {
        let mut msg = ChatMessage::thinking_streaming(text);
        let card = msg.thinking.as_mut().unwrap();
        card.streaming = streaming;
        card.elapsed_secs = secs;
        msg
    }

    #[test]
    fn streaming_thinking_block_is_one_collapsed_header() {
        let rendered = lines(&[thinking_card("weighing options", true, 0)], W);
        assert_eq!(rendered.len(), 1, "collapsed: header only");
        let header: String = rendered[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header.contains("▸"), "closed chevron");
        assert!(header.contains("Thinking…"));
        assert!(!header.contains("weighing"), "body hidden while collapsed");
    }

    #[test]
    fn finalized_thinking_header_shows_the_elapsed_seconds() {
        let rendered = lines(&[thinking_card("done", false, 12)], W);
        let header: String = rendered[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header.contains("Thought for 12s"), "header was: {header}");
    }

    #[test]
    fn expanded_thinking_block_reveals_its_text() {
        let mut msg = thinking_card("line one\nline two", false, 3);
        msg.thinking.as_mut().unwrap().expanded = true;
        let rendered = lines(&[msg], W);
        // Header + the two reasoning lines.
        assert_eq!(rendered.len(), 3);
        assert!(rendered[0].spans.iter().any(|s| s.content.contains("▾")));
        let body: String = rendered[2]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(body.contains("line two"));
    }

    #[test]
    fn click_regions_target_tool_and_thinking_headers() {
        // spacer math: thinking header (1) + spacer (1) + collapsed tool (1).
        let messages = vec![
            thinking_card("reasoning", false, 5),
            completed_card(8, ToolStatus::Ok),
        ];
        let regions = click_regions(&messages, W);
        assert_eq!(regions.len(), 2);
        // The thinking header is content row 0.
        assert_eq!(regions[0].index, 0);
        assert_eq!((regions[0].start, regions[0].end), (0, 1));
        // The tool summary follows the spacer: row 2.
        assert_eq!(regions[1].index, 1);
        assert_eq!((regions[1].start, regions[1].end), (2, 3));
    }

    #[test]
    fn click_regions_skip_running_and_error_cards() {
        let running = ChatMessage::tool_running("read", "a.rs");
        let mut errored = ChatMessage::tool_running("bash", "x");
        errored.tool.as_mut().unwrap().status = ToolStatus::Error;
        errored.tool.as_mut().unwrap().output = "boom".into();
        let regions = click_regions(&[running, errored], W);
        assert!(
            regions.is_empty(),
            "only Ok cards and thinking are clickable"
        );
    }

    #[test]
    fn error_card_is_danger_styled_with_the_error_visible() {
        let mut msg = ChatMessage::tool_running("bash", "cargo build");
        let card = msg.tool.as_mut().unwrap();
        card.status = ToolStatus::Error;
        card.output = "error[E0308]: mismatched types\nexpected u16".into();
        let rendered = lines(&[msg], W);
        // One card: status line + both error lines, all visible.
        assert_eq!(rendered.len(), 3);
        assert_eq!(rendered[0].spans[0].style.fg, Some(theme::DANGER));
        assert_eq!(rendered[1].spans[1].style.fg, Some(theme::DANGER));
        let body: String = rendered[1]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(body.contains("mismatched types"));
    }

    #[test]
    fn edit_diff_card_renders_every_diff_line() {
        let mut msg = ChatMessage::tool_running("edit", "src/main.rs");
        let card = msg.tool.as_mut().unwrap();
        card.status = ToolStatus::Ok;
        card.output =
            "Edited 'src/main.rs'.\n--- src/main.rs\n+++ src/main.rs\n-old\n+new\n context".into();
        let rendered = lines(&[msg], W);
        // Summary + all six diff lines, despite the card being collapsed.
        assert_eq!(rendered.len(), 7);
        // The header summarises the change as `+added −removed`, not a line count.
        let header: String = rendered[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(header.contains("(+1 −1)"), "missing diff summary: {header}");
        // Added/removed lines keep their diff colours.
        let plus: String = rendered[5]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(plus.contains("+new"));
    }

    #[test]
    fn transcript_text_dumps_full_raw_history_including_tool_calls() {
        let mut tool = ChatMessage::tool_running("edit", "index.html");
        let card = tool.tool.as_mut().unwrap();
        card.status = ToolStatus::Ok;
        card.output = "Edited 'index.html'.\n-<script>\n+".into();
        card.raw_call = "edit {\n  \"new_string\": \"\"\n}".into();
        // Collapsed on screen, but the dump still carries the output.
        card.expanded = false;
        card.show_raw = false;

        let messages = vec![
            ChatMessage::new(MsgRole::User, "make it draggable"),
            thinking_card("the JS is a placeholder", false, 7),
            tool,
            ChatMessage::new(MsgRole::Agent, "Done."),
        ];
        let dump = transcript_text(&messages);

        // Roles, thinking, and the tool's name + raw call + full output all show,
        // regardless of the on-screen collapse state.
        assert!(dump.contains("You: make it draggable"), "{dump}");
        assert!(dump.contains("[thinking 7s]"), "{dump}");
        assert!(dump.contains("the JS is a placeholder"), "{dump}");
        assert!(dump.contains("[tool] edit index.html (ok)"), "{dump}");
        assert!(dump.contains("raw call:"), "{dump}");
        assert!(dump.contains("\"new_string\""), "{dump}");
        assert!(dump.contains("output:"), "{dump}");
        assert!(dump.contains("-<script>"), "{dump}");
        assert!(dump.contains("Suis: Done."), "{dump}");
    }

    #[test]
    fn transcript_text_is_empty_for_no_messages() {
        assert_eq!(transcript_text(&[]), "");
    }

    #[test]
    fn developer_raw_call_shows_under_an_edit_diff() {
        // An edit card with developer mode armed: the raw call (name + args)
        // renders above the always-shown diff, so an empty/truncated new_string
        // is visible next to the change it produced.
        let mut msg = ChatMessage::tool_running("edit", "index.html");
        let card = msg.tool.as_mut().unwrap();
        card.status = ToolStatus::Ok;
        card.output = "Edited 'index.html'.\n--- index.html\n+++ index.html\n-<script>\n+".into();
        card.raw_call = "edit {\n  \"path\": \"index.html\",\n  \"new_string\": \"\"\n}".into();
        card.show_raw = true;
        let flat: String = lines(&[msg], W)
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(flat.contains("raw call:"), "raw call label missing: {flat}");
        assert!(flat.contains("\"new_string\""), "args missing: {flat}");
        // The diff still renders too.
        assert!(flat.contains("-<script>"), "diff missing: {flat}");
    }

    #[test]
    fn developer_raw_call_on_collapsible_card_only_shows_when_expanded() {
        // A read card with developer mode armed: collapsed shows the summary
        // alone (no raw call); expanding reveals the raw call before the output.
        let mut collapsed = completed_card(3, ToolStatus::Ok);
        let card = collapsed.tool.as_mut().unwrap();
        card.raw_call = "read {\n  \"path\": \"src/main.rs\"\n}".into();
        card.show_raw = true;
        let flat_collapsed: String = lines(&[collapsed.clone()], W)
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            !flat_collapsed.contains("raw call:"),
            "raw call must stay hidden while collapsed: {flat_collapsed}"
        );

        collapsed.tool.as_mut().unwrap().expanded = true;
        let flat_expanded: String = lines(&[collapsed], W)
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            flat_expanded.contains("raw call:"),
            "raw call should appear once expanded: {flat_expanded}"
        );
        assert!(flat_expanded.contains("src/main.rs"));
    }

    #[test]
    fn no_raw_call_without_developer_mode() {
        // The same edit card with developer mode off renders exactly as before:
        // no raw-call block, just the diff.
        let mut msg = ChatMessage::tool_running("edit", "a.rs");
        let card = msg.tool.as_mut().unwrap();
        card.status = ToolStatus::Ok;
        card.output = "Edited 'a.rs'.\n--- a.rs\n+++ a.rs\n-old\n+new".into();
        card.raw_call = "edit {\n  \"path\": \"a.rs\"\n}".into();
        card.show_raw = false;
        let flat: String = lines(&[msg], W)
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(!flat.contains("raw call:"), "raw call leaked: {flat}");
    }

    #[test]
    fn read_output_with_a_diff_like_line_still_collapses() {
        // A file whose contents include a line starting with `--- ` must not be
        // mistaken for an `edit` diff: the card stays collapsible (chevron) and
        // shows only its summary line until expanded.
        let mut msg = ChatMessage::tool_running("read", "MASTER.md");
        let card = msg.tool.as_mut().unwrap();
        card.status = ToolStatus::Ok;
        card.output = "intro\n--- old\nmore".into();
        let rendered = lines(&[msg], W);
        assert_eq!(rendered.len(), 1, "collapsed: summary line only");
        let summary: String = rendered[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            summary.contains("▸"),
            "a non-edit read card keeps its closed chevron"
        );
        assert!(!summary.contains("old"), "body hidden while collapsed");
    }

    #[test]
    fn agent_markdown_renders_styled_spans() {
        let msgs = vec![ChatMessage::new(MsgRole::Agent, "use `cargo test` here")];
        let rendered = lines(&msgs, W);
        assert_eq!(rendered.len(), 1);
        assert!(
            rendered[0]
                .spans
                .iter()
                .any(|s| s.content == "cargo test" && s.style.bg == Some(theme::CODE_BG)),
            "inline code span survives the header prefix"
        );
    }

    #[test]
    fn streaming_cursor_trails_styled_agent_lines() {
        let mut msg = ChatMessage::streaming_agent();
        msg.text = "first\n`code`".into();
        let rendered = lines(&[msg], W);
        assert_eq!(rendered.len(), 2);
        assert_eq!(rendered[1].spans.last().unwrap().content, "▊");
    }

    #[test]
    fn styled_system_message_keeps_its_spans_and_derives_text() {
        let styled = vec![
            Line::from("Available commands:"),
            Line::from(vec![
                Span::raw("  "),
                Span::styled("/help", Style::default().fg(theme::ACCENT)),
            ]),
        ];
        let msg = ChatMessage::system_styled(styled);
        assert_eq!(msg.role, MsgRole::System);
        assert_eq!(msg.text, "Available commands:\n  /help");

        let rendered = lines(&[msg], W);
        assert_eq!(rendered.len(), 2);
        assert!(
            rendered[1]
                .spans
                .iter()
                .any(|s| s.content == "/help" && s.style.fg == Some(theme::ACCENT)),
            "the accent span survives rendering"
        );
    }

    /// Render the transcript widget into a `w`×`h` buffer string.
    fn render_to_string(
        messages: &[ChatMessage],
        scroll: u16,
        detached: bool,
        streaming: bool,
    ) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    frame.area(),
                    messages,
                    scroll,
                    detached,
                    streaming,
                    None,
                )
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    /// Enough agent one-liners to overflow a 10-row viewport.
    fn tall_transcript() -> Vec<ChatMessage> {
        (0..30)
            .map(|i| ChatMessage::new(MsgRole::Agent, format!("answer {i}")))
            .collect()
    }

    #[test]
    fn detached_view_renders_a_scrollbar() {
        let screen = render_to_string(&tall_transcript(), 5, true, false);
        assert!(screen.contains('█'), "scrollbar thumb missing: {screen}");
    }

    #[test]
    fn pinned_view_renders_no_scrollbar_or_notice() {
        let messages = tall_transcript();
        let max = content_height(&messages, 36, None).saturating_sub(8);
        let screen = render_to_string(&messages, max, false, true);
        assert!(!screen.contains('█'), "no scrollbar while pinned");
        assert!(
            !screen.contains("new output below"),
            "no notice while pinned"
        );
    }

    #[test]
    fn detached_streaming_view_pins_the_new_output_notice() {
        let screen = render_to_string(&tall_transcript(), 5, true, true);
        assert!(
            screen.contains("new output below"),
            "notice missing: {screen}"
        );
    }

    #[test]
    fn detached_idle_view_shows_no_notice() {
        let screen = render_to_string(&tall_transcript(), 5, true, false);
        assert!(!screen.contains("new output below"));
    }

    #[test]
    fn working_status_trails_the_transcript() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let messages = vec![ChatMessage::new(MsgRole::User, "hello")];
        let status = "⠋ snuffling · 3s · Esc to interrupt";
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &messages, 0, false, true, Some(status)))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            screen.contains("snuffling"),
            "status line missing: {screen}"
        );
        // The status also counts toward the scroll math: card (4) + spacer + status.
        assert_eq!(content_height(&messages, 56, Some(status)), 6);
        assert_eq!(content_height(&messages, 56, None), 4);
    }

    #[test]
    fn long_words_are_hard_broken() {
        let wrapped = wrap_text("abcdefghij", 4);
        assert_eq!(wrapped, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn wrapping_breaks_on_word_boundaries() {
        let wrapped = wrap_text("the quick brown fox", 9);
        assert_eq!(wrapped, vec!["the quick", "brown fox"]);
    }
}
