//! Soft-delete trash system. Deleted files are moved under
//! `.suis/trash/<timestamp>/<relative-path>` instead of being removed, so they
//! can be restored.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::errors::{FilesystemError, Result};
use crate::workspace::Workspace;

/// A record of a trashed file, sufficient to restore it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashEntry {
    /// Timestamp directory name the file was filed under.
    pub trashed_at: String,
    /// The original absolute path the file lived at.
    pub original: PathBuf,
    /// Where the file now lives inside the trash.
    pub location: PathBuf,
}

/// The trash root for a workspace: `.suis/trash/`.
pub fn trash_root(workspace: &Workspace) -> PathBuf {
    workspace.suis_dir.join("trash")
}

fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}-{:09}", now.as_secs(), now.subsec_nanos())
}

/// Move `path` into the workspace trash, returning a [`TrashEntry`].
///
/// `path` is expected to be an absolute path already validated to be inside the
/// workspace.
pub fn trash(workspace: &Workspace, path: &Path) -> Result<TrashEntry> {
    if !path.exists() {
        return Err(FilesystemError::NotFound(path.to_path_buf()).into());
    }
    let rel = path.strip_prefix(&workspace.root).unwrap_or(path);
    let trashed_at = timestamp();
    let location = trash_root(workspace).join(&trashed_at).join(rel);
    if let Some(parent) = location.parent() {
        std::fs::create_dir_all(parent).map_err(FilesystemError::Io)?;
    }
    move_path(path, &location)?;
    Ok(TrashEntry {
        trashed_at,
        original: path.to_path_buf(),
        location,
    })
}

/// Restore a previously trashed entry to its original location.
pub fn restore(entry: &TrashEntry) -> Result<()> {
    if !entry.location.exists() {
        return Err(FilesystemError::NotFound(entry.location.clone()).into());
    }
    if let Some(parent) = entry.original.parent() {
        std::fs::create_dir_all(parent).map_err(FilesystemError::Io)?;
    }
    move_path(&entry.location, &entry.original)?;
    Ok(())
}

/// Rename `from` to `to`, falling back to copy+remove across filesystems.
fn move_path(from: &Path, to: &Path) -> Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    if from.is_dir() {
        copy_dir(from, to)?;
        std::fs::remove_dir_all(from).map_err(FilesystemError::Io)?;
    } else {
        std::fs::copy(from, to).map_err(FilesystemError::Io)?;
        std::fs::remove_file(from).map_err(FilesystemError::Io)?;
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).map_err(FilesystemError::Io)?;
    for entry in std::fs::read_dir(from).map_err(FilesystemError::Io)? {
        let entry = entry.map_err(FilesystemError::Io)?;
        let file_type = entry.file_type().map_err(FilesystemError::Io)?;
        let dest = to.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), &dest).map_err(FilesystemError::Io)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    fn ws(dir: &TempDir) -> Workspace {
        Workspace::detect(dir.path()).unwrap()
    }

    #[test]
    fn trash_then_restore_round_trips() {
        let dir = TempDir::new();
        let workspace = ws(&dir);
        let file = workspace.root.join("notes.txt");
        std::fs::write(&file, "important").unwrap();

        let entry = trash(&workspace, &file).unwrap();
        assert!(!file.exists(), "original should be gone");
        assert!(entry.location.exists(), "trash copy should exist");

        restore(&entry).unwrap();
        assert!(file.exists(), "original should be back");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "important");
    }

    #[test]
    fn trash_missing_file_errors() {
        let dir = TempDir::new();
        let workspace = ws(&dir);
        let err = trash(&workspace, &workspace.root.join("nope.txt")).unwrap_err();
        assert!(matches!(
            err,
            crate::errors::Error::Filesystem(FilesystemError::NotFound(_))
        ));
    }
}
