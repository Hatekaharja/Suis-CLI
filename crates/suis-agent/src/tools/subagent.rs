//! Typed sub-agents: profiles that drive a nested, isolated agent turn.
//!
//! A sub-agent runs in its own fresh, lean context and reports back only a
//! dense summary — the full sub-transcript never enters the parent's context,
//! the leverage for a small local-model window. The agent loop (`run_subagent`,
//! see [`runtime::agent`](crate::runtime::agent)) intercepts a call to any of
//! these tools and runs the nested turn itself; the [`Tool::execute`] bodies
//! here are unreachable, exactly like `plan` and the old `delegate`.
//!
//! Each type is a [`SubAgentProfile`]: the single source of truth for its
//! model-facing definition, the (restricted) [`Mode`] its sub-turn runs in, the
//! role framing it is seeded with, how its handoff note is shaped, and its
//! iteration ceiling. The two recon profiles (`explore`, `find`) run their
//! sub-turn in [`Mode::Chat`] — read-only, enforced at both the assembler and
//! the executor — so they cannot edit or run commands even if the model tries.
//! Re-entrant delegation is refused (a sub-agent never sees these tools),
//! bounding depth to 1.

use serde_json::{json, Value};

use crate::Mode;

use super::{Tool, ToolContext, ToolDefinition, ToolOutcome};

/// Everything the model and the agent loop need to run one sub-agent type.
pub struct SubAgentProfile {
    /// The tool name, matching the model-facing schema.
    pub name: &'static str,
    /// The model-facing tool description.
    pub description: &'static str,
    /// Builds the model-facing JSON-Schema parameters (a `Value` can't be
    /// `const`, so this is a builder).
    pub parameters: fn() -> Value,
    /// The mode the *sub-turn* runs in. Recon profiles use [`Mode::Chat`]
    /// (read-only); the general executor uses [`Mode::Agent`] (full toolset).
    pub sub_mode: Mode,
    /// Role framing pushed as a `System` message into the sub-agent's fresh
    /// history. Empty for the general executor (it works from the objective
    /// alone, preserving the old `delegate` behavior).
    pub seed_prompt: &'static str,
    /// The instruction that shapes the sub-agent's handoff note.
    pub summary_prompt: &'static str,
    /// Iteration ceiling for the nested turn, bounded separately from the parent.
    pub max_iterations: usize,
}

impl SubAgentProfile {
    /// The model-facing definition advertised for this sub-agent's tool.
    pub fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.to_string(),
            description: self.description.to_string(),
            parameters: (self.parameters)(),
        }
    }
}

/// The exploration sub-agent: read-only codebase orientation.
pub const EXPLORE: SubAgentProfile = SubAgentProfile {
    name: "explore",
    description: "Spawn a read-only exploration sub-agent that orients in the codebase and \
                  reports back where the work lives — the files, symbols, and entry points you \
                  should start from for a task. It works in its own clean context (sweeping the \
                  tree, searching, reading) and returns only a concise map, so your own context \
                  stays small. Use it at the start of a non-trivial task when you don't yet know \
                  the layout. It cannot edit or run commands. Provide a clear 'objective' \
                  describing what you're about to do.",
    parameters: explore_params,
    sub_mode: Mode::Chat,
    seed_prompt: "You are an exploration sub-agent working in a clean context. You may only read \
                  — never edit files or run commands. Investigate the codebase to answer where \
                  the work for the objective lives: sweep the tree, search for the relevant code, \
                  and read enough to be sure. Then report a concise map — the files, symbols, and \
                  entry points the main agent should start from, each with a one-line note — and \
                  flag anything that will trip it up. Do not implement anything.",
    summary_prompt: "You are writing a handoff note from an exploration sub-agent back to the \
                     main agent. List the files, symbols, and entry points relevant to the \
                     objective, each with a one-line note, and flag anything the main agent must \
                     watch out for. Be dense and factual — no preamble, no pleasantries. Write \
                     the note and nothing else.",
    max_iterations: 10,
};

/// The search sub-agent: read-only, focused locating of a pattern or usage.
pub const FIND: SubAgentProfile = SubAgentProfile {
    name: "find",
    description: "Spawn a read-only search sub-agent that locates a specific pattern, symbol, or \
                  usage across the repo and reports every hit with its location. Unlike the \
                  one-shot 'search' tool, it runs several search→read rounds in its own clean \
                  context to confirm and synthesize, returning only a list of `path:line` hits — \
                  so your own context stays small. Use it to answer \"where is X used / defined / \
                  referenced?\". It cannot edit or run commands. Provide an 'objective' naming \
                  exactly what to find.",
    parameters: find_params,
    sub_mode: Mode::Chat,
    seed_prompt: "You are a search sub-agent working in a clean context. You may only read — \
                  never edit files or run commands. Locate every place that matches the objective \
                  (a pattern, symbol, or usage) by searching and reading to confirm. Then report \
                  the concrete hits as `path:line`, each with a one-line note, grouped sensibly. \
                  Do not implement anything — only find and report.",
    summary_prompt: "You are writing a handoff note from a search sub-agent back to the main \
                     agent. List the concrete matches as `path:line`, each with a one-line note, \
                     plus a one-sentence summary of the overall pattern of results. Be dense and \
                     factual — no preamble, no pleasantries. Write the note and nothing else.",
    max_iterations: 8,
};

/// The general executor sub-agent: a self-contained chunk of work with the full
/// toolset. This is the original `delegate` behavior.
pub const DELEGATE: SubAgentProfile = SubAgentProfile {
    name: "delegate",
    description: "Delegate a self-contained subtask to a fresh sub-agent that works in its own \
                  clean context and reports back only a short summary. Use this for a chunk of \
                  work that can be described fully up front (e.g. \"add input validation to the \
                  signup form and its tests\"), so your own context stays focused. The sub-agent \
                  shares this project's tools and permissions but cannot itself delegate. Provide \
                  a clear, complete 'objective'; optionally add a 'context_hint' with files or \
                  facts it should start from.",
    parameters: delegate_params,
    sub_mode: Mode::Agent,
    seed_prompt: "",
    summary_prompt: "You are writing a handoff note from a sub-agent back to the main agent. In a \
                     few sentences, state what the sub-task accomplished, the key files and \
                     symbols touched, and anything the main agent must know to continue. Be dense \
                     and factual — no preamble, no pleasantries. Write the note and nothing else.",
    max_iterations: 12,
};

/// Every sub-agent profile, in registration order.
const PROFILES: &[&SubAgentProfile] = &[&EXPLORE, &FIND, &DELEGATE];

/// The profile for a tool `name`, if it names a sub-agent.
pub fn profile(name: &str) -> Option<&'static SubAgentProfile> {
    PROFILES.iter().copied().find(|p| p.name == name)
}

/// Whether `name` is a sub-agent tool. Used by the loop's re-entrance filter to
/// strip every sub-agent tool from a sub-turn (so depth stays 1).
pub fn is_subagent(name: &str) -> bool {
    PROFILES.iter().any(|p| p.name == name)
}

/// The model-facing arguments shared by the recon profiles and the executor:
/// a required `objective` plus an optional `context_hint`. `objective_desc`
/// tailors the objective's wording per profile.
fn recon_params(objective_desc: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "objective": {
                "type": "string",
                "description": objective_desc,
            },
            "context_hint": {
                "type": "string",
                "description": "Optional pointers — files, symbols, or facts the sub-agent \
                                should start from."
            }
        },
        "required": ["objective"]
    })
}

fn explore_params() -> Value {
    recon_params(
        "What you are about to work on, described well enough for the sub-agent to know what \
         parts of the codebase to map out.",
    )
}

fn find_params() -> Value {
    recon_params(
        "Exactly what to find — a pattern, symbol, or usage — described well enough to locate \
         every relevant hit.",
    )
}

fn delegate_params() -> Value {
    recon_params(
        "The subtask to accomplish, described completely enough to work from without further \
         questions.",
    )
}

/// The exploration sub-agent tool. Its body is unreachable — the agent loop
/// resolves the call by running a nested sub-turn.
pub struct ExploreTool;

impl Tool for ExploreTool {
    fn name(&self) -> &'static str {
        EXPLORE.name
    }

    fn definition(&self) -> ToolDefinition {
        EXPLORE.definition()
    }

    fn execute(&self, _args: &Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        Err(handled_by_loop(EXPLORE.name))
    }
}

/// The search sub-agent tool. Its body is unreachable (see [`ExploreTool`]).
pub struct FindTool;

impl Tool for FindTool {
    fn name(&self) -> &'static str {
        FIND.name
    }

    fn definition(&self) -> ToolDefinition {
        FIND.definition()
    }

    fn execute(&self, _args: &Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        Err(handled_by_loop(FIND.name))
    }
}

/// The general executor sub-agent tool. Its body is unreachable (see
/// [`ExploreTool`]).
pub struct DelegateTool;

impl Tool for DelegateTool {
    fn name(&self) -> &'static str {
        DELEGATE.name
    }

    fn definition(&self) -> ToolDefinition {
        DELEGATE.definition()
    }

    fn execute(&self, _args: &Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        Err(handled_by_loop(DELEGATE.name))
    }
}

/// The (unreachable) body error: the agent loop resolves these tools itself.
fn handled_by_loop(name: &str) -> String {
    format!("the {name} tool is handled by the agent loop")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_lookup_covers_every_registered_tool() {
        for name in ["explore", "find", "delegate"] {
            assert_eq!(profile(name).expect("registered").name, name);
            assert!(is_subagent(name));
        }
        assert!(profile("read_lines").is_none());
        assert!(!is_subagent("read_lines"));
    }

    #[test]
    fn recon_profiles_are_read_only() {
        assert_eq!(EXPLORE.sub_mode, Mode::Chat);
        assert_eq!(FIND.sub_mode, Mode::Chat);
        // The general executor keeps the full toolset.
        assert_eq!(DELEGATE.sub_mode, Mode::Agent);
    }

    #[test]
    fn definitions_require_an_objective() {
        for p in [&EXPLORE, &FIND, &DELEGATE] {
            let def = p.definition();
            assert_eq!(def.name, p.name);
            let required = def.parameters["required"].as_array().expect("required");
            assert!(required.iter().any(|v| v == "objective"));
        }
    }
}
