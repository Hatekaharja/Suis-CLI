//! Ordered message history for a single session.

use super::message::Message;

/// The full, ordered transcript of a conversation. MVP: no truncation or
/// compression — the entire history is sent on every turn.
#[derive(Debug, Clone, Default)]
pub struct ConversationHistory {
    messages: Vec<Message>,
}

impl ConversationHistory {
    /// An empty history.
    pub fn new() -> Self {
        ConversationHistory::default()
    }

    /// Append a message to the end of the transcript.
    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// The full transcript in order.
    pub fn as_slice(&self) -> &[Message] {
        &self.messages
    }

    /// The most recently appended message, if any.
    pub fn last(&self) -> Option<&Message> {
        self.messages.last()
    }

    /// Reset the transcript (e.g. on `/clear`).
    pub fn clear(&mut self) {
        self.messages.clear();
    }

    /// Number of messages.
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Whether the transcript is empty.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::Role;

    #[test]
    fn push_preserves_order() {
        let mut h = ConversationHistory::new();
        h.push(Message::text(Role::System, "sys"));
        h.push(Message::text(Role::User, "hi"));
        h.push(Message::text(Role::Assistant, "hello"));
        let roles: Vec<Role> = h.as_slice().iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::System, Role::User, Role::Assistant]);
    }

    #[test]
    fn clear_empties() {
        let mut h = ConversationHistory::new();
        h.push(Message::text(Role::User, "x"));
        assert!(!h.is_empty());
        h.clear();
        assert!(h.is_empty());
        assert_eq!(h.len(), 0);
    }

    #[test]
    fn last_returns_most_recent() {
        let mut h = ConversationHistory::new();
        h.push(Message::text(Role::User, "first"));
        h.push(Message::text(Role::User, "second"));
        assert_eq!(h.last().unwrap().content, "second");
    }
}
