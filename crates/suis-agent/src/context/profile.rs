//! Deterministic, offline project profiling.
//!
//! [`detect`] is a single, free first pass: it reads the manifest files at the
//! workspace root (`Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`,
//! `Makefile`) and infers the toolchain plus likely build/test commands. No
//! model call, no network, no guessing beyond what a manifest plainly states —
//! the result seeds [`ProjectConfig::profile`] and [`ProjectConfig::verify_command`]
//! so a session opens already knowing the project's shape.
//!
//! When nothing recognizable is present, [`detect`] still returns a profile (an
//! empty toolchain, no commands) — the caller decides whether an inference that
//! found nothing is worth caching.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use suis_core::{ProjectProfile, Workspace};

/// Infer a [`ProjectProfile`] from the manifest files at `workspace`'s root.
///
/// Manifests are checked in a fixed priority order; the first recognized one
/// drives the headline toolchain and the build/test commands, while the others
/// contribute a note to the summary (a Rust project with a `Makefile`, say).
/// The inference is deterministic apart from `generated_at`, which records the
/// day it ran.
pub fn detect(workspace: &Workspace) -> ProjectProfile {
    let root = &workspace.root;
    let mut detected: Vec<Toolchain> = Vec::new();

    if exists(root, "Cargo.toml") {
        detected.push(rust(root));
    }
    if exists(root, "package.json") {
        detected.push(node(root));
    }
    if exists(root, "pyproject.toml") {
        detected.push(python());
    }
    if exists(root, "go.mod") {
        detected.push(go());
    }
    if exists(root, "Makefile") {
        detected.push(make());
    }

    build_profile(detected)
}

/// One recognized toolchain and the commands inferred for it.
struct Toolchain {
    /// Headline label, e.g. "Rust (cargo)".
    label: String,
    build_cmd: Option<String>,
    test_cmd: Option<String>,
    /// Confident, project-shaping facts worth stating up front.
    conventions: Vec<String>,
}

/// Fold the recognized toolchains into a profile: the first drives the headline
/// commands, the rest only widen the summary and conventions.
fn build_profile(detected: Vec<Toolchain>) -> ProjectProfile {
    let generated_at = utc_date();
    let Some((primary, extra)) = detected.split_first() else {
        return ProjectProfile {
            generated_at,
            ..Default::default()
        };
    };

    let toolchain = primary.label.clone();
    let summary = if extra.is_empty() {
        format!("{} project.", primary.label)
    } else {
        let others: Vec<&str> = extra.iter().map(|t| t.label.as_str()).collect();
        format!("{} project (also: {}).", primary.label, others.join(", "))
    };

    // The primary's conventions lead; later toolchains append theirs so a
    // Makefile alongside cargo still surfaces its `make` entry points.
    let mut conventions = primary.conventions.clone();
    for tc in extra {
        conventions.extend(tc.conventions.iter().cloned());
    }

    ProjectProfile {
        summary,
        toolchain,
        build_cmd: primary.build_cmd.clone(),
        test_cmd: primary.test_cmd.clone(),
        conventions,
        generated_at,
    }
}

fn rust(root: &Path) -> Toolchain {
    let mut conventions = vec!["Lint with `cargo clippy --workspace --all-targets`.".to_string()];
    // A `[workspace]` table means commands should usually span every crate.
    if read(root, "Cargo.toml")
        .map(|c| c.contains("[workspace]"))
        .unwrap_or(false)
    {
        conventions.push("Cargo workspace — prefer `--workspace` for build/test.".to_string());
    }
    Toolchain {
        label: "Rust (cargo)".to_string(),
        build_cmd: Some("cargo build".to_string()),
        test_cmd: Some("cargo test".to_string()),
        conventions,
    }
}

fn node(root: &Path) -> Toolchain {
    // The lockfile names the package manager; default to npm when none is found.
    let (pm, run) = if exists(root, "pnpm-lock.yaml") {
        ("pnpm", "pnpm")
    } else if exists(root, "yarn.lock") {
        ("yarn", "yarn")
    } else {
        ("npm", "npm run")
    };

    let scripts = read(root, "package.json")
        .as_deref()
        .map(package_scripts)
        .unwrap_or_default();
    // Only claim a command the project actually defines as a script.
    let build_cmd = scripts
        .iter()
        .any(|s| s == "build")
        .then(|| format!("{run} build"));
    // `npm test` / `pnpm test` / `yarn test` are the canonical spellings.
    let test_cmd = scripts
        .iter()
        .any(|s| s == "test")
        .then(|| format!("{pm} test"));

    Toolchain {
        label: format!("Node.js ({pm})"),
        build_cmd,
        test_cmd,
        conventions: vec![format!("Manage packages with {pm}.")],
    }
}

fn python() -> Toolchain {
    Toolchain {
        label: "Python".to_string(),
        build_cmd: None,
        test_cmd: Some("pytest".to_string()),
        conventions: Vec::new(),
    }
}

fn go() -> Toolchain {
    Toolchain {
        label: "Go".to_string(),
        build_cmd: Some("go build ./...".to_string()),
        test_cmd: Some("go test ./...".to_string()),
        conventions: Vec::new(),
    }
}

fn make() -> Toolchain {
    Toolchain {
        label: "Makefile".to_string(),
        build_cmd: Some("make".to_string()),
        test_cmd: Some("make test".to_string()),
        conventions: vec!["A Makefile is present — check it for project tasks.".to_string()],
    }
}

/// The `scripts` object keys from a `package.json`, or empty when the file is
/// unparseable or has no scripts. Best-effort: a malformed manifest yields no
/// scripts rather than an error (detection never fails the session).
fn package_scripts(raw: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| v.get("scripts").and_then(|s| s.as_object()).cloned())
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

fn exists(root: &Path, name: &str) -> bool {
    root.join(name).is_file()
}

fn read(root: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(root.join(name)).ok()
}

/// Today's UTC date as `YYYY-MM-DD`, dependency-free (Howard Hinnant's
/// days-from-civil inverse). A clock error before the epoch degrades to the
/// epoch date rather than panicking.
fn utc_date() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let z = (secs / 86_400) as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::Fixture;

    fn profile(fx: &Fixture) -> ProjectProfile {
        detect(&fx.workspace)
    }

    #[test]
    fn cargo_manifest_infers_rust() {
        let fx = Fixture::new();
        fx.write("Cargo.toml", "[package]\nname = \"demo\"\n");
        let p = profile(&fx);
        assert_eq!(p.toolchain, "Rust (cargo)");
        assert_eq!(p.build_cmd.as_deref(), Some("cargo build"));
        assert_eq!(p.test_cmd.as_deref(), Some("cargo test"));
        assert!(p.conventions.iter().any(|c| c.contains("clippy")));
        assert!(!p.generated_at.is_empty());
    }

    #[test]
    fn cargo_workspace_adds_workspace_convention() {
        let fx = Fixture::new();
        fx.write("Cargo.toml", "[workspace]\nmembers = [\"a\"]\n");
        let p = profile(&fx);
        assert!(p.conventions.iter().any(|c| c.contains("workspace")));
    }

    #[test]
    fn package_json_scripts_drive_node_commands() {
        let fx = Fixture::new();
        fx.write(
            "package.json",
            r#"{"scripts":{"build":"tsc","test":"vitest"}}"#,
        );
        let p = profile(&fx);
        assert_eq!(p.toolchain, "Node.js (npm)");
        assert_eq!(p.build_cmd.as_deref(), Some("npm run build"));
        assert_eq!(p.test_cmd.as_deref(), Some("npm test"));
    }

    #[test]
    fn pnpm_lockfile_selects_pnpm() {
        let fx = Fixture::new();
        fx.write("package.json", r#"{"scripts":{"test":"jest"}}"#);
        fx.write("pnpm-lock.yaml", "lockfileVersion: 6.0\n");
        let p = profile(&fx);
        assert_eq!(p.toolchain, "Node.js (pnpm)");
        assert_eq!(p.test_cmd.as_deref(), Some("pnpm test"));
    }

    #[test]
    fn package_json_without_scripts_yields_no_commands() {
        let fx = Fixture::new();
        fx.write("package.json", r#"{"name":"demo"}"#);
        let p = profile(&fx);
        assert_eq!(p.build_cmd, None);
        assert_eq!(p.test_cmd, None);
    }

    #[test]
    fn malformed_package_json_does_not_panic() {
        let fx = Fixture::new();
        fx.write("package.json", "{ not json");
        let p = profile(&fx);
        assert_eq!(p.toolchain, "Node.js (npm)");
        assert_eq!(p.test_cmd, None);
    }

    #[test]
    fn go_module_infers_go() {
        let fx = Fixture::new();
        fx.write("go.mod", "module demo\n\ngo 1.22\n");
        let p = profile(&fx);
        assert_eq!(p.toolchain, "Go");
        assert_eq!(p.test_cmd.as_deref(), Some("go test ./..."));
    }

    #[test]
    fn first_manifest_leads_and_others_join_the_summary() {
        // Cargo has priority over an accompanying Makefile.
        let fx = Fixture::new();
        fx.write("Cargo.toml", "[package]\nname=\"d\"\n");
        fx.write("Makefile", "test:\n\tcargo test\n");
        let p = profile(&fx);
        assert_eq!(p.toolchain, "Rust (cargo)");
        assert!(p.summary.contains("Makefile"));
        // The Makefile's own convention still surfaces.
        assert!(p.conventions.iter().any(|c| c.contains("Makefile")));
    }

    #[test]
    fn empty_workspace_yields_blank_toolchain() {
        let fx = Fixture::new();
        let p = profile(&fx);
        assert!(p.toolchain.is_empty());
        assert_eq!(p.build_cmd, None);
        assert_eq!(p.test_cmd, None);
        // Still stamped with a date.
        assert!(!p.generated_at.is_empty());
    }

    #[test]
    fn utc_date_is_iso_shaped() {
        let date = utc_date();
        assert_eq!(date.len(), 10);
        let parts: Vec<&str> = date.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), 4);
        assert_eq!(parts[1].len(), 2);
        assert_eq!(parts[2].len(), 2);
    }
}
