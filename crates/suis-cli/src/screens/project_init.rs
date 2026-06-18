//! First-run project initialization.
//!
//! When a workspace has no `.suis/` directory, the app opens on this screen
//! instead of going straight to model selection. It walks the user through a
//! short series of yes/no questions and produces a [`ProjectConfig`] (and an
//! empty permission store) for the event loop to persist:
//!
//! 1. **Confirm** — initialize Suis for this project at all?
//! 2. **Import** — if a `.gitignore` is present, import its entries as
//!    hidden/hardened files (lock files become *hardened*, everything else
//!    *hidden*)?
//! 3. **Git access** — allow the agent to use git?
//!
//! The view-model ([`ProjectInit`]) is pure and unit-tested; [`render`] draws
//! it. The event loop turns each answer into an [`InitOutcome`] and, on
//! [`InitOutcome::Complete`], writes the config to disk.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};
use ratatui::Frame;

use crate::theme;
use crate::widgets::footer;
use suis_core::{GitAccess, ProjectConfig};

use crate::app::startup::default_tools;

/// Which question the init flow is currently asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitStep {
    /// "Initialize Suis for this project?"
    Confirm,
    /// Per-entry classification of `.gitignore` entries (hidden/hardened/skip).
    ImportGitignore,
    /// "Allow the agent to use git?"
    GitAccess,
}

/// How a single `.gitignore` entry should be imported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryClass {
    /// Hidden from reads and listings entirely.
    Hidden,
    /// Listable/readable, but writes require approval.
    Hardened,
    /// Not imported at all.
    Skip,
}

impl EntryClass {
    /// Short label shown next to the entry.
    fn badge(self) -> &'static str {
        match self {
            EntryClass::Hidden => "hidden",
            EntryClass::Hardened => "hardened",
            EntryClass::Skip => "skip",
        }
    }

    /// The next class when cycling: Hidden → Hardened → Skip → Hidden.
    fn next(self) -> EntryClass {
        match self {
            EntryClass::Hidden => EntryClass::Hardened,
            EntryClass::Hardened => EntryClass::Skip,
            EntryClass::Skip => EntryClass::Hidden,
        }
    }
}

/// What the event loop should do after an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitOutcome {
    /// More questions remain; keep showing the screen.
    Pending,
    /// The user declined to initialize. Proceed without writing `.suis/`.
    Cancelled,
    /// Initialization finished; persist this config and proceed.
    Complete(ProjectConfig),
}

/// The init-flow view-model: the current step and the answers gathered so far.
#[derive(Debug, Clone)]
pub struct ProjectInit {
    step: InitStep,
    /// Raw `.gitignore` patterns discovered at startup (may be empty).
    gitignore: Vec<String>,
    /// Per-entry import classification, parallel to the `.gitignore` entries.
    /// Defaults from [`classify_gitignore`]; the user adjusts each on the
    /// ImportGitignore step.
    entries: Vec<(String, EntryClass)>,
    /// Cursor into `entries` while on the ImportGitignore step.
    cursor: usize,
    /// Whether the user chose to allow git access.
    allow_git: bool,
}

impl ProjectInit {
    /// Start the flow with the workspace's discovered `.gitignore` patterns.
    pub fn new(gitignore: Vec<String>) -> Self {
        let entries = gitignore
            .iter()
            .map(|pattern| {
                let class = if is_lock_pattern(pattern) {
                    EntryClass::Hardened
                } else {
                    EntryClass::Hidden
                };
                (pattern.clone(), class)
            })
            .collect();
        ProjectInit {
            step: InitStep::Confirm,
            gitignore,
            entries,
            cursor: 0,
            allow_git: false,
        }
    }

    /// Whether the flow is currently on the per-entry import step (so the input
    /// layer knows to use list keys rather than yes/no).
    pub fn on_import_step(&self) -> bool {
        self.step == InitStep::ImportGitignore
    }

    /// Move the import-list cursor by `delta`, clamped to the entry range.
    pub fn move_cursor(&mut self, delta: i32) {
        if self.entries.is_empty() {
            return;
        }
        let last = (self.entries.len() - 1) as i32;
        self.cursor = (self.cursor as i32 + delta).clamp(0, last) as usize;
    }

    /// Cycle the highlighted entry's class: Hidden → Hardened → Skip → Hidden.
    pub fn cycle_current(&mut self) {
        if let Some((_, class)) = self.entries.get_mut(self.cursor) {
            *class = class.next();
        }
    }

    /// Finish the import step, keeping each entry's chosen class, and advance.
    pub fn confirm_import(&mut self) -> InitOutcome {
        self.step = InitStep::GitAccess;
        InitOutcome::Pending
    }

    /// Skip the whole import (mark every entry Skip) and advance.
    pub fn skip_all(&mut self) -> InitOutcome {
        for (_, class) in &mut self.entries {
            *class = EntryClass::Skip;
        }
        self.step = InitStep::GitAccess;
        InitOutcome::Pending
    }

    /// Advance the flow with a "yes".
    pub fn answer_yes(&mut self) -> InitOutcome {
        match self.step {
            InitStep::Confirm => {
                if self.gitignore.is_empty() {
                    self.step = InitStep::GitAccess;
                } else {
                    self.step = InitStep::ImportGitignore;
                }
                InitOutcome::Pending
            }
            // The import step is driven by its own list keys, not yes/no.
            InitStep::ImportGitignore => InitOutcome::Pending,
            InitStep::GitAccess => {
                self.allow_git = true;
                InitOutcome::Complete(self.build_config())
            }
        }
    }

    /// Advance the flow with a "no".
    pub fn answer_no(&mut self) -> InitOutcome {
        match self.step {
            InitStep::Confirm => InitOutcome::Cancelled,
            // The import step is driven by its own list keys, not yes/no.
            InitStep::ImportGitignore => InitOutcome::Pending,
            InitStep::GitAccess => {
                self.allow_git = false;
                InitOutcome::Complete(self.build_config())
            }
        }
    }

    /// Assemble the configured [`ProjectConfig`], partitioning entries by class
    /// and dropping anything marked Skip.
    fn build_config(&self) -> ProjectConfig {
        let mut hidden = Vec::new();
        let mut hardened = Vec::new();
        for (pattern, class) in &self.entries {
            match class {
                EntryClass::Hidden => hidden.push(pattern.clone()),
                EntryClass::Hardened => hardened.push(pattern.clone()),
                EntryClass::Skip => {}
            }
        }
        ProjectConfig {
            allowed_tools: default_tools(),
            git_access: if self.allow_git {
                GitAccess::ReadWrite
            } else {
                GitAccess::Disabled
            },
            hidden,
            hardened,
            ..ProjectConfig::default()
        }
    }
}

/// Whether a gitignore pattern names a dependency lock file. Such files default
/// to *hardened* (the agent should not rewrite them without approval).
fn is_lock_pattern(pattern: &str) -> bool {
    let name = pattern
        .trim_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(pattern);
    name.ends_with(".lock")
        || matches!(
            name,
            "Cargo.lock"
                | "package-lock.json"
                | "yarn.lock"
                | "pnpm-lock.yaml"
                | "poetry.lock"
                | "Gemfile.lock"
                | "composer.lock"
        )
}

/// Render the current question.
pub fn render(frame: &mut Frame, area: Rect, init: &ProjectInit) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let title = Span::styled(
        "Initialize Suis",
        Style::default()
            .fg(theme::INFO)
            .add_modifier(Modifier::BOLD),
    );

    let mut lines = vec![Line::from(title), Line::from("")];
    for line in body_lines(init) {
        lines.push(line);
    }

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER))
                .padding(Padding::horizontal(1)),
        )
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, rows[0]);

    footer::render(frame, rows[1], footer_keys(init));
}

/// Footer key hints for the current step. The import step has its own list
/// controls; the others are simple yes/no.
fn footer_keys(init: &ProjectInit) -> &'static [(&'static str, &'static str)] {
    match init.step {
        InitStep::ImportGitignore => &[
            ("↑/↓", "move"),
            ("Space", "cycle"),
            ("Enter", "import"),
            ("N", "skip all"),
            ("Ctrl+C", "quit"),
        ],
        _ => &[("Y", "yes"), ("N", "no"), ("Ctrl+C", "quit")],
    }
}

/// How many import entries to show at once; the list windows around the cursor.
const IMPORT_WINDOW: usize = 10;

/// The question and any context for the current step.
fn body_lines(init: &ProjectInit) -> Vec<Line<'static>> {
    match init.step {
        InitStep::Confirm => vec![Line::from(
            "No .suis/ found. Initialize Suis for this project?",
        )],
        InitStep::ImportGitignore => {
            let count = init.entries.len();
            let mut lines = vec![
                Line::from(format!(
                    "Found {count} .gitignore entr{} — classify each, then import:",
                    if count == 1 { "y" } else { "ies" },
                )),
                Line::from(""),
            ];
            // Window the list around the cursor so a long list cannot overflow.
            let start = init
                .cursor
                .saturating_sub(IMPORT_WINDOW / 2)
                .min(count.saturating_sub(IMPORT_WINDOW));
            let end = (start + IMPORT_WINDOW).min(count);
            if start > 0 {
                lines.push(Line::from(Span::styled(
                    format!("  … {start} above"),
                    Style::default().fg(theme::TEXT_FAINT),
                )));
            }
            for (i, (pattern, class)) in init.entries.iter().enumerate().take(end).skip(start) {
                let selected = i == init.cursor;
                let marker = if selected { ">" } else { " " };
                let style = if selected {
                    Style::default()
                        .fg(theme::INFO)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::TEXT_DIM)
                };
                lines.push(Line::from(Span::styled(
                    format!("{marker} {pattern}  [{}]", class.badge()),
                    style,
                )));
            }
            if end < count {
                lines.push(Line::from(Span::styled(
                    format!("  … {} below", count - end),
                    Style::default().fg(theme::TEXT_FAINT),
                )));
            }
            lines
        }
        InitStep::GitAccess => vec![Line::from(
            "Allow the agent to use git (status, diff, and commits)?",
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declining_confirm_cancels() {
        let mut init = ProjectInit::new(vec![]);
        assert_eq!(init.answer_no(), InitOutcome::Cancelled);
    }

    #[test]
    fn no_gitignore_skips_import_step() {
        let mut init = ProjectInit::new(vec![]);
        // Confirm yes → straight to git access (no import question).
        assert_eq!(init.answer_yes(), InitOutcome::Pending);
        // Next yes completes (git allowed).
        match init.answer_yes() {
            InitOutcome::Complete(config) => {
                assert_eq!(config.git_access, GitAccess::ReadWrite);
                assert!(config.hidden.is_empty());
                assert!(!config.allowed_tools.is_empty());
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn import_defaults_classify_lock_files_as_hardened() {
        let entries = vec![".env".to_string(), "Cargo.lock".to_string()];
        let mut init = ProjectInit::new(entries);
        assert_eq!(init.answer_yes(), InitOutcome::Pending); // confirm → import
        assert!(init.on_import_step());
        // Accept the defaults: .env hidden, Cargo.lock hardened.
        assert_eq!(init.confirm_import(), InitOutcome::Pending); // import → git access
        match init.answer_no() {
            InitOutcome::Complete(config) => {
                assert_eq!(config.hidden, vec![".env".to_string()]);
                assert_eq!(config.hardened, vec!["Cargo.lock".to_string()]);
                assert_eq!(config.git_access, GitAccess::Disabled);
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn skip_all_leaves_no_hidden_or_hardened() {
        let entries = vec![".env".to_string(), "Cargo.lock".to_string()];
        let mut init = ProjectInit::new(entries);
        assert_eq!(init.answer_yes(), InitOutcome::Pending); // confirm → import
        assert_eq!(init.skip_all(), InitOutcome::Pending); // skip import → git access
        match init.answer_yes() {
            InitOutcome::Complete(config) => {
                assert!(config.hidden.is_empty());
                assert!(config.hardened.is_empty());
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn cycling_an_entry_changes_its_class() {
        // `.env` defaults to Hidden. Cycle it: Hidden → Hardened → Skip.
        let mut init = ProjectInit::new(vec![".env".to_string(), "build/".to_string()]);
        init.answer_yes(); // confirm → import
        init.cycle_current(); // .env → hardened
        init.confirm_import();
        match init.answer_no() {
            InitOutcome::Complete(config) => {
                // .env is now hardened; build/ keeps its hidden default.
                assert_eq!(config.hardened, vec![".env".to_string()]);
                assert_eq!(config.hidden, vec!["build/".to_string()]);
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn skipped_entry_is_excluded_from_import() {
        let mut init = ProjectInit::new(vec![".env".to_string()]);
        init.answer_yes(); // confirm → import
        init.cycle_current(); // hidden → hardened
        init.cycle_current(); // hardened → skip
        init.confirm_import();
        match init.answer_no() {
            InitOutcome::Complete(config) => {
                assert!(config.hidden.is_empty());
                assert!(config.hardened.is_empty());
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn cursor_stays_in_range() {
        let mut init = ProjectInit::new(vec!["a".to_string(), "b".to_string()]);
        init.answer_yes(); // confirm → import
        init.move_cursor(-1); // clamped at 0
        init.move_cursor(5); // clamped at last (1)
        init.cycle_current(); // affects "b"
        init.confirm_import();
        match init.answer_no() {
            InitOutcome::Complete(config) => {
                // Only "b" was cycled hidden → hardened; "a" stays hidden.
                assert_eq!(config.hidden, vec!["a".to_string()]);
                assert_eq!(config.hardened, vec!["b".to_string()]);
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn lock_pattern_detection() {
        assert!(is_lock_pattern("Cargo.lock"));
        assert!(is_lock_pattern("/package-lock.json"));
        assert!(is_lock_pattern("subdir/yarn.lock"));
        assert!(is_lock_pattern("custom.lock"));
        assert!(!is_lock_pattern(".env"));
        assert!(!is_lock_pattern("target/"));
        assert!(!is_lock_pattern("node_modules"));
    }
}
