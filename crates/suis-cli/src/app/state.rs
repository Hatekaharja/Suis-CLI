//! The application's single source of truth.
//!
//! [`AppState`] holds everything the renderer draws and the input layer
//! mutates: which screen is active, the chat transcript and input buffer, the
//! task list, the model-selection view-model, and any open permission prompt.
//!
//! Business logic lives in suis-agent; this state only *reflects* it. The one
//! piece of behavior here is [`AppState::apply_event`], which folds an
//! [`AgentEvent`] into the transcript/tasks/prompt — pure enough to unit-test
//! without a terminal or a running agent.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use ratatui::layout::Rect;
use tokio::sync::oneshot;

use suis_agent::{AgentEvent, Mode, PermissionDecision, PlanDecision, PlanDraft, Task, TaskStatus};
use suis_core::ProjectConfig;
use suis_providers::Model;

use crate::app::discovery::DiscoveryState;
use crate::commands::parser::COMMANDS;
use crate::prompts::tool_summary;
use crate::screens::diff_screen::DiffView;
use crate::screens::model_select::{ModelEntry, ModelSelect};
use crate::screens::permissions::PermissionsView;
use crate::screens::plan_select::PlanSelect;
use crate::screens::project_init::ProjectInit;
use crate::screens::provider_form::ProviderForm;
use crate::screens::providers::ProvidersView;
use crate::widgets::context_gauge::ContextGauge;
use crate::widgets::message_list::{self, ChatMessage, MsgRole, ToolStatus};
use crate::widgets::permission_prompt::PermissionPrompt;

/// Which screen is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// First-run project initialization (no `.suis/` yet).
    ProjectInit,
    /// Choosing a provider/model.
    ModelSelect,
    /// Enabling/disabling providers (`/providers`).
    Providers,
    /// Adding or editing a provider entry (the form opened from `/providers`).
    ProviderForm,
    /// Viewing/editing stored permissions (`/permissions`).
    Permissions,
    /// Choosing a plan (or one of its steps) for `/implement`.
    PlanSelect,
    /// The main chat interface.
    Chat,
    /// A full-screen view of the most recent edit's diff.
    Diff,
    /// A fatal startup condition (e.g. no providers found).
    Error,
}

/// An open permission prompt plus the channel the decision is returned on.
pub struct PendingPermission {
    /// The dialog model (offered options, danger level).
    pub prompt: PermissionPrompt,
    /// The agent is blocked awaiting a decision on this channel.
    pub sender: oneshot::Sender<PermissionDecision>,
}

/// An open plan-draft review plus the channel the verdict is returned on.
pub struct PendingPlan {
    /// The draft awaiting approval.
    pub draft: PlanDraft,
    /// The agent is blocked awaiting the verdict on this channel.
    pub sender: oneshot::Sender<PlanDecision>,
    /// Vertical scroll offset (top line) of the review overlay, so a long plan
    /// can be read past the popup's height. Clamped to the content by the
    /// renderer, which knows the measured bounds.
    pub scroll_y: u16,
    /// Horizontal scroll offset (left column), so a long step or task line can
    /// be read in a very thin terminal. Clamped to the content by the renderer.
    pub scroll_x: u16,
}

/// An implementation target: the plan step `/implement` runs (or is about to
/// run, while the start is awaiting confirmation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplementState {
    /// The plan's store id.
    pub plan_id: String,
    /// Zero-based step index.
    pub step_index: usize,
    /// The plan (project) title, shown alone in the input-box border.
    pub plan_title: String,
    /// The current step's title, shown in the task panel header.
    pub step_title: String,
}

impl ImplementState {
    /// `plan title · step title`, for the session-start notice.
    pub fn label(&self) -> String {
        format!("{} · {}", self.plan_title, self.step_title)
    }
}

/// Most recent submitted inputs kept for Up/Down recall.
const HISTORY_CAP: usize = 100;

/// Session-scoped recall of submitted inputs (messages and slash commands
/// alike), with shell semantics: Up walks back, Down walks forward, the
/// in-progress draft is stashed when browsing begins and restored when walking
/// past the newest entry, and editing a recalled entry detaches from history.
#[derive(Debug, Default)]
pub struct InputHistory {
    entries: Vec<String>,
    /// Index into `entries` while browsing; `None` while on the live draft.
    cursor: Option<usize>,
    /// The in-progress draft, stashed when browsing begins.
    stash: String,
}

impl InputHistory {
    /// Record a submitted input. Consecutive duplicates collapse, the ring is
    /// capped at [`HISTORY_CAP`], and any browse position resets.
    pub fn push(&mut self, entry: &str) {
        if !entry.is_empty() && self.entries.last().map(String::as_str) != Some(entry) {
            self.entries.push(entry.to_string());
            if self.entries.len() > HISTORY_CAP {
                self.entries.remove(0);
            }
        }
        self.cursor = None;
        self.stash.clear();
    }

    /// Walk back one entry, stashing `current` when browsing begins. `None`
    /// when there is nothing further back.
    pub fn up(&mut self, current: &str) -> Option<String> {
        let next = match self.cursor {
            None if self.entries.is_empty() => return None,
            None => {
                self.stash = current.to_string();
                self.entries.len() - 1
            }
            Some(0) => return None,
            Some(i) => i - 1,
        };
        self.cursor = Some(next);
        Some(self.entries[next].clone())
    }

    /// Walk forward one entry; past the newest, restore the stashed draft.
    /// `None` when not browsing.
    pub fn down(&mut self) -> Option<String> {
        let i = self.cursor?;
        if i + 1 < self.entries.len() {
            self.cursor = Some(i + 1);
            Some(self.entries[i + 1].clone())
        } else {
            self.cursor = None;
            Some(std::mem::take(&mut self.stash))
        }
    }

    /// Leave browse mode without restoring the stash (the recalled entry was
    /// edited, so it is the live draft now).
    pub fn detach(&mut self) {
        self.cursor = None;
    }
}

/// The busy spinner's glyph cycle.
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Boar-flavoured verbs for the busy line (in place of a plain "working"), one
/// picked per turn so it stays stable while the agent works but varies between
/// messages. All five are things a boar actually does while nosing about.
const WORK_VERBS: &[&str] = &["truffling", "rootling", "snuffling", "foraging", "grubbing"];

/// Pick a [`WORK_VERBS`] entry at random for a fresh busy spell. Uses std's
/// per-instance `RandomState` seed as the entropy source, so no `rand`
/// dependency is pulled in just for a bit of flavour.
fn random_work_verb() -> &'static str {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u8(0);
    WORK_VERBS[hasher.finish() as usize % WORK_VERBS.len()]
}

/// The session-stable key identifying a (provider, model) pair for the
/// declined-probe set (20.1).
pub fn caps_consent_key(provider_id: &str, model_id: &str) -> String {
    format!("{provider_id}/{model_id}")
}

/// The capability-verification prompt shown on the chat screen right after an
/// unverified model is selected. While [`probing`](Self::probing) the popup
/// shows a spinner instead of the Y/N choice and swallows input until the probe
/// returns; on completion the model's real capabilities replace its assumed
/// chat-only default and the agent is rebuilt with them.
pub struct VerifyCaps {
    /// The model being verified — the same entry that was just selected.
    pub entry: ModelEntry,
    /// True once the user pressed Y and the background probe is running.
    pub probing: bool,
}

/// A running per-provider tally of tokens spent this session, split into what
/// was sent (prompt) and received (completion).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderUsage {
    /// Prompt tokens sent across every turn this session.
    pub sent: usize,
    /// Completion tokens received across every turn this session.
    pub received: usize,
}

impl ProviderUsage {
    /// Total tokens (sent + received), as shown in the footer.
    pub fn total(&self) -> usize {
        self.sent + self.received
    }
}

/// A live preview of the request currently in flight, so the context gauge and
/// session total reflect the turn before the provider's final counts land. It is
/// *armed* only when the provider reports the exact prompt size mid-stream
/// ([`AgentEvent::PromptTokens`] — Anthropic, at `message_start`), which floors
/// the context gauge at the real input from the first chunk; the chars/4 guess at
/// the prompt overshoots the real count by ~1–2k, so providers that don't report
/// early (OpenAI, Ollama) get no preview and their gauge settles exactly at turn
/// end. `prompt` holds that reported value; `completion_chars` accumulates
/// streamed characters, converted to a token estimate (chars/4) on read so many
/// tiny chunks don't each round down to zero. When the provider's final counts
/// arrive via [`AgentEvent::TokenUsage`] the preview is dropped for the exact
/// numbers; if the turn ends without a final usage (e.g. interrupted), the
/// preview is folded into the session total instead so it isn't lost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiveTurn {
    /// The exact prompt size the provider reported mid-stream (system prompt,
    /// history, tools, and the new message). Zero until
    /// [`AgentEvent::PromptTokens`] arms the preview.
    pub prompt: usize,
    /// Streamed completion characters so far this request.
    pub completion_chars: usize,
}

impl LiveTurn {
    /// The estimated completion tokens streamed so far.
    fn completion_estimate(&self) -> usize {
        self.completion_chars / 4
    }

    /// The preview split into sent (prompt) and received (completion) tokens,
    /// for folding into the running totals when a turn ends without a final
    /// usage report.
    fn as_usage(&self) -> ProviderUsage {
        ProviderUsage {
            sent: self.prompt,
            received: self.completion_estimate(),
        }
    }
}

/// The whole UI state.
pub struct AppState {
    /// The active screen.
    pub screen: Screen,
    /// Set when the app should exit the event loop.
    pub should_quit: bool,
    /// A fatal message shown on [`Screen::Error`].
    pub error: Option<String>,

    /// The interactive project-init flow, active on [`Screen::ProjectInit`].
    pub project_init: Option<ProjectInit>,
    /// The resolved project configuration (loaded at startup, produced by the
    /// init flow, or a default). `None` only before either has happened.
    pub project: Option<ProjectConfig>,

    /// Model-selection view-model.
    pub model_select: ModelSelect,
    /// Live provider-discovery state for the session: every provider that was
    /// (or is being) probed and its current status. The single source of truth
    /// the `/providers` screen and the add-provider form rebuild from, kept in
    /// step as background probe outcomes stream in.
    pub discovery: DiscoveryState,
    /// Provider enable/disable view-model, populated when `/providers` opens.
    pub providers: ProvidersView,
    /// The add/edit provider form, present while [`Screen::ProviderForm`] is
    /// active.
    pub provider_form: Option<ProviderForm>,
    /// Where the provider form returns on save or cancel. Usually
    /// [`Screen::Providers`] (opened from `/providers`), but the model-select
    /// "add a provider" row opens it pointing back at [`Screen::ModelSelect`].
    pub provider_form_return: Screen,
    /// Where the providers screen returns on Esc. [`Screen::Chat`] for the
    /// `/providers` command; [`Screen::ModelSelect`] during first-run
    /// onboarding, where it is shown before the model picker.
    pub providers_return: Screen,
    /// A provider id staged for removal, awaiting the confirm overlay's verdict.
    pub pending_provider_remove: Option<String>,
    /// Stored-permissions view-model, populated when `/permissions` opens.
    pub permissions: PermissionsView,
    /// The row index staged for revocation, awaiting the confirm overlay's verdict.
    pub pending_permission_revoke: Option<usize>,
    /// The capability-verification prompt, shown over the chat screen after an
    /// unverified model is selected: "Verify capabilities? Y/N", then a spinner
    /// while the probe runs. Owns input until answered (and while probing).
    pub verify_caps: Option<VerifyCaps>,
    /// Consent keys (`provider_id/model_id`) the user declined to probe this
    /// session, so a declined model is not re-asked on the next selection.
    pub declined_caps: HashSet<String>,
    /// The model chosen for the chat session, once selected.
    pub selected_model: Option<Model>,
    /// Display name of the selected model's provider, for the welcome banner
    /// and the connected notice.
    pub provider_name: Option<String>,
    /// The workspace root path, for the welcome banner.
    pub workspace_root: Option<String>,
    /// A one-line `connected · model @ provider` notice, armed on model
    /// selection (and re-armed on clear) and flushed into the transcript just
    /// before the next user message — so session identity survives once the
    /// welcome banner is replaced by conversation.
    pub session_notice: Option<String>,

    /// The chat transcript.
    pub messages: Vec<ChatMessage>,
    /// The input buffer.
    pub input: String,
    /// Caret position within the input buffer, in characters (not bytes).
    pub input_cursor: usize,
    /// Up/Down recall of previously submitted inputs.
    pub history: InputHistory,
    /// The command palette was dismissed with Esc; cleared by the next edit
    /// so typing reopens it.
    pub palette_dismissed: bool,
    /// Selected row within the palette's filtered matches.
    pub palette_selected: usize,
    /// Vertical scroll offset (lines from the top) of the transcript. Only
    /// consulted when [`AppState::follow_bottom`] is false; while following,
    /// the effective offset is recomputed each frame to pin the view to the
    /// newest line.
    pub scroll: u16,
    /// Whether the transcript is stuck to the bottom. True by default and
    /// whenever the user scrolls all the way down; cleared the moment they
    /// scroll up. While true, new/streamed content keeps the latest line in
    /// view; while false, the view stays put as content arrives.
    pub follow_bottom: bool,
    /// Wrapped height of the transcript at the last render, in rows. Updated by
    /// the renderer so input handling has fresh bounds for the scroll math.
    pub content_height: u16,
    /// Height of the transcript's visible area at the last render, in rows.
    pub viewport_height: u16,
    /// The transcript's inner content rectangle (inside the border/padding) at
    /// the last render, recorded so a mouse click can be mapped to the card it
    /// landed on. `None` until the chat screen has drawn at least once.
    pub transcript_area: Option<Rect>,
    /// When the current thinking block began streaming, for the "Thought for
    /// Ns" elapsed shown once it is finalized. `None` between thinking blocks.
    pub thinking_since: Option<Instant>,
    /// True while the agent is processing a turn.
    pub busy: bool,
    /// True once the user pressed Esc to interrupt the running turn; cleared
    /// when the turn ends. Gates repeat sends and switches the busy hint.
    pub interrupting: bool,
    /// When the running turn started, for the busy hint's elapsed seconds.
    pub busy_since: Option<Instant>,
    /// The tool currently executing, shown in the busy hint.
    pub current_tool: Option<String>,
    /// This turn's boar verb for the busy line (e.g. "snuffling"), chosen at
    /// [`begin_busy`](Self::begin_busy) so it holds steady while the agent works.
    pub busy_verb: &'static str,
    /// Frame index of the busy spinner, advanced by the UI tick.
    pub spinner_frame: usize,
    /// The session's runtime mode, mirrored for rendering (the input-box
    /// label) and kept in sync with the agent's [`Session`] via the bridge.
    pub mode: Mode,

    /// Session tasks, mirrored from the agent.
    pub tasks: Vec<Task>,
    /// Whether the task panel is shown.
    pub show_tasks: bool,
    /// Whether the user has explicitly asked for the panel via `/tasks`. The
    /// narrow popup suppresses its empty state until then (so it doesn't cover
    /// the chat at startup), but an explicit toggle overrides that.
    pub tasks_explicit: bool,
    /// Whether the task panel was actually drawn on the last frame, recorded by
    /// the renderer. Lets `/tasks` toggle on real visibility — so one press
    /// shows it and the next hides it — regardless of the docked/popup layout.
    pub tasks_visible: bool,

    /// An open permission prompt, if the agent is awaiting a decision.
    pub pending_permission: Option<PendingPermission>,
    /// An open plan-draft review, if the agent is awaiting the verdict.
    pub pending_plan: Option<PendingPlan>,

    /// Plan-selection view-model, populated when `/implement` opens.
    pub plan_select: PlanSelect,
    /// A selected implementation target awaiting the user's go-ahead (the
    /// "conversation will be cleared" confirmation).
    pub pending_implement: Option<ImplementState>,
    /// The running implementation session's target, if any.
    pub implement: Option<ImplementState>,
    /// Whether the begin-verification prompt is currently open.
    pub pending_verify: bool,
    /// Whether the begin-verification prompt has already been offered for the
    /// current step (so it is asked exactly once).
    pub verify_prompted: bool,
    /// A completed step with a next step available, awaiting the user's
    /// confirmation to continue ("Continue to next Step?" popup).
    pub pending_next_step: Option<ImplementState>,
    /// Whether the "continue to next step" prompt has already been shown for
    /// the current step. Prevents re-prompting if the user declines.
    pub next_step_prompted: bool,

    /// The most recent edit's diff, viewable full-screen with Ctrl+D.
    pub last_diff: Option<DiffView>,

    /// The latest context-usage snapshot, shown as the pressure gauge in the
    /// input-box border. `None` until the first turn reports usage.
    pub context: Option<ContextGauge>,
    /// Running token totals for this program run, keyed by provider name. Fed by
    /// real provider usage; the footer shows the active provider's total and
    /// `/usage` shows the full per-provider sent/received breakdown. Persists
    /// across `/clear` (it is a session total, not a per-chat one).
    pub session_usage: HashMap<String, ProviderUsage>,
    /// Estimated usage for the request currently streaming, added on top of the
    /// committed totals so they climb live. `None` between turns. See
    /// [`LiveTurn`].
    pub live_turn: Option<LiveTurn>,
    /// Whether the `/usage` detail popup is open.
    pub show_usage: bool,
    /// A transient advisory message shown as a dismissible popup (e.g. an API
    /// key being sent over plaintext http). `None` when nothing is pending; any
    /// key dismisses it. Set when a session is spawned.
    pub notice: Option<String>,
    /// True while a `/compact` is in flight, so the UI can show "Compacting…".
    pub compacting: bool,
    /// Developer mode (`/developer`): when on, every tool card shows its raw
    /// call — the exact name and JSON arguments the agent sent — for debugging
    /// what a tool was actually asked to do. Off by default.
    pub developer: bool,
}

impl AppState {
    /// A fresh state showing the model-selection screen.
    pub fn new(model_select: ModelSelect) -> Self {
        AppState {
            screen: Screen::ModelSelect,
            should_quit: false,
            error: None,
            project_init: None,
            project: None,
            model_select,
            discovery: DiscoveryState::default(),
            providers: ProvidersView::default(),
            provider_form: None,
            provider_form_return: Screen::Providers,
            providers_return: Screen::Chat,
            pending_provider_remove: None,
            permissions: PermissionsView::default(),
            pending_permission_revoke: None,
            verify_caps: None,
            declined_caps: HashSet::new(),
            selected_model: None,
            provider_name: None,
            workspace_root: None,
            session_notice: None,
            messages: Vec::new(),
            input: String::new(),
            input_cursor: 0,
            history: InputHistory::default(),
            palette_dismissed: false,
            palette_selected: 0,
            scroll: 0,
            follow_bottom: true,
            content_height: 0,
            viewport_height: 0,
            transcript_area: None,
            thinking_since: None,
            busy: false,
            interrupting: false,
            busy_since: None,
            current_tool: None,
            busy_verb: WORK_VERBS[0],
            spinner_frame: 0,
            mode: Mode::default(),
            tasks: Vec::new(),
            show_tasks: true,
            tasks_explicit: false,
            tasks_visible: false,
            pending_permission: None,
            pending_plan: None,
            plan_select: PlanSelect::default(),
            pending_implement: None,
            implement: None,
            pending_verify: false,
            verify_prompted: false,
            pending_next_step: None,
            next_step_prompted: false,
            last_diff: None,
            context: None,
            session_usage: HashMap::new(),
            live_turn: None,
            show_usage: false,
            notice: None,
            compacting: false,
            developer: false,
        }
    }

    /// A state that opens straight onto the error screen.
    pub fn error(message: impl Into<String>) -> Self {
        let mut state = AppState::new(ModelSelect::default());
        state.screen = Screen::Error;
        state.error = Some(message.into());
        state
    }

    /// Whether a permission prompt is currently blocking the agent.
    pub fn awaiting_permission(&self) -> bool {
        self.pending_permission.is_some()
    }

    /// Whether selecting `entry` should offer to verify its capabilities: it is
    /// an unverified model (capabilities are no longer probed at startup) and
    /// the user has not already declined it this session. Already-verified
    /// (advertised) models select straight through.
    pub fn needs_caps_consent(&self, entry: &ModelEntry) -> bool {
        entry.caps_unknown()
            && !self
                .declined_caps
                .contains(&caps_consent_key(&entry.provider_id, &entry.model.model_id))
    }

    /// Whether the capability-verification probe is currently running, so the
    /// popup shows a spinner and the spinner keeps ticking.
    pub fn verify_caps_probing(&self) -> bool {
        self.verify_caps.as_ref().is_some_and(|v| v.probing)
    }

    // --- input editing ---------------------------------------------------

    /// The byte offset of the character cursor, for `String` edits.
    fn cursor_byte(&self) -> usize {
        self.input
            .char_indices()
            .nth(self.input_cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len())
    }

    /// Note that the buffer was edited: a recalled history entry detaches
    /// (shell semantics) and the palette re-derives from scratch.
    fn note_edit(&mut self) {
        self.history.detach();
        self.palette_dismissed = false;
        self.palette_selected = 0;
    }

    /// Insert a character at the cursor.
    pub fn push_char(&mut self, c: char) {
        let at = self.cursor_byte();
        self.input.insert(at, c);
        self.input_cursor += 1;
        self.note_edit();
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        self.input_cursor -= 1;
        let at = self.cursor_byte();
        self.input.remove(at);
        self.note_edit();
    }

    /// Take the input buffer, leaving it empty.
    pub fn take_input(&mut self) -> String {
        self.input_cursor = 0;
        self.palette_dismissed = false;
        self.palette_selected = 0;
        std::mem::take(&mut self.input)
    }

    /// Replace the buffer, placing the cursor at the end.
    pub fn set_input(&mut self, text: impl Into<String>) {
        self.input = text.into();
        self.input_cursor = self.input.chars().count();
    }

    /// Clear the buffer (Esc).
    pub fn clear_input(&mut self) {
        self.input.clear();
        self.input_cursor = 0;
        self.palette_dismissed = false;
        self.palette_selected = 0;
    }

    /// Move the cursor one character left; at a line start this lands on the
    /// previous line's end (the cursor is linear over the buffer).
    pub fn cursor_left(&mut self) {
        self.input_cursor = self.input_cursor.saturating_sub(1);
    }

    /// Move the cursor one character right.
    pub fn cursor_right(&mut self) {
        self.input_cursor = (self.input_cursor + 1).min(self.input.chars().count());
    }

    /// Move the cursor to the start of the current logical line.
    pub fn cursor_home(&mut self) {
        let before: Vec<char> = self.input.chars().take(self.input_cursor).collect();
        self.input_cursor = before
            .iter()
            .rposition(|&c| c == '\n')
            .map(|i| i + 1)
            .unwrap_or(0);
    }

    /// Move the cursor to the end of the current logical line.
    pub fn cursor_end(&mut self) {
        let rest = self.input.chars().skip(self.input_cursor);
        let to_newline = rest.take_while(|&c| c != '\n').count();
        self.input_cursor += to_newline;
    }

    // --- input history -----------------------------------------------------

    /// Up: recall the previous submitted input into the buffer.
    pub fn history_up(&mut self) {
        if let Some(text) = self.history.up(&self.input.clone()) {
            self.set_recalled(text);
        }
    }

    /// Down: walk forward through history, restoring the stashed draft past
    /// the newest entry.
    pub fn history_down(&mut self) {
        if let Some(text) = self.history.down() {
            self.set_recalled(text);
        }
    }

    /// Install a history entry as the buffer. The palette stays dismissed so
    /// a recalled command does not pop it open mid-recall.
    fn set_recalled(&mut self, text: String) {
        self.set_input(text);
        self.palette_dismissed = true;
        self.palette_selected = 0;
    }

    // --- command palette ---------------------------------------------------

    /// The command-name prefix typed so far, when the buffer is a bare `/name`
    /// (no arguments yet). `None` otherwise.
    fn palette_query(&self) -> Option<&str> {
        let rest = self.input.trim_start().strip_prefix('/')?;
        if rest.contains(char::is_whitespace) {
            return None;
        }
        Some(rest)
    }

    /// The commands matching the typed prefix, in `COMMANDS` order.
    pub fn palette_matches(&self) -> Vec<(&'static str, &'static str)> {
        match self.palette_query() {
            Some(prefix) => COMMANDS
                .iter()
                .filter(|(name, _)| name.starts_with(prefix))
                .copied()
                .collect(),
            None => Vec::new(),
        }
    }

    /// Whether the palette is showing: a command prefix with matches, not
    /// dismissed, and no modal overlay open (overlays suppress it entirely).
    pub fn palette_open(&self) -> bool {
        !self.palette_dismissed
            && !self.awaiting_permission()
            && self.pending_plan.is_none()
            && self.pending_implement.is_none()
            && !self.pending_verify
            && self.pending_next_step.is_none()
            && !self.palette_matches().is_empty()
    }

    /// Move the palette selection by `delta`, wrapping at both ends.
    pub fn palette_move(&mut self, delta: isize) {
        let len = self.palette_matches().len() as isize;
        if len == 0 {
            return;
        }
        let cur = (self.palette_selected as isize).min(len - 1);
        self.palette_selected = ((cur + delta).rem_euclid(len)) as usize;
    }

    /// The name of the currently selected palette row, if any.
    pub fn palette_selected_name(&self) -> Option<&'static str> {
        let matches = self.palette_matches();
        if matches.is_empty() {
            return None;
        }
        let idx = self.palette_selected.min(matches.len() - 1);
        Some(matches[idx].0)
    }

    /// Dismiss the palette (Esc) without touching the buffer; the next edit
    /// reopens it.
    pub fn dismiss_palette(&mut self) {
        self.palette_dismissed = true;
    }

    // --- busy pulse ----------------------------------------------------------

    /// Mark the start of a turn: busy, with the elapsed clock running and a
    /// fresh boar verb chosen for this spell's busy line.
    pub fn begin_busy(&mut self) {
        self.busy = true;
        self.busy_since = Some(Instant::now());
        self.busy_verb = random_work_verb();
    }

    /// Mark the turn over, clearing the busy clock, tool, and any pending
    /// interrupt.
    pub fn end_busy(&mut self) {
        self.busy = false;
        self.interrupting = false;
        self.busy_since = None;
        self.current_tool = None;
    }

    /// Advance the spinner on a UI tick. Returns whether anything changed
    /// (i.e. a redraw is warranted); idle sessions hold still.
    pub fn on_tick(&mut self) -> bool {
        if self.busy || self.compacting || self.verify_caps_probing() {
            self.spinner_frame = self.spinner_frame.wrapping_add(1);
            true
        } else {
            false
        }
    }

    /// The current spinner glyph, for overlays (like the capability-verification
    /// popup) that animate while a background task runs.
    pub fn spinner_glyph(&self) -> &'static str {
        SPINNER[self.spinner_frame % SPINNER.len()]
    }

    /// The input-border hint while the agent works: spinner, elapsed seconds,
    /// and the running tool — `None` when idle.
    pub fn busy_hint(&self) -> Option<String> {
        if !self.busy && !self.compacting {
            return None;
        }
        let spinner = SPINNER[self.spinner_frame % SPINNER.len()];
        if self.interrupting {
            return Some(format!("{spinner} interrupting…"));
        }
        if self.compacting {
            return Some(format!("{spinner} compacting the conversation…"));
        }
        let elapsed = self.busy_since.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        // A boar verb (this turn's pick) in place of a plain "working", to match
        // Suis' theming. Kept lowercase like its siblings ("interrupting…",
        // "compacting…").
        let verb = self.busy_verb;
        Some(match &self.current_tool {
            Some(tool) => format!("{spinner} {verb} · {tool} · {elapsed}s · Esc to interrupt"),
            None => format!("{spinner} {verb} · {elapsed}s · Esc to interrupt"),
        })
    }

    // --- transcript helpers ---------------------------------------------

    /// Push a user message into the transcript. Sending snaps the view back to
    /// the bottom so the user always sees their own message and the reply. An
    /// armed connected notice lands first, so identity opens the conversation.
    pub fn push_user(&mut self, text: impl Into<String>) {
        if let Some(notice) = self.session_notice.take() {
            self.push_system(notice);
        }
        self.messages.push(ChatMessage::new(MsgRole::User, text));
        self.follow_bottom = true;
    }

    /// Push a local system notice into the transcript.
    pub fn push_system(&mut self, text: impl Into<String>) {
        self.messages.push(ChatMessage::new(MsgRole::System, text));
    }

    /// Push a pre-styled system notice (aligned command output) into the
    /// transcript.
    pub fn push_system_styled(&mut self, lines: Vec<ratatui::text::Line<'static>>) {
        self.messages.push(ChatMessage::system_styled(lines));
    }

    /// Clear the transcript (mirrors the agent-side history clear). The context
    /// gauge is reset too — it refreshes on the next turn — and the connected
    /// notice is re-armed so identity opens the next conversation.
    pub fn clear_transcript(&mut self) {
        self.messages.clear();
        self.scroll = 0;
        self.follow_bottom = true;
        self.context = None;
        self.live_turn = None;
        self.thinking_since = None;
        self.arm_session_notice();
    }

    /// Tokens currently occupying the agent's context: the size of the last
    /// assembled request (the context gauge's numerator). This is the live agent
    /// context, re-measured every turn — and reset to the lean seed on each
    /// per-task reset in an implementation session — not a cumulative tally. The
    /// input-box border renders this beside the percentage; `None` hides it. For
    /// cumulative spend see [`Self::active_session_total`].
    pub fn context_tokens(&self) -> Option<usize> {
        self.context.map(|c| c.used).filter(|&used| used > 0)
    }

    /// Settle the in-flight live estimate at turn end. A reporting provider has
    /// already cleared it via [`AgentEvent::TokenUsage`], so this is a no-op
    /// there; for one that never reports usage, the estimate is committed to the
    /// session total so the count holds steady instead of dropping back.
    fn settle_live_turn(&mut self) {
        let Some(live) = self.live_turn.take() else {
            return;
        };
        let usage = live.as_usage();
        let provider = self
            .provider_name
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let entry = self.session_usage.entry(provider).or_default();
        entry.sent += usage.sent;
        entry.received += usage.received;
    }

    /// The active provider's running session token total: exact committed
    /// counts only, with no in-flight estimate, so the figure is always the
    /// real number of tokens processed. It therefore moves in discrete steps at
    /// each turn's end (when the provider's counts land), not continuously while
    /// a response streams. The footer renders this; `None` hides it.
    pub fn active_session_total(&self) -> Option<usize> {
        let provider = self.provider_name.as_deref()?;
        let committed = self
            .session_usage
            .get(provider)
            .map(ProviderUsage::total)
            .unwrap_or(0);
        Some(committed).filter(|&total| total > 0)
    }

    /// (Re)arm the one-line connected notice from the current model/provider
    /// identity. A no-op until a model has been selected.
    pub fn arm_session_notice(&mut self) {
        if let (Some(model), Some(provider)) = (&self.selected_model, &self.provider_name) {
            self.session_notice =
                Some(format!("connected · {} @ {}", model.display_name, provider));
        }
    }

    /// Flip developer mode (`/developer`) and apply it to every tool card
    /// already in the transcript, so toggling reveals (or hides) the raw call on
    /// past tool uses too, not just future ones. Returns the new state.
    pub fn toggle_developer(&mut self) -> bool {
        self.developer = !self.developer;
        for msg in &mut self.messages {
            if let Some(card) = msg.tool.as_mut() {
                card.show_raw = self.developer;
            }
        }
        self.developer
    }

    /// Toggle full-output expansion on the most recent completed tool card.
    pub fn toggle_last_tool_card(&mut self) {
        if let Some(card) = self
            .messages
            .iter_mut()
            .rev()
            .filter_map(|m| m.tool.as_mut())
            .find(|c| c.status != ToolStatus::Running)
        {
            card.expanded = !card.expanded;
        }
    }

    /// The largest meaningful top offset: scrolling past it would only reveal
    /// blank space below the final line. Computed from the last render's
    /// measurements.
    fn max_scroll(&self) -> u16 {
        self.content_height.saturating_sub(self.viewport_height)
    }

    /// The top offset to render at this frame: the bottom while following,
    /// otherwise the user's offset clamped to the content.
    pub fn effective_scroll(&self) -> u16 {
        if self.follow_bottom {
            self.max_scroll()
        } else {
            self.scroll.min(self.max_scroll())
        }
    }

    /// Record the transcript's wrapped height and visible height for this
    /// frame, so subsequent scroll input is bounded by real measurements.
    pub fn note_transcript_metrics(&mut self, content_height: u16, viewport_height: u16) {
        self.content_height = content_height;
        self.viewport_height = viewport_height;
    }

    /// Whether the view is detached from the bottom with content actually
    /// scrollable — this drives the scrollbar and the "new output below"
    /// notice; a bottom-pinned view renders neither.
    pub fn is_detached(&self) -> bool {
        !self.follow_bottom && self.content_height > self.viewport_height
    }

    /// Whether transcript content is still arriving (the agent is mid-turn,
    /// streaming text or producing tool output).
    pub fn is_streaming(&self) -> bool {
        self.busy
    }

    /// Scroll the transcript up one line, detaching from the bottom. When
    /// following, the offset first resolves to the current bottom so the first
    /// press nudges up by exactly one line.
    pub fn scroll_up(&mut self) {
        self.scroll = self.effective_scroll().saturating_sub(1);
        self.follow_bottom = false;
    }

    /// Scroll the transcript down one line, never past the bottom. Reaching the
    /// bottom re-attaches the view so future content keeps following.
    pub fn scroll_down(&mut self) {
        let max = self.max_scroll();
        let next = self.effective_scroll().saturating_add(1).min(max);
        self.scroll = next;
        self.follow_bottom = next >= max;
    }

    /// Whether the transcript holds an actual conversation (user, agent, or
    /// tool messages — system notices alone don't count). Gates `/implement`'s
    /// "conversation will be cleared" confirmation.
    pub fn has_conversation(&self) -> bool {
        self.messages
            .iter()
            .any(|m| !matches!(m.role, MsgRole::System))
    }

    // --- task panel ------------------------------------------------------

    /// Toggle the task panel from `/tasks`. Keyed off the panel's *actual*
    /// last-frame visibility rather than `show_tasks` alone, so one press always
    /// flips what the user sees — even in a narrow terminal where the empty
    /// panel is otherwise suppressed. Turning it on marks the request explicit,
    /// which lets the narrow popup show even with no tasks yet.
    pub fn toggle_tasks(&mut self) {
        if self.tasks_visible {
            self.show_tasks = false;
        } else {
            self.show_tasks = true;
            self.tasks_explicit = true;
        }
    }

    /// Whether the narrow centered popup should be drawn: the panel is on and
    /// there is either something to show or an explicit request to show it.
    pub fn show_tasks_popup(&self) -> bool {
        self.show_tasks && (!self.tasks.is_empty() || self.tasks_explicit)
    }

    // --- permission prompt ----------------------------------------------

    /// Answer (and dismiss) the open permission prompt, unblocking the agent.
    pub fn answer_permission(&mut self, decision: PermissionDecision) {
        if let Some(pending) = self.pending_permission.take() {
            let _ = pending.sender.send(decision);
        }
    }

    /// Answer (and dismiss) the open plan review, unblocking the agent.
    pub fn answer_plan(&mut self, decision: PlanDecision) {
        if let Some(pending) = self.pending_plan.take() {
            let _ = pending.sender.send(decision);
        }
    }

    /// Scroll the open plan review vertically by `delta` lines (negative = up).
    /// The offset is clamped to the content by the renderer, so over-scrolling
    /// settles at the last screenful rather than running off.
    pub fn scroll_plan_v(&mut self, delta: i16) {
        if let Some(plan) = &mut self.pending_plan {
            plan.scroll_y = plan.scroll_y.saturating_add_signed(delta);
        }
    }

    /// Scroll the open plan review horizontally by `delta` columns (negative =
    /// left), for reading long lines in a thin terminal.
    pub fn scroll_plan_h(&mut self, delta: i16) {
        if let Some(plan) = &mut self.pending_plan {
            plan.scroll_x = plan.scroll_x.saturating_add_signed(delta);
        }
    }

    // --- implementation sessions ------------------------------------------

    /// Reset the UI into a fresh implementation session for `target`: clear
    /// the transcript and tasks, force Agent mode, and mark the session busy
    /// (the work package is the opening turn). The caller sends the matching
    /// bridge message.
    pub fn begin_implement(&mut self, target: ImplementState) {
        self.clear_transcript();
        self.tasks.clear();
        self.mode = Mode::Agent;
        self.pending_verify = false;
        self.verify_prompted = false;
        self.pending_next_step = None;
        self.next_step_prompted = false;
        self.push_system(format!("Implementation session: {}", target.label()));
        self.begin_busy();
        self.implement = Some(target);
    }

    /// Leave any implementation session (on `/clear` or a new model).
    pub fn end_implement(&mut self) {
        self.implement = None;
        self.pending_implement = None;
        self.pending_verify = false;
        self.verify_prompted = false;
        self.pending_next_step = None;
        self.next_step_prompted = false;
    }

    // --- agent events ----------------------------------------------------

    /// Fold an [`AgentEvent`] into the UI state.
    pub fn apply_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::StreamChunk(chunk) => {
                // Grow the in-flight estimate so the token totals climb live.
                if let Some(live) = &mut self.live_turn {
                    live.completion_chars += chunk.chars().count();
                }
                self.append_stream(&chunk);
            }
            AgentEvent::ReasoningChunk(chunk) => {
                self.append_reasoning(&chunk);
            }
            AgentEvent::ToolCallStarted { name, args } => {
                self.finish_stream();
                self.current_tool = Some(name.clone());
                let subject = tool_summary::subject(&name, &args);
                let mut msg = ChatMessage::tool_running(name.clone(), subject);
                if let Some(card) = msg.tool.as_mut() {
                    // Capture the exact call now; whether it is shown is the
                    // developer flag's call, applied here and on `/developer`.
                    card.raw_call = format_raw_call(&name, &args);
                    card.show_raw = self.developer;
                }
                self.messages.push(msg);
            }
            AgentEvent::ToolCallCompleted { result } => {
                self.finish_stream();
                self.current_tool = None;
                if !result.is_error {
                    if let Some(diff) = parse_edit_diff(&result.content) {
                        self.last_diff = Some(diff);
                    }
                }
                let status = if result.is_error {
                    ToolStatus::Error
                } else {
                    ToolStatus::Ok
                };
                // Tools execute sequentially, so the oldest running card is the
                // one this result answers — it updates in place.
                match self
                    .messages
                    .iter_mut()
                    .find_map(|m| m.tool.as_mut().filter(|c| c.status == ToolStatus::Running))
                {
                    Some(card) => {
                        card.status = status;
                        card.output = result.content;
                    }
                    None => {
                        // Defensive: a completion with no running card still
                        // lands as a completed card rather than dropping output.
                        let mut msg = ChatMessage::tool_running("tool", "");
                        let card = msg.tool.as_mut().expect("tool_running sets the card");
                        card.status = status;
                        card.output = result.content;
                        self.messages.push(msg);
                    }
                }
            }
            AgentEvent::PermissionRequest { action, sender } => {
                self.pending_permission = Some(PendingPermission {
                    prompt: PermissionPrompt::new(action),
                    sender,
                });
            }
            AgentEvent::PlanProposal { draft, sender } => {
                self.pending_plan = Some(PendingPlan {
                    draft,
                    sender,
                    scroll_y: 0,
                    scroll_x: 0,
                });
            }
            AgentEvent::TaskUpdated(tasks) => {
                let was_step_done = self.implement.is_some() && step_done(&self.tasks);
                self.tasks = tasks;
                // Task changes no longer force the panel open — the user toggles
                // it themselves, so updates don't repeatedly pop the popup.
                if self.implement.is_some() && !was_step_done && step_done(&self.tasks) {
                    self.push_system("✓ Step complete: every work and verify task is done.");
                }
            }
            AgentEvent::ContextUsage {
                used_tokens,
                budget,
                pruned,
            } => {
                self.context = Some(ContextGauge::new(used_tokens, budget, pruned));
                // Do not seed a chars/4 prompt estimate here: it overshoots the
                // real count by ~1–2k. The live preview is armed only when a
                // provider reports the exact prompt mid-stream (PromptTokens,
                // Anthropic); providers that don't (OpenAI, Ollama) show no live
                // estimate and their total commits exactly with TokenUsage.
                self.live_turn = None;
            }
            AgentEvent::PromptTokens { prompt_tokens } => {
                // The provider reported the exact prompt size mid-stream
                // (Anthropic, at message_start). Arm the live preview with it so
                // the in-flight chat total and the context gauge show real input
                // from the first chunk; the streamed completion grows on top. The
                // gauge is floored at its estimate for parity with TokenUsage, so
                // a delta-reporting backend can never shrink it.
                let completion_chars = self.live_turn.map_or(0, |l| l.completion_chars);
                self.live_turn = Some(LiveTurn {
                    prompt: prompt_tokens,
                    completion_chars,
                });
                if let Some(gauge) = self.context {
                    let used = prompt_tokens.max(gauge.used);
                    self.context = Some(ContextGauge::new(used, gauge.budget, gauge.pruned));
                }
            }
            AgentEvent::TokenUsage {
                prompt_tokens,
                completion_tokens,
            } => {
                // The provider's real prompt size is the live context occupancy,
                // but some backends (Ollama) report only the *newly evaluated*
                // tokens per turn thanks to prompt caching — which would collapse
                // the gauge after the first message. Floor it at the current
                // estimate (set by the preceding ContextUsage), which always
                // reflects the whole assembled conversation, so the number tracks
                // the full chat on every provider.
                let (estimate, budget, pruned) = self
                    .context
                    .map(|c| (c.used, c.budget, c.pruned))
                    .unwrap_or((0, 0, false));
                let used = prompt_tokens.max(estimate);
                self.context = Some(ContextGauge::new(used, budget, pruned));
                // Accumulate the running per-provider session total.
                let provider = self
                    .provider_name
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string());
                let entry = self.session_usage.entry(provider).or_default();
                entry.sent += prompt_tokens;
                entry.received += completion_tokens;
                // Real numbers supersede this request's live estimate.
                self.live_turn = None;
            }
            AgentEvent::Compacted { summary } => {
                self.finish_stream();
                self.clear_transcript();
                self.end_busy();
                self.compacting = false;
                if summary.trim().is_empty() {
                    self.push_system("Nothing to compact.");
                } else {
                    self.push_system(format!("(conversation compacted)\n\n{summary}"));
                }
            }
            AgentEvent::TaskCompacted { id, title } => {
                // A task finished inside an implementation session and its
                // working context was reset before the next task. Show a thin
                // marker; the hidden handoff text never reaches the UI. The
                // turn is still live (the driver continues), so busy stays set.
                self.finish_stream();
                self.push_system(format!("✓ {id} {title} — done, context compacted"));
            }
            AgentEvent::VerifyStarted { command } => {
                // Auto-verification (Agent mode) is running the project check.
                // The turn is still live (busy stays set); show a thin marker.
                self.finish_stream();
                self.push_system(format!("⟳ Verifying — {command}"));
            }
            AgentEvent::VerifyResult { passed, summary } => {
                self.finish_stream();
                let marker = if passed { "✓" } else { "✗" };
                self.push_system(format!("{marker} {summary}"));
            }
            AgentEvent::SubAgentStarted { kind, objective } => {
                // A sub-agent is starting; only its summary folds back into the
                // model's context. The turn is still live, so busy stays set.
                self.finish_stream();
                self.push_system(format!("⤷ {kind} — {objective}"));
            }
            AgentEvent::SubAgentFinished { kind, summary } => {
                self.finish_stream();
                self.push_system(format!("⤶ {kind} done — {summary}"));
            }
            AgentEvent::Retrying {
                attempt,
                max,
                reason,
                delay_ms,
            } => {
                // A transient failure is being retried; the turn is still alive,
                // so keep the busy state and just note the wait.
                self.push_system(format!(
                    "Retrying ({attempt}/{max}) in {:.1}s — {reason}",
                    delay_ms as f64 / 1000.0
                ));
            }
            AgentEvent::Error(message) => {
                self.finish_stream();
                self.settle_live_turn();
                self.push_system(format!("Error: {message}"));
                self.end_busy();
                self.compacting = false;
            }
            AgentEvent::Interrupted => {
                // Whatever streamed stays visible; the turn is simply over.
                self.finish_stream();
                self.settle_live_turn();
                self.push_system("Interrupted.");
                self.end_busy();
                self.compacting = false;
            }
            AgentEvent::Done => {
                self.finish_stream();
                self.settle_live_turn();
                self.end_busy();
                self.maybe_prompt_verify();
            }
        }
    }

    /// Open the begin-verification prompt when the turn that just ended left
    /// every work task done with verification still outstanding. Offered at
    /// most once per step. When the step is fully complete (no verify tasks or
    /// verify already done), checks whether a next step exists and offers the
    /// "Continue to next Step?" prompt.
    fn maybe_prompt_verify(&mut self) {
        if self.implement.is_none() {
            return;
        }
        if self.verify_prompted {
            // Verify phase just finished — step is fully done.
            self.maybe_prompt_next_step();
            return;
        }
        if work_tasks_done(&self.tasks) && verify_outstanding(&self.tasks) {
            self.pending_verify = true;
            self.verify_prompted = true;
        } else if work_tasks_done(&self.tasks) && !verify_outstanding(&self.tasks) {
            // All work done and no verify tasks — step is fully done.
            self.maybe_prompt_next_step();
        }
    }

    /// When the current step is fully done and a next step exists in the plan,
    /// offer the "Continue to next Step?" prompt. Loads the plan store from
    /// the workspace root (a fast local read of a small JSON file).
    fn maybe_prompt_next_step(&mut self) {
        if self.next_step_prompted || self.pending_next_step.is_some() {
            return;
        }
        let Some(current) = &self.implement else {
            return;
        };
        let Some(root) = &self.workspace_root else {
            return;
        };
        let ws = match suis_core::Workspace::detect(root) {
            Ok(ws) => ws,
            Err(_) => return,
        };
        let Ok(store) = suis_core::PlanStore::load(&ws) else {
            return;
        };
        let Some(plan) = store.get(&current.plan_id).cloned() else {
            return;
        };
        let next_idx = current.step_index + 1;
        if next_idx >= plan.steps.len() {
            return;
        }
        let next_step = &plan.steps[next_idx];
        // Only prompt when the next step is still incomplete (todo).
        if next_step.is_complete() {
            return;
        }
        self.next_step_prompted = true;
        self.pending_next_step = Some(ImplementState {
            plan_id: current.plan_id.clone(),
            step_index: next_idx,
            plan_title: current.plan_title.clone(),
            step_title: next_step.title.clone(),
        });
    }

    /// Advance to the next step in an implementation session (after the user
    /// confirms the "Continue to next Step?" popup). Clears the transcript,
    /// resets the task panel, and starts the next step's work phase.
    pub fn begin_next_step(&mut self, next: ImplementState) {
        self.clear_transcript();
        self.tasks.clear();
        self.mode = Mode::Agent;
        self.pending_verify = false;
        self.verify_prompted = false;
        self.pending_next_step = None;
        self.next_step_prompted = false;
        self.push_system(format!("Continuing to next step: {}", next.step_title));
        self.begin_busy();
        self.implement = Some(next);
    }

    /// Append streamed text to the trailing agent message, starting a new one
    /// if the last message is not an in-progress agent message. Any open
    /// thinking block is finalized first: answer text means reasoning is over.
    fn append_stream(&mut self, chunk: &str) {
        self.finish_thinking();
        match self.messages.last_mut() {
            Some(msg) if msg.streaming => msg.text.push_str(chunk),
            _ => {
                let mut msg = ChatMessage::streaming_agent();
                msg.text.push_str(chunk);
                self.messages.push(msg);
            }
        }
    }

    /// Append streamed reasoning to the trailing thinking block, opening a new
    /// one (and starting its elapsed clock) when the last message is not an
    /// in-progress thinking block.
    fn append_reasoning(&mut self, chunk: &str) {
        match self.messages.last_mut().and_then(|m| m.thinking.as_mut()) {
            Some(card) if card.streaming => card.text.push_str(chunk),
            _ => {
                self.messages.push(ChatMessage::thinking_streaming(chunk));
                self.thinking_since = Some(Instant::now());
            }
        }
    }

    /// Finalize the trailing thinking block, recording how long it ran so the
    /// header can read "Thought for Ns". A no-op unless the last message is a
    /// still-streaming thinking block.
    fn finish_thinking(&mut self) {
        let elapsed = self
            .thinking_since
            .take()
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        if let Some(card) = self.messages.last_mut().and_then(|m| m.thinking.as_mut()) {
            if card.streaming {
                card.streaming = false;
                card.elapsed_secs = elapsed;
            }
        }
    }

    /// Finalize the trailing agent message. An empty streamed message (the
    /// model produced only tool calls) is dropped rather than left blank. Any
    /// open thinking block is finalized too — a turn boundary ends reasoning.
    fn finish_stream(&mut self) {
        self.finish_thinking();
        match self.messages.last_mut() {
            Some(msg) if msg.streaming && msg.text.is_empty() => {
                self.messages.pop();
            }
            Some(msg) if msg.streaming => msg.streaming = false,
            _ => {}
        }
    }

    /// Toggle full output / collapse on the message at `index` — a tool card or
    /// a thinking block. The click and keyboard paths both route here.
    pub fn toggle_card_at(&mut self, index: usize) {
        let Some(msg) = self.messages.get_mut(index) else {
            return;
        };
        if let Some(card) = msg.thinking.as_mut() {
            card.expanded = !card.expanded;
        } else if let Some(card) = msg.tool.as_mut() {
            card.expanded = !card.expanded;
        }
    }

    /// Toggle the most recent thinking block (keyboard shortcut).
    pub fn toggle_last_thinking(&mut self) {
        if let Some(card) = self
            .messages
            .iter_mut()
            .rev()
            .find_map(|m| m.thinking.as_mut())
        {
            card.expanded = !card.expanded;
        }
    }

    /// Resolve a mouse click at terminal cell (`col`, `row`) against the last
    /// rendered transcript and toggle the collapsible card it landed on, if any.
    /// Clicks outside the transcript, or on non-collapsible content, do nothing.
    pub fn handle_transcript_click(&mut self, col: u16, row: u16) {
        let Some(area) = self.transcript_area else {
            return;
        };
        if row < area.y
            || row >= area.y.saturating_add(area.height)
            || col < area.x
            || col >= area.x.saturating_add(area.width)
        {
            return;
        }
        // The clicked transcript row, in content coordinates (undo the scroll).
        let content_row = self.effective_scroll().saturating_add(row - area.y);
        let region = message_list::click_regions(&self.messages, area.width)
            .into_iter()
            .find(|r| content_row >= r.start && content_row < r.end);
        if let Some(region) = region {
            self.toggle_card_at(region.index);
        }
    }
}

/// Format a tool call as a one-block `name { …pretty JSON args… }` string for
/// the developer-mode raw-call display — the exact name and arguments the agent
/// sent. Falls back to just the name if the arguments cannot be serialized.
fn format_raw_call(name: &str, args: &serde_json::Value) -> String {
    match serde_json::to_string_pretty(args) {
        Ok(json) => format!("{name} {json}"),
        Err(_) => name.to_string(),
    }
}

/// Extract a [`DiffView`] from an `edit` tool result of the form
/// `"Edited '<path>'.\n--- <path>\n+++ ...\n..."`. Returns `None` if the result
/// carries no unified diff (e.g. "No changes").
fn parse_edit_diff(content: &str) -> Option<DiffView> {
    let idx = content.find("--- ")?;
    let label = content[..idx].trim().trim_end_matches('.').to_string();
    Some(DiffView {
        label: if label.is_empty() {
            "edit".to_string()
        } else {
            label
        },
        diff: content[idx..].to_string(),
    })
}

/// Whether the mirrored plan-step tasks (`w1..`) report every work task done.
/// False when there are no work tasks (an empty mirror is not "done").
pub(crate) fn work_tasks_done(tasks: &[Task]) -> bool {
    let mut any = false;
    for task in tasks.iter().filter(|t| t.id.starts_with('w')) {
        any = true;
        if task.status != TaskStatus::Done {
            return false;
        }
    }
    any
}

/// Whether any verify task (`v1..`) is still not done.
pub(crate) fn verify_outstanding(tasks: &[Task]) -> bool {
    tasks
        .iter()
        .any(|t| t.id.starts_with('v') && t.status != TaskStatus::Done)
}

/// Whether the whole step is done: tasks exist and every one is `Done`.
pub(crate) fn step_done(tasks: &[Task]) -> bool {
    !tasks.is_empty() && tasks.iter().all(|t| t.status == TaskStatus::Done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use suis_agent::{Task, TaskStatus, ToolResult};

    fn state() -> AppState {
        AppState::new(ModelSelect::default())
    }

    #[test]
    fn streamed_chunks_accumulate_into_one_message() {
        let mut s = state();
        s.apply_event(AgentEvent::StreamChunk("Hel".into()));
        s.apply_event(AgentEvent::StreamChunk("lo".into()));
        assert_eq!(s.messages.len(), 1);
        assert_eq!(s.messages[0].text, "Hello");
        assert!(s.messages[0].streaming);
    }

    #[test]
    fn reasoning_chunks_form_a_thinking_block_finalized_by_text() {
        let mut s = state();
        s.apply_event(AgentEvent::ReasoningChunk("weigh ".into()));
        s.apply_event(AgentEvent::ReasoningChunk("it".into()));
        // One streaming thinking block, accumulating its text.
        assert_eq!(s.messages.len(), 1);
        let card = s.messages[0].thinking.as_ref().expect("thinking block");
        assert!(card.streaming);
        assert_eq!(card.text, "weigh it");

        // The first answer text finalizes the thinking block and starts a fresh
        // agent message.
        s.apply_event(AgentEvent::StreamChunk("the answer".into()));
        assert_eq!(s.messages.len(), 2);
        assert!(!s.messages[0].thinking.as_ref().unwrap().streaming);
        assert_eq!(s.messages[1].text, "the answer");
        assert!(s.messages[1].thinking.is_none());
    }

    #[test]
    fn done_finalizes_a_reasoning_only_turn() {
        let mut s = state();
        s.apply_event(AgentEvent::ReasoningChunk("thought".into()));
        s.apply_event(AgentEvent::Done);
        let card = s.messages[0].thinking.as_ref().unwrap();
        assert!(!card.streaming, "Done closes the thinking block");
    }

    #[test]
    fn toggle_card_at_flips_thinking_and_tool_cards() {
        let mut s = state();
        s.apply_event(AgentEvent::ReasoningChunk("r".into()));
        s.apply_event(AgentEvent::ToolCallStarted {
            name: "read".into(),
            args: serde_json::json!({ "path": "a.txt" }),
        });
        s.apply_event(AgentEvent::ToolCallCompleted {
            result: suis_agent::ToolResult::ok("c1", "contents"),
        });
        // Message 0 is the thinking block, message 1 the completed tool card.
        assert!(!s.messages[0].thinking.as_ref().unwrap().expanded);
        s.toggle_card_at(0);
        assert!(s.messages[0].thinking.as_ref().unwrap().expanded);
        s.toggle_card_at(1);
        assert!(s.messages[1].tool.as_ref().unwrap().expanded);
    }

    #[test]
    fn developer_toggle_captures_and_reveals_the_raw_call() {
        let mut s = state();
        // A tool call made before developer mode is on: the raw call is still
        // captured, just not shown yet.
        s.apply_event(AgentEvent::ToolCallStarted {
            name: "edit".into(),
            args: serde_json::json!({ "path": "index.html", "new_string": "" }),
        });
        let card = s.messages[0].tool.as_ref().unwrap();
        assert!(card.raw_call.contains("\"path\""), "raw call not captured");
        assert!(
            card.raw_call.starts_with("edit "),
            "name missing from raw call"
        );
        assert!(!card.show_raw, "raw call shown before /developer");

        // `/developer` flips the flag and back-applies it to the existing card.
        assert!(s.toggle_developer(), "first toggle turns developer on");
        assert!(s.messages[0].tool.as_ref().unwrap().show_raw);

        // A new call now captures show_raw = true from the live flag.
        s.apply_event(AgentEvent::ToolCallStarted {
            name: "read".into(),
            args: serde_json::json!({ "path": "a.rs" }),
        });
        assert!(s.messages[1].tool.as_ref().unwrap().show_raw);

        // Toggling off hides the raw call everywhere again.
        assert!(!s.toggle_developer(), "second toggle turns developer off");
        assert!(!s.messages[0].tool.as_ref().unwrap().show_raw);
        assert!(!s.messages[1].tool.as_ref().unwrap().show_raw);
    }

    #[test]
    fn transcript_click_toggles_the_card_under_the_cursor() {
        let mut s = state();
        s.apply_event(AgentEvent::ReasoningChunk("reasoning".into()));
        s.apply_event(AgentEvent::Done);
        // Simulate a render: the transcript occupies the whole screen, pinned to
        // the top, with the thinking header on its first content row.
        s.transcript_area = Some(Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        });
        s.note_transcript_metrics(1, 10);

        // A click on the header row toggles it open.
        s.handle_transcript_click(2, 0);
        assert!(s.messages[0].thinking.as_ref().unwrap().expanded);
        // A click outside the transcript is ignored.
        s.handle_transcript_click(2, 50);
        assert!(s.messages[0].thinking.as_ref().unwrap().expanded);
    }

    #[test]
    fn agent_error_renders_provider_hint_in_the_transcript() {
        // The transport classifies a remote 401 into a typed, attributed error
        // (20.2); by the time it reaches the UI it is a Display string carrying
        // the provider id and the env-var hint. The transcript must surface both.
        let mut s = state();
        s.busy = true;
        s.apply_event(AgentEvent::Error(
            "auth failed for openrouter — check $OPENROUTER_API_KEY".into(),
        ));
        let line = &s.messages.last().unwrap().text;
        assert!(line.contains("openrouter"), "names the provider: {line}");
        assert!(
            line.contains("$OPENROUTER_API_KEY"),
            "names the env var: {line}"
        );
        assert!(!s.busy, "an error ends the turn");
    }

    #[test]
    fn caps_consent_is_gated_and_remembered() {
        use crate::screens::model_select::ModelEntry;
        use suis_providers::{Capabilities, Model, TransportType};

        let remote = ModelEntry {
            provider_id: "openrouter".into(),
            provider_name: "OpenRouter".into(),
            endpoint: "https://openrouter.ai/api".into(),
            transport: TransportType::OpenAiCompatible,
            api_key: Some("sk-x".into()),
            api_key_env: Some("OPENROUTER_API_KEY".into()),
            model: Model::new(
                "openrouter",
                "qwen/qwen3-coder",
                Capabilities::discovery_default(),
            ),
        };
        let mut s = state();
        assert!(
            s.needs_caps_consent(&remote),
            "unverified remote needs consent"
        );

        // A local (unkeyed) model is gated too — probing is deferred to
        // selection for everyone now, not just keyed providers.
        let mut local = remote.clone();
        local.provider_id = "lmstudio".into();
        local.api_key = None;
        local.api_key_env = None;
        assert!(s.needs_caps_consent(&local), "unverified local also asks");

        // A verified (advertised) model selects straight through.
        let mut verified = remote.clone();
        verified.model.verified = true;
        assert!(!s.needs_caps_consent(&verified), "verified never asks");

        // A declined model is not re-asked this session.
        s.declined_caps.insert(caps_consent_key(
            &remote.provider_id,
            &remote.model.model_id,
        ));
        assert!(!s.needs_caps_consent(&remote), "declined is remembered");
    }

    #[test]
    fn done_finalizes_and_clears_busy() {
        let mut s = state();
        s.busy = true;
        s.apply_event(AgentEvent::StreamChunk("hi".into()));
        s.apply_event(AgentEvent::Done);
        assert!(!s.messages[0].streaming);
        assert!(!s.busy);
    }

    #[test]
    fn empty_stream_before_tool_is_dropped() {
        let mut s = state();
        // Model emitted no text, only a tool call.
        s.apply_event(AgentEvent::ToolCallStarted {
            name: "read".into(),
            args: serde_json::json!({ "path": "src/main.rs" }),
        });
        // The running tool card is present; no blank agent line.
        assert_eq!(s.messages.len(), 1);
        let card = s.messages[0].tool.as_ref().expect("a tool card");
        assert_eq!(card.status, ToolStatus::Running);
        assert_eq!(card.name, "read");
        assert_eq!(card.subject, "src/main.rs");
    }

    #[test]
    fn started_and_completed_pair_is_one_card_updated_in_place() {
        let mut s = state();
        s.apply_event(AgentEvent::ToolCallStarted {
            name: "read".into(),
            args: serde_json::json!({ "path": "src/main.rs" }),
        });
        s.apply_event(AgentEvent::ToolCallCompleted {
            result: ToolResult::ok("c1", "line 1\nline 2\nline 3"),
        });
        assert_eq!(s.messages.len(), 1, "the running card updates in place");
        let card = s.messages[0].tool.as_ref().unwrap();
        assert_eq!(card.status, ToolStatus::Ok);
        assert_eq!(card.output, "line 1\nline 2\nline 3");
        assert_eq!(card.subject, "src/main.rs");
        assert!(!card.expanded);
    }

    #[test]
    fn tool_error_stays_a_single_card_in_error_state() {
        let mut s = state();
        s.apply_event(AgentEvent::ToolCallStarted {
            name: "bash".into(),
            args: serde_json::json!({ "command": "cargo build" }),
        });
        s.apply_event(AgentEvent::ToolCallCompleted {
            result: ToolResult::error("c1", "boom"),
        });
        assert_eq!(s.messages.len(), 1);
        let card = s.messages[0].tool.as_ref().unwrap();
        assert_eq!(card.status, ToolStatus::Error);
        assert_eq!(card.output, "boom");
    }

    #[test]
    fn unmatched_completion_appends_a_completed_card() {
        let mut s = state();
        s.apply_event(AgentEvent::ToolCallCompleted {
            result: ToolResult::ok("c1", "file contents"),
        });
        let card = s
            .messages
            .last()
            .unwrap()
            .tool
            .as_ref()
            .expect("fallback card");
        assert_eq!(card.status, ToolStatus::Ok);
        assert_eq!(card.output, "file contents");

        s.apply_event(AgentEvent::ToolCallCompleted {
            result: ToolResult::error("c2", "boom"),
        });
        let card = s.messages.last().unwrap().tool.as_ref().unwrap();
        assert_eq!(card.status, ToolStatus::Error);
    }

    #[test]
    fn ctrl_o_target_is_the_latest_completed_card() {
        let mut s = state();
        // One completed card, then a running one.
        s.apply_event(AgentEvent::ToolCallStarted {
            name: "read".into(),
            args: serde_json::json!({ "path": "a.txt" }),
        });
        s.apply_event(AgentEvent::ToolCallCompleted {
            result: ToolResult::ok("c1", "contents"),
        });
        s.apply_event(AgentEvent::ToolCallStarted {
            name: "bash".into(),
            args: serde_json::json!({ "command": "ls" }),
        });

        s.toggle_last_tool_card();
        assert!(
            s.messages[0].tool.as_ref().unwrap().expanded,
            "the completed card expands, not the running one"
        );
        assert!(!s.messages[1].tool.as_ref().unwrap().expanded);
        s.toggle_last_tool_card();
        assert!(
            !s.messages[0].tool.as_ref().unwrap().expanded,
            "toggles back"
        );
    }

    #[test]
    fn edit_result_captures_last_diff() {
        let mut s = state();
        s.apply_event(AgentEvent::ToolCallCompleted {
            result: ToolResult::ok(
                "c1",
                "Edited 'src/main.rs'.\n--- src/main.rs\n+++ src/main.rs\n-old\n+new\n",
            ),
        });
        let diff = s.last_diff.expect("diff captured");
        assert!(diff.label.contains("src/main.rs"));
        assert!(diff.diff.starts_with("--- src/main.rs"));
    }

    #[test]
    fn non_diff_result_leaves_last_diff_unset() {
        let mut s = state();
        s.apply_event(AgentEvent::ToolCallCompleted {
            result: ToolResult::ok("c1", "file contents, no diff here"),
        });
        assert!(s.last_diff.is_none());
    }

    #[test]
    fn task_update_populates_panel() {
        let mut s = state();
        // The panel is open by default.
        assert!(s.show_tasks);
        s.apply_event(AgentEvent::TaskUpdated(vec![Task {
            id: "t1".into(),
            title: "do it".into(),
            status: TaskStatus::Doing,
        }]));
        assert_eq!(s.tasks.len(), 1);
        assert!(s.show_tasks);
    }

    #[test]
    fn context_usage_updates_the_gauge() {
        let mut s = state();
        assert!(s.context.is_none());
        s.apply_event(AgentEvent::ContextUsage {
            used_tokens: 7_440,
            budget: 12_000,
            pruned: false,
        });
        let gauge = s.context.expect("gauge set");
        assert_eq!(gauge.percent(), 62);
        assert!(!gauge.pruned);

        s.apply_event(AgentEvent::ContextUsage {
            used_tokens: 12_500,
            budget: 12_000,
            pruned: true,
        });
        assert!(s.context.unwrap().pruned);
    }

    #[test]
    fn token_usage_sets_real_occupancy_and_accumulates_per_provider() {
        let mut s = state();
        s.provider_name = Some("Ollama".into());
        // The pre-request estimate sets the budget on the gauge.
        s.apply_event(AgentEvent::ContextUsage {
            used_tokens: 1_000,
            budget: 30_000,
            pruned: false,
        });
        // Real usage replaces the gauge's `used` and feeds the session total.
        s.apply_event(AgentEvent::TokenUsage {
            prompt_tokens: 12_000,
            completion_tokens: 300,
        });
        let gauge = s.context.expect("gauge");
        assert_eq!(gauge.used, 12_000);
        assert_eq!(gauge.budget, 30_000, "budget kept from ContextUsage");
        assert_eq!(s.active_session_total(), Some(12_300));
        // The input-box border shows the live context size — the gauge's `used`.
        assert_eq!(s.context_tokens(), Some(12_000));

        // A fresh request re-measures the context; a second turn accumulates
        // onto the same provider's session total.
        s.apply_event(AgentEvent::ContextUsage {
            used_tokens: 13_000,
            budget: 30_000,
            pruned: false,
        });
        s.apply_event(AgentEvent::TokenUsage {
            prompt_tokens: 13_000,
            completion_tokens: 200,
        });
        assert_eq!(s.context_tokens(), Some(13_000));
        assert_eq!(s.active_session_total(), Some(12_300 + 13_200));
    }

    #[test]
    fn gauge_is_floored_at_the_estimate_when_provider_reports_a_delta() {
        // Ollama reports only newly-evaluated tokens per turn (prompt caching);
        // the gauge must not collapse to that delta but track the full chat via
        // the estimate from the preceding ContextUsage.
        let mut s = state();
        s.provider_name = Some("Ollama".into());
        s.apply_event(AgentEvent::ContextUsage {
            used_tokens: 15_000, // estimate of the full assembled conversation
            budget: 30_000,
            pruned: false,
        });
        s.apply_event(AgentEvent::TokenUsage {
            prompt_tokens: 500, // only the uncached delta this turn
            completion_tokens: 40,
        });
        // The border shows the full-context estimate, not the 500-token delta.
        assert_eq!(s.context.expect("gauge").used, 15_000);
        // The session total still counts the real tokens that were processed.
        assert_eq!(s.active_session_total(), Some(540));
    }

    #[test]
    fn clear_resets_the_gauge_but_keeps_session_totals() {
        let mut s = state();
        s.provider_name = Some("Ollama".into());
        s.apply_event(AgentEvent::TokenUsage {
            prompt_tokens: 8_000,
            completion_tokens: 100,
        });
        assert!(s.context.is_some());

        // The border shows the live context size — the gauge's `used`.
        assert_eq!(s.context_tokens(), Some(8_000));

        s.clear_transcript();
        // The per-chat gauge and its token figure both zero out...
        assert!(s.context.is_none());
        assert_eq!(s.context_tokens(), None);
        // ...but the session total persists across the clear.
        assert_eq!(s.active_session_total(), Some(8_100));
    }

    #[test]
    fn streaming_does_not_move_the_context_size_without_an_early_prompt_report() {
        // OpenAI/Ollama only report the prompt at turn end. The gauge is seeded
        // from the pre-request estimate (ContextUsage) and holds steady while the
        // body streams — no live preview — then settles on the real prompt.
        let mut s = state();
        s.provider_name = Some("Ollama".into());
        s.apply_event(AgentEvent::ContextUsage {
            used_tokens: 1_000,
            budget: 30_000,
            pruned: false,
        });
        // The border shows the estimate; the session total is still empty.
        assert_eq!(s.context_tokens(), Some(1_000));
        assert_eq!(s.active_session_total(), None);

        // Streamed output does not move the context size — no preview is armed.
        s.apply_event(AgentEvent::StreamChunk("a".repeat(400)));
        assert_eq!(s.context_tokens(), Some(1_000));
        assert_eq!(s.active_session_total(), None);

        // Only the provider's real counts, at turn end, settle the gauge and the
        // session total.
        s.apply_event(AgentEvent::TokenUsage {
            prompt_tokens: 1_050,
            completion_tokens: 25,
        });
        assert_eq!(s.context_tokens(), Some(1_050));
        assert_eq!(s.active_session_total(), Some(1_075));
    }

    #[test]
    fn early_prompt_report_makes_the_context_size_exact_mid_stream() {
        // Anthropic reports the real prompt size at message_start; PromptTokens
        // floors the context gauge with that exact value before the body
        // streams, so the context size is exact from the first chunk.
        let mut s = state();
        s.provider_name = Some("Anthropic".into());
        s.apply_event(AgentEvent::ContextUsage {
            used_tokens: 1_000, // chars/4 estimate — the gauge's starting point
            budget: 30_000,
            pruned: false,
        });
        assert_eq!(s.context_tokens(), Some(1_000));

        // The exact prompt size lands mid-stream and floors the gauge.
        s.apply_event(AgentEvent::PromptTokens {
            prompt_tokens: 1_120,
        });
        assert_eq!(s.context_tokens(), Some(1_120));
        assert_eq!(s.context.expect("gauge").used, 1_120);

        // Streamed completion is not context yet, so the size holds steady.
        s.apply_event(AgentEvent::StreamChunk("a".repeat(40)));
        assert_eq!(s.context_tokens(), Some(1_120));

        // TokenUsage commits the exact counts to the session total.
        s.apply_event(AgentEvent::TokenUsage {
            prompt_tokens: 1_120,
            completion_tokens: 12,
        });
        assert_eq!(s.context_tokens(), Some(1_120));
        assert_eq!(s.active_session_total(), Some(1_132));
    }

    #[test]
    fn an_armed_preview_is_committed_to_the_session_when_the_turn_ends_without_usage() {
        // Anthropic arms the preview via PromptTokens. If the turn ends without
        // a final TokenUsage (e.g. interrupted), the armed estimate is folded
        // into the session total rather than lost.
        let mut s = state();
        s.provider_name = Some("Anthropic".into());
        s.apply_event(AgentEvent::ContextUsage {
            used_tokens: 1,
            budget: 30_000,
            pruned: false,
        });
        s.apply_event(AgentEvent::PromptTokens { prompt_tokens: 500 });
        s.apply_event(AgentEvent::StreamChunk("x".repeat(80))); // +20 completion tokens
        assert_eq!(s.context_tokens(), Some(500));
        assert_eq!(
            s.active_session_total(),
            None,
            "uncommitted until the turn ends"
        );

        s.apply_event(AgentEvent::Done);
        assert!(s.live_turn.is_none());
        assert_eq!(s.active_session_total(), Some(520));
    }

    #[test]
    fn compacted_replaces_transcript_with_summary() {
        let mut s = state();
        s.push_user("a long conversation");
        s.push_system("more lines");
        s.busy = true;
        s.compacting = true;

        s.apply_event(AgentEvent::Compacted {
            summary: "the user built a parser".into(),
        });

        assert!(!s.busy);
        assert!(!s.compacting);
        // Exactly one message remains: the compacted summary, with its header.
        assert_eq!(s.messages.len(), 1);
        let text = &s.messages[0].text;
        assert!(text.contains("(conversation compacted)"));
        assert!(text.contains("the user built a parser"));
    }

    #[test]
    fn empty_compaction_reports_nothing_to_compact() {
        let mut s = state();
        s.compacting = true;
        s.apply_event(AgentEvent::Compacted {
            summary: "   ".into(),
        });
        assert!(!s.compacting);
        assert_eq!(s.messages.len(), 1);
        assert!(s.messages[0].text.contains("Nothing to compact"));
    }

    #[test]
    fn error_event_records_message_and_unbusies() {
        let mut s = state();
        s.busy = true;
        s.apply_event(AgentEvent::Error("nope".into()));
        assert!(!s.busy);
        assert!(s.messages.last().unwrap().text.contains("nope"));
    }

    #[test]
    fn retrying_event_notes_progress_without_ending_the_turn() {
        let mut s = state();
        s.busy = true;
        s.apply_event(AgentEvent::Retrying {
            attempt: 1,
            max: 3,
            reason: "provider timed out".into(),
            delay_ms: 400,
        });
        // The turn is still alive: stay busy, just surface the wait.
        assert!(s.busy);
        let last = &s.messages.last().unwrap().text;
        assert!(last.contains("Retrying"));
        assert!(last.contains("1/3"));
        assert!(last.contains("provider timed out"));
    }

    #[test]
    fn permission_request_becomes_pending() {
        let mut s = state();
        let (tx, _rx) = oneshot::channel();
        s.apply_event(AgentEvent::PermissionRequest {
            action: "run command: cargo test".into(),
            sender: tx,
        });
        assert!(s.awaiting_permission());
    }

    #[test]
    fn answering_permission_clears_pending() {
        let mut s = state();
        let (tx, rx) = oneshot::channel();
        s.apply_event(AgentEvent::PermissionRequest {
            action: "run command: ls".into(),
            sender: tx,
        });
        s.answer_permission(PermissionDecision::once());
        assert!(!s.awaiting_permission());
        // The decision reached the (would-be agent) receiver.
        assert_eq!(rx.blocking_recv().ok(), Some(PermissionDecision::once()));
    }

    #[test]
    fn following_pins_effective_scroll_to_the_bottom() {
        let mut s = state();
        s.note_transcript_metrics(100, 10);
        // While following, the rendered offset tracks the bottom regardless of
        // the stored `scroll`.
        assert!(s.follow_bottom);
        assert_eq!(s.effective_scroll(), 90);
        // Growing content keeps the view at the new bottom.
        s.note_transcript_metrics(150, 10);
        assert_eq!(s.effective_scroll(), 140);
    }

    #[test]
    fn detached_view_stays_put_as_content_grows() {
        let mut s = state();
        s.note_transcript_metrics(100, 10);
        s.scroll_up(); // detaches at offset 89
        assert!(!s.follow_bottom);
        assert_eq!(s.effective_scroll(), 89);
        // New content arrives; the offset does not jump to the bottom.
        s.note_transcript_metrics(150, 10);
        assert_eq!(s.effective_scroll(), 89);
    }

    #[test]
    fn sending_a_message_snaps_back_to_the_bottom() {
        let mut s = state();
        s.note_transcript_metrics(100, 10);
        s.scroll_up();
        assert!(!s.follow_bottom);
        s.push_user("hi");
        assert!(s.follow_bottom);
    }

    #[test]
    fn detachment_tracks_the_pinning_state_machine() {
        let mut s = state();
        s.note_transcript_metrics(100, 10);
        assert!(!s.is_detached(), "following the bottom is not detached");

        s.scroll_up();
        assert!(s.is_detached(), "scrolling up detaches");

        // Scrolling back to the bottom re-attaches.
        while !s.follow_bottom {
            s.scroll_down();
        }
        assert!(!s.is_detached());

        // Detaching with content that fits the viewport is not "detached" for
        // rendering: there is nothing to scroll.
        s.note_transcript_metrics(5, 10);
        s.follow_bottom = false;
        assert!(!s.is_detached());
    }

    #[test]
    fn streaming_mirrors_the_busy_turn() {
        let mut s = state();
        assert!(!s.is_streaming());
        s.begin_busy();
        assert!(s.is_streaming());
        s.end_busy();
        assert!(!s.is_streaming());
    }

    #[test]
    fn styled_system_notice_lands_in_the_transcript() {
        let mut s = state();
        s.push_system_styled(vec![ratatui::text::Line::from("Available commands:")]);
        let msg = s.messages.last().unwrap();
        assert_eq!(msg.role, MsgRole::System);
        assert!(msg.styled.is_some());
        assert_eq!(msg.text, "Available commands:");
    }

    #[test]
    fn effective_scroll_clamps_to_content_when_detached() {
        let mut s = state();
        s.follow_bottom = false;
        s.scroll = 500; // stale offset larger than the content
        s.note_transcript_metrics(40, 10);
        assert_eq!(s.effective_scroll(), 30);
    }

    #[test]
    fn session_notice_lands_before_the_first_user_message() {
        let mut s = state();
        s.selected_model = Some(suis_providers::Model::new(
            "ollama",
            "qwen3-coder",
            suis_providers::Capabilities::default(),
        ));
        s.provider_name = Some("Ollama".into());
        s.arm_session_notice();
        assert!(
            s.messages.is_empty(),
            "the armed notice is not a message yet"
        );

        s.push_user("hello");
        assert_eq!(s.messages.len(), 2);
        assert_eq!(s.messages[0].role, MsgRole::System);
        assert_eq!(s.messages[0].text, "connected · qwen3-coder @ Ollama");
        assert_eq!(s.messages[1].role, MsgRole::User);

        // Flushed once; the next message does not repeat it.
        s.push_user("again");
        assert_eq!(s.messages.len(), 3);

        // Clearing re-arms it for the next conversation.
        s.clear_transcript();
        assert!(s.messages.is_empty());
        s.push_user("fresh start");
        assert!(s.messages[0].text.starts_with("connected ·"));
    }

    #[test]
    fn take_input_empties_buffer() {
        let mut s = state();
        s.push_char('h');
        s.push_char('i');
        assert_eq!(s.take_input(), "hi");
        assert!(s.input.is_empty());
    }

    fn task(id: &str, status: TaskStatus) -> Task {
        Task {
            id: id.into(),
            title: format!("task {id}"),
            status,
        }
    }

    fn implementing() -> ImplementState {
        ImplementState {
            plan_id: "auth".into(),
            step_index: 0,
            plan_title: "Auth".into(),
            step_title: "tokens".into(),
        }
    }

    #[test]
    fn work_done_detection_from_task_state() {
        // Work done, verify outstanding.
        let tasks = vec![
            task("w1", TaskStatus::Done),
            task("w2", TaskStatus::Done),
            task("v1", TaskStatus::Todo),
        ];
        assert!(work_tasks_done(&tasks));
        assert!(verify_outstanding(&tasks));
        assert!(!step_done(&tasks));

        // A work task still open.
        let open = vec![task("w1", TaskStatus::Doing), task("v1", TaskStatus::Todo)];
        assert!(!work_tasks_done(&open));

        // Everything done.
        let all = vec![task("w1", TaskStatus::Done), task("v1", TaskStatus::Done)];
        assert!(step_done(&all));
        assert!(!verify_outstanding(&all));

        // No tasks at all is never "done".
        assert!(!work_tasks_done(&[]));
        assert!(!step_done(&[]));
    }

    #[test]
    fn begin_implement_clears_history_and_forces_agent_mode() {
        let mut s = state();
        s.push_user("old conversation");
        s.mode = Mode::Plan;
        s.tasks = vec![task("t1", TaskStatus::Todo)];

        s.begin_implement(implementing());

        assert_eq!(s.mode, Mode::Agent);
        assert!(s.busy);
        assert!(s.tasks.is_empty());
        assert_eq!(s.implement.as_ref().unwrap().plan_id, "auth");
        // Only the session notice remains — the conversation was cleared.
        assert!(!s.has_conversation());
        assert!(s.messages.last().unwrap().text.contains("Auth · tokens"));
    }

    #[test]
    fn verify_prompt_opens_once_when_work_finishes() {
        let mut s = state();
        s.implement = Some(implementing());
        s.apply_event(AgentEvent::TaskUpdated(vec![
            task("w1", TaskStatus::Done),
            task("v1", TaskStatus::Todo),
        ]));
        assert!(!s.pending_verify, "prompt waits for the turn to end");

        s.apply_event(AgentEvent::Done);
        assert!(s.pending_verify);
        assert!(s.verify_prompted);

        // A later turn ending does not re-open it.
        s.pending_verify = false;
        s.apply_event(AgentEvent::Done);
        assert!(!s.pending_verify);
    }

    #[test]
    fn verify_prompt_skipped_outside_implement_or_with_open_work() {
        let mut s = state();
        s.tasks = vec![task("w1", TaskStatus::Done), task("v1", TaskStatus::Todo)];
        s.apply_event(AgentEvent::Done);
        assert!(!s.pending_verify, "no implementation session");

        s.implement = Some(implementing());
        s.tasks = vec![task("w1", TaskStatus::Doing), task("v1", TaskStatus::Todo)];
        s.apply_event(AgentEvent::Done);
        assert!(!s.pending_verify, "work still open");
    }

    #[test]
    fn step_completion_notice_appears_once() {
        let mut s = state();
        s.implement = Some(implementing());
        s.apply_event(AgentEvent::TaskUpdated(vec![
            task("w1", TaskStatus::Done),
            task("v1", TaskStatus::Doing),
        ]));
        assert!(!s.messages.iter().any(|m| m.text.contains("Step complete")));

        s.apply_event(AgentEvent::TaskUpdated(vec![
            task("w1", TaskStatus::Done),
            task("v1", TaskStatus::Done),
        ]));
        let count = |s: &AppState| {
            s.messages
                .iter()
                .filter(|m| m.text.contains("Step complete"))
                .count()
        };
        assert_eq!(count(&s), 1);

        // A repeated all-done update does not repeat the notice.
        s.apply_event(AgentEvent::TaskUpdated(vec![
            task("w1", TaskStatus::Done),
            task("v1", TaskStatus::Done),
        ]));
        assert_eq!(count(&s), 1);
    }

    #[test]
    fn history_recalls_newest_first_and_restores_the_draft() {
        let mut s = state();
        for text in ["one", "two", "three"] {
            s.history.push(text);
        }
        s.set_input("draft");

        s.history_up();
        assert_eq!(s.input, "three");
        s.history_up();
        assert_eq!(s.input, "two");
        s.history_up();
        assert_eq!(s.input, "one");
        // Past the oldest entry, Up holds.
        s.history_up();
        assert_eq!(s.input, "one");

        s.history_down();
        assert_eq!(s.input, "two");
        s.history_down();
        assert_eq!(s.input, "three");
        // Walking past the newest restores the stashed draft.
        s.history_down();
        assert_eq!(s.input, "draft");
        assert_eq!(s.input_cursor, 5, "cursor lands at the end of the recall");
    }

    #[test]
    fn history_collapses_consecutive_duplicates() {
        let mut h = InputHistory::default();
        h.push("same");
        h.push("same");
        h.push("other");
        h.push("same");
        assert_eq!(h.entries, vec!["same", "other", "same"]);
    }

    #[test]
    fn history_caps_its_entries() {
        let mut h = InputHistory::default();
        for i in 0..150 {
            h.push(&format!("msg {i}"));
        }
        assert_eq!(h.entries.len(), HISTORY_CAP);
        assert_eq!(h.entries[0], "msg 50", "oldest entries drop first");
    }

    #[test]
    fn editing_a_recalled_entry_detaches_from_history() {
        let mut s = state();
        s.history.push("hello");
        s.history_up();
        assert_eq!(s.input, "hello");
        s.push_char('!');
        assert_eq!(s.input, "hello!");
        // Detached: Down no longer walks history.
        s.history_down();
        assert_eq!(s.input, "hello!");
    }

    #[test]
    fn cursor_editing_inserts_and_deletes_at_the_caret() {
        let mut s = state();
        for c in "ace".chars() {
            s.push_char(c);
        }
        s.cursor_left();
        s.cursor_left();
        s.push_char('b');
        assert_eq!(s.input, "abce");
        s.cursor_end();
        s.backspace();
        assert_eq!(s.input, "abc");
        s.cursor_home();
        assert_eq!(s.input_cursor, 0);
        s.backspace();
        assert_eq!(s.input, "abc", "backspace at the start is a no-op");
    }

    #[test]
    fn home_and_end_stay_within_the_logical_line() {
        let mut s = state();
        s.set_input("first\nsecond");
        // Cursor at the very end; Home goes to the start of "second".
        s.cursor_home();
        assert_eq!(s.input_cursor, 6);
        s.cursor_end();
        assert_eq!(s.input_cursor, 12);
        // Left across the boundary lands on the previous line's end.
        s.cursor_home();
        s.cursor_left();
        assert_eq!(s.input_cursor, 5);
    }

    #[test]
    fn palette_lists_matches_in_commands_order() {
        let mut s = state();
        s.set_input("/p");
        let names: Vec<&str> = s.palette_matches().iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            vec!["permissions", "providers", "plan", "plans", "profile"]
        );
        assert!(s.palette_open());

        // A bare slash lists everything.
        s.set_input("/");
        assert_eq!(s.palette_matches().len(), COMMANDS.len());

        // Arguments and non-commands close it.
        s.set_input("/model qwen");
        assert!(!s.palette_open());
        s.set_input("hello");
        assert!(!s.palette_open());
        s.set_input("/zzz");
        assert!(!s.palette_open(), "no matches, no palette");
    }

    #[test]
    fn palette_selection_wraps_both_ways() {
        let mut s = state();
        s.set_input("/p"); // permissions, providers, plan, plans, profile
        assert_eq!(s.palette_selected_name(), Some("permissions"));
        s.palette_move(-1);
        assert_eq!(
            s.palette_selected_name(),
            Some("profile"),
            "wraps to the end"
        );
        s.palette_move(1);
        assert_eq!(s.palette_selected_name(), Some("permissions"));
        s.palette_move(1);
        assert_eq!(s.palette_selected_name(), Some("providers"));
    }

    #[test]
    fn palette_dismiss_holds_until_the_next_edit() {
        let mut s = state();
        s.set_input("/p");
        assert!(s.palette_open());
        s.dismiss_palette();
        assert!(!s.palette_open());
        // Typing re-derives and reopens.
        s.push_char('l');
        assert!(s.palette_open());
        assert_eq!(s.input, "/pl");
    }

    #[test]
    fn overlays_suppress_the_palette() {
        let mut s = state();
        s.set_input("/p");
        assert!(s.palette_open());
        let (tx, _rx) = oneshot::channel();
        s.apply_event(AgentEvent::PermissionRequest {
            action: "run command: ls".into(),
            sender: tx,
        });
        assert!(!s.palette_open());
    }

    #[test]
    fn busy_hint_covers_each_state() {
        let mut s = state();
        assert_eq!(s.busy_hint(), None, "idle has no hint");

        s.begin_busy();
        let hint = s.busy_hint().unwrap();
        assert!(
            WORK_VERBS.iter().any(|v| hint.contains(v)),
            "busy line carries a boar verb: {hint}"
        );
        assert!(hint.contains("0s"));
        assert!(hint.contains("Esc to interrupt"));
        assert!(!hint.contains("·  ·"), "no empty tool segment");

        s.apply_event(AgentEvent::ToolCallStarted {
            name: "read".into(),
            args: serde_json::json!({ "path": "a.txt" }),
        });
        let hint = s.busy_hint().unwrap();
        assert!(
            hint.contains(&format!("{} · read · ", s.busy_verb)),
            "busy+tool: {hint}"
        );
        s.apply_event(AgentEvent::ToolCallCompleted {
            result: ToolResult::ok("c1", "contents"),
        });
        assert!(!s.busy_hint().unwrap().contains("read"), "tool cleared");

        s.compacting = true;
        let hint = s.busy_hint().unwrap();
        assert!(
            hint.contains("compacting the conversation…"),
            "compacting: {hint}"
        );

        s.compacting = false;
        s.apply_event(AgentEvent::Done);
        assert_eq!(s.busy_hint(), None);
    }

    #[test]
    fn interrupted_event_finalizes_the_stream_and_ends_the_turn() {
        let mut s = state();
        s.begin_busy();
        s.interrupting = true;
        s.apply_event(AgentEvent::StreamChunk("a partial answ".into()));

        s.apply_event(AgentEvent::Interrupted);

        assert!(!s.busy);
        assert!(!s.interrupting);
        // The partial text stays visible, finalized.
        assert_eq!(s.messages[0].text, "a partial answ");
        assert!(!s.messages[0].streaming);
        assert_eq!(s.messages.last().unwrap().text, "Interrupted.");
    }

    #[test]
    fn interrupted_compaction_clears_the_compacting_flag() {
        let mut s = state();
        s.begin_busy();
        s.compacting = true;
        s.interrupting = true;
        s.apply_event(AgentEvent::Interrupted);
        assert!(!s.compacting);
        assert!(!s.busy);
    }

    #[test]
    fn interrupting_hint_replaces_the_busy_hint() {
        let mut s = state();
        s.begin_busy();
        s.interrupting = true;
        let hint = s.busy_hint().unwrap();
        assert!(hint.contains("interrupting…"), "got: {hint}");
        assert!(!hint.contains("Esc to interrupt"));

        // The turn ends; everything resets.
        s.apply_event(AgentEvent::Interrupted);
        assert_eq!(s.busy_hint(), None);
    }

    #[test]
    fn spinner_advances_on_tick_and_holds_when_idle() {
        let mut s = state();
        assert!(!s.on_tick(), "idle tick is a no-op");
        assert_eq!(s.spinner_frame, 0);

        s.begin_busy();
        assert!(s.on_tick());
        assert!(s.on_tick());
        assert_eq!(s.spinner_frame, 2);

        s.end_busy();
        assert!(!s.on_tick());
        assert_eq!(s.spinner_frame, 2, "frame holds while idle");
    }

    #[test]
    fn tasks_toggle_keys_off_actual_visibility() {
        let mut s = state();
        // Narrow, empty, untouched: the popup is suppressed, so the panel is
        // not visible even though show_tasks defaults on.
        s.tasks_visible = false;
        assert!(!s.show_tasks_popup(), "empty + not explicit stays hidden");

        // One /tasks press shows it (explicit), even with no tasks.
        s.toggle_tasks();
        assert!(s.show_tasks);
        assert!(
            s.show_tasks_popup(),
            "explicit toggle reveals the empty popup"
        );

        // The renderer marks it visible; the next press hides it.
        s.tasks_visible = true;
        s.toggle_tasks();
        assert!(!s.show_tasks);
        assert!(!s.show_tasks_popup());

        // Tasks present show the popup regardless of the explicit flag.
        let mut s = state();
        s.tasks = vec![Task {
            id: "t1".into(),
            title: "do it".into(),
            status: TaskStatus::Todo,
        }];
        assert!(s.show_tasks_popup());
    }

    #[test]
    fn plan_scroll_offsets_move_and_floor_at_zero() {
        let mut s = state();
        let (tx, _rx) = oneshot::channel();
        s.apply_event(AgentEvent::PlanProposal {
            draft: PlanDraft {
                revises: None,
                title: "Auth".into(),
                description: String::new(),
                steps: vec![],
            },
            sender: tx,
        });
        s.scroll_plan_v(5);
        s.scroll_plan_h(4);
        assert_eq!(s.pending_plan.as_ref().unwrap().scroll_y, 5);
        assert_eq!(s.pending_plan.as_ref().unwrap().scroll_x, 4);
        // Scrolling back past the top floors at zero rather than underflowing.
        s.scroll_plan_v(-10);
        s.scroll_plan_h(-10);
        assert_eq!(s.pending_plan.as_ref().unwrap().scroll_y, 0);
        assert_eq!(s.pending_plan.as_ref().unwrap().scroll_x, 0);
    }

    #[test]
    fn plan_proposal_becomes_pending_and_answer_unblocks() {
        let mut s = state();
        let (tx, rx) = oneshot::channel();
        s.apply_event(AgentEvent::PlanProposal {
            draft: PlanDraft {
                revises: None,
                title: "Auth".into(),
                description: String::new(),
                steps: vec![],
            },
            sender: tx,
        });
        assert!(s.pending_plan.is_some());

        s.answer_plan(PlanDecision::Approve);
        assert!(s.pending_plan.is_none());
        assert_eq!(rx.blocking_recv().ok(), Some(PlanDecision::Approve));
    }
}
