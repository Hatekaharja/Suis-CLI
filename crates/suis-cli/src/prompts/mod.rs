//! Helpers that interpret the agent's permission-request `action` strings into
//! their specific kinds (shell command, workspace boundary, hardened file) for
//! clearer prompts. The interactive dialog itself is
//! [`crate::widgets::permission_prompt`].

pub mod command_prompt;
pub mod file_prompt;
pub mod tool_summary;
