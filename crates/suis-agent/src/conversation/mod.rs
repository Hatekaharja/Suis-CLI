//! Conversation state for a session: the ordered message history.
//!
//! [`Message`] and [`Role`] come from `suis-providers` unchanged; only
//! [`ConversationHistory`] is added here.

pub mod history;
pub mod message;

pub use history::ConversationHistory;
pub use message::{Message, Role};
