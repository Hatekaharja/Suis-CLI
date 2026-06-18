//! The `plan` tool: the model drafts a persistent plan, the user approves.
//!
//! Exposed only in Plan mode (see [`Mode::allows_tool`](crate::Mode)). The tool
//! itself never writes: [`parse_draft`] validates the model's arguments into a
//! [`PlanDraft`], and the [`ToolExecutor`](super::ToolExecutor) routes the
//! draft to the UI as an
//! [`AgentEvent::PlanProposal`](crate::runtime::events::AgentEvent) and only
//! persists it to `.suis/plans.json` after the user approves — the same
//! oneshot event/response pattern as permission prompts.

use serde_json::{json, Value};

use suis_core::{PlanStep, PlanTask};

use super::{opt_str, require_str, Tool, ToolContext, ToolDefinition, ToolOutcome};

/// A validated plan draft awaiting the user's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDraft {
    /// The id of the existing plan this draft replaces (`revise`), or `None`
    /// for a new plan (`propose`).
    pub revises: Option<String>,
    /// Plan title (the id slug is derived from it on save).
    pub title: String,
    /// What the plan is for.
    pub description: String,
    /// The proposed steps; every task starts `Todo`.
    pub steps: Vec<PlanStep>,
}

/// Drafts plans on behalf of the agent; writes happen in the executor, after
/// user approval.
pub struct PlanTool;

impl Tool for PlanTool {
    fn name(&self) -> &'static str {
        "plan"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Propose a persistent project plan for the user to approve. \
                          action=propose drafts a new plan; action=revise (with the plan 'id') \
                          replaces an existing plan's structure. Provide a title, a short \
                          description, and steps — each step has a title, work_tasks (what to \
                          implement), and verify_tasks (how to prove it works). Nothing is \
                          saved unless the user approves the draft."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["propose", "revise"],
                        "description": "propose a new plan, or revise an existing one."
                    },
                    "id": {
                        "type": "string",
                        "description": "The id of the plan to revise (revise only)."
                    },
                    "title": { "type": "string", "description": "Plan title." },
                    "description": {
                        "type": "string",
                        "description": "One or two sentences on what the plan achieves."
                    },
                    "steps": {
                        "type": "array",
                        "description": "The ordered steps of the plan.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string", "description": "Step title." },
                                "work_tasks": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Implementation task titles, in order."
                                },
                                "verify_tasks": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Verification task titles (tests, checks)."
                                }
                            },
                            "required": ["title", "work_tasks"]
                        }
                    }
                },
                "required": ["action", "title", "steps"]
            }),
        }
    }

    fn execute(&self, _args: &Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        // The executor resolves `plan` calls itself (proposal → approval →
        // persist); this body is unreachable through normal execution.
        Err("the plan tool is handled by the executor".to_string())
    }
}

/// Validate the model's arguments into a [`PlanDraft`], or a message the model
/// can correct from. Pure: no I/O, no store access.
pub(crate) fn parse_draft(args: &Value) -> Result<PlanDraft, String> {
    let action = require_str(args, "action")?;
    let revises = match action.as_str() {
        "propose" => None,
        "revise" => Some(require_str(args, "id")?),
        other => return Err(format!("unknown plan action '{other}'")),
    };

    let title = require_str(args, "title")?.trim().to_string();
    if title.is_empty() {
        return Err("plan title must not be empty".to_string());
    }
    let description = opt_str(args, "description").unwrap_or_default();

    let steps_val = args
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing required array argument 'steps'".to_string())?;
    if steps_val.is_empty() {
        return Err("a plan needs at least one step".to_string());
    }

    let mut steps = Vec::with_capacity(steps_val.len());
    for (i, step) in steps_val.iter().enumerate() {
        let n = i + 1;
        let step_title = step
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| format!("step {n} needs a non-empty 'title'"))?;
        let work_tasks = task_titles(step, "work_tasks", n)?;
        if work_tasks.is_empty() {
            return Err(format!("step {n} needs at least one work task"));
        }
        let verify_tasks = task_titles(step, "verify_tasks", n)?;
        steps.push(PlanStep {
            title: step_title.to_string(),
            work_tasks,
            verify_tasks,
        });
    }

    Ok(PlanDraft {
        revises,
        title,
        description,
        steps,
    })
}

/// Read a step's task-title array (`work_tasks` / `verify_tasks`) into fresh
/// `Todo` tasks. A missing key is an empty list; a non-string entry is an error.
fn task_titles(step: &Value, key: &str, step_no: usize) -> Result<Vec<PlanTask>, String> {
    let Some(value) = step.get(key) else {
        return Ok(Vec::new());
    };
    let items = value
        .as_array()
        .ok_or_else(|| format!("step {step_no}: '{key}' must be an array of strings"))?;
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(PlanTask::new)
                .ok_or_else(|| format!("step {step_no}: '{key}' must contain non-empty strings"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use suis_core::TaskStatus;

    #[test]
    fn valid_propose_parses_with_todo_tasks() {
        let draft = parse_draft(&json!({
            "action": "propose",
            "title": "Auth System",
            "description": "Add JWT auth",
            "steps": [
                { "title": "Tokens", "work_tasks": ["login route"], "verify_tasks": ["auth tests"] },
                { "title": "Middleware", "work_tasks": ["verify tokens"] }
            ]
        }))
        .unwrap();
        assert_eq!(draft.revises, None);
        assert_eq!(draft.title, "Auth System");
        assert_eq!(draft.steps.len(), 2);
        assert_eq!(draft.steps[0].verify_tasks[0].status, TaskStatus::Todo);
        assert!(draft.steps[1].verify_tasks.is_empty());
    }

    #[test]
    fn revise_requires_an_id() {
        let err = parse_draft(&json!({
            "action": "revise",
            "title": "T",
            "steps": [{ "title": "s", "work_tasks": ["a"] }]
        }))
        .unwrap_err();
        assert!(err.contains("'id'"), "unexpected error: {err}");
    }

    #[test]
    fn empty_steps_and_empty_work_are_rejected() {
        let err =
            parse_draft(&json!({ "action": "propose", "title": "T", "steps": [] })).unwrap_err();
        assert!(err.contains("at least one step"));

        let err = parse_draft(&json!({
            "action": "propose",
            "title": "T",
            "steps": [{ "title": "s", "work_tasks": [] }]
        }))
        .unwrap_err();
        assert!(err.contains("at least one work task"));
    }

    #[test]
    fn malformed_step_is_rejected() {
        let err = parse_draft(&json!({
            "action": "propose",
            "title": "T",
            "steps": [{ "title": "", "work_tasks": ["a"] }]
        }))
        .unwrap_err();
        assert!(err.contains("step 1"));

        let err = parse_draft(&json!({
            "action": "propose",
            "title": "T",
            "steps": [{ "title": "s", "work_tasks": [42] }]
        }))
        .unwrap_err();
        assert!(err.contains("non-empty strings"));
    }
}
