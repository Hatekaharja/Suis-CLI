//! The bridge between the UI and the agent: channel wiring for CLI ↔ Agent.
//!
//! [`AgentBridge::spawn`] starts the tokio task that owns the [`Session`] and
//! [`Agent`] for the active model. The UI drives it by sending [`UiToAgent`]
//! messages (a user turn, or a history clear); the agent reports progress back
//! over a separate [`AgentEvent`](suis_agent::AgentEvent) channel, whose
//! receiver `spawn` hands to the event loop. Permission requests flow as
//! [`AgentEvent::PermissionRequest`](suis_agent::AgentEvent::PermissionRequest)
//! carrying their own `oneshot` reply channel, so they need no extra plumbing
//! here.
//!
//! Selecting a new model spawns a fresh bridge; dropping the bridge (its sender)
//! ends the previous agent task.

use tokio::sync::{mpsc, watch};

use suis_agent::{Agent, AgentEvent, ImplementTarget, Mode, Phase, Session, Transport};
use suis_core::{ProjectConfig, Workspace};
use suis_providers::Model;

/// A control or chat message sent from the UI to the agent task.
#[derive(Debug, Clone)]
pub enum UiToAgent {
    /// A user turn to run.
    UserMessage(String),
    /// Reset the agent's conversation history (`/clear`). Also ends any
    /// implementation session — the work package went with the history.
    Clear,
    /// Switch the session's runtime mode (Shift+Tab, `/plan`, `/agent`, `/chat`).
    SetMode(Mode),
    /// Replace the session's project config (e.g. after `/profile refresh`), so a
    /// freshly detected profile takes effect without re-spawning the session.
    /// Boxed to keep the enum small.
    SetProject(Box<ProjectConfig>),
    /// Start a fresh, focused implementation session: clear history and ledger,
    /// force Agent mode, set the target, and drive the work tasks one at a time
    /// (each on its own reset context).
    Implement {
        /// The plan's store id.
        plan_id: String,
        /// Zero-based step index.
        step_index: usize,
    },
    /// The user approved the verify gate: drive the step's verify tasks one at a
    /// time, the same way the work tasks ran.
    StartVerify,
    /// Summarize the conversation and replace history with the summary
    /// (`/compact`). Spends one visible model call; only ever user-initiated.
    Compact,
}

/// Owns the channel to the agent task spawned for the current session.
pub struct AgentBridge {
    to_agent: mpsc::Sender<UiToAgent>,
    /// Out-of-band interrupt signal: the agent task is busy inside a turn (not
    /// reading `to_agent`), so Esc reaches it through this side channel.
    interrupt: watch::Sender<()>,
}

impl AgentBridge {
    /// Spawn the agent task for `model` and return the bridge plus the receiver
    /// of [`AgentEvent`](suis_agent::AgentEvent)s the task emits.
    ///
    /// The task owns the [`Session`] and [`Agent`]; it runs a turn per
    /// [`UiToAgent::UserMessage`] and clears history on [`UiToAgent::Clear`].
    /// It ends when the returned [`AgentBridge`] (and thus its sender) is dropped.
    pub fn spawn(
        workspace: Workspace,
        project: ProjectConfig,
        model: Model,
        transport: Box<dyn Transport>,
    ) -> (AgentBridge, mpsc::Receiver<AgentEvent>) {
        let (ui_tx, mut ui_rx) = mpsc::channel::<UiToAgent>(64);
        let (ev_tx, ev_rx) = mpsc::channel::<AgentEvent>(256);
        let (interrupt_tx, interrupt_rx) = watch::channel(());

        tokio::spawn(async move {
            let mut session = Session::new(workspace, project, model);
            // Resolve the session's history-pruning budget. A per-model
            // `settings.json` window override wins (and caps); otherwise the
            // discovered window — uniform across providers, Ollama included. The
            // context gauge is built from this, so the percentage tracks the
            // model's window. Suis sizes only its own prompt; Ollama's own
            // context-window and KV-cache settings govern its memory.
            let settings = suis_core::GlobalConfig::load().ok().map(|c| c.settings);
            let key = format!("{}/{}", session.model.provider_id, session.model.model_id);
            let window_override = settings
                .as_ref()
                .and_then(|s| s.model_context_windows.get(&key).copied());
            let configured = settings.as_ref().and_then(|s| s.context_budget);
            session.context_budget =
                suis_agent::resolve_context_budget(&session.model, window_override, configured);
            let agent = Agent::new(transport, ev_tx).with_interrupt(interrupt_rx);
            while let Some(msg) = ui_rx.recv().await {
                match msg {
                    UiToAgent::UserMessage(text) => agent.run_turn(&mut session, text).await,
                    UiToAgent::Clear => {
                        session.history.clear();
                        session.ledger.clear();
                        session.implement = None;
                        if let Ok(mut log) = session.access.lock() {
                            *log = Default::default();
                        }
                    }
                    UiToAgent::SetMode(mode) => session.mode = mode,
                    UiToAgent::SetProject(project) => session.project = *project,
                    UiToAgent::Implement {
                        plan_id,
                        step_index,
                    } => {
                        session.history.clear();
                        session.ledger.clear();
                        session.mode = Mode::Agent;
                        session.implement = Some(ImplementTarget {
                            plan_id,
                            step_index,
                        });
                        // The driver seeds the lean context, fills the task
                        // panel, and works the step's work tasks one at a time.
                        agent.run_implement_phase(&mut session, Phase::Work).await;
                    }
                    UiToAgent::StartVerify => {
                        agent.run_implement_phase(&mut session, Phase::Verify).await;
                    }
                    UiToAgent::Compact => agent.compact(&mut session).await,
                }
            }
        });

        (
            AgentBridge {
                to_agent: ui_tx,
                interrupt: interrupt_tx,
            },
            ev_rx,
        )
    }

    /// Ask the agent to abandon the running turn at the next safe point: a
    /// streaming response stops immediately (even mid-stall); a tool that is
    /// already executing finishes first — tools are never killed mid-write —
    /// and everything after it is skipped. Synchronous and idempotent.
    pub fn interrupt(&self) {
        let _ = self.interrupt.send(());
    }

    /// Send a user turn to the agent. Returns `false` if the agent task has
    /// already ended (its receiver was dropped).
    pub async fn send_message(&self, text: String) -> bool {
        self.to_agent
            .send(UiToAgent::UserMessage(text))
            .await
            .is_ok()
    }

    /// Ask the agent to reset its conversation history.
    pub async fn clear(&self) {
        let _ = self.to_agent.send(UiToAgent::Clear).await;
    }

    /// Switch the session's runtime mode.
    pub async fn set_mode(&self, mode: Mode) {
        let _ = self.to_agent.send(UiToAgent::SetMode(mode)).await;
    }

    /// Replace the session's project config (e.g. after `/profile refresh`).
    pub async fn set_project(&self, project: ProjectConfig) {
        let _ = self
            .to_agent
            .send(UiToAgent::SetProject(Box::new(project)))
            .await;
    }

    /// Ask the agent to compact (summarize) the conversation. Returns `false`
    /// if the agent task has already ended.
    pub async fn compact(&self) -> bool {
        self.to_agent.send(UiToAgent::Compact).await.is_ok()
    }

    /// Start an implementation session for one plan step.
    pub async fn implement(&self, plan_id: String, step_index: usize) {
        let _ = self
            .to_agent
            .send(UiToAgent::Implement {
                plan_id,
                step_index,
            })
            .await;
    }

    /// Begin the verify phase of the active step (after the user approves the
    /// gate). Returns `false` if the agent task has already ended.
    pub async fn verify(&self) -> bool {
        self.to_agent.send(UiToAgent::StartVerify).await.is_ok()
    }
}
