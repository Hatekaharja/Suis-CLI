//! Test-only helpers: a dependency-free temp dir and a fixture builder for the
//! pieces a [`ToolContext`](crate::tools::ToolContext) needs.
#![cfg(test)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use suis_core::{ProjectConfig, Workspace};

use crate::tasks::TaskStore;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique temporary directory, removed on drop.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("suis-agent-{}-{}-{}", std::process::id(), n, nanos));
        std::fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A workspace-rooted fixture: a temp dir, the workspace, a project config, and
/// a shared task store. Keep the [`TempDir`] alive for the test's duration.
pub struct Fixture {
    /// Held to keep the temp dir alive for the fixture's lifetime.
    #[allow(dead_code)]
    pub dir: TempDir,
    pub workspace: Workspace,
    pub project: ProjectConfig,
    pub tasks: Arc<Mutex<TaskStore>>,
    /// The session file-access log [`Fixture::ctx`] borrows into the context.
    pub access: Arc<Mutex<crate::tools::AccessLog>>,
    /// Set to make [`Fixture::ctx`] simulate an implementation session.
    pub implement: Option<crate::runtime::session::ImplementTarget>,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = TempDir::new();
        let workspace = Workspace::detect(dir.path()).unwrap();
        Fixture {
            dir,
            workspace,
            project: ProjectConfig::default(),
            tasks: Arc::new(Mutex::new(TaskStore::new())),
            access: Arc::new(Mutex::new(crate::tools::AccessLog::default())),
            implement: None,
        }
    }

    /// Write a file relative to the workspace root.
    pub fn write(&self, rel: &str, content: &str) {
        let path = self.workspace.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    /// Read a file relative to the workspace root.
    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.workspace.root.join(rel)).unwrap()
    }

    /// Build a [`ToolContext`](crate::tools::ToolContext) borrowing this fixture.
    pub fn ctx(&self) -> crate::tools::ToolContext<'_> {
        crate::tools::ToolContext {
            workspace: &self.workspace,
            project: &self.project,
            tasks: &self.tasks,
            implement: self.implement.as_ref(),
            access: &self.access,
        }
    }
}
