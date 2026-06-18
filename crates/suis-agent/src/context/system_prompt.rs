//! The static system prompt that opens every request, plus the per-mode
//! instruction block appended to it.

use suis_core::ProjectProfile;

use crate::runtime::mode::Mode;

/// Suis's agent identity, behavior rules, and tool-usage guidance. MVP: static.
pub const SYSTEM_PROMPT: &str = "\
You are Suis, a local-first coding agent working inside the user's project workspace.

- Work only within the workspace. Make minimal, targeted changes.
- There is no whole-file read. The file funnel is: search a file to find the lines you need, then read_lines a window around them, then edit. You must search a file before read_lines, and read_lines a file before editing an existing one (creating a new file needs neither). You may search and then read_lines the same file in one turn.
- Briefly say what you'll do, then do it with a tool. Actually emit the call — describing it is not doing it.
- Send tool arguments as a raw JSON object matching the schema: no markdown fences, no commentary. Example: {\"path\": \"src/main.rs\"}
- Read-only calls (read_lines, search, tree) may be batched in one turn; emit edits, commands, and other state-changing calls one at a time, awaiting each result.
- Don't repeat a read_lines/search/tree from this turn — its result is still above; reuse it. Once you can act, act.
- Risky edits and commands are approval-gated; respect the user's decision. Track multi-step work with the task tool.";

/// The mode-specific instruction block appended to [`SYSTEM_PROMPT`]. Kept
/// short — context is scarce — and matching what the mode actually enforces,
/// so the prompt never promises a capability the executor would refuse.
pub fn mode_prompt(mode: Mode) -> &'static str {
    match mode {
        Mode::Plan => {
            "\
Mode: PLAN. You are analyzing and planning, not implementing. Explore the \
codebase with tree, search, and read_lines, decompose the work, and draft a plan with the \
plan tool — steps with work tasks and verify tasks — for the user to approve. \
You cannot edit files or run commands in this mode; implementation happens \
later via /implement or agent mode."
        }
        Mode::Agent => {
            "\
Mode: AGENT. Full execution mode: implement the user's request, verify your \
work, and report the outcome."
        }
        Mode::Chat => {
            "\
Mode: CHAT. You are discussing, reviewing, and explaining. You may read and \
search the codebase to ground your answers, but you cannot edit files or run \
commands in this mode."
        }
    }
}

/// Render a project's cached [`ProjectProfile`] into the terse brief appended to
/// the system prompt, mirroring the style of
/// [`work_package::render_ledger`](super::work_package::render_ledger).
///
/// Brief only (summary, toolchain, commands, conventions) — the layout is
/// injected separately as a top-level snapshot, so it isn't repeated here.
pub fn render_profile(profile: &ProjectProfile) -> String {
    let mut out = String::from("Project profile (cached; refresh with /profile):\n");
    if !profile.summary.is_empty() {
        out.push_str(&profile.summary);
        out.push('\n');
    }
    if !profile.toolchain.is_empty() {
        out.push_str(&format!("Toolchain: {}\n", profile.toolchain));
    }
    if let Some(build) = &profile.build_cmd {
        out.push_str(&format!("Build: {build}\n"));
    }
    if let Some(test) = &profile.test_cmd {
        out.push_str(&format!("Test: {test}\n"));
    }
    if !profile.conventions.is_empty() {
        out.push_str("Conventions:\n");
        for convention in &profile.conventions {
            out.push_str(&format!("- {convention}\n"));
        }
    }

    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_profile() -> ProjectProfile {
        ProjectProfile {
            summary: "Rust workspace.".into(),
            toolchain: "Rust (cargo)".into(),
            build_cmd: Some("cargo build".into()),
            test_cmd: Some("cargo test".into()),
            conventions: vec!["Lint with cargo clippy.".into()],
            generated_at: "2026-06-15".into(),
        }
    }

    #[test]
    fn render_profile_includes_the_brief() {
        let block = render_profile(&sample_profile());
        assert!(block.contains("Project profile"));
        assert!(block.contains("Toolchain: Rust (cargo)"));
        assert!(block.contains("Build: cargo build"));
        assert!(block.contains("Test: cargo test"));
        assert!(block.contains("- Lint with cargo clippy."));
        // The layout is injected separately, never inside the profile brief.
        assert!(!block.contains("Layout"));
    }

    #[test]
    fn render_profile_omits_empty_fields() {
        let mut profile = sample_profile();
        profile.build_cmd = None;
        profile.conventions.clear();
        let block = render_profile(&profile);
        assert!(!block.contains("Build:"));
        assert!(!block.contains("Conventions:"));
    }

    #[test]
    fn system_prompt_carries_concrete_tool_guidance() {
        // The guidance that helps weaker local models actually emit valid calls.
        assert!(SYSTEM_PROMPT.contains("Actually emit the call"));
        // Read-only calls may batch; state-changing calls stay one at a time.
        assert!(SYSTEM_PROMPT.contains("Read-only calls"));
        assert!(SYSTEM_PROMPT.contains("one at a time"));
        assert!(SYSTEM_PROMPT.contains("no markdown fences"));
        assert!(SYSTEM_PROMPT.contains("{\"path\": \"src/main.rs\"}"));
    }
}
