//! The stored-permissions screen and its view-model.
//!
//! Opened by `/permissions`, this replaces the old in-chat text dump with a
//! navigable screen modeled on [`providers`](crate::screens::providers). It
//! lists every persisted grant grouped by where it lives — `Global`
//! (`~/.config/suis/permissions.json`) and `Project` (`.suis/permissions.json`)
//! — with denies rendered in the danger colour so a deny never reads like an
//! allow. The user can revoke the highlighted entry (`d`) or add a project-level
//! command deny (`n`); both persist via
//! [`PermissionStore::save_split`](suis_core::PermissionStore::save_split).
//!
//! The view-model ([`PermissionsView`]) is pure and unit-tested; [`render`]
//! draws it.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use suis_core::{CommandPermission, PermissionScope, PermissionStore};

use crate::theme;
use crate::widgets::{footer, list_frame};

/// Which file a stored permission lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermSource {
    /// `~/.config/suis/permissions.json` — applies across all projects.
    Global,
    /// `.suis/permissions.json` — applies to this project only.
    Project,
}

impl PermSource {
    /// The section header shown above this source's rows.
    fn header(self) -> &'static str {
        match self {
            PermSource::Global => "Global",
            PermSource::Project => "Project",
        }
    }
}

/// One row in the permissions list: a stored grant or deny, tagged with its
/// source so it can be rendered in its section and written back to the right
/// file.
#[derive(Debug, Clone)]
pub struct PermissionRow {
    /// Which file this entry lives in.
    pub source: PermSource,
    /// `"command"` or `"tool"` — the kind column.
    pub kind: &'static str,
    /// The command pattern or tool name.
    pub name: String,
    /// The granted (or denied) scope.
    pub scope: PermissionScope,
}

impl PermissionRow {
    /// Whether this is an explicit deny (rendered in the danger colour).
    pub fn is_deny(&self) -> bool {
        self.scope == PermissionScope::Deny
    }
}

/// The permissions view-model: the two source stores, the flattened display
/// rows, the cursor, and the optional in-progress "new deny" text input.
#[derive(Debug, Clone, Default)]
pub struct PermissionsView {
    global: PermissionStore,
    project: PermissionStore,
    rows: Vec<PermissionRow>,
    cursor: usize,
    /// The buffer for the inline "new deny" input, present while it is open.
    add: Option<String>,
}

impl PermissionsView {
    /// Build from the global and project stores loaded by
    /// [`PermissionStore::load_split`](suis_core::PermissionStore::load_split).
    pub fn from_split(global: PermissionStore, project: PermissionStore) -> Self {
        let mut view = PermissionsView {
            global,
            project,
            rows: Vec::new(),
            cursor: 0,
            add: None,
        };
        view.rebuild();
        view
    }

    /// Rebuild the flat row list from the two stores: Global section first
    /// (commands then tools), then Project, so order is stable across edits.
    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        for (source, store) in [
            (PermSource::Global, &self.global),
            (PermSource::Project, &self.project),
        ] {
            for c in &store.commands {
                rows.push(PermissionRow {
                    source,
                    kind: "command",
                    name: c.pattern.clone(),
                    scope: c.scope,
                });
            }
            for t in &store.tools {
                rows.push(PermissionRow {
                    source,
                    kind: "tool",
                    name: t.tool.clone(),
                    scope: t.scope,
                });
            }
        }
        self.rows = rows;
        if self.cursor >= self.rows.len() {
            self.cursor = self.rows.len().saturating_sub(1);
        }
    }

    /// The rows, in display order.
    pub fn rows(&self) -> &[PermissionRow] {
        &self.rows
    }

    /// The highlighted row, if any.
    pub fn selected(&self) -> Option<&PermissionRow> {
        self.rows.get(self.cursor)
    }

    /// The highlighted row index (for the revoke-confirm overlay's label).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether there is nothing stored at all.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The two stores, for persistence after an edit.
    pub fn stores(&self) -> (&PermissionStore, &PermissionStore) {
        (&self.global, &self.project)
    }

    /// Move the cursor down one, wrapping at the end.
    pub fn move_down(&mut self) {
        if !self.rows.is_empty() {
            self.cursor = (self.cursor + 1) % self.rows.len();
        }
    }

    /// Move the cursor up one, wrapping at the start.
    pub fn move_up(&mut self) {
        let len = self.rows.len();
        if len > 0 {
            self.cursor = (self.cursor + len - 1) % len;
        }
    }

    /// Remove the highlighted entry from its source store and rebuild. Returns
    /// whether anything was removed.
    pub fn revoke_selected(&mut self) -> bool {
        let Some(row) = self.rows.get(self.cursor).cloned() else {
            return false;
        };
        let store = match row.source {
            PermSource::Global => &mut self.global,
            PermSource::Project => &mut self.project,
        };
        let removed = if row.kind == "command" {
            store
                .commands
                .iter()
                .position(|c| c.pattern == row.name && c.scope == row.scope)
                .map(|i| store.commands.remove(i))
                .is_some()
        } else {
            store
                .tools
                .iter()
                .position(|t| t.tool == row.name && t.scope == row.scope)
                .map(|i| store.tools.remove(i))
                .is_some()
        };
        if removed {
            self.rebuild();
        }
        removed
    }

    /// Add a project-level command deny for `pattern` (trimmed). A no-op for an
    /// empty or already-denied pattern. Returns whether a deny was added.
    pub fn add_deny(&mut self, pattern: impl Into<String>) -> bool {
        let pattern = pattern.into().trim().to_string();
        if pattern.is_empty() {
            return false;
        }
        let exists = self
            .project
            .commands
            .iter()
            .any(|c| c.pattern == pattern && c.scope == PermissionScope::Deny);
        if exists {
            return false;
        }
        self.project.commands.push(CommandPermission {
            pattern,
            scope: PermissionScope::Deny,
        });
        self.rebuild();
        true
    }

    // --- the inline "new deny" text input ---

    /// Open the "new deny" input.
    pub fn begin_add(&mut self) {
        self.add = Some(String::new());
    }

    /// The current input buffer, if the input is open.
    pub fn add_input(&self) -> Option<&str> {
        self.add.as_deref()
    }

    /// Append a typed character to the input.
    pub fn push_add_char(&mut self, c: char) {
        if let Some(buf) = &mut self.add {
            buf.push(c);
        }
    }

    /// Delete the last character of the input.
    pub fn pop_add_char(&mut self) {
        if let Some(buf) = &mut self.add {
            buf.pop();
        }
    }

    /// Close the input, discarding its buffer.
    pub fn cancel_add(&mut self) {
        self.add = None;
    }

    /// Close the input and return its buffer (for committing a deny).
    pub fn take_add(&mut self) -> Option<String> {
        self.add.take()
    }
}

/// The faint scope word shown at the end of a row.
fn scope_label(scope: PermissionScope) -> &'static str {
    match scope {
        PermissionScope::Once => "once",
        PermissionScope::Session => "session",
        PermissionScope::Project => "project",
        PermissionScope::Always => "always",
        PermissionScope::Deny => "deny",
    }
}

/// Render the permissions screen: a title with a count, the grouped list, and a
/// context-sensitive footer.
pub fn render(frame: &mut Frame, area: Rect, state: &PermissionsView) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // title
            Constraint::Min(1),    // list
            Constraint::Length(1), // footer
        ])
        .split(area);

    let n = state.rows().len();
    let title = format!("Permissions ({n} stored)");
    list_frame::render_title(frame, chunks[0], &title);

    let list = Paragraph::new(build_lines(state)).block(list_frame::block());
    frame.render_widget(list, chunks[1]);

    let hints: &[(&str, &str)] = if state.add_input().is_some() {
        &[("Enter", "add deny"), ("Esc", "cancel")]
    } else {
        &[
            ("↑/↓", "move"),
            ("d", "revoke"),
            ("n", "new deny"),
            ("Esc", "back"),
        ]
    };
    footer::render(frame, chunks[2], hints);
}

/// Build the grouped, highlighted list lines for the current view.
fn build_lines(state: &PermissionsView) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    if state.is_empty() {
        lines.push(list_frame::empty_line(
            "no stored permissions — grants are created when you approve an agent action",
        ));
    } else {
        // Widest name, so the scope column lines up; capped so one long pattern
        // can't push it off-screen.
        let width = state
            .rows()
            .iter()
            .map(|r| r.name.len())
            .max()
            .unwrap_or(0)
            .min(32);

        let mut current: Option<PermSource> = None;
        for (idx, row) in state.rows().iter().enumerate() {
            // A section header each time the source changes.
            if current != Some(row.source) {
                if current.is_some() {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(Span::styled(
                    row.source.header().to_string(),
                    Style::default().fg(theme::TEXT_DIM),
                )));
                current = Some(row.source);
            }

            let (name_style, marker) = list_frame::row_style(idx == state.cursor);
            let scope_color = if row.is_deny() {
                theme::DANGER
            } else {
                theme::ACCENT
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {marker}"), name_style),
                Span::styled(
                    format!("{:<8}", row.kind),
                    Style::default().fg(theme::TEXT_FAINT),
                ),
                Span::styled(format!("{:<width$}  ", row.name), name_style),
                Span::styled(
                    scope_label(row.scope).to_string(),
                    Style::default().fg(scope_color),
                ),
            ]));
        }
    }

    // The inline "new deny" input, when open.
    if let Some(buf) = state.add_input() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("new deny: ", Style::default().fg(theme::DANGER)),
            Span::styled(format!("{buf}_"), Style::default().fg(theme::TEXT_BRIGHT)),
        ]));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use suis_core::ToolPermission;

    fn cmd(pattern: &str, scope: PermissionScope) -> CommandPermission {
        CommandPermission {
            pattern: pattern.into(),
            scope,
        }
    }

    fn fixture() -> PermissionsView {
        let global = PermissionStore {
            commands: vec![cmd("git status", PermissionScope::Always)],
            tools: Vec::new(),
            session_denies: Vec::new(),
        };
        let project = PermissionStore {
            commands: vec![
                cmd("cargo *", PermissionScope::Project),
                cmd("git push", PermissionScope::Deny),
            ],
            tools: vec![ToolPermission {
                tool: "edit".into(),
                scope: PermissionScope::Project,
            }],
            session_denies: Vec::new(),
        };
        PermissionsView::from_split(global, project)
    }

    #[test]
    fn rows_are_grouped_global_then_project() {
        let view = fixture();
        let rows = view.rows();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].source, PermSource::Global);
        assert_eq!(rows[0].name, "git status");
        assert_eq!(rows[1].source, PermSource::Project);
        assert_eq!(rows[1].name, "cargo *");
        // Tools come after commands within a source.
        assert_eq!(rows[3].kind, "tool");
        assert_eq!(rows[3].name, "edit");
    }

    #[test]
    fn navigation_wraps() {
        let mut view = fixture();
        view.move_up(); // wrap to last
        assert_eq!(view.selected().unwrap().name, "edit");
        view.move_down(); // wrap back to first
        assert_eq!(view.selected().unwrap().name, "git status");
    }

    #[test]
    fn revoke_removes_the_selected_entry_from_its_source() {
        let mut view = fixture();
        view.move_down(); // cargo * (project command)
        assert_eq!(view.selected().unwrap().name, "cargo *");
        assert!(view.revoke_selected());
        let (_, project) = view.stores();
        assert!(!project.commands.iter().any(|c| c.pattern == "cargo *"));
        assert_eq!(view.rows().len(), 3);
    }

    #[test]
    fn revoke_targets_the_global_file_for_global_rows() {
        let mut view = fixture();
        // Row 0 is the global `git status` grant.
        assert!(view.revoke_selected());
        let (global, _) = view.stores();
        assert!(global.commands.is_empty());
    }

    #[test]
    fn add_deny_appends_a_project_deny_and_dedups() {
        let mut view = fixture();
        assert!(view.add_deny("rm -rf"));
        let (_, project) = view.stores();
        assert!(project
            .commands
            .iter()
            .any(|c| c.pattern == "rm -rf" && c.scope == PermissionScope::Deny));
        // Adding the same deny again is a no-op.
        assert!(!view.add_deny("rm -rf"));
        // Empty / whitespace is rejected.
        assert!(!view.add_deny("   "));
    }

    #[test]
    fn add_input_lifecycle() {
        let mut view = fixture();
        assert!(view.add_input().is_none());
        view.begin_add();
        view.push_add_char('r');
        view.push_add_char('m');
        assert_eq!(view.add_input(), Some("rm"));
        view.pop_add_char();
        assert_eq!(view.add_input(), Some("r"));
        assert_eq!(view.take_add().as_deref(), Some("r"));
        assert!(view.add_input().is_none());
    }

    #[test]
    fn empty_view_is_empty() {
        let mut view = PermissionsView::default();
        assert!(view.is_empty());
        assert!(view.selected().is_none());
        assert!(!view.revoke_selected());
    }
}
