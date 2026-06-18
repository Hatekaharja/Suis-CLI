//! Turning a parsed [`Command`] into an effect the app applies, plus the
//! line-producing helpers (`/help`, `/permissions`, `/plans`) that build
//! aligned, styled transcript output as pure functions of their inputs.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use suis_agent::Mode;
use suis_core::{PlanStore, ProjectProfile};

use super::parser::{Command, COMMANDS};
use crate::theme;

/// What handling a slash command should do to the application.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandEffect {
    /// Switch to the model-selection screen.
    OpenModelSelect,
    /// Clear the conversation transcript.
    ClearHistory,
    /// Toggle the task panel's visibility.
    ToggleTasks,
    /// Switch to the provider enable/disable screen.
    OpenProviders,
    /// Switch to the stored-permissions screen (`/permissions`).
    OpenPermissions,
    /// Switch the session's runtime mode (`/plan`, `/agent`, `/chat`).
    SetMode(Mode),
    /// List stored plans with their progress (`/plans`).
    ShowPlans,
    /// Open the plan-selection screen for `/implement`.
    OpenImplement,
    /// Summarize the conversation and replace history with it (`/compact`).
    Compact,
    /// Show the cached project profile, re-detecting and persisting it first
    /// when `refresh` is set (`/profile`, `/profile refresh`).
    ShowProfile { refresh: bool },
    /// Open the per-provider token-usage popup (`/usage`).
    ToggleUsage,
    /// Toggle the raw tool-call display under each tool card (`/developer`).
    ToggleDeveloper,
    /// Quit the program (`/exit`, `/quit`).
    Quit,
    /// Show a plain system message in the chat (e.g. "unknown command").
    SystemMessage(String),
    /// Show a pre-styled, aligned system message (`/help`).
    StyledMessage(Vec<Line<'static>>),
}

/// Map a [`Command`] to its [`CommandEffect`].
pub fn handle(command: &Command) -> CommandEffect {
    match command {
        Command::Model => CommandEffect::OpenModelSelect,
        Command::Clear => CommandEffect::ClearHistory,
        Command::Tasks => CommandEffect::ToggleTasks,
        Command::Providers => CommandEffect::OpenProviders,
        Command::Permissions => CommandEffect::OpenPermissions,
        Command::Plan => CommandEffect::SetMode(Mode::Plan),
        Command::Agent => CommandEffect::SetMode(Mode::Agent),
        Command::Chat => CommandEffect::SetMode(Mode::Chat),
        Command::Plans => CommandEffect::ShowPlans,
        Command::Implement => CommandEffect::OpenImplement,
        Command::Compact => CommandEffect::Compact,
        Command::Profile { refresh } => CommandEffect::ShowProfile { refresh: *refresh },
        Command::Usage => CommandEffect::ToggleUsage,
        Command::Developer => CommandEffect::ToggleDeveloper,
        Command::Exit => CommandEffect::Quit,
        Command::Help => CommandEffect::StyledMessage(help_lines()),
        Command::Unknown(name) => CommandEffect::SystemMessage(format!("Unknown command: {name}")),
    }
}

/// The name/pattern column stops growing here, so one long entry cannot push
/// the descriptions off-screen.
const NAME_COL_CAP: usize = 32;

/// The width of the left-hand name column: the longest entry, capped.
fn name_col(lens: impl Iterator<Item = usize>) -> usize {
    lens.max().unwrap_or(0).min(NAME_COL_CAP)
}

/// A section header line ("Available commands:").
fn header(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default()
            .fg(theme::TEXT)
            .add_modifier(Modifier::BOLD),
    ))
}

/// One aligned row: an accent `name` padded to `width`, then a dim `detail`.
fn row(name: String, width: usize, detail: String) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{name:<width$}"),
            Style::default().fg(theme::ACCENT),
        ),
        Span::raw("  "),
        Span::styled(detail, Style::default().fg(theme::TEXT_DIM)),
    ])
}

/// A faint trailing pointer line ("Use /implement to …").
fn pointer(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(theme::TEXT_FAINT),
    ))
}

/// The `/help` listing: command names in the accent, descriptions dim, in one
/// aligned column.
pub fn help_lines() -> Vec<Line<'static>> {
    let width = name_col(COMMANDS.iter().map(|(name, _)| name.len() + 1));
    let mut out = vec![header("Available commands:")];
    for (name, description) in COMMANDS {
        out.push(row(format!("/{name}"), width, (*description).to_string()));
    }
    out
}

/// The `/plans` listing: stored plans with `[done/total steps]` progress, or a
/// pointer at Plan mode when none exist yet.
pub fn plans_lines(store: &PlanStore) -> Vec<Line<'static>> {
    if store.plans.is_empty() {
        return vec![Line::from(
            "No plans stored. Switch to Plan mode (/plan) and ask the agent to draft one.",
        )];
    }
    let width = name_col(store.plans.iter().map(|p| p.id.len()));
    let mut out = vec![header("Stored plans:")];
    for plan in &store.plans {
        let (done, total) = plan.progress();
        out.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{:<width$}", plan.id),
                Style::default().fg(theme::TEXT),
            ),
            Span::raw("  "),
            Span::styled(
                format!("[{done}/{total} steps]"),
                Style::default().fg(theme::ACCENT),
            ),
            Span::raw("  "),
            Span::styled(plan.title.clone(), Style::default().fg(theme::TEXT_DIM)),
        ]));
    }
    out.push(Line::from(""));
    out.push(pointer("Use /implement to start working on one."));
    out
}

/// The `/profile` listing: the cached project brief as aligned label rows, or a
/// pointer at `/profile refresh` when none has been detected yet.
pub fn profile_lines(profile: Option<&ProjectProfile>) -> Vec<Line<'static>> {
    let Some(profile) = profile else {
        return vec![
            Line::from("No project profile yet."),
            pointer("Run /profile refresh to detect the toolchain and build/test commands."),
        ];
    };

    // One label column for the brief's field rows; only non-empty fields show.
    let labels = ["Summary", "Toolchain", "Build", "Test"];
    let width = name_col(labels.iter().map(|l| l.len()));
    let fields = [
        ("Summary", profile.summary.as_str()),
        ("Toolchain", profile.toolchain.as_str()),
        ("Build", profile.build_cmd.as_deref().unwrap_or("")),
        ("Test", profile.test_cmd.as_deref().unwrap_or("")),
    ];
    let mut out = vec![header("Project profile:")];
    for (label, value) in fields {
        if !value.is_empty() {
            out.push(row(label.to_string(), width, value.to_string()));
        }
    }

    if !profile.conventions.is_empty() {
        out.push(Line::from(""));
        out.push(header("Conventions:"));
        for convention in &profile.conventions {
            out.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("• {convention}"),
                    Style::default().fg(theme::TEXT_DIM),
                ),
            ]));
        }
    }

    if !profile.generated_at.is_empty() {
        out.push(Line::from(""));
        out.push(pointer(&format!(
            "Detected {}. Run /profile refresh to re-detect.",
            profile.generated_at
        )));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flatten styled lines to their plain text.
    fn text(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn help_lists_every_command() {
        let text = text(&help_lines());
        for (name, _) in COMMANDS {
            assert!(text.contains(&format!("/{name}")), "missing /{name}");
        }
    }

    #[test]
    fn help_aligns_descriptions_in_one_column() {
        let lines = help_lines();
        // Every row's name span is padded to the same width, so descriptions
        // start at one shared column.
        let widths: Vec<usize> = lines[1..]
            .iter()
            .map(|l| l.spans[1].content.chars().count())
            .collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "ragged: {widths:?}"
        );
        // Names are accent, descriptions dim.
        assert_eq!(lines[1].spans[1].style.fg, Some(theme::ACCENT));
        assert_eq!(lines[1].spans[3].style.fg, Some(theme::TEXT_DIM));
    }

    #[test]
    fn unknown_command_reports_name() {
        let effect = handle(&Command::Unknown("frob".into()));
        assert_eq!(
            effect,
            CommandEffect::SystemMessage("Unknown command: frob".into())
        );
    }

    #[test]
    fn help_produces_a_styled_message() {
        assert!(matches!(
            handle(&Command::Help),
            CommandEffect::StyledMessage(_)
        ));
    }

    #[test]
    fn permissions_opens_its_screen() {
        assert_eq!(
            handle(&Command::Permissions),
            CommandEffect::OpenPermissions
        );
    }

    #[test]
    fn model_and_clear_and_tasks_map_to_effects() {
        assert_eq!(handle(&Command::Model), CommandEffect::OpenModelSelect);
        assert_eq!(handle(&Command::Clear), CommandEffect::ClearHistory);
        assert_eq!(handle(&Command::Tasks), CommandEffect::ToggleTasks);
        assert_eq!(handle(&Command::Providers), CommandEffect::OpenProviders);
    }

    #[test]
    fn mode_commands_map_to_set_mode() {
        assert_eq!(handle(&Command::Plan), CommandEffect::SetMode(Mode::Plan));
        assert_eq!(handle(&Command::Agent), CommandEffect::SetMode(Mode::Agent));
        assert_eq!(handle(&Command::Chat), CommandEffect::SetMode(Mode::Chat));
    }

    #[test]
    fn plan_commands_map_to_effects() {
        assert_eq!(handle(&Command::Plans), CommandEffect::ShowPlans);
        assert_eq!(handle(&Command::Implement), CommandEffect::OpenImplement);
        assert_eq!(handle(&Command::Compact), CommandEffect::Compact);
    }

    #[test]
    fn profile_command_carries_refresh_flag() {
        assert_eq!(
            handle(&Command::Profile { refresh: false }),
            CommandEffect::ShowProfile { refresh: false }
        );
        assert_eq!(
            handle(&Command::Profile { refresh: true }),
            CommandEffect::ShowProfile { refresh: true }
        );
    }

    #[test]
    fn profile_lines_point_at_refresh_when_absent() {
        let flat = text(&profile_lines(None));
        assert!(flat.contains("No project profile"));
        assert!(flat.contains("/profile refresh"));
    }

    #[test]
    fn profile_lines_render_the_brief() {
        let profile = ProjectProfile {
            summary: "Rust workspace.".into(),
            toolchain: "Rust (cargo)".into(),
            build_cmd: Some("cargo build".into()),
            test_cmd: Some("cargo test".into()),
            conventions: vec!["Lint with cargo clippy.".into()],
            generated_at: "2026-06-15".into(),
        };
        let flat = text(&profile_lines(Some(&profile)));
        assert!(flat.contains("Toolchain"));
        assert!(flat.contains("Rust (cargo)"));
        assert!(flat.contains("cargo test"));
        assert!(flat.contains("Lint with cargo clippy."));
        assert!(flat.contains("2026-06-15"));
    }

    #[test]
    fn exit_maps_to_quit() {
        assert_eq!(handle(&Command::Exit), CommandEffect::Quit);
    }

    #[test]
    fn developer_maps_to_toggle() {
        assert_eq!(handle(&Command::Developer), CommandEffect::ToggleDeveloper);
    }

    #[test]
    fn plans_lines_list_progress_or_point_at_plan_mode() {
        use suis_core::{PlanStep, PlanTask, TaskStatus};

        assert!(text(&plans_lines(&PlanStore::default())).contains("/plan"));

        let mut store = PlanStore::default();
        let mut step = PlanStep {
            title: "one".into(),
            work_tasks: vec![PlanTask::new("a")],
            verify_tasks: vec![],
        };
        step.work_tasks[0].status = TaskStatus::Done;
        store.insert(
            "Auth System",
            "",
            vec![
                step,
                PlanStep {
                    title: "two".into(),
                    work_tasks: vec![PlanTask::new("b")],
                    verify_tasks: vec![],
                },
            ],
        );
        let lines = plans_lines(&store);
        let flat = text(&lines);
        assert!(flat.contains("auth-system"));
        assert!(flat.contains("[1/2 steps]"));
        assert!(flat.contains("/implement"));
        // The progress badge carries the accent.
        assert_eq!(lines[1].spans[3].style.fg, Some(theme::ACCENT));
    }

    #[test]
    fn name_column_caps_long_entries() {
        assert_eq!(name_col([80usize, 4].into_iter()), NAME_COL_CAP);
    }
}
