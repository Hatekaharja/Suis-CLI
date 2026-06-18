//! Capability detection: probe a model to learn what it actually supports,
//! caching results under `~/.config/suis/models/<provider_id>.json`.
//!
//! Rather than trusting discovery-time defaults, [`CapabilityDetector`] sends a
//! minimal request with a dummy tool and a streaming request, then records the
//! observed capabilities. Results are reused until they go stale (7 days).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};

use suis_core::config::paths;
use suis_core::{ProviderError, Result};

use crate::capability::Capabilities;
use crate::transport::types::{ChatRequest, Message, Role, ToolDefinition};
use crate::transport::Transport;

/// Cached capabilities are considered stale after this long.
pub const DEFAULT_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How many models to probe at once in [`CapabilityDetector::resolve_models`].
const PROBE_CONCURRENCY: usize = 4;

/// A capability-resolution request for one model (see
/// [`CapabilityDetector::resolve_models`]).
#[derive(Debug, Clone, Copy)]
pub struct ModelCapsRequest<'a> {
    /// The provider-native model id to resolve.
    pub model_id: &'a str,
    /// Capabilities already advertised by the provider, trusted without a
    /// probe. `None` means resolve via cache or a runtime probe.
    pub advertised: Option<Capabilities>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CapabilityRecord {
    capabilities: Capabilities,
    /// Unix seconds at which detection ran.
    detected_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CachedCapabilities {
    #[serde(default)]
    models: HashMap<String, CapabilityRecord>,
}

/// Detects and caches model capabilities.
pub struct CapabilityDetector {
    cache_dir: PathBuf,
    ttl: Duration,
}

impl CapabilityDetector {
    /// Use the real global cache directory and the default TTL.
    pub fn new() -> Self {
        CapabilityDetector {
            cache_dir: paths::models_dir(),
            ttl: DEFAULT_TTL,
        }
    }

    /// Use an explicit cache directory (tests).
    pub fn with_cache_dir(cache_dir: PathBuf) -> Self {
        CapabilityDetector {
            cache_dir,
            ttl: DEFAULT_TTL,
        }
    }

    /// Override the staleness TTL.
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Return capabilities for `model_id`, using the cache when fresh and
    /// probing `transport` otherwise. A successful probe is written to cache.
    ///
    /// `transport` is taken as a trait object so callers holding a
    /// `Box<dyn Transport>` (as startup does) can drive detection directly.
    pub async fn detect(
        &self,
        provider_id: &str,
        model_id: &str,
        transport: &dyn Transport,
    ) -> Result<Capabilities> {
        let mut cache = self.load_cache(provider_id);
        if let Some(record) = cache.models.get(model_id) {
            if !self.is_stale(record.detected_at) {
                return Ok(record.capabilities);
            }
        }

        let capabilities = self.probe(model_id, transport).await?;

        cache.models.insert(
            model_id.to_string(),
            CapabilityRecord {
                capabilities,
                detected_at: now_secs(),
            },
        );
        self.save_cache(provider_id, &cache)?;

        Ok(capabilities)
    }

    /// Whether `model_id` has a fresh (non-stale) entry in `provider_id`'s
    /// cache. Lets callers decide up front whether a probe phase is needed
    /// without performing one.
    pub fn is_fresh_cached(&self, provider_id: &str, model_id: &str) -> bool {
        self.load_cache(provider_id)
            .models
            .get(model_id)
            .is_some_and(|record| !self.is_stale(record.detected_at))
    }

    /// Every fresh (non-stale) cached capability for `provider_id`, keyed by
    /// model id. A single, network-free file read used at startup to re-apply
    /// previously-verified results so a model verified in an earlier session
    /// stays verified across launches without re-probing.
    pub fn fresh_cached_models(&self, provider_id: &str) -> HashMap<String, Capabilities> {
        self.load_cache(provider_id)
            .models
            .into_iter()
            .filter(|(_, record)| !self.is_stale(record.detected_at))
            .map(|(model_id, record)| (model_id, record.capabilities))
            .collect()
    }

    /// Resolve capabilities for several models of one provider at once, reading
    /// and writing that provider's cache file a single time. Priority per model:
    ///
    /// 1. advertised capabilities (trusted, recorded to cache),
    /// 2. a fresh cache entry (reused, no probe),
    /// 3. a runtime probe over `transport` (cached on success).
    ///
    /// Probes run concurrently (bounded). A failed/offline probe falls back to
    /// [`Capabilities::discovery_default`] and is not cached, so it is retried
    /// on the next run. Results are returned in `requests` order.
    pub async fn resolve_models(
        &self,
        provider_id: &str,
        requests: &[ModelCapsRequest<'_>],
        transport: &dyn Transport,
    ) -> Vec<Capabilities> {
        let mut cache = self.load_cache(provider_id);
        let now = now_secs();
        let mut resolved: Vec<Option<Capabilities>> = vec![None; requests.len()];
        let mut to_probe: Vec<usize> = Vec::new();

        for (idx, req) in requests.iter().enumerate() {
            if let Some(caps) = req.advertised {
                // Trust advertised caps and record them so later runs agree.
                cache.models.insert(
                    req.model_id.to_string(),
                    CapabilityRecord {
                        capabilities: caps,
                        detected_at: now,
                    },
                );
                resolved[idx] = Some(caps);
            } else if let Some(record) = cache.models.get(req.model_id) {
                if !self.is_stale(record.detected_at) {
                    resolved[idx] = Some(record.capabilities);
                    continue;
                }
                to_probe.push(idx);
            } else {
                to_probe.push(idx);
            }
        }

        // Probe the remaining models concurrently, bounded so a provider with
        // many models doesn't open an unbounded number of connections.
        let probes = futures::stream::iter(to_probe.into_iter().map(|idx| async move {
            let caps = self.probe(requests[idx].model_id, transport).await;
            (idx, caps)
        }))
        .buffer_unordered(PROBE_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

        for (idx, caps) in probes {
            match caps {
                Ok(caps) => {
                    cache.models.insert(
                        requests[idx].model_id.to_string(),
                        CapabilityRecord {
                            capabilities: caps,
                            detected_at: now_secs(),
                        },
                    );
                    resolved[idx] = Some(caps);
                }
                // Offline / errored probe: fall back, don't cache, retry later.
                Err(_) => resolved[idx] = Some(Capabilities::discovery_default()),
            }
        }

        // Persist the cache once. A write failure is non-fatal (we still return
        // the resolved capabilities for this session).
        let _ = self.save_cache(provider_id, &cache);

        resolved
            .into_iter()
            .map(|c| c.unwrap_or_else(Capabilities::discovery_default))
            .collect()
    }

    /// Run the actual detection requests against a model.
    async fn probe(&self, model_id: &str, transport: &dyn Transport) -> Result<Capabilities> {
        let tool_use = self.detect_tool_use(model_id, transport).await?;
        let streaming = self.detect_streaming(model_id, transport).await?;
        Ok(Capabilities {
            chat: true,
            streaming,
            tool_use,
            structured_output: false,
        })
    }

    async fn detect_tool_use(&self, model_id: &str, transport: &dyn Transport) -> Result<bool> {
        let request = ChatRequest {
            model: model_id.to_string(),
            messages: vec![Message::text(
                Role::User,
                "What is the weather in Paris? Use the tool.",
            )],
            tools: Some(vec![probe_tool()]),
            stream: false,
        };
        let response = transport.chat(request).await?;
        // A model is tool-capable whether it returns the call on the structured
        // channel or prints it as text (Hermes-style) — recover the latter too,
        // so such models still get tools offered.
        let text_calls = crate::transport::tool_text::parse_text_tool_calls(&response.content);
        Ok(!response.tool_calls.is_empty() || !text_calls.calls.is_empty())
    }

    async fn detect_streaming(&self, model_id: &str, transport: &dyn Transport) -> Result<bool> {
        let request = ChatRequest {
            model: model_id.to_string(),
            messages: vec![Message::text(Role::User, "Say hi.")],
            tools: None,
            stream: true,
        };
        let mut stream = transport.chat_stream(request).await?;
        // At least one chunk arriving means streaming is supported.
        Ok(stream.next().await.is_some())
    }

    fn is_stale(&self, detected_at: u64) -> bool {
        now_secs().saturating_sub(detected_at) >= self.ttl.as_secs()
    }

    fn cache_path(&self, provider_id: &str) -> PathBuf {
        self.cache_dir.join(format!("{provider_id}.json"))
    }

    fn load_cache(&self, provider_id: &str) -> CachedCapabilities {
        let path = self.cache_path(provider_id);
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => CachedCapabilities::default(),
        }
    }

    fn save_cache(&self, provider_id: &str, cache: &CachedCapabilities) -> Result<()> {
        std::fs::create_dir_all(&self.cache_dir)
            .map_err(|e| ProviderError::RequestError(format!("cache dir: {e}")))?;
        let json = serde_json::to_vec_pretty(cache)
            .map_err(|e| ProviderError::ParseError(format!("cache serialize: {e}")))?;
        std::fs::write(self.cache_path(provider_id), json)
            .map_err(|e| ProviderError::RequestError(format!("cache write: {e}")))?;
        Ok(())
    }
}

impl Default for CapabilityDetector {
    fn default() -> Self {
        Self::new()
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The dummy tool offered during tool-use detection.
fn probe_tool() -> ToolDefinition {
    ToolDefinition {
        name: "get_weather".into(),
        description: "Get the current weather for a city.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempCacheDir;
    use crate::transport::types::{ChatResponse, ToolCall};
    use crate::transport::{ChatStream, Transport};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A transport that counts calls and optionally returns a tool call.
    struct MockTransport {
        calls: Arc<AtomicUsize>,
        returns_tool_call: bool,
    }

    impl MockTransport {
        fn new(returns_tool_call: bool) -> Self {
            MockTransport {
                calls: Arc::new(AtomicUsize::new(0)),
                returns_tool_call,
            }
        }
    }

    #[async_trait]
    impl Transport for MockTransport {
        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let tool_calls = if self.returns_tool_call {
                vec![ToolCall {
                    id: "call_0".into(),
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

        async fn chat_stream(&self, _request: ChatRequest) -> Result<ChatStream> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let chunks = vec![Ok(ChatResponse {
                content: "hi".into(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                done: true,
                usage: None,
            })];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }
    }

    /// A transport that emits its tool call as *text* (Hermes-style) rather than
    /// on the structured channel — what many local models do.
    struct TextToolTransport;

    #[async_trait]
    impl Transport for TextToolTransport {
        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse> {
            Ok(ChatResponse {
                content:
                    "<tool_call>{\"name\": \"get_weather\", \"arguments\": {\"city\": \"Paris\"}}</tool_call>"
                        .into(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                done: true,
                usage: None,
            })
        }

        async fn chat_stream(&self, _request: ChatRequest) -> Result<ChatStream> {
            Ok(Box::pin(futures::stream::iter(vec![Ok(ChatResponse {
                content: "hi".into(),
                reasoning: String::new(),
                tool_calls: Vec::new(),
                done: true,
                usage: None,
            })])))
        }
    }

    /// A transport whose requests always fail (simulates an offline model).
    struct FailingTransport;

    #[async_trait]
    impl Transport for FailingTransport {
        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse> {
            Err(ProviderError::NotRunning("offline".into()).into())
        }

        async fn chat_stream(&self, _request: ChatRequest) -> Result<ChatStream> {
            Err(ProviderError::NotRunning("offline".into()).into())
        }
    }

    #[tokio::test]
    async fn cache_miss_runs_detection_and_writes() {
        let dir = TempCacheDir::new();
        let detector = CapabilityDetector::with_cache_dir(dir.path().to_path_buf());
        let transport = MockTransport::new(true);

        let caps = detector
            .detect("ollama", "qwen3", &transport)
            .await
            .unwrap();
        assert!(caps.tool_use);
        assert!(caps.streaming);
        assert!(transport.calls.load(Ordering::Relaxed) >= 1);
        assert!(dir.path().join("ollama.json").exists());
    }

    #[tokio::test]
    async fn cache_hit_makes_no_requests() {
        let dir = TempCacheDir::new();
        let detector = CapabilityDetector::with_cache_dir(dir.path().to_path_buf());

        // First call populates the cache.
        let first = MockTransport::new(true);
        detector.detect("ollama", "qwen3", &first).await.unwrap();

        // Second call must read from cache and not touch the transport.
        let second = MockTransport::new(true);
        let caps = detector.detect("ollama", "qwen3", &second).await.unwrap();
        assert!(caps.tool_use);
        assert_eq!(second.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn model_that_calls_tool_is_tool_use_true() {
        let dir = TempCacheDir::new();
        let detector = CapabilityDetector::with_cache_dir(dir.path().to_path_buf());
        let transport = MockTransport::new(true);
        let caps = detector.detect("ollama", "m", &transport).await.unwrap();
        assert!(caps.tool_use);
    }

    #[tokio::test]
    async fn model_that_emits_tool_call_as_text_is_tool_use_true() {
        let dir = TempCacheDir::new();
        let detector = CapabilityDetector::with_cache_dir(dir.path().to_path_buf());
        let caps = detector
            .detect("ollama", "qwen3", &TextToolTransport)
            .await
            .unwrap();
        assert!(
            caps.tool_use,
            "a text-emitted tool call should count as tool use"
        );
    }

    #[tokio::test]
    async fn model_that_ignores_tool_is_tool_use_false() {
        let dir = TempCacheDir::new();
        let detector = CapabilityDetector::with_cache_dir(dir.path().to_path_buf());
        let transport = MockTransport::new(false);
        let caps = detector.detect("ollama", "m", &transport).await.unwrap();
        assert!(!caps.tool_use);
    }

    #[tokio::test]
    async fn stale_cache_triggers_redetection() {
        let dir = TempCacheDir::new();
        let detector =
            CapabilityDetector::with_cache_dir(dir.path().to_path_buf()).with_ttl(Duration::ZERO);

        let first = MockTransport::new(true);
        detector.detect("ollama", "m", &first).await.unwrap();

        // TTL is zero, so the cached entry is immediately stale → re-detect.
        let second = MockTransport::new(true);
        detector.detect("ollama", "m", &second).await.unwrap();
        assert!(second.calls.load(Ordering::Relaxed) >= 1);
    }

    #[tokio::test]
    async fn detect_drives_a_boxed_trait_object() {
        let dir = TempCacheDir::new();
        let detector = CapabilityDetector::with_cache_dir(dir.path().to_path_buf());
        // Exercise the &dyn Transport path explicitly (startup holds a Box).
        let transport: Box<dyn Transport> = Box::new(MockTransport::new(true));
        let caps = detector
            .detect("ollama", "m", transport.as_ref())
            .await
            .unwrap();
        assert!(caps.tool_use);
    }

    #[tokio::test]
    async fn resolve_trusts_advertised_caps_without_probing() {
        let dir = TempCacheDir::new();
        let detector = CapabilityDetector::with_cache_dir(dir.path().to_path_buf());
        let transport = MockTransport::new(true);

        let advertised = Capabilities {
            tool_use: true,
            ..Capabilities::discovery_default()
        };
        let requests = [ModelCapsRequest {
            model_id: "qwen3",
            advertised: Some(advertised),
        }];
        let caps = detector
            .resolve_models("ollama", &requests, &transport)
            .await;
        assert!(caps[0].tool_use);
        // Advertised caps are trusted: no request hit the transport.
        assert_eq!(transport.calls.load(Ordering::Relaxed), 0);
        // …and they were recorded, so a later probe-less resolve still hits.
        assert!(detector.is_fresh_cached("ollama", "qwen3"));
    }

    #[tokio::test]
    async fn resolve_probes_unverified_models_and_caches() {
        let dir = TempCacheDir::new();
        let detector = CapabilityDetector::with_cache_dir(dir.path().to_path_buf());
        let transport = MockTransport::new(true);

        let requests = [ModelCapsRequest {
            model_id: "m",
            advertised: None,
        }];
        let caps = detector
            .resolve_models("ollama", &requests, &transport)
            .await;
        assert!(caps[0].tool_use);
        assert!(transport.calls.load(Ordering::Relaxed) >= 1);

        // Second resolve uses the cache: no further probes.
        let second = MockTransport::new(true);
        let caps = detector.resolve_models("ollama", &requests, &second).await;
        assert!(caps[0].tool_use);
        assert_eq!(second.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn resolve_falls_back_when_probe_fails() {
        let dir = TempCacheDir::new();
        let detector = CapabilityDetector::with_cache_dir(dir.path().to_path_buf());
        let transport = FailingTransport;

        let requests = [ModelCapsRequest {
            model_id: "m",
            advertised: None,
        }];
        let caps = detector
            .resolve_models("ollama", &requests, &transport)
            .await;
        // Offline probe → conservative default, no error surfaced.
        assert!(!caps[0].tool_use);
        // A failed probe is not cached, so it will be retried next run.
        assert!(!detector.is_fresh_cached("ollama", "m"));
    }
}
