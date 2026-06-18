//! In-chat slash commands.
//!
//! [`parser`] decides whether input is a command and which one; [`handlers`]
//! maps a parsed [`Command`](parser::Command) to a [`CommandEffect`] the app
//! applies, and builds the aligned, styled lines for `/help`, `/permissions`,
//! and `/plans`.

pub mod handlers;
pub mod parser;

pub use handlers::{handle, plans_lines, profile_lines, CommandEffect};
pub use parser::{complete, is_command, parse};
