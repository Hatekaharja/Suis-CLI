//! The focused context an implementation session opens with.
//!
//! `/implement` clears the conversation and sends this work package as the
//! opening turn: the plan's intent, the current step's work and verify tasks
//! with their states, a shallow project snapshot, and the working rules. It
//! deliberately contains no file bodies — the agent reads on demand — so a
//! fresh session starts with its objective, not a context window full of code.

use std::path::Path;

use suis_core::filesystem::guard;
use suis_core::{PlanStore, ProjectConfig, TaskStatus, Workspace};

use crate::runtime::LedgerEntry;
use crate::tasks::plan_step_tasks;

/// Cap on entries listed per directory level in the project snapshot, so a
/// huge tree cannot swamp a small context window.
const MAX_ENTRIES_PER_LEVEL: usize = 40;

/// Assemble the work package for `plan_id`'s step `step_index` (zero-based).
/// Reads the plan store fresh, so resumed steps show their persisted progress.
pub fn assemble(
    workspace: &Workspace,
    project: &ProjectConfig,
    plan_id: &str,
    step_index: usize,
) -> Result<String, String> {
    let store = PlanStore::load(workspace).map_err(|e| e.to_string())?;
    let plan = store
        .get(plan_id)
        .ok_or_else(|| format!("no plan with id '{plan_id}'"))?;
    let step = plan
        .steps
        .get(step_index)
        .ok_or_else(|| format!("plan '{plan_id}' has no step {}", step_index + 1))?;

    let mut out = String::new();
    out.push_str("You are starting a focused implementation session for an approved plan.\n\n");
    out.push_str(&format!("Plan: {}\n", plan.title));
    if !plan.description.is_empty() {
        out.push_str(&format!("{}\n", plan.description));
    }
    out.push_str(&format!(
        "\nCurrent step ({}/{}): {}\n",
        step_index + 1,
        plan.steps.len(),
        step.title
    ));

    // The session is fenced to one task at a time: show only the current task
    // (the first not-yet-settled one, which the driver has put in `doing`), never
    // the sibling tasks — so the model can't burn its budget reasoning about
    // where this task ends and the next begins.
    let tasks = plan_step_tasks(step);
    match tasks
        .iter()
        .find(|t| matches!(t.status, TaskStatus::Todo | TaskStatus::Doing))
    {
        Some(task) => {
            out.push_str(&format!("\nYour task: {} {}\n", task.id, task.title));
            out.push_str(
                "\nThis is one task within a larger step. Other tasks handle related \
                 concerns and run separately, each on its own — do ONLY this task. Do not \
                 anticipate, set up for, or do work that belongs to a later task.\n",
            );
        }
        None => out.push_str("\nEvery task in this step is settled.\n"),
    }

    let tree = snapshot(workspace, project);
    if !tree.is_empty() {
        out.push_str("\nProject structure (top levels — read files on demand):\n");
        out.push_str(&tree);
        out.push('\n');
    }

    out.push_str(
        "\nRules for this session:\n\
         - Your current task is already marked 'doing' for you. Make the change by editing \
           files directly — do not write the whole solution out in your reasoning; edit \
           incrementally and let the file be the record.\n\
         - When the task is complete, mark it 'done' with the task tool (or 'blocked' if \
           you cannot). That is your only task-tool action — task status is otherwise \
           managed for you.\n\
         - If you cannot complete it as described — the file or line it names does not \
           exist, its premise is wrong, or there is nothing to change — mark it 'blocked' \
           and say why. Never fake a 'done'; a false 'done' is compacted into the record \
           as if the work happened.\n\
         - Do only this task. Do not start, set up for, or modify any other task.",
    );
    Ok(out)
}

/// Render an implementation session's handoff ledger into one block for the
/// seed context: a progress list the agent reads instead of the prior tasks'
/// full transcripts. Returns `None` when the ledger is empty (nothing to seed).
pub fn render_ledger(ledger: &[LedgerEntry]) -> Option<String> {
    if ledger.is_empty() {
        return None;
    }
    let mut out = String::from(
        "Progress so far in this step (settled tasks — their working detail has \
         been compacted away; re-read files if you need specifics). Tasks marked \
         BLOCKED were not completed; do not assume their work was done:\n",
    );
    for entry in ledger {
        let mark = if entry.blocked { " [BLOCKED]" } else { "" };
        out.push_str(&format!("- {} {}{mark}", entry.id, entry.title));
        if !entry.summary.is_empty() {
            out.push_str(&format!(" — {}", entry.summary));
        }
        if !entry.touched.is_empty() {
            out.push_str(&format!("\n  Touched: {}", entry.touched.join(", ")));
        }
        out.push('\n');
    }
    Some(out)
}

/// A one-level (top-level only) listing of the workspace root — the cheap
/// project oversight injected into every system prompt, so a session opens
/// already knowing the project's shape without spending a `tree` call.
/// Names only — never file contents.
pub(crate) fn top_level_snapshot(workspace: &Workspace, project: &ProjectConfig) -> String {
    list_dir(&workspace.root, project, None)
        .into_iter()
        .map(|(name, is_dir)| if is_dir { format!("{name}/") } else { name })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A two-level listing of the workspace, respecting the project's hidden
/// patterns and skipping `.git`/`.suis`. Names only — never file contents.
pub(crate) fn snapshot(workspace: &Workspace, project: &ProjectConfig) -> String {
    let mut lines = Vec::new();
    for (name, is_dir) in list_dir(&workspace.root, project, None) {
        if is_dir {
            lines.push(format!("{name}/"));
            for (child, child_is_dir) in list_dir(&workspace.root.join(&name), project, Some(&name))
            {
                let suffix = if child_is_dir { "/" } else { "" };
                lines.push(format!("  {child}{suffix}"));
            }
        } else {
            lines.push(name);
        }
    }
    lines.join("\n")
}

/// Sorted, filtered entries of one directory as `(name, is_dir)`, capped at
/// [`MAX_ENTRIES_PER_LEVEL`] with a trailing marker. `parent` is the
/// workspace-relative prefix used for hidden-pattern matching.
fn list_dir(dir: &Path, project: &ProjectConfig, parent: Option<&str>) -> Vec<(String, bool)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, bool)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".git" || name == ".suis" {
                return None;
            }
            let rel = match parent {
                Some(parent) => format!("{parent}/{name}"),
                None => name.clone(),
            };
            if guard::is_hidden(project, Path::new(&rel)) {
                return None;
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            Some((name, is_dir))
        })
        .collect();
    out.sort();
    if out.len() > MAX_ENTRIES_PER_LEVEL {
        let more = out.len() - MAX_ENTRIES_PER_LEVEL;
        out.truncate(MAX_ENTRIES_PER_LEVEL);
        out.push((format!("… ({more} more)"), false));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::Fixture;
    use suis_core::{PlanStep, PlanTask};

    fn store_plan(fx: &Fixture) {
        let mut store = PlanStore::default();
        let mut step = PlanStep {
            title: "Token issuing".into(),
            work_tasks: vec![
                PlanTask::new("add login route"),
                PlanTask::new("sign tokens"),
            ],
            verify_tasks: vec![PlanTask::new("run auth tests")],
        };
        step.work_tasks[0].status = TaskStatus::Done;
        store.insert("Auth System", "Add JWT auth", vec![step]);
        store.save(&fx.workspace).unwrap();
    }

    #[test]
    fn package_shows_only_the_current_task_with_snapshot_but_no_file_bodies() {
        let mut fx = Fixture::new();
        fx.project.hidden.push(".env".into());
        fx.write("src/main.rs", "SECRET_FILE_BODY");
        fx.write(".env", "TOKEN=shh");
        store_plan(&fx);

        let package = assemble(&fx.workspace, &fx.project, "auth-system", 0).unwrap();
        // Plan and step orient the agent.
        assert!(package.contains("Auth System"));
        assert!(package.contains("Token issuing"));
        // Only the current task (w1 is done, so w2 is current) is shown — the
        // sibling tasks are deliberately hidden so the agent can't agonize over
        // where this task ends and the next begins.
        assert!(package.contains("Your task: w2 sign tokens"), "{package}");
        assert!(!package.contains("add login route"), "w1 leaked: {package}");
        assert!(!package.contains("run auth tests"), "v1 leaked: {package}");
        // The scope fence is spelled out.
        assert!(package.contains("do ONLY this task"));
        // The snapshot lists structure, not contents — and respects hidden.
        assert!(package.contains("src/"));
        assert!(package.contains("main.rs"));
        assert!(!package.contains("SECRET_FILE_BODY"));
        assert!(!package.contains(".env"));
    }

    #[test]
    fn render_ledger_marks_blocked_tasks_so_they_are_not_read_as_done() {
        let ledger = vec![
            LedgerEntry {
                id: "w1".into(),
                title: "add login route".into(),
                summary: "added the route".into(),
                touched: vec!["src/auth.rs".into()],
                blocked: false,
            },
            LedgerEntry {
                id: "w2".into(),
                title: "remove waitForTimeout".into(),
                summary: "file does not exist".into(),
                touched: Vec::new(),
                blocked: true,
            },
        ];
        let out = render_ledger(&ledger).unwrap();
        // The done task is plain; the blocked one is flagged so the next task's
        // fresh context cannot mistake it for completed work.
        assert!(out.contains("w1 add login route"));
        assert!(!out.contains("w1 add login route [BLOCKED]"));
        assert!(out.contains("w2 remove waitForTimeout [BLOCKED]"));
        assert!(out.contains("not completed"));
    }

    #[test]
    fn assemble_rules_offer_blocked_and_push_for_edits_over_reasoning() {
        let fx = Fixture::new();
        store_plan(&fx);
        let package = assemble(&fx.workspace, &fx.project, "auth-system", 0).unwrap();
        assert!(package.contains("'blocked'"));
        assert!(package.contains("Never fake a 'done'"));
        // The action-oriented rule that counters the reasoning-runaway.
        assert!(package.contains("do not write the whole solution out in your reasoning"));
    }

    #[test]
    fn unknown_plan_or_step_errors() {
        let fx = Fixture::new();
        store_plan(&fx);
        let err = assemble(&fx.workspace, &fx.project, "nope", 0).unwrap_err();
        assert!(err.contains("no plan with id 'nope'"));
        let err = assemble(&fx.workspace, &fx.project, "auth-system", 5).unwrap_err();
        assert!(err.contains("has no step 6"));
    }
}
