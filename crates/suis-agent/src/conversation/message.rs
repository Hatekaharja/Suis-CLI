//! Conversation message types.
//!
//! These are re-exported straight from `suis-providers`: a conversation
//! message and a wire message are the same thing, so there is no separate type
//! to convert through when building a request.

pub use suis_providers::{Message, Role};
