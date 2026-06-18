//! The plan-draft review pane.
//!
//! Shown when the agent proposes a plan
//! ([`AgentEvent::PlanProposal`](suis_agent::AgentEvent)): the draft's title,
//! description, and steps with their work/verify tasks, rendered as the
//! checklist it will become. Nothing touches `.suis/plans.json` until the user
//! approves here.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};
use ratatui::Frame;

use suis_agent::PlanDraft;

use crate::theme;
use crate::widgets::message_list::wrap_text;
use crate::widgets::permission_prompt::centered_rect;

/// Render the review pane for `draft` as a centered overlay, scrolled vertically
/// to `scroll_y`. A long plan exceeds the popup, so the body scrolls rather than
/// being cut off; lines word-wrap to the popup width, so step and task titles
/// flow onto extra lines (words stay whole) instead of being truncated. The
/// requested offset is clamped to the measured content and the clamped
/// `(scroll_y, scroll_x)` is returned, so the caller's stored offsets never run
/// past the end (`scroll_x` clamps to 0 now that nothing overflows horizontally).
pub fn render(
    frame: &mut Frame,
    area: Rect,
    draft: &PlanDraft,
    scroll_y: u16,
    scroll_x: u16,
) -> (u16, u16) {
    let popup = centered_rect(80, 80, area);
    frame.render_widget(Clear, popup);

    // Inner text width: the popup minus the border (2) and the block's
    // horizontal padding (1 each side). Titles wrap into this width.
    let inner_width = popup.width.saturating_sub(4) as usize;

    // Append `text`, word-wrapped to the inner width, as `style`d lines.
    let push_wrapped = |lines: &mut Vec<Line>, text: &str, style: Style| {
        for piece in wrap_text(text, inner_width) {
            lines.push(Line::from(Span::styled(piece, style)));
        }
    };
    // Append a `prefix`ed task title, wrapping into the room left by the prefix
    // and indenting continuation rows to sit under the title text.
    let push_task = |lines: &mut Vec<Line>, prefix: &str, title: &str, style: Style| {
        let indent = " ".repeat(prefix.chars().count());
        for (i, piece) in wrap_text(title, inner_width.saturating_sub(prefix.chars().count()))
            .into_iter()
            .enumerate()
        {
            let lead = if i == 0 { prefix } else { indent.as_str() };
            lines.push(Line::from(Span::styled(format!("{lead}{piece}"), style)));
        }
    };

    let header = match &draft.revises {
        Some(id) => format!("Plan Revision ({id})"),
        None => "Plan Proposal".to_string(),
    };
    let mut lines: Vec<Line> = Vec::new();
    push_wrapped(
        &mut lines,
        &header,
        Style::default()
            .fg(theme::WARN)
            .add_modifier(Modifier::BOLD),
    );
    lines.push(Line::from(""));
    push_wrapped(
        &mut lines,
        &draft.title,
        Style::default()
            .fg(theme::TEXT_BRIGHT)
            .add_modifier(Modifier::BOLD),
    );
    if !draft.description.is_empty() {
        push_wrapped(
            &mut lines,
            &draft.description,
            Style::default().fg(theme::TEXT_DIM),
        );
    }

    for (idx, step) in draft.steps.iter().enumerate() {
        lines.push(Line::from(""));
        push_wrapped(
            &mut lines,
            &format!("Step {}: {}", idx + 1, step.title),
            Style::default()
                .fg(theme::INFO)
                .add_modifier(Modifier::BOLD),
        );
        for task in &step.work_tasks {
            push_task(
                &mut lines,
                "  □ ",
                &task.title,
                Style::default().fg(theme::TEXT),
            );
        }
        for task in &step.verify_tasks {
            push_task(
                &mut lines,
                "  □ verify: ",
                &task.title,
                Style::default().fg(theme::TEXT_DIM),
            );
        }
    }

    // Clamp the scroll to the content: the inner area is the popup minus the
    // border (2 each axis) and the block's horizontal padding (1 each side).
    let inner_height = popup.height.saturating_sub(2);
    let inner_width = popup.width.saturating_sub(4);
    let total_lines = lines.len() as u16;
    let widest = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let max_y = total_lines.saturating_sub(inner_height);
    let max_x = widest.saturating_sub(inner_width);
    let scroll_y = scroll_y.min(max_y);
    let scroll_x = scroll_x.min(max_x);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::WARN))
        .style(Style::default().bg(theme::BG))
        .padding(Padding::horizontal(1));
    // Lines are pre-wrapped to the inner width, so `widest <= inner_width` and
    // `scroll_x` clamps to 0; vertical scroll handles a plan taller than the popup.
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((scroll_y, scroll_x)),
        popup,
    );
    (scroll_y, scroll_x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use suis_core::{PlanStep, PlanTask};

    /// A plan with `n` steps, used to overflow a short popup.
    fn tall_draft(n: usize) -> PlanDraft {
        PlanDraft {
            revises: None,
            title: "Auth".into(),
            description: String::new(),
            steps: (0..n)
                .map(|i| PlanStep {
                    title: format!("step {i}"),
                    work_tasks: vec![PlanTask::new("do a thing")],
                    verify_tasks: vec![],
                })
                .collect(),
        }
    }

    fn clamp(draft: &PlanDraft, w: u16, h: u16, y: u16, x: u16) -> (u16, u16) {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut out = (0, 0);
        terminal
            .draw(|frame| out = render(frame, frame.area(), draft, y, x))
            .unwrap();
        out
    }

    #[test]
    fn vertical_scroll_is_clamped_to_the_content() {
        let draft = tall_draft(40);
        // A wildly out-of-range offset settles at the last screenful, not past it.
        let (y, _) = clamp(&draft, 80, 20, 9_999, 0);
        assert!(y > 0, "a tall plan can scroll down");
        assert!(y < 9_999, "but not past the end");
        // Re-clamping the returned offset is a fixed point.
        let (y2, _) = clamp(&draft, 80, 20, y, 0);
        assert_eq!(y, y2);
    }

    #[test]
    fn short_plan_does_not_scroll() {
        let draft = tall_draft(1);
        let (y, x) = clamp(&draft, 120, 40, 50, 50);
        assert_eq!(
            (y, x),
            (0, 0),
            "content fits, so both offsets clamp to zero"
        );
    }
}
