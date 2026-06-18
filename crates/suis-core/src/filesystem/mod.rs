//! Filesystem safety layer: boundary-checked operations, hidden/hardened
//! guards, and a soft-delete trash system.

pub mod guard;
pub mod ops;
pub mod trash;

pub use trash::TrashEntry;
