//! Workspace detection from a starting directory.

use std::path::Path;

use super::Workspace;
use crate::errors::{Result, WorkspaceError};

impl Workspace {
    /// Detect the workspace rooted at `cwd`.
    ///
    /// The path is canonicalized (resolving symlinks) so later boundary checks
    /// compare against a stable absolute root. Errors if the path is missing or
    /// is not a directory.
    pub fn detect(cwd: impl AsRef<Path>) -> Result<Self> {
        let cwd = cwd.as_ref();
        let root = cwd.canonicalize().map_err(|_| {
            WorkspaceError::Invalid(format!("workspace path does not exist: {}", cwd.display()))
        })?;
        if !root.is_dir() {
            return Err(WorkspaceError::Invalid(format!(
                "workspace root is not a directory: {}",
                root.display()
            ))
            .into());
        }
        let suis_dir = root.join(".suis");
        let is_git = root.join(".git").exists();
        Ok(Self {
            root,
            suis_dir,
            is_git,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    #[test]
    fn detect_sets_root_and_suis_dir() {
        let dir = TempDir::new();
        let ws = Workspace::detect(dir.path()).unwrap();
        assert_eq!(ws.root, dir.path().canonicalize().unwrap());
        assert_eq!(ws.suis_dir, ws.root.join(".suis"));
        assert!(!ws.is_git);
    }

    #[test]
    fn detect_marks_git_repos() {
        let dir = TempDir::new();
        std::fs::create_dir(dir.child(".git")).unwrap();
        let ws = Workspace::detect(dir.path()).unwrap();
        assert!(ws.is_git);
    }

    #[test]
    fn detect_missing_path_errors() {
        let err = Workspace::detect("/nonexistent/suis/path/xyz").unwrap_err();
        assert!(matches!(
            err,
            crate::errors::Error::Workspace(WorkspaceError::Invalid(_))
        ));
    }
}
