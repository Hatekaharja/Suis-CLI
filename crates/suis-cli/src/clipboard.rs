//! Copying text to the system clipboard.
//!
//! A thin wrapper over [`arboard`], used by the `/developer` `Ctrl+Y` shortcut
//! to put the conversation transcript on the clipboard. Kept tiny and isolated
//! so the one place that touches the OS clipboard is easy to find and the rest
//! of the app stays platform-agnostic.

/// Copy `text` to the system clipboard. Returns a human-readable error string
/// when no clipboard is reachable (e.g. a headless or SSH session with no
/// display), so the caller can surface it as a notice rather than crashing.
pub fn copy(text: &str) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    clipboard
        .set_text(text.to_owned())
        .map_err(|e| e.to_string())
}
