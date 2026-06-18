//! The `task` tool: let the agent manage its session task list.
//!
//! Actions: `create` (a new `Todo`), `update` (change status by id), and
//! `list` (render the full list). Mutates the shared [`TaskStore`].
//!
//! During an implementation session (`/implement` — `ctx.implement` is set)
//! the tool is *fenced* to one task: the agent's only move is to settle the
//! **current** task — mark it `done`, or `blocked` when it cannot be completed.
//! The driver owns the `todo`→`doing` transition and the plan's structure is
//! fixed, so `doing`/`todo`, `list`, `create`, and updates to any other task are
//! refused. Accepted changes persist to `.suis/plans.json` immediately;
//! discovered extra work is reported to the user, and plan changes go through
//! Plan mode's approval gate.

use serde_json::{json, Value};

use suis_core::PlanStore;

use super::{opt_str, require_str, Tool, ToolContext, ToolDefinition, ToolOutcome};
use crate::runtime::session::ImplementTarget;
use crate::tasks::{plan_step_tasks, TaskStatus};

/// Manages session tasks on behalf of the agent.
pub struct TaskTool;

impl Tool for TaskTool {
    fn name(&self) -> &'static str {
        "task"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Track work as tasks. action=create needs a 'title'; \
                          action=update needs 'id' and 'status' (todo|doing|done|blocked); \
                          action=list returns the current tasks."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create", "update", "list"],
                        "description": "The task operation to perform."
                    },
                    "title": { "type": "string", "description": "Title for a new task (create)." },
                    "id": { "type": "string", "description": "Task id (update)." },
                    "status": {
                        "type": "string",
                        "enum": ["todo", "doing", "done", "blocked"],
                        "description": "New status (update)."
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: &Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let action = require_str(args, "action")?;
        if let Some(target) = ctx.implement {
            return execute_plan_backed(&action, args, ctx, target);
        }
        let mut store = ctx
            .tasks
            .lock()
            .map_err(|_| "task store lock poisoned".to_string())?;

        match action.as_str() {
            "create" => {
                let title = require_str(args, "title")?;
                let id = store.create(title.clone());
                Ok(format!("Created task {id}: {title}"))
            }
            "update" => {
                let id = require_str(args, "id")?;
                let status_str = require_str(args, "status")?;
                let status = TaskStatus::parse(&status_str)
                    .ok_or_else(|| format!("invalid status '{status_str}'"))?;
                if store.update(&id, status) {
                    let title = store.get(&id).map(|t| t.title.as_str()).unwrap_or(&id);
                    Ok(format!("{title} → {status_str}"))
                } else {
                    Err(format!("no task with id '{id}'"))
                }
            }
            "list" => Ok(render(&store)),
            other => {
                // Tolerate a `status` shorthand: action could be a status word.
                let _ = opt_str(args, "status");
                Err(format!("unknown task action '{other}'"))
            }
        }
    }
}

/// Run a task action against the active plan step's tasks. The session is
/// *fenced* to a single task: the only legal move is settling the **current**
/// task — `done`, or `blocked` when it genuinely cannot be completed. The driver
/// owns the `todo`→`doing` transition, and the plan's structure is fixed, so
/// `doing`/`todo`, `list`, `create`, and updates to any other task are refused.
/// Every accepted change is persisted to `.suis/plans.json` before returning, so
/// progress survives a crash or exit mid-step.
fn execute_plan_backed(
    action: &str,
    args: &Value,
    ctx: &ToolContext<'_>,
    target: &ImplementTarget,
) -> ToolOutcome {
    if action != "update" {
        // One task at a time: no listing the plan, adding work, or restructuring.
        return Err(
            "only 'update' is available during an implementation session — mark the \
             current task 'done' when it is finished, or 'blocked' if you cannot. The \
             plan's tasks are fixed; report any missing work to the user."
                .to_string(),
        );
    }

    let id = require_str(args, "id")?;
    let status_str = require_str(args, "status")?;
    let status =
        TaskStatus::parse(&status_str).ok_or_else(|| format!("invalid status '{status_str}'"))?;
    // The driver sets the current task to `doing` for the agent; the agent only
    // settles it. Anything else (re-`doing`, reverting to `todo`) is refused.
    if !matches!(status, TaskStatus::Done | TaskStatus::Blocked) {
        return Err(
            "task status is managed automatically — just mark the current task 'done' \
             when it is finished, or 'blocked' if you cannot complete it"
                .to_string(),
        );
    }

    let mut store = PlanStore::load(ctx.workspace).map_err(|e| e.to_string())?;
    let plan = store
        .get_mut(&target.plan_id)
        .ok_or_else(|| format!("no plan with id '{}'", target.plan_id))?;
    let step = plan
        .steps
        .get_mut(target.step_index)
        .ok_or_else(|| format!("plan has no step {}", target.step_index + 1))?;

    // Only the current task — the one the driver put in `doing` — may be settled.
    match current_task_id(step) {
        Some(current) if current == id => {}
        Some(current) => return Err(format!("you can only update the current task ({current})")),
        None => return Err("there is no current task to update".to_string()),
    }

    let task = step
        .task_by_id_mut(&id)
        .ok_or_else(|| format!("no task with id '{id}' in this step"))?;
    task.status = status;
    let title = task.title.clone();

    let step_complete = step.is_complete();
    let plan_complete = plan.is_complete();
    store.save(ctx.workspace).map_err(|e| e.to_string())?;

    let mut message = format!("{title} → {status_str}");
    if step_complete {
        message.push_str("\nAll tasks in this step are complete.");
        if plan_complete {
            message.push_str(" The whole plan is now complete.");
        }
    }
    Ok(message)
}

/// The id of the task currently in play — the first not-yet-settled task (`todo`
/// or `doing`), work tasks before verify. Mirrors what the driver points at and
/// what the work package shows, so the tool fences edits to that one task.
fn current_task_id(step: &suis_core::PlanStep) -> Option<String> {
    plan_step_tasks(step)
        .into_iter()
        .find(|t| matches!(t.status, TaskStatus::Todo | TaskStatus::Doing))
        .map(|t| t.id)
}

fn render(store: &crate::tasks::TaskStore) -> String {
    if store.is_empty() {
        return "No tasks.".to_string();
    }
    store
        .all()
        .iter()
        .map(|t| format!("{} {} {}", t.status.icon(), t.id, t.title))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::TaskStatus;
    use crate::test_util::Fixture;

    #[test]
    fn create_adds_to_store() {
        let fx = Fixture::new();
        let out = TaskTool
            .execute(
                &json!({ "action": "create", "title": "analyze" }),
                &fx.ctx(),
            )
            .unwrap();
        assert!(out.contains("analyze"));
        assert_eq!(fx.tasks.lock().unwrap().all().len(), 1);
    }

    #[test]
    fn update_changes_status() {
        let fx = Fixture::new();
        let id = fx.tasks.lock().unwrap().create("write tests");
        TaskTool
            .execute(
                &json!({ "action": "update", "id": id, "status": "doing" }),
                &fx.ctx(),
            )
            .unwrap();
        assert_eq!(
            fx.tasks.lock().unwrap().active().unwrap().status,
            TaskStatus::Doing
        );
    }

    #[test]
    fn update_unknown_id_errors() {
        let fx = Fixture::new();
        let err = TaskTool
            .execute(
                &json!({ "action": "update", "id": "tX", "status": "done" }),
                &fx.ctx(),
            )
            .unwrap_err();
        assert!(err.contains("no task"));
    }

    #[test]
    fn list_renders_tasks() {
        let fx = Fixture::new();
        fx.tasks.lock().unwrap().create("first");
        let out = TaskTool
            .execute(&json!({ "action": "list" }), &fx.ctx())
            .unwrap();
        assert!(out.contains("first"));
    }

    use suis_core::{PlanStep, PlanTask};

    /// A fixture with a stored one-step plan ("impl": 2 work, 1 verify) and an
    /// active implementation target pointing at it.
    fn implement_fixture() -> Fixture {
        let mut fx = Fixture::new();
        let mut store = PlanStore::default();
        store.insert(
            "Impl",
            "",
            vec![PlanStep {
                title: "step one".into(),
                work_tasks: vec![PlanTask::new("write code"), PlanTask::new("wire it up")],
                verify_tasks: vec![PlanTask::new("run tests")],
            }],
        );
        store.save(&fx.workspace).unwrap();
        fx.implement = Some(ImplementTarget {
            plan_id: "impl".into(),
            step_index: 0,
        });
        fx
    }

    #[test]
    fn plan_backed_update_persists_to_plans_json() {
        let fx = implement_fixture();
        let out = TaskTool
            .execute(
                &json!({ "action": "update", "id": "w1", "status": "done" }),
                &fx.ctx(),
            )
            .unwrap();
        assert!(out.contains("write code"));

        // The change reached disk, not just memory.
        let store = PlanStore::load(&fx.workspace).unwrap();
        let step = &store.get("impl").unwrap().steps[0];
        assert_eq!(step.work_tasks[0].status, TaskStatus::Done);
        assert_eq!(step.work_tasks[1].status, TaskStatus::Todo);
        // The session task store was not touched.
        assert!(fx.tasks.lock().unwrap().is_empty());
    }

    #[test]
    fn plan_backed_verify_id_settles_once_it_is_current() {
        let fx = implement_fixture();
        // A verify task only becomes the current task after the work tasks
        // settle; then its derived `v` id resolves and it can be marked done.
        for id in ["w1", "w2"] {
            TaskTool
                .execute(
                    &json!({ "action": "update", "id": id, "status": "done" }),
                    &fx.ctx(),
                )
                .unwrap();
        }
        TaskTool
            .execute(
                &json!({ "action": "update", "id": "v1", "status": "done" }),
                &fx.ctx(),
            )
            .unwrap();
        let store = PlanStore::load(&fx.workspace).unwrap();
        assert_eq!(
            store.get("impl").unwrap().steps[0].verify_tasks[0].status,
            TaskStatus::Done
        );
    }

    #[test]
    fn plan_backed_only_the_current_task_can_be_settled() {
        let fx = implement_fixture();
        // w1 is current (first todo); marking a later task is refused, naming the
        // task the agent may actually settle.
        let err = TaskTool
            .execute(
                &json!({ "action": "update", "id": "w2", "status": "done" }),
                &fx.ctx(),
            )
            .unwrap_err();
        assert!(err.contains("current task (w1)"), "{err}");
        // The current task itself settles fine.
        TaskTool
            .execute(
                &json!({ "action": "update", "id": "w1", "status": "done" }),
                &fx.ctx(),
            )
            .unwrap();
        let store = PlanStore::load(&fx.workspace).unwrap();
        assert_eq!(
            store.get("impl").unwrap().steps[0].work_tasks[0].status,
            TaskStatus::Done
        );
    }

    #[test]
    fn plan_backed_doing_and_todo_are_refused() {
        let fx = implement_fixture();
        for status in ["doing", "todo"] {
            let err = TaskTool
                .execute(
                    &json!({ "action": "update", "id": "w1", "status": status }),
                    &fx.ctx(),
                )
                .unwrap_err();
            assert!(err.contains("managed automatically"), "{status}: {err}");
        }
        // Nothing moved off todo.
        let store = PlanStore::load(&fx.workspace).unwrap();
        assert_eq!(
            store.get("impl").unwrap().steps[0].work_tasks[0].status,
            TaskStatus::Todo
        );
    }

    #[test]
    fn plan_backed_structure_changes_are_refused() {
        let fx = implement_fixture();
        let err = TaskTool
            .execute(
                &json!({ "action": "create", "title": "extra work" }),
                &fx.ctx(),
            )
            .unwrap_err();
        assert!(err.contains("implementation session"), "{err}");
        // Nothing was added.
        let store = PlanStore::load(&fx.workspace).unwrap();
        assert_eq!(store.get("impl").unwrap().steps[0].work_tasks.len(), 2);
    }

    #[test]
    fn plan_backed_non_current_id_is_refused() {
        let fx = implement_fixture();
        // A non-current id (here also out of range) is fenced off before any
        // lookup — the agent is pointed back at the current task.
        let err = TaskTool
            .execute(
                &json!({ "action": "update", "id": "w9", "status": "done" }),
                &fx.ctx(),
            )
            .unwrap_err();
        assert!(err.contains("current task (w1)"), "{err}");
    }

    #[test]
    fn completing_the_last_task_reports_step_and_plan_completion() {
        let fx = implement_fixture();
        for id in ["w1", "w2"] {
            let out = TaskTool
                .execute(
                    &json!({ "action": "update", "id": id, "status": "done" }),
                    &fx.ctx(),
                )
                .unwrap();
            assert!(!out.contains("complete"), "not complete yet: {out}");
        }
        let out = TaskTool
            .execute(
                &json!({ "action": "update", "id": "v1", "status": "done" }),
                &fx.ctx(),
            )
            .unwrap();
        assert!(out.contains("All tasks in this step are complete"));
        assert!(out.contains("whole plan is now complete"));
    }

    #[test]
    fn plan_backed_list_is_unavailable() {
        // Listing the plan would re-expose the sibling tasks the fenced session
        // deliberately hides, so it is refused.
        let fx = implement_fixture();
        let err = TaskTool
            .execute(&json!({ "action": "list" }), &fx.ctx())
            .unwrap_err();
        assert!(err.contains("only 'update' is available"), "{err}");
    }
}
