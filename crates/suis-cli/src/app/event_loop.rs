//! The terminal event loop.
//!
//! Owns the terminal (raw mode + alternate screen, restored on exit and on
//! panic), reads terminal events on a blocking thread, and on each iteration
//! draws the active screen and then waits for the next key, resize, agent
//! event, or render tick. Keys become [`Action`]s ([`super::input`]);
//! [`apply_action`] carries out the ones that need the agent channel or the
//! terminal. A resize simply triggers a redraw — `Terminal::draw` re-measures
//! the backend and repaints at the new size.

use std::io::{self, Stdout};
use std::path::Path;
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Frame;
use ratatui::Terminal;
use tokio::sync::mpsc;

use std::collections::HashSet;

use suis_agent::AgentEvent;
use suis_core::{PermissionStore, ProjectConfig, ProviderConfig, ProviderEntry, Workspace};
use suis_providers::{
    Capabilities, CapabilityDetector, ProbeOutcome, ProviderRegistry, ProviderStatus,
    DEFAULT_PROVIDER_IDS,
};

use super::agent_bridge::AgentBridge;
use super::discovery::DiscoveryState;
use super::input::{self, Action};
use super::startup::{self, Startup};
use super::state::{AppState, Screen, VerifyCaps};
use crate::commands::{self, CommandEffect};
use crate::screens;
use crate::screens::model_select::ModelSelect;
use crate::screens::project_init::InitOutcome;
use crate::screens::provider_form::{TestOutcome, TestState};
use crate::screens::providers::ProvidersView;

/// A finished connection test, routed back from its background task.
struct TestResult {
    outcome: TestOutcome,
    /// True when the result belongs to the open form, false for the list test.
    in_form: bool,
}

/// A finished capability-verification probe, routed back from its background
/// task so the chat-screen spinner can resolve into the verified caps.
struct CapsResult {
    provider_id: String,
    model_id: String,
    caps: Capabilities,
}

/// A message from the background discovery task. Each provider's probe streams
/// an [`Outcome`](DiscoveryMsg::Outcome) the moment it lands — so a fast
/// provider resolves immediately while a dead one resolves later — and a final
/// [`Done`](DiscoveryMsg::Done) marks the batch complete (first-run persistence).
enum DiscoveryMsg {
    Outcome(ProbeOutcome),
    Done,
}

type Term = Terminal<CrosstermBackend<Stdout>>;

/// Run the event loop until the user quits. `startup` is `None` on the error
/// screen, where no agent is ever spawned.
pub async fn run(mut state: AppState, startup: Option<Startup>) -> io::Result<()> {
    let mut terminal = init_terminal()?;
    install_panic_hook();

    let result = event_loop(&mut terminal, &mut state, startup).await;

    restore_terminal(&mut terminal)?;
    result
}

/// The core loop, separated so the terminal is always restored afterward.
async fn event_loop(
    terminal: &mut Term,
    state: &mut AppState,
    startup: Option<Startup>,
) -> io::Result<()> {
    let (term_tx, mut term_rx) = mpsc::channel::<Event>(64);
    spawn_input_thread(term_tx);

    // The agent bridge is established when a model is selected. Until then a
    // kept-alive empty channel keeps the event branch pending (not spinning).
    let (keep_tx, mut events_rx) = mpsc::channel::<AgentEvent>(256);
    let mut _events_keepalive = Some(keep_tx);
    let mut bridge: Option<AgentBridge> = None;

    // Connection-test outcomes from background probe tasks (19.2) arrive here so
    // the UI shows "testing…" immediately and the result lands without blocking.
    let (test_tx, mut test_rx) = mpsc::channel::<TestResult>(8);

    // Capability-verification probes run on a background task so the chat-screen
    // popup can animate its spinner; the resolved caps return over this channel.
    let (caps_tx, mut caps_rx) = mpsc::channel::<CapsResult>(4);

    // Provider discovery runs in the background so the picker opens immediately
    // (it used to block the whole UI behind the slowest endpoint). Outcomes
    // stream back here and resolve each "checking…" row as they land. Unbounded
    // so the probe closure can send synchronously without awaiting.
    // Kept past the initial discovery spawn so a single-provider re-probe (Enter
    // on the `/providers` list) can stream its outcome back through the same path.
    let (disc_tx, mut disc_rx) = mpsc::unbounded_channel::<DiscoveryMsg>();
    if let Some(s) = startup.as_ref() {
        spawn_discovery(s.provider_config.clone(), disc_tx.clone());
    }

    // The UI tick drives the busy spinner/elapsed display; while idle it is a
    // no-op (no tick-driven redraws), so an idle session renders only on input
    // and agent events.
    let mut tick = tokio::time::interval(Duration::from_millis(150));

    loop {
        terminal.draw(|frame| render(frame, state))?;
        if state.should_quit {
            return Ok(());
        }

        // A model selection done this iteration installs new agent channels
        // *after* the select block (so it doesn't alias `events_rx`).
        let mut pending_events_rx: Option<mpsc::Receiver<AgentEvent>> = None;

        // Wait for something that warrants a redraw.
        loop {
            tokio::select! {
                maybe_term = term_rx.recv() => {
                    match maybe_term {
                        Some(Event::Key(key)) => {
                            let action = input::handle_key(state, key);
                            apply_action(
                                action,
                                state,
                                startup.as_ref(),
                                &mut bridge,
                                &mut pending_events_rx,
                                &test_tx,
                                &caps_tx,
                                &disc_tx,
                            )
                            .await;
                        }
                        // Scroll-wheel events scroll the active screen in place;
                        // handled here so the terminal doesn't turn the wheel
                        // into history-walking Up/Down arrows.
                        Some(Event::Mouse(mouse)) => {
                            input::handle_mouse(state, mouse);
                        }
                        // Any other event (a resize) just falls through to the
                        // redraw at the top of the outer loop.
                        _ => {}
                    }
                    break;
                }
                maybe_event = events_rx.recv() => {
                    if let Some(event) = maybe_event {
                        state.apply_event(event);
                    }
                    break;
                }
                maybe_test = test_rx.recv() => {
                    if let Some(result) = maybe_test {
                        apply_test_result(state, result);
                    }
                    break;
                }
                maybe_caps = caps_rx.recv() => {
                    if let Some(result) = maybe_caps {
                        apply_caps_result(
                            state,
                            result,
                            startup.as_ref(),
                            &mut bridge,
                            &mut pending_events_rx,
                        );
                    }
                    break;
                }
                maybe_disc = disc_rx.recv() => {
                    if let Some(msg) = maybe_disc {
                        apply_discovery_msg(state, startup.as_ref(), msg);
                    }
                    break;
                }
                _ = tick.tick() => {
                    if state.on_tick() {
                        break;
                    }
                }
            }
        }

        if let Some(rx) = pending_events_rx.take() {
            events_rx = rx;
            _events_keepalive = None;
        }
    }
}

/// Carry out an [`Action`] that the input layer could not perform in place.
async fn apply_action(
    action: Action,
    state: &mut AppState,
    startup: Option<&Startup>,
    bridge: &mut Option<AgentBridge>,
    pending_events_rx: &mut Option<mpsc::Receiver<AgentEvent>>,
    test_tx: &mpsc::Sender<TestResult>,
    caps_tx: &mpsc::Sender<CapsResult>,
    disc_tx: &mpsc::UnboundedSender<DiscoveryMsg>,
) {
    match action {
        Action::None => {}
        Action::Quit => state.should_quit = true,
        Action::AnswerPermission(decision) => state.answer_permission(decision),
        Action::InitAnswer(yes) => init_answer(yes, state, startup),
        Action::InitConfirmImport => init_advance_import(state, false),
        Action::InitSkipImport => init_advance_import(state, true),
        Action::SelectModel => select_model(state, startup, bridge, pending_events_rx),
        Action::AddProvider => open_add_provider_form(state, Screen::ModelSelect),
        Action::LeaveProviders => leave_providers(state, startup).await,
        Action::ProbeCaps => start_caps_probe(state, caps_tx),
        Action::PersistProviders => persist_providers(state),
        Action::SaveProvider => save_provider(state, startup).await,
        Action::RemoveProvider(id) => remove_provider(state, startup, id).await,
        Action::RevokePermission => revoke_permission(state, startup),
        Action::AddDeny(pattern) => add_deny(state, startup, pattern),
        Action::TestProvider { entry, in_form } => spawn_test(entry, in_form, test_tx),
        Action::ReprobeProvider { entry } => spawn_reprobe(entry, disc_tx),
        Action::SetMode(mode) => set_mode(mode, state, bridge).await,
        Action::AnswerPlan(decision) => state.answer_plan(decision),
        Action::StartImplement => start_implement(state, bridge).await,
        Action::BeginVerification => begin_verification(state, bridge).await,
        Action::StartNextStep => start_next_step(state, bridge).await,
        Action::Submit(text) => submit(text, state, startup, bridge).await,
        Action::Interrupt => {
            if let Some(bridge) = bridge {
                bridge.interrupt();
            }
        }
        Action::CopyTranscript => copy_transcript(state),
    }
}

/// Copy the whole conversation to the system clipboard (`Ctrl+Y` in developer
/// mode) and report the outcome as a system notice. The dump is the full raw
/// history — every message, tool call, and tool output — not the collapsed view.
fn copy_transcript(state: &mut AppState) {
    let text = crate::widgets::message_list::transcript_text(&state.messages);
    if text.is_empty() {
        state.push_system("Nothing to copy yet.");
        return;
    }
    let count = state.messages.len();
    match crate::clipboard::copy(&text) {
        Ok(()) => state.push_system(format!(
            "Copied conversation to clipboard ({count} message{}).",
            if count == 1 { "" } else { "s" }
        )),
        Err(e) => state.push_system(format!("Could not copy to clipboard: {e}")),
    }
}

/// Fold a finished connection test into whichever surface started it.
fn apply_test_result(state: &mut AppState, result: TestResult) {
    let done = TestState::Done(result.outcome);
    if result.in_form {
        if let Some(form) = state.provider_form.as_mut() {
            form.test = done;
        }
    } else {
        // An in-list test (Ctrl+T) can come from either surface. Each view-model
        // gates its own display by the header the test was started on, so feeding
        // both is harmless and keeps whichever is on screen in step.
        state.providers.set_test(done.clone());
        state.model_select.set_test(done);
    }
}

/// Probe `entry` on a background task and route the outcome back over `test_tx`,
/// so the UI shows "testing…" immediately and never blocks on a dead endpoint.
fn spawn_test(entry: ProviderEntry, in_form: bool, test_tx: &mpsc::Sender<TestResult>) {
    let tx = test_tx.clone();
    let key_env = entry.api_key_env.clone();
    tokio::spawn(async move {
        let outcome = ProviderRegistry::probe_one(&entry).await;
        let result = TestResult {
            outcome: TestOutcome::from_probe(outcome, key_env),
            in_form,
        };
        let _ = tx.send(result).await;
    });
}

/// Re-probe a single provider on a background task (Enter on `/providers`),
/// routing the outcome through the discovery channel so it refreshes the picker,
/// the live discovery state, and the `/providers` row — the same path startup
/// discovery uses, so a server brought up mid-session resolves without a restart.
fn spawn_reprobe(entry: ProviderEntry, disc_tx: &mpsc::UnboundedSender<DiscoveryMsg>) {
    let tx = disc_tx.clone();
    tokio::spawn(async move {
        let outcome = ProviderRegistry::probe_one(&entry).await;
        let _ = tx.send(DiscoveryMsg::Outcome(outcome));
    });
}

/// Spawn the background discovery task: stream each provider's probe outcome
/// over `tx` as it lands, then a final `Done`. The closure sends synchronously
/// (the channel is unbounded), so a fast provider's result reaches the UI in
/// milliseconds without waiting on the slow ones.
fn spawn_discovery(config: ProviderConfig, tx: mpsc::UnboundedSender<DiscoveryMsg>) {
    tokio::spawn(async move {
        let sender = tx.clone();
        ProviderRegistry::discover_streaming(&config, |outcome| {
            let _ = sender.send(DiscoveryMsg::Outcome(outcome));
        })
        .await;
        let _ = tx.send(DiscoveryMsg::Done);
    });
}

/// Fold a streamed discovery message into the UI: an outcome resolves its
/// provider's status (and, if online, its models) in both the picker and the
/// live discovery state; `Done` persists first-run discoveries.
fn apply_discovery_msg(state: &mut AppState, startup: Option<&Startup>, msg: DiscoveryMsg) {
    match msg {
        DiscoveryMsg::Outcome(outcome) => {
            let outcome = apply_cached_caps(outcome);
            state.model_select.apply_outcome(&outcome);
            // Keep the `/providers` list (if open) in step: a single re-probe
            // flips that row's status in place, cursor preserved.
            state.providers.apply_outcome(&outcome);
            // Keep a preferred provider opening expanded once its models arrive.
            if let ProbeOutcome::Online(result) = &outcome {
                if let Some(startup) = startup {
                    if startup.global.settings.default_provider.as_deref()
                        == Some(result.provider.id.as_str())
                        && state.model_select.filter.is_empty()
                    {
                        state.model_select.focus_provider(&result.provider.id);
                    }
                }
            }
            state.discovery.apply(outcome);
        }
        DiscoveryMsg::Done => startup::persist_discovered_providers(&state.discovery),
    }
}

/// Re-apply previously-verified capabilities from the on-disk cache to an online
/// provider's freshly-discovered models, so a model verified in an earlier
/// session stays verified across launches (and skips the "Verify capabilities?"
/// prompt) without a re-probe. A purely local read; non-online outcomes pass
/// through untouched.
fn apply_cached_caps(outcome: ProbeOutcome) -> ProbeOutcome {
    let ProbeOutcome::Online(result) = outcome else {
        return outcome;
    };
    let registry = ProviderRegistry::from_results(vec![result])
        .apply_cached_capabilities(&CapabilityDetector::new());
    // `from_results` kept exactly one result, so it is still the only one here.
    let result = registry
        .results()
        .first()
        .cloned()
        .expect("single result survives cache application");
    ProbeOutcome::Online(result)
}

/// Save the provider form: validate (staying open on failure), persist through
/// `ProviderConfig`, then re-probe so the change is visible immediately.
async fn save_provider(state: &mut AppState, startup: Option<&Startup>) {
    let Some(form) = state.provider_form.as_mut() else {
        return;
    };
    let entry = match form.validate() {
        Ok(entry) => entry,
        // Invalid: the form records the field + reason and stays open.
        Err(_) => return,
    };
    let original = form.original_id().map(str::to_string);

    let mut config = ProviderConfig::load().unwrap_or_default();
    upsert_entry(&mut config, entry, original.as_deref());
    let _ = config.save();

    state.provider_form = None;
    state.screen = state.provider_form_return;
    reprobe(state, startup).await;
}

/// Insert `entry`, replacing any same-id entry; when an edit renamed the id, the
/// old id is dropped first so no stale duplicate lingers.
fn upsert_entry(config: &mut ProviderConfig, entry: ProviderEntry, original_id: Option<&str>) {
    if let Some(old) = original_id {
        if old != entry.id {
            config.providers.retain(|e| e.id != old);
        }
    }
    match config.providers.iter_mut().find(|e| e.id == entry.id) {
        Some(existing) => *existing = entry,
        None => config.providers.push(entry),
    }
}

/// Remove a provider: a custom entry is dropped; one of the built-in defaults is
/// stored disabled instead (so discovery suppresses it but a preset can revive
/// it). Updates the in-memory state directly instead of re-probing every
/// provider, so the deletion is instant and doesn't block on unreachable
/// endpoints.
async fn remove_provider(state: &mut AppState, _startup: Option<&Startup>, id: String) {
    let mut config = ProviderConfig::load().unwrap_or_default();
    // A disabled default not yet in the file is reconstructed from its row so
    // its endpoint/transport survive for a later re-enable. The picker is the
    // source when the removal came from there (the `/providers` list may be empty
    // at the initial selection screen).
    let fallback = state
        .providers
        .rows()
        .iter()
        .find(|r| r.id == id)
        .map(|r| r.to_entry())
        .or_else(|| state.model_select.entry_for(&id));
    apply_removal(&mut config, &id, fallback);
    let _ = config.save();

    // Directly update the in-memory state — no blocking re-discovery needed.
    // The model-select picker removes the provider from its groups.
    state.model_select.remove_provider(&id);
    // Forget the live discovery snapshot too; otherwise the add form still sees
    // a deleted custom id in `known_ids()` and rejects reusing it until restart.
    state.discovery.remove_provider(&id);
    // Rebuild the `/providers` view from the current discovery + new config.
    state.providers = providers_view(&state.discovery, &config);
}

/// Apply a removal to the persisted config: a custom entry is dropped entirely;
/// one of the built-in defaults is stored disabled (updating its entry, or
/// adding `fallback` disabled when the file has no row for it yet) so discovery
/// suppresses it but a preset can revive it.
fn apply_removal(config: &mut ProviderConfig, id: &str, fallback: Option<ProviderEntry>) {
    if DEFAULT_PROVIDER_IDS.contains(&id) {
        match config.providers.iter_mut().find(|e| e.id == id) {
            Some(existing) => existing.enabled = false,
            None => {
                if let Some(mut entry) = fallback {
                    entry.enabled = false;
                    config.providers.push(entry);
                }
            }
        }
    } else {
        config.providers.retain(|e| e.id != id);
    }
}

/// Re-run discovery from the persisted config and rebuild both the providers
/// list and the model-selection view, so an in-app provider change is reflected
/// the moment it is saved — in `/providers` and the next `/model`. Capability
/// probing is intentionally skipped here (unlike startup) to avoid surprise
/// model calls and a mid-session stall; models carry their discovery-default or
/// advertised capabilities until selected.
async fn reprobe(state: &mut AppState, startup: Option<&Startup>) {
    let config = ProviderConfig::load().unwrap_or_default();
    let scope = state
        .project
        .as_ref()
        .map(|p| p.model_scope.clone())
        .unwrap_or_default();

    // Mirror the streaming startup path so offline/issue providers also appear:
    // open on the "checking…" skeleton, then fold every probe outcome in. This
    // is blocking (the user just edited a provider and is waiting on the result)
    // but still abandons an unreachable endpoint at its timeout, not reqwest's.
    let mut discovery = DiscoveryState::planning(&config);
    let mut model_select = ModelSelect::from_plan(&discovery.planned, &scope);
    let mut outcomes = Vec::new();
    ProviderRegistry::discover_streaming(&config, |o| outcomes.push(o)).await;
    for outcome in outcomes {
        let outcome = apply_cached_caps(outcome);
        model_select.apply_outcome(&outcome);
        discovery.apply(outcome);
    }
    if let Some(startup) = startup {
        if let Some(preferred) = &startup.global.settings.default_provider {
            model_select.focus_provider(preferred);
        }
    }

    state.model_select = model_select;
    state.providers = providers_view(&discovery, &config);
    state.discovery = discovery;
}

/// Build the `/providers` view-model from the live discovery state merged with
/// stored config, threading each provider's status (online / offline /
/// connection-issue / auth-failed) so the screen colours match the picker.
fn providers_view(discovery: &DiscoveryState, config: &ProviderConfig) -> ProvidersView {
    let merged = discovery.merged(&config.providers);
    let online = discovery.online_ids();
    let auth_failed: HashSet<String> = discovery.auth_failed.iter().cloned().collect();
    let connection_issue: HashSet<String> = discovery
        .statuses
        .iter()
        .filter(|(_, s)| **s == ProviderStatus::ConnectionIssue)
        .map(|(id, _)| id.clone())
        .collect();
    ProvidersView::from_parts(
        &merged,
        &online,
        &auth_failed,
        &connection_issue,
        &discovery.issues,
    )
}

/// Start the implementation session staged in `state.pending_implement`:
/// reset the UI into the fresh session and tell the agent to clear history,
/// switch to Agent mode, and open with the work package.
async fn start_implement(state: &mut AppState, bridge: &Option<AgentBridge>) {
    let Some(target) = state.pending_implement.take() else {
        return;
    };
    let Some(bridge) = bridge else {
        state.push_system("Select a model first (/model).");
        return;
    };
    if state.busy {
        state.push_system("Cannot start an implementation session while the agent is working.");
        return;
    }
    bridge
        .implement(target.plan_id.clone(), target.step_index)
        .await;
    state.begin_implement(target);
}

/// The user approved verification: drive the step's verify tasks one at a time,
/// the same per-task reset the work tasks ran through.
async fn begin_verification(state: &mut AppState, bridge: &Option<AgentBridge>) {
    let Some(bridge) = bridge else {
        state.push_system("Agent is not available.");
        return;
    };
    state.push_system("Beginning verification.");
    state.begin_busy();
    if !bridge.verify().await {
        state.end_busy();
        state.push_system("Agent is not available.");
    }
}

/// The user confirmed "Continue to next Step?": advance to the next step in
/// the plan, clearing the transcript and starting a fresh implementation
/// session for it.
async fn start_next_step(state: &mut AppState, bridge: &Option<AgentBridge>) {
    let Some(next) = state.pending_next_step.take() else {
        return;
    };
    let Some(bridge) = bridge else {
        state.push_system("Select a model first (/model).");
        return;
    };
    if state.busy {
        state.push_system("Cannot start an implementation step while the agent is working.");
        return;
    }
    bridge
        .implement(next.plan_id.clone(), next.step_index)
        .await;
    state.begin_next_step(next);
}

/// `/plans`: list the stored plans with progress in the transcript.
fn show_plans(state: &mut AppState, startup: Option<&Startup>) {
    let Some(startup) = startup else {
        return;
    };
    match suis_core::PlanStore::load(&startup.workspace) {
        Ok(store) => state.push_system_styled(commands::plans_lines(&store)),
        Err(e) => state.push_system(format!("Could not load plans: {e}")),
    }
}

/// `/implement`: open the plan-selection screen, or point at Plan mode when
/// no plans exist yet.
fn open_implement(state: &mut AppState, startup: Option<&Startup>) {
    let Some(startup) = startup else {
        return;
    };
    if state.busy {
        state.push_system("Cannot start an implementation session while the agent is working.");
        return;
    }
    let store = match suis_core::PlanStore::load(&startup.workspace) {
        Ok(store) => store,
        Err(e) => {
            state.push_system(format!("Could not load plans: {e}"));
            return;
        }
    };
    if store.plans.is_empty() {
        state.push_system(
            "No plans stored. Switch to Plan mode (/plan) and ask the agent to draft one.",
        );
        return;
    }
    state.plan_select = screens::plan_select::PlanSelect::from_store(&store);
    state.screen = Screen::PlanSelect;
}

/// `/compact`: ask the agent to summarize the conversation and replace history
/// with the summary. Spends one visible model call, so it is rejected mid-turn;
/// the busy/`compacting` flags drive the "Compacting…" status until the
/// `Compacted` event lands.
async fn compact(state: &mut AppState, bridge: &Option<AgentBridge>) {
    let Some(bridge) = bridge else {
        state.push_system("Select a model first (/model).");
        return;
    };
    if state.busy {
        state.push_system("Cannot compact while the agent is working.");
        return;
    }
    if !state.has_conversation() {
        state.push_system("Nothing to compact yet.");
        return;
    }
    state.begin_busy();
    state.compacting = true;
    state.push_system("Compacting the conversation…");
    if !bridge.compact().await {
        state.end_busy();
        state.compacting = false;
        state.push_system("Agent is not available.");
    }
}

/// `/profile`: show the cached project brief, or — with `refresh` — re-detect it
/// from the workspace manifests, persist it to `.suis/project.json`, and push the
/// new config to the live agent so the warm-start prompt updates this session.
async fn show_profile(
    refresh: bool,
    state: &mut AppState,
    startup: Option<&Startup>,
    bridge: &Option<AgentBridge>,
) {
    let Some(startup) = startup else {
        state.push_system("No workspace yet — the project profile is unavailable.");
        return;
    };
    if refresh {
        if state.busy {
            state.push_system("Cannot refresh the profile while the agent is working.");
            return;
        }
        let mut project = state
            .project
            .clone()
            .unwrap_or_else(startup::default_project);
        let profile = suis_agent::detect_profile(&startup.workspace);
        // Adopt the detected test command as the verify command only when none is
        // set, so a refresh never clobbers a verify command the user chose.
        if project.verify_command.is_none() {
            project.verify_command = profile.test_cmd.clone();
        }
        project.profile = Some(profile);
        // Best-effort persist: a write failure shouldn't lose the in-memory profile.
        if let Err(e) = project.save(&startup.workspace) {
            state.push_system(format!("Could not save the project profile: {e}"));
        }
        // Propagate to the running agent so the warm-start prompt picks it up now.
        if let Some(bridge) = bridge {
            bridge.set_project(project.clone()).await;
        }
        state.project = Some(project);
        state.push_system("Project profile refreshed.");
    }
    let profile = state.project.as_ref().and_then(|p| p.profile.as_ref());
    state.push_system_styled(commands::profile_lines(profile));
}

/// Switch the runtime mode, keeping the UI mirror and the agent's session in
/// step. Rejected mid-turn: the agent reads the mode when a turn starts, so a
/// change while it is working would not apply until the next turn anyway —
/// better to say so than to silently defer.
async fn set_mode(mode: suis_agent::Mode, state: &mut AppState, bridge: &Option<AgentBridge>) {
    if state.busy {
        state.push_system("Cannot switch modes while the agent is working.");
        return;
    }
    state.mode = mode;
    if let Some(bridge) = bridge {
        bridge.set_mode(mode).await;
    }
}

/// Persist the current provider enable/disable toggles. Best-effort: a write
/// failure must not interrupt the session.
fn persist_providers(state: &AppState) {
    let _ = state.providers.to_config().save();
}

/// Load the global and project permission stores and open the permissions
/// screen. Without a workspace (no `startup`) there is nothing to show.
fn open_permissions(state: &mut AppState, startup: Option<&Startup>) {
    let Some(startup) = startup else {
        return;
    };
    let (global, project) = PermissionStore::load_split(&startup.workspace).unwrap_or_default();
    state.permissions = crate::screens::permissions::PermissionsView::from_split(global, project);
    state.screen = Screen::Permissions;
}

/// Write the two permission stores back to disk. Best-effort: a write failure
/// must not interrupt the session.
fn persist_permissions(state: &AppState, startup: Option<&Startup>) {
    if let Some(startup) = startup {
        let (global, project) = state.permissions.stores();
        let _ = PermissionStore::save_split(global, project, &startup.workspace);
    }
}

/// Revoke the staged permission row (set by the confirm overlay) and persist.
/// The overlay owns input while open, so the highlighted row is still the one it
/// was opened on.
fn revoke_permission(state: &mut AppState, startup: Option<&Startup>) {
    if state.pending_permission_revoke.take().is_none() {
        return;
    }
    if state.permissions.revoke_selected() {
        persist_permissions(state, startup);
    }
}

/// Add a project-level command deny and persist.
fn add_deny(state: &mut AppState, startup: Option<&Startup>, pattern: String) {
    if state.permissions.add_deny(pattern) {
        persist_permissions(state, startup);
    }
}

/// Open the add-provider form, returning to `return_to` on save or cancel. Used
/// by the model-select "add a provider" row (returning to the picker) so a new
/// endpoint can be set up without first opening `/providers`.
fn open_add_provider_form(state: &mut AppState, return_to: Screen) {
    state.provider_form = Some(crate::screens::provider_form::ProviderForm::new_add(
        existing_provider_ids(state),
    ));
    state.provider_form_return = return_to;
    state.screen = Screen::ProviderForm;
}

/// Every configured or discovered provider id, for the add-form's uniqueness
/// check: the planned providers (built-in defaults ∪ stored config) merged with
/// whatever discovery has resolved so far this session.
fn existing_provider_ids(state: &AppState) -> Vec<String> {
    let stored = ProviderConfig::load().unwrap_or_default();
    let mut ids = state.discovery.known_ids();
    for entry in &stored.providers {
        if !ids.contains(&entry.id) {
            ids.push(entry.id.clone());
        }
    }
    ids
}

/// Leave the providers screen for its recorded return target. When that target
/// is the model picker (onboarding), discovery is re-run first so the list
/// reflects providers just added or enabled; returning to chat needs no refresh.
async fn leave_providers(state: &mut AppState, startup: Option<&Startup>) {
    if state.providers_return == Screen::ModelSelect {
        reprobe(state, startup).await;
    }
    state.screen = state.providers_return;
}

/// Advance past the per-entry `.gitignore` import step: either keep the current
/// per-entry selections (`skip == false`) or skip the import entirely. The flow
/// stays on screen (the next step is git access), so no persistence happens yet.
fn init_advance_import(state: &mut AppState, skip: bool) {
    if let Some(init) = state.project_init.as_mut() {
        if skip {
            init.skip_all();
        } else {
            init.confirm_import();
        }
    }
}

/// Advance the project-init flow by one answer, persisting and proceeding to
/// model selection once it completes (or is cancelled).
fn init_answer(yes: bool, state: &mut AppState, startup: Option<&Startup>) {
    let Some(init) = state.project_init.as_mut() else {
        return;
    };
    let outcome = if yes {
        init.answer_yes()
    } else {
        init.answer_no()
    };
    match outcome {
        InitOutcome::Pending => {}
        InitOutcome::Cancelled => {
            // Run this session without writing `.suis/`.
            state.project_init = None;
            state.project = Some(startup::default_project());
            state.screen = Screen::ModelSelect;
        }
        InitOutcome::Complete(mut config) => {
            state.project_init = None;
            if let Some(startup) = startup {
                // Seed the warm-start profile from a deterministic, offline scan
                // of the project's manifests, and adopt its test command as the
                // verify command. The user can re-detect later with /profile.
                let profile = suis_agent::detect_profile(&startup.workspace);
                if config.verify_command.is_none() {
                    config.verify_command = profile.test_cmd.clone();
                }
                config.profile = Some(profile);
                // Best-effort: a write failure shouldn't block using the session.
                let _ = persist_init(&startup.workspace, &config);
            }
            state.project = Some(config);
            // Onboarding goes straight to the model picker. Setting up a
            // provider is handled there by its "Add a provider" row, so the
            // separate provider-review step is no longer part of first-run.
            state.screen = Screen::ModelSelect;
        }
    }
}

/// Write the freshly chosen project config and an empty permission store to
/// `.suis/`. Both writes create the directory as needed. Also appends `.suis`
/// to the project's `.gitignore` so the directory is excluded from version
/// control.
fn persist_init(workspace: &Workspace, config: &ProjectConfig) -> Result<(), String> {
    config
        .save(workspace)
        .map_err(|e| format!("writing project config: {e}"))?;
    PermissionStore::default()
        .save(workspace)
        .map_err(|e| format!("writing permissions: {e}"))?;
    append_gitignore_suis(&workspace.root);
    Ok(())
}

/// Append `.suis` to the project's `.gitignore` if it is not already present.
/// Creates the file if it does not exist. Best-effort: silently ignores errors
/// so a missing or read-only `.gitignore` cannot block the setup flow.
fn append_gitignore_suis(root: &Path) {
    let gitignore_path = root.join(".gitignore");
    let existing = std::fs::read_to_string(&gitignore_path).ok();
    if existing.as_deref() == Some(".suis\n") || existing.as_deref() == Some(".suis") {
        return;
    }
    let mut content = existing.unwrap_or_default();
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(".suis\n");
    let _ = std::fs::write(&gitignore_path, content);
}

/// Spawn (or replace) the agent for the highlighted model and open the chat.
/// A no-op when the cursor is not on a model (a provider header or the add
/// row); the actual work is delegated to [`spawn_session`].
fn select_model(
    state: &mut AppState,
    startup: Option<&Startup>,
    bridge: &mut Option<AgentBridge>,
    pending_events_rx: &mut Option<mpsc::Receiver<AgentEvent>>,
) {
    let Some(entry) = state.model_select.selected().cloned() else {
        return;
    };
    // Fresh selection: surface session-start warnings (e.g. plaintext key).
    spawn_session(state, startup, entry, bridge, pending_events_rx, true);
}

/// Spawn (or replace) the agent for an explicit model `entry` and open the chat.
/// Taking the entry directly — rather than re-reading the model-select cursor —
/// lets the post-verification path ([`apply_caps_result`]) rebuild the session
/// without depending on where the cursor happens to sit (it is reset by the
/// first selection's `clear_filter`).
fn spawn_session(
    state: &mut AppState,
    startup: Option<&Startup>,
    entry: crate::screens::model_select::ModelEntry,
    bridge: &mut Option<AgentBridge>,
    pending_events_rx: &mut Option<mpsc::Receiver<AgentEvent>>,
    announce_warnings: bool,
) {
    let Some(startup) = startup else {
        return;
    };

    let project = state
        .project
        .clone()
        .unwrap_or_else(startup::default_project);
    let transport = startup::build_transport(&entry);
    let (new_bridge, ev_rx) = AgentBridge::spawn(
        startup.workspace.clone(),
        project,
        entry.model.clone(),
        transport,
    );
    *bridge = Some(new_bridge);
    *pending_events_rx = Some(ev_rx);

    // Fresh session: reset transcript/tasks/mode and any implementation
    // session, leave filter mode. A dropped plan review resolves to Reject.
    state.clear_transcript();
    state.tasks.clear();
    state.end_busy();
    state.mode = suis_agent::Mode::default();
    state.end_implement();
    state.pending_plan = None;
    state.model_select.clear_filter();
    state.selected_model = Some(entry.model.clone());
    state.provider_name = Some(entry.provider_name.clone());
    state.screen = Screen::Chat;
    // The welcome banner shows identity on the empty transcript; the armed
    // notice lands with the first message so identity survives in scrollback.
    state.arm_session_notice();

    // Surface a one-time popup if this session will send the API key over
    // plaintext http to a non-local host (see `key_sent_in_plaintext`). Only on
    // a fresh spawn, not the post-verification rebuild, so it isn't shown twice.
    if announce_warnings
        && suis_providers::transport::key_sent_in_plaintext(
            &entry.endpoint,
            entry.api_key.is_some(),
        )
    {
        state.notice = Some(format!(
            "The API key for {} is being sent over plaintext HTTP to {}. \
             It can be intercepted in transit — use an https:// endpoint instead.",
            entry.provider_name, entry.endpoint
        ));
    }

    // An unverified model opens the chat with the bridge already spawned on its
    // assumed chat-only default, then offers to verify its real capabilities;
    // accepting re-spawns the agent with the resolved caps (see
    // `apply_caps_result`). Already-verified and declined models skip this.
    if state.needs_caps_consent(&entry) {
        state.verify_caps = Some(VerifyCaps {
            entry,
            probing: false,
        });
    }
}

/// Start the capability-verification probe for the model in `verify_caps` on a
/// background task, marking the popup as probing so it shows a spinner. The
/// probe runs the standard detection path over the provider's transport and
/// caches its result; the outcome returns over `caps_tx` (see
/// [`apply_caps_result`]). A failed probe falls back to conservative chat-only
/// capabilities. Does nothing if the probe is already running.
fn start_caps_probe(state: &mut AppState, caps_tx: &mpsc::Sender<CapsResult>) {
    let Some(vc) = state.verify_caps.as_mut() else {
        return;
    };
    if vc.probing {
        return;
    }
    vc.probing = true;
    let entry = vc.entry.clone();
    let tx = caps_tx.clone();
    tokio::spawn(async move {
        let detector = CapabilityDetector::new();
        let transport = startup::build_transport(&entry);
        let caps = detector
            .detect(
                &entry.model.provider_id,
                &entry.model.model_id,
                transport.as_ref(),
            )
            .await
            .unwrap_or_else(|_| Capabilities::discovery_default());
        let _ = tx
            .send(CapsResult {
                provider_id: entry.model.provider_id,
                model_id: entry.model.model_id,
                caps,
            })
            .await;
    });
}

/// Fold a finished verification probe back in: record the resolved caps on the
/// model (flipping it verified so its badge brightens), close the popup, and
/// rebuild the agent so its now-known capabilities gate its tools — all without
/// a restart.
///
/// The rebuild spawns from the *verified entry itself*, not the model-select
/// cursor: the first selection's `clear_filter` reset the cursor (to a provider
/// header under the collapsible list), so re-reading `selected()` here would
/// find no model and silently skip the rebuild — the bug where a freshly
/// verified model still reported "no tools" until the next launch.
fn apply_caps_result(
    state: &mut AppState,
    result: CapsResult,
    startup: Option<&Startup>,
    bridge: &mut Option<AgentBridge>,
    pending_events_rx: &mut Option<mpsc::Receiver<AgentEvent>>,
) {
    state
        .model_select
        .set_caps(&result.provider_id, &result.model_id, result.caps);
    // Rebuild from the entry that was being verified, carrying the resolved
    // capabilities so the spawned agent advertises tools immediately.
    if let Some(vc) = state.verify_caps.take() {
        let mut entry = vc.entry;
        entry.model.capabilities = result.caps;
        entry.model.verified = true;
        // A rebuild of the same session: warnings were already shown on the
        // initial spawn, so don't pop them again.
        spawn_session(state, startup, entry, bridge, pending_events_rx, false);
    }
}

/// Handle a chat submission: a slash command, or a message for the agent.
async fn submit(
    text: String,
    state: &mut AppState,
    startup: Option<&Startup>,
    bridge: &Option<AgentBridge>,
) {
    if commands::is_command(&text) {
        let command = commands::parse(&text).expect("is_command implies parse succeeds");
        match commands::handle(&command) {
            CommandEffect::OpenModelSelect => state.screen = Screen::ModelSelect,
            // `/providers` now opens the merged model picker, where provider
            // management (add/edit/delete/test) lives on the provider headers —
            // one place to both pick a model and manage its provider.
            CommandEffect::OpenProviders => state.screen = Screen::ModelSelect,
            CommandEffect::OpenPermissions => open_permissions(state, startup),
            CommandEffect::ToggleTasks => state.toggle_tasks(),
            CommandEffect::SetMode(mode) => set_mode(mode, state, bridge).await,
            CommandEffect::ShowPlans => show_plans(state, startup),
            CommandEffect::OpenImplement => open_implement(state, startup),
            CommandEffect::Compact => compact(state, bridge).await,
            CommandEffect::ShowProfile { refresh } => {
                show_profile(refresh, state, startup, bridge).await
            }
            CommandEffect::ToggleUsage => state.show_usage = !state.show_usage,
            CommandEffect::ToggleDeveloper => {
                let on = state.toggle_developer();
                state.push_system(if on {
                    "Developer mode on — each tool card now carries its raw call \
                     (the exact name + JSON arguments the agent sent). It shows \
                     under an edit's diff, and when you expand any other tool card."
                } else {
                    "Developer mode off."
                });
            }
            CommandEffect::Quit => state.should_quit = true,
            CommandEffect::ClearHistory => {
                // No "cleared" notice: the returning welcome banner is the
                // feedback, and a notice would suppress it.
                state.clear_transcript();
                state.tasks.clear();
                state.end_implement();
                if let Some(bridge) = bridge {
                    bridge.clear().await;
                }
            }
            CommandEffect::SystemMessage(message) => state.push_system(message),
            CommandEffect::StyledMessage(lines) => state.push_system_styled(lines),
        }
        return;
    }

    // An ordinary message.
    match bridge {
        Some(bridge) => {
            state.push_user(text.clone());
            state.begin_busy();
            if !bridge.send_message(text).await {
                state.end_busy();
                state.push_system("Agent is not available.");
            }
        }
        None => state.push_system("Select a model first (/model)."),
    }
}

/// Draw the active screen.
fn render(frame: &mut Frame, state: &mut AppState) {
    let area = frame.area();
    // Paint the unified dark background under every screen so the whole UI
    // reads as one palette regardless of the host terminal's colours.
    frame.render_widget(
        ratatui::widgets::Block::default()
            .style(ratatui::style::Style::default().bg(crate::theme::BG)),
        area,
    );
    match state.screen {
        Screen::Error => screens::render_error(
            frame,
            area,
            state.error.as_deref().unwrap_or("Unknown error."),
        ),
        Screen::ProjectInit => match &state.project_init {
            Some(init) => screens::project_init::render(frame, area, init),
            // Defensive: shouldn't happen, but never leave a blank screen.
            None => screens::model_select::render(frame, area, &state.model_select, false),
        },
        Screen::ModelSelect => {
            // Mid-session (a model is already chosen) Esc returns to chat; at the
            // initial pick it quits.
            let can_return = state.selected_model.is_some();
            screens::model_select::render(frame, area, &state.model_select, can_return);
            if let Some(id) = &state.pending_provider_remove {
                crate::widgets::confirm_box::render(
                    frame,
                    area,
                    "Remove provider?",
                    &format!("{id} — Enter removes, Esc cancels"),
                );
            }
        }
        Screen::Providers => {
            let onboarding = state.providers_return == Screen::ModelSelect;
            screens::providers::render(frame, area, &state.providers, onboarding);
            if let Some(id) = &state.pending_provider_remove {
                crate::widgets::confirm_box::render(
                    frame,
                    area,
                    "Remove provider?",
                    &format!("{id} — Enter removes, Esc cancels"),
                );
            }
        }
        Screen::ProviderForm => match &state.provider_form {
            Some(form) => screens::provider_form::render(frame, area, form),
            None => {
                let onboarding = state.providers_return == Screen::ModelSelect;
                screens::providers::render(frame, area, &state.providers, onboarding)
            }
        },
        Screen::Permissions => {
            screens::permissions::render(frame, area, &state.permissions);
            if let Some(idx) = state.pending_permission_revoke {
                if let Some(row) = state.permissions.rows().get(idx) {
                    crate::widgets::confirm_box::render(
                        frame,
                        area,
                        "Revoke permission?",
                        &format!("{} — Enter revokes, Esc cancels", row.name),
                    );
                }
            }
        }
        Screen::PlanSelect => screens::plan_select::render(frame, area, &state.plan_select),
        Screen::Chat => {
            screens::chat::render(frame, area, state);
            render_verify_caps(frame, area, state);
        }
        Screen::Diff => {
            if let Some(view) = &state.last_diff {
                screens::diff_screen::render(frame, area, view);
            } else {
                // No diff to show: fall back to the chat screen.
                screens::chat::render(frame, area, state);
            }
        }
    }
}

/// Draw the "Verify capabilities?" overlay on the chat screen when a model
/// awaits verification: a Y/N confirm while idle, or a spinner line while the
/// probe runs.
fn render_verify_caps(frame: &mut Frame, area: ratatui::layout::Rect, state: &AppState) {
    let Some(vc) = &state.verify_caps else {
        return;
    };
    if vc.probing {
        crate::widgets::confirm_box::render(
            frame,
            area,
            "Verifying capabilities…",
            &format!(
                "{} testing {} — sending one short request",
                state.spinner_glyph(),
                vc.entry.model.display_name
            ),
        );
    } else {
        crate::widgets::confirm_box::render(
            frame,
            area,
            "Verify capabilities?",
            &format!(
                "{} — Y runs the tests, N keeps it chat-only",
                vc.entry.model.display_name
            ),
        );
    }
}

/// Read key presses and resizes on a blocking thread and forward them to the
/// loop. Resizes carry no state of their own — the loop just needs to wake up
/// and redraw at the new dimensions.
fn spawn_input_thread(tx: mpsc::Sender<Event>) {
    std::thread::spawn(move || loop {
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                if tx.blocking_send(Event::Key(key)).is_err() {
                    break;
                }
            }
            Ok(resize @ Event::Resize(..)) => {
                if tx.blocking_send(resize).is_err() {
                    break;
                }
            }
            Ok(mouse @ Event::Mouse(..)) => {
                if tx.blocking_send(mouse).is_err() {
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    });
}

/// Enter raw mode + the alternate screen and build the ratatui terminal.
///
/// Where the terminal supports the kitty keyboard protocol, enhanced key
/// reporting is enabled so modified keys (e.g. Shift+Enter on the permission
/// prompt) are distinguishable; elsewhere input behaves as before.
fn init_terminal() -> io::Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Mouse capture routes the scroll wheel to the app as real scroll events,
    // so it scrolls the chat/list instead of the terminal turning the wheel
    // into Up/Down arrows (which would walk the input history).
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    if matches!(supports_keyboard_enhancement(), Ok(true)) {
        let _ = execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    Terminal::new(CrosstermBackend::new(stdout))
}

/// Leave the alternate screen and disable raw mode.
fn restore_terminal(terminal: &mut Term) -> io::Result<()> {
    disable_raw_mode()?;
    // Harmless if enhancement flags were never pushed.
    let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()
}

/// Ensure the terminal is restored even if a panic unwinds past the loop.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        original(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, enabled: bool) -> ProviderEntry {
        ProviderEntry {
            id: id.into(),
            endpoint: format!("http://localhost/{id}"),
            transport: "openai".into(),
            enabled,
            name: None,
            api_key_env: None,
            api_key: None,
        }
    }

    #[test]
    fn upsert_adds_a_new_entry() {
        let mut config = ProviderConfig::default();
        upsert_entry(&mut config, entry("work", true), None);
        assert_eq!(config.providers.len(), 1);
        assert_eq!(config.providers[0].id, "work");
    }

    #[test]
    fn upsert_replaces_same_id_in_place() {
        let mut config = ProviderConfig {
            providers: vec![entry("work", true)],
        };
        let mut updated = entry("work", true);
        updated.endpoint = "https://new.example/v1".into();
        upsert_entry(&mut config, updated, Some("work"));
        assert_eq!(config.providers.len(), 1);
        assert_eq!(config.providers[0].endpoint, "https://new.example/v1");
    }

    #[test]
    fn upsert_renaming_drops_the_old_id() {
        let mut config = ProviderConfig {
            providers: vec![entry("old", true)],
        };
        upsert_entry(&mut config, entry("new", true), Some("old"));
        let ids: Vec<&str> = config.providers.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["new"], "the renamed id replaces the old one");
    }

    #[test]
    fn removing_a_custom_provider_drops_it() {
        let mut config = ProviderConfig {
            providers: vec![entry("ollama", true), entry("work", true)],
        };
        apply_removal(&mut config, "work", None);
        let ids: Vec<&str> = config.providers.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["ollama"], "custom entry is gone");
    }

    #[test]
    fn removing_a_default_disables_it_in_place() {
        let mut config = ProviderConfig {
            providers: vec![entry("ollama", true)],
        };
        apply_removal(&mut config, "ollama", None);
        assert_eq!(config.providers.len(), 1, "the default stays, disabled");
        assert!(!config.providers[0].enabled);
    }

    #[test]
    fn removing_an_unlisted_default_appends_it_disabled() {
        // The default isn't in the file yet (e.g. discovered but never edited);
        // a disabled entry is written from the fallback row so it can be revived.
        let mut config = ProviderConfig::default();
        apply_removal(&mut config, "lmstudio", Some(entry("lmstudio", true)));
        assert_eq!(config.providers.len(), 1);
        assert_eq!(config.providers[0].id, "lmstudio");
        assert!(!config.providers[0].enabled);
    }
}
