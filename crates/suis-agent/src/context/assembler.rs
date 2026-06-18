//! Assembling a [`ChatRequest`] from session state, per the MVP context layout:
//!
//! ```text
//! System Prompt → Available Tools → Current Task → Chat History
//! ```
//!
//! "Available Tools" live in the request's `tools` field; everything else is a
//! message. The current user input is the last entry of the chat history, so it
//! is not passed separately.

use suis_core::{ProjectConfig, Workspace};
use suis_providers::Capabilities;

use super::system_prompt::{mode_prompt, render_profile, SYSTEM_PROMPT};
use super::{budget, work_package};
use crate::conversation::{Message, Role};
use crate::runtime::mode::Mode;
use crate::tasks::Task;
use crate::{ChatRequest, ToolDefinition};

/// An assembled request plus the context accounting the UI needs: whether the
/// history was pruned to fit, and the post-prune estimated token usage.
pub struct Assembled {
    /// The request to send to the model.
    pub request: ChatRequest,
    /// Whether mechanical pruning altered the history for this request.
    pub pruned: bool,
    /// Estimated tokens the assembled messages occupy (chars/4 heuristic).
    pub used_tokens: usize,
}

/// Builds requests sent to the model.
pub struct ContextAssembler;

impl ContextAssembler {
    /// Assemble a streaming request.
    ///
    /// The system prompt carries the `mode`'s instruction block. Tools are
    /// exposed only when the model supports tool use *and* the mode allows
    /// them *and* they appear in the project's `allowed_tools` (see
    /// [`Self::select_tools`]). The active task, if any, is injected as a
    /// system message.
    ///
    /// Before the request is finalized the history is mechanically pruned to
    /// fit `budget` estimated tokens (see [`budget::prune`]). The system prompt
    /// and active-task note are always kept; when `pin_work_package` is set
    /// (an implementation session) the opening work-package turn is kept too.
    /// Pruning operates on this request's copy only — `session.history` is left
    /// intact, so an under-budget session is byte-identical to before.
    #[allow(clippy::too_many_arguments)] // assembling needs the full session view
    pub fn build(
        workspace: &Workspace,
        model_id: &str,
        capabilities: &Capabilities,
        project: &ProjectConfig,
        mode: Mode,
        active_task: Option<&Task>,
        history: &[Message],
        tool_defs: &[ToolDefinition],
        budget: usize,
        pin_work_package: bool,
    ) -> Assembled {
        let mut messages = Vec::with_capacity(history.len() + 2);
        let mut system = format!("{SYSTEM_PROMPT}\n\n{}", mode_prompt(mode));
        // A cached profile makes the prompt project-aware: append its brief
        // (summary, toolchain, commands, conventions) so the session opens
        // knowing how to build/test and what conventions to follow.
        if let Some(profile) = &project.profile {
            system.push_str("\n\n");
            system.push_str(&render_profile(profile));
        }
        // Always give a cheap top-level layout so the model starts oriented and
        // doesn't burn a `tree` call just to see the project's shape; it drills
        // deeper with the tree tool only when it actually needs to. Both blocks
        // ride in the pinned system message, so the pruner never drops them.
        let layout = work_package::top_level_snapshot(workspace, project);
        if !layout.is_empty() {
            system.push_str(
                "\n\nProject layout (top level — use the tree tool to look inside a directory):\n",
            );
            system.push_str(&layout);
        }
        messages.push(Message::text(Role::System, system));

        // The pinned prefix the pruner must never drop: the system prompt plus
        // the active-task note, if present.
        let mut pinned_prefix = 1;
        if let Some(task) = active_task {
            messages.push(Message::text(
                Role::System,
                format!("Current task [{}]: {}", task.id, task.title),
            ));
            pinned_prefix += 1;
        }

        messages.extend(history.iter().cloned());

        let pruned = budget::prune(&mut messages, pinned_prefix, pin_work_package, budget);
        let used_tokens = budget::total_tokens(&messages);

        let tools = Self::select_tools(capabilities, project, mode, tool_defs);

        Assembled {
            request: ChatRequest {
                model: model_id.to_string(),
                messages,
                tools,
                stream: true,
            },
            pruned,
            used_tokens,
        }
    }

    /// The tools to advertise: the intersection of what the mode allows and
    /// what the project's `allowed_tools` exposes — project visibility defines
    /// what exists, mode defines what is usable right now. Empty (`None`) when
    /// the model can't use tools or nothing passes both filters.
    ///
    /// The `plan` tool is exempt from the project filter: it exists only in
    /// Plan mode, every use is individually approval-gated, and it governs
    /// Suis's own workflow artifact rather than project resources — so a
    /// project config written before plans existed doesn't silently disable
    /// planning.
    fn select_tools(
        capabilities: &Capabilities,
        project: &ProjectConfig,
        mode: Mode,
        tool_defs: &[ToolDefinition],
    ) -> Option<Vec<ToolDefinition>> {
        if !capabilities.tool_use {
            return None;
        }
        let allowed: Vec<ToolDefinition> = tool_defs
            .iter()
            .filter(|t| mode.allows_tool(&t.name))
            .filter(|t| t.name == "plan" || project.allowed_tools.iter().any(|a| a == &t.name))
            .cloned()
            .collect();
        if allowed.is_empty() {
            None
        } else {
            Some(allowed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::TaskStatus;
    use crate::test_util::Fixture;
    use crate::tools::default_tool_definitions;
    use suis_core::ProjectProfile;

    fn caps(tool_use: bool) -> Capabilities {
        Capabilities {
            chat: true,
            streaming: true,
            tool_use,
            structured_output: false,
        }
    }

    fn project_with_tools(tools: &[&str]) -> ProjectConfig {
        ProjectConfig {
            allowed_tools: tools.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    /// A throwaway workspace handle rooted at the crate dir. The assembler now
    /// always snapshots the top-level layout, but the tests using this only
    /// assert on roles, lengths, and substrings, so whatever the cwd contains is
    /// harmless. Tests that need a deterministic layout (or none) use a
    /// `Fixture` instead.
    fn ws() -> Workspace {
        Workspace {
            root: std::path::PathBuf::from("."),
            suis_dir: std::path::PathBuf::from("./.suis"),
            is_git: false,
        }
    }

    #[test]
    fn opens_with_system_prompt_then_history() {
        let project = project_with_tools(&[]);
        let history = vec![Message::text(Role::User, "hello")];
        let req = ContextAssembler::build(
            &ws(),
            "m",
            &caps(false),
            &project,
            Mode::Agent,
            None,
            &history,
            &default_tool_definitions(),
            1_000_000,
            false,
        )
        .request;
        assert_eq!(req.messages[0].role, Role::System);
        assert_eq!(req.messages.last().unwrap().content, "hello");
        assert!(req.stream);
    }

    #[test]
    fn active_task_injected_after_system() {
        let project = project_with_tools(&[]);
        let task = Task {
            id: "t1".into(),
            title: "write tests".into(),
            status: TaskStatus::Doing,
        };
        let req = ContextAssembler::build(
            &ws(),
            "m",
            &caps(false),
            &project,
            Mode::Agent,
            Some(&task),
            &[],
            &default_tool_definitions(),
            1_000_000,
            false,
        )
        .request;
        assert_eq!(req.messages[0].role, Role::System);
        assert_eq!(req.messages[1].role, Role::System);
        assert!(req.messages[1].content.contains("write tests"));
    }

    #[test]
    fn no_tools_when_capability_absent() {
        let project = project_with_tools(&["read_lines", "edit"]);
        let req = ContextAssembler::build(
            &ws(),
            "m",
            &caps(false),
            &project,
            Mode::Agent,
            None,
            &[],
            &default_tool_definitions(),
            1_000_000,
            false,
        )
        .request;
        assert!(req.tools.is_none());
    }

    #[test]
    fn tools_filtered_by_allowed_list() {
        let project = project_with_tools(&["read_lines"]);
        let req = ContextAssembler::build(
            &ws(),
            "m",
            &caps(true),
            &project,
            Mode::Agent,
            None,
            &[],
            &default_tool_definitions(),
            1_000_000,
            false,
        )
        .request;
        let tools = req.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "read_lines");
    }

    #[test]
    fn empty_allowed_list_yields_no_tools() {
        let project = project_with_tools(&[]);
        let req = ContextAssembler::build(
            &ws(),
            "m",
            &caps(true),
            &project,
            Mode::Agent,
            None,
            &[],
            &default_tool_definitions(),
            1_000_000,
            false,
        )
        .request;
        assert!(req.tools.is_none());
    }

    #[test]
    fn plan_tool_advertised_only_in_plan_mode_without_project_listing() {
        // The project lists `read_lines` but (like any pre-plans config) not `plan`.
        let project = project_with_tools(&["read_lines", "edit"]);
        let names = |mode: Mode| -> Vec<String> {
            ContextAssembler::build(
                &ws(),
                "m",
                &caps(true),
                &project,
                mode,
                None,
                &[],
                &default_tool_definitions(),
                1_000_000,
                false,
            )
            .request
            .tools
            .unwrap_or_default()
            .into_iter()
            .map(|t| t.name)
            .collect()
        };
        assert_eq!(names(Mode::Plan), vec!["read_lines", "plan"]);
        assert_eq!(names(Mode::Agent), vec!["read_lines", "edit"]);
        assert_eq!(names(Mode::Chat), vec!["read_lines"]);
    }

    #[test]
    fn each_mode_injects_its_marker_and_not_the_others() {
        let project = project_with_tools(&[]);
        for (mode, marker, absent) in [
            (Mode::Plan, "Mode: PLAN", ["Mode: AGENT", "Mode: CHAT"]),
            (Mode::Agent, "Mode: AGENT", ["Mode: PLAN", "Mode: CHAT"]),
            (Mode::Chat, "Mode: CHAT", ["Mode: PLAN", "Mode: AGENT"]),
        ] {
            let req = ContextAssembler::build(
                &ws(),
                "m",
                &caps(false),
                &project,
                mode,
                None,
                &[],
                &default_tool_definitions(),
                1_000_000,
                false,
            )
            .request;
            let system = &req.messages[0].content;
            assert!(system.contains(marker), "{mode}: missing {marker}");
            for other in absent {
                assert!(!system.contains(other), "{mode}: stray {other}");
            }
            // The shared identity prompt is still present.
            assert!(system.contains("You are Suis"));
        }
    }

    #[test]
    fn over_budget_history_is_pruned_and_reported() {
        let project = project_with_tools(&[]);
        // Many fat user/assistant turns, well over a small budget.
        let mut history = Vec::new();
        for i in 0..20 {
            history.push(Message::text(
                Role::User,
                format!("turn {i} {}", "x".repeat(400)),
            ));
            history.push(Message::text(Role::Assistant, "ok".to_string()));
        }
        let assembled = ContextAssembler::build(
            &ws(),
            "m",
            &caps(false),
            &project,
            Mode::Agent,
            None,
            &history,
            &default_tool_definitions(),
            200,
            false,
        );
        assert!(assembled.pruned, "an over-budget history must be pruned");
        assert!(assembled.used_tokens > 0);
        // The system prompt is always first and intact.
        assert_eq!(assembled.request.messages[0].role, Role::System);
        assert!(assembled.request.messages[0]
            .content
            .contains("You are Suis"));
        // Fewer messages than were supplied.
        assert!(assembled.request.messages.len() < history.len() + 1);
    }

    #[test]
    fn under_budget_build_reports_no_pruning() {
        let project = project_with_tools(&[]);
        let history = vec![Message::text(Role::User, "hi")];
        let assembled = ContextAssembler::build(
            &ws(),
            "m",
            &caps(false),
            &project,
            Mode::Agent,
            None,
            &history,
            &default_tool_definitions(),
            1_000_000,
            false,
        );
        assert!(!assembled.pruned);
        assert_eq!(assembled.request.messages.last().unwrap().content, "hi");
    }

    #[test]
    fn no_profile_reproduces_the_static_prompt() {
        // Without a cached profile and an empty workspace (no layout to inject)
        // the system message is exactly the static prompt plus the mode block.
        let fx = Fixture::new();
        let req = ContextAssembler::build(
            &fx.workspace,
            "m",
            &caps(false),
            &fx.project,
            Mode::Agent,
            None,
            &[],
            &default_tool_definitions(),
            1_000_000,
            false,
        )
        .request;
        assert_eq!(
            req.messages[0].content,
            format!("{SYSTEM_PROMPT}\n\n{}", mode_prompt(Mode::Agent))
        );
        assert!(!req.messages[0].content.contains("Project profile"));
        assert!(!req.messages[0].content.contains("Project layout"));
    }

    #[test]
    fn top_level_layout_injected_without_a_profile() {
        // Even with no cached profile, a non-empty workspace contributes a
        // top-level layout so the model opens oriented.
        let fx = Fixture::new();
        fx.write("src/main.rs", "fn main() {}");
        fx.write("Cargo.toml", "[package]");
        let req = ContextAssembler::build(
            &fx.workspace,
            "m",
            &caps(false),
            &fx.project,
            Mode::Agent,
            None,
            &[],
            &default_tool_definitions(),
            1_000_000,
            false,
        )
        .request;
        let system = &req.messages[0].content;
        let layout_at = system
            .find("Project layout (top level")
            .expect("layout block");
        let layout = &system[layout_at..];
        assert!(layout.contains("src/"));
        assert!(layout.contains("Cargo.toml"));
        // Top level only — the file inside src/ is not listed, nor its body.
        assert!(!layout.contains("main.rs"));
        assert!(!system.contains("fn main"));
    }

    #[test]
    fn cached_profile_is_appended_to_the_system_prompt() {
        let mut fx = Fixture::new();
        fx.write("src/main.rs", "fn main() {}");
        fx.project.profile = Some(ProjectProfile {
            summary: "Rust workspace.".into(),
            toolchain: "Rust (cargo)".into(),
            build_cmd: Some("cargo build".into()),
            test_cmd: Some("cargo test".into()),
            conventions: vec!["Lint with cargo clippy.".into()],
            generated_at: "2026-06-15".into(),
        });

        let req = ContextAssembler::build(
            &fx.workspace,
            "m",
            &caps(false),
            &fx.project,
            Mode::Agent,
            None,
            &[],
            &default_tool_definitions(),
            1_000_000,
            false,
        )
        .request;

        let system = &req.messages[0].content;
        // The static prompt still opens the message; the profile follows it.
        assert!(system.starts_with(SYSTEM_PROMPT));
        let profile_at = system
            .find("Project profile")
            .expect("profile block present");
        let mode_at = system.find("Mode: AGENT").expect("mode block present");
        assert!(profile_at > mode_at, "profile follows the mode block");
        assert!(system.contains("Toolchain: Rust (cargo)"));
        assert!(system.contains("Test: cargo test"));
        // The live layout snapshot rode along (structure, not file bodies).
        assert!(system.contains("src/"));
        assert!(!system.contains("fn main"));
    }

    #[test]
    fn pinned_profile_survives_a_tiny_budget() {
        // The profile rides in the pinned system message, so even a brutal
        // budget that prunes the history leaves the brief intact.
        let mut fx = Fixture::new();
        fx.project.profile = Some(ProjectProfile {
            toolchain: "Rust (cargo)".into(),
            test_cmd: Some("cargo test".into()),
            generated_at: "2026-06-15".into(),
            ..Default::default()
        });
        let mut history = Vec::new();
        for i in 0..20 {
            history.push(Message::text(
                Role::User,
                format!("turn {i} {}", "x".repeat(400)),
            ));
            history.push(Message::text(Role::Assistant, "ok".to_string()));
        }
        let assembled = ContextAssembler::build(
            &fx.workspace,
            "m",
            &caps(false),
            &fx.project,
            Mode::Agent,
            None,
            &history,
            &default_tool_definitions(),
            200,
            false,
        );
        assert!(assembled.pruned);
        assert!(assembled.request.messages[0]
            .content
            .contains("Toolchain: Rust (cargo)"));
    }
}
