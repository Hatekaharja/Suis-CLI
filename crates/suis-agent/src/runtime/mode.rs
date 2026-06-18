//! Runtime modes: who may do what, enforced rather than suggested.
//!
//! A [`Mode`] gates the agent's capabilities at two independent points: the
//! context assembler only *advertises* mode-allowed tools to the model, and the
//! [`ToolExecutor`](crate::tools::ToolExecutor) *refuses* calls to tools outside
//! the mode before any permission gate runs. Plan and Chat mode therefore
//! cannot edit files or run commands even if the model hallucinates the call.

use std::fmt;

/// The agent's runtime mode. Session state, never persisted — every session
/// starts in [`Mode::Agent`], preserving the pre-mode behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Analyze, decompose, propose structure. Read-only capabilities.
    Plan,
    /// Implement, execute, verify. Full tool set (the default).
    #[default]
    Agent,
    /// Discuss, review, explain. Read-only capabilities.
    Chat,
}

impl Mode {
    /// Whether `tool` may be advertised and executed in this mode.
    ///
    /// Agent mode imposes no mode-level restriction beyond excluding the
    /// `plan` tool — the project's `allowed_tools` and the permission gates
    /// remain the only filters, so tools registered via `Agent::with_tools`
    /// keep their fail-closed prompting behavior. Plan and Chat mode restrict
    /// to read-only tools (plus `task`, which only mutates the in-session task
    /// list); Plan mode additionally gets `plan`, the only path through which
    /// plans may be drafted — Agent and Chat mode may not modify plans, and
    /// that is structural, not convention.
    ///
    /// The read-only recon sub-agents (`explore`, `find`) are allowed in every
    /// mode: their nested turns only read, so they help research and planning
    /// just as much as execution. The general `delegate` sub-agent stays
    /// Agent-only — it can edit and run commands.
    pub fn allows_tool(self, tool: &str) -> bool {
        match self {
            Mode::Agent => tool != "plan",
            Mode::Plan => matches!(
                tool,
                "read_lines" | "search" | "tree" | "task" | "plan" | "explore" | "find"
            ),
            Mode::Chat => matches!(
                tool,
                "read_lines" | "search" | "tree" | "task" | "explore" | "find"
            ),
        }
    }

    /// The next mode in the Shift+Tab cycle: Plan → Agent → Chat → Plan.
    pub fn next(self) -> Mode {
        match self {
            Mode::Plan => Mode::Agent,
            Mode::Agent => Mode::Chat,
            Mode::Chat => Mode::Plan,
        }
    }

    /// Uppercase label for UI display (input-box border, status line).
    pub fn label(self) -> &'static str {
        match self {
            Mode::Plan => "PLAN",
            Mode::Agent => "AGENT",
            Mode::Chat => "CHAT",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Mode::Plan => "plan",
            Mode::Agent => "agent",
            Mode::Chat => "chat",
        };
        f.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_mode_allows_everything_except_plan() {
        for tool in [
            "read_lines",
            "search",
            "edit",
            "bash",
            "git",
            "task",
            "delegate",
            "explore",
            "find",
            "custom",
        ] {
            assert!(Mode::Agent.allows_tool(tool), "agent should allow {tool}");
        }
        // Plans are modified only in Plan mode, enforced structurally.
        assert!(!Mode::Agent.allows_tool("plan"));
    }

    #[test]
    fn plan_and_chat_are_read_only() {
        for mode in [Mode::Plan, Mode::Chat] {
            for tool in ["read_lines", "search", "tree", "task"] {
                assert!(mode.allows_tool(tool), "{mode} should allow {tool}");
            }
            for tool in ["edit", "bash", "git", "delegate", "custom"] {
                assert!(!mode.allows_tool(tool), "{mode} should refuse {tool}");
            }
        }
        // Only Plan mode sees the plan tool.
        assert!(Mode::Plan.allows_tool("plan"));
        assert!(!Mode::Chat.allows_tool("plan"));
    }

    #[test]
    fn read_only_recon_sub_agents_are_allowed_in_every_mode() {
        // `explore`/`find` only read, so they help in Plan and Chat too; the
        // general `delegate` (which can write) stays Agent-only.
        for mode in [Mode::Agent, Mode::Plan, Mode::Chat] {
            assert!(mode.allows_tool("explore"), "{mode} should allow explore");
            assert!(mode.allows_tool("find"), "{mode} should allow find");
        }
        assert!(Mode::Agent.allows_tool("delegate"));
        assert!(!Mode::Plan.allows_tool("delegate"));
        assert!(!Mode::Chat.allows_tool("delegate"));
    }

    #[test]
    fn cycle_visits_all_modes_and_wraps() {
        assert_eq!(Mode::Plan.next(), Mode::Agent);
        assert_eq!(Mode::Agent.next(), Mode::Chat);
        assert_eq!(Mode::Chat.next(), Mode::Plan);
    }

    #[test]
    fn default_is_agent() {
        assert_eq!(Mode::default(), Mode::Agent);
    }
}
