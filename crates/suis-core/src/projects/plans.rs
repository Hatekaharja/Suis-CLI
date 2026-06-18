//! Persistent plans (`.suis/plans.json`) — the workflow model's project
//! artifact.
//!
//! A [`Plan`] is drafted by the agent in Plan mode, approved by the user before
//! it ever touches disk, and executed step-by-step through `/implement`. Each
//! [`PlanStep`] splits into work tasks and verify tasks; verification starts
//! only when the user says so. Completion is *derived*, never stored: a step is
//! complete when every work and verify task is `Done`, a plan when every step
//! is.
//!
//! [`PlanStore`] mirrors the [`PermissionStore`](crate::PermissionStore)
//! patterns: serde types, atomic writes, defensive load (missing file → empty,
//! unparseable → a clear error naming the file).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::errors::{ConfigError, Result};
use crate::util::write_atomic;
use crate::workspace::Workspace;

/// The lifecycle state of a task — shared between session tasks (suis-agent)
/// and persistent plan tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    /// Not started.
    #[default]
    Todo,
    /// Actively being worked on. At most one task is meaningfully `Doing`.
    Doing,
    /// Finished.
    Done,
    /// Cannot proceed.
    Blocked,
}

impl TaskStatus {
    /// Parse a status from its lowercase wire string. Accepts a couple of
    /// friendly synonyms the model is likely to emit.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "todo" | "pending" => Some(TaskStatus::Todo),
            "doing" | "in_progress" | "active" => Some(TaskStatus::Doing),
            "done" | "completed" => Some(TaskStatus::Done),
            "blocked" => Some(TaskStatus::Blocked),
            _ => None,
        }
    }

    /// A single-character glyph for terminal display.
    pub fn icon(self) -> &'static str {
        match self {
            TaskStatus::Todo => "□",
            TaskStatus::Doing => "▶",
            TaskStatus::Done => "✓",
            TaskStatus::Blocked => "!",
        }
    }
}

/// One unit of work inside a plan step. Identified positionally within its
/// step (`w1`/`v1`-style ids are derived, not stored).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanTask {
    /// Human-readable title.
    pub title: String,
    /// Current lifecycle state.
    #[serde(default)]
    pub status: TaskStatus,
}

impl PlanTask {
    /// A fresh `Todo` task.
    pub fn new(title: impl Into<String>) -> Self {
        PlanTask {
            title: title.into(),
            status: TaskStatus::default(),
        }
    }
}

/// One step of a plan: the work to do and the verification that proves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    /// Human-readable step title.
    pub title: String,
    /// The implementation tasks.
    #[serde(default)]
    pub work_tasks: Vec<PlanTask>,
    /// The verification tasks, started only after the user confirms.
    #[serde(default)]
    pub verify_tasks: Vec<PlanTask>,
}

impl PlanStep {
    /// Derived: a step is complete when every work *and* verify task is `Done`.
    pub fn is_complete(&self) -> bool {
        self.work_tasks
            .iter()
            .chain(&self.verify_tasks)
            .all(|t| t.status == TaskStatus::Done)
    }

    /// Resolve a derived task id (`w2` → second work task, `v1` → first verify
    /// task) to a mutable reference within this step. `None` for a malformed id
    /// or an out-of-range index.
    pub fn task_by_id_mut(&mut self, id: &str) -> Option<&mut PlanTask> {
        if !id.is_ascii() || id.len() < 2 {
            return None;
        }
        let (prefix, number) = id.split_at(1);
        let index = number.parse::<usize>().ok()?.checked_sub(1)?;
        match prefix {
            "w" => self.work_tasks.get_mut(index),
            "v" => self.verify_tasks.get_mut(index),
            _ => None,
        }
    }
}

/// A plan's stored lifecycle status. Completion is derived from its steps, not
/// recorded here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanStatus {
    /// In play: listed by `/plans`, selectable by `/implement`.
    #[default]
    Active,
    /// Kept for the record but out of the way.
    Archived,
}

/// A persistent plan: an id (slug of the title), the structure the user
/// approved, and the live task states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// Slug identifier derived from the title (e.g. `authentication-system`),
    /// unique within the store.
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// What the plan is for, in a sentence or two.
    #[serde(default)]
    pub description: String,
    /// Stored lifecycle status (not completion — that is derived).
    #[serde(default)]
    pub status: PlanStatus,
    /// The ordered steps.
    #[serde(default)]
    pub steps: Vec<PlanStep>,
}

impl Plan {
    /// Derived: a plan is complete when it has steps and every one is complete.
    pub fn is_complete(&self) -> bool {
        !self.steps.is_empty() && self.steps.iter().all(PlanStep::is_complete)
    }

    /// `(complete, total)` step counts, for progress display (`[3/5 steps]`).
    pub fn progress(&self) -> (usize, usize) {
        let done = self.steps.iter().filter(|s| s.is_complete()).count();
        (done, self.steps.len())
    }

    /// The index of the first step that is not yet complete, if any — what
    /// "implement the whole plan" resolves to.
    pub fn next_step(&self) -> Option<usize> {
        self.steps.iter().position(|s| !s.is_complete())
    }
}

/// The workspace's plans, persisted at `.suis/plans.json`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PlanStore {
    /// All stored plans, in creation order.
    #[serde(default)]
    pub plans: Vec<Plan>,
}

impl PlanStore {
    fn path(workspace: &Workspace) -> PathBuf {
        workspace.suis_dir.join("plans.json")
    }

    /// Load the workspace's plans. A missing file is an empty store; an
    /// unparseable one is an error naming the file.
    pub fn load(workspace: &Workspace) -> Result<Self> {
        let path = Self::path(workspace);
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path).map_err(|source| ConfigError::ReadFailure {
            path: path.clone(),
            source,
        })?;
        let store = serde_json::from_str(&raw).map_err(|source| ConfigError::ParseFailure {
            path: path.clone(),
            source,
        })?;
        Ok(store)
    }

    /// Persist the store atomically to `.suis/plans.json`.
    pub fn save(&self, workspace: &Workspace) -> Result<()> {
        let path = Self::path(workspace);
        let json = serde_json::to_vec_pretty(self)
            .map_err(|source| ConfigError::SerializeFailure { source })?;
        write_atomic(&path, &json).map_err(|source| ConfigError::WriteFailure { path, source })?;
        Ok(())
    }

    /// Look up a plan by id.
    pub fn get(&self, id: &str) -> Option<&Plan> {
        self.plans.iter().find(|p| p.id == id)
    }

    /// Look up a plan by id, mutably.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Plan> {
        self.plans.iter_mut().find(|p| p.id == id)
    }

    /// Add a new plan, deriving its id from `title` (deduplicated with a
    /// numeric suffix). Returns the assigned id. All tasks start as written —
    /// callers building fresh drafts should pass `Todo` tasks.
    pub fn insert(
        &mut self,
        title: impl Into<String>,
        description: impl Into<String>,
        steps: Vec<PlanStep>,
    ) -> String {
        let title = title.into();
        let base = slugify(&title);
        let mut id = base.clone();
        let mut n = 1;
        while self.get(&id).is_some() {
            n += 1;
            id = format!("{base}-{n}");
        }
        self.plans.push(Plan {
            id: id.clone(),
            title,
            description: description.into(),
            status: PlanStatus::default(),
            steps,
        });
        id
    }
}

/// Derive a slug id from a plan title: lowercase, alphanumeric runs joined by
/// single dashes (`"Authentication System!"` → `"authentication-system"`).
fn slugify(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    let mut pending_dash = false;
    for c in title.chars() {
        if c.is_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.extend(c.to_lowercase());
        } else {
            pending_dash = true;
        }
    }
    if slug.is_empty() {
        "plan".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    fn ws(dir: &TempDir) -> Workspace {
        Workspace::detect(dir.path()).unwrap()
    }

    fn step(title: &str, work: &[&str], verify: &[&str]) -> PlanStep {
        PlanStep {
            title: title.into(),
            work_tasks: work.iter().map(|t| PlanTask::new(*t)).collect(),
            verify_tasks: verify.iter().map(|t| PlanTask::new(*t)).collect(),
        }
    }

    #[test]
    fn parses_status_synonyms() {
        assert_eq!(TaskStatus::parse("todo"), Some(TaskStatus::Todo));
        assert_eq!(TaskStatus::parse("in_progress"), Some(TaskStatus::Doing));
        assert_eq!(TaskStatus::parse("DONE"), Some(TaskStatus::Done));
        assert_eq!(TaskStatus::parse("blocked"), Some(TaskStatus::Blocked));
        assert_eq!(TaskStatus::parse("nonsense"), None);
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = TempDir::new();
        let store = PlanStore::load(&ws(&dir)).unwrap();
        assert!(store.plans.is_empty());
    }

    #[test]
    fn unparseable_file_errors_naming_the_file() {
        let dir = TempDir::new();
        let workspace = ws(&dir);
        std::fs::create_dir_all(&workspace.suis_dir).unwrap();
        std::fs::write(workspace.suis_dir.join("plans.json"), "{not json").unwrap();
        let err = PlanStore::load(&workspace).unwrap_err().to_string();
        assert!(
            err.contains("plans.json"),
            "error should name the file: {err}"
        );
    }

    #[test]
    fn round_trip_multi_step_plan() {
        let dir = TempDir::new();
        let workspace = ws(&dir);
        let mut store = PlanStore::default();
        let id = store.insert(
            "Authentication System",
            "Add JWT auth",
            vec![
                step(
                    "Token issuing",
                    &["add login route", "sign tokens"],
                    &["run auth tests"],
                ),
                step("Middleware", &["verify tokens"], &["manual smoke test"]),
            ],
        );
        store.save(&workspace).unwrap();

        let loaded = PlanStore::load(&workspace).unwrap();
        assert_eq!(loaded, store);
        let plan = loaded.get(&id).unwrap();
        assert_eq!(plan.title, "Authentication System");
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].work_tasks.len(), 2);
        assert_eq!(plan.steps[0].verify_tasks[0].status, TaskStatus::Todo);
    }

    #[test]
    fn slug_derived_and_collision_gets_suffix() {
        let mut store = PlanStore::default();
        let a = store.insert("Authentication System!", "", vec![]);
        let b = store.insert("authentication  system", "", vec![]);
        let c = store.insert("Authentication System", "", vec![]);
        assert_eq!(a, "authentication-system");
        assert_eq!(b, "authentication-system-2");
        assert_eq!(c, "authentication-system-3");
    }

    #[test]
    fn empty_title_slug_falls_back() {
        assert_eq!(slugify("!!!"), "plan");
    }

    #[test]
    fn completion_is_derived_from_task_states() {
        let mut s = step("s", &["a", "b"], &["check"]);
        assert!(!s.is_complete());
        s.work_tasks[0].status = TaskStatus::Done;
        s.work_tasks[1].status = TaskStatus::Done;
        // One Todo verify task keeps the step incomplete.
        assert!(!s.is_complete());
        s.verify_tasks[0].status = TaskStatus::Done;
        assert!(s.is_complete());
    }

    #[test]
    fn plan_progress_and_next_step() {
        let mut plan = Plan {
            id: "p".into(),
            title: "P".into(),
            description: String::new(),
            status: PlanStatus::Active,
            steps: vec![step("one", &["a"], &[]), step("two", &["b"], &[])],
        };
        assert_eq!(plan.progress(), (0, 2));
        assert_eq!(plan.next_step(), Some(0));
        assert!(!plan.is_complete());

        plan.steps[0].work_tasks[0].status = TaskStatus::Done;
        assert_eq!(plan.progress(), (1, 2));
        assert_eq!(plan.next_step(), Some(1));

        plan.steps[1].work_tasks[0].status = TaskStatus::Done;
        assert_eq!(plan.progress(), (2, 2));
        assert_eq!(plan.next_step(), None);
        assert!(plan.is_complete());
    }

    #[test]
    fn empty_plan_is_never_complete() {
        let plan = Plan {
            id: "p".into(),
            title: "P".into(),
            description: String::new(),
            status: PlanStatus::Active,
            steps: vec![],
        };
        assert!(!plan.is_complete());
    }
}
