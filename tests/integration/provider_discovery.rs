//! Integration: real provider discovery. Opt-in — this probes the network and
//! only runs when `SUIS_TEST_OLLAMA=1` is set with Ollama actually running.
//! Without the env var it returns early so the default `cargo test` stays
//! hermetic.

use suis_core::ProviderConfig;
use suis_providers::ProviderRegistry;

#[tokio::test]
async fn ollama_discovery_returns_at_least_one_model() {
    if std::env::var("SUIS_TEST_OLLAMA").as_deref() != Ok("1") {
        eprintln!("skipping provider_discovery: set SUIS_TEST_OLLAMA=1 (with Ollama running)");
        return;
    }

    // An empty config probes the built-in default endpoints.
    let registry = ProviderRegistry::discover_with(&ProviderConfig::default()).await;
    let results = registry.results();
    assert!(
        !results.is_empty(),
        "expected at least one running provider to be discovered"
    );

    let total_models: usize = results.iter().map(|r| r.models.len()).sum();
    assert!(
        total_models > 0,
        "expected the discovered provider(s) to report at least one model"
    );
}
