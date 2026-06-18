//! Context assembly: turning session state into a [`ChatRequest`].

pub mod assembler;
pub mod budget;
pub mod profile;
pub mod system_prompt;
pub mod work_package;

pub use assembler::{Assembled, ContextAssembler};
pub use budget::{
    budget_for, estimate_tokens, resolve_context_budget, total_tokens, DEFAULT_CONTEXT_BUDGET,
};
pub use profile::detect as detect_profile;
pub use system_prompt::SYSTEM_PROMPT;
