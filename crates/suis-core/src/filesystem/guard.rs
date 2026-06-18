//! Hidden and hardened file checks driven by [`ProjectConfig`].
//!
//! Patterns are matched against both the workspace-relative path and the bare
//! file name, support `*` wildcards, and treat a directory-style pattern
//! (`secrets` / `secrets/`) as covering everything beneath it.

use std::path::Path;

use crate::projects::ProjectConfig;
use crate::util::wildcard_match;

fn matches_any(patterns: &[String], rel_path: &str, file_name: &str) -> bool {
    patterns.iter().any(|raw| {
        // A leading '/' is gitignore "anchored to root" — our paths are already
        // workspace-relative, so strip it; otherwise `/foo/` would match nothing.
        let pattern = raw.trim().trim_start_matches('/');
        if pattern.is_empty() {
            return false;
        }
        if pattern == rel_path || pattern == file_name {
            return true;
        }
        if wildcard_match(pattern, rel_path) || wildcard_match(pattern, file_name) {
            return true;
        }
        // Directory-style pattern (`secrets`, `secrets/`, `/secrets/`): cover the
        // directory entry itself and everything beneath it.
        let dir = pattern.trim_end_matches('/');
        !dir.is_empty() && (rel_path == dir || rel_path.starts_with(&format!("{dir}/")))
    })
}

fn parts(path: &Path) -> (String, String) {
    let rel = path.to_string_lossy().replace('\\', "/");
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    (rel, name)
}

/// Whether `rel_path` (workspace-relative) is hidden from reads and listings.
pub fn is_hidden(config: &ProjectConfig, rel_path: &Path) -> bool {
    let (rel, name) = parts(rel_path);
    matches_any(&config.hidden, &rel, &name)
}

/// Whether `rel_path` (workspace-relative) is hardened (writes require approval).
pub fn is_hardened(config: &ProjectConfig, rel_path: &Path) -> bool {
    let (rel, name) = parts(rel_path);
    matches_any(&config.hardened, &rel, &name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cfg(hidden: &[&str], hardened: &[&str]) -> ProjectConfig {
        ProjectConfig {
            hidden: hidden.iter().map(|s| s.to_string()).collect(),
            hardened: hardened.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn exact_name_hidden() {
        let c = cfg(&[".env"], &[]);
        assert!(is_hidden(&c, &PathBuf::from(".env")));
        assert!(is_hidden(&c, &PathBuf::from("config/.env")));
        assert!(!is_hidden(&c, &PathBuf::from("env.example")));
    }

    #[test]
    fn wildcard_hidden() {
        let c = cfg(&["*.key"], &[]);
        assert!(is_hidden(&c, &PathBuf::from("server.key")));
        assert!(is_hidden(&c, &PathBuf::from("certs/server.key")));
        assert!(!is_hidden(&c, &PathBuf::from("server.crt")));
    }

    #[test]
    fn directory_pattern_covers_subtree() {
        let c = cfg(&["secrets"], &[]);
        assert!(is_hidden(&c, &PathBuf::from("secrets/token.txt")));
        assert!(!is_hidden(&c, &PathBuf::from("public/readme.md")));
    }

    #[test]
    fn leading_slash_anchored_dir_pattern() {
        // gitignore-style anchored directory entry (the original bug).
        let c = cfg(&["/playwright-report/"], &[]);
        // The directory entry itself is hidden…
        assert!(is_hidden(&c, &PathBuf::from("playwright-report")));
        // …and everything beneath it.
        assert!(is_hidden(
            &c,
            &PathBuf::from("playwright-report/index.html")
        ));
        assert!(is_hidden(
            &c,
            &PathBuf::from("playwright-report/data/run.json")
        ));
        assert!(!is_hidden(&c, &PathBuf::from("src/main.rs")));
    }

    #[test]
    fn trailing_slash_dir_pattern() {
        let c = cfg(&["test-results/"], &[]);
        assert!(is_hidden(&c, &PathBuf::from("test-results")));
        assert!(is_hidden(&c, &PathBuf::from("test-results/out.txt")));
    }

    #[test]
    fn slash_wrapped_dotdir_pattern() {
        // `.claude` stored gitignore-style with surrounding slashes.
        let c = cfg(&["/.claude/"], &[]);
        assert!(is_hidden(&c, &PathBuf::from(".claude")));
        assert!(is_hidden(&c, &PathBuf::from(".claude/suis-test.txt")));
    }

    #[test]
    fn hardened_independent_of_hidden() {
        let c = cfg(&[], &["Cargo.lock"]);
        assert!(is_hardened(&c, &PathBuf::from("Cargo.lock")));
        assert!(!is_hidden(&c, &PathBuf::from("Cargo.lock")));
    }
}
