//! Curated per-model context-window metadata.
//!
//! Provider `/v1/models` listings don't carry context lengths, so we map a
//! model id onto a known maximum context window by matching its id against a
//! table of model families. This is best-effort: a miss returns `None`, and the
//! agent falls back to its default budget. For Ollama, the model's own
//! `/api/show` metadata is more authoritative and is preferred when available.

/// Known context windows (in tokens) keyed by a lowercase substring of the
/// model id. Ordered most-specific-first, since the first containing match
/// wins (e.g. `gpt-4o` must precede `gpt-4`, `llama3.1` must precede `llama3`).
const TABLE: &[(&str, usize)] = &[
    // OpenAI
    ("gpt-4.1", 1_047_576),
    ("gpt-4o", 128_000),
    ("gpt-4-turbo", 128_000),
    ("gpt-4-32k", 32_768),
    ("gpt-4", 8_192),
    ("gpt-3.5-turbo-16k", 16_385),
    ("gpt-3.5", 16_385),
    ("o1", 200_000),
    ("o3", 200_000),
    ("o4", 200_000),
    // Anthropic Claude
    ("claude", 200_000),
    ("sonnet", 200_000),
    ("opus", 200_000),
    ("haiku", 200_000),
    // Meta Llama (version-tagged variants before the bare family)
    ("llama-3.1", 128_000),
    ("llama-3.2", 128_000),
    ("llama-3.3", 128_000),
    ("llama3.1", 128_000),
    ("llama3.2", 128_000),
    ("llama3.3", 128_000),
    ("llama-3", 8_192),
    ("llama3", 8_192),
    ("llama-2", 4_096),
    ("llama2", 4_096),
    // Alibaba Qwen
    ("qwen3", 32_768),
    ("qwen2.5", 32_768),
    ("qwen2", 32_768),
    ("qwen", 32_768),
    // Mistral
    ("mixtral", 32_768),
    ("mistral", 32_768),
    // Google Gemma
    ("gemma2", 8_192),
    ("gemma", 8_192),
    // Microsoft Phi
    ("phi-3", 4_096),
    ("phi3", 4_096),
    // DeepSeek
    ("deepseek-coder", 16_384),
    ("deepseek", 65_536),
];

/// Look up a model's maximum context window (in tokens) by matching `model_id`
/// against the curated table. Returns `None` when the model is unrecognized.
pub fn lookup_context_window(_provider_id: &str, model_id: &str) -> Option<usize> {
    let id = model_id.to_ascii_lowercase();
    TABLE
        .iter()
        .find(|(key, _)| id.contains(key))
        .map(|(_, window)| *window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_openai_families() {
        assert_eq!(
            lookup_context_window("openai", "gpt-4o-mini"),
            Some(128_000)
        );
        assert_eq!(lookup_context_window("openai", "gpt-4o"), Some(128_000));
        // gpt-4o is matched before the bare gpt-4 entry.
        assert_eq!(
            lookup_context_window("openai", "gpt-4-turbo"),
            Some(128_000)
        );
        assert_eq!(
            lookup_context_window("openai", "gpt-3.5-turbo"),
            Some(16_385)
        );
    }

    #[test]
    fn matches_claude_by_family_word() {
        assert_eq!(
            lookup_context_window("anthropic", "claude-3-5-sonnet-20241022"),
            Some(200_000)
        );
        assert_eq!(
            lookup_context_window("anthropic", "claude-opus-4"),
            Some(200_000)
        );
    }

    #[test]
    fn matches_local_families() {
        assert_eq!(
            lookup_context_window("ollama", "qwen3-coder:latest"),
            Some(32_768)
        );
        assert_eq!(
            lookup_context_window("ollama", "llama3.1:8b"),
            Some(128_000)
        );
        // Bare llama3 (no minor version) is the smaller original window.
        assert_eq!(lookup_context_window("ollama", "llama3:8b"), Some(8_192));
    }

    #[test]
    fn unknown_model_is_none() {
        assert_eq!(lookup_context_window("ollama", "some-exotic-model"), None);
    }
}
