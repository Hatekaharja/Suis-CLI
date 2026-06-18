//! Boundary enforcement: ensuring a path stays within the workspace root.

use std::path::{Component, Path, PathBuf};

use super::Workspace;
use crate::errors::{Error, Result};

impl Workspace {
    /// Resolve `path` (relative paths are joined onto the root) into an absolute
    /// path, resolving symlinks in the existing portion and folding `..`/`.`
    /// components in the non-existing tail. The result is suitable for a prefix
    /// comparison against the workspace root.
    fn resolve_for_boundary(&self, path: &Path) -> PathBuf {
        let joined: PathBuf = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };

        // Walk up to the deepest ancestor that exists on disk.
        let mut ancestor = joined.clone();
        loop {
            if ancestor.exists() {
                break;
            }
            match ancestor.parent() {
                Some(parent) => ancestor = parent.to_path_buf(),
                None => break,
            }
        }

        // Canonicalize that ancestor to resolve any symlinks it traverses.
        let base = ancestor.canonicalize().unwrap_or_else(|_| ancestor.clone());

        // Re-apply the remaining (non-existing) components lexically.
        let tail = joined.strip_prefix(&ancestor).unwrap_or(&joined);
        let mut result = base;
        for comp in tail.components() {
            match comp {
                Component::ParentDir => {
                    result.pop();
                }
                Component::Normal(name) => result.push(name),
                Component::CurDir => {}
                Component::RootDir | Component::Prefix(_) => {}
            }
        }
        result
    }

    /// Returns true if `path` resolves to a location inside the workspace root
    /// (the root itself is considered inside).
    pub fn contains(&self, path: impl AsRef<Path>) -> bool {
        let resolved = self.resolve_for_boundary(path.as_ref());
        resolved.starts_with(&self.root)
    }

    /// Validate that `path` is inside the workspace, returning the resolved
    /// absolute path on success or [`Error::PermissionDenied`] on a boundary
    /// violation.
    pub fn check_boundary(&self, path: impl AsRef<Path>) -> Result<PathBuf> {
        let path = path.as_ref();
        let resolved = self.resolve_for_boundary(path);
        if resolved.starts_with(&self.root) {
            Ok(resolved)
        } else {
            Err(Error::PermissionDenied(format!(
                "path escapes workspace boundary: {}",
                path.display()
            )))
        }
    }

    /// Validate a path that is about to be **written**. Stricter than
    /// [`check_boundary`](Self::check_boundary): it additionally defeats the
    /// symlink TOCTOU where a link appears between the boundary check and the
    /// write and redirects it outside the workspace.
    ///
    /// On top of the boundary check it:
    /// - rejects a final component that already exists and is a symlink (a write
    ///   would follow it to its target), and
    /// - re-canonicalizes the parent directory and re-asserts it is inside the
    ///   root, so a symlinked parent can't redirect the write either.
    pub fn check_write_target(&self, path: impl AsRef<Path>) -> Result<PathBuf> {
        let resolved = self.check_boundary(path.as_ref())?;

        let deny = |what: &str| Error::PermissionDenied(format!("{what}: {}", resolved.display()));

        // A pre-existing symlink at the target itself would be followed on write.
        if let Ok(meta) = std::fs::symlink_metadata(&resolved) {
            if meta.file_type().is_symlink() {
                return Err(deny("refusing to write through a symlink"));
            }
        }

        // The parent must canonicalize to a location still inside the root, so a
        // symlinked ancestor can't redirect the write out of the workspace.
        if let Some(parent) = resolved.parent() {
            if let Ok(canonical_parent) = parent.canonicalize() {
                if !canonical_parent.starts_with(&self.root) {
                    return Err(deny("write target escapes workspace via a symlink"));
                }
            }
        }

        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    fn ws(dir: &TempDir) -> Workspace {
        Workspace::detect(dir.path()).unwrap()
    }

    #[test]
    fn path_inside_is_contained() {
        let dir = TempDir::new();
        let ws = ws(&dir);
        assert!(ws.contains("src/main.rs"));
        assert!(ws.contains(ws.root.join("a/b/c.txt")));
    }

    #[test]
    fn root_itself_is_allowed() {
        let dir = TempDir::new();
        let ws = ws(&dir);
        assert!(ws.contains(&ws.root));
        assert!(ws.check_boundary(&ws.root).is_ok());
    }

    #[test]
    fn parent_traversal_escapes() {
        let dir = TempDir::new();
        let ws = ws(&dir);
        assert!(!ws.contains("../../etc/passwd"));
        let err = ws.check_boundary("../../etc/passwd").unwrap_err();
        assert!(matches!(err, Error::PermissionDenied(_)));
    }

    #[test]
    fn parent_traversal_in_tail_escapes() {
        let dir = TempDir::new();
        let ws = ws(&dir);
        // A non-existing tail that climbs back out of the root.
        assert!(!ws.contains("newdir/../../outside"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_caught() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new();
        std::fs::write(outside.child("secret.txt"), "secret").unwrap();

        let dir = TempDir::new();
        let ws = ws(&dir);
        // A symlink inside the workspace pointing at the outside dir.
        symlink(outside.path(), ws.root.join("link")).unwrap();

        assert!(!ws.contains("link/secret.txt"));
        assert!(ws.check_boundary("link/secret.txt").is_err());
    }

    #[test]
    fn check_write_target_allows_a_normal_path() {
        let dir = TempDir::new();
        let ws = ws(&dir);
        // A plain in-workspace file (existing or not) is a valid write target.
        assert!(ws.check_write_target("src/main.rs").is_ok());
        std::fs::write(ws.root.join("existing.txt"), "x").unwrap();
        assert!(ws.check_write_target("existing.txt").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn check_write_target_refuses_a_dangling_symlink_final_component() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new();
        // A *dangling* symlink (target does not yet exist): `check_boundary`
        // alone lets this through — the resolved path looks in-bounds — but a
        // write would follow the link and create the file at its outside target.
        let dangling_target = outside.child("not-yet.txt");
        assert!(!dangling_target.exists());

        let dir = TempDir::new();
        let ws = ws(&dir);
        symlink(&dangling_target, ws.root.join("link.txt")).unwrap();

        // The plain boundary check is fooled; the write-target check is not.
        assert!(ws.check_boundary("link.txt").is_ok());
        let err = ws.check_write_target("link.txt").unwrap_err();
        assert!(matches!(err, Error::PermissionDenied(_)));
    }

    #[cfg(unix)]
    #[test]
    fn check_write_target_refuses_a_symlinked_parent() {
        use std::os::unix::fs::symlink;
        let outside = TempDir::new();

        let dir = TempDir::new();
        let ws = ws(&dir);
        // A symlinked directory inside the workspace pointing outside it; a write
        // beneath it would land outside the root.
        symlink(outside.path(), ws.root.join("escape")).unwrap();

        let err = ws.check_write_target("escape/new.txt").unwrap_err();
        assert!(matches!(err, Error::PermissionDenied(_)));
    }
}
