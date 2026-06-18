//! The agentic execution loop.
//!
//! [`Agent::run_turn`] drives one user turn to completion: assemble context →
//! stream a model response → if it requested tools, execute them (gated by the
//! [`ToolExecutor`]) and loop with their results → otherwise finish. Progress is
//! reported to the UI as [`AgentEvent`]s.
//!
//! The UI can interrupt a running turn (Esc) through the [`watch`] signal
//! installed with [`Agent::with_interrupt`]: the model stream is abandoned
//! immediately (even mid-stall), a *running* tool is never killed mid-write,
//! and any not-yet-started tool calls are skipped with synthetic results so
//! the recorded conversation stays valid.

use std::sync::Arc;

use futures::StreamExt;
use tokio::sync::{mpsc, watch};

use suis_providers::Transport;

use suis_providers::ChatRequest;

use suis_core::PlanStore;

use super::events::AgentEvent;
use super::session::{ImplementTarget, LedgerEntry, Session};
use crate::context::{work_package, ContextAssembler};
use crate::conversation::{ConversationHistory, Message, Role};
use crate::tasks::{plan_step_tasks, Task, TaskStatus};
use crate::tools::subagent::{self, SubAgentProfile};
use crate::tools::{default_tools, Tool, ToolCall, ToolDefinition, ToolExecutor, ToolResult};

/// A runaway backstop on tool-call iterations per turn — not a work limit. Real
/// turns never approach it; it exists only so a genuinely stuck model can't spin
/// forever (the user can also interrupt at any point). Deliberately high so it
/// never cuts off legitimate long-running work.
const MAX_ITERATIONS: usize = 1_000;

/// How many times a single Agent-mode turn may auto-run the project's
/// `verify_command` and loop the model to fix a failure (Phase 2). A small cap
/// so a perpetually-failing build can't burn the whole iteration budget; after
/// it, the turn settles and reports honestly.
const MAX_VERIFY_ROUNDS: usize = 3;

/// Exact-duplicate read-only calls within one turn before the repetition guard
/// drops in a one-shot reminder. Re-issuing the *same* read/search/tree is the
/// signature of a stuck local model; reading *different* things is just
/// exploration and never counts, so this stays clear of legitimate orientation.
const REDUNDANT_READ_LIMIT: usize = 2;

/// The one-shot, non-coercive note the repetition guard injects. It never says
/// "stop and answer" — it only flags the redundancy and leaves the next move to
/// the model, so a model that genuinely needs more context is not cut off.
const REPETITION_NOTE: &str = "Note: you've repeated an orientation call \
    (read/search/tree) you already ran this turn — its result is unchanged and \
    still above. If you have what you need, continue; otherwise look at \
    something you haven't examined yet. Don't re-run the same call.";

/// How many times a single model request may be re-sent after a *transient*
/// failure (timeout, refused/unreachable endpoint, throttle, 5xx) before the
/// turn gives up. Keeps a flaky local server or a brief network blip from
/// killing a turn, without masking a persistently-down provider.
const MAX_STREAM_RETRIES: usize = 3;

/// Base backoff before the first retry, doubled each attempt and capped by
/// [`RETRY_MAX_DELAY_MS`]. A throttle (HTTP 429) waits longer — see
/// [`retry_delay_ms`].
const RETRY_BASE_DELAY_MS: u64 = 400;

/// Upper bound on a single backoff wait, so the doubling can't stall a turn for
/// long.
const RETRY_MAX_DELAY_MS: u64 = 5_000;

/// Compute the backoff for a 1-based retry `attempt`: exponential off
/// [`RETRY_BASE_DELAY_MS`] (a larger base when the provider throttled us),
/// capped at [`RETRY_MAX_DELAY_MS`], plus a little clock-derived jitter so
/// concurrent agents don't retry in lockstep.
fn retry_delay_ms(attempt: usize, rate_limited: bool) -> u64 {
    let base = if rate_limited {
        2_000
    } else {
        RETRY_BASE_DELAY_MS
    };
    let exp = base.saturating_mul(1u64 << (attempt.saturating_sub(1)).min(16));
    let capped = exp.min(RETRY_MAX_DELAY_MS);
    // Cheap, dependency-free jitter in [0, 100) ms derived from the wall clock.
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_millis()) % 100)
        .unwrap_or(0);
    capped.saturating_add(jitter)
}

/// How a single turn ended. The terminal `Done` event is emitted by the
/// caller, not [`Agent::run_turn_step`], so an implementation-session driver
/// can run many turns and emit `Done` exactly once at the end. `Interrupted`
/// and `Errored` carry no further obligation — their events are already sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOutcome {
    /// The model settled the turn (no more tool calls).
    Completed,
    /// The user interrupted; an `Interrupted` event was emitted.
    Interrupted,
    /// A transport or iteration-limit error ended the turn; an `Error` event
    /// was emitted.
    Errored,
}

/// Which half of an implementation step the per-task driver is working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The work tasks (`w*`).
    Work,
    /// The verify tasks (`v*`), entered only after the user approves the gate.
    Verify,
}

impl Phase {
    /// The derived-id prefix this phase's tasks carry.
    fn prefix(self) -> char {
        match self {
            Phase::Work => 'w',
            Phase::Verify => 'v',
        }
    }
}

/// The result of a summarization stream (used by both `/compact` and the
/// silent per-task handoff). `Failed` carries the error for the caller to
/// surface or swallow; the helper itself emits nothing.
enum SummaryResult {
    Done(String),
    Interrupted,
    Failed(String),
}

/// How a delegated sub-agent turn (Phase 4) ended, from the parent's view.
enum SubAgent {
    /// The sub-agent settled; carries the handoff note used as the tool result.
    Finished(String),
    /// The user interrupted during the sub-turn; the parent turn unwinds.
    Interrupted,
    /// The sub-agent errored or hit its ceiling; carries the model-facing reason.
    Errored(String),
}

/// The instruction for a silent per-task handoff summary: one task's worth of
/// work, condensed to a couple of sentences for the ledger.
const TASK_SUMMARY_PROMPT: &str = "\
You are recording a brief handoff note for one finished task in a coding \
session. In 1-3 sentences, state what was actually done and any decision or \
gotcha worth carrying forward. Be terse and factual — no preamble, no \
pleasantries. Write the note and nothing else.";

/// The instruction that drives `/compact`: a system prompt asking the model to
/// summarize the conversation so far densely enough to continue from.
const COMPACT_PROMPT: &str = "\
You are compacting a coding session's conversation to save context. Summarize \
everything so far so work can continue from the summary alone: the user's \
goals, decisions made, files and symbols touched, what is done, what remains, \
and any open problems. Be dense and factual — no pleasantries, no preamble. \
Write the summary and nothing else.";

/// Owns the transport and tool set, and runs turns against a [`Session`],
/// emitting [`AgentEvent`]s on its channel.
pub struct Agent {
    transport: Box<dyn Transport>,
    /// Shared so a tool body can be moved onto a blocking thread per call (see
    /// [`ToolExecutor::execute`]).
    tools: Arc<[Box<dyn Tool>]>,
    events: mpsc::Sender<AgentEvent>,
    /// Signalled by the UI to abandon the running turn. Without
    /// [`Agent::with_interrupt`] the sender is already dropped and the signal
    /// never fires.
    interrupt: watch::Receiver<()>,
}

/// Resolves when the interrupt signal fires. If no UI holds the sender the
/// future never resolves, so a `select!` against it degrades to waiting on
/// the other branch alone.
async fn user_interrupt(signal: &mut watch::Receiver<()>) {
    if signal.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

impl Agent {
    /// Create an agent with the default tool set.
    pub fn new(transport: Box<dyn Transport>, events: mpsc::Sender<AgentEvent>) -> Self {
        Agent {
            transport,
            tools: default_tools().into(),
            events,
            interrupt: watch::channel(()).1,
        }
    }

    /// Create an agent with an explicit tool set (used in tests).
    pub fn with_tools(
        transport: Box<dyn Transport>,
        tools: Vec<Box<dyn Tool>>,
        events: mpsc::Sender<AgentEvent>,
    ) -> Self {
        Agent {
            transport,
            tools: tools.into(),
            events,
            interrupt: watch::channel(()).1,
        }
    }

    /// Install the UI's interrupt signal: a send on the paired
    /// [`watch::Sender`] abandons the running turn at the next safe point.
    pub fn with_interrupt(mut self, signal: watch::Receiver<()>) -> Self {
        self.interrupt = signal;
        self
    }

    /// The definitions of this agent's tools, for context assembly.
    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|t| t.definition()).collect()
    }

    /// Run one user turn to completion (or until the user interrupts), emitting
    /// the terminal [`AgentEvent::Done`] when it settles normally.
    pub async fn run_turn(&self, session: &mut Session, user_input: impl Into<String>) {
        if self.run_turn_step(session, user_input).await == TurnOutcome::Completed {
            self.emit(AgentEvent::Done).await;
        }
    }

    /// Run one turn and report how it ended, *without* emitting `Done` — so a
    /// caller running several turns (the implementation-session driver) can emit
    /// the terminal event once. `Interrupted`/`Errored` events are still emitted
    /// here, since they end the turn regardless of who drives it.
    pub async fn run_turn_step(
        &self,
        session: &mut Session,
        user_input: impl Into<String>,
    ) -> TurnOutcome {
        // The turn answers only interrupts requested after it starts; a press
        // that raced the end of the previous turn is not carried over.
        let mut interrupt = self.interrupt.clone();
        interrupt.mark_unchanged();
        self.run_turn_inner(
            session,
            user_input.into(),
            MAX_ITERATIONS,
            true,
            &mut interrupt,
        )
        .await
    }

    /// The turn loop, parameterized for both top-level turns and delegated
    /// sub-agent turns (Phase 4): `max_iterations` bounds the tool-call loop,
    /// `allow_subagents` controls whether the sub-agent tools (`explore`,
    /// `find`, `delegate`) are offered and honored (a sub-agent gets none —
    /// re-entrant delegation is refused, bounding depth to 1), and the caller
    /// owns the `interrupt` receiver so a press propagates into a nested turn.
    /// Wait out the backoff before retrying a transient model failure, emitting a
    /// [`AgentEvent::Retrying`] so the UI shows progress rather than appearing
    /// hung. Returns `true` once the wait completes (retry now) or `false` if the
    /// user interrupted during the wait (the caller should abandon the turn).
    async fn backoff_retry(
        &self,
        attempt: usize,
        error: &suis_core::Error,
        interrupt: &mut watch::Receiver<()>,
    ) -> bool {
        let rate_limited = matches!(
            error,
            suis_core::Error::Provider(suis_core::ProviderError::RateLimited(_))
        );
        let delay_ms = retry_delay_ms(attempt, rate_limited);
        self.emit(AgentEvent::Retrying {
            attempt,
            max: MAX_STREAM_RETRIES,
            reason: error.to_string(),
            delay_ms,
        })
        .await;
        tokio::select! {
            _ = user_interrupt(interrupt) => false,
            _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => true,
        }
    }

    async fn run_turn_inner(
        &self,
        session: &mut Session,
        user_input: String,
        max_iterations: usize,
        allow_subagents: bool,
        interrupt: &mut watch::Receiver<()>,
    ) -> TurnOutcome {
        session.history.push(Message::text(Role::User, user_input));
        // The assembler advertises only mode-allowed tools; the executor
        // independently refuses disallowed calls, so the mode boundary holds
        // even if the model hallucinates a tool it was never offered. A
        // sub-agent never sees the sub-agent tools, so it cannot recurse.
        let tool_defs: Vec<ToolDefinition> = self
            .tool_definitions()
            .into_iter()
            .filter(|d| allow_subagents || !subagent::is_subagent(&d.name))
            .collect();

        // Phase 2 (self-verification) bookkeeping. `verify_anchor` marks the
        // point in history after the last verification (initially the start of
        // this turn): a re-verify only fires when *new* edits land past it, so
        // a model that just talks after a failure isn't re-checked. `verify_rounds`
        // caps the verify→fix cycle so a perpetually-failing build can't loop.
        let mut verify_anchor = session.history.len();
        let mut verify_rounds = 0usize;

        // Repetition guard: signatures of read-only calls already issued this
        // turn, the count of exact repeats, and whether the one-shot reminder
        // has fired. All reset per turn (they live for this call only).
        let mut seen_read_sigs: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut redundant_reads = 0usize;
        let mut repetition_noted = false;

        for _ in 0..max_iterations {
            // An interrupt during the previous iteration's final tool call
            // lands here, before another model request is spent.
            if interrupt.has_changed().unwrap_or(false) {
                self.emit(AgentEvent::Interrupted).await;
                return TurnOutcome::Interrupted;
            }
            let active = session.tasks.lock().ok().and_then(|t| t.active().cloned());
            let assembled = ContextAssembler::build(
                &session.workspace,
                &session.model.model_id,
                &session.model.capabilities,
                &session.project,
                session.mode,
                active.as_ref(),
                session.history.as_slice(),
                &tool_defs,
                session.context_budget,
                session.implement.is_some(),
            );
            // Report context pressure (and any pruning) before the request runs.
            self.emit(AgentEvent::ContextUsage {
                used_tokens: assembled.used_tokens,
                budget: session.context_budget,
                pruned: assembled.pruned,
            })
            .await;

            // Stream the model's response, accumulating text and tool calls. The
            // request is re-sendable, so a *transient* failure (a dropped stream,
            // a 5xx, a brief timeout, a throttle) is retried with backoff instead
            // of ending the turn. Retries happen inside this iteration so they
            // don't consume the model-iteration budget. We only retry while no
            // user-visible text has streamed yet — once an answer/reasoning chunk
            // has been shown, re-sending would duplicate it, so a failure past
            // that point is surfaced as an error (today's behaviour).
            let request = assembled.request;
            let mut content = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            // Assigned at the top of every attempt (see below); declared here so
            // they outlive the retry loop for the post-stream flush/accounting.
            let mut usage;
            let mut interrupted;
            // Peels inline `<think>…</think>` reasoning out of the text stream so
            // it reaches the UI's thinking block instead of the answer (and never
            // the recorded history).
            let mut think;
            let mut retries = 0usize;
            'retry: loop {
                // Each attempt starts from a clean slate so partial output from a
                // failed attempt can't leak into the next one.
                content.clear();
                tool_calls.clear();
                usage = None;
                interrupted = false;
                think = ThinkSplitter::default();
                // Whether any user-visible text/reasoning has streamed this
                // attempt. Tool-call deltas don't count: they're accumulated here
                // and not shown until execution, so re-sending can't duplicate
                // them.
                let mut emitted_output = false;

                let mut stream = match self.transport.chat_stream(request.clone()).await {
                    Ok(s) => s,
                    Err(e) => {
                        if e.is_transient() && retries < MAX_STREAM_RETRIES {
                            retries += 1;
                            if self.backoff_retry(retries, &e, interrupt).await {
                                continue 'retry;
                            }
                            self.emit(AgentEvent::Interrupted).await;
                            return TurnOutcome::Interrupted;
                        }
                        self.emit(AgentEvent::Error(e.to_string())).await;
                        return TurnOutcome::Errored;
                    }
                };

                loop {
                    let chunk = tokio::select! {
                        _ = user_interrupt(interrupt) => {
                            interrupted = true;
                            break;
                        }
                        chunk = stream.next() => match chunk {
                            Some(chunk) => chunk,
                            None => break,
                        },
                    };
                    match chunk {
                        Ok(resp) => {
                            // A provider's dedicated reasoning channel passes straight
                            // through to the thinking block.
                            if !resp.reasoning.is_empty() {
                                emitted_output = true;
                                self.emit(AgentEvent::ReasoningChunk(resp.reasoning)).await;
                            }
                            if !resp.content.is_empty() {
                                let (answer, reasoning) = think.push(&resp.content);
                                if !reasoning.is_empty() {
                                    emitted_output = true;
                                    self.emit(AgentEvent::ReasoningChunk(reasoning)).await;
                                }
                                if !answer.is_empty() {
                                    emitted_output = true;
                                    content.push_str(&answer);
                                    self.emit(AgentEvent::StreamChunk(answer)).await;
                                }
                            }
                            tool_calls.extend(resp.tool_calls);
                            if let Some(u) = resp.usage {
                                // A mid-stream chunk carrying usage is an early
                                // prompt-size report (Anthropic, at message_start):
                                // surface the exact input now so the UI stops riding
                                // its estimate. The terminal chunk (`done`) carries
                                // the final counts handled as TokenUsage below.
                                if !resp.done && u.prompt_tokens > 0 {
                                    self.emit(AgentEvent::PromptTokens {
                                        prompt_tokens: u.prompt_tokens,
                                    })
                                    .await;
                                }
                                usage = Some(u);
                            }
                        }
                        Err(e) => {
                            // A transient failure before anything was shown is safe
                            // to retry from scratch; once output streamed, surface
                            // it instead of duplicating text on a re-send.
                            if e.is_transient() && !emitted_output && retries < MAX_STREAM_RETRIES {
                                retries += 1;
                                if self.backoff_retry(retries, &e, interrupt).await {
                                    continue 'retry;
                                }
                                self.emit(AgentEvent::Interrupted).await;
                                return TurnOutcome::Interrupted;
                            }
                            self.emit(AgentEvent::Error(e.to_string())).await;
                            return TurnOutcome::Errored;
                        }
                    }
                }
                break 'retry;
            }

            // Flush any fragment the splitter held back (a partial tag, or an
            // unterminated `<think>` block) now the stream has ended.
            let (answer, reasoning) = think.flush();
            if !reasoning.is_empty() {
                self.emit(AgentEvent::ReasoningChunk(reasoning)).await;
            }
            if !answer.is_empty() {
                content.push_str(&answer);
                self.emit(AgentEvent::StreamChunk(answer)).await;
            }

            // Report the provider's real token counts for this request, so the
            // UI can show actual context occupancy and a running session total.
            // One request per turn-iteration; a tool-using turn reports again on
            // each follow-up request.
            if let Some(u) = usage {
                self.emit(AgentEvent::TokenUsage {
                    prompt_tokens: u.prompt_tokens,
                    completion_tokens: u.completion_tokens,
                })
                .await;
            }

            if interrupted {
                // Keep what was said; drop the never-started tool calls so the
                // recorded conversation needs no answers for them.
                if !content.is_empty() {
                    session.history.push(Message {
                        role: Role::Assistant,
                        content,
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                    });
                }
                self.emit(AgentEvent::Interrupted).await;
                return TurnOutcome::Interrupted;
            }

            // Some models print tool calls as text (Hermes-style tags or fenced
            // JSON) instead of using the structured channel. When the structured
            // channel is empty, recover those so the turn continues instead of
            // ending prematurely; the matched spans are stripped from the
            // recorded prose.
            if tool_calls.is_empty() {
                let recovered = suis_providers::parse_text_tool_calls(&content);
                if !recovered.calls.is_empty() {
                    content = recovered.cleaned;
                    tool_calls = recovered.calls;
                }
            }

            session.history.push(Message {
                role: Role::Assistant,
                content,
                tool_calls: tool_calls.clone(),
                tool_call_id: None,
            });

            if tool_calls.is_empty() {
                // Phase 2: before settling, run the project's verify command if
                // this Agent-mode turn edited files since the last check. A
                // failure is fed back so the model can self-correct and loop;
                // success (or no verify configured / no edits / cap reached)
                // settles the turn.
                if self.should_verify(session)
                    && verify_rounds < MAX_VERIFY_ROUNDS
                    && turn_edited(&session.history.as_slice()[verify_anchor..])
                {
                    let command = session
                        .project
                        .verify_command
                        .clone()
                        .expect("should_verify checked verify_command is set");
                    verify_rounds += 1;
                    self.emit(AgentEvent::VerifyStarted {
                        command: command.clone(),
                    })
                    .await;
                    let result = self.run_verification(session, &command).await;
                    let passed = !result.is_error;
                    self.emit(AgentEvent::VerifyResult {
                        passed,
                        summary: verify_summary(passed, &result.content),
                    })
                    .await;
                    if passed {
                        return TurnOutcome::Completed;
                    }
                    // Feed the failure back as a system note and loop so the
                    // model fixes it. The anchor advances past the note, so the
                    // next verify only fires if the fix actually edits again.
                    session.history.push(Message::text(
                        Role::System,
                        format!(
                            "Automatic verification failed for `{command}`:\n{}\n\
                             Fix the problem, then continue.",
                            result.content
                        ),
                    ));
                    verify_anchor = session.history.len();
                    continue;
                }
                return TurnOutcome::Completed;
            }

            if let Some(outcome) = self
                .dispatch_tools(session, &tool_calls, allow_subagents, interrupt)
                .await
            {
                return outcome;
            }

            // Repetition guard: count exact-duplicate read-only calls and, once
            // the model is clearly re-treading rather than exploring, append a
            // single non-coercive reminder *after* the tool results (so it is
            // the most recent context and the tool-call→result pairing stays
            // valid). Distinct reads never trip it.
            for call in &tool_calls {
                if is_read_only(&call.name) {
                    let sig = format!("{}\u{1f}{}", call.name, call.arguments);
                    if !seen_read_sigs.insert(sig) {
                        redundant_reads += 1;
                    }
                }
            }
            if !repetition_noted && redundant_reads >= REDUNDANT_READ_LIMIT {
                session
                    .history
                    .push(Message::text(Role::System, REPETITION_NOTE));
                repetition_noted = true;
            }
        }

        // Hit the iteration ceiling without the model settling.
        self.emit(AgentEvent::Error(format!(
            "stopped after {max_iterations} tool iterations"
        )))
        .await;
        TurnOutcome::Errored
    }

    /// Compact the conversation on the user's request: ask the model to
    /// summarize the history, then replace it with that summary.
    ///
    /// This is the one place Suis spends a model call to manage context, and
    /// only ever when the user asks — automatic compaction is out of scope.
    /// The summary is built before anything is destroyed: a transport error
    /// leaves `session.history` exactly as it was and reports the error, so the
    /// conversation is never lost to a failed compaction. In an implementation
    /// session the work package is re-injected after the summary so the
    /// objective survives.
    pub async fn compact(&self, session: &mut Session) {
        let mut interrupt = self.interrupt.clone();
        interrupt.mark_unchanged();

        if session.history.is_empty() {
            self.emit(AgentEvent::Compacted {
                summary: String::new(),
            })
            .await;
            return;
        }

        // Summarization context: the standing prompt, the conversation, and a
        // final instruction. No tools — this is a single, plain completion that
        // streams to the UI (`emit = true`), so the user watches it form.
        let mut messages = Vec::with_capacity(session.history.len() + 2);
        messages.push(Message::text(Role::System, COMPACT_PROMPT));
        messages.extend(session.history.as_slice().iter().cloned());
        messages.push(Message::text(
            Role::User,
            "Summarize the conversation so far as instructed.",
        ));

        let summary = match self
            .summarize(&session.model.model_id, messages, &mut interrupt, true)
            .await
        {
            // The summary exists — now it is safe to replace the history.
            SummaryResult::Done(summary) => summary,
            // The summary is incomplete; leave history untouched.
            SummaryResult::Interrupted => {
                self.emit(AgentEvent::Interrupted).await;
                return;
            }
            SummaryResult::Failed(e) => {
                self.emit(AgentEvent::Error(e)).await;
                return;
            }
        };

        session.history.clear();
        session.history.push(Message::text(
            Role::System,
            format!("Summary of the prior conversation:\n{summary}"),
        ));

        // An implementation session keeps its objective (and its ledger) across
        // compaction, re-seeded the same way the per-task driver seeds a task.
        if let Some(target) = session.implement.clone() {
            self.seed_implement_context(session, &target);
        }

        self.emit(AgentEvent::Compacted { summary }).await;
    }

    /// Stream a summarization completion to its end. Shared by `/compact`
    /// (visible, `emit = true`) and the silent per-task handoff (`emit =
    /// false`). Touches no session state and emits no terminal event — the
    /// caller decides what a `Done`/`Interrupted`/`Failed` result means.
    async fn summarize(
        &self,
        model_id: &str,
        messages: Vec<Message>,
        interrupt: &mut watch::Receiver<()>,
        emit: bool,
    ) -> SummaryResult {
        let request = ChatRequest {
            model: model_id.to_string(),
            messages,
            tools: None,
            stream: true,
        };

        let mut stream = match self.transport.chat_stream(request).await {
            Ok(s) => s,
            Err(e) => return SummaryResult::Failed(e.to_string()),
        };

        let mut summary = String::new();
        loop {
            let chunk = tokio::select! {
                _ = user_interrupt(interrupt) => return SummaryResult::Interrupted,
                chunk = stream.next() => match chunk {
                    Some(chunk) => chunk,
                    None => break,
                },
            };
            match chunk {
                Ok(resp) => {
                    if !resp.content.is_empty() {
                        summary.push_str(&resp.content);
                        if emit {
                            self.emit(AgentEvent::StreamChunk(resp.content)).await;
                        }
                    }
                }
                Err(e) => return SummaryResult::Failed(e.to_string()),
            }
        }
        SummaryResult::Done(summary)
    }

    /// Drive one phase of an implementation step task-by-task, resetting the
    /// agent's working context between tasks.
    ///
    /// For each still-open task, in order: seed a lean context (the bodyless
    /// work package + the running ledger), run a single turn pointed at that
    /// task, then fold what was done into the ledger (a deterministic record of
    /// files touched plus a short, silent model summary) and emit a
    /// [`AgentEvent::TaskCompacted`] marker. The full working transcript never
    /// crosses into the next task — only the ledger does.
    ///
    /// Stops and emits a single terminal [`AgentEvent::Done`] when the phase has
    /// no open tasks left (for [`Phase::Work`] that lets the UI open the verify
    /// gate) or when a turn made no progress (so a stuck model can't loop). An
    /// interrupt or error during a turn ends the driver immediately; that turn
    /// already emitted its own event.
    pub async fn run_implement_phase(&self, session: &mut Session, phase: Phase) {
        let mut interrupt = self.interrupt.clone();
        interrupt.mark_unchanged();

        let Some(target) = session.implement.clone() else {
            self.emit(AgentEvent::Error(
                "no implementation session is active".into(),
            ))
            .await;
            return;
        };

        loop {
            // An interrupt requested between tasks stops the driver before the
            // next turn is spent.
            if interrupt.has_changed().unwrap_or(false) {
                self.emit(AgentEvent::Interrupted).await;
                return;
            }

            let tasks = match phase_tasks(&session.workspace, &target, phase) {
                Ok(tasks) => tasks,
                Err(e) => {
                    self.emit(AgentEvent::Error(e)).await;
                    return;
                }
            };
            // The next actionable task (todo or resumed-doing); blocked/done are
            // skipped. None left → the phase is finished.
            let Some(current) = tasks
                .iter()
                .find(|t| matches!(t.status, TaskStatus::Todo | TaskStatus::Doing))
                .cloned()
            else {
                self.emit(AgentEvent::Done).await;
                return;
            };
            // Both `done` and `blocked` are terminal: a task that reaches either
            // is settled and the driver should move on. Tracking both lets a
            // model honestly mark a task it cannot complete as `blocked` and
            // still advance, instead of being cornered into a false `done`.
            let settled_before: Vec<String> = tasks
                .iter()
                .filter(|t| matches!(t.status, TaskStatus::Done | TaskStatus::Blocked))
                .map(|t| t.id.clone())
                .collect();

            // The driver owns the todo→doing transition: put the current task in
            // `doing` before its turn so the model never has to, and the task
            // panel shows it as in progress. The agent's only task-tool move is
            // then to settle it (done, or blocked). Done before seeding so the
            // work package and the panel snapshot both reflect `doing`.
            mark_task_doing(&session.workspace, &target, &current.id);

            // Seed a fresh, lean context for just this task.
            session.history.clear();
            self.seed_implement_context(session, &target);
            let seed_len = session.history.len();
            // Refresh the task panel for the (possibly resumed) step.
            self.emit(AgentEvent::TaskUpdated(session.task_snapshot()))
                .await;

            let gate = match phase {
                // The work package warns against starting verify tasks early;
                // here the user has approved, so say so to clear that.
                Phase::Verify => "The user has approved verification. ",
                Phase::Work => "",
            };
            let pointer = format!(
                "{gate}Your current task is {id} ({title}) — it is already marked \
                 'doing'. Make the change by editing files now; do not write the \
                 whole solution out in your reasoning. When it is complete, mark \
                 {id} 'done' with the task tool (or 'blocked', with a one-line \
                 reason, if you cannot complete it as described). Do only this \
                 task, and never mark a task 'done' that you did not actually do.",
                id = current.id,
                title = current.title,
            );
            match self.run_turn_step(session, pointer).await {
                TurnOutcome::Completed => {}
                // The turn already emitted Interrupted/Error; just stop driving.
                TurnOutcome::Interrupted | TurnOutcome::Errored => return,
            }

            // Which tasks newly settled (done *or* blocked) during that turn —
            // robust to the model finishing a different or extra task than the
            // one pointed at. Counting `blocked` as progress lets an honestly
            // un-completable task advance the driver instead of stalling it.
            let after = phase_tasks(&session.workspace, &target, phase).unwrap_or_default();
            let newly: Vec<Task> = after
                .into_iter()
                .filter(|t| {
                    matches!(t.status, TaskStatus::Done | TaskStatus::Blocked)
                        && !settled_before.contains(&t.id)
                })
                .collect();

            if newly.is_empty() {
                // No status changed — the model stalled or asked the user
                // something. Hand control back rather than re-seeding the same
                // task forever. (A task it deliberately marked `blocked` is a
                // status change, so it does not land here.)
                self.emit(AgentEvent::Done).await;
                return;
            }

            // The hybrid handoff: a deterministic record of the turn plus one
            // silent model summary, attached to the first completed task; any
            // others completed in the same turn get a deterministic note.
            let work = &session.history.as_slice()[seed_len..];
            let touched = touched_paths(work);
            let summary = self
                .task_summary(&session.model.model_id, work, &mut interrupt)
                .await;
            for (i, task) in newly.iter().enumerate() {
                let blocked = task.status == TaskStatus::Blocked;
                let entry = if i == 0 {
                    LedgerEntry {
                        id: task.id.clone(),
                        title: task.title.clone(),
                        summary: summary.clone(),
                        touched: touched.clone(),
                        blocked,
                    }
                } else {
                    LedgerEntry {
                        id: task.id.clone(),
                        title: task.title.clone(),
                        summary: String::new(),
                        touched: Vec::new(),
                        blocked,
                    }
                };
                session.ledger.push(entry);
                self.emit(AgentEvent::TaskCompacted {
                    id: task.id.clone(),
                    title: task.title.clone(),
                })
                .await;
            }
        }
    }

    /// Push the implementation session's opening context onto a freshly cleared
    /// history: the bodyless work package, then the running ledger (if any). The
    /// shared seed for both the per-task driver and `/compact` re-injection.
    fn seed_implement_context(&self, session: &mut Session, target: &ImplementTarget) {
        if let Ok(package) = work_package::assemble(
            &session.workspace,
            &session.project,
            &target.plan_id,
            target.step_index,
        ) {
            session.history.push(Message::text(Role::User, package));
        }
        if let Some(ledger) = work_package::render_ledger(&session.ledger) {
            session.history.push(Message::text(Role::System, ledger));
        }
    }

    /// A short, silent summary of one task's working messages for the ledger.
    /// Best-effort: an empty string when the summary call fails or is
    /// interrupted — the deterministic ledger fields stand on their own.
    async fn task_summary(
        &self,
        model_id: &str,
        work: &[Message],
        interrupt: &mut watch::Receiver<()>,
    ) -> String {
        let mut messages = Vec::with_capacity(work.len() + 2);
        messages.push(Message::text(Role::System, TASK_SUMMARY_PROMPT));
        messages.extend(work.iter().cloned());
        messages.push(Message::text(
            Role::User,
            "Write the handoff note for this task as instructed.",
        ));
        match self.summarize(model_id, messages, interrupt, false).await {
            SummaryResult::Done(summary) => summary.trim().to_string(),
            SummaryResult::Interrupted | SummaryResult::Failed(_) => String::new(),
        }
    }

    /// Whether Phase-2 auto-verification applies to this session: only in Agent
    /// mode, only with a configured `verify_command`, and never inside an
    /// `/implement` session (which has its own explicit work→verify gate).
    fn should_verify(&self, session: &Session) -> bool {
        session.mode == crate::runtime::Mode::Agent
            && session.implement.is_none()
            && session.project.verify_command.is_some()
    }

    /// Run the project's verify command once, through the normal permission
    /// path: a synthesized `bash` call routed via [`ToolExecutor`], so it
    /// inherits the command gate, dangerous-command rules, output capping, and
    /// timeout. No new execution path, no bypass.
    async fn run_verification(&self, session: &mut Session, command: &str) -> ToolResult {
        let call = ToolCall {
            id: "verify".to_string(),
            name: "bash".to_string(),
            arguments: serde_json::json!({ "command": command }),
        };
        let mut executor = ToolExecutor::new(
            &session.workspace,
            &session.project,
            &mut session.permissions,
            &session.tasks,
            &session.access,
            Arc::clone(&self.tools),
            session.mode,
            session.implement.clone(),
            &self.events,
        );
        executor.execute(&call).await
    }

    /// Resolve a sub-agent call (Phase 4): run a nested turn against a fresh,
    /// lean context following the given [`SubAgentProfile`], and fold back only a
    /// dense handoff note. The parent's history *and* mode are set aside for the
    /// sub-turn and restored after, so the full sub-transcript never crosses into
    /// the parent's context — only the note (the tool result) does — and the
    /// profile's (possibly read-only) mode governs the sub-turn regardless of the
    /// parent's mode. Re-entrant delegation is refused (`allow_subagents` is
    /// false inside a sub-agent), bounding depth to 1.
    async fn run_subagent(
        &self,
        session: &mut Session,
        call: &ToolCall,
        profile: &SubAgentProfile,
        allow_subagents: bool,
        interrupt: &mut watch::Receiver<()>,
    ) -> SubAgent {
        if !allow_subagents {
            return SubAgent::Errored(format!(
                "a sub-agent cannot spawn another sub-agent ('{}'); complete this subtask directly.",
                profile.name
            ));
        }
        let objective = match call.arguments.get("objective").and_then(|v| v.as_str()) {
            Some(o) if !o.trim().is_empty() => o.trim().to_string(),
            _ => {
                return SubAgent::Errored(format!(
                    "{} requires a non-empty 'objective'.",
                    profile.name
                ))
            }
        };
        let context_hint = call
            .arguments
            .get("context_hint")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        self.emit(AgentEvent::SubAgentStarted {
            kind: profile.name.to_string(),
            objective: objective.clone(),
        })
        .await;

        // Set the parent context aside; the sub-agent works on a fresh history
        // and the profile's mode (read-only for the recon profiles, enforced at
        // both the assembler and the executor). The project profile rides in the
        // system prompt automatically; the role seed and the running ledger (if
        // any) hand the sub-agent its brief and the parent's gist.
        let parent_history = std::mem::replace(&mut session.history, ConversationHistory::new());
        let parent_mode = session.mode;
        session.mode = profile.sub_mode;
        if !profile.seed_prompt.is_empty() {
            session
                .history
                .push(Message::text(Role::System, profile.seed_prompt));
        }
        if let Some(ledger) = work_package::render_ledger(&session.ledger) {
            session.history.push(Message::text(Role::System, ledger));
        }
        let seed_len = session.history.len();
        let input = match context_hint {
            Some(hint) => format!("{objective}\n\nContext to start from: {hint}"),
            None => objective,
        };

        // Boxed: `run_turn_inner → dispatch_tools → run_subagent → run_turn_inner`
        // is an async cycle, so the recursive future needs indirection.
        let outcome =
            Box::pin(self.run_turn_inner(session, input, profile.max_iterations, false, interrupt))
                .await;

        let result = match outcome {
            TurnOutcome::Completed => {
                let work = &session.history.as_slice()[seed_len..];
                let summary = self
                    .subagent_summary(&session.model.model_id, profile, work, interrupt)
                    .await;
                let note = if summary.is_empty() {
                    "The sub-agent finished but produced no summary.".to_string()
                } else {
                    summary
                };
                self.emit(AgentEvent::SubAgentFinished {
                    kind: profile.name.to_string(),
                    summary: note.clone(),
                })
                .await;
                SubAgent::Finished(note)
            }
            TurnOutcome::Interrupted => SubAgent::Interrupted,
            TurnOutcome::Errored => SubAgent::Errored(
                "the sub-agent did not finish (it errored or hit its iteration limit).".to_string(),
            ),
        };

        // Restore the parent context and mode; only the note (the tool result)
        // crosses back.
        session.history = parent_history;
        session.mode = parent_mode;
        result
    }

    /// A short, silent handoff note summarizing a sub-agent's work, for the
    /// parent's tool result, shaped by the profile's `summary_prompt`.
    /// Best-effort: an empty string when the summary call fails or is interrupted.
    async fn subagent_summary(
        &self,
        model_id: &str,
        profile: &SubAgentProfile,
        work: &[Message],
        interrupt: &mut watch::Receiver<()>,
    ) -> String {
        let mut messages = Vec::with_capacity(work.len() + 2);
        messages.push(Message::text(Role::System, profile.summary_prompt));
        messages.extend(work.iter().cloned());
        messages.push(Message::text(
            Role::User,
            "Write the handoff note for this sub-task as instructed.",
        ));
        match self.summarize(model_id, messages, interrupt, false).await {
            SummaryResult::Done(summary) => summary.trim().to_string(),
            SummaryResult::Interrupted | SummaryResult::Failed(_) => String::new(),
        }
    }

    /// Execute one model response's tool calls and fold their results back into
    /// the history. Concurrently-safe orientation calls (`search`/`tree`) run in
    /// parallel; everything else — including `read_lines` — runs sequentially
    /// after them, so a turn can `search` a file and then `read_lines` it.
    /// Per-call UI events
    /// are emitted, and results are recorded as `Tool` messages in original call
    /// order so every call id is answered.
    ///
    /// Returns `Some(TurnOutcome::Interrupted)` if the user interrupted — the
    /// history is left valid (a running tool finishes; not-yet-started calls get
    /// synthetic results) — or `None` to continue the turn.
    async fn dispatch_tools(
        &self,
        session: &mut Session,
        calls: &[ToolCall],
        allow_subagents: bool,
        interrupt: &mut watch::Receiver<()>,
    ) -> Option<TurnOutcome> {
        // Filled in original call order; `None` marks a skipped (interrupted)
        // call, which becomes a synthetic result below.
        let mut results: Vec<Option<ToolResult>> = (0..calls.len()).map(|_| None).collect();
        let read_idx: Vec<usize> = (0..calls.len())
            .filter(|&i| runs_concurrently(&calls[i].name))
            .collect();
        let other_idx: Vec<usize> = (0..calls.len())
            .filter(|&i| !runs_concurrently(&calls[i].name))
            .collect();

        // An interrupt requested before any tool ran skips the whole batch; the
        // recorded calls still each get a synthetic result below.
        let mut interrupted = interrupt.has_changed().unwrap_or(false);

        // --- concurrent subset (search/tree): gate in order (needs &mut), then run concurrently ---
        if !interrupted && !read_idx.is_empty() {
            for &i in &read_idx {
                self.emit(AgentEvent::ToolCallStarted {
                    name: calls[i].name.clone(),
                    args: calls[i].arguments.clone(),
                })
                .await;
            }
            let gated: Vec<crate::tools::executor::Gated> = {
                let mut executor = ToolExecutor::new(
                    &session.workspace,
                    &session.project,
                    &mut session.permissions,
                    &session.tasks,
                    &session.access,
                    Arc::clone(&self.tools),
                    session.mode,
                    session.implement.clone(),
                    &self.events,
                );
                let mut gated = Vec::with_capacity(read_idx.len());
                for &i in &read_idx {
                    gated.push(executor.gate_call(&calls[i]).await);
                }
                gated
            };
            // Shared (non-`&mut`) bindings so the body futures can run
            // concurrently without moving `session`; scoped to this block so the
            // borrows release before the history is mutated below.
            let read_results = {
                let workspace = &session.workspace;
                let project = &session.project;
                let tasks = &session.tasks;
                let access = &session.access;
                let implement = session.implement.as_ref();
                let tools = &self.tools;
                let bodies = read_idx.iter().zip(gated).map(|(&i, gate)| {
                    let call = &calls[i];
                    async move {
                        match gate {
                            crate::tools::executor::Gated::Resolved(result) => result,
                            crate::tools::executor::Gated::Proceed => {
                                crate::tools::executor::run_tool_body(
                                    workspace, project, tasks, access, tools, implement, call,
                                )
                                .await
                            }
                        }
                    }
                });
                futures::future::join_all(bodies).await
            };
            for (&i, result) in read_idx.iter().zip(read_results) {
                self.emit(AgentEvent::ToolCallCompleted {
                    result: result.clone(),
                })
                .await;
                results[i] = Some(result);
            }
        }

        // --- the rest: sequential, with an interrupt checked before each ---
        for &i in &other_idx {
            // A running tool always finishes; an interrupt takes effect here,
            // before the next call starts.
            if interrupted || interrupt.has_changed().unwrap_or(false) {
                interrupted = true;
                break;
            }
            // A sub-agent tool (`explore`/`find`/`delegate`) is resolved by the
            // agent loop, not the executor: it runs a nested sub-agent turn and
            // folds back only a summary. It emits its own SubAgent events instead
            // of a tool card.
            if let Some(profile) = subagent::profile(&calls[i].name) {
                match self
                    .run_subagent(session, &calls[i], profile, allow_subagents, interrupt)
                    .await
                {
                    SubAgent::Finished(summary) => {
                        results[i] = Some(ToolResult::ok(&calls[i].id, summary));
                    }
                    SubAgent::Errored(message) => {
                        results[i] = Some(ToolResult::error(&calls[i].id, message));
                    }
                    SubAgent::Interrupted => {
                        interrupted = true;
                        break;
                    }
                }
                continue;
            }
            self.emit(AgentEvent::ToolCallStarted {
                name: calls[i].name.clone(),
                args: calls[i].arguments.clone(),
            })
            .await;
            let result = {
                let mut executor = ToolExecutor::new(
                    &session.workspace,
                    &session.project,
                    &mut session.permissions,
                    &session.tasks,
                    &session.access,
                    Arc::clone(&self.tools),
                    session.mode,
                    session.implement.clone(),
                    &self.events,
                );
                executor.execute(&calls[i]).await
            };
            self.emit(AgentEvent::ToolCallCompleted {
                result: result.clone(),
            })
            .await;
            results[i] = Some(result);
        }

        // Record every call's result in order; a skipped call gets a synthetic
        // result so the conversation stays valid for the next request.
        let mut task_touched = false;
        for (i, slot) in results.into_iter().enumerate() {
            let content = match slot {
                Some(result) => {
                    if calls[i].name == "task" {
                        task_touched = true;
                    }
                    result.content
                }
                None => "Interrupted by the user; this tool call was not executed.".to_string(),
            };
            session.history.push(Message {
                role: Role::Tool,
                content,
                tool_calls: Vec::new(),
                tool_call_id: Some(calls[i].id.clone()),
            });
        }
        if task_touched {
            self.emit(AgentEvent::TaskUpdated(session.task_snapshot()))
                .await;
        }
        if interrupted {
            self.emit(AgentEvent::Interrupted).await;
            return Some(TurnOutcome::Interrupted);
        }
        None
    }

    async fn emit(&self, event: AgentEvent) {
        let _ = self.events.send(event).await;
    }
}

/// The phase's tasks (derived `w*`/`v*` ids with current statuses), read fresh
/// from the plan store so resumed progress is reflected.
fn phase_tasks(
    workspace: &suis_core::Workspace,
    target: &ImplementTarget,
    phase: Phase,
) -> Result<Vec<Task>, String> {
    let store = PlanStore::load(workspace).map_err(|e| e.to_string())?;
    let step = store
        .get(&target.plan_id)
        .and_then(|plan| plan.steps.get(target.step_index))
        .ok_or_else(|| format!("no plan step for '{}'", target.plan_id))?;
    let prefix = phase.prefix();
    Ok(plan_step_tasks(step)
        .into_iter()
        .filter(|t| t.id.starts_with(prefix))
        .collect())
}

/// Put a task into `doing` in the plan store (persisted), so the per-task driver
/// — not the model — owns the `todo`→`doing` transition. Best-effort: a load or
/// save failure is left for the upcoming turn to surface, and a task that is
/// already `doing` (a resumed one) or settled is left untouched.
fn mark_task_doing(workspace: &suis_core::Workspace, target: &ImplementTarget, id: &str) {
    let Ok(mut store) = PlanStore::load(workspace) else {
        return;
    };
    let Some(step) = store
        .get_mut(&target.plan_id)
        .and_then(|plan| plan.steps.get_mut(target.step_index))
    else {
        return;
    };
    if let Some(task) = step.task_by_id_mut(id) {
        if task.status == TaskStatus::Todo {
            task.status = TaskStatus::Doing;
            let _ = store.save(workspace);
        }
    }
}

/// Whether a tool is a read-only *orientation* call — no side effects, and its
/// result is fully determined by its arguments. Used by the repetition guard
/// and the budget's duplicate-collapse; `read_lines` belongs here even though it
/// is dispatched serially (see [`runs_concurrently`]).
fn is_read_only(name: &str) -> bool {
    matches!(name, "read_lines" | "search" | "tree")
}

/// Whether a tool may run concurrently within a batch (Phase 3). A subset of the
/// orientation tools ([`is_read_only`]): `read_lines` is excluded so it runs in
/// the serial phase *after* the concurrent `search`/`tree` bodies have recorded
/// into the access log — letting a single turn `search` a file and then
/// `read_lines` it (the read gate requires a prior search). Everything else
/// (edit/bash/git/task/plan/delegate) also runs serially.
fn runs_concurrently(name: &str) -> bool {
    matches!(name, "search" | "tree")
}

/// Whether `messages` contain at least one `edit` tool call — the Phase-2
/// trigger for auto-verification. A turn that only read, searched, or ran
/// commands made no source edits, so there is nothing new to verify.
fn turn_edited(messages: &[Message]) -> bool {
    messages
        .iter()
        .flat_map(|m| m.tool_calls.iter())
        .any(|c| c.name == "edit")
}

/// A short, one-line gist of a verify result for the status line. On success a
/// fixed phrase; on failure the first non-empty output line (trimmed), so the
/// UI hints at the failure without carrying the whole log.
fn verify_summary(passed: bool, content: &str) -> String {
    if passed {
        return "verification passed".to_string();
    }
    let first = content
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("verification failed");
    if first.chars().count() > 80 {
        format!("{}…", first.chars().take(80).collect::<String>())
    } else {
        first.to_string()
    }
}

/// The distinct files edited and shell commands run across `work`'s tool calls,
/// in first-seen order — the deterministic half of a ledger entry.
fn touched_paths(work: &[Message]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |item: String| {
        if !out.contains(&item) {
            out.push(item);
        }
    };
    for msg in work {
        for call in &msg.tool_calls {
            match call.name.as_str() {
                "edit" => {
                    if let Some(path) = call.arguments.get("path").and_then(|v| v.as_str()) {
                        push(path.to_string());
                    }
                }
                "bash" => {
                    if let Some(cmd) = call.arguments.get("command").and_then(|v| v.as_str()) {
                        let line = cmd.lines().next().unwrap_or(cmd).trim();
                        let line = if line.chars().count() > 60 {
                            format!("{}…", line.chars().take(60).collect::<String>())
                        } else {
                            line.to_string()
                        };
                        push(format!("$ {line}"));
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// The opening and closing markers some models wrap their reasoning in when they
/// fold it into the normal text stream (Qwen3, DeepSeek-R1, and similar).
const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

/// Peels inline `<think>…</think>` reasoning out of a streamed text, tolerating
/// tags that straddle chunk boundaries.
///
/// Models that don't expose reasoning on a dedicated channel instead emit it in
/// the text stream wrapped in `<think>` tags. [`push`](Self::push) routes each
/// chunk's text to either the answer or the reasoning side, holding back a
/// trailing fragment that might be the start of a tag split across two chunks;
/// [`flush`](Self::flush) releases whatever remains at stream end.
#[derive(Default)]
struct ThinkSplitter {
    /// Whether the stream is currently inside a `<think>` block.
    in_think: bool,
    /// A trailing fragment held back because it could be the start of a tag the
    /// next chunk completes.
    pending: String,
}

impl ThinkSplitter {
    /// Fold one streamed chunk in, returning `(answer_text, reasoning_text)`
    /// resolved so far. Either part may be empty.
    fn push(&mut self, chunk: &str) -> (String, String) {
        let mut buf = std::mem::take(&mut self.pending);
        buf.push_str(chunk);
        let mut answer = String::new();
        let mut reasoning = String::new();
        loop {
            let tag = if self.in_think {
                THINK_CLOSE
            } else {
                THINK_OPEN
            };
            if let Some(pos) = buf.find(tag) {
                let sink = if self.in_think {
                    &mut reasoning
                } else {
                    &mut answer
                };
                sink.push_str(&buf[..pos]);
                self.in_think = !self.in_think;
                buf.drain(..pos + tag.len());
            } else {
                // No complete tag: emit everything but a suffix that could be the
                // start of the tag we're waiting for, held back for the next chunk.
                let hold = partial_prefix_len(&buf, tag);
                let split = buf.len() - hold;
                let sink = if self.in_think {
                    &mut reasoning
                } else {
                    &mut answer
                };
                sink.push_str(&buf[..split]);
                self.pending = buf[split..].to_string();
                break;
            }
        }
        (answer, reasoning)
    }

    /// Release any held-back fragment at stream end. An unterminated `<think>`
    /// leaves its text as reasoning; otherwise it is answer text.
    fn flush(&mut self) -> (String, String) {
        let buf = std::mem::take(&mut self.pending);
        if buf.is_empty() || !self.in_think {
            (buf, String::new())
        } else {
            (String::new(), buf)
        }
    }
}

/// The length of the longest suffix of `buf` that is a proper prefix of `tag`
/// (never the whole tag — a complete tag is matched by `find` first). Tags are
/// ASCII, so the byte-suffix comparison always falls on a char boundary.
fn partial_prefix_len(buf: &str, tag: &str) -> usize {
    let bytes = buf.as_bytes();
    let upper = (tag.len() - 1).min(bytes.len());
    (1..=upper)
        .rev()
        .find(|&n| bytes[bytes.len() - n..] == tag.as_bytes()[..n])
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;
    use serde_json::json;

    use suis_core::{CommandPermission, PermissionScope, ProjectConfig, Workspace};
    use suis_providers::{Capabilities, ChatRequest, ChatResponse, ChatStream, Model, ToolCall};

    use crate::runtime::events::PermissionDecision;
    use crate::test_util::TempDir;

    #[test]
    fn think_splitter_separates_inline_reasoning() {
        let mut s = ThinkSplitter::default();
        let (answer, reasoning) = s.push("<think>weigh it</think>the answer");
        assert_eq!(answer, "the answer");
        assert_eq!(reasoning, "weigh it");
    }

    #[test]
    fn think_splitter_handles_tags_split_across_chunks() {
        let mut s = ThinkSplitter::default();
        // The opening tag arrives in two pieces, then reasoning, then a closing
        // tag also split, then the answer.
        let mut answer = String::new();
        let mut reasoning = String::new();
        for chunk in ["<th", "ink>plan", " more</thi", "nk>done"] {
            let (a, r) = s.push(chunk);
            answer.push_str(&a);
            reasoning.push_str(&r);
        }
        let (a, r) = s.flush();
        answer.push_str(&a);
        reasoning.push_str(&r);
        assert_eq!(answer, "done");
        assert_eq!(reasoning, "plan more");
    }

    #[test]
    fn think_splitter_passes_plain_text_through() {
        let mut s = ThinkSplitter::default();
        let (answer, reasoning) = s.push("just an answer");
        assert_eq!(answer, "just an answer");
        assert!(reasoning.is_empty());
    }

    #[test]
    fn think_splitter_flushes_unterminated_reasoning() {
        let mut s = ThinkSplitter::default();
        let (answer, reasoning) = s.push("<think>still going");
        assert!(answer.is_empty());
        assert_eq!(reasoning, "still going");
        // An unterminated block leaves the held tail as reasoning at flush.
        let (a, r) = s.flush();
        assert!(a.is_empty());
        assert!(r.is_empty(), "everything was already emitted as reasoning");
    }

    /// A transport that replays a scripted sequence of per-turn chunk lists.
    struct MockTransport {
        turns: StdMutex<VecDeque<Vec<ChatResponse>>>,
        /// When set, every `chat_stream` fails *permanently* (model-not-found),
        /// so the agent must not retry.
        fail: bool,
        /// The first N `chat_stream` calls fail with a *transient* error (and
        /// don't consume a scripted turn); call N+1 onward serve normally. Lets
        /// a test exercise the retry path.
        fail_first: std::sync::atomic::AtomicUsize,
        /// After a turn's chunks, the stream stalls forever instead of ending
        /// (for interrupt tests: a hung provider mid-turn).
        hang: bool,
        /// Zero-based `chat_stream` call index from which streams hang (after
        /// their chunks). `usize::MAX` ⇒ never (unless `hang` is set). Lets a
        /// test hang only a specific nested turn.
        hang_from: usize,
        /// How many `chat_stream` calls have been served.
        calls: std::sync::atomic::AtomicUsize,
    }

    impl MockTransport {
        fn new(turns: Vec<Vec<ChatResponse>>) -> Self {
            MockTransport {
                turns: StdMutex::new(turns.into_iter().collect()),
                fail: false,
                fail_first: std::sync::atomic::AtomicUsize::new(0),
                hang: false,
                hang_from: usize::MAX,
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn failing() -> Self {
            MockTransport {
                fail: true,
                ..MockTransport::new(Vec::new())
            }
        }
        /// Serve `turns`, but fail the first `n` `chat_stream` calls with a
        /// transient error first, so the retry path runs before success.
        fn transient_then(n: usize, turns: Vec<Vec<ChatResponse>>) -> Self {
            MockTransport {
                fail_first: std::sync::atomic::AtomicUsize::new(n),
                ..MockTransport::new(turns)
            }
        }
        fn hanging(turns: Vec<Vec<ChatResponse>>) -> Self {
            MockTransport {
                hang: true,
                ..MockTransport::new(turns)
            }
        }
        /// Serve the scripted turns, but hang every stream from the `from`-th
        /// `chat_stream` call onward (zero-based).
        fn hanging_from(turns: Vec<Vec<ChatResponse>>, from: usize) -> Self {
            MockTransport {
                hang_from: from,
                ..MockTransport::new(turns)
            }
        }
    }

    #[async_trait]
    impl Transport for MockTransport {
        async fn chat(&self, _req: ChatRequest) -> suis_core::Result<ChatResponse> {
            Ok(ChatResponse::default())
        }

        async fn chat_stream(&self, _req: ChatRequest) -> suis_core::Result<ChatStream> {
            if self.fail {
                return Err(suis_core::ProviderError::ModelNotFound {
                    provider: "mock".into(),
                    model: "missing".into(),
                }
                .into());
            }
            // Burn a leading transient failure without consuming a scripted turn.
            if self
                .fail_first
                .fetch_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |n| n.checked_sub(1),
                )
                .is_ok()
            {
                return Err(suis_core::ProviderError::RequestError("transient boom".into()).into());
            }
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let chunks = self.turns.lock().unwrap().pop_front().unwrap_or_default();
            let stream = futures::stream::iter(chunks.into_iter().map(Ok));
            if self.hang || n >= self.hang_from {
                return Ok(Box::pin(stream.chain(futures::stream::pending())));
            }
            Ok(Box::pin(stream))
        }
    }

    fn text_chunk(s: &str, done: bool) -> ChatResponse {
        ChatResponse {
            content: s.into(),
            reasoning: String::new(),
            tool_calls: vec![],
            done,
            usage: None,
        }
    }

    fn tool_chunk(name: &str, args: serde_json::Value) -> ChatResponse {
        ChatResponse {
            content: String::new(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: name.into(),
                arguments: args,
            }],
            done: true,
            usage: None,
        }
    }

    fn session(fx_dir: &TempDir) -> Session {
        let workspace = Workspace::detect(fx_dir.path()).unwrap();
        let project = ProjectConfig {
            allowed_tools: vec!["task".into(), "bash".into()],
            ..ProjectConfig::default()
        };
        let model = Model::new(
            "mock",
            "mock-model",
            Capabilities {
                chat: true,
                streaming: true,
                tool_use: true,
                structured_output: false,
            },
        );
        let mut session = Session::new(workspace, project, model);
        // Session::new merges the user's stored permissions (including the
        // global file); start from a clean store so tests stay hermetic.
        session.permissions = suis_core::PermissionStore::default();
        session
    }

    #[tokio::test]
    async fn plain_response_streams_then_done() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(64);
        let transport = MockTransport::new(vec![vec![
            text_chunk("Hello ", false),
            text_chunk("world", true),
        ]]);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = session(&dir);
        agent.run_turn(&mut sess, "hi").await;
        drop(agent);

        let mut chunks = String::new();
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::StreamChunk(s) => chunks.push_str(&s),
                AgentEvent::Done => done = true,
                _ => {}
            }
        }
        assert_eq!(chunks, "Hello world");
        assert!(done);
        // The assistant message was recorded.
        assert_eq!(sess.history.last().unwrap().role, Role::Assistant);
    }

    #[tokio::test]
    async fn reasoning_is_emitted_and_kept_out_of_history() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(64);
        // A chunk on the native reasoning channel, then an inline `<think>`
        // block folded into the text stream, then the answer.
        let native = ChatResponse {
            content: String::new(),
            reasoning: "native plan. ".into(),
            tool_calls: vec![],
            done: false,
            usage: None,
        };
        let transport = MockTransport::new(vec![vec![
            native,
            text_chunk("<think>inline plan</think>the answer", true),
        ]]);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = session(&dir);
        agent.run_turn(&mut sess, "hi").await;
        drop(agent);

        let mut answer = String::new();
        let mut reasoning = String::new();
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::StreamChunk(s) => answer.push_str(&s),
                AgentEvent::ReasoningChunk(s) => reasoning.push_str(&s),
                _ => {}
            }
        }
        assert_eq!(answer, "the answer");
        assert_eq!(reasoning, "native plan. inline plan");
        // Only the answer is recorded; reasoning never enters the history sent
        // back to the model.
        let recorded = &sess.history.last().unwrap().content;
        assert_eq!(recorded, "the answer");
        assert!(!recorded.contains("plan"));
    }

    #[tokio::test]
    async fn tool_call_executes_then_loop_finishes() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(64);
        // Turn 1: create a task. Turn 2: plain text → Done.
        let transport = MockTransport::new(vec![
            vec![tool_chunk(
                "task",
                json!({ "action": "create", "title": "do it" }),
            )],
            vec![text_chunk("all set", true)],
        ]);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = session(&dir);
        agent.run_turn(&mut sess, "make a task").await;
        let tasks_after = sess.task_snapshot();
        drop(agent);

        let mut started = 0;
        let mut completed = 0;
        let mut task_updates = 0;
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::ToolCallStarted { .. } => started += 1,
                AgentEvent::ToolCallCompleted { result } => {
                    completed += 1;
                    assert!(!result.is_error);
                }
                AgentEvent::TaskUpdated(_) => task_updates += 1,
                AgentEvent::Done => done = true,
                _ => {}
            }
        }
        assert_eq!(started, 1);
        assert_eq!(completed, 1);
        assert_eq!(task_updates, 1);
        assert!(done);
        assert_eq!(tasks_after.len(), 1);
    }

    #[tokio::test]
    async fn repetition_guard_notes_repeated_reads() {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("f.rs"), "x").unwrap();
        let (tx, mut rx) = mpsc::channel(256);
        // Three turns reading the *same* file, then a settling text turn. The
        // third identical read crosses REDUNDANT_READ_LIMIT and trips the guard.
        let read = || vec![tool_chunk("read_lines", json!({ "path": "f.rs" }))];
        let transport =
            MockTransport::new(vec![read(), read(), read(), vec![text_chunk("done", true)]]);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = session(&dir);
        sess.project.allowed_tools = vec!["read_lines".into()];
        // Pre-seed the funnel so the reads succeed and actually repeat.
        sess.access.lock().unwrap().record_searched("f.rs".into());
        agent.run_turn(&mut sess, "analyze f.rs").await;
        drop(agent);
        while rx.recv().await.is_some() {}

        let notes = sess
            .history
            .as_slice()
            .iter()
            .filter(|m| m.content == REPETITION_NOTE)
            .count();
        assert_eq!(notes, 1, "the repetition note fires exactly once");
    }

    #[tokio::test]
    async fn repetition_guard_ignores_distinct_reads() {
        let dir = TempDir::new();
        for i in 0..3 {
            std::fs::write(dir.path().join(format!("f{i}.rs")), "x").unwrap();
        }
        let (tx, mut rx) = mpsc::channel(256);
        // Reading three *different* files is exploration, not a loop — the guard
        // must stay silent.
        let transport = MockTransport::new(vec![
            vec![tool_chunk("read_lines", json!({ "path": "f0.rs" }))],
            vec![tool_chunk("read_lines", json!({ "path": "f1.rs" }))],
            vec![tool_chunk("read_lines", json!({ "path": "f2.rs" }))],
            vec![text_chunk("done", true)],
        ]);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = session(&dir);
        sess.project.allowed_tools = vec!["read_lines".into()];
        // Pre-seed the funnel so the distinct reads succeed.
        {
            let mut log = sess.access.lock().unwrap();
            for i in 0..3 {
                log.record_searched(format!("f{i}.rs"));
            }
        }
        agent.run_turn(&mut sess, "analyze").await;
        drop(agent);
        while rx.recv().await.is_some() {}

        assert!(
            sess.history
                .as_slice()
                .iter()
                .all(|m| m.content != REPETITION_NOTE),
            "distinct reads must never trip the guard"
        );
    }

    #[tokio::test]
    async fn text_emitted_tool_call_is_recovered_and_executed() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(64);
        // Turn 1: the model prints a Hermes-style tool call as text with an empty
        // structured tool_calls channel. Turn 2: plain text → Done.
        let textual = text_chunk(
            "<tool_call>{\"name\": \"task\", \"arguments\": {\"action\": \"create\", \"title\": \"do it\"}}</tool_call>",
            true,
        );
        let transport = MockTransport::new(vec![vec![textual], vec![text_chunk("all set", true)]]);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = session(&dir);
        agent.run_turn(&mut sess, "make a task").await;
        let tasks_after = sess.task_snapshot();
        drop(agent);

        let mut completed = 0;
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::ToolCallCompleted { result } => {
                    assert!(!result.is_error, "{}", result.content);
                    completed += 1;
                }
                AgentEvent::Done => done = true,
                _ => {}
            }
        }
        assert_eq!(completed, 1, "the text-emitted call should execute");
        assert!(done);
        assert_eq!(tasks_after.len(), 1);
    }

    #[tokio::test]
    async fn denied_permission_yields_error_result_and_continues() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(64);
        let transport = MockTransport::new(vec![
            vec![tool_chunk("bash", json!({ "command": "echo hi" }))],
            vec![text_chunk("done", true)],
        ]);
        let agent = Agent::new(Box::new(transport), tx);

        // Drive the turn while answering the permission prompt with a denial.
        let handle = tokio::spawn(async move {
            let mut sess = session(&dir);
            agent.run_turn(&mut sess, "run it").await;
            drop(agent);
        });

        let mut denied_error = false;
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::PermissionRequest { sender, .. } => {
                    let _ = sender.send(PermissionDecision::deny());
                }
                AgentEvent::ToolCallCompleted { result } => {
                    if result.is_error {
                        denied_error = true;
                    }
                }
                AgentEvent::Done => done = true,
                _ => {}
            }
        }
        handle.await.unwrap();
        assert!(denied_error);
        assert!(done);
    }

    #[tokio::test]
    async fn plan_mode_refuses_bash_without_prompting() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(64);
        let transport = MockTransport::new(vec![
            vec![tool_chunk("bash", json!({ "command": "echo hi" }))],
            vec![text_chunk("understood", true)],
        ]);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = session(&dir);
        sess.mode = crate::runtime::Mode::Plan;
        agent.run_turn(&mut sess, "plan something").await;
        drop(agent);

        let mut mode_error = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::PermissionRequest { .. } => {
                    panic!("mode refusal must not prompt");
                }
                AgentEvent::ToolCallCompleted { result } => {
                    assert!(result.is_error);
                    assert!(result.content.contains("not available in plan mode"));
                    mode_error = true;
                }
                _ => {}
            }
        }
        assert!(mode_error);
    }

    #[tokio::test]
    async fn compact_replaces_history_with_summary() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(64);
        let transport = MockTransport::new(vec![vec![
            text_chunk("Summary: ", false),
            text_chunk("the user built a parser.", true),
        ]]);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = session(&dir);
        // A few messages to compact away.
        sess.history
            .push(Message::text(Role::User, "write a parser"));
        sess.history
            .push(Message::text(Role::Assistant, "done, here it is"));
        sess.history
            .push(Message::text(Role::User, "now add tests"));

        agent.compact(&mut sess).await;
        drop(agent);

        // History is replaced by a single labelled summary message.
        assert_eq!(sess.history.len(), 1);
        let summary = &sess.history.as_slice()[0];
        assert_eq!(summary.role, Role::System);
        assert!(summary
            .content
            .contains("Summary of the prior conversation"));
        assert!(summary.content.contains("the user built a parser."));

        // A Compacted event carried the summary text.
        let mut compacted = None;
        while let Some(ev) = rx.recv().await {
            if let AgentEvent::Compacted { summary } = ev {
                compacted = Some(summary);
            }
        }
        assert!(compacted.unwrap().contains("the user built a parser."));
    }

    #[tokio::test]
    async fn compact_failure_leaves_history_intact() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(64);
        let agent = Agent::new(Box::new(MockTransport::failing()), tx);
        let mut sess = session(&dir);
        sess.history.push(Message::text(Role::User, "keep me"));
        sess.history.push(Message::text(Role::Assistant, "and me"));
        let before = sess.history.as_slice().to_vec();

        agent.compact(&mut sess).await;
        drop(agent);

        // Nothing destroyed; an error was reported.
        assert_eq!(sess.history.as_slice(), before.as_slice());
        let mut saw_error = false;
        let mut saw_compacted = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::Error(_) => saw_error = true,
                AgentEvent::Compacted { .. } => saw_compacted = true,
                _ => {}
            }
        }
        assert!(saw_error);
        assert!(
            !saw_compacted,
            "a failed compaction must not report success"
        );
    }

    #[tokio::test]
    async fn compact_reinjects_work_package_in_implementation_session() {
        use suis_core::{PlanStep, PlanStore, PlanTask};

        let dir = TempDir::new();
        let (tx, _rx) = mpsc::channel(64);
        let transport = MockTransport::new(vec![vec![text_chunk("a dense summary", true)]]);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = session(&dir);

        // Store a plan and point the session at its first step.
        let mut store = PlanStore::default();
        let id = store.insert(
            "Auth System",
            "Add JWT auth",
            vec![PlanStep {
                title: "Tokens".into(),
                work_tasks: vec![PlanTask::new("sign tokens")],
                verify_tasks: vec![PlanTask::new("auth tests")],
            }],
        );
        store.save(&sess.workspace).unwrap();
        sess.implement = Some(crate::runtime::ImplementTarget {
            plan_id: id,
            step_index: 0,
        });
        sess.history
            .push(Message::text(Role::User, "earlier work package"));
        sess.history
            .push(Message::text(Role::Assistant, "working on it"));

        agent.compact(&mut sess).await;
        drop(agent);

        // Summary first, then the regenerated work package.
        assert_eq!(sess.history.len(), 2);
        assert!(sess.history.as_slice()[0]
            .content
            .contains("a dense summary"));
        let package = &sess.history.as_slice()[1];
        assert_eq!(package.role, Role::User);
        assert!(package.content.contains("Tokens"));
        assert!(package.content.contains("sign tokens"));
    }

    #[tokio::test]
    async fn interrupt_mid_stream_keeps_partial_text_and_abandons_the_turn() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(64);
        let (int_tx, int_rx) = watch::channel(());
        // One chunk arrives, then the stream stalls forever — the interrupt
        // must take effect mid-stall, not wait for the next chunk.
        let transport = MockTransport::hanging(vec![vec![text_chunk("partial ", false)]]);
        let agent = Agent::new(Box::new(transport), tx).with_interrupt(int_rx);
        let mut sess = session(&dir);

        let handle = tokio::spawn(async move {
            agent.run_turn(&mut sess, "hi").await;
            sess
        });

        let mut interrupted = false;
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::StreamChunk(_) => {
                    let _ = int_tx.send(());
                }
                AgentEvent::Interrupted => interrupted = true,
                AgentEvent::Done => done = true,
                _ => {}
            }
        }
        let sess = handle.await.unwrap();
        assert!(interrupted);
        assert!(!done, "an interrupted turn must not also report Done");
        // The partial text is recorded, with no dangling tool calls.
        let last = sess.history.last().unwrap();
        assert_eq!(last.role, Role::Assistant);
        assert_eq!(last.content, "partial ");
        assert!(last.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn interrupt_lets_the_running_tool_finish_and_skips_the_rest() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(64);
        let (int_tx, int_rx) = watch::channel(());
        // One response carrying two bash calls; the interrupt lands while the
        // first awaits its permission decision, so the first still runs and
        // the second is skipped.
        let transport = MockTransport::new(vec![vec![ChatResponse {
            content: String::new(),
            reasoning: String::new(),
            tool_calls: vec![
                ToolCall {
                    id: "tc1".into(),
                    name: "bash".into(),
                    arguments: json!({ "command": "echo one" }),
                },
                ToolCall {
                    id: "tc2".into(),
                    name: "bash".into(),
                    arguments: json!({ "command": "echo two" }),
                },
            ],
            done: true,
            usage: None,
        }]]);
        let agent = Agent::new(Box::new(transport), tx).with_interrupt(int_rx);
        let mut sess = session(&dir);

        let handle = tokio::spawn(async move {
            agent.run_turn(&mut sess, "run both").await;
            sess
        });

        let mut started = 0;
        let mut completed = 0;
        let mut interrupted = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::PermissionRequest { sender, .. } => {
                    // Interrupt first, then let the prompted call proceed.
                    let _ = int_tx.send(());
                    let _ = sender.send(PermissionDecision::once());
                }
                AgentEvent::ToolCallStarted { .. } => started += 1,
                AgentEvent::ToolCallCompleted { result } => {
                    assert!(!result.is_error);
                    completed += 1;
                }
                AgentEvent::Interrupted => interrupted = true,
                _ => {}
            }
        }
        let sess = handle.await.unwrap();
        assert!(interrupted);
        assert_eq!(started, 1, "the second call must never start");
        assert_eq!(completed, 1, "the running call finishes");
        // Both recorded calls have answers: tc1's real result, tc2 synthetic.
        let history = sess.history.as_slice();
        let result_of = |id: &str| {
            history
                .iter()
                .find(|m| m.tool_call_id.as_deref() == Some(id))
                .expect("every recorded call needs a result")
        };
        assert!(result_of("tc1").content.contains("one"));
        assert!(result_of("tc2").content.contains("Interrupted by the user"));
    }

    #[tokio::test]
    async fn stale_interrupt_does_not_cancel_the_next_turn() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(64);
        let (int_tx, int_rx) = watch::channel(());
        let transport = MockTransport::new(vec![vec![text_chunk("hello", true)]]);
        let agent = Agent::new(Box::new(transport), tx).with_interrupt(int_rx);
        let mut sess = session(&dir);

        // An interrupt that raced the end of a previous turn.
        let _ = int_tx.send(());
        agent.run_turn(&mut sess, "hi").await;
        drop(agent);

        let mut done = false;
        let mut interrupted = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::Done => done = true,
                AgentEvent::Interrupted => interrupted = true,
                _ => {}
            }
        }
        assert!(done, "the fresh turn runs to completion");
        assert!(!interrupted);
    }

    #[tokio::test]
    async fn interrupted_compaction_leaves_history_untouched() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(64);
        let (int_tx, int_rx) = watch::channel(());
        let transport = MockTransport::hanging(vec![vec![text_chunk("Summary…", false)]]);
        let agent = Agent::new(Box::new(transport), tx).with_interrupt(int_rx);
        let mut sess = session(&dir);
        sess.history.push(Message::text(Role::User, "keep me"));
        sess.history.push(Message::text(Role::Assistant, "and me"));
        let before = sess.history.as_slice().to_vec();

        let handle = tokio::spawn(async move {
            agent.compact(&mut sess).await;
            sess
        });

        let mut interrupted = false;
        let mut compacted = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::StreamChunk(_) => {
                    let _ = int_tx.send(());
                }
                AgentEvent::Interrupted => interrupted = true,
                AgentEvent::Compacted { .. } => compacted = true,
                _ => {}
            }
        }
        let sess = handle.await.unwrap();
        assert!(interrupted);
        assert!(
            !compacted,
            "an interrupted compaction must not report success"
        );
        assert_eq!(sess.history.as_slice(), before.as_slice());
    }

    #[tokio::test]
    async fn transport_error_emits_error_event() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(64);
        let agent = Agent::new(Box::new(MockTransport::failing()), tx);
        let mut sess = session(&dir);
        agent.run_turn(&mut sess, "hi").await;
        drop(agent);

        let mut saw_error = false;
        let mut saw_retry = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::Error(_) => saw_error = true,
                AgentEvent::Retrying { .. } => saw_retry = true,
                _ => {}
            }
        }
        assert!(saw_error);
        assert!(!saw_retry, "a permanent error must not be retried");
    }

    #[tokio::test]
    async fn transient_failures_are_retried_then_succeed() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(64);
        // The first two stream opens fail transiently; the third serves a real
        // answer, so the turn recovers without the user resubmitting.
        let transport = MockTransport::transient_then(2, vec![vec![text_chunk("hello", true)]]);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = session(&dir);
        agent.run_turn(&mut sess, "hi").await;
        drop(agent);

        let mut retries = 0usize;
        let mut errored = false;
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::Retrying { .. } => retries += 1,
                AgentEvent::Error(_) => errored = true,
                AgentEvent::Done => done = true,
                _ => {}
            }
        }
        assert_eq!(retries, 2, "each transient failure emits one Retrying");
        assert!(!errored, "the turn recovered, so no error");
        assert!(done);
        assert_eq!(sess.history.last().unwrap().content, "hello");
    }

    #[tokio::test]
    async fn transient_failures_beyond_the_cap_give_up() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(64);
        // More transient failures than the retry ceiling: the turn errors after
        // exhausting its retries rather than looping forever.
        let transport = MockTransport::transient_then(
            MAX_STREAM_RETRIES + 5,
            vec![vec![text_chunk("never reached", true)]],
        );
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = session(&dir);
        agent.run_turn(&mut sess, "hi").await;
        drop(agent);

        let mut retries = 0usize;
        let mut errored = false;
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::Retrying { .. } => retries += 1,
                AgentEvent::Error(_) => errored = true,
                AgentEvent::Done => done = true,
                _ => {}
            }
        }
        assert_eq!(retries, MAX_STREAM_RETRIES, "retries are capped");
        assert!(errored, "the turn gives up after the cap");
        assert!(!done);
    }

    #[tokio::test]
    async fn interrupt_during_backoff_abandons_the_turn() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(64);
        let (int_tx, int_rx) = watch::channel(());
        // Always-transient, so the agent is mid-backoff when the user hits Esc.
        let transport =
            MockTransport::transient_then(MAX_STREAM_RETRIES, vec![vec![text_chunk("late", true)]]);
        let agent = Agent::new(Box::new(transport), tx).with_interrupt(int_rx);
        let mut sess = session(&dir);

        let handle = tokio::spawn(async move {
            agent.run_turn(&mut sess, "hi").await;
            sess
        });

        let mut interrupted = false;
        let mut errored = false;
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                // Esc lands while the first backoff is still sleeping.
                AgentEvent::Retrying { .. } => {
                    let _ = int_tx.send(());
                }
                AgentEvent::Interrupted => interrupted = true,
                AgentEvent::Error(_) => errored = true,
                AgentEvent::Done => done = true,
                _ => {}
            }
        }
        let _sess = handle.await.unwrap();
        assert!(interrupted, "a backoff wait is interruptible");
        assert!(!errored, "an interrupt is not an error");
        assert!(!done);
    }

    #[test]
    fn touched_paths_collects_edits_and_commands_in_order_without_dupes() {
        use suis_providers::ToolCall;
        let call = |name: &str, args: serde_json::Value| Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "tc".into(),
                name: name.into(),
                arguments: args,
            }],
            tool_call_id: None,
        };
        let work = vec![
            call("edit", json!({ "path": "src/a.rs" })),
            call(
                "bash",
                json!({ "command": "cargo test\nignored second line" }),
            ),
            call("edit", json!({ "path": "src/a.rs" })), // duplicate path
            call("read", json!({ "path": "src/b.rs" })), // reads aren't "touched"
        ];
        let touched = touched_paths(&work);
        assert_eq!(
            touched,
            vec!["src/a.rs".to_string(), "$ cargo test".to_string()]
        );
    }

    /// A session pointed at a stored plan's first step, for driver tests.
    /// `work`/`verify` are the task titles to create.
    fn implement_session(dir: &TempDir, work: &[&str], verify: &[&str]) -> Session {
        use suis_core::{PlanStep, PlanStore, PlanTask};
        let mut sess = session(dir);
        let mut store = PlanStore::default();
        let id = store.insert(
            "Auth System",
            "Add JWT auth",
            vec![PlanStep {
                title: "Tokens".into(),
                work_tasks: work.iter().map(|t| PlanTask::new(*t)).collect(),
                verify_tasks: verify.iter().map(|t| PlanTask::new(*t)).collect(),
            }],
        );
        store.save(&sess.workspace).unwrap();
        sess.implement = Some(crate::runtime::ImplementTarget {
            plan_id: id,
            step_index: 0,
        });
        sess
    }

    /// Scripted turns for one task the model completes: a `task update done`
    /// call, a closing plain-text turn, then the (silent) summary completion.
    fn complete_task_turns(id: &str, summary: &str) -> Vec<Vec<ChatResponse>> {
        vec![
            vec![tool_chunk(
                "task",
                json!({ "action": "update", "id": id, "status": "done" }),
            )],
            vec![text_chunk("done", true)],
            vec![text_chunk(summary, true)],
        ]
    }

    /// Scripted turns for one task the model marks `blocked` (it cannot do it):
    /// a `task update blocked` call, a closing text turn, then the summary.
    fn block_task_turns(id: &str, reason: &str) -> Vec<Vec<ChatResponse>> {
        vec![
            vec![tool_chunk(
                "task",
                json!({ "action": "update", "id": id, "status": "blocked" }),
            )],
            vec![text_chunk(reason, true)],
            vec![text_chunk(reason, true)],
        ]
    }

    #[tokio::test]
    async fn driver_advances_past_a_blocked_task_instead_of_halting() {
        use suis_core::{PlanStore, TaskStatus};
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(256);
        // w1 is blocked (file missing); the driver must still reach w2 and
        // finish it, rather than treating the block as a no-progress stall.
        let mut turns = block_task_turns("w1", "file does not exist");
        turns.extend(complete_task_turns("w2", "wrote w2"));
        let agent = Agent::new(Box::new(MockTransport::new(turns)), tx);
        let mut sess = implement_session(&dir, &["clean nonexistent file", "refresh"], &["tests"]);

        agent.run_implement_phase(&mut sess, Phase::Work).await;
        drop(agent);

        // w1 stayed blocked, w2 completed — the block did not abort the phase.
        let store = PlanStore::load(&sess.workspace).unwrap();
        let step = &store.get("auth-system").unwrap().steps[0];
        assert_eq!(step.work_tasks[0].status, TaskStatus::Blocked);
        assert_eq!(step.work_tasks[1].status, TaskStatus::Done);

        // Both tasks compacted; the phase ends with exactly one terminal Done.
        let mut compacted = Vec::new();
        let mut done = 0;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::TaskCompacted { id, .. } => compacted.push(id),
                AgentEvent::Done => done += 1,
                _ => {}
            }
        }
        assert_eq!(compacted, vec!["w1".to_string(), "w2".to_string()]);
        assert_eq!(done, 1);

        // The ledger records w1 as blocked, not as completed work.
        assert_eq!(sess.ledger.len(), 2);
        assert_eq!(sess.ledger[0].id, "w1");
        assert!(sess.ledger[0].blocked, "w1 recorded as blocked");
        assert!(!sess.ledger[1].blocked, "w2 recorded as done");
    }

    #[tokio::test]
    async fn driver_advances_through_work_tasks_compacting_each() {
        use suis_core::{PlanStore, TaskStatus};
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(256);
        // w1 then w2 each: tool-done, text, summary. v1 is never touched.
        let mut turns = complete_task_turns("w1", "wrote w1");
        turns.extend(complete_task_turns("w2", "wrote w2"));
        let agent = Agent::new(Box::new(MockTransport::new(turns)), tx);
        let mut sess = implement_session(&dir, &["sign tokens", "refresh"], &["auth tests"]);

        agent.run_implement_phase(&mut sess, Phase::Work).await;
        drop(agent);

        // Both work tasks done on disk; the verify task untouched (gate kept).
        let store = PlanStore::load(&sess.workspace).unwrap();
        let step = &store.get("auth-system").unwrap().steps[0];
        assert_eq!(step.work_tasks[0].status, TaskStatus::Done);
        assert_eq!(step.work_tasks[1].status, TaskStatus::Done);
        assert_eq!(step.verify_tasks[0].status, TaskStatus::Todo);

        // One TaskCompacted per work task, then exactly one terminal Done.
        let mut compacted = Vec::new();
        let mut done = 0;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::TaskCompacted { id, .. } => compacted.push(id),
                AgentEvent::Done => done += 1,
                _ => {}
            }
        }
        assert_eq!(compacted, vec!["w1".to_string(), "w2".to_string()]);
        assert_eq!(done, 1, "the phase emits Done once, at the end");

        // The ledger carried both tasks with their summaries.
        assert_eq!(sess.ledger.len(), 2);
        assert_eq!(sess.ledger[0].id, "w1");
        assert_eq!(sess.ledger[0].summary, "wrote w1");
        assert_eq!(sess.ledger[1].id, "w2");
    }

    #[tokio::test]
    async fn context_resets_between_tasks_carrying_only_the_ledger() {
        let dir = TempDir::new();
        let (tx, _rx) = mpsc::channel(256);
        let mut turns = complete_task_turns("w1", "did the first task");
        turns.extend(complete_task_turns("w2", "did the second task"));
        let agent = Agent::new(Box::new(MockTransport::new(turns)), tx);
        let mut sess = implement_session(&dir, &["first", "second"], &[]);

        agent.run_implement_phase(&mut sess, Phase::Work).await;
        drop(agent);

        // The history left over is w2's lean seed: the work package plus the
        // ledger summarizing w1 — not w1's raw working transcript.
        let joined: String = sess
            .history
            .as_slice()
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("did the first task"),
            "w1's ledger note carries forward"
        );
        // The leftover history is w2's turn only: its pointer is present, w1's
        // pointer was reset away (the ledger note, not the raw turn, carries w1).
        assert!(joined.contains("Your current task is w2"));
        assert!(
            !joined.contains("Your current task is w1"),
            "w1's working turn must be reset out before w2's turn"
        );
    }

    #[tokio::test]
    async fn no_progress_turn_halts_the_driver() {
        use suis_core::{PlanStore, TaskStatus};
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(256);
        // The model just talks; it never marks the task done.
        let agent = Agent::new(
            Box::new(MockTransport::new(vec![vec![text_chunk(
                "hmm, a question?",
                true,
            )]])),
            tx,
        );
        let mut sess = implement_session(&dir, &["do it"], &["check"]);

        agent.run_implement_phase(&mut sess, Phase::Work).await;
        drop(agent);

        // The task is still open (left in `doing`, which the driver set before
        // the turn) and nothing was compacted into the ledger. A resumed run
        // picks it up again, since the driver treats `doing` as actionable.
        let store = PlanStore::load(&sess.workspace).unwrap();
        assert_eq!(
            store.get("auth-system").unwrap().steps[0].work_tasks[0].status,
            TaskStatus::Doing
        );
        assert!(sess.ledger.is_empty());

        let mut compacted = 0;
        let mut done = 0;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::TaskCompacted { .. } => compacted += 1,
                AgentEvent::Done => done += 1,
                _ => {}
            }
        }
        assert_eq!(compacted, 0, "no task completed, so none is compacted");
        assert_eq!(done, 1, "the driver hands back control instead of looping");
    }

    #[tokio::test]
    async fn driver_sets_the_current_task_to_doing_before_the_turn() {
        use suis_core::{PlanStore, TaskStatus};
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(256);
        // The model stalls without touching status, so the only todo→doing move
        // is the driver's own — made before the turn ran. The agent no longer
        // sets `doing` itself.
        let agent = Agent::new(
            Box::new(MockTransport::new(vec![vec![text_chunk(
                "thinking…",
                true,
            )]])),
            tx,
        );
        let mut sess = implement_session(&dir, &["do it"], &["check"]);

        agent.run_implement_phase(&mut sess, Phase::Work).await;
        drop(agent);

        // Persisted: w1 moved todo→doing with no model action.
        let store = PlanStore::load(&sess.workspace).unwrap();
        assert_eq!(
            store.get("auth-system").unwrap().steps[0].work_tasks[0].status,
            TaskStatus::Doing
        );
        // The panel was told, too: a TaskUpdated carried w1 as doing.
        let mut saw_doing = false;
        while let Some(ev) = rx.recv().await {
            if let AgentEvent::TaskUpdated(tasks) = ev {
                if tasks
                    .iter()
                    .any(|t| t.id == "w1" && t.status == TaskStatus::Doing)
                {
                    saw_doing = true;
                }
            }
        }
        assert!(saw_doing, "the task panel was not told the task is doing");
    }

    #[tokio::test]
    async fn failed_summary_still_records_a_deterministic_ledger_entry() {
        let dir = TempDir::new();
        let (tx, _rx) = mpsc::channel(256);
        // Only the task-done and closing turns are scripted; the summary call
        // then gets an empty stream (turns exhausted) — a best-effort miss.
        let turns = vec![
            vec![tool_chunk(
                "task",
                json!({ "action": "update", "id": "w1", "status": "done" }),
            )],
            vec![text_chunk("done", true)],
        ];
        let agent = Agent::new(Box::new(MockTransport::new(turns)), tx);
        let mut sess = implement_session(&dir, &["only task"], &[]);

        agent.run_implement_phase(&mut sess, Phase::Work).await;
        drop(agent);

        // The entry exists with its deterministic fields even though the summary
        // came back empty.
        assert_eq!(sess.ledger.len(), 1);
        assert_eq!(sess.ledger[0].id, "w1");
        assert_eq!(sess.ledger[0].title, "only task");
        assert!(sess.ledger[0].summary.is_empty());
    }

    // ---- Phase 2: self-verification ----

    /// A session with a configured verify command and a standing grant for it,
    /// so the synthesized verify `bash` call runs without a permission prompt.
    fn verify_session(dir: &TempDir, command: &str, grant: &str) -> Session {
        let mut sess = session(dir);
        sess.project.verify_command = Some(command.to_string());
        sess.permissions.commands.push(CommandPermission {
            pattern: grant.to_string(),
            scope: PermissionScope::Project,
        });
        sess
    }

    /// A turn that emits a single `edit` writing `content` to `path`.
    fn edit_chunk(path: &str, content: &str) -> ChatResponse {
        ChatResponse {
            content: String::new(),
            reasoning: String::new(),
            tool_calls: vec![ToolCall {
                id: "ed".into(),
                name: "edit".into(),
                arguments: json!({ "path": path, "content": content }),
            }],
            done: true,
            usage: None,
        }
    }

    #[tokio::test]
    async fn verify_runs_after_edit_loops_on_failure_then_settles_on_pass() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(256);
        // verify passes once the file `ok` exists. The model edits an unrelated
        // file first (verify fails), is fed the failure, then creates `ok`.
        let transport = MockTransport::new(vec![
            vec![edit_chunk("a.rs", "fn a() {}")],
            vec![text_chunk("done", true)],
            vec![edit_chunk("ok", "marker")],
            vec![text_chunk("fixed", true)],
        ]);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = verify_session(&dir, "test -f ok", "test *");

        agent.run_turn(&mut sess, "make a change").await;
        drop(agent);

        let mut started = 0;
        let mut results = Vec::new();
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::VerifyStarted { command } => {
                    assert_eq!(command, "test -f ok");
                    started += 1;
                }
                AgentEvent::VerifyResult { passed, .. } => results.push(passed),
                AgentEvent::Done => done = true,
                _ => {}
            }
        }
        assert!(done);
        assert_eq!(
            started, 2,
            "verify ran once per settle: a failure then a pass"
        );
        assert_eq!(results, vec![false, true]);
        // A failure note was fed back so the model could fix it.
        assert!(sess
            .history
            .as_slice()
            .iter()
            .any(|m| m.content.contains("Automatic verification failed")));
    }

    #[tokio::test]
    async fn no_verify_without_a_verify_command() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(256);
        let transport = MockTransport::new(vec![
            vec![edit_chunk("a.rs", "fn a() {}")],
            vec![text_chunk("done", true)],
        ]);
        let agent = Agent::new(Box::new(transport), tx);
        // No verify_command ⇒ today's behavior (the zero-risk default).
        let mut sess = session(&dir);

        agent.run_turn(&mut sess, "edit").await;
        drop(agent);

        let mut verified = false;
        while let Some(ev) = rx.recv().await {
            if matches!(ev, AgentEvent::VerifyStarted { .. }) {
                verified = true;
            }
        }
        assert!(!verified, "no verify command ⇒ no verification");
    }

    #[tokio::test]
    async fn no_verify_when_the_turn_made_no_edits() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(256);
        // The turn only runs a command (no edit), then settles.
        let transport = MockTransport::new(vec![
            vec![tool_chunk("bash", json!({ "command": "echo hi" }))],
            vec![text_chunk("done", true)],
        ]);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = verify_session(&dir, "test -f ok", "test *");
        // Also grant the echo so the turn's own command isn't prompted.
        sess.permissions.commands.push(CommandPermission {
            pattern: "echo *".into(),
            scope: PermissionScope::Project,
        });

        agent.run_turn(&mut sess, "run something").await;
        drop(agent);

        let mut verified = false;
        while let Some(ev) = rx.recv().await {
            if matches!(ev, AgentEvent::VerifyStarted { .. }) {
                verified = true;
            }
        }
        assert!(!verified, "no edits ⇒ nothing to verify");
    }

    #[tokio::test]
    async fn no_verify_in_chat_mode() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(256);
        // Even with an edit in the script, Chat mode never auto-verifies (and
        // the edit itself is refused by the mode gate).
        let transport = MockTransport::new(vec![
            vec![edit_chunk("a.rs", "fn a() {}")],
            vec![text_chunk("done", true)],
        ]);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = verify_session(&dir, "test -f ok", "test *");
        sess.mode = crate::runtime::Mode::Chat;

        agent.run_turn(&mut sess, "discuss").await;
        drop(agent);

        let mut verified = false;
        while let Some(ev) = rx.recv().await {
            if matches!(ev, AgentEvent::VerifyStarted { .. }) {
                verified = true;
            }
        }
        assert!(!verified, "verification is Agent-mode only");
    }

    #[tokio::test]
    async fn verify_round_cap_halts_an_always_failing_build() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(512);
        // The build always fails; the model keeps editing. The round cap must
        // stop the verify→fix cycle and settle honestly.
        let mut turns = Vec::new();
        for _ in 0..MAX_VERIFY_ROUNDS + 2 {
            turns.push(vec![edit_chunk("a.rs", "fn a() {}")]);
            turns.push(vec![text_chunk("done", true)]);
        }
        let transport = MockTransport::new(turns);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = verify_session(&dir, "false", "false");

        agent.run_turn(&mut sess, "edit").await;
        drop(agent);

        let mut started = 0;
        let mut done = false;
        let mut errored = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::VerifyStarted { .. } => started += 1,
                AgentEvent::Done => done = true,
                AgentEvent::Error(_) => errored = true,
                _ => {}
            }
        }
        assert_eq!(started, MAX_VERIFY_ROUNDS, "verify is capped per turn");
        assert!(
            done,
            "the turn settles after the cap rather than looping forever"
        );
        assert!(!errored, "the cap settles cleanly, it does not error out");
    }

    // ---- Phase 3: batched read-only tool calls ----

    /// One model response carrying several tool calls at once.
    fn multi_tool_chunk(calls: &[(&str, &str, serde_json::Value)]) -> ChatResponse {
        ChatResponse {
            content: String::new(),
            reasoning: String::new(),
            tool_calls: calls
                .iter()
                .map(|(id, name, args)| ToolCall {
                    id: (*id).into(),
                    name: (*name).into(),
                    arguments: args.clone(),
                })
                .collect(),
            done: true,
            usage: None,
        }
    }

    #[tokio::test]
    async fn batched_reads_and_an_edit_all_execute_and_are_recorded() {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("a.txt"), "alpha").unwrap();
        std::fs::write(dir.path().join("b.txt"), "beta").unwrap();
        let (tx, mut rx) = mpsc::channel(256);
        // Two reads (concurrent) plus an edit (sequential), then a settle.
        let transport = MockTransport::new(vec![
            vec![multi_tool_chunk(&[
                ("r1", "read_lines", json!({ "path": "a.txt" })),
                ("r2", "read_lines", json!({ "path": "b.txt" })),
                ("w1", "edit", json!({ "path": "c.txt", "content": "gamma" })),
            ])],
            vec![text_chunk("done", true)],
        ]);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = session(&dir);
        // Pre-seed the funnel so the two reads pass (c.txt is a new file, exempt).
        {
            let mut log = sess.access.lock().unwrap();
            log.record_searched("a.txt".into());
            log.record_searched("b.txt".into());
        }

        agent.run_turn(&mut sess, "read a and b, write c").await;
        drop(agent);

        let mut completed = 0;
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::ToolCallCompleted { result } => {
                    assert!(!result.is_error, "{}", result.content);
                    completed += 1;
                }
                AgentEvent::Done => done = true,
                _ => {}
            }
        }
        assert_eq!(completed, 3, "all three calls execute");
        assert!(done);
        // The edit happened, and every call id has a recorded tool result.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("c.txt")).unwrap(),
            "gamma"
        );
        let answered = |id: &str| {
            sess.history
                .as_slice()
                .iter()
                .any(|m| m.tool_call_id.as_deref() == Some(id))
        };
        assert!(answered("r1") && answered("r2") && answered("w1"));
        // The read results carried each file's contents back.
        let content_of = |id: &str| {
            sess.history
                .as_slice()
                .iter()
                .find(|m| m.tool_call_id.as_deref() == Some(id))
                .map(|m| m.content.clone())
                .unwrap()
        };
        assert!(content_of("r1").contains("alpha"), "{}", content_of("r1"));
        assert!(content_of("r2").contains("beta"), "{}", content_of("r2"));
    }

    #[test]
    fn is_read_only_classifies_orientation_tools() {
        for t in ["read_lines", "search", "tree"] {
            assert!(is_read_only(t), "{t} is read-only");
        }
        for t in ["edit", "bash", "git", "task", "plan", "delegate"] {
            assert!(!is_read_only(t), "{t} is not read-only");
        }
    }

    #[test]
    fn turn_edited_detects_only_edit_calls() {
        let edit = Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "edit".into(),
                arguments: json!({ "path": "a.rs" }),
            }],
            tool_call_id: None,
        };
        let read = Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "2".into(),
                name: "read".into(),
                arguments: json!({ "path": "a.rs" }),
            }],
            tool_call_id: None,
        };
        assert!(turn_edited(std::slice::from_ref(&edit)));
        assert!(!turn_edited(std::slice::from_ref(&read)));
        assert!(!turn_edited(&[]));
    }

    #[test]
    fn verify_summary_is_terse() {
        assert_eq!(verify_summary(true, "irrelevant"), "verification passed");
        assert_eq!(
            verify_summary(false, "\n  error[E0382]: borrow of moved value\nmore\n"),
            "error[E0382]: borrow of moved value"
        );
        let long = "x".repeat(200);
        assert!(verify_summary(false, &long).ends_with('…'));
    }

    // ---- Phase 4: sub-agent delegation ----

    #[tokio::test]
    async fn delegate_runs_a_nested_turn_and_returns_only_the_summary() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(256);
        // Parent delegates; the sub-agent works then settles; its work is
        // summarized; the parent then settles.
        let transport = MockTransport::new(vec![
            vec![tool_chunk(
                "delegate",
                json!({ "objective": "do the subtask" }),
            )],
            vec![text_chunk("did the secret sub-work", true)],
            vec![text_chunk("Summary: built the subtask", true)],
            vec![text_chunk("all done", true)],
        ]);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = session(&dir);

        agent.run_turn(&mut sess, "please delegate this").await;
        drop(agent);

        let mut started = Vec::new();
        let mut finished = Vec::new();
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::SubAgentStarted { objective, .. } => started.push(objective),
                AgentEvent::SubAgentFinished { summary, .. } => finished.push(summary),
                AgentEvent::Done => done = true,
                _ => {}
            }
        }
        assert_eq!(started, vec!["do the subtask".to_string()]);
        assert_eq!(finished.len(), 1);
        assert!(finished[0].contains("built the subtask"));
        assert!(done);

        // The parent context got the summary as the delegate result — and never
        // the sub-agent's raw transcript.
        let joined: String = sess
            .history
            .as_slice()
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("built the subtask"), "summary folded back");
        assert!(
            !joined.contains("did the secret sub-work"),
            "the sub-transcript must not leak into the parent context"
        );
    }

    #[tokio::test]
    async fn depth_two_delegation_is_refused() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(256);
        // Parent delegates; the sub-agent itself tries to delegate (refused),
        // then settles directly; summarized; parent settles.
        let transport = MockTransport::new(vec![
            vec![tool_chunk("delegate", json!({ "objective": "outer" }))],
            vec![tool_chunk("delegate", json!({ "objective": "inner" }))],
            vec![text_chunk("did it myself instead", true)],
            vec![text_chunk("Summary: outer done", true)],
            vec![text_chunk("done", true)],
        ]);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = session(&dir);

        agent.run_turn(&mut sess, "delegate").await;
        drop(agent);

        let mut started = 0;
        let mut finished = 0;
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::SubAgentStarted { .. } => started += 1,
                AgentEvent::SubAgentFinished { .. } => finished += 1,
                AgentEvent::Done => done = true,
                _ => {}
            }
        }
        // Only the outer delegation spawned a sub-agent; the inner one was
        // refused before starting another.
        assert_eq!(started, 1, "no second-level sub-agent is spawned");
        assert_eq!(finished, 1);
        assert!(done);
    }

    #[tokio::test]
    async fn explore_subagent_is_read_only_and_returns_only_the_summary() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(256);
        // Parent spawns an `explore` sub-agent. The sub-turn runs in a read-only
        // mode, so an `edit` it attempts is refused (no file written); it then
        // reports, its work is summarized, and the parent settles.
        let transport = MockTransport::new(vec![
            vec![tool_chunk(
                "explore",
                json!({ "objective": "find where auth lives" }),
            )],
            vec![tool_chunk(
                "edit",
                json!({ "path": "hack.rs", "content": "nope" }),
            )],
            vec![text_chunk("auth lives in src/auth.rs", true)],
            vec![text_chunk("Map: src/auth.rs handles auth", true)],
            vec![text_chunk("thanks", true)],
        ]);
        let agent = Agent::new(Box::new(transport), tx);
        let mut sess = session(&dir);

        agent.run_turn(&mut sess, "where is auth?").await;
        drop(agent);

        let mut kind = None;
        let mut finished = Vec::new();
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::SubAgentStarted { kind: k, .. } => kind = Some(k),
                AgentEvent::SubAgentFinished { summary, .. } => finished.push(summary),
                _ => {}
            }
        }
        // The event is labeled by sub-agent type.
        assert_eq!(kind.as_deref(), Some("explore"));
        assert_eq!(finished.len(), 1);
        assert!(finished[0].contains("auth"));
        // Read-only: the sub-agent's edit was refused, so no file was written.
        assert!(
            !dir.path().join("hack.rs").exists(),
            "an explore sub-agent must not be able to edit files"
        );
        // The sub-turn's restricted mode was set aside; the parent is Agent again.
        assert_eq!(sess.mode, crate::Mode::Agent);
        // Only the summary folded back — never the sub-agent's raw transcript.
        let joined: String = sess
            .history
            .as_slice()
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("Map: src/auth.rs handles auth"),
            "the summary folds back into the parent context"
        );
        assert!(
            !joined.contains("auth lives in src/auth.rs"),
            "the sub-agent's raw transcript must not leak into the parent context"
        );
    }

    #[tokio::test]
    async fn interrupt_during_a_sub_turn_unwinds_cleanly() {
        let dir = TempDir::new();
        let (tx, mut rx) = mpsc::channel(256);
        let (int_tx, int_rx) = watch::channel(());
        // Parent delegates (served normally); the sub-agent's turn hangs, so the
        // interrupt lands inside it.
        let transport = MockTransport::hanging_from(
            vec![vec![tool_chunk("delegate", json!({ "objective": "work" }))]],
            1,
        );
        let agent = Agent::new(Box::new(transport), tx).with_interrupt(int_rx);
        let mut sess = session(&dir);

        let handle = tokio::spawn(async move {
            agent.run_turn(&mut sess, "delegate").await;
            sess
        });

        let mut interrupted = false;
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::SubAgentStarted { .. } => {
                    // The sub-turn is now hanging; interrupt it.
                    let _ = int_tx.send(());
                }
                AgentEvent::Interrupted => interrupted = true,
                AgentEvent::Done => done = true,
                _ => {}
            }
        }
        let sess = handle.await.unwrap();
        assert!(interrupted, "the interrupt unwinds the parent turn");
        assert!(!done, "an interrupted turn does not also report Done");
        // The parent context was restored and the delegate call answered with a
        // synthetic result, so the conversation stays valid.
        let answered = sess
            .history
            .as_slice()
            .iter()
            .any(|m| m.tool_call_id.as_deref() == Some("tc1"));
        assert!(answered, "the delegate call still gets a tool result");
    }
}
