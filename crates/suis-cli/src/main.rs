//! suis-cli — the terminal user interface for Suis.
//!
//! Owns application startup, rendering, the chat UI, permission prompts, diff
//! rendering, and slash commands. All business logic is delegated to
//! suis-agent; this crate only renders state and forwards input.

mod app;
mod clipboard;
mod commands;
mod prompts;
mod screens;
mod theme;
mod widgets;

/// Entry point. `--version` / `-V` prints the version and exits; otherwise the
/// full TUI starts.
#[tokio::main]
async fn main() -> std::io::Result<()> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("suis {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    app::run().await
}
