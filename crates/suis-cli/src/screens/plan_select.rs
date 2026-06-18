//! The `/implement` plan-selection screen and its view-model.
//!
//! Two levels: a list of stored plans with `[done/total steps]` progress, and
//! (after Enter) a drill-down into one plan to choose "whole plan" — which
//! resolves to its first incomplete step — or a single step. The view-model
//! ([`PlanSelect`]) is pure and unit-tested; [`render`] draws it, reusing the
//! model-select list interaction patterns.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use suis_core::PlanStore;

use crate::theme;
use crate::widgets::{footer, list_frame};

/// One step's display row.
#[derive(Debug, Clone)]
pub struct StepRow {
    /// Step title.
    pub title: String,
    /// Whether every work and verify task is done.
    pub complete: bool,
    /// `(done, total)` across the step's work + verify tasks.
    pub tasks: (usize, usize),
}

/// One plan's display row, with the data the drill-down needs.
#[derive(Debug, Clone)]
pub struct PlanRow {
    /// The plan's store id.
    pub id: String,
    /// Plan title.
    pub title: String,
    /// `(done, total)` step counts.
    pub progress: (usize, usize),
    /// Index of the first incomplete step ("whole plan" resolves here).
    pub next_step: Option<usize>,
    /// The steps, for the drill-down list.
    pub steps: Vec<StepRow>,
}

/// What the user picked: a plan step to implement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSelection {
    /// The plan's store id.
    pub plan_id: String,
    /// Zero-based step index.
    pub step_index: usize,
    /// The plan (project) title.
    pub plan_title: String,
    /// The chosen step's title.
    pub step_title: String,
}

/// The plan-selection view-model: plan list, cursor, and drill-down state.
#[derive(Debug, Clone, Default)]
pub struct PlanSelect {
    plans: Vec<PlanRow>,
    /// Cursor in the plan list.
    cursor: usize,
    /// `Some(plan index)` while choosing a step within that plan.
    drilled: Option<usize>,
    /// Cursor in the drill-down: 0 = whole plan, 1..=n = step n.
    step_cursor: usize,
    /// A one-line notice (e.g. "plan already complete").
    pub notice: Option<String>,
}

impl PlanSelect {
    /// Build the view-model from the stored plans.
    pub fn from_store(store: &PlanStore) -> Self {
        let plans = store
            .plans
            .iter()
            .map(|plan| PlanRow {
                id: plan.id.clone(),
                title: plan.title.clone(),
                progress: plan.progress(),
                next_step: plan.next_step(),
                steps: plan
                    .steps
                    .iter()
                    .map(|step| {
                        let all: Vec<_> =
                            step.work_tasks.iter().chain(&step.verify_tasks).collect();
                        let done = all
                            .iter()
                            .filter(|t| t.status == suis_core::TaskStatus::Done)
                            .count();
                        StepRow {
                            title: step.title.clone(),
                            complete: step.is_complete(),
                            tasks: (done, all.len()),
                        }
                    })
                    .collect(),
            })
            .collect();
        PlanSelect {
            plans,
            ..Default::default()
        }
    }

    /// The plan rows, for rendering.
    pub fn plans(&self) -> &[PlanRow] {
        &self.plans
    }

    /// The plan being drilled into, if any.
    pub fn drilled(&self) -> Option<&PlanRow> {
        self.drilled.and_then(|i| self.plans.get(i))
    }

    /// The active cursor position: the plan cursor at the top level, the
    /// step cursor (0 = whole plan) when drilled in.
    pub fn cursor(&self) -> usize {
        if self.drilled.is_some() {
            self.step_cursor
        } else {
            self.cursor
        }
    }

    /// Move the cursor up one, wrapping.
    pub fn move_up(&mut self) {
        let len = self.list_len();
        if len > 0 {
            let cur = self.cursor_mut();
            *cur = (*cur + len - 1) % len;
        }
    }

    /// Move the cursor down one, wrapping.
    pub fn move_down(&mut self) {
        let len = self.list_len();
        if len > 0 {
            let cur = self.cursor_mut();
            *cur = (*cur + 1) % len;
        }
    }

    /// Enter: drill into the highlighted plan, or resolve the highlighted
    /// drill-down entry to a [`PlanSelection`]. "Whole plan" resolves to the
    /// first incomplete step; if the plan is already complete it sets a notice
    /// and returns `None`.
    pub fn enter(&mut self) -> Option<PlanSelection> {
        self.notice = None;
        match self.drilled {
            None => {
                if !self.plans.is_empty() {
                    self.drilled = Some(self.cursor);
                    self.step_cursor = 0;
                }
                None
            }
            Some(plan_idx) => {
                let plan = self.plans.get(plan_idx)?;
                let step_index = if self.step_cursor == 0 {
                    match plan.next_step {
                        Some(next) => next,
                        None => {
                            self.notice = Some("This plan is already complete.".to_string());
                            return None;
                        }
                    }
                } else {
                    self.step_cursor - 1
                };
                let step = plan.steps.get(step_index)?;
                Some(PlanSelection {
                    plan_id: plan.id.clone(),
                    step_index,
                    plan_title: plan.title.clone(),
                    step_title: step.title.clone(),
                })
            }
        }
    }

    /// Esc: back out of the drill-down. Returns `false` when already at the
    /// top level (the caller should leave the screen).
    pub fn back(&mut self) -> bool {
        self.notice = None;
        if self.drilled.take().is_some() {
            self.step_cursor = 0;
            true
        } else {
            false
        }
    }

    fn list_len(&self) -> usize {
        match self.drilled {
            None => self.plans.len(),
            // "Whole plan" + each step.
            Some(i) => self.plans.get(i).map(|p| p.steps.len() + 1).unwrap_or(0),
        }
    }

    fn cursor_mut(&mut self) -> &mut usize {
        if self.drilled.is_some() {
            &mut self.step_cursor
        } else {
            &mut self.cursor
        }
    }
}

/// Render the plan-selection screen.
pub fn render(frame: &mut Frame, area: Rect, state: &PlanSelect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // title
            Constraint::Min(1),    // list
            Constraint::Length(1), // footer
        ])
        .split(area);

    let title = match state.drilled() {
        Some(plan) => format!("Implement: {}", plan.title),
        None => "Implement a Plan".to_string(),
    };
    list_frame::render_title(frame, chunks[0], &title);

    let lines = match state.drilled() {
        Some(plan) => step_lines(plan, state.cursor()),
        None => plan_lines(state),
    };
    frame.render_widget(Paragraph::new(lines).block(list_frame::block()), chunks[1]);

    // A notice ("plan already complete") takes the footer row over the hints.
    match &state.notice {
        Some(notice) => frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {notice}"),
                Style::default().fg(theme::WARN),
            ))),
            chunks[2],
        ),
        None if state.drilled.is_some() => footer::render(
            frame,
            chunks[2],
            &[("Enter", "implement"), ("↑/↓", "navigate"), ("Esc", "back")],
        ),
        None => footer::render(
            frame,
            chunks[2],
            &[
                ("Enter", "choose plan"),
                ("↑/↓", "navigate"),
                ("Esc", "cancel"),
            ],
        ),
    }
}

fn plan_lines(state: &PlanSelect) -> Vec<Line<'static>> {
    if state.plans().is_empty() {
        return vec![list_frame::empty_line("no plans")];
    }
    state
        .plans()
        .iter()
        .enumerate()
        .map(|(idx, plan)| {
            let (style, marker) = list_frame::row_style(idx == state.cursor());
            let (done, total) = plan.progress;
            Line::from(vec![
                Span::styled(marker, style),
                Span::styled(format!("{:<28}", plan.id), style),
                list_frame::badge(format!("[{done}/{total} steps]  ")),
                Span::styled(plan.title.clone(), Style::default().fg(theme::TEXT)),
            ])
        })
        .collect()
}

fn step_lines(plan: &PlanRow, cursor: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(plan.steps.len() + 1);
    let (style, marker) = list_frame::row_style(cursor == 0);
    let whole = match plan.next_step.and_then(|i| plan.steps.get(i)) {
        Some(next) => format!("Whole plan (next: {})", next.title),
        None => "Whole plan (complete)".to_string(),
    };
    lines.push(Line::from(vec![
        Span::styled(marker, style),
        Span::styled(whole, style),
    ]));
    for (idx, step) in plan.steps.iter().enumerate() {
        let (style, marker) = list_frame::row_style(cursor == idx + 1);
        let glyph = if step.complete { "✓" } else { "□" };
        let (done, total) = step.tasks;
        lines.push(Line::from(vec![
            Span::styled(marker, style),
            Span::styled(
                format!("{glyph} "),
                Style::default().fg(if step.complete {
                    theme::ACCENT
                } else {
                    theme::TEXT_DIM
                }),
            ),
            Span::styled(format!("{}. {:<32}", idx + 1, step.title), style),
            list_frame::badge(format!("[{done}/{total} tasks]")),
        ]));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use suis_core::{PlanStep, PlanTask, TaskStatus};

    fn store() -> PlanStore {
        let mut store = PlanStore::default();
        let mut done_step = PlanStep {
            title: "tokens".into(),
            work_tasks: vec![PlanTask::new("a")],
            verify_tasks: vec![PlanTask::new("check")],
        };
        done_step.work_tasks[0].status = TaskStatus::Done;
        done_step.verify_tasks[0].status = TaskStatus::Done;
        store.insert(
            "Auth",
            "",
            vec![
                done_step,
                PlanStep {
                    title: "middleware".into(),
                    work_tasks: vec![PlanTask::new("b"), PlanTask::new("c")],
                    verify_tasks: vec![],
                },
            ],
        );
        store.insert(
            "Search",
            "",
            vec![PlanStep {
                title: "index".into(),
                work_tasks: vec![PlanTask::new("d")],
                verify_tasks: vec![],
            }],
        );
        store
    }

    #[test]
    fn progress_counts_steps_and_tasks() {
        let view = PlanSelect::from_store(&store());
        assert_eq!(view.plans().len(), 2);
        let auth = &view.plans()[0];
        assert_eq!(auth.progress, (1, 2));
        assert_eq!(auth.next_step, Some(1));
        assert!(auth.steps[0].complete);
        assert_eq!(auth.steps[0].tasks, (2, 2));
        assert_eq!(auth.steps[1].tasks, (0, 2));
    }

    #[test]
    fn drill_down_and_step_selection() {
        let mut view = PlanSelect::from_store(&store());
        // Enter on a plan drills in rather than selecting.
        assert_eq!(view.enter(), None);
        assert!(view.drilled().is_some());

        // Step entries are offset by the "whole plan" row.
        view.move_down(); // -> step 1
        view.move_down(); // -> step 2
        let sel = view.enter().expect("step selected");
        assert_eq!(sel.plan_id, "auth");
        assert_eq!(sel.step_index, 1);
        assert_eq!(sel.plan_title, "Auth");
        assert_eq!(sel.step_title, "middleware");
    }

    #[test]
    fn whole_plan_resolves_to_first_incomplete_step() {
        let mut view = PlanSelect::from_store(&store());
        view.enter(); // drill into "auth"
        let sel = view.enter().expect("whole plan resolves");
        // Step 0 is complete, so the whole plan starts at step 1.
        assert_eq!(sel.step_index, 1);
    }

    #[test]
    fn complete_plan_sets_notice_instead_of_selecting() {
        let mut plans = store();
        // Finish the second plan entirely.
        plans.get_mut("search").unwrap().steps[0].work_tasks[0].status = TaskStatus::Done;
        let mut view = PlanSelect::from_store(&plans);
        view.move_down(); // -> "search"
        view.enter(); // drill in
        assert_eq!(view.enter(), None);
        assert!(view.notice.as_deref().unwrap().contains("already complete"));
    }

    #[test]
    fn back_unwinds_drill_then_exits() {
        let mut view = PlanSelect::from_store(&store());
        view.enter();
        assert!(view.back(), "first Esc backs out of the drill-down");
        assert!(view.drilled().is_none());
        assert!(!view.back(), "second Esc leaves the screen");
    }

    #[test]
    fn navigation_wraps_in_both_levels() {
        let mut view = PlanSelect::from_store(&store());
        view.move_up();
        assert_eq!(view.cursor(), 1);
        view.move_down();
        assert_eq!(view.cursor(), 0);

        view.enter(); // drill into "auth": rows = whole + 2 steps
        view.move_up();
        assert_eq!(view.cursor(), 2);
        view.move_down();
        assert_eq!(view.cursor(), 0);
    }

    #[test]
    fn renders_progress_and_drill_down() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut view = PlanSelect::from_store(&store());
        let draw = |view: &PlanSelect| -> String {
            let mut terminal = Terminal::new(TestBackend::new(90, 16)).unwrap();
            terminal
                .draw(|frame| render(frame, frame.area(), view))
                .unwrap();
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect()
        };

        let top = draw(&view);
        assert!(top.contains("auth"));
        assert!(top.contains("[1/2 steps]"));

        view.enter();
        let drilled = draw(&view);
        assert!(drilled.contains("Whole plan (next: middleware)"));
        assert!(drilled.contains("middleware"));
        assert!(drilled.contains("[0/2 tasks]"));
    }
}
