//! The main chat screen.
//!
//! Composes the transcript, the input box, and (when the panel is toggled on)
//! the task panel, then overlays the permission prompt when the agent is
//! awaiting a decision.
//!
//! The task panel is responsive: in a wide terminal it docks to the right
//! quarter of the screen; in a narrow one it floats as a centered popup over
//! the chat, appearing only once there are tasks to show. The `/tasks`
//! command toggles it on and off.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::Clear;
use ratatui::Frame;

use crate::app::state::AppState;
use crate::widgets::permission_prompt::centered_rect;
use crate::widgets::{
    command_palette, confirm_box, footer, input_box, message_list, notice_popup, permission_prompt,
    plan_review, task_panel, usage_popup, welcome,
};

/// Minimum terminal width (cols) at which the task panel docks to the right
/// quarter; below this it is shown as a centered popup instead.
const DOCK_MIN_WIDTH: u16 = 100;

/// The footer key set for the current chat state, in the same precedence
/// order the input layer answers keys: the active overlay's hints win over
/// the screen's, then the palette, then the busy/idle sets.
fn footer_hints(state: &AppState) -> &'static [(&'static str, &'static str)] {
    if let Some(pending) = &state.pending_permission {
        return permission_prompt::hints(pending.prompt.dangerous);
    }
    if state.pending_plan.is_some() {
        return &[
            ("Enter", "approve"),
            ("Esc", "reject"),
            ("↑/↓", "scroll"),
            ("←/→", "pan"),
        ];
    }
    if state.pending_implement.is_some() {
        return &[("Enter", "start"), ("Esc", "cancel")];
    }
    if state.pending_verify {
        return &[("Y", "begin"), ("N", "not yet")];
    }
    if state.pending_next_step.is_some() {
        return &[("Y", "continue"), ("N", "stay")];
    }
    if state.show_usage {
        return &[("Esc", "close")];
    }
    if state.palette_open() {
        return &[
            ("↑/↓", "select"),
            ("Tab", "fill"),
            ("Enter", "run"),
            ("Esc", "dismiss"),
        ];
    }
    if state.busy {
        return &[("Esc", "interrupt")];
    }
    // Developer mode surfaces the Ctrl+Y copy affordance; the default set omits
    // it so the footer stays clean for ordinary use.
    if state.developer {
        return &[
            ("Enter", "send"),
            ("Alt+Enter", "newline"),
            ("↑", "history"),
            ("PgUp/PgDn", "scroll"),
            ("Shift+Tab", "mode"),
            ("Ctrl+Y", "copy"),
            ("/", "commands"),
        ];
    }
    &[
        ("Enter", "send"),
        ("Alt+Enter", "newline"),
        ("↑", "history"),
        ("PgUp/PgDn", "scroll"),
        ("Shift+Tab", "mode"),
        ("/", "commands"),
    ]
}

/// Render the chat screen for the current `state`.
pub fn render(frame: &mut Frame, full: Rect, state: &mut AppState) {
    // The bottom row of the whole screen is the key-hint footer; everything
    // else renders into the remaining area.
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(full);
    let (area, footer_area) = (outer[0], outer[1]);

    // Dock the panel to the right quarter only when both visible and the
    // terminal is wide enough; otherwise the body uses the full width.
    let docked = state.show_tasks && area.width >= DOCK_MIN_WIDTH;
    let (body, dock_area) = if docked {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(75), Constraint::Percentage(25)])
            .split(area);
        (cols[0], Some(cols[1]))
    } else {
        (area, None)
    };

    // The body stacks the transcript over the input box, which gets its
    // measured height: it grows with multi-line/wrapped input up to its clamp,
    // and the transcript shrinks to match.
    let input_height = input_box::required_height(&state.input, body.width);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(input_height)])
        .split(body);

    // Measure the wrapped transcript against its visible area, then resolve the
    // scroll offset for this frame. The width is shrunk by the border (2) and
    // the transcript block's horizontal padding (2); the height only by the
    // border. Recording the measurements lets keyboard scrolling stay correctly
    // bounded and lets the view stick to the bottom as new content streams in.
    let inner_width = rows[0].width.saturating_sub(4);
    let inner_height = rows[0].height.saturating_sub(2);
    // Record the transcript's inner content rectangle (inside the 1-col border
    // and 1-col horizontal padding) so a mouse click can be mapped to the card
    // it landed on. Matches `message_list::render`'s `block.inner`.
    state.transcript_area = Some(Rect {
        x: rows[0].x.saturating_add(2),
        y: rows[0].y.saturating_add(1),
        width: inner_width,
        height: inner_height,
    });
    // The busy hint (spinner · tool · elapsed) renders as the transcript's
    // trailing line — where the work is happening — not in the input box.
    let hint = state.busy_hint();
    if state.messages.is_empty() {
        // Empty transcript: the welcome banner stands in for the message list.
        state.note_transcript_metrics(0, inner_height);
        let identity = welcome::Identity {
            workspace: state.workspace_root.as_deref(),
            model: state
                .selected_model
                .as_ref()
                .map(|m| m.display_name.as_str()),
            provider: state.provider_name.as_deref(),
            mode: state.mode.label(),
        };
        welcome::render(frame, rows[0], &identity);
    } else {
        let content_height =
            message_list::content_height(&state.messages, inner_width, hint.as_deref());
        state.note_transcript_metrics(content_height, inner_height);
        message_list::render(
            frame,
            rows[0],
            &state.messages,
            state.effective_scroll(),
            state.is_detached(),
            state.is_streaming(),
            hint.as_deref(),
        );
    }
    input_box::render(
        frame,
        rows[1],
        &state.input,
        state.input_cursor,
        hint.is_some(),
        state.mode,
        state.implement.as_ref().map(|i| i.plan_title.as_str()),
        state.context,
        state.context_tokens(),
    );

    // The live command palette, anchored above the input box. Modal overlays
    // suppress it (palette_open checks them), so it never fights a prompt.
    if state.palette_open() {
        command_palette::render(
            frame,
            rows[0],
            &state.palette_matches(),
            state.palette_selected,
        );
    }

    // The current implementation step (when one is running) heads the task
    // panel, so the step lives with its tasks rather than in the input border.
    let step = state.implement.as_ref().map(|i| i.step_title.as_str());
    if let Some(panel_area) = dock_area {
        // Wide terminal: docked sidebar.
        task_panel::render(frame, panel_area, &state.tasks, step);
        state.tasks_visible = true;
    } else if state.show_tasks_popup() {
        // Narrow terminal: centered popup over the chat. Unlike the docked
        // sidebar it covers the conversation, so it stays hidden until there
        // are tasks to show or the user opens it explicitly with `/tasks`.
        let popup = centered_rect(60, 60, area);
        frame.render_widget(Clear, popup);
        task_panel::render(frame, popup, &state.tasks, step);
        state.tasks_visible = true;
    } else {
        state.tasks_visible = false;
    }

    // Modal overlays, in the same precedence order the input layer answers
    // them: the plan review, the implement confirmation, the verify prompt —
    // and the permission prompt on top, since it blocks everything.
    if let Some(pending) = &state.pending_plan {
        let (scroll_y, scroll_x) = plan_review::render(
            frame,
            area,
            &pending.draft,
            pending.scroll_y,
            pending.scroll_x,
        );
        // Store the clamped offsets back so input never advances past the end.
        if let Some(pending) = &mut state.pending_plan {
            pending.scroll_y = scroll_y;
            pending.scroll_x = scroll_x;
        }
    } else if state.pending_implement.is_some() {
        confirm_box::render(
            frame,
            area,
            "Start implementation session?",
            "The current conversation will be cleared.",
        );
    } else if state.pending_verify {
        confirm_box::render(
            frame,
            area,
            "All work tasks complete.",
            "Begin verification?",
        );
    } else if let Some(ref next) = state.pending_next_step {
        confirm_box::render(
            frame,
            area,
            "Continue to next Step?",
            &format!("{} — Y continues, N stays", next.step_title),
        );
    }
    if let Some(pending) = &state.pending_permission {
        permission_prompt::render(frame, area, &pending.prompt);
    }

    // The `/usage` detail popup, over everything but the blocking prompts above.
    if state.show_usage {
        usage_popup::render(frame, area, &state.session_usage);
    }

    // A transient advisory (e.g. a plaintext-key warning) sits on top of
    // everything: it is shown once at session start and dismissed with any key.
    if let Some(message) = &state.notice {
        notice_popup::render(frame, area, message);
    }

    // Exactly one footer: the key hints, plus the active provider's running
    // session token total anchored at the right edge of the input column.
    footer::render_with_usage(
        frame,
        footer_area,
        footer_hints(state),
        body.width,
        state.active_session_total(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::Screen;
    use crate::screens::model_select::ModelSelect;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Render a fresh chat screen (no tasks) at `w`×`h` and return the buffer
    /// as a single string.
    fn render_chat(w: u16, h: u16) -> String {
        let mut state = AppState::new(ModelSelect::default());
        state.screen = Screen::Chat;
        assert!(state.tasks.is_empty());

        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &mut state))
            .unwrap();

        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn notice_popup_renders_over_the_chat() {
        let mut state = AppState::new(ModelSelect::default());
        state.screen = Screen::Chat;
        state.notice = Some("key sent over plaintext HTTP".into());

        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &mut state))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        assert!(screen.contains("Warning"), "no warning title: {screen}");
        assert!(screen.contains("plaintext"), "message not shown: {screen}");
        assert!(
            screen.contains("press any key to dismiss"),
            "no dismiss hint"
        );
    }

    #[test]
    fn panel_shows_with_no_tasks_when_wide() {
        // Open by default, docked on a wide (>=100 col) terminal.
        let screen = render_chat(120, 24);
        assert!(screen.contains("Tasks"), "no Tasks panel: {screen}");
        assert!(screen.contains("(no tasks)"), "no empty-state line");
    }

    #[test]
    fn empty_panel_stays_hidden_when_narrow() {
        // Below the dock width the panel is a popup over the chat, so it does
        // not open until there are tasks to show.
        let screen = render_chat(80, 24);
        assert!(
            !screen.contains("(no tasks)"),
            "empty popup must stay hidden"
        );
    }

    #[test]
    fn panel_pops_up_when_narrow_with_tasks() {
        let mut state = AppState::new(ModelSelect::default());
        state.screen = Screen::Chat;
        state.tasks = vec![suis_agent::Task {
            id: "t1".into(),
            title: "do it".into(),
            status: suis_agent::TaskStatus::Todo,
        }];
        let screen = render_sized(&mut state, 80, 24);
        assert!(screen.contains("Tasks"), "no Tasks popup: {screen}");
        assert!(screen.contains("do it"), "task row missing");
    }

    #[test]
    fn narrow_popup_opens_on_explicit_toggle_even_with_no_tasks() {
        // The empty popup stays hidden at startup (it would cover the chat),
        // but an explicit /tasks request opens it. One toggle, one open.
        let mut state = AppState::new(ModelSelect::default());
        state.screen = Screen::Chat;
        assert!(state.tasks.is_empty());

        let hidden = render_sized(&mut state, 80, 24);
        assert!(
            !hidden.contains("(no tasks)"),
            "empty popup hidden by default"
        );
        assert!(!state.tasks_visible, "renderer recorded it as not visible");

        state.toggle_tasks();
        let shown = render_sized(&mut state, 80, 24);
        assert!(
            shown.contains("(no tasks)"),
            "explicit toggle opens the popup"
        );

        // The next toggle hides it again.
        state.toggle_tasks();
        let hidden = render_sized(&mut state, 80, 24);
        assert!(!hidden.contains("(no tasks)"), "second toggle hides it");
    }

    #[test]
    fn input_box_shows_the_active_mode_label() {
        let mut state = AppState::new(ModelSelect::default());
        state.screen = Screen::Chat;
        for (mode, label) in [
            (suis_agent::Mode::Agent, "AGENT"),
            (suis_agent::Mode::Plan, "PLAN"),
            (suis_agent::Mode::Chat, "CHAT"),
        ] {
            state.mode = mode;
            let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
            terminal
                .draw(|frame| render(frame, frame.area(), &mut state))
                .unwrap();
            let screen: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(screen.contains(label), "missing {label} label");
        }
    }

    fn render_state(state: &mut AppState) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), state))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn context_gauge_shows_in_the_input_border() {
        let mut state = AppState::new(ModelSelect::default());
        state.screen = Screen::Chat;
        state.context = Some(crate::widgets::context_gauge::ContextGauge::new(
            7_440, 12_000, false,
        ));
        let screen = render_state(&mut state);
        assert!(screen.contains("ctx 62%"), "missing context gauge");
        // The token figure is the live context size (the gauge's `used`), not a
        // cumulative chat total — so it appears straight from the gauge.
        assert!(screen.contains("7.4k"), "missing live context token figure");
    }

    #[test]
    fn implement_target_shows_in_the_input_border() {
        let mut state = AppState::new(ModelSelect::default());
        state.screen = Screen::Chat;
        state.implement = Some(crate::app::state::ImplementState {
            plan_id: "auth".into(),
            step_index: 0,
            plan_title: "Auth".into(),
            step_title: "tokens".into(),
        });
        let screen = render_state(&mut state);
        // The input border now shows only the project (plan) title; the step
        // moved into the task panel header.
        assert!(screen.contains("Auth"), "missing project label");
    }

    #[test]
    fn implement_and_verify_overlays_render() {
        let mut state = AppState::new(ModelSelect::default());
        state.screen = Screen::Chat;
        state.pending_implement = Some(crate::app::state::ImplementState {
            plan_id: "auth".into(),
            step_index: 0,
            plan_title: "Auth".into(),
            step_title: "tokens".into(),
        });
        let screen = render_state(&mut state);
        assert!(screen.contains("Start implementation session?"));

        state.pending_implement = None;
        state.pending_verify = true;
        let screen = render_state(&mut state);
        assert!(screen.contains("Begin verification?"));
    }

    #[test]
    fn plan_review_overlay_renders_the_draft() {
        let mut state = AppState::new(ModelSelect::default());
        state.screen = Screen::Chat;
        let (tx, _rx) = tokio::sync::oneshot::channel();
        state.pending_plan = Some(crate::app::state::PendingPlan {
            draft: suis_agent::PlanDraft {
                revises: None,
                title: "Auth System".into(),
                description: "Add JWT auth".into(),
                steps: vec![suis_core::PlanStep {
                    title: "Tokens".into(),
                    work_tasks: vec![suis_core::PlanTask::new("login route")],
                    verify_tasks: vec![suis_core::PlanTask::new("auth tests")],
                }],
            },
            sender: tx,
            scroll_y: 0,
            scroll_x: 0,
        });
        let screen = render_state(&mut state);
        assert!(screen.contains("Plan Proposal"));
        assert!(screen.contains("Auth System"));
        assert!(screen.contains("login route"));
        assert!(screen.contains("verify: auth tests"));
        // The keys live in the footer, not the overlay body.
        assert!(screen.contains("Enter approve"));
        assert!(screen.contains("Esc reject"));
    }

    /// A chat state with a selected model/provider/workspace, as after
    /// model selection.
    fn connected_state() -> AppState {
        let mut state = AppState::new(ModelSelect::default());
        state.screen = Screen::Chat;
        state.selected_model = Some(suis_providers::Model::new(
            "ollama",
            "qwen3-coder",
            suis_providers::Capabilities::default(),
        ));
        state.provider_name = Some("Ollama".into());
        state.workspace_root = Some("/home/me/project".into());
        state
    }

    fn render_sized(state: &mut AppState, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), state))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    /// A glyph row that only the full LOGO.txt mark contains.
    const LOGO_FRAGMENT: &str = "▄▄▄█████";

    #[test]
    fn empty_wide_chat_shows_logo_and_identity() {
        let mut state = connected_state();
        let screen = render_sized(&mut state, 120, 30);
        assert!(screen.contains(LOGO_FRAGMENT), "logo missing: {screen}");
        assert!(screen.contains("qwen3-coder @ Ollama"));
        assert!(screen.contains("/home/me/project"));
        assert!(screen.contains("/help for commands"));
    }

    #[test]
    fn empty_narrow_chat_falls_back_to_the_wordmark() {
        let mut state = connected_state();
        let screen = render_sized(&mut state, 60, 20);
        assert!(!screen.contains(LOGO_FRAGMENT), "logo must not overflow");
        assert!(screen.contains("suis"), "wordmark missing: {screen}");
    }

    #[test]
    fn a_message_replaces_the_banner_and_clear_restores_it() {
        let mut state = connected_state();
        state.push_user("hello");
        let screen = render_sized(&mut state, 120, 30);
        assert!(
            !screen.contains("/help for commands"),
            "banner should be gone"
        );

        state.clear_transcript();
        let screen = render_sized(&mut state, 120, 30);
        assert!(
            screen.contains("/help for commands"),
            "banner should return"
        );
    }

    #[test]
    fn idle_footer_teaches_the_idle_key_set() {
        let mut state = connected_state();
        let screen = render_sized(&mut state, 120, 30);
        for hint in [
            "Enter send",
            "Alt+Enter newline",
            "↑ history",
            "PgUp/PgDn scroll",
            "Shift+Tab mode",
            "/ commands",
        ] {
            assert!(screen.contains(hint), "idle footer missing {hint:?}");
        }
    }

    #[test]
    fn developer_footer_offers_the_copy_hint() {
        let mut state = connected_state();
        // Off by default: no copy hint clutters the footer.
        let plain = render_sized(&mut state, 120, 30);
        assert!(!plain.contains("Ctrl+Y copy"), "copy hint leaked when off");
        // On: the footer teaches the Ctrl+Y copy affordance.
        state.developer = true;
        let dev = render_sized(&mut state, 120, 30);
        assert!(dev.contains("Ctrl+Y copy"), "developer footer missing copy");
    }

    #[test]
    fn busy_footer_shows_only_the_interrupt() {
        let mut state = connected_state();
        state.begin_busy();
        let screen = render_sized(&mut state, 120, 30);
        assert!(screen.contains("Esc interrupt"));
        assert!(
            !screen.contains("Enter send"),
            "idle set must yield to busy"
        );
    }

    #[test]
    fn permission_choices_render_as_a_menu_in_the_prompt_body() {
        let mut state = connected_state();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        state.pending_permission = Some(crate::app::state::PendingPermission {
            prompt: crate::widgets::permission_prompt::PermissionPrompt::new(
                "run command: cargo test",
            ),
            sender: tx,
        });
        let screen = render_sized(&mut state, 120, 30);
        assert!(screen.contains("Permission Required"));
        // The choices are a numbered menu inside the popup now.
        for option in [
            "[1] Allow once",
            "[2] Allow for this session",
            "[3] Allow for this project",
            "[4] Always allow",
            "[5] Deny",
        ] {
            assert!(screen.contains(option), "menu missing {option:?}");
        }
        // The footer teaches navigation, not each individual choice.
        for hint in ["select", "confirm", "Esc deny"] {
            assert!(screen.contains(hint), "footer missing {hint:?}");
        }
    }

    #[test]
    fn narrow_footer_truncates_whole_pairs() {
        let mut state = connected_state();
        state.show_tasks = false;
        let screen = render_sized(&mut state, 50, 20);
        assert!(screen.contains("Enter send"));
        assert!(screen.contains("↑ history"));
        assert!(
            !screen.contains("PgUp/PgDn scroll"),
            "pairs past the width are dropped, not wrapped"
        );
    }

    #[test]
    fn panel_hidden_when_toggled_off() {
        let mut state = AppState::new(ModelSelect::default());
        state.screen = Screen::Chat;
        state.show_tasks = false;

        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &mut state))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        assert!(!screen.contains("(no tasks)"), "panel should be hidden");
    }
}
