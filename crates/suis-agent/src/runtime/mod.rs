//! The agent runtime: the conversation loop and the event stream it drives.

pub mod agent;
pub mod events;
pub mod mode;
pub mod session;

pub use agent::{Agent, Phase, TurnOutcome};
pub use events::{AgentEvent, PermissionDecision, PlanDecision};
pub use mode::Mode;
pub use session::{ImplementTarget, LedgerEntry, Session};
