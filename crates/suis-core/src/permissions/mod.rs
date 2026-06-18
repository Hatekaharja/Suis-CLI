//! Permission storage and evaluation.
//!
//! Permissions are stored per-project in `.suis/permissions.json` and evaluated
//! into [`PermissionResult`]s. Dangerous commands can never be silently allowed
//! by a stored grant — see [`evaluator`].

pub mod evaluator;
pub mod store;
pub mod types;

pub use evaluator::{is_dangerous, DANGEROUS_COMMANDS};
pub use store::PermissionStore;
pub use types::{CommandPermission, PermissionResult, PermissionScope, ToolPermission};
