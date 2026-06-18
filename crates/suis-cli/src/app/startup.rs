//! Application startup: load configuration, detect the workspace, resolve the
//! project config, and discover running providers.
//!
//! The result feeds [`AppState`](super::state::AppState): if no providers are
//! found the app opens on the error screen; if the workspace has no `.suis/`
//! the app opens on the interactive project-init flow
//! ([`project_init`](crate::screens::project_init)); otherwise on model
//! selection. Startup never writes anything itself — first-run setup is the
//! init flow's job.

use std::path::Path;

use suis_agent::Transport;
use suis_core::config::paths::providers_path;
use suis_core::{GlobalConfig, ProjectConfig, ProviderConfig, ProviderEntry, Workspace};
use suis_providers::{AnthropicTransport, OllamaTransport, OpenAiTransport, TransportType};

use super::discovery::DiscoveryState;
use crate::screens::model_select::ModelEntry;

/// The tools a freshly-initialized project exposes to the agent. The core
/// default is empty (no tools); the CLI enables the standard set so a new
/// project is usable immediately.
const DEFAULT_TOOLS: &[&str] = &[
    "tree",
    "read_lines",
    "search",
    "edit",
    "bash",
    "git",
    "task",
    "delegate",
];

/// The standard tool set, as owned strings, for building a project config.
pub(crate) fn default_tools() -> Vec<String> {
    DEFAULT_TOOLS.iter().map(|s| s.to_string()).collect()
}

/// A default project config (standard tools, no hidden/hardened entries). Used
/// when the user declines initialization but still wants to run this session.
pub(crate) fn default_project() -> ProjectConfig {
    ProjectConfig {
        allowed_tools: default_tools(),
        ..ProjectConfig::default()
    }
}

/// Everything resolved before the UI starts.
///
/// Discovery is **not** run here: probing every provider used to block the whole
/// UI behind the slowest endpoint (a sleeping LAN host eats the full connect
/// timeout). Instead the loaded [`provider_config`](Startup::provider_config)
/// and the "checking…" [`discovery`](Startup::discovery) skeleton are carried so
/// the event loop can open the picker immediately and stream probe results in.
pub struct Startup {
    /// User-level configuration.
    pub global: GlobalConfig,
    /// The detected workspace.
    pub workspace: Workspace,
    /// The loaded project configuration, or `None` if the project has not been
    /// initialized yet (no `.suis/`), in which case the init flow runs.
    pub project: Option<ProjectConfig>,
    /// `.gitignore` patterns discovered for an uninitialized project, offered
    /// to the init flow for import. Empty for an already-initialized project.
    pub gitignore: Vec<String>,
    /// The loaded provider config, kept so the event loop can run (and re-run)
    /// background discovery against it without re-reading the file.
    pub provider_config: ProviderConfig,
    /// The initial discovery skeleton: every provider that will be probed,
    /// marked "checking…", resolved as the background probes report.
    pub discovery: DiscoveryState,
}

impl Startup {
    /// Run the startup sequence rooted at `cwd`. Fast by construction — only
    /// local config reads, no network — so the UI appears immediately; the
    /// background discovery the event loop spawns fills in provider status.
    pub async fn run(cwd: &Path) -> Result<Startup, String> {
        let global = GlobalConfig::load().map_err(|e| format!("loading config: {e}"))?;
        let workspace = Workspace::detect(cwd).map_err(|e| format!("detecting workspace: {e}"))?;
        let (project, gitignore) = if workspace.suis_dir.exists() {
            let config = ProjectConfig::load(&workspace)
                .map_err(|e| format!("loading project config: {e}"))?;
            (Some(config), Vec::new())
        } else {
            (None, read_gitignore(&workspace.root))
        };
        let provider_config = ProviderConfig::load().unwrap_or_default();
        let discovery = DiscoveryState::planning(&provider_config);
        Ok(Startup {
            global,
            workspace,
            project,
            gitignore,
            provider_config,
            discovery,
        })
    }
}

/// Read the workspace's `.gitignore`, returning its meaningful patterns
/// (skipping blanks, comments, and negations). Returns an empty list if there
/// is no `.gitignore`.
fn read_gitignore(root: &Path) -> Vec<String> {
    let Ok(contents) = std::fs::read_to_string(root.join(".gitignore")) else {
        return Vec::new();
    };
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('!'))
        .map(str::to_string)
        .collect()
}

/// On first run — before any `providers.json` exists — write the discovered
/// providers out so later runs know about them and the user can edit or disable
/// entries via `/providers`. Writing is best-effort: a failure must not block
/// the session, and once the file exists we never overwrite it (that would clobber
/// the user's enable/disable choices).
pub(crate) fn persist_discovered_providers(discovery: &DiscoveryState) {
    if providers_path().exists() {
        return;
    }
    let providers: Vec<ProviderEntry> = discovery
        .results
        .iter()
        .map(|r| r.provider.to_entry())
        .collect();
    if providers.is_empty() {
        return;
    }
    let _ = ProviderConfig { providers }.save();
}

/// Build the transport for a selected model from its provider's transport type,
/// endpoint, resolved API key, and (for error attribution) provider id and key
/// env-var name.
pub fn build_transport(entry: &ModelEntry) -> Box<dyn Transport> {
    transport_for(
        entry.transport,
        &entry.endpoint,
        &entry.provider_id,
        entry.api_key.as_deref(),
        entry.api_key_env.as_deref(),
    )
}

/// Build a transport for a provider's transport type and endpoint, attaching
/// the resolved API key for transports that authenticate plus the provider id
/// and key env-var name so remote failures are attributed (20.2). The Ollama
/// protocol has no auth, so the key is ignored there.
fn transport_for(
    kind: TransportType,
    endpoint: &str,
    provider_id: &str,
    key: Option<&str>,
    key_env: Option<&str>,
) -> Box<dyn Transport> {
    // Note: a plaintext-key warning (key sent over remote http) is surfaced as a
    // dismissible popup when the session is spawned (see `spawn_session`), not
    // here — this builder must stay silent so it can't corrupt the TUI.
    match kind {
        // Model residency (`keep_alive`) and context sizing are the user's own
        // Ollama settings to manage; Suis just points the transport at the
        // endpoint.
        TransportType::Ollama => Box::new(OllamaTransport::new(endpoint.to_string())),
        TransportType::OpenAiCompatible => Box::new(OpenAiTransport::with_auth(
            endpoint.to_string(),
            provider_id.to_string(),
            key.map(|s| s.to_string()),
            key_env.map(|s| s.to_string()),
        )),
        TransportType::Anthropic => Box::new(AnthropicTransport::with_auth(
            endpoint.to_string(),
            provider_id.to_string(),
            key.map(|s| s.to_string()),
            key_env.map(|s| s.to_string()),
        )),
    }
}
