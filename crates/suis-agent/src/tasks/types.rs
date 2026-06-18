//! Task data types.
//!
//! [`TaskStatus`] is defined in suis-core (shared with persistent plan tasks)
//! and re-exported here; [`Task`] is the session-scoped unit the `task` tool
//! and the UI work with.

use serde::{Deserialize, Serialize};

pub use suis_core::TaskStatus;

/// A single unit of work tracked for the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Session-unique identifier (e.g. `"t1"`; plan-backed tasks use `"w1"` /
    /// `"v1"` for work and verify tasks respectively).
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Current lifecycle state.
    pub status: TaskStatus,
}
