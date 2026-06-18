//! Model capability flags and their (de)serialization.
//!
//! Capability *detection* — actually probing a model to fill these in — lives
//! in [`crate::detection`]. The types here are the persisted result.

use serde::{Deserialize, Serialize};

/// What a model can do. Conservative by default: assume only chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Supports plain chat completion.
    pub chat: bool,
    /// Supports incremental/streamed responses.
    pub streaming: bool,
    /// Supports tool / function calling.
    pub tool_use: bool,
    /// Supports constrained/structured (e.g. JSON-schema) output.
    pub structured_output: bool,
}

impl Default for Capabilities {
    /// The conservative default: chat only, everything else off.
    fn default() -> Self {
        Capabilities {
            chat: true,
            streaming: false,
            tool_use: false,
            structured_output: false,
        }
    }
}

impl Capabilities {
    /// The optimistic default applied to freshly discovered models before
    /// real detection runs: local providers stream by default.
    pub fn discovery_default() -> Self {
        Capabilities {
            chat: true,
            streaming: true,
            tool_use: false,
            structured_output: false,
        }
    }

    /// Capabilities derived from Ollama's per-model `capabilities` array (as
    /// returned by `/api/tags`). The presence of `"tools"` means the model
    /// supports tool calling; local Ollama models chat and stream by default.
    /// These are *advertised* by the provider, so they need no runtime probe.
    pub fn from_ollama_tags(advertised: &[String]) -> Self {
        Capabilities {
            chat: true,
            streaming: true,
            tool_use: advertised.iter().any(|c| c == "tools"),
            structured_output: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_conservative() {
        let caps = Capabilities::default();
        assert!(caps.chat);
        assert!(!caps.streaming);
        assert!(!caps.tool_use);
        assert!(!caps.structured_output);
    }

    #[test]
    fn ollama_tags_with_tools_enable_tool_use() {
        let caps = Capabilities::from_ollama_tags(&["completion".into(), "tools".into()]);
        assert!(caps.tool_use);
        assert!(caps.chat);
        assert!(caps.streaming);
    }

    #[test]
    fn ollama_tags_without_tools_leave_tool_use_off() {
        let caps = Capabilities::from_ollama_tags(&["completion".into()]);
        assert!(!caps.tool_use);
        assert!(caps.chat);
    }

    #[test]
    fn round_trips_through_json() {
        let caps = Capabilities {
            chat: true,
            streaming: true,
            tool_use: true,
            structured_output: false,
        };
        let json = serde_json::to_string(&caps).unwrap();
        let back: Capabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(caps, back);
    }
}
