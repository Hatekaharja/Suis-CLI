//! Formatting helper for command permission prompts.
//!
//! The agent emits a permission request as a single `action` string; this
//! module extracts the command for clearer display. The actual dialog and key
//! handling live in [`crate::widgets::permission_prompt`].

const COMMAND_PREFIX: &str = "run command: ";

/// If `action` describes a shell command, return just the command text.
pub fn command_of(action: &str) -> Option<&str> {
    action.strip_prefix(COMMAND_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_command() {
        assert_eq!(command_of("run command: cargo test"), Some("cargo test"));
        assert_eq!(command_of("modify hardened file: x"), None);
    }
}
