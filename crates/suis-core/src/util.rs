//! Internal helpers shared across `suis-core` modules.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `path` atomically: write to a unique temporary sibling file
/// and then rename it into place. Parent directories are created as needed.
///
/// The rename is atomic on POSIX filesystems, so readers never observe a
/// partially written file.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tmp".to_string());
    let tmp_name = format!(".{file_name}.{pid}.{counter}.tmp");
    let tmp_path: PathBuf = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    };

    std::fs::write(&tmp_path, bytes)?;
    match std::fs::rename(&tmp_path, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

/// Like [`write_atomic`], but creates the file readable only by its owner
/// (mode `0600` on Unix) and best-effort tightens the parent directory to
/// `0700`. Used for files that may hold credentials or security policy
/// (provider API keys, permission grants), so they are never world-readable on
/// a shared host. On non-Unix it is identical to [`write_atomic`].
///
/// The temp file is created `0600` from the outset (not chmod'd afterward), so
/// there is no window in which it is world-readable before the rename.
pub(crate) fn write_atomic_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
                // Best-effort: keep the config dir owner-only too.
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            }
        }

        let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "tmp".to_string());
        let tmp_name = format!(".{file_name}.{pid}.{counter}.tmp");
        let tmp_path: PathBuf = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
            _ => PathBuf::from(tmp_name),
        };

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp_path)?;
        let write_result = file.write_all(bytes);
        drop(file);
        if let Err(e) = write_result {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }
        match std::fs::rename(&tmp_path, path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp_path);
                Err(e)
            }
        }
    }
    #[cfg(not(unix))]
    {
        write_atomic(path, bytes)
    }
}

/// Match `text` against `pattern`, where `*` in the pattern matches any
/// (possibly empty) sequence of characters. All other characters match
/// literally. Used for command and file-glob style matching.
pub(crate) fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    // Position of the most recent `*` in the pattern and the text index it was
    // matched against, for backtracking.
    let mut star_p: Option<usize> = None;
    let mut star_t = 0usize;

    while ti < t.len() {
        if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_p = Some(pi);
            star_t = ti;
            pi += 1;
        } else if let Some(sp) = star_p {
            pi = sp + 1;
            star_t += 1;
            ti = star_t;
        } else {
            return false;
        }
    }

    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_exact() {
        assert!(wildcard_match("cargo test", "cargo test"));
        assert!(!wildcard_match("cargo test", "cargo build"));
    }

    #[test]
    fn wildcard_trailing_star() {
        assert!(wildcard_match("cargo *", "cargo test"));
        assert!(wildcard_match("cargo *", "cargo build --release"));
        assert!(!wildcard_match("cargo *", "npm test"));
    }

    #[test]
    fn wildcard_middle_star() {
        assert!(wildcard_match("*.env", ".env"));
        assert!(wildcard_match("*.env", "config/prod.env"));
        assert!(!wildcard_match("*.env", "env.example"));
    }

    #[test]
    fn write_atomic_creates_dirs_and_file() {
        let dir = crate::test_util::TempDir::new();
        let path = dir.child("a/b/c.json");
        write_atomic(&path, b"hello").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn write_atomic_private_writes_contents() {
        let dir = crate::test_util::TempDir::new();
        let path = dir.child("secret/key.json");
        write_atomic_private(&path, b"hunter2").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hunter2");
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_private_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::test_util::TempDir::new();
        let path = dir.child("creds.json");
        write_atomic_private(&path, b"{}").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "credential file must be owner-read/write only");
        // Overwriting an existing private file keeps it owner-only.
        write_atomic_private(&path, b"{\"x\":1}").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
