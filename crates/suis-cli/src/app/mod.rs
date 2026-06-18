//! Application assembly: startup, the agent bridge, and the entry point that
//! wires them to the event loop.
//!
//! - [`startup`] resolves config / workspace / providers.
//! - [`state`] is the render/-input source of truth.
//! - [`input`] maps keys to actions.
//! - [`event_loop`] owns the terminal and the render/poll cycle.
//! - [`agent_bridge`] is the CLI ↔ Agent channel wiring: it owns the tokio task
//!   running the agent loop, accepting user messages and emitting
//!   [`AgentEvent`](suis_agent::AgentEvent)s back to the UI.

pub mod agent_bridge;
pub mod discovery;
pub mod event_loop;
pub mod input;
pub mod startup;
pub mod state;

use std::env;

use crate::screens::model_select::ModelSelect;
use crate::screens::project_init::ProjectInit;
use startup::Startup;
use state::{AppState, Screen};

/// Run the application: startup, then the event loop.
pub async fn run() -> std::io::Result<()> {
    let cwd = env::current_dir()?;
    let (state, startup) = match Startup::run(&cwd).await {
        Ok(startup) => build_initial_state(startup),
        Err(message) => (AppState::error(format!("Startup failed: {message}")), None),
    };
    event_loop::run(state, startup).await
}

/// Turn a successful [`Startup`] into the initial [`AppState`] and the startup
/// context the event loop keeps.
///
/// - No `.suis/` yet → the interactive project-init flow.
/// - Otherwise → straight to model selection with the loaded project config.
///
/// No providers discovered is *not* an error: the model picker opens on its
/// empty state with the "+ Add a provider…" row, so the user can set one up
/// without ever leaving the app. Startup context is always carried so that
/// add-provider flow (and the re-probe behind it) can run.
fn build_initial_state(startup: Startup) -> (AppState, Option<Startup>) {
    // Honor the project's model scope so a scoped project shows only its
    // models even against a provider that lists hundreds (20.3).
    let scope = startup
        .project
        .as_ref()
        .map(|p| p.model_scope.clone())
        .unwrap_or_default();
    // The picker opens on the discovery skeleton — every provider that will be
    // probed, each "checking…" — and the event loop streams statuses in.
    let mut model_select = ModelSelect::from_plan(&startup.discovery.planned, &scope);
    // Honor the configured default provider, if any, by pre-selecting it.
    if let Some(preferred) = &startup.global.settings.default_provider {
        model_select.focus_provider(preferred);
    }

    let mut state = AppState::new(model_select);
    // Seed the live discovery state with the "checking…" skeleton; the event
    // loop's background probes resolve it.
    state.discovery = startup.discovery.clone();
    state.workspace_root = Some(startup.workspace.root.display().to_string());
    match &startup.project {
        Some(project) => state.project = Some(project.clone()),
        None => {
            state.project_init = Some(ProjectInit::new(startup.gitignore.clone()));
            state.screen = Screen::ProjectInit;
        }
    }
    (state, Some(startup))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use suis_core::{GlobalConfig, ProjectConfig, ProviderConfig, Workspace};

    /// A startup with no providers discovered; `initialized` controls whether a
    /// project config is present (an existing `.suis/`).
    fn empty_startup(initialized: bool) -> Startup {
        Startup {
            global: GlobalConfig::default(),
            workspace: Workspace {
                root: PathBuf::from("/tmp/project"),
                suis_dir: PathBuf::from("/tmp/project/.suis"),
                is_git: false,
            },
            project: initialized.then(ProjectConfig::default),
            gitignore: Vec::new(),
            provider_config: ProviderConfig::default(),
            discovery: crate::app::discovery::DiscoveryState::default(),
        }
    }

    #[test]
    fn no_providers_opens_the_picker_and_keeps_startup() {
        // An empty registry is no longer fatal: with the project initialized the
        // model picker opens straight away (on its empty "+ Add a provider…"
        // state), and the startup context survives so that flow can run.
        let (state, startup) = build_initial_state(empty_startup(true));
        assert_eq!(state.screen, Screen::ModelSelect);
        assert!(state.model_select.is_empty(), "no providers discovered");
        assert!(
            startup.is_some(),
            "startup must survive for the add-provider flow"
        );
    }

    #[test]
    fn no_providers_still_runs_project_init_first_when_uninitialized() {
        // No providers must not skip first-run init; that still leads the way,
        // and the (empty) picker follows it.
        let (state, startup) = build_initial_state(empty_startup(false));
        assert_eq!(state.screen, Screen::ProjectInit);
        assert!(state.project_init.is_some());
        assert!(startup.is_some());
    }
}
