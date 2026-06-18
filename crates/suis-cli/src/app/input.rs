//! Keyboard handling.
//!
//! [`handle_key`] maps a key event to either a direct mutation of [`AppState`]
//! (scrolling, typing, navigation, panel toggles) or an [`Action`] for the
//! event loop to carry out (anything needing the agent channel or terminal —
//! quitting, submitting a turn, selecting a model, answering a prompt).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use suis_agent::{Mode, PermissionDecision, PlanDecision};

use suis_core::ProviderEntry;

use super::state::{AppState, Screen};
use crate::commands;
use crate::screens::diff_screen::DiffDecision;
use crate::screens::provider_form::ProviderForm;

/// An effect the event loop must perform; everything else is handled in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing for the loop to do.
    None,
    /// Exit the application.
    Quit,
    /// Submit chat input (a message or a slash command).
    Submit(String),
    /// Select the highlighted model and open the chat session.
    SelectModel,
    /// Open the add-provider form from the model-select "add a provider" row,
    /// returning there once the form is saved or cancelled.
    AddProvider,
    /// Leave the providers screen for its return target, re-running discovery
    /// first when that target is the model picker (first-run onboarding) so the
    /// list reflects any providers just added or enabled.
    LeaveProviders,
    /// Run the capability-verification probe for the model in `verify_caps`,
    /// then rebuild the agent with the resolved capabilities.
    ProbeCaps,
    /// Answer the open permission prompt.
    AnswerPermission(PermissionDecision),
    /// Answer the current project-init question (`true` = yes, `false` = no).
    InitAnswer(bool),
    /// Confirm the per-entry `.gitignore` import with the current selections.
    InitConfirmImport,
    /// Skip the `.gitignore` import entirely (import nothing).
    InitSkipImport,
    /// Persist the current provider enable/disable toggles to `providers.json`.
    PersistProviders,
    /// Save the provider form: validate, persist, and re-probe.
    SaveProvider,
    /// Remove the provider with this id (drop a custom one, disable a default).
    RemoveProvider(String),
    /// Revoke the highlighted stored permission (confirmed in the overlay).
    RevokePermission,
    /// Add a project-level command deny for this pattern.
    AddDeny(String),
    /// Probe a provider's connection and show the outcome. `in_form` routes the
    /// result to the form (true) or the provider list (false).
    TestProvider { entry: ProviderEntry, in_form: bool },
    /// Re-probe a single provider from the `/providers` list (Enter) and fold the
    /// fresh outcome back into its row status — so a server turned on mid-session
    /// flips from offline to online without restarting Suis.
    ReprobeProvider { entry: ProviderEntry },
    /// Switch the session's runtime mode (Shift+Tab cycle or `/plan` etc.).
    SetMode(Mode),
    /// Answer the open plan-draft review.
    AnswerPlan(PlanDecision),
    /// Start the implementation session in `state.pending_implement`.
    StartImplement,
    /// The user approved verification: send the verify tasks as the next turn.
    BeginVerification,
    /// The user confirmed "Continue to next Step?" — advance to the next step.
    StartNextStep,
    /// Interrupt the running turn (Esc while busy).
    Interrupt,
    /// Copy the whole conversation to the clipboard (`Ctrl+Y`, developer mode).
    CopyTranscript,
}

/// Handle one key event against the current state.
pub fn handle_key(state: &mut AppState, key: KeyEvent) -> Action {
    // Ctrl+C always quits.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Action::Quit;
    }

    // A transient advisory popup sits on top of everything: any key dismisses it
    // and is swallowed so it doesn't leak to the screen beneath.
    if state.notice.is_some() {
        state.notice = None;
        return Action::None;
    }

    // A blocking permission prompt captures all input first, then the other
    // modal overlays (plan review, implement confirmation, verify prompt).
    if state.awaiting_permission() {
        return handle_permission(state, key);
    }
    if state.pending_plan.is_some() {
        return handle_plan_review(state, key);
    }
    if state.pending_implement.is_some() {
        return handle_implement_confirm(state, key);
    }
    if state.pending_verify {
        return handle_verify_prompt(state, key);
    }
    if state.pending_next_step.is_some() {
        return handle_next_step_prompt(state, key);
    }
    if state.verify_caps.is_some() {
        return handle_verify_caps(state, key);
    }
    // The `/usage` detail popup is a read-only overlay: any dismiss key closes
    // it, everything else is swallowed so keystrokes don't leak to the input.
    if state.show_usage {
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
            state.show_usage = false;
        }
        return Action::None;
    }

    match state.screen {
        Screen::Error => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
            _ => Action::None,
        },
        Screen::ProjectInit => handle_project_init(state, key),
        Screen::ModelSelect => handle_model_select(state, key),
        Screen::Providers => handle_providers(state, key),
        Screen::ProviderForm => handle_provider_form(state, key),
        Screen::Permissions => handle_permissions(state, key),
        Screen::PlanSelect => handle_plan_select(state, key),
        Screen::Chat => handle_chat(state, key),
        Screen::Diff => handle_diff(state, key),
    }
}

/// Number of transcript lines moved per wheel notch on the chat screen.
const WHEEL_LINES: usize = 3;

/// Handle one mouse event. A left click on the chat transcript toggles the
/// collapsible card (tool card or thinking block) under the cursor. The scroll
/// wheel scrolls the active screen's own content (the chat transcript, or the
/// cursor through a list) rather than letting the terminal turn the wheel into
/// Up/Down arrows — which on the chat screen would otherwise walk the input
/// history. Other clicks and drags are ignored. Mouse handling never produces an
/// [`Action`]; it is all handled in place, so this always returns
/// [`Action::None`].
pub fn handle_mouse(state: &mut AppState, mouse: MouseEvent) -> Action {
    // A left click on the chat transcript toggles the collapsible card (a tool
    // card or a thinking block) it landed on; clicks elsewhere are ignored.
    if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
        if state.screen == Screen::Chat && state.pending_plan.is_none() {
            state.handle_transcript_click(mouse.column, mouse.row);
        }
        return Action::None;
    }
    let up = match mouse.kind {
        MouseEventKind::ScrollUp => true,
        MouseEventKind::ScrollDown => false,
        _ => return Action::None,
    };
    // The plan review overlays the chat; the wheel scrolls it, not the
    // transcript behind it.
    if state.pending_plan.is_some() {
        state.scroll_plan_v(if up { -1 } else { 1 });
        return Action::None;
    }
    match state.screen {
        Screen::Chat => {
            for _ in 0..WHEEL_LINES {
                if up {
                    state.scroll_up();
                } else {
                    state.scroll_down();
                }
            }
        }
        Screen::ModelSelect => {
            if up {
                state.model_select.move_up();
            } else {
                state.model_select.move_down();
            }
        }
        Screen::Providers => {
            if up {
                state.providers.move_up();
            } else {
                state.providers.move_down();
            }
        }
        Screen::Permissions => {
            if up {
                state.permissions.move_up();
            } else {
                state.permissions.move_down();
            }
        }
        Screen::PlanSelect => {
            if up {
                state.plan_select.move_up();
            } else {
                state.plan_select.move_down();
            }
        }
        _ => {}
    }
    Action::None
}

/// Number of lines a PageUp/PageDown moves the plan review.
const PLAN_PAGE: i16 = 10;

/// Keys while a plan-draft review is open: Enter approves, Esc rejects, and the
/// arrows / PageUp / PageDown scroll a plan too large for the popup. Scrolling
/// is handled in place (the offset lives in `pending_plan`), so it returns
/// [`Action::None`]; the renderer clamps the offset to the content.
fn handle_plan_review(state: &mut AppState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Enter => Action::AnswerPlan(PlanDecision::Approve),
        KeyCode::Esc => Action::AnswerPlan(PlanDecision::Reject),
        KeyCode::Up => {
            state.scroll_plan_v(-1);
            Action::None
        }
        KeyCode::Down => {
            state.scroll_plan_v(1);
            Action::None
        }
        KeyCode::PageUp => {
            state.scroll_plan_v(-PLAN_PAGE);
            Action::None
        }
        KeyCode::PageDown => {
            state.scroll_plan_v(PLAN_PAGE);
            Action::None
        }
        KeyCode::Left => {
            state.scroll_plan_h(-2);
            Action::None
        }
        KeyCode::Right => {
            state.scroll_plan_h(2);
            Action::None
        }
        _ => Action::None,
    }
}

/// Keys while the "start implementation session?" confirmation is open.
fn handle_implement_confirm(state: &mut AppState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => Action::StartImplement,
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            state.pending_implement = None;
            Action::None
        }
        _ => Action::None,
    }
}

/// Keys while the "begin verification?" prompt is open.
fn handle_verify_prompt(state: &mut AppState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
            state.pending_verify = false;
            Action::BeginVerification
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            state.pending_verify = false;
            state.push_system(
                "Verification deferred — the session stays open; send a message when ready.",
            );
            Action::None
        }
        _ => Action::None,
    }
}

/// Keys while the "Continue to next Step?" prompt is open.
fn handle_next_step_prompt(state: &mut AppState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => Action::StartNextStep,
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            state.pending_next_step = None;
            state.push_system(
                "Continuing in the current step. Use /implement to start another step manually.",
            );
            Action::None
        }
        _ => Action::None,
    }
}

/// Keys on the `/implement` plan-selection screen. Enter drills in or selects;
/// Esc backs out of the drill-down and then leaves the screen.
fn handle_plan_select(state: &mut AppState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            if !state.plan_select.back() {
                state.screen = Screen::Chat;
            }
            Action::None
        }
        KeyCode::Up => {
            state.plan_select.move_up();
            Action::None
        }
        KeyCode::Down => {
            state.plan_select.move_down();
            Action::None
        }
        KeyCode::Enter => match state.plan_select.enter() {
            Some(selection) => {
                state.screen = Screen::Chat;
                state.pending_implement = Some(crate::app::state::ImplementState {
                    plan_id: selection.plan_id,
                    step_index: selection.step_index,
                    plan_title: selection.plan_title,
                    step_title: selection.step_title,
                });
                if state.has_conversation() {
                    // The confirmation overlay takes it from here.
                    Action::None
                } else {
                    Action::StartImplement
                }
            }
            None => Action::None,
        },
        _ => Action::None,
    }
}

/// Keys on the first-run project-init screen. Most steps are a yes/no answer
/// (Esc declines). The `.gitignore` import step is an interactive list: ↑/↓
/// move, Space (or ←/→) cycles a row's class, Enter imports the selections, and
/// N/Esc skips the import entirely.
fn handle_project_init(state: &mut AppState, key: KeyEvent) -> Action {
    if let Some(init) = state.project_init.as_mut() {
        if init.on_import_step() {
            return match key.code {
                KeyCode::Up => {
                    init.move_cursor(-1);
                    Action::None
                }
                KeyCode::Down => {
                    init.move_cursor(1);
                    Action::None
                }
                KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right => {
                    init.cycle_current();
                    Action::None
                }
                KeyCode::Enter => Action::InitConfirmImport,
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Action::InitSkipImport,
                _ => Action::None,
            };
        }
    }
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Action::InitAnswer(true),
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Action::InitAnswer(false),
        _ => Action::None,
    }
}

/// Keys while a permission prompt is open. The choices are an explicit menu in
/// the popup: ↑/↓ move the highlight, Enter confirms it, a number key picks a
/// row directly, and Esc denies this invocation.
fn handle_permission(state: &mut AppState, key: KeyEvent) -> Action {
    let prompt = &mut state.pending_permission.as_mut().unwrap().prompt;
    match key.code {
        KeyCode::Esc => Action::AnswerPermission(PermissionDecision::deny()),
        KeyCode::Up => {
            prompt.move_selection(-1);
            Action::None
        }
        KeyCode::Down => {
            prompt.move_selection(1);
            Action::None
        }
        KeyCode::Enter => {
            // Shift+confirm applies the highlighted row's advanced variant
            // (wildcard allow / deny for project) when it has one.
            let shift = key.modifiers.contains(KeyModifiers::SHIFT);
            Action::AnswerPermission(prompt.selected_decision(shift))
        }
        KeyCode::Char(c) => match c.to_digit(10).and_then(|n| prompt.decide(n as usize)) {
            Some(decision) => Action::AnswerPermission(decision),
            None => Action::None,
        },
        _ => Action::None,
    }
}

/// Keys on the model-selection screen. Filtering is type-to-filter (20.3): any
/// printable key narrows the list live, so `/` and provider-prefixed ids are
/// literal filter text. Esc clears a non-empty filter first, then quits on a
/// second press. Enter selects; an unverified model then raises the
/// "Verify capabilities?" prompt on the chat screen (see `select_model`).
fn handle_model_select(state: &mut AppState, key: KeyEvent) -> Action {
    // The remove-confirmation overlay owns input while it is open (a provider was
    // staged for deletion with Ctrl+D on its header).
    if state.pending_provider_remove.is_some() {
        return match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                Action::RemoveProvider(state.pending_provider_remove.take().unwrap())
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                state.pending_provider_remove = None;
                Action::None
            }
            _ => Action::None,
        };
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            if !state.model_select.filter.is_empty() {
                state.model_select.clear_filter();
                Action::None
            } else if state.selected_model.is_some() {
                // Opened mid-session (a model is already chosen): Esc returns to
                // the chat rather than quitting the app.
                state.screen = Screen::Chat;
                Action::None
            } else {
                Action::Quit
            }
        }
        KeyCode::Up => {
            state.model_select.move_up();
            Action::None
        }
        KeyCode::Down => {
            state.model_select.move_down();
            Action::None
        }
        KeyCode::Backspace => {
            state.model_select.pop_filter();
            Action::None
        }
        KeyCode::Enter => {
            // The add row opens the form; an online provider header expands or
            // collapses in place; an offline/errored header re-probes (retry);
            // only a model row starts a session.
            if state.model_select.is_add_provider_selected() {
                Action::AddProvider
            } else if state.model_select.toggle_selected_provider() {
                Action::None
            } else if let Some(entry) = state.model_select.begin_reprobe_selected() {
                Action::ReprobeProvider { entry }
            } else {
                Action::SelectModel
            }
        }
        // Provider-management keys on a header row. Modified so they never collide
        // with type-to-filter (every plain letter narrows the list).
        KeyCode::Char('e') if ctrl => {
            open_edit_form_from_picker(state);
            Action::None
        }
        KeyCode::Char('d') if ctrl => {
            if let Some(id) = state.model_select.selected_provider_id() {
                state.pending_provider_remove = Some(id);
            }
            Action::None
        }
        KeyCode::Char('t') if ctrl => match state.model_select.selected_provider_entry() {
            Some(entry) => {
                state.model_select.begin_test();
                Action::TestProvider {
                    entry,
                    in_form: false,
                }
            }
            None => Action::None,
        },
        KeyCode::Char(c) if !ctrl => {
            state.model_select.push_filter(c);
            Action::None
        }
        _ => Action::None,
    }
}

/// Build and open the edit form for the provider header under the picker's
/// cursor, prefilled from its group and returning to the picker on save/cancel.
/// Sourced from the picker (not `state.providers`, which is empty at the initial
/// selection screen).
fn open_edit_form_from_picker(state: &mut AppState) {
    let Some(entry) = state.model_select.selected_provider_entry() else {
        return;
    };
    let Ok(transport) = suis_providers::TransportType::parse(&entry.transport) else {
        return;
    };
    let existing_ids: Vec<String> = state
        .model_select
        .provider_ids()
        .into_iter()
        .filter(|id| id != &entry.id)
        .collect();
    let form = ProviderForm::new_edit(
        entry.id.clone(),
        entry.enabled,
        entry.name.clone().unwrap_or_else(|| entry.id.clone()),
        entry.id.clone(),
        entry.endpoint.clone(),
        transport,
        entry.api_key_env.clone(),
        existing_ids,
    );
    state.provider_form = Some(form);
    state.provider_form_return = Screen::ModelSelect;
    state.screen = Screen::ProviderForm;
}

/// Keys while the chat-screen "Verify capabilities?" prompt is open. Y starts
/// the probe (the popup then shows a spinner and swallows input until it
/// returns); N (or Esc) declines — remembered for the session — and keeps the
/// model on its conservative chat-only default.
fn handle_verify_caps(state: &mut AppState, key: KeyEvent) -> Action {
    // While the probe runs the spinner owns the popup; ignore all input.
    if state.verify_caps_probing() {
        return Action::None;
    }
    match key.code {
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => Action::ProbeCaps,
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            if let Some(vc) = state.verify_caps.take() {
                state
                    .declined_caps
                    .insert(crate::app::state::caps_consent_key(
                        &vc.entry.provider_id,
                        &vc.entry.model.model_id,
                    ));
            }
            Action::None
        }
        _ => Action::None,
    }
}

/// Keys on the providers screen. Space toggles enable; `a`/`e`/`d` add, edit, or
/// remove an entry; `t` tests the selected provider's connection; Esc returns
/// to chat. While a remove is staged, the confirm overlay captures input first.
fn handle_providers(state: &mut AppState, key: KeyEvent) -> Action {
    // The remove-confirmation overlay owns input while it is open.
    if state.pending_provider_remove.is_some() {
        return match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                Action::RemoveProvider(state.pending_provider_remove.take().unwrap())
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                state.pending_provider_remove = None;
                Action::None
            }
            _ => Action::None,
        };
    }

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => leave_providers(state),
        // During onboarding Enter is the "continue to the model picker"
        // affordance; in the `/providers` flow it re-probes the selected provider
        // so a server brought up mid-session refreshes without a restart.
        KeyCode::Enter if state.providers_return == Screen::ModelSelect => Action::LeaveProviders,
        KeyCode::Enter => match state.providers.selected_entry() {
            Some(entry) => {
                state.providers.begin_reprobe();
                Action::ReprobeProvider { entry }
            }
            None => Action::None,
        },
        KeyCode::Up => {
            state.providers.move_up();
            Action::None
        }
        KeyCode::Down => {
            state.providers.move_down();
            Action::None
        }
        KeyCode::Char(' ') => {
            state.providers.toggle_selected();
            Action::PersistProviders
        }
        KeyCode::Char('a') => {
            state.provider_form = Some(ProviderForm::new_add(state.providers.ids()));
            state.provider_form_return = Screen::Providers;
            state.screen = Screen::ProviderForm;
            Action::None
        }
        KeyCode::Char('e') => {
            open_edit_form(state);
            Action::None
        }
        KeyCode::Char('d') => {
            if let Some(row) = state.providers.selected() {
                state.pending_provider_remove = Some(row.id.clone());
            }
            Action::None
        }
        KeyCode::Char('t') => match state.providers.selected_entry() {
            Some(entry) => {
                state.providers.begin_test();
                Action::TestProvider {
                    entry,
                    in_form: false,
                }
            }
            None => Action::None,
        },
        _ => Action::None,
    }
}

/// Keys on the stored-permissions screen (`/permissions`). The revoke-confirm
/// overlay and the "new deny" text input each own input while open; otherwise
/// `↑/↓` navigate, `d` stages a revoke, `n` opens the deny input, and `Esc`/`q`
/// returns to chat.
fn handle_permissions(state: &mut AppState, key: KeyEvent) -> Action {
    // The revoke-confirmation overlay owns input while it is open.
    if state.pending_permission_revoke.is_some() {
        return match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => Action::RevokePermission,
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                state.pending_permission_revoke = None;
                Action::None
            }
            _ => Action::None,
        };
    }

    // The "new deny" text input owns keys while it is open.
    if state.permissions.add_input().is_some() {
        return match key.code {
            KeyCode::Enter => match state.permissions.take_add() {
                Some(pattern) => Action::AddDeny(pattern),
                None => Action::None,
            },
            KeyCode::Esc => {
                state.permissions.cancel_add();
                Action::None
            }
            KeyCode::Backspace => {
                state.permissions.pop_add_char();
                Action::None
            }
            KeyCode::Char(c) => {
                state.permissions.push_add_char(c);
                Action::None
            }
            _ => Action::None,
        };
    }

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            state.screen = Screen::Chat;
            Action::None
        }
        KeyCode::Up => {
            state.permissions.move_up();
            Action::None
        }
        KeyCode::Down => {
            state.permissions.move_down();
            Action::None
        }
        KeyCode::Char('d') => {
            if state.permissions.selected().is_some() {
                state.pending_permission_revoke = Some(state.permissions.cursor());
            }
            Action::None
        }
        KeyCode::Char('n') => {
            state.permissions.begin_add();
            Action::None
        }
        _ => Action::None,
    }
}

/// Leave the providers screen for its return target. Returning to chat (the
/// `/providers` flow) is a plain in-place screen switch; returning to the model
/// picker (onboarding) defers to the event loop so it can re-run discovery
/// first, hence the [`Action::LeaveProviders`].
fn leave_providers(state: &mut AppState) -> Action {
    if state.providers_return == Screen::ModelSelect {
        Action::LeaveProviders
    } else {
        state.screen = state.providers_return;
        Action::None
    }
}

/// Build and open the edit form for the highlighted provider, prefilled from its
/// row, with every other id reserved for the uniqueness check.
fn open_edit_form(state: &mut AppState) {
    let Some(row) = state.providers.selected() else {
        return;
    };
    let existing_ids: Vec<String> = state
        .providers
        .ids()
        .into_iter()
        .filter(|id| id != &row.id)
        .collect();
    let form = ProviderForm::new_edit(
        row.id.clone(),
        row.enabled,
        row.name.clone(),
        row.id.clone(),
        row.endpoint.clone(),
        row.transport,
        row.api_key_env().map(str::to_string),
        existing_ids,
    );
    state.provider_form = Some(form);
    state.provider_form_return = Screen::Providers;
    state.screen = Screen::ProviderForm;
}

/// Keys on the add/edit provider form. Tab/↑/↓ move between controls; on the
/// transport picker ←/→/Space switch the language; Ctrl+T tests the draft;
/// Enter saves; Esc cancels back to the providers list. Plain letters type into
/// the focused text field (so `t` in a URL is a literal `t`, not a test).
fn handle_provider_form(state: &mut AppState, key: KeyEvent) -> Action {
    use crate::screens::provider_form::FormField;

    let Some(form) = state.provider_form.as_mut() else {
        state.screen = state.provider_form_return;
        return Action::None;
    };

    // The preset chooser is the first step of an Add.
    if form.choosing_preset() {
        match key.code {
            KeyCode::Up => form.preset_move(-1),
            KeyCode::Down => form.preset_move(1),
            KeyCode::Enter => form.choose_preset(),
            KeyCode::Esc => {
                state.provider_form = None;
                state.screen = state.provider_form_return;
            }
            _ => {}
        }
        return Action::None;
    }

    // Ctrl+T tests the current draft (a non-letter binding so text fields keep
    // every printable key, including the `t` in `http`).
    if key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL) {
        let entry = form.draft_entry();
        form.begin_test();
        return Action::TestProvider {
            entry,
            in_form: true,
        };
    }

    match key.code {
        KeyCode::Esc => {
            state.provider_form = None;
            state.screen = state.provider_form_return;
            Action::None
        }
        KeyCode::Enter => Action::SaveProvider,
        KeyCode::Tab | KeyCode::Down => {
            form.focus_next();
            Action::None
        }
        KeyCode::BackTab | KeyCode::Up => {
            form.focus_prev();
            Action::None
        }
        KeyCode::Backspace => {
            form.backspace();
            Action::None
        }
        KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
            if form.focus() == FormField::Transport =>
        {
            form.toggle_transport();
            Action::None
        }
        KeyCode::Char(c) => {
            form.push_char(c);
            Action::None
        }
        _ => Action::None,
    }
}

/// Keys on the chat screen.
fn handle_chat(state: &mut AppState, key: KeyEvent) -> Action {
    // Ctrl+D opens the most recent edit's diff full-screen.
    if key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if state.last_diff.is_some() {
            state.screen = Screen::Diff;
        }
        return Action::None;
    }

    // Ctrl+O toggles full output on the most recent completed tool card.
    if key.code == KeyCode::Char('o') && key.modifiers.contains(KeyModifiers::CONTROL) {
        state.toggle_last_tool_card();
        return Action::None;
    }

    // Ctrl+T expands/collapses the most recent thinking block.
    if key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL) {
        state.toggle_last_thinking();
        return Action::None;
    }

    // Ctrl+Y copies the whole conversation to the clipboard — a developer-mode
    // affordance for grabbing the raw history (a CLI can't shift-drag-select).
    // Inert unless developer mode is on, matching the footer hint.
    if key.code == KeyCode::Char('y') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return if state.developer {
            Action::CopyTranscript
        } else {
            Action::None
        };
    }

    // Shift+Tab cycles the runtime mode. Most terminals report it as BackTab;
    // kitty-protocol terminals may report Tab with the SHIFT modifier.
    let shift_tab = key.code == KeyCode::BackTab
        || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT));
    if shift_tab {
        if state.busy {
            state.push_system("Cannot switch modes while the agent is working.");
            return Action::None;
        }
        return Action::SetMode(state.mode.next());
    }

    // Alt+Enter inserts a newline for multi-line composition. (Shift+Enter is
    // reserved for queued messages per AGENT_RUNTIME.md.)
    if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::ALT) {
        state.push_char('\n');
        return Action::None;
    }

    // While the command palette is open it owns Up/Down/Tab/Esc; Enter accepts
    // the selection into the buffer and falls through to the ordinary submit
    // path, so a palette-accepted command runs exactly like a typed one.
    if state.palette_open() {
        match key.code {
            KeyCode::Up => {
                state.palette_move(-1);
                return Action::None;
            }
            KeyCode::Down => {
                state.palette_move(1);
                return Action::None;
            }
            KeyCode::Tab => {
                if let Some(name) = state.palette_selected_name() {
                    state.set_input(format!("/{name}"));
                }
                return Action::None;
            }
            KeyCode::Esc => {
                state.dismiss_palette();
                return Action::None;
            }
            KeyCode::Enter => {
                if let Some(name) = state.palette_selected_name() {
                    state.set_input(format!("/{name}"));
                }
            }
            _ => {} // type through; the palette re-filters from the buffer
        }
    }

    match key.code {
        KeyCode::Enter => {
            let text = state.input.trim().to_string();
            if text.is_empty() {
                Action::None
            } else {
                state.history.push(&text);
                state.take_input();
                Action::Submit(text)
            }
        }
        KeyCode::Tab => {
            if let Some(completed) = commands::complete(&state.input) {
                state.set_input(completed);
            }
            Action::None
        }
        KeyCode::Backspace => {
            state.backspace();
            Action::None
        }
        KeyCode::Left => {
            state.cursor_left();
            Action::None
        }
        KeyCode::Right => {
            state.cursor_right();
            Action::None
        }
        KeyCode::Home => {
            state.cursor_home();
            Action::None
        }
        KeyCode::End => {
            state.cursor_end();
            Action::None
        }
        KeyCode::Up => {
            state.history_up();
            Action::None
        }
        KeyCode::Down => {
            state.history_down();
            Action::None
        }
        KeyCode::PageUp => {
            state.scroll_up();
            Action::None
        }
        KeyCode::PageDown => {
            state.scroll_down();
            Action::None
        }
        KeyCode::Esc => {
            // While the agent works, Esc interrupts the turn (the draft is
            // kept); requesting it twice is a no-op. Idle Esc clears the input.
            if state.busy {
                if state.interrupting {
                    return Action::None;
                }
                state.interrupting = true;
                return Action::Interrupt;
            }
            state.clear_input();
            Action::None
        }
        KeyCode::Char(c) => {
            state.push_char(c);
            Action::None
        }
        _ => Action::None,
    }
}

/// Keys on the full-screen diff viewer. The agent applies edits as it makes
/// them, so the choices here dismiss the view with a note rather than gating a
/// write (interactive apply/reject arrives with Project 5's diff-approval hook).
fn handle_diff(state: &mut AppState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => state.screen = Screen::Chat,
        KeyCode::Char(c) => {
            if let Some(decision) = DiffDecision::from_key(c) {
                let note = match decision {
                    DiffDecision::Apply => "Kept the edit.",
                    DiffDecision::Reject => {
                        "The edit was already written by the agent; reverting is not yet supported."
                    }
                    DiffDecision::Skip => "Skipped.",
                };
                state.push_system(note);
                state.screen = Screen::Chat;
            }
        }
        _ => {}
    }
    Action::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::model_select::ModelSelect;
    use suis_core::PermissionScope;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn chat_state() -> AppState {
        let mut s = AppState::new(ModelSelect::default());
        s.screen = Screen::Chat;
        s
    }

    #[test]
    fn ctrl_c_quits_from_any_screen() {
        let mut s = chat_state();
        let action = handle_key(
            &mut s,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert_eq!(action, Action::Quit);
    }

    #[test]
    fn typing_appends_to_input() {
        let mut s = chat_state();
        handle_key(&mut s, key(KeyCode::Char('h')));
        handle_key(&mut s, key(KeyCode::Char('i')));
        assert_eq!(s.input, "hi");
    }

    #[test]
    fn ctrl_y_copies_only_in_developer_mode() {
        let mut s = chat_state();
        // Off by default: Ctrl+Y is inert and does not eat the keystroke's turn.
        assert_eq!(handle_key(&mut s, ctrl(KeyCode::Char('y'))), Action::None);
        // With developer mode on it requests a transcript copy.
        s.developer = true;
        assert_eq!(
            handle_key(&mut s, ctrl(KeyCode::Char('y'))),
            Action::CopyTranscript
        );
    }

    #[test]
    fn any_key_dismisses_the_notice_popup_and_is_swallowed() {
        let mut s = chat_state();
        s.notice = Some("plaintext key warning".into());
        // An ordinary key dismisses the popup and does not leak into the input.
        let action = handle_key(&mut s, key(KeyCode::Char('x')));
        assert_eq!(action, Action::None);
        assert!(s.notice.is_none());
        assert!(
            s.input.is_empty(),
            "the dismiss key must not reach the input"
        );
    }

    #[test]
    fn ctrl_c_still_quits_with_a_notice_open() {
        let mut s = chat_state();
        s.notice = Some("warning".into());
        let action = handle_key(
            &mut s,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert_eq!(action, Action::Quit);
    }

    #[test]
    fn enter_submits_trimmed_nonempty_input() {
        let mut s = chat_state();
        s.input = "  hello  ".into();
        let action = handle_key(&mut s, key(KeyCode::Enter));
        assert_eq!(action, Action::Submit("hello".into()));
        assert!(s.input.is_empty());
    }

    #[test]
    fn enter_on_empty_input_does_nothing() {
        let mut s = chat_state();
        assert_eq!(handle_key(&mut s, key(KeyCode::Enter)), Action::None);
    }

    #[test]
    fn tab_completes_slash_command() {
        let mut s = chat_state();
        s.input = "/he".into();
        handle_key(&mut s, key(KeyCode::Tab));
        assert_eq!(s.input, "/help");
    }

    #[test]
    fn ctrl_o_toggles_the_latest_tool_card() {
        let mut s = chat_state();
        s.apply_event(suis_agent::AgentEvent::ToolCallStarted {
            name: "read".into(),
            args: serde_json::json!({ "path": "a.txt" }),
        });
        s.apply_event(suis_agent::AgentEvent::ToolCallCompleted {
            result: suis_agent::ToolResult::ok("c1", "contents"),
        });

        let ctrl_o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut s, ctrl_o), Action::None);
        assert!(s.messages[0].tool.as_ref().unwrap().expanded);
        handle_key(&mut s, ctrl_o);
        assert!(!s.messages[0].tool.as_ref().unwrap().expanded);
    }

    #[test]
    fn ctrl_t_toggles_the_latest_thinking_block() {
        let mut s = chat_state();
        s.apply_event(suis_agent::AgentEvent::ReasoningChunk("thinking".into()));
        s.apply_event(suis_agent::AgentEvent::Done);

        let ctrl_t = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut s, ctrl_t), Action::None);
        assert!(s.messages[0].thinking.as_ref().unwrap().expanded);
        handle_key(&mut s, ctrl_t);
        assert!(!s.messages[0].thinking.as_ref().unwrap().expanded);
    }

    #[test]
    fn left_click_on_a_tool_card_toggles_it() {
        use crossterm::event::{MouseButton, MouseEventKind};
        let mut s = chat_state();
        s.apply_event(suis_agent::AgentEvent::ToolCallStarted {
            name: "read".into(),
            args: serde_json::json!({ "path": "a.txt" }),
        });
        s.apply_event(suis_agent::AgentEvent::ToolCallCompleted {
            result: suis_agent::ToolResult::ok("c1", "line a\nline b"),
        });
        // Simulate a render placing the transcript across the screen, top-pinned.
        s.transcript_area = Some(ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        });
        s.note_transcript_metrics(1, 10);

        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(handle_mouse(&mut s, click), Action::None);
        assert!(
            s.messages[0].tool.as_ref().unwrap().expanded,
            "click expands the card"
        );
    }

    #[test]
    fn page_keys_scroll_the_transcript() {
        let mut s = chat_state();
        // Simulate a render: 100 lines of content in a 10-row viewport, so the
        // bottom offset is 90. The view starts following the bottom.
        s.note_transcript_metrics(100, 10);
        assert!(s.follow_bottom);

        // PageUp detaches from the bottom and nudges one line off it.
        handle_key(&mut s, key(KeyCode::PageUp));
        assert!(!s.follow_bottom);
        assert_eq!(s.scroll, 89);
        handle_key(&mut s, key(KeyCode::PageUp));
        assert_eq!(s.scroll, 88);

        // PageDown moves back toward the bottom; reaching it re-attaches.
        handle_key(&mut s, key(KeyCode::PageDown));
        assert_eq!(s.scroll, 89);
        handle_key(&mut s, key(KeyCode::PageDown));
        assert_eq!(s.scroll, 90);
        assert!(s.follow_bottom);
        // Further PageDown stays pinned at the bottom.
        handle_key(&mut s, key(KeyCode::PageDown));
        assert_eq!(s.scroll, 90);
        assert!(s.follow_bottom);
    }

    #[test]
    fn up_recalls_history_and_down_restores_the_draft() {
        let mut s = chat_state();
        for text in ["first", "second", "third"] {
            s.set_input(text);
            assert_eq!(
                handle_key(&mut s, key(KeyCode::Enter)),
                Action::Submit(text.into())
            );
        }
        s.set_input("a draft");

        handle_key(&mut s, key(KeyCode::Up));
        assert_eq!(s.input, "third");
        handle_key(&mut s, key(KeyCode::Up));
        assert_eq!(s.input, "second");
        handle_key(&mut s, key(KeyCode::Down));
        assert_eq!(s.input, "third");
        handle_key(&mut s, key(KeyCode::Down));
        assert_eq!(s.input, "a draft", "the stashed draft returns");
        // The transcript did not scroll: Up/Down are history now.
        assert!(s.follow_bottom);
    }

    #[test]
    fn consecutive_duplicate_submissions_collapse_in_history() {
        let mut s = chat_state();
        for _ in 0..2 {
            s.set_input("same");
            handle_key(&mut s, key(KeyCode::Enter));
        }
        handle_key(&mut s, key(KeyCode::Up));
        assert_eq!(s.input, "same");
        // Only one "same" is stored: another Up has nowhere to go.
        handle_key(&mut s, key(KeyCode::Up));
        assert_eq!(s.input, "same");
        handle_key(&mut s, key(KeyCode::Down));
        assert_eq!(s.input, "", "back to the (empty) draft after one entry");
    }

    #[test]
    fn alt_enter_inserts_a_newline_and_submit_keeps_it() {
        let mut s = chat_state();
        for c in "line one".chars() {
            handle_key(&mut s, key(KeyCode::Char(c)));
        }
        handle_key(&mut s, KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        for c in "line two".chars() {
            handle_key(&mut s, key(KeyCode::Char(c)));
        }
        assert_eq!(s.input, "line one\nline two");

        let action = handle_key(&mut s, key(KeyCode::Enter));
        assert_eq!(action, Action::Submit("line one\nline two".into()));
    }

    #[test]
    fn palette_up_down_move_selection_not_history() {
        let mut s = chat_state();
        s.set_input("previous message");
        handle_key(&mut s, key(KeyCode::Enter));

        handle_key(&mut s, key(KeyCode::Char('/')));
        handle_key(&mut s, key(KeyCode::Char('p')));
        assert!(s.palette_open());
        handle_key(&mut s, key(KeyCode::Down));
        assert_eq!(s.palette_selected_name(), Some("providers"));
        assert_eq!(s.input, "/p", "history did not fire");

        handle_key(&mut s, key(KeyCode::Tab));
        assert_eq!(s.input, "/providers", "Tab fills the selected name");
    }

    #[test]
    fn palette_enter_submits_like_a_typed_command() {
        let mut s = chat_state();
        for c in "/clear".chars() {
            handle_key(&mut s, key(KeyCode::Char(c)));
        }
        assert!(s.palette_open());
        let action = handle_key(&mut s, key(KeyCode::Enter));
        assert_eq!(action, Action::Submit("/clear".into()));
        assert!(s.input.is_empty());
    }

    #[test]
    fn palette_enter_runs_the_selected_command() {
        let mut s = chat_state();
        for c in "/comp".chars() {
            handle_key(&mut s, key(KeyCode::Char(c)));
        }
        // The unique match is /compact; Enter accepts and runs it.
        let action = handle_key(&mut s, key(KeyCode::Enter));
        assert_eq!(action, Action::Submit("/compact".into()));
    }

    #[test]
    fn palette_esc_dismisses_then_up_recalls_history() {
        let mut s = chat_state();
        s.set_input("earlier");
        handle_key(&mut s, key(KeyCode::Enter));

        handle_key(&mut s, key(KeyCode::Char('/')));
        assert!(s.palette_open());
        handle_key(&mut s, key(KeyCode::Esc));
        assert!(!s.palette_open());
        assert_eq!(s.input, "/", "Esc dismisses without clearing the buffer");

        handle_key(&mut s, key(KeyCode::Up));
        assert_eq!(s.input, "earlier");
    }

    #[test]
    fn esc_interrupts_while_busy_and_clears_input_when_idle() {
        let mut s = chat_state();
        s.set_input("a draft");
        s.busy = true;

        assert_eq!(handle_key(&mut s, key(KeyCode::Esc)), Action::Interrupt);
        assert!(s.interrupting);
        assert_eq!(s.input, "a draft", "the draft survives an interrupt");

        // A second Esc while the interrupt is pending is a no-op.
        assert_eq!(handle_key(&mut s, key(KeyCode::Esc)), Action::None);

        // Idle again: Esc clears the input as before.
        s.end_busy();
        assert_eq!(handle_key(&mut s, key(KeyCode::Esc)), Action::None);
        assert!(s.input.is_empty());
    }

    #[test]
    fn esc_with_palette_open_dismisses_before_interrupting() {
        let mut s = chat_state();
        s.busy = true;
        s.set_input("/p");
        assert!(s.palette_open());
        // First Esc goes to the palette, even while busy.
        assert_eq!(handle_key(&mut s, key(KeyCode::Esc)), Action::None);
        assert!(!s.palette_open());
        // The next Esc interrupts.
        assert_eq!(handle_key(&mut s, key(KeyCode::Esc)), Action::Interrupt);
    }

    #[test]
    fn providers_space_toggles_and_persists() {
        use crate::screens::providers::ProvidersView;
        use std::collections::HashSet;
        use suis_providers::{Provider, TransportType};

        let mut s = AppState::new(ModelSelect::default());
        s.screen = Screen::Providers;
        s.providers = ProvidersView::from_providers(
            &[Provider {
                id: "ollama".into(),
                name: "Ollama".into(),
                endpoint: "http://localhost:11434".into(),
                transport: TransportType::Ollama,
                enabled: true,
                api_key: None,
                api_key_env: None,
            }],
            &HashSet::new(),
        );

        let action = handle_key(&mut s, key(KeyCode::Char(' ')));
        assert_eq!(action, Action::PersistProviders);
        assert!(!s.providers.rows()[0].enabled, "space should toggle off");
    }

    #[test]
    fn providers_esc_returns_to_chat() {
        let mut s = AppState::new(ModelSelect::default());
        s.screen = Screen::Providers;
        let action = handle_key(&mut s, key(KeyCode::Esc));
        assert_eq!(action, Action::None);
        assert_eq!(s.screen, Screen::Chat);
    }

    #[test]
    fn esc_quits_error_screen() {
        let mut s = AppState::error("boom");
        assert_eq!(handle_key(&mut s, key(KeyCode::Esc)), Action::Quit);
    }

    #[test]
    fn model_select_enter_expands_then_selects_a_model() {
        let mut s = AppState::new(ModelSelect::from_results(&remote_results()));
        // The cursor opens on the collapsed provider header: Enter expands it
        // in place rather than starting a session.
        assert_eq!(handle_key(&mut s, key(KeyCode::Enter)), Action::None);
        // Down onto the now-visible first model, Enter selects it.
        handle_key(&mut s, key(KeyCode::Down));
        assert!(s.model_select.selected().is_some());
        assert_eq!(handle_key(&mut s, key(KeyCode::Enter)), Action::SelectModel);
    }

    #[test]
    fn model_select_enter_on_the_add_row_opens_the_form() {
        let mut s = AppState::new(ModelSelect::from_results(&remote_results()));
        while !s.model_select.is_add_provider_selected() {
            handle_key(&mut s, key(KeyCode::Up));
        }
        assert_eq!(handle_key(&mut s, key(KeyCode::Enter)), Action::AddProvider);
    }

    #[test]
    fn picker_ctrl_e_on_provider_header_opens_edit_form_returning_to_picker() {
        let mut s = AppState::new(ModelSelect::from_results(&remote_results()));
        // The cursor opens on the provider header.
        assert!(s.model_select.on_provider_header());
        assert_eq!(handle_key(&mut s, ctrl(KeyCode::Char('e'))), Action::None);
        assert_eq!(s.screen, Screen::ProviderForm);
        assert!(s.provider_form.is_some(), "edit form opened");
        assert_eq!(
            s.provider_form_return,
            Screen::ModelSelect,
            "edit returns to the picker, not the old providers screen"
        );
    }

    #[test]
    fn picker_ctrl_d_stages_then_confirms_a_remove() {
        let mut s = AppState::new(ModelSelect::from_results(&remote_results()));
        handle_key(&mut s, ctrl(KeyCode::Char('d')));
        assert_eq!(s.pending_provider_remove.as_deref(), Some("openrouter"));
        // The confirm overlay now owns input; Enter confirms the removal.
        match handle_key(&mut s, key(KeyCode::Enter)) {
            Action::RemoveProvider(id) => assert_eq!(id, "openrouter"),
            other => panic!("expected RemoveProvider, got {other:?}"),
        }
        assert!(
            s.pending_provider_remove.is_none(),
            "stage cleared on confirm"
        );
    }

    #[test]
    fn picker_ctrl_t_tests_the_selected_provider() {
        use crate::screens::provider_form::TestState;
        let mut s = AppState::new(ModelSelect::from_results(&remote_results()));
        match handle_key(&mut s, ctrl(KeyCode::Char('t'))) {
            Action::TestProvider { entry, in_form } => {
                assert_eq!(entry.id, "openrouter");
                assert!(!in_form, "an in-list test, not a form test");
            }
            other => panic!("expected TestProvider, got {other:?}"),
        }
        // The header reads "testing…" until the outcome lands.
        assert!(matches!(s.model_select.visible_test(), TestState::Testing));
    }

    #[test]
    fn picker_plain_letter_filters_rather_than_managing() {
        let mut s = AppState::new(ModelSelect::from_results(&remote_results()));
        // A bare 'e' is a filter keystroke — only Ctrl+E edits.
        assert_eq!(handle_key(&mut s, key(KeyCode::Char('e'))), Action::None);
        assert_eq!(s.screen, Screen::ModelSelect);
        assert!(s.provider_form.is_none(), "no form from a plain letter");
        assert_eq!(s.model_select.filter, "e");
    }

    #[test]
    fn picker_esc_returns_to_chat_mid_session_but_quits_at_first_pick() {
        let mut s = AppState::new(ModelSelect::from_results(&remote_results()));
        s.screen = Screen::ModelSelect;
        // No model chosen yet: Esc quits the app.
        assert!(s.selected_model.is_none());
        assert_eq!(handle_key(&mut s, key(KeyCode::Esc)), Action::Quit);

        // With a model already in use, Esc is a "back to chat" instead.
        s.selected_model = Some(suis_providers::Model::new(
            "openrouter",
            "qwen/qwen3-coder",
            suis_providers::Capabilities::default(),
        ));
        assert_eq!(handle_key(&mut s, key(KeyCode::Esc)), Action::None);
        assert_eq!(s.screen, Screen::Chat);
    }

    #[test]
    fn model_select_enter_on_offline_provider_reprobes() {
        use crate::screens::model_select::ModelSelect;
        use suis_core::ModelScope;
        use suis_providers::{ProbeOutcome, Provider, TransportType};

        let plan = vec![Provider {
            id: "lmstudio".into(),
            name: "LM Studio".into(),
            endpoint: "http://localhost:1234".into(),
            transport: TransportType::OpenAiCompatible,
            enabled: true,
            api_key: None,
            api_key_env: None,
        }];
        let mut ms = ModelSelect::from_plan(&plan, &ModelScope::All);
        ms.apply_outcome(&ProbeOutcome::Offline {
            id: "lmstudio".into(),
        });

        let mut s = AppState::new(ms);
        s.screen = Screen::ModelSelect;
        // The cursor opens on the offline header; Enter re-probes it (it has no
        // models to expand into).
        match handle_key(&mut s, key(KeyCode::Enter)) {
            Action::ReprobeProvider { entry } => assert_eq!(entry.id, "lmstudio"),
            other => panic!("expected ReprobeProvider, got {other:?}"),
        }
    }

    #[test]
    fn providers_enter_continues_only_during_onboarding() {
        let mut s = AppState::new(ModelSelect::default());
        s.screen = Screen::Providers;
        // Default return is chat with an empty list: Enter has nothing to
        // re-probe, so it is inert and Esc goes back to chat.
        assert_eq!(handle_key(&mut s, key(KeyCode::Enter)), Action::None);
        assert_eq!(s.screen, Screen::Providers);

        // Onboarding return: Esc and Enter both leave via the event loop.
        s.providers_return = Screen::ModelSelect;
        assert_eq!(
            handle_key(&mut s, key(KeyCode::Enter)),
            Action::LeaveProviders
        );
        assert_eq!(
            handle_key(&mut s, key(KeyCode::Esc)),
            Action::LeaveProviders
        );
    }

    #[test]
    fn providers_enter_reprobes_selected_provider_in_chat_flow() {
        use crate::screens::providers::ProvidersView;
        use std::collections::HashSet;
        use suis_providers::{Provider, TransportType};

        let mut s = AppState::new(ModelSelect::default());
        s.screen = Screen::Providers;
        s.providers = ProvidersView::from_providers(
            &[Provider {
                id: "ollama".into(),
                name: "Ollama".into(),
                endpoint: "http://localhost:11434".into(),
                transport: TransportType::Ollama,
                enabled: true,
                api_key: None,
                api_key_env: None,
            }],
            &HashSet::new(),
        );

        let action = handle_key(&mut s, key(KeyCode::Enter));
        match action {
            Action::ReprobeProvider { entry } => assert_eq!(entry.id, "ollama"),
            other => panic!("expected ReprobeProvider, got {other:?}"),
        }
        // The row shows it is being re-checked until the outcome lands.
        assert!(s.providers.rows()[0].checking);
    }

    fn remote_results() -> Vec<suis_providers::DiscoveryResult> {
        use suis_providers::{Capabilities, Model, Provider, TransportType};
        vec![suis_providers::DiscoveryResult {
            provider: Provider {
                id: "openrouter".into(),
                name: "OpenRouter".into(),
                endpoint: "https://openrouter.ai/api".into(),
                transport: TransportType::OpenAiCompatible,
                enabled: true,
                api_key: Some("sk-x".into()),
                api_key_env: Some("OPENROUTER_API_KEY".into()),
            },
            models: vec![
                Model::new(
                    "openrouter",
                    "qwen/qwen3-coder",
                    Capabilities::discovery_default(),
                ),
                Model::new(
                    "openrouter",
                    "meta/llama3",
                    Capabilities::discovery_default(),
                ),
            ],
        }]
    }

    #[test]
    fn typing_filters_the_model_list_directly() {
        let mut s = AppState::new(ModelSelect::from_results(&remote_results()));
        // No `/` needed: printable keys feed the filter (a `/` is literal).
        handle_key(&mut s, key(KeyCode::Char('q')));
        assert_eq!(s.model_select.filter, "q");
        assert_eq!(s.model_select.filtered().len(), 1);
        assert_eq!(
            s.model_select.selected().unwrap().model.model_id,
            "qwen/qwen3-coder"
        );
    }

    #[test]
    fn esc_clears_filter_then_quits() {
        let mut s = AppState::new(ModelSelect::from_results(&remote_results()));
        for c in "qwen".chars() {
            handle_key(&mut s, key(KeyCode::Char(c)));
        }
        assert!(!s.model_select.filter.is_empty());
        // First Esc clears the filter and stays on the screen.
        assert_eq!(handle_key(&mut s, key(KeyCode::Esc)), Action::None);
        assert!(s.model_select.filter.is_empty());
        // Second Esc quits.
        assert_eq!(handle_key(&mut s, key(KeyCode::Esc)), Action::Quit);
    }

    #[test]
    fn enter_selects_then_verify_prompt_gates_the_probe() {
        use crate::app::state::{caps_consent_key, VerifyCaps};

        // Enter on a model selects it — the verify prompt is raised on the chat
        // screen by `select_model`, not staged here. (Enter on the collapsed
        // provider header expands it first.)
        let mut s = AppState::new(ModelSelect::from_results(&remote_results()));
        assert_eq!(handle_key(&mut s, key(KeyCode::Enter)), Action::None);
        handle_key(&mut s, key(KeyCode::Down));
        assert_eq!(handle_key(&mut s, key(KeyCode::Enter)), Action::SelectModel);

        let entry = s.model_select.selected().unwrap().clone();
        let consent_key = caps_consent_key(&entry.provider_id, &entry.model.model_id);

        // Y on the open prompt starts the probe.
        let mut s = AppState::new(ModelSelect::default());
        s.verify_caps = Some(VerifyCaps {
            entry: entry.clone(),
            probing: false,
        });
        assert_eq!(
            handle_key(&mut s, key(KeyCode::Char('y'))),
            Action::ProbeCaps
        );

        // N declines: the prompt closes and the decline is remembered.
        let mut s2 = AppState::new(ModelSelect::default());
        s2.verify_caps = Some(VerifyCaps {
            entry: entry.clone(),
            probing: false,
        });
        assert_eq!(handle_key(&mut s2, key(KeyCode::Char('n'))), Action::None);
        assert!(s2.verify_caps.is_none());
        assert!(s2.declined_caps.contains(&consent_key));

        // While probing the popup swallows input.
        let mut s3 = AppState::new(ModelSelect::default());
        s3.verify_caps = Some(VerifyCaps {
            entry,
            probing: true,
        });
        assert_eq!(handle_key(&mut s3, key(KeyCode::Char('y'))), Action::None);
    }

    #[test]
    fn permission_number_keys_pick_a_menu_row() {
        let mut s = chat_state();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        s.apply_event(suis_agent::AgentEvent::PermissionRequest {
            action: "run command: cargo test".into(),
            sender: tx,
        });
        // '3' → the third row: grant Project.
        let action = handle_key(&mut s, key(KeyCode::Char('3')));
        assert_eq!(
            action,
            Action::AnswerPermission(PermissionDecision::grant(PermissionScope::Project, false))
        );
    }

    #[test]
    fn permission_arrows_move_the_highlight_and_enter_confirms() {
        let mut s = chat_state();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        s.apply_event(suis_agent::AgentEvent::PermissionRequest {
            action: "run command: cargo test".into(),
            sender: tx,
        });
        // Opens highlighting "Allow once"; Down moves to "Allow for this session".
        handle_key(&mut s, key(KeyCode::Down));
        let action = handle_key(&mut s, key(KeyCode::Enter));
        assert_eq!(
            action,
            Action::AnswerPermission(PermissionDecision::grant(PermissionScope::Session, false))
        );
    }

    #[test]
    fn permission_shift_enter_applies_the_advanced_variant() {
        let mut s = chat_state();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        s.apply_event(suis_agent::AgentEvent::PermissionRequest {
            action: "run command: cargo test".into(),
            sender: tx,
        });
        // Highlight "Allow for this session", then Shift+Enter stores a wildcard.
        handle_key(&mut s, key(KeyCode::Down));
        let action = handle_key(&mut s, KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(
            action,
            Action::AnswerPermission(PermissionDecision::grant(PermissionScope::Session, true))
        );
    }

    #[test]
    fn permission_up_from_top_wraps_to_deny() {
        let mut s = chat_state();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        s.apply_event(suis_agent::AgentEvent::PermissionRequest {
            action: "run command: cargo test".into(),
            sender: tx,
        });
        // Up from the top row wraps to the last row (Deny); Enter confirms it.
        handle_key(&mut s, key(KeyCode::Up));
        let action = handle_key(&mut s, key(KeyCode::Enter));
        assert_eq!(action, Action::AnswerPermission(PermissionDecision::deny()));
    }

    #[test]
    fn shift_tab_cycles_mode_from_each_mode() {
        let mut s = chat_state();
        assert_eq!(s.mode, Mode::Agent);
        // BackTab is how most terminals report Shift+Tab.
        let action = handle_key(&mut s, key(KeyCode::BackTab));
        assert_eq!(action, Action::SetMode(Mode::Chat));

        s.mode = Mode::Chat;
        let action = handle_key(&mut s, key(KeyCode::BackTab));
        assert_eq!(action, Action::SetMode(Mode::Plan));

        s.mode = Mode::Plan;
        // Kitty-protocol terminals may report Tab with the SHIFT modifier.
        let action = handle_key(&mut s, KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
        assert_eq!(action, Action::SetMode(Mode::Agent));
    }

    #[test]
    fn mode_switch_is_rejected_while_busy() {
        let mut s = chat_state();
        s.busy = true;
        let action = handle_key(&mut s, key(KeyCode::BackTab));
        assert_eq!(action, Action::None);
        assert!(s
            .messages
            .last()
            .unwrap()
            .text
            .contains("Cannot switch modes"));
        assert_eq!(s.mode, Mode::Agent, "mode unchanged");
    }

    #[test]
    fn plain_tab_still_completes_commands() {
        let mut s = chat_state();
        s.input = "/he".into();
        let action = handle_key(&mut s, key(KeyCode::Tab));
        assert_eq!(action, Action::None);
        assert_eq!(s.input, "/help", "Tab without Shift must keep completing");
    }

    #[test]
    fn plan_review_enter_approves_esc_rejects() {
        let mut s = chat_state();
        let open_review = |s: &mut AppState| {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            s.apply_event(suis_agent::AgentEvent::PlanProposal {
                draft: suis_agent::PlanDraft {
                    revises: None,
                    title: "Auth".into(),
                    description: String::new(),
                    steps: vec![],
                },
                sender: tx,
            });
        };

        open_review(&mut s);
        assert_eq!(
            handle_key(&mut s, key(KeyCode::Enter)),
            Action::AnswerPlan(PlanDecision::Approve)
        );
        open_review(&mut s);
        assert_eq!(
            handle_key(&mut s, key(KeyCode::Esc)),
            Action::AnswerPlan(PlanDecision::Reject)
        );
        // Typing keys are swallowed while the review is open.
        assert_eq!(handle_key(&mut s, key(KeyCode::Char('x'))), Action::None);
        assert!(s.input.is_empty());
    }

    #[test]
    fn implement_confirm_starts_or_cancels() {
        use crate::app::state::ImplementState;
        let target = ImplementState {
            plan_id: "auth".into(),
            step_index: 0,
            plan_title: "Auth".into(),
            step_title: "tokens".into(),
        };

        let mut s = chat_state();
        s.pending_implement = Some(target.clone());
        assert_eq!(
            handle_key(&mut s, key(KeyCode::Enter)),
            Action::StartImplement
        );

        s.pending_implement = Some(target);
        assert_eq!(handle_key(&mut s, key(KeyCode::Esc)), Action::None);
        assert!(s.pending_implement.is_none(), "Esc cancels the start");
    }

    #[test]
    fn verify_prompt_y_begins_n_defers() {
        let mut s = chat_state();
        s.pending_verify = true;
        assert_eq!(
            handle_key(&mut s, key(KeyCode::Char('y'))),
            Action::BeginVerification
        );
        assert!(!s.pending_verify);

        s.pending_verify = true;
        assert_eq!(handle_key(&mut s, key(KeyCode::Char('n'))), Action::None);
        assert!(!s.pending_verify);
        assert!(s.messages.last().unwrap().text.contains("deferred"));
    }

    #[test]
    fn plan_select_enter_drills_then_selects() {
        use suis_core::{PlanStep, PlanStore, PlanTask};
        let mut store = PlanStore::default();
        store.insert(
            "Auth",
            "",
            vec![PlanStep {
                title: "tokens".into(),
                work_tasks: vec![PlanTask::new("a")],
                verify_tasks: vec![],
            }],
        );

        let mut s = chat_state();
        s.screen = Screen::PlanSelect;
        s.plan_select = crate::screens::plan_select::PlanSelect::from_store(&store);

        // First Enter drills into the plan.
        assert_eq!(handle_key(&mut s, key(KeyCode::Enter)), Action::None);
        // Second Enter ("whole plan", empty transcript) starts immediately.
        let action = handle_key(&mut s, key(KeyCode::Enter));
        assert_eq!(action, Action::StartImplement);
        assert_eq!(s.screen, Screen::Chat);
        let pending = s.pending_implement.as_ref().unwrap();
        assert_eq!(pending.plan_id, "auth");
        assert_eq!(pending.step_index, 0);
    }

    #[test]
    fn plan_select_waits_for_confirmation_when_conversation_exists() {
        use suis_core::{PlanStep, PlanStore, PlanTask};
        let mut store = PlanStore::default();
        store.insert(
            "Auth",
            "",
            vec![PlanStep {
                title: "tokens".into(),
                work_tasks: vec![PlanTask::new("a")],
                verify_tasks: vec![],
            }],
        );

        let mut s = chat_state();
        s.push_user("an existing conversation");
        s.screen = Screen::PlanSelect;
        s.plan_select = crate::screens::plan_select::PlanSelect::from_store(&store);

        handle_key(&mut s, key(KeyCode::Enter)); // drill
        let action = handle_key(&mut s, key(KeyCode::Enter)); // select
        assert_eq!(action, Action::None, "must wait for the confirmation");
        assert!(s.pending_implement.is_some());
    }

    #[test]
    fn plan_select_esc_backs_out_then_leaves() {
        use suis_core::{PlanStep, PlanStore, PlanTask};
        let mut store = PlanStore::default();
        store.insert(
            "Auth",
            "",
            vec![PlanStep {
                title: "tokens".into(),
                work_tasks: vec![PlanTask::new("a")],
                verify_tasks: vec![],
            }],
        );

        let mut s = chat_state();
        s.screen = Screen::PlanSelect;
        s.plan_select = crate::screens::plan_select::PlanSelect::from_store(&store);

        handle_key(&mut s, key(KeyCode::Enter)); // drill in
        handle_key(&mut s, key(KeyCode::Esc)); // back to the plan list
        assert_eq!(s.screen, Screen::PlanSelect);
        handle_key(&mut s, key(KeyCode::Esc)); // leave the screen
        assert_eq!(s.screen, Screen::Chat);
    }

    #[test]
    fn esc_denies_open_permission() {
        let mut s = chat_state();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        s.apply_event(suis_agent::AgentEvent::PermissionRequest {
            action: "run command: ls".into(),
            sender: tx,
        });
        let action = handle_key(&mut s, key(KeyCode::Esc));
        assert_eq!(action, Action::AnswerPermission(PermissionDecision::deny()));
    }
}
