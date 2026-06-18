//! The task sidebar.
//!
//! Renders the session task list with a per-status glyph. The chat screen only
//! allocates space for this panel when there are tasks to show and the panel is
//! toggled on (see `/tasks`).

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};
use ratatui::Frame;

use crate::theme;
use crate::widgets::message_list::wrap_text;
use suis_agent::{Task, TaskStatus};

/// Render the task panel for `tasks` into `area`. When an implementation
/// session is running, `step` carries the current step's title, drawn as a
/// header above its tasks (the step lives here now, not in the input border).
///
/// The step header and task titles wrap onto extra lines rather than being
/// truncated; words stay whole, and a task's continuation rows indent to sit
/// under its title (past the status icon).
pub fn render(frame: &mut Frame, area: Rect, tasks: &[Task], step: Option<&str>) {
    // Inner text width: the panel area minus the border (2) and the block's
    // horizontal padding (1 each side).
    let inner_width = area.width.saturating_sub(4) as usize;
    let mut lines: Vec<Line> = Vec::new();

    // The current step heads the panel, so its tasks read as belonging to it.
    if let Some(step) = step {
        let style = Style::default()
            .fg(theme::INFO)
            .add_modifier(Modifier::BOLD);
        for piece in wrap_text(step, inner_width) {
            lines.push(Line::from(Span::styled(piece, style)));
        }
        lines.push(Line::from(""));
    }

    if tasks.is_empty() {
        lines.push(Line::from(Span::styled(
            "(no tasks)",
            Style::default().fg(theme::TEXT_FAINT),
        )));
    } else {
        for task in tasks {
            let icon_style = Style::default().fg(status_color(task.status));
            let text_style = Style::default().fg(theme::TEXT);
            // The status icon plus its trailing space takes two columns; wrap
            // the title into what remains and indent continuation rows to match.
            for (i, piece) in wrap_text(&task.title, inner_width.saturating_sub(2))
                .into_iter()
                .enumerate()
            {
                let lead = if i == 0 {
                    Span::styled(format!("{} ", task.status.icon()), icon_style)
                } else {
                    Span::raw("  ")
                };
                lines.push(Line::from(vec![lead, Span::styled(piece, text_style)]));
            }
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER))
        .style(Style::default().bg(theme::BG))
        .padding(Padding::horizontal(1))
        .title(Span::styled(
            "Tasks",
            Style::default()
                .fg(theme::WARN)
                .add_modifier(Modifier::BOLD),
        ));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn status_color(status: TaskStatus) -> Color {
    match status {
        TaskStatus::Todo => theme::TEXT_DIM,
        TaskStatus::Doing => theme::INFO,
        TaskStatus::Done => theme::ACCENT,
        TaskStatus::Blocked => theme::DANGER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render_to_string(tasks: &[Task], step: Option<&str>) -> String {
        let mut terminal = Terminal::new(TestBackend::new(30, 10)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), tasks, step))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn task(title: &str) -> Task {
        Task {
            id: "w1".into(),
            title: title.into(),
            status: TaskStatus::Doing,
        }
    }

    #[test]
    fn step_header_precedes_the_tasks() {
        let screen = render_to_string(&[task("write login route")], Some("tokens"));
        assert!(screen.contains("Tasks"), "panel title missing");
        assert!(screen.contains("tokens"), "step header missing");
        assert!(screen.contains("write login route"), "task row missing");
    }

    #[test]
    fn no_step_means_no_header() {
        // Without an implementation session there is no step line, just tasks.
        let screen = render_to_string(&[task("do it")], None);
        assert!(screen.contains("do it"));
    }

    #[test]
    fn long_titles_wrap_without_breaking_words() {
        // A title wider than the 30-col panel flows onto a second row with its
        // words intact rather than being truncated.
        let screen = render_to_string(
            &[task("implement the refresh token rotation endpoint")],
            None,
        );
        assert!(screen.contains("rotation"), "later words survive the wrap");
        assert!(
            !screen.contains("rota tion"),
            "words are not split mid-word"
        );
    }

    #[test]
    fn empty_tasks_still_show_the_step_and_empty_state() {
        let screen = render_to_string(&[], Some("tokens"));
        assert!(screen.contains("tokens"), "step header missing");
        assert!(screen.contains("(no tasks)"), "empty state missing");
    }
}
