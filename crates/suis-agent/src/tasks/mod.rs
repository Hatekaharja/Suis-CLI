//! Session-scoped task tracking, visible to both the user and the agent.
//!
//! The store is in-memory only (MVP: not persisted across sessions). The
//! `task` tool mutates it; the UI renders it; the context assembler injects the
//! active task into each request.

pub mod store;
pub mod types;

pub use store::TaskStore;
pub use types::{Task, TaskStatus};

/// The display tasks for one plan step, with the derived positional ids the
/// `task` tool addresses during an implementation session: `w1..` for work
/// tasks, `v1..` for verify tasks.
pub fn plan_step_tasks(step: &suis_core::PlanStep) -> Vec<Task> {
    let numbered = |prefix: char, tasks: &[suis_core::PlanTask]| {
        tasks
            .iter()
            .enumerate()
            .map(|(i, t)| Task {
                id: format!("{prefix}{}", i + 1),
                title: t.title.clone(),
                status: t.status,
            })
            .collect::<Vec<_>>()
    };
    let mut out = numbered('w', &step.work_tasks);
    out.extend(numbered('v', &step.verify_tasks));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use suis_core::{PlanStep, PlanTask};

    #[test]
    fn plan_step_tasks_number_work_then_verify() {
        let step = PlanStep {
            title: "s".into(),
            work_tasks: vec![PlanTask::new("a"), PlanTask::new("b")],
            verify_tasks: vec![PlanTask::new("check")],
        };
        let tasks = plan_step_tasks(&step);
        let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["w1", "w2", "v1"]);
        assert_eq!(tasks[2].title, "check");
        assert!(tasks.iter().all(|t| t.status == TaskStatus::Todo));
    }
}
