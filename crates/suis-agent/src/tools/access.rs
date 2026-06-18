//! Per-session record of which files the model has *searched* and *read*.
//!
//! This backs the file-tool funnel that keeps a weak local model from burning
//! its context window: it must `search` a file before it may `read_lines` from
//! it, and `read_lines` a file before it may `edit` an existing one. The
//! [`ToolExecutor`](crate::tools::ToolExecutor) consults this log in its gate;
//! the `search` and `read_lines` tool bodies record into it as they run. The
//! log is shared (`Arc<Mutex<_>>`) so a tool body moved onto a blocking thread
//! can still record, and lives for the whole session.

use std::collections::HashSet;

use suis_core::Workspace;

/// What the model has searched and read this session, keyed by canonical
/// workspace-relative path (see [`rel_key`]).
#[derive(Debug, Default)]
pub struct AccessLog {
    /// Files surfaced by a `search` (a directory walk that matched the file, or
    /// a search scoped directly at the file). A `read_lines` is gated on this.
    searched: HashSet<String>,
    /// Files read via `read_lines`. An `edit` of an *existing* file is gated on
    /// this.
    read: HashSet<String>,
}

impl AccessLog {
    /// Mark `key` as searched (call with a [`rel_key`]).
    pub fn record_searched(&mut self, key: String) {
        self.searched.insert(key);
    }

    /// Mark `key` as read (call with a [`rel_key`]).
    pub fn record_read(&mut self, key: String) {
        self.read.insert(key);
    }

    /// Whether `key` has been surfaced by a search this session.
    pub fn was_searched(&self, key: &str) -> bool {
        self.searched.contains(key)
    }

    /// Whether `key` has been read via `read_lines` this session.
    pub fn was_read(&self, key: &str) -> bool {
        self.read.contains(key)
    }
}

/// The canonical key for `path`: its workspace-relative form with forward
/// slashes, matching what `search` reports and what the gate checks. Purely
/// lexical (no filesystem access), so it works for not-yet-created files too.
/// `None` when `path` escapes the workspace boundary.
pub fn rel_key(workspace: &Workspace, path: &str) -> Option<String> {
    let resolved = workspace.check_boundary(path).ok()?;
    let rel = resolved
        .strip_prefix(&workspace.root)
        .unwrap_or(&resolved)
        .to_string_lossy()
        .replace('\\', "/");
    Some(rel)
}
