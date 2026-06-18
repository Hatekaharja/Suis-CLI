//! Workspace-scoped filesystem operations with boundary, hidden, and hardened
//! enforcement.
//!
//! Every operation validates the target against the workspace boundary first.
//! Reads/listings honor the hidden list; writes honor the hardened list.
//! Deletes route through the soft-delete [`trash`](super::trash) system.

use std::path::{Path, PathBuf};

use super::{guard, trash};
use crate::errors::{Error, FilesystemError, Result};
use crate::projects::ProjectConfig;
use crate::util::write_atomic;
use crate::workspace::Workspace;

/// Compute the workspace-relative path for guard checks.
fn relative(workspace: &Workspace, resolved: &Path) -> PathBuf {
    resolved
        .strip_prefix(&workspace.root)
        .unwrap_or(resolved)
        .to_path_buf()
}

/// Read a file's contents. Errors with [`Error::PermissionDenied`] for hidden
/// files (without leaking any content) or for paths outside the workspace.
pub fn read(
    workspace: &Workspace,
    project: &ProjectConfig,
    path: impl AsRef<Path>,
) -> Result<String> {
    let resolved = workspace.check_boundary(path.as_ref())?;
    let rel = relative(workspace, &resolved);
    if guard::is_hidden(project, &rel) {
        return Err(Error::PermissionDenied(format!(
            "file is hidden: {}",
            rel.display()
        )));
    }
    std::fs::read_to_string(&resolved).map_err(|e| FilesystemError::Io(e).into())
}

/// Overwrite an existing file (or create it). Errors for hidden files (never
/// writable) and hardened files (which require explicit approval handled at a
/// higher layer). The target is validated against symlink redirection.
pub fn write(
    workspace: &Workspace,
    project: &ProjectConfig,
    path: impl AsRef<Path>,
    content: &str,
) -> Result<()> {
    let resolved = workspace.check_write_target(path.as_ref())?;
    let rel = relative(workspace, &resolved);
    if guard::is_hidden(project, &rel) {
        return Err(Error::PermissionDenied(format!(
            "file is hidden: {}",
            rel.display()
        )));
    }
    if guard::is_hardened(project, &rel) {
        return Err(Error::PermissionDenied(format!(
            "file is hardened: {}",
            rel.display()
        )));
    }
    write_atomic(&resolved, content.as_bytes()).map_err(FilesystemError::Io)?;
    Ok(())
}

/// Create a new file, erroring if it already exists.
pub fn create(
    workspace: &Workspace,
    project: &ProjectConfig,
    path: impl AsRef<Path>,
    content: &str,
) -> Result<()> {
    let resolved = workspace.check_write_target(path.as_ref())?;
    if resolved.exists() {
        return Err(FilesystemError::AlreadyExists(resolved).into());
    }
    let rel = relative(workspace, &resolved);
    if guard::is_hidden(project, &rel) {
        return Err(Error::PermissionDenied(format!(
            "file is hidden: {}",
            rel.display()
        )));
    }
    if guard::is_hardened(project, &rel) {
        return Err(Error::PermissionDenied(format!(
            "file is hardened: {}",
            rel.display()
        )));
    }
    write_atomic(&resolved, content.as_bytes()).map_err(FilesystemError::Io)?;
    Ok(())
}

/// Soft-delete a file by moving it into the workspace trash.
pub fn delete(workspace: &Workspace, path: impl AsRef<Path>) -> Result<trash::TrashEntry> {
    let resolved = workspace.check_boundary(path.as_ref())?;
    if !resolved.exists() {
        return Err(FilesystemError::NotFound(resolved).into());
    }
    trash::trash(workspace, &resolved)
}

/// List a directory's entries, omitting hidden files. Returns absolute paths,
/// sorted for determinism.
pub fn list_dir(
    workspace: &Workspace,
    project: &ProjectConfig,
    path: impl AsRef<Path>,
) -> Result<Vec<PathBuf>> {
    let resolved = workspace.check_boundary(path.as_ref())?;
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&resolved).map_err(FilesystemError::Io)? {
        let entry = entry.map_err(FilesystemError::Io)?;
        let entry_path = entry.path();
        let rel = relative(workspace, &entry_path);
        if guard::is_hidden(project, &rel) {
            continue;
        }
        entries.push(entry_path);
    }
    entries.sort();
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    fn setup() -> (TempDir, Workspace, ProjectConfig) {
        let dir = TempDir::new();
        let workspace = Workspace::detect(dir.path()).unwrap();
        (dir, workspace, ProjectConfig::default())
    }

    #[test]
    fn read_normal_file() {
        let (_d, ws, cfg) = setup();
        std::fs::write(ws.root.join("a.txt"), "hello").unwrap();
        assert_eq!(read(&ws, &cfg, "a.txt").unwrap(), "hello");
    }

    #[test]
    fn read_hidden_file_denied_no_leak() {
        let (_d, ws, mut cfg) = setup();
        cfg.hidden.push(".env".into());
        std::fs::write(ws.root.join(".env"), "SECRET=1").unwrap();
        let err = read(&ws, &cfg, ".env").unwrap_err();
        assert!(matches!(err, Error::PermissionDenied(_)));
        // The error message must not contain the file contents.
        assert!(!err.to_string().contains("SECRET"));
    }

    #[test]
    fn read_outside_workspace_denied() {
        let (_d, ws, cfg) = setup();
        let err = read(&ws, &cfg, "../../etc/passwd").unwrap_err();
        assert!(matches!(err, Error::PermissionDenied(_)));
    }

    #[test]
    fn write_normal_file() {
        let (_d, ws, cfg) = setup();
        write(&ws, &cfg, "out.txt", "data").unwrap();
        assert_eq!(
            std::fs::read_to_string(ws.root.join("out.txt")).unwrap(),
            "data"
        );
    }

    #[test]
    fn write_hardened_file_blocked() {
        let (_d, ws, mut cfg) = setup();
        cfg.hardened.push("Cargo.lock".into());
        std::fs::write(ws.root.join("Cargo.lock"), "old").unwrap();
        let err = write(&ws, &cfg, "Cargo.lock", "new").unwrap_err();
        assert!(matches!(err, Error::PermissionDenied(_)));
        // Original untouched.
        assert_eq!(
            std::fs::read_to_string(ws.root.join("Cargo.lock")).unwrap(),
            "old"
        );
    }

    #[test]
    fn write_hidden_file_blocked() {
        let (_d, ws, mut cfg) = setup();
        cfg.hidden.push(".env".into());
        std::fs::write(ws.root.join(".env"), "SECRET=old").unwrap();
        let err = write(&ws, &cfg, ".env", "SECRET=new").unwrap_err();
        assert!(matches!(err, Error::PermissionDenied(_)));
        // Original untouched, and the error never leaks the contents.
        assert_eq!(
            std::fs::read_to_string(ws.root.join(".env")).unwrap(),
            "SECRET=old"
        );
        assert!(!err.to_string().contains("SECRET"));
    }

    #[test]
    fn create_hidden_file_blocked() {
        let (_d, ws, mut cfg) = setup();
        cfg.hidden.push("*.key".into());
        let err = create(&ws, &cfg, "server.key", "private").unwrap_err();
        assert!(matches!(err, Error::PermissionDenied(_)));
        assert!(!ws.root.join("server.key").exists());
    }

    #[test]
    fn create_new_then_conflict() {
        let (_d, ws, cfg) = setup();
        create(&ws, &cfg, "new.txt", "x").unwrap();
        assert_eq!(
            std::fs::read_to_string(ws.root.join("new.txt")).unwrap(),
            "x"
        );
        let err = create(&ws, &cfg, "new.txt", "y").unwrap_err();
        assert!(matches!(
            err,
            Error::Filesystem(FilesystemError::AlreadyExists(_))
        ));
    }

    #[test]
    fn delete_moves_to_trash() {
        let (_d, ws, _cfg) = setup();
        let file = ws.root.join("gone.txt");
        std::fs::write(&file, "bye").unwrap();
        let entry = delete(&ws, "gone.txt").unwrap();
        assert!(!file.exists());
        assert!(entry.location.exists());
        // And it can be restored.
        trash::restore(&entry).unwrap();
        assert!(file.exists());
    }

    #[test]
    fn list_dir_filters_hidden() {
        let (_d, ws, mut cfg) = setup();
        cfg.hidden.push(".env".into());
        std::fs::write(ws.root.join("visible.txt"), "1").unwrap();
        std::fs::write(ws.root.join(".env"), "2").unwrap();
        let entries = list_dir(&ws, &cfg, ".").unwrap();
        let names: Vec<String> = entries
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"visible.txt".to_string()));
        assert!(!names.contains(&".env".to_string()));
    }
}
