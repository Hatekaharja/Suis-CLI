//! suis-providers — provider discovery and model communication for Suis.
//!
//! Detects running local AI providers, enumerates their models, detects
//! capabilities, and communicates via transports. Depends only on suis-core.
//!
//! Layout:
//! - [`provider`] / [`model`] / [`capability`] — domain types
//! - [`discovery`] — probing endpoints for running providers
//! - [`registry`] — merging live discovery with stored config
//! - [`detection`] — probing models for real capabilities (cached)
//! - [`transport`] — sending chat requests over Ollama / OpenAI / Anthropic
//!   protocols

pub mod capability;
pub mod detection;
pub mod discovery;
pub mod model;
pub mod model_meta;
pub mod presets;
pub mod provider;
pub mod registry;
pub mod transport;

#[cfg(test)]
mod test_util;

pub use capability::Capabilities;
pub use detection::{CapabilityDetector, ModelCapsRequest};
pub use discovery::{probe_v1_models, DiscoveryResult, OllamaDiscovery};
pub use model::Model;
pub use model_meta::lookup_context_window;
pub use presets::{ProviderPreset, PRESETS};
pub use provider::{Provider, ProviderIssue, TransportType, UnknownTransport};
pub use registry::{
    endpoint_problem, ProbeOutcome, ProviderRegistry, ProviderStatus, DEFAULT_PROVIDER_IDS,
};
pub use transport::anthropic::AnthropicTransport;
pub use transport::ollama::OllamaTransport;
pub use transport::openai::OpenAiTransport;
pub use transport::tool_text::{parse_text_tool_calls, TextToolCalls};
pub use transport::types::{
    ChatRequest, ChatResponse, Message, Role, ToolCall, ToolDefinition, Usage,
};
pub use transport::{
    output_reserve, ChatStream, Transport, MAX_OUTPUT_RESERVE, MIN_OUTPUT_RESERVE,
};
