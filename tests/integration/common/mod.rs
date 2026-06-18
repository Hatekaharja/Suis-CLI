//! Shared helpers for the cross-crate integration tests.
//!
//! Included by each test binary via `mod common;`. It is not itself a test
//! target (the root package declares its tests explicitly).

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique temporary directory, removed when dropped. Used as a throwaway
/// workspace root so tests never touch the real filesystem outside `temp_dir()`.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Create and return a fresh, empty temporary directory.
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("suis-it-{}-{nanos}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    /// The directory's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write `contents` to a workspace-relative path, creating parent dirs.
    pub fn write(&self, rel: &str, contents: &str) {
        let target = self.path.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(target, contents).expect("write file");
    }

    /// Read a workspace-relative path to a string.
    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path.join(rel)).expect("read file")
    }

    /// Whether a workspace-relative path exists.
    pub fn exists(&self, rel: &str) -> bool {
        self.path.join(rel).exists()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
