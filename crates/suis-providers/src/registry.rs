//! Unified view of providers: live discovery merged with stored configuration.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use suis_core::{Error, ProviderConfig, ProviderEntry, ProviderError};

use crate::detection::{CapabilityDetector, ModelCapsRequest};
use crate::discovery::openai_compat::probe_v1_models;
use crate::discovery::{anthropic, llamacpp, lmstudio, ollama, DiscoveryResult, OllamaDiscovery};
use crate::model::Model;
use crate::provider::{Provider, ProviderIssue, TransportType};
use crate::transport::Transport;

/// How long a discovery probe waits to establish a TCP connection before
/// treating the endpoint as absent.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Upper bound on a single discovery probe (connect + response). A port that
/// accepts but never answers is abandoned here instead of stalling startup
/// until reqwest's default fires.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The outcome of probing a single target. The failure cases are kept distinct
/// so a manual connection test (19.2) can tell the user which knob to turn:
/// auth (wrong/missing key), wrong language (connected but the models call did
/// not parse), offline (the port refused the connection), or a connection issue
/// (the host accepted but never answered, or returned a server error). The
/// model picker and `/providers` surface this distinction as a coloured status
/// dot; capability resolution still treats every non-`Online` case as absent.
#[derive(Debug, Clone)]
pub enum ProbeOutcome {
    /// The provider answered; carries its discovery result.
    Online(DiscoveryResult),
    /// The provider rejected the credentials (401/403).
    AuthFailed { id: String, name: String },
    /// The provider connected and responded, but the body could not be parsed
    /// as the expected protocol — usually the wrong transport for the endpoint.
    Unparsable { id: String },
    /// The port refused the connection (nothing is listening / not running).
    Offline { id: String },
    /// The host accepted (or never refused) the connection but did not answer in
    /// time, or returned a server error — a sleeping LAN box, a wedged endpoint,
    /// or a 5xx. Distinct from [`Offline`](ProbeOutcome::Offline) so the UI can
    /// flag it red ("connection issue") rather than a plain hollow "offline".
    ConnectionIssue { id: String },
}

impl ProbeOutcome {
    /// The probed provider's id, common to every outcome.
    pub fn id(&self) -> &str {
        match self {
            ProbeOutcome::Online(result) => &result.provider.id,
            ProbeOutcome::AuthFailed { id, .. }
            | ProbeOutcome::Unparsable { id }
            | ProbeOutcome::Offline { id }
            | ProbeOutcome::ConnectionIssue { id } => id,
        }
    }

    /// The display status this outcome resolves to. An unparsable body is a
    /// connection issue (the host answered, just in the wrong language).
    pub fn status(&self) -> ProviderStatus {
        match self {
            ProbeOutcome::Online(_) => ProviderStatus::Online,
            ProbeOutcome::AuthFailed { .. } => ProviderStatus::AuthFailed,
            ProbeOutcome::Offline { .. } => ProviderStatus::Offline,
            ProbeOutcome::Unparsable { .. } | ProbeOutcome::ConnectionIssue { .. } => {
                ProviderStatus::ConnectionIssue
            }
        }
    }
}

/// A provider's reachability as shown in the model picker and `/providers`. The
/// initial [`Checking`](ProviderStatus::Checking) state is UI-only — it is what
/// a row reads while its background probe is still in flight, before any
/// [`ProbeOutcome`] resolves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderStatus {
    /// Probe still in flight (the non-blocking discovery window).
    Checking,
    /// The provider answered with its model list.
    Online,
    /// The port refused the connection — nothing listening.
    Offline,
    /// Reachable but timed out or returned a server error / unparsable body.
    ConnectionIssue,
    /// The provider answered but rejected the credentials (401/403).
    AuthFailed,
}

/// Build the shared, timeout-configured HTTP client every discovery probe uses,
/// so a port that accepts but never answers is abandoned at [`REQUEST_TIMEOUT`]
/// (and an unresolved host at [`CONNECT_TIMEOUT`]) instead of stalling on
/// reqwest's defaults. Connection pooling is reused across the run's probes.
fn discovery_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .unwrap_or_default()
}

/// The ids of the three built-in local defaults. Removing one of these in
/// `/providers` stores it disabled (so discovery suppresses it) rather than
/// dropping it, so it can be resurrected from a preset.
pub const DEFAULT_PROVIDER_IDS: &[&str] = &["ollama", "lmstudio", "llamacpp"];

/// The set of providers Suis currently knows about, after discovery.
#[derive(Debug, Clone, Default)]
pub struct ProviderRegistry {
    results: Vec<DiscoveryResult>,
    /// Configuration problems found while loading (unknown transport, bad
    /// endpoint, duplicate id) — surfaced in `/providers`, never silently
    /// coerced.
    issues: Vec<ProviderIssue>,
    /// Ids of providers that responded but rejected the credentials.
    auth_failed: Vec<String>,
}

impl ProviderRegistry {
    /// Build a registry directly from discovery results (no issues, no auth
    /// failures).
    pub fn from_results(results: Vec<DiscoveryResult>) -> Self {
        ProviderRegistry {
            results,
            issues: Vec::new(),
            auth_failed: Vec::new(),
        }
    }

    /// Discover providers honoring stored configuration: probe the built-in
    /// default endpoints **plus** any endpoints from `config`, with stored
    /// entries overriding a default's endpoint/transport (so a provider moved to
    /// a custom port is found there) and disabled entries dropped entirely (so
    /// they never reach model selection). Offline endpoints are simply absent.
    ///
    /// Every probe shares one timeout-configured client, so a port that accepts
    /// but never responds cannot stall startup past [`REQUEST_TIMEOUT`], and
    /// connection pooling is reused across the (three or more) probes.
    pub async fn discover_with(config: &ProviderConfig) -> Self {
        let client = discovery_client();
        Self::discover_with_client(config, &client).await
    }

    /// The providers discovery *will* probe for `config` (built-in defaults ∪
    /// enabled config entries, validated, disabled removed), as display-ready
    /// [`Provider`]s, plus the load issues. The UI builds its initial
    /// "checking…" skeleton from this so every provider that will be probed
    /// shows immediately — before any probe lands — and
    /// [`discover_streaming`](Self::discover_streaming) then resolves each row.
    pub fn plan(config: &ProviderConfig) -> (Vec<Provider>, Vec<ProviderIssue>) {
        let (targets, issues) = plan_probes(config);
        let providers = targets
            .into_iter()
            .map(ProbeTarget::into_provider)
            .collect();
        (providers, issues)
    }

    /// Probe every planned target for `config`, invoking `on_outcome` with each
    /// [`ProbeOutcome`] the moment it lands — so a responsive provider resolves
    /// in milliseconds while a slow or silently-unreachable one (a sleeping LAN
    /// host) resolves later, never holding up the others or the UI. Returns once
    /// every probe has reported.
    ///
    /// This is the non-blocking counterpart to
    /// [`discover_with`](Self::discover_with): the caller renders the picker
    /// from [`plan`](Self::plan) right away and folds these outcomes in as they
    /// arrive, instead of awaiting the whole batch behind its slowest endpoint.
    pub async fn discover_streaming<F: FnMut(ProbeOutcome)>(
        config: &ProviderConfig,
        mut on_outcome: F,
    ) {
        use futures::stream::{FuturesUnordered, StreamExt};
        let client = discovery_client();
        let (targets, _issues) = plan_probes(config);
        let mut probes: FuturesUnordered<_> = targets
            .into_iter()
            .map(|target| probe_target(&client, target))
            .collect();
        while let Some(outcome) = probes.next().await {
            on_outcome(outcome);
        }
    }

    /// The body of [`discover_with`](Self::discover_with) against an explicit
    /// client, so tests can inject a short-timeout client.
    async fn discover_with_client(config: &ProviderConfig, client: &reqwest::Client) -> Self {
        let (targets, issues) = plan_probes(config);
        let probes = targets
            .into_iter()
            .map(|target| probe_target(client, target));
        let outcomes = futures::future::join_all(probes).await;

        let mut results = Vec::new();
        let mut auth_failed = Vec::new();
        for outcome in outcomes {
            match outcome {
                ProbeOutcome::Online(result) => results.push(result),
                ProbeOutcome::AuthFailed { id, .. } => auth_failed.push(id),
                ProbeOutcome::Unparsable { .. }
                | ProbeOutcome::Offline { .. }
                | ProbeOutcome::ConnectionIssue { .. } => {}
            }
        }
        ProviderRegistry {
            results,
            issues,
            auth_failed,
        }
    }

    /// All discovery results (one per running provider).
    pub fn results(&self) -> &[DiscoveryResult] {
        &self.results
    }

    /// Configuration problems found while loading (for display in `/providers`).
    pub fn issues(&self) -> &[ProviderIssue] {
        &self.issues
    }

    /// Ids of providers that responded but rejected the credentials, so the UI
    /// can distinguish an auth failure from an offline provider.
    pub fn auth_failed(&self) -> &[String] {
        &self.auth_failed
    }

    /// All discovered providers.
    pub fn providers(&self) -> Vec<&Provider> {
        self.results.iter().map(|r| &r.provider).collect()
    }

    /// Every model across every discovered provider.
    pub fn models(&self) -> Vec<&Model> {
        self.results.iter().flat_map(|r| r.models.iter()).collect()
    }

    /// Apply previously-verified capabilities from the on-disk cache to every
    /// still-unverified discovered model, marking those that hit as verified.
    ///
    /// This is a purely local read — no network probe, no status line — so a
    /// model the user verified in an earlier session keeps its resolved
    /// capabilities across launches (and skips the "Verify capabilities?"
    /// prompt) without re-probing. Models with no fresh cache entry are left
    /// unverified, carrying their discovery default until verified on selection.
    pub fn apply_cached_capabilities(mut self, detector: &CapabilityDetector) -> Self {
        for result in &mut self.results {
            let cached = detector.fresh_cached_models(&result.provider.id);
            if cached.is_empty() {
                continue;
            }
            for model in &mut result.models {
                if model.verified {
                    continue;
                }
                if let Some(caps) = cached.get(&model.model_id) {
                    model.capabilities = *caps;
                    model.verified = true;
                }
            }
        }
        self
    }

    /// Whether resolving capabilities would require at least one runtime probe:
    /// some discovered model is unverified and lacks a fresh cache entry. Keyed
    /// (remote) providers are excluded — their models are never auto-probed
    /// (20.1) — so startup's "Detecting…" line stays honest and counts only the
    /// local probes that will actually run.
    pub fn needs_capability_probe(&self, detector: &CapabilityDetector) -> bool {
        self.results
            .iter()
            .filter(|r| r.provider.api_key.is_none())
            .flat_map(|r| r.models.iter())
            .any(|m| !m.verified && !detector.is_fresh_cached(&m.provider_id, &m.model_id))
    }

    /// Resolve every discovered model's real capabilities, returning a registry
    /// whose models carry verified capabilities (above all `tool_use`, which
    /// gates the agent's tools). Advertised caps are trusted; otherwise a fresh
    /// cache entry is reused, else the model is probed over a transport built by
    /// `transport_for`. Never fails: an offline probe falls back conservatively.
    ///
    /// Keyed (remote) providers are consent-gated (20.1): their unverified,
    /// uncached models are **not** probed here — an unverified remote model
    /// against a metered endpoint would be a surprise paid call. Those models
    /// stay unverified (carrying their discovery default) until the user
    /// confirms a probe at selection time; their advertised or cached caps are
    /// still honored without a probe.
    pub async fn resolve_capabilities<F>(
        mut self,
        detector: &CapabilityDetector,
        transport_for: F,
    ) -> Self
    where
        F: Fn(&Provider) -> Box<dyn Transport>,
    {
        for result in &mut self.results {
            let transport = transport_for(&result.provider);
            let keyed = result.provider.api_key.is_some();
            // Borrow models immutably to build requests, then release before the
            // mutable write-back below. For a keyed provider, only models that
            // can resolve without a probe (advertised, or already fresh in
            // cache) are submitted; the rest are left untouched (unverified).
            let (indices, resolved) = {
                let mut indices: Vec<usize> = Vec::new();
                let mut requests: Vec<ModelCapsRequest> = Vec::new();
                for (idx, m) in result.models.iter().enumerate() {
                    if keyed
                        && !m.verified
                        && !detector.is_fresh_cached(&result.provider.id, &m.model_id)
                    {
                        // Consent-gated: never auto-probe a remote model.
                        continue;
                    }
                    indices.push(idx);
                    requests.push(ModelCapsRequest {
                        model_id: &m.model_id,
                        advertised: m.verified.then_some(m.capabilities),
                    });
                }
                let resolved = detector
                    .resolve_models(&result.provider.id, &requests, transport.as_ref())
                    .await;
                (indices, resolved)
            };
            for (idx, caps) in indices.into_iter().zip(resolved) {
                result.models[idx].capabilities = caps;
                result.models[idx].verified = true;
            }
        }
        self
    }

    /// Build an (offline) registry view from stored configuration alone.
    pub fn load_from_config(config: &ProviderConfig) -> Vec<Provider> {
        config.providers.iter().map(Provider::from_entry).collect()
    }

    /// Merge live discovery with stored entries into a unified provider list:
    ///
    /// - stored provider also discovered → endpoint refreshed, stays enabled
    /// - stored provider not discovered → preserved but `enabled: false`
    /// - discovered provider not stored → added as a new enabled entry
    pub fn merge(discovered: &[DiscoveryResult], stored: &[ProviderEntry]) -> Vec<Provider> {
        let mut merged: Vec<Provider> = Vec::new();

        for entry in stored {
            let mut provider = Provider::from_entry(entry);
            if let Some(live) = discovered.iter().find(|d| d.provider.id == entry.id) {
                provider.endpoint = live.provider.endpoint.clone();
                provider.enabled = true;
            } else {
                provider.enabled = false;
            }
            merged.push(provider);
        }

        for result in discovered {
            if !stored.iter().any(|e| e.id == result.provider.id) {
                merged.push(result.provider.clone());
            }
        }

        merged
    }

    /// Probe a single provider entry through its chosen transport and report the
    /// outcome — the manual "test connection" path (19.2). This reuses the exact
    /// per-target machinery startup discovery uses ([`probe_target`]), with the
    /// same connect/request timeouts, so a dead endpoint resolves in bounded
    /// time and a healthy one reports its model list. Nothing is persisted.
    ///
    /// An entry whose transport string is unknown cannot be probed; that is a
    /// validation error the form catches before testing, but as a defensive
    /// fallback it reports [`ProbeOutcome::Offline`].
    pub async fn probe_one(entry: &ProviderEntry) -> ProbeOutcome {
        let client = discovery_client();
        Self::probe_one_with(entry, &client).await
    }

    /// The body of [`probe_one`](Self::probe_one) against an explicit client, so
    /// tests can inject a short-timeout client.
    async fn probe_one_with(entry: &ProviderEntry, client: &reqwest::Client) -> ProbeOutcome {
        let Ok(transport) = TransportType::parse(&entry.transport) else {
            return ProbeOutcome::Offline {
                id: entry.id.clone(),
            };
        };
        let target = ProbeTarget {
            id: entry.id.clone(),
            name: entry
                .name
                .clone()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| entry.id.clone()),
            endpoint: entry.endpoint.clone(),
            transport,
            api_key: entry.resolve_api_key(),
            api_key_env: entry.api_key_env.clone(),
        };
        probe_target(client, target).await
    }
}

/// A single endpoint to probe, identified by its provider id, carrying its
/// resolved API key (if any) so the probe can authenticate.
struct ProbeTarget {
    id: String,
    name: String,
    endpoint: String,
    transport: TransportType,
    /// Resolved API key (env-first), threaded onto the discovered provider so
    /// later chat requests authenticate.
    api_key: Option<String>,
    /// Configured key env-var name, preserved on the discovered provider for
    /// round-trip and the "key env not set" flag.
    api_key_env: Option<String>,
}

impl ProbeTarget {
    /// This target as a display-ready [`Provider`] for the picker skeleton,
    /// before any probe has run. Marked `enabled` (a planned target is, by
    /// definition, an enabled provider) and carrying its auth fields so a later
    /// selection can authenticate without re-reading config.
    fn into_provider(self) -> Provider {
        Provider {
            id: self.id,
            name: self.name,
            endpoint: self.endpoint,
            transport: self.transport,
            enabled: true,
            api_key: self.api_key,
            api_key_env: self.api_key_env,
        }
    }
}

/// Map a probe failure to its [`ProbeOutcome`], keeping the distinctions the UI
/// renders: rejected credentials, an unparsable body (wrong protocol), a refused
/// port (offline), or a timeout / server error (connection issue).
fn failure_outcome(err: &Error, id: String, name: String) -> ProbeOutcome {
    match err {
        Error::Provider(ProviderError::AuthFailed { .. }) => ProbeOutcome::AuthFailed { id, name },
        Error::Provider(ProviderError::ParseError(_)) => ProbeOutcome::Unparsable { id },
        Error::Provider(ProviderError::NotRunning(_)) => ProbeOutcome::Offline { id },
        // Timeout, RequestError (5xx), and anything else: reachable but wrong.
        _ => ProbeOutcome::ConnectionIssue { id },
    }
}

/// Validate `config` and build the endpoints to probe for
/// [`ProviderRegistry::discover_with`]: the built-in defaults, with each valid
/// stored entry overriding the same-id default's endpoint/transport (or adding
/// a new target), disabled entries removed, and every invalid entry carried out
/// as a [`ProviderIssue`] instead of being coerced or dropping the rest of the
/// file.
///
/// An enabled entry is invalid when its transport is unknown, its endpoint is
/// empty or does not parse as a URL with a host, or its id duplicates an
/// earlier enabled entry (first wins). A fully valid config yields zero issues
/// and the same probe targets as before validation existed.
fn plan_probes(config: &ProviderConfig) -> (Vec<ProbeTarget>, Vec<ProviderIssue>) {
    let mut targets: BTreeMap<String, ProbeTarget> = BTreeMap::new();
    targets.insert(
        "ollama".into(),
        ProbeTarget {
            id: "ollama".into(),
            name: "Ollama".into(),
            endpoint: ollama::DEFAULT_ENDPOINT.into(),
            transport: TransportType::Ollama,
            api_key: None,
            api_key_env: None,
        },
    );
    targets.insert(
        "lmstudio".into(),
        ProbeTarget {
            id: "lmstudio".into(),
            name: "LM Studio".into(),
            endpoint: lmstudio::DEFAULT_ENDPOINT.into(),
            transport: TransportType::OpenAiCompatible,
            api_key: None,
            api_key_env: None,
        },
    );
    targets.insert(
        "llamacpp".into(),
        ProbeTarget {
            id: "llamacpp".into(),
            name: "llama.cpp".into(),
            endpoint: llamacpp::DEFAULT_ENDPOINT.into(),
            transport: TransportType::OpenAiCompatible,
            api_key: None,
            api_key_env: None,
        },
    );

    let mut issues: Vec<ProviderIssue> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for entry in &config.providers {
        if !entry.enabled {
            // A user-disabled entry is simply not probed; it is not validated.
            targets.remove(&entry.id);
            continue;
        }

        if !seen.insert(entry.id.clone()) {
            issues.push(ProviderIssue {
                id: entry.id.clone(),
                field: "id".into(),
                reason: format!("duplicate id {:?} (first wins)", entry.id),
            });
            continue;
        }

        let transport = match TransportType::parse(&entry.transport) {
            Ok(t) => t,
            Err(unknown) => {
                issues.push(ProviderIssue {
                    id: entry.id.clone(),
                    field: "transport".into(),
                    reason: unknown.to_string(),
                });
                continue;
            }
        };

        if let Some(reason) = endpoint_problem(&entry.endpoint) {
            issues.push(ProviderIssue {
                id: entry.id.clone(),
                field: "endpoint".into(),
                reason,
            });
            continue;
        }

        let api_key = entry.resolve_api_key();
        let api_key_env = entry.api_key_env.clone();
        let name = entry.name.clone().filter(|n| !n.is_empty());
        targets
            .entry(entry.id.clone())
            .and_modify(|t| {
                t.endpoint = entry.endpoint.clone();
                t.transport = transport;
                t.api_key = api_key.clone();
                t.api_key_env = api_key_env.clone();
                if let Some(name) = &name {
                    t.name = name.clone();
                }
            })
            .or_insert_with(|| ProbeTarget {
                id: entry.id.clone(),
                name: name.unwrap_or_else(|| entry.id.clone()),
                endpoint: entry.endpoint.clone(),
                transport,
                api_key,
                api_key_env,
            });
    }

    (targets.into_values().collect(), issues)
}

/// Why an endpoint string is unusable, or `None` if it is a valid URL with a
/// host. Kept deliberately lightweight (no scheme allow-listing) so any
/// real endpoint passes while obvious garbage is reported. Public so the
/// in-app provider form validates with the loader's own rule (18.3) and cannot
/// save what the loader would later flag.
pub fn endpoint_problem(endpoint: &str) -> Option<String> {
    if endpoint.trim().is_empty() {
        return Some("endpoint is empty".into());
    }
    match reqwest::Url::parse(endpoint) {
        Ok(url) if url.has_host() => None,
        Ok(_) => Some(format!("endpoint {endpoint:?} has no host")),
        Err(e) => Some(format!("endpoint {endpoint:?} is not a valid URL: {e}")),
    }
}

/// Probe a single target with the discovery matching its transport. The probed
/// provider's identity is attributed to the target's id/name so a stored entry
/// at a custom endpoint — and distinct OpenAI-compatible providers like LM
/// Studio and llama.cpp — stay under the right provider. A 401/403 from an
/// authed probe becomes [`ProbeOutcome::AuthFailed`], distinct from offline.
async fn probe_target(client: &reqwest::Client, target: ProbeTarget) -> ProbeOutcome {
    match target.transport {
        TransportType::Ollama => {
            // Ollama's discovery hardcodes its own identity; normalize it to the
            // target so a custom-endpoint entry stays under the right provider.
            // The Ollama protocol has no auth, so the key is ignored.
            match OllamaDiscovery::new().probe(client, &target.endpoint).await {
                Ok(mut result) => {
                    result.provider.id = target.id.clone();
                    result.provider.name = target.name;
                    for model in &mut result.models {
                        model.provider_id = target.id.clone();
                    }
                    ProbeOutcome::Online(result)
                }
                Err(err) => failure_outcome(&err, target.id, target.name),
            }
        }
        TransportType::OpenAiCompatible => {
            // The shared helper attributes the target's id/name directly.
            match probe_v1_models(
                client,
                &target.endpoint,
                &target.id,
                &target.name,
                target.api_key.as_deref(),
            )
            .await
            {
                Ok(mut result) => {
                    // Carry the resolved key onto the discovered provider so a
                    // later chat over this provider authenticates.
                    result.provider.api_key = target.api_key;
                    result.provider.api_key_env = target.api_key_env;
                    ProbeOutcome::Online(result)
                }
                Err(err) => failure_outcome(&err, target.id, target.name),
            }
        }
        TransportType::Anthropic => {
            // Anthropic discovery mirrors the OpenAI-compatible path: same id/
            // name attribution, same auth/parse/offline classification — only
            // the wire headers and model-caps shortcut differ.
            match anthropic::probe_models(
                client,
                &target.endpoint,
                &target.id,
                &target.name,
                target.api_key.as_deref(),
            )
            .await
            {
                Ok(mut result) => {
                    result.provider.api_key = target.api_key;
                    result.provider.api_key_env = target.api_key_env;
                    ProbeOutcome::Online(result)
                }
                Err(err) => failure_outcome(&err, target.id, target.name),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Capabilities;
    use crate::provider::TransportType;
    use crate::test_util::{MockServer, TempCacheDir};
    use crate::transport::types::{ChatRequest, ChatResponse, ToolCall};
    use crate::transport::ChatStream;
    use async_trait::async_trait;

    /// A transport that reports tool use (or fails) for capability-probe tests.
    struct ProbeTransport {
        tool_use: bool,
        offline: bool,
    }

    #[async_trait]
    impl Transport for ProbeTransport {
        async fn chat(&self, _request: ChatRequest) -> suis_core::Result<ChatResponse> {
            if self.offline {
                return Err(suis_core::ProviderError::NotRunning("offline".into()).into());
            }
            let tool_calls = if self.tool_use {
                vec![ToolCall {
                    id: "c0".into(),
                    name: "get_weather".into(),
                    arguments: serde_json::json!({"city": "Paris"}),
                }]
            } else {
                Vec::new()
            };
            Ok(ChatResponse {
                content: "ok".into(),
                reasoning: String::new(),
                tool_calls,
                done: true,
                usage: None,
            })
        }

        async fn chat_stream(&self, _request: ChatRequest) -> suis_core::Result<ChatStream> {
            if self.offline {
                return Err(suis_core::ProviderError::NotRunning("offline".into()).into());
            }
            Ok(Box::pin(futures::stream::iter(vec![Ok(ChatResponse {
                content: "hi".into(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                done: true,
                usage: None,
            })])))
        }
    }

    fn registry_with_unverified_ollama() -> ProviderRegistry {
        ProviderRegistry::from_results(vec![ollama_result("http://localhost:11434")])
    }

    fn ollama_result(endpoint: &str) -> DiscoveryResult {
        DiscoveryResult {
            provider: Provider {
                id: "ollama".into(),
                name: "Ollama".into(),
                endpoint: endpoint.into(),
                transport: TransportType::Ollama,
                enabled: true,
                api_key: None,
                api_key_env: None,
            },
            models: vec![Model::new(
                "ollama",
                "llama3",
                Capabilities::discovery_default(),
            )],
        }
    }

    #[tokio::test]
    async fn no_providers_running_yields_empty() {
        // Every known provider enabled but pointed at a dead port: nothing
        // responds, independent of what is actually running locally.
        let config = ProviderConfig {
            providers: vec![
                entry("ollama", "http://127.0.0.1:1", "ollama", true),
                entry("lmstudio", "http://127.0.0.1:2", "openai", true),
                entry("llamacpp", "http://127.0.0.1:3", "openai", true),
            ],
        };
        let registry = ProviderRegistry::discover_with(&config).await;
        assert!(registry.results().is_empty());
        assert!(registry.providers().is_empty());
        assert!(registry.models().is_empty());
    }

    #[tokio::test]
    async fn discovers_running_ollama() {
        let server = MockServer::json(r#"{"models":[{"name":"llama3:8b"}]}"#);
        // Ollama at the mock endpoint; the OpenAI-compatible defaults disabled so
        // only ollama is probed and the test stays independent of local state.
        let config = ProviderConfig {
            providers: vec![
                entry("ollama", &server.endpoint(), "ollama", true),
                entry("lmstudio", "http://127.0.0.1:1", "openai", false),
                entry("llamacpp", "http://127.0.0.1:2", "openai", false),
            ],
        };
        let registry = ProviderRegistry::discover_with(&config).await;
        assert_eq!(registry.results().len(), 1);
        assert_eq!(registry.providers()[0].id, "ollama");
        assert_eq!(registry.models().len(), 1);
    }

    #[tokio::test]
    async fn resolve_capabilities_marks_tool_capable_model() {
        let dir = TempCacheDir::new();
        let detector = CapabilityDetector::with_cache_dir(dir.path().to_path_buf());
        let registry = registry_with_unverified_ollama();

        let resolved = registry
            .resolve_capabilities(&detector, |_p| {
                Box::new(ProbeTransport {
                    tool_use: true,
                    offline: false,
                })
            })
            .await;

        let model = resolved.models()[0];
        assert!(model.capabilities.tool_use, "probe should detect tool use");
        assert!(model.verified);
    }

    #[tokio::test]
    async fn resolve_capabilities_survives_offline_probe() {
        let dir = TempCacheDir::new();
        let detector = CapabilityDetector::with_cache_dir(dir.path().to_path_buf());
        let registry = registry_with_unverified_ollama();

        let resolved = registry
            .resolve_capabilities(&detector, |_p| {
                Box::new(ProbeTransport {
                    tool_use: false,
                    offline: true,
                })
            })
            .await;

        // Falls back conservatively without erroring.
        assert!(!resolved.models()[0].capabilities.tool_use);
    }

    #[tokio::test]
    async fn apply_cached_capabilities_reuses_a_prior_verification() {
        let dir = TempCacheDir::new();
        let detector = CapabilityDetector::with_cache_dir(dir.path().to_path_buf());

        // Simulate an earlier session that verified the model (writes the cache).
        detector
            .detect(
                "ollama",
                "llama3",
                &ProbeTransport {
                    tool_use: true,
                    offline: false,
                },
            )
            .await
            .unwrap();

        // A fresh discovery starts unverified; applying the cache resolves it
        // with no probe at all.
        let resolved = registry_with_unverified_ollama().apply_cached_capabilities(&detector);
        let model = resolved.models()[0];
        assert!(model.verified, "cache hit marks the model verified");
        assert!(model.capabilities.tool_use, "cached tool_use is restored");
    }

    #[tokio::test]
    async fn apply_cached_capabilities_leaves_uncached_models_unverified() {
        let dir = TempCacheDir::new();
        let detector = CapabilityDetector::with_cache_dir(dir.path().to_path_buf());

        // Empty cache: nothing to apply, so the model stays unverified and will
        // prompt for verification on selection.
        let resolved = registry_with_unverified_ollama().apply_cached_capabilities(&detector);
        assert!(!resolved.models()[0].verified);
    }

    /// A keyed (remote) provider result with one unverified model.
    fn keyed_result() -> DiscoveryResult {
        DiscoveryResult {
            provider: Provider {
                id: "openrouter".into(),
                name: "OpenRouter".into(),
                endpoint: "https://openrouter.ai/api".into(),
                transport: TransportType::OpenAiCompatible,
                enabled: true,
                api_key: Some("sk-x".into()),
                api_key_env: Some("OPENROUTER_API_KEY".into()),
            },
            models: vec![Model::new(
                "openrouter",
                "qwen/qwen3-coder",
                Capabilities::discovery_default(),
            )],
        }
    }

    #[tokio::test]
    async fn keyed_provider_is_not_auto_probed_while_local_is() {
        let dir = TempCacheDir::new();
        let detector = CapabilityDetector::with_cache_dir(dir.path().to_path_buf());
        let registry = ProviderRegistry::from_results(vec![
            ollama_result("http://localhost:11434"),
            keyed_result(),
        ]);

        // Only the local provider counts toward the startup probe.
        assert!(registry.needs_capability_probe(&detector));

        let resolved = registry
            .resolve_capabilities(&detector, |_p| {
                Box::new(ProbeTransport {
                    tool_use: true,
                    offline: false,
                })
            })
            .await;

        // The local model was probed and verified.
        let local = resolved
            .models()
            .into_iter()
            .find(|m| m.provider_id == "ollama")
            .unwrap();
        assert!(local.verified, "local model is probed at startup");
        assert!(local.capabilities.tool_use);

        // The remote model was left untouched — no surprise paid call.
        let remote = resolved
            .models()
            .into_iter()
            .find(|m| m.provider_id == "openrouter")
            .unwrap();
        assert!(
            !remote.verified,
            "remote model is consent-gated, not probed"
        );
        assert!(!remote.capabilities.tool_use);
    }

    #[tokio::test]
    async fn keyed_only_registry_needs_no_startup_probe() {
        let dir = TempCacheDir::new();
        let detector = CapabilityDetector::with_cache_dir(dir.path().to_path_buf());
        let registry = ProviderRegistry::from_results(vec![keyed_result()]);
        assert!(
            !registry.needs_capability_probe(&detector),
            "a keyed-only registry runs no startup probes"
        );
    }

    #[tokio::test]
    async fn needs_probe_reflects_verified_state() {
        let dir = TempCacheDir::new();
        let detector = CapabilityDetector::with_cache_dir(dir.path().to_path_buf());

        // An unverified model with no cache entry needs a probe.
        let registry = registry_with_unverified_ollama();
        assert!(registry.needs_capability_probe(&detector));

        // An advertised (verified) model needs none.
        let advertised = DiscoveryResult {
            provider: ollama_result("http://localhost:11434").provider,
            models: vec![Model::verified_caps(
                "ollama",
                "qwen3",
                Capabilities::from_ollama_tags(&["tools".into()]),
            )],
        };
        let verified_registry = ProviderRegistry::from_results(vec![advertised]);
        assert!(!verified_registry.needs_capability_probe(&detector));
    }

    fn entry(id: &str, endpoint: &str, transport: &str, enabled: bool) -> ProviderEntry {
        ProviderEntry {
            id: id.into(),
            endpoint: endpoint.into(),
            transport: transport.into(),
            enabled,
            name: None,
            api_key_env: None,
            api_key: None,
        }
    }

    #[tokio::test]
    async fn discover_with_finds_stored_provider_at_custom_endpoint() {
        let server = MockServer::json(r#"{"models":[{"name":"qwen3-coder:latest"}]}"#);
        // Stored ollama at the mock endpoint; the other defaults disabled so the
        // default ports are never probed and the test stays independent of local
        // state.
        let config = ProviderConfig {
            providers: vec![
                entry("ollama", &server.endpoint(), "ollama", true),
                entry("lmstudio", "http://127.0.0.1:1", "openai", false),
                entry("llamacpp", "http://127.0.0.1:2", "openai", false),
            ],
        };

        let registry = ProviderRegistry::discover_with(&config).await;

        assert_eq!(registry.results().len(), 1);
        let provider = registry.providers()[0];
        assert_eq!(provider.id, "ollama");
        assert_eq!(provider.endpoint, server.endpoint());
        assert_eq!(registry.models().len(), 1);
    }

    #[tokio::test]
    async fn discover_with_drops_disabled_provider() {
        // All known providers disabled: nothing is probed, regardless of what
        // is actually running locally.
        let config = ProviderConfig {
            providers: vec![
                entry("ollama", "http://127.0.0.1:1", "ollama", false),
                entry("lmstudio", "http://127.0.0.1:2", "openai", false),
                entry("llamacpp", "http://127.0.0.1:3", "openai", false),
            ],
        };

        let registry = ProviderRegistry::discover_with(&config).await;
        assert!(registry.results().is_empty());
    }

    #[tokio::test]
    async fn discover_with_probes_llamacpp_at_custom_endpoint() {
        // A stored llama.cpp endpoint is probed instead of the default 8080
        // (Project 8.3). The other defaults are disabled to keep the test
        // independent of whatever is running locally.
        let server =
            MockServer::json(r#"{"object":"list","data":[{"id":"qwen2.5-coder-7b-instruct"}]}"#);
        let config = ProviderConfig {
            providers: vec![
                entry("ollama", "http://127.0.0.1:1", "ollama", false),
                entry("lmstudio", "http://127.0.0.1:2", "openai", false),
                entry("llamacpp", &server.endpoint(), "openai", true),
            ],
        };

        let registry = ProviderRegistry::discover_with(&config).await;

        assert_eq!(registry.results().len(), 1);
        let provider = registry.providers()[0];
        assert_eq!(provider.id, "llamacpp");
        assert_eq!(provider.name, "llama.cpp");
        assert_eq!(provider.endpoint, server.endpoint());
        assert_eq!(provider.transport, TransportType::OpenAiCompatible);
        assert_eq!(registry.models().len(), 1);
        assert_eq!(registry.models()[0].provider_id, "llamacpp");
    }

    #[tokio::test]
    async fn stalling_endpoint_is_abandoned_within_timeout() {
        // A port that accepts the connection but never answers must not hold up
        // discovery: it is dropped at the request timeout while a responsive
        // provider is still found. A short injected timeout keeps the test fast.
        let stall = MockServer::stalling();
        let ollama = MockServer::json(r#"{"models":[{"name":"llama3:8b"}]}"#);
        let config = ProviderConfig {
            providers: vec![
                entry("ollama", &ollama.endpoint(), "ollama", true),
                entry("lmstudio", &stall.endpoint(), "openai", true),
                entry("llamacpp", "http://127.0.0.1:1", "openai", false),
            ],
        };
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(300))
            .build()
            .unwrap();

        let start = std::time::Instant::now();
        let registry = ProviderRegistry::discover_with_client(&config, &client).await;
        let elapsed = start.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "a stalling endpoint must not block past the timeout (took {elapsed:?})"
        );
        assert_eq!(registry.results().len(), 1, "only the responsive provider");
        assert_eq!(registry.providers()[0].id, "ollama");
    }

    #[tokio::test]
    async fn auth_failed_recorded_while_healthy_provider_still_discovers() {
        // A keyed OpenAI-compatible provider returns 401; a parallel Ollama is
        // healthy. The auth failure is recorded as a status, not silently
        // dropped, and does not abort the healthy provider's discovery.
        let healthy = MockServer::json(r#"{"models":[{"name":"llama3:8b"}]}"#);
        let denied = MockServer::json_status(401, r#"{"error":"bad key"}"#);
        let config = ProviderConfig {
            providers: vec![
                entry("ollama", &healthy.endpoint(), "ollama", true),
                entry("proxy", &denied.endpoint(), "openai", true),
                entry("lmstudio", "http://127.0.0.1:1", "openai", false),
                entry("llamacpp", "http://127.0.0.1:2", "openai", false),
            ],
        };

        let registry = ProviderRegistry::discover_with(&config).await;

        assert_eq!(registry.results().len(), 1, "only the healthy provider");
        assert_eq!(registry.providers()[0].id, "ollama");
        assert_eq!(registry.auth_failed(), &["proxy".to_string()]);
    }

    #[tokio::test]
    async fn discovered_keyed_provider_carries_resolved_key() {
        // A keyed provider's resolved key must ride along on the discovered
        // provider so a subsequent chat authenticates.
        let server = MockServer::json(r#"{"object":"list","data":[{"id":"gpt-4o-mini"}]}"#);
        let keyed = ProviderEntry {
            id: "proxy".into(),
            endpoint: server.endpoint(),
            transport: "openai".into(),
            enabled: true,
            name: Some("Work Proxy".into()),
            api_key_env: None,
            api_key: Some("literal-key".into()),
        };
        let config = ProviderConfig {
            providers: vec![
                keyed,
                entry("ollama", "http://127.0.0.1:1", "ollama", false),
                entry("lmstudio", "http://127.0.0.1:2", "openai", false),
                entry("llamacpp", "http://127.0.0.1:3", "openai", false),
            ],
        };

        let registry = ProviderRegistry::discover_with(&config).await;
        assert_eq!(registry.results().len(), 1);
        let provider = registry.providers()[0];
        assert_eq!(provider.id, "proxy");
        assert_eq!(provider.name, "Work Proxy");
        assert_eq!(provider.api_key.as_deref(), Some("literal-key"));
    }

    #[tokio::test]
    async fn probe_one_reports_online_with_models() {
        let server =
            MockServer::json(r#"{"object":"list","data":[{"id":"gpt-4o-mini"},{"id":"gpt-4o"}]}"#);
        let outcome =
            ProviderRegistry::probe_one(&entry("proxy", &server.endpoint(), "openai", true)).await;
        match outcome {
            ProbeOutcome::Online(result) => assert_eq!(result.models.len(), 2),
            other => panic!("expected Online, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_one_reports_auth_failed_on_401() {
        let server = MockServer::json_status(401, r#"{"error":"bad key"}"#);
        let keyed = ProviderEntry {
            id: "openrouter".into(),
            endpoint: server.endpoint(),
            transport: "openai".into(),
            enabled: true,
            name: Some("OpenRouter".into()),
            api_key_env: None,
            api_key: Some("sk-bad".into()),
        };
        let outcome = ProviderRegistry::probe_one(&keyed).await;
        match outcome {
            ProbeOutcome::AuthFailed { id, .. } => assert_eq!(id, "openrouter"),
            other => panic!("expected AuthFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn probe_one_reports_offline_on_dead_port() {
        let outcome =
            ProviderRegistry::probe_one(&entry("x", "http://127.0.0.1:1", "openai", true)).await;
        // A refused port is plainly offline (a hollow dot), not a red issue.
        assert!(matches!(outcome, ProbeOutcome::Offline { .. }));
        assert_eq!(outcome.status(), ProviderStatus::Offline);
    }

    #[tokio::test]
    async fn stalling_endpoint_is_a_connection_issue_not_offline() {
        // A host that accepts but never answers times out — distinct from a
        // refused port, so the UI can flag it red ("connection issue") rather
        // than a plain "offline". A short injected timeout keeps the test fast.
        let stall = MockServer::stalling();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(300))
            .build()
            .unwrap();
        let outcome = ProviderRegistry::probe_one_with(
            &entry("x", &stall.endpoint(), "openai", true),
            &client,
        )
        .await;
        assert!(
            matches!(outcome, ProbeOutcome::ConnectionIssue { .. }),
            "a timeout is a connection issue, got {outcome:?}"
        );
        assert_eq!(outcome.status(), ProviderStatus::ConnectionIssue);
    }

    #[tokio::test]
    async fn server_error_is_a_connection_issue() {
        // A 500 is reachable-but-broken, not offline.
        let server = MockServer::json_status(500, r#"{"error":"boom"}"#);
        let outcome =
            ProviderRegistry::probe_one(&entry("x", &server.endpoint(), "openai", true)).await;
        assert!(
            matches!(outcome, ProbeOutcome::ConnectionIssue { .. }),
            "a 5xx is a connection issue, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn probe_one_reports_unparsable_on_wrong_language() {
        // The endpoint connects and responds, but the body is not the expected
        // protocol — the "wrong language" hint case.
        let server = MockServer::json("this is not json at all");
        let outcome =
            ProviderRegistry::probe_one(&entry("proxy", &server.endpoint(), "openai", true)).await;
        assert!(
            matches!(outcome, ProbeOutcome::Unparsable { .. }),
            "expected Unparsable"
        );
    }

    #[test]
    fn every_preset_passes_loader_validation() {
        // A preset chosen as-is must produce a clean probe target (zero issues),
        // proving presets are reproducible through the same path as Custom.
        use crate::presets::PRESETS;
        for preset in PRESETS {
            let config = ProviderConfig {
                providers: vec![ProviderEntry {
                    id: "preset-under-test".into(),
                    endpoint: preset.endpoint.into(),
                    transport: preset.transport.as_str().into(),
                    enabled: true,
                    name: Some(preset.name.into()),
                    api_key_env: preset.key_env.map(str::to_string),
                    api_key: None,
                }],
            };
            let (_targets, issues) = plan_probes(&config);
            assert!(
                issues.is_empty(),
                "preset {} produced issues: {issues:?}",
                preset.name
            );
        }
    }

    #[test]
    fn unknown_transport_entry_becomes_issue_and_rest_loads() {
        let config = ProviderConfig {
            providers: vec![
                entry("ollama", "http://localhost:11434", "ollama", true),
                entry("myproxy", "http://localhost:9999", "openai-compat", true),
            ],
        };
        let (targets, issues) = plan_probes(&config);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "myproxy");
        assert_eq!(issues[0].field, "transport");
        assert!(issues[0].reason.contains("openai-compat"));
        // The valid entry still produces a target; the invalid one does not.
        assert!(targets.iter().any(|t| t.id == "ollama"));
        assert!(!targets.iter().any(|t| t.id == "myproxy"));
    }

    #[test]
    fn malformed_url_is_reported() {
        let config = ProviderConfig {
            providers: vec![entry("bad", "not a url", "openai", true)],
        };
        let (targets, issues) = plan_probes(&config);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "bad");
        assert_eq!(issues[0].field, "endpoint");
        assert!(!targets.iter().any(|t| t.id == "bad"));
    }

    #[test]
    fn duplicate_ids_reported_first_wins() {
        let config = ProviderConfig {
            providers: vec![
                entry("dup", "http://localhost:1", "openai", true),
                entry("dup", "http://localhost:2", "ollama", true),
            ],
        };
        let (targets, issues) = plan_probes(&config);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, "dup");
        assert_eq!(issues[0].field, "id");
        let dup = targets.iter().find(|t| t.id == "dup").unwrap();
        assert_eq!(dup.endpoint, "http://localhost:1");
        assert_eq!(dup.transport, TransportType::OpenAiCompatible);
    }

    #[test]
    fn valid_config_yields_zero_issues_and_default_targets() {
        // An empty config probes exactly the three built-in defaults — the same
        // set as before validation existed.
        let (targets, issues) = plan_probes(&ProviderConfig::default());
        assert!(issues.is_empty());
        let ids: std::collections::BTreeSet<&str> = targets.iter().map(|t| t.id.as_str()).collect();
        let expected: std::collections::BTreeSet<&str> =
            ["llamacpp", "lmstudio", "ollama"].into_iter().collect();
        assert_eq!(ids, expected);
    }

    #[test]
    fn merge_updates_existing_stored_entry() {
        let stored = vec![entry("ollama", "http://old:11434", "ollama", true)];
        let discovered = vec![ollama_result("http://localhost:11434")];

        let merged = ProviderRegistry::merge(&discovered, &stored);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].endpoint, "http://localhost:11434");
        assert!(merged[0].enabled);
    }

    #[test]
    fn merge_adds_new_discovered_entry() {
        let stored: Vec<ProviderEntry> = Vec::new();
        let discovered = vec![ollama_result("http://localhost:11434")];

        let merged = ProviderRegistry::merge(&discovered, &stored);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id, "ollama");
        assert!(merged[0].enabled);
    }

    #[test]
    fn merge_preserves_offline_stored_entry_as_disabled() {
        let stored = vec![entry("lmstudio", "http://localhost:1234", "openai", true)];
        let discovered = vec![ollama_result("http://localhost:11434")];

        let merged = ProviderRegistry::merge(&discovered, &stored);
        // lmstudio (offline, disabled) + ollama (discovered, enabled).
        assert_eq!(merged.len(), 2);
        let lmstudio = merged.iter().find(|p| p.id == "lmstudio").unwrap();
        assert!(!lmstudio.enabled);
        let ollama = merged.iter().find(|p| p.id == "ollama").unwrap();
        assert!(ollama.enabled);
    }
}
