//! Formatting helpers for file-access permission prompts.
//!
//! Distinguishes the two file-related prompts the executor emits — a
//! workspace-boundary access and a hardened-file modification — so the dialog
//! can describe them precisely. Dialog rendering lives in
//! [`crate::widgets::permission_prompt`].

const HARDENED_PREFIX: &str = "modify hardened file: ";

/// If `action` is a hardened-file modification prompt, return the file path;
/// otherwise `None` (the action is a command or boundary prompt).
pub fn hardened_path(action: &str) -> Option<&str> {
    action.strip_prefix(HARDENED_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_hardened_path() {
        assert_eq!(
            hardened_path("modify hardened file: Cargo.lock"),
            Some("Cargo.lock")
        );
        assert_eq!(hardened_path("run command: ls"), None);
    }
}
