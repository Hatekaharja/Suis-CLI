//! Per-session state the agent loop reads and mutates.

use std::sync::{Arc, Mutex};

use suis_core::{PermissionStore, PlanStore, ProjectConfig, Workspace};
use suis_providers::Model;

use crate::context::budget;
use crate::conversation::ConversationHistory;
use crate::runtime::mode::Mode;
use crate::tasks::{plan_step_tasks, TaskStore};

/// The plan step an implementation session (`/implement`) is working on. While
/// set, the `task` tool operates on this step's tasks (state updates only,
/// persisted to `.suis/plans.json` on every change) instead of the in-memory
/// session task list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplementTarget {
    /// The plan's id in the [`PlanStore`].
    pub plan_id: String,
    /// Zero-based index into the plan's steps.
    pub step_index: usize,
}

/// One completed task's entry in an implementation session's handoff ledger.
///
/// The ledger is what survives a per-task context reset (see the per-task
/// driver in [`crate::Agent`]): instead of carrying a task's full working
/// transcript into the next task, the session keeps this dense record — the
/// deterministic facts (id, title, files touched) plus a short model-written
/// summary — and re-seeds the next task from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    /// The derived task id (`w1`, `v2`, …).
    pub id: String,
    /// The task's title.
    pub title: String,
    /// A short model-written summary of what was done. Empty when the summary
    /// call failed — the deterministic fields below still stand on their own.
    pub summary: String,
    /// Workspace-relative paths edited and shell commands run during the task,
    /// extracted deterministically from the task's tool calls.
    pub touched: Vec<String>,
    /// Whether the task ended `blocked` rather than `done` — the model could not
    /// complete it as stated (missing file, wrong premise, nothing to change).
    /// Recorded honestly so the next task's seed context never mistakes a
    /// blocked task for a finished one.
    pub blocked: bool,
}

/// Everything one chat session carries: where it runs, which model it talks to,
/// the transcript so far, its task list, and its accumulated permissions.
pub struct Session {
    /// The project workspace.
    pub workspace: Workspace,
    /// The project configuration (tools, hidden/hardened, git access).
    pub project: ProjectConfig,
    /// The selected model and its capabilities.
    pub model: Model,
    /// The conversation transcript.
    pub history: ConversationHistory,
    /// Session task list, shared with the `task` tool.
    pub tasks: Arc<Mutex<TaskStore>>,
    /// What the model has searched and read this session, enforcing
    /// search-before-read and read-before-edit. Shared with the file tools that
    /// record into it. Reset whenever the conversation is.
    pub access: Arc<Mutex<crate::tools::AccessLog>>,
    /// Stored + session-granted permissions.
    pub permissions: PermissionStore,
    /// The runtime mode gating which tools are advertised and executable.
    /// Session state only — every session starts in [`Mode::Agent`].
    pub mode: Mode,
    /// The active implementation target, set by `/implement` and cleared with
    /// the conversation.
    pub implement: Option<ImplementTarget>,
    /// The implementation session's handoff ledger: one [`LedgerEntry`] per
    /// completed task, carried across the per-task context resets so a fresh
    /// task is re-seeded from a dense record instead of the full transcript.
    /// Cleared whenever the conversation is (`/clear`, a new `/implement`).
    pub ledger: Vec<LedgerEntry>,
    /// Estimated-token budget the history is pruned against before each
    /// request. Defaults to [`budget::DEFAULT_CONTEXT_BUDGET`]; the CLI
    /// overrides it from `settings.json`'s `context_budget`.
    pub context_budget: usize,
}

impl Session {
    /// Start a session, loading the persisted permissions that apply to the
    /// workspace (its own grants merged with the user's global `Always` grants).
    pub fn new(workspace: Workspace, project: ProjectConfig, model: Model) -> Self {
        let permissions = PermissionStore::load(&workspace).unwrap_or_default();
        Session {
            workspace,
            project,
            model,
            history: ConversationHistory::new(),
            tasks: Arc::new(Mutex::new(TaskStore::new())),
            access: Arc::new(Mutex::new(crate::tools::AccessLog::default())),
            permissions,
            mode: Mode::default(),
            implement: None,
            ledger: Vec::new(),
            context_budget: budget::DEFAULT_CONTEXT_BUDGET,
        }
    }

    /// A snapshot of the current task list, for UI updates. During an
    /// implementation session this is the active plan step's tasks (read fresh
    /// from `.suis/plans.json`, the source of truth the `task` tool persists
    /// to); otherwise the in-memory session tasks.
    pub fn task_snapshot(&self) -> Vec<crate::tasks::Task> {
        if let Some(target) = &self.implement {
            return PlanStore::load(&self.workspace)
                .ok()
                .and_then(|store| {
                    store
                        .get(&target.plan_id)
                        .and_then(|plan| plan.steps.get(target.step_index))
                        .map(plan_step_tasks)
                })
                .unwrap_or_default();
        }
        self.tasks
            .lock()
            .map(|t| t.all().to_vec())
            .unwrap_or_default()
    }
}
