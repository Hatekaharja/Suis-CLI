//! Global configuration: settings, provider storage, and path resolution.

pub mod global;
pub mod paths;
pub mod providers;

pub use global::{GlobalConfig, Settings};
pub use providers::{ProviderConfig, ProviderEntry};
