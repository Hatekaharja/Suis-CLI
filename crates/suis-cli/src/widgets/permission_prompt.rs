//! The permission dialog.
//!
//! Built from an [`AgentEvent::PermissionRequest`](suis_agent::AgentEvent)'s
//! `action` string. The dialog presents the grant choices as an explicit,
//! numbered menu *inside* the popup — one option per row — so the user reads
//! the choices where the decision is made rather than decoding a key strip in
//! the footer.
//!
//! For an ordinary action the full set is offered (Allow once, this session,
//! this project, always, and Deny). For a *dangerous* command only "Allow
//! once" and "Deny" are offered — matching the executor, which never honors a
//! stored grant for a dangerous command.
//!
//! The menu is driven either by the number keys (`1`..) or by moving the
//! highlight with ↑/↓ and pressing Enter; Esc denies this invocation. Holding
//! Shift while confirming a row applies its advanced variant: a stored-grant
//! row becomes a *wildcard* allow (e.g. `cargo *`), and the Deny row becomes a
//! persistent *deny for project*. The current row's Shift action is spelled
//! out under the menu so it is discoverable rather than hidden. The option
//! list and its key mapping are pure and unit-tested; [`render`] draws the box
//! with the highlighted row.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};
use ratatui::Frame;

use crate::theme;
use suis_agent::PermissionDecision;
use suis_core::permissions::is_dangerous;
use suis_core::PermissionScope;

use crate::prompts::{command_prompt, file_prompt};

/// Prefix the executor uses for shell-command permission requests.
const COMMAND_PREFIX: &str = "run command: ";

/// One selectable row in the permission menu: a label, the decision it
/// returns, and (optionally) the advanced variant applied when the row is
/// confirmed with Shift held.
#[derive(Debug, Clone, Copy)]
pub struct PermissionOption {
    /// The human-readable choice shown in the menu.
    pub label: &'static str,
    /// The decision returned when this row is chosen.
    pub decision: PermissionDecision,
    /// What Shift+confirm does on this row, if anything.
    pub shift: Option<ShiftAction>,
}

/// The advanced variant a row offers when confirmed with Shift held: a short
/// label for the hint line and the decision it returns.
#[derive(Debug, Clone, Copy)]
pub struct ShiftAction {
    /// Short description shown in the "shift + confirm — …" hint line.
    pub hint: &'static str,
    /// The decision returned when the row is confirmed with Shift.
    pub decision: PermissionDecision,
}

/// An open permission prompt awaiting a choice.
#[derive(Debug, Clone)]
pub struct PermissionPrompt {
    /// Human-readable description of the action (as emitted by the agent).
    pub action: String,
    /// Whether this is a dangerous command (restricts the offered options).
    pub dangerous: bool,
    /// The highlighted menu row, moved with ↑/↓ and confirmed with Enter.
    pub selected: usize,
}

impl PermissionPrompt {
    /// Build a prompt from an agent action description, inferring danger from
    /// the embedded command (if any). The highlight opens on the first row.
    pub fn new(action: impl Into<String>) -> Self {
        let action = action.into();
        let dangerous = action
            .strip_prefix(COMMAND_PREFIX)
            .map(is_dangerous)
            .unwrap_or(false);
        PermissionPrompt {
            action,
            dangerous,
            selected: 0,
        }
    }

    /// The menu rows offered for this prompt, in display order. A dangerous
    /// command offers only a one-shot grant (and Deny), matching the executor.
    pub fn options(&self) -> Vec<PermissionOption> {
        let mut opts = vec![PermissionOption {
            // "Allow once" does not persist, so there is no wildcard variant.
            label: "Allow once",
            decision: PermissionDecision::once(),
            shift: None,
        }];
        if !self.dangerous {
            // Stored-grant rows: Shift+confirm stores a wildcard pattern instead
            // of the exact command.
            for (label, scope) in [
                ("Allow for this session", PermissionScope::Session),
                ("Allow for this project", PermissionScope::Project),
                ("Always allow", PermissionScope::Always),
            ] {
                opts.push(PermissionOption {
                    label,
                    decision: PermissionDecision::grant(scope, false),
                    shift: Some(ShiftAction {
                        hint: "wildcard allow",
                        decision: PermissionDecision::grant(scope, true),
                    }),
                });
            }
        }
        opts.push(PermissionOption {
            // Deny once by default; Shift+confirm denies persistently for the
            // project.
            label: "Deny",
            decision: PermissionDecision::deny(),
            shift: Some(ShiftAction {
                hint: "deny for project",
                decision: PermissionDecision::DenyProject,
            }),
        });
        opts
    }

    /// Move the highlight by `delta`, wrapping at both ends.
    pub fn move_selection(&mut self, delta: isize) {
        let len = self.options().len() as isize;
        if len == 0 {
            return;
        }
        let cur = (self.selected as isize).min(len - 1);
        self.selected = (cur + delta).rem_euclid(len) as usize;
    }

    /// The highlighted row, clamped to the offered options.
    fn selected_option(&self) -> PermissionOption {
        let opts = self.options();
        opts[self.selected.min(opts.len() - 1)]
    }

    /// The decision for the currently highlighted row. `shift` confirms the
    /// row's advanced variant (wildcard allow / deny for project) when it has
    /// one, and is otherwise ignored.
    pub fn selected_decision(&self, shift: bool) -> PermissionDecision {
        let opt = self.selected_option();
        match (shift, opt.shift) {
            (true, Some(s)) => s.decision,
            _ => opt.decision,
        }
    }

    /// The hint describing the highlighted row's Shift+confirm action, if it
    /// has one — shown under the menu so the advanced variant is discoverable.
    pub fn selected_shift_hint(&self) -> Option<&'static str> {
        self.selected_option().shift.map(|s| s.hint)
    }

    /// Map a 1-based menu number (`1` is the first row) to its plain decision,
    /// or `None` if no such row exists for this prompt. (The Shift variants are
    /// reached by confirming the highlighted row, not by the number keys.)
    pub fn decide(&self, choice: usize) -> Option<PermissionDecision> {
        choice
            .checked_sub(1)
            .and_then(|i| self.options().get(i).map(|o| o.decision))
    }
}

/// The footer key set for an open permission prompt: how to drive the menu.
/// The choices themselves now live in the popup, so the footer only teaches
/// navigation.
pub fn hints(dangerous: bool) -> &'static [(&'static str, &'static str)] {
    if dangerous {
        &[
            ("↑/↓", "select"),
            ("Enter", "confirm"),
            ("1/2", "pick"),
            ("Esc", "deny"),
        ]
    } else {
        &[
            ("↑/↓", "select"),
            ("Enter", "confirm"),
            ("Shift+Enter", "advanced"),
            ("1-5", "pick"),
            ("Esc", "deny"),
        ]
    }
}

/// Render the permission prompt as a centered, bordered box over `area`, with
/// the offered options listed as a numbered menu and the highlight on the
/// selected row.
pub fn render(frame: &mut Frame, area: Rect, prompt: &PermissionPrompt) {
    let popup = centered_rect(70, 50, area);
    frame.render_widget(Clear, popup);

    // Describe the action by kind: a shell command, a hardened-file write, or
    // (the fallback) the raw action text.
    let (kind_label, detail) = if let Some(command) = command_prompt::command_of(&prompt.action) {
        ("Agent wants to run:", command.to_string())
    } else if let Some(path) = file_prompt::hardened_path(&prompt.action) {
        ("Agent wants to modify a protected file:", path.to_string())
    } else {
        ("Agent wants to:", prompt.action.clone())
    };

    let mut lines = vec![
        Line::from(Span::styled(
            "Permission Required",
            Style::default()
                .fg(theme::WARN)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            kind_label,
            Style::default().fg(theme::TEXT_DIM),
        )),
        Line::from(Span::styled(
            format!("  {detail}"),
            Style::default()
                .fg(theme::TEXT_BRIGHT)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    if prompt.dangerous {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Dangerous command — only a one-shot grant is offered.",
            Style::default().fg(theme::DANGER),
        )));
    }

    // The choices, one per row: a highlighted selection marker, the numeric
    // shortcut, then the label. The selected row reads bright and bold; the
    // others stay dim so the highlight is unmistakable.
    lines.push(Line::from(""));
    let options = prompt.options();
    let selected = prompt.selected.min(options.len().saturating_sub(1));
    for (i, option) in options.iter().enumerate() {
        let is_selected = i == selected;
        let is_deny = option.label == "Deny";
        let marker = if is_selected { "› " } else { "  " };
        let base = if is_selected {
            theme::TEXT_BRIGHT
        } else if is_deny {
            theme::DANGER
        } else {
            theme::TEXT_DIM
        };
        let mut label_style = Style::default().fg(base);
        if is_selected {
            label_style = label_style.add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(vec![
            Span::styled(marker, Style::default().fg(theme::ACCENT)),
            Span::styled(
                format!("[{}] ", i + 1),
                Style::default()
                    .fg(if is_selected {
                        theme::ACCENT
                    } else {
                        theme::TEXT_FAINT
                    })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(option.label.to_string(), label_style),
        ]));
    }

    // The Shift+confirm action for the highlighted row, spelled out so the
    // advanced variant (wildcard allow / deny for project) is discoverable.
    if let Some(hint) = prompt.selected_shift_hint() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                "shift + confirm",
                Style::default()
                    .fg(theme::TEXT_DIM)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" — {hint}"), Style::default().fg(theme::TEXT_FAINT)),
        ]));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::WARN))
        .style(Style::default().bg(theme::BG))
        .padding(Padding::horizontal(1));
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

/// A rectangle `percent_x` × `percent_y` of `area`, centered.
pub(crate) fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let w = area.width * percent_x / 100;
    let h = area.height * percent_y / 100;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w.max(1),
        height: h.max(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_command_offers_full_menu() {
        let p = PermissionPrompt::new("run command: cargo test");
        assert!(!p.dangerous);
        let labels: Vec<&str> = p.options().iter().map(|o| o.label).collect();
        assert_eq!(
            labels,
            vec![
                "Allow once",
                "Allow for this session",
                "Allow for this project",
                "Always allow",
                "Deny",
            ]
        );
        // The number keys map 1-based onto the rows.
        assert_eq!(p.decide(1), Some(PermissionDecision::once()));
        assert_eq!(
            p.decide(3),
            Some(PermissionDecision::grant(PermissionScope::Project, false))
        );
        assert_eq!(p.decide(5), Some(PermissionDecision::deny()));
        // Out-of-range numbers are not choices.
        assert_eq!(p.decide(0), None);
        assert_eq!(p.decide(6), None);
    }

    #[test]
    fn dangerous_command_restricts_to_once_and_deny() {
        let p = PermissionPrompt::new("run command: rm -rf build");
        assert!(p.dangerous);
        let labels: Vec<&str> = p.options().iter().map(|o| o.label).collect();
        assert_eq!(labels, vec!["Allow once", "Deny"]);
        assert_eq!(p.decide(1), Some(PermissionDecision::once()));
        assert_eq!(p.decide(2), Some(PermissionDecision::deny()));
        // The stored-grant rows are not offered.
        assert_eq!(p.decide(3), None);
    }

    #[test]
    fn selection_wraps_and_confirms_the_highlighted_row() {
        let mut p = PermissionPrompt::new("run command: cargo test");
        // Opens on the first row (Allow once).
        assert_eq!(p.selected, 0);
        assert_eq!(p.selected_decision(false), PermissionDecision::once());

        p.move_selection(1);
        assert_eq!(p.selected, 1);
        assert_eq!(
            p.selected_decision(false),
            PermissionDecision::grant(PermissionScope::Session, false)
        );

        // Up from the top wraps to the last row (Deny).
        p.selected = 0;
        p.move_selection(-1);
        assert_eq!(p.selected, 4);
        assert_eq!(p.selected_decision(false), PermissionDecision::deny());

        // Down from the last row wraps back to the top.
        p.move_selection(1);
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn shift_confirm_applies_the_advanced_variant() {
        let mut p = PermissionPrompt::new("run command: cargo test");

        // Allow once has no advanced variant — Shift is ignored and there is no
        // hint to show.
        assert_eq!(p.selected, 0);
        assert_eq!(p.selected_decision(true), PermissionDecision::once());
        assert_eq!(p.selected_shift_hint(), None);

        // A stored-grant row: Shift+confirm stores a wildcard.
        p.selected = 1; // Allow for this session
        assert_eq!(p.selected_shift_hint(), Some("wildcard allow"));
        assert_eq!(
            p.selected_decision(true),
            PermissionDecision::grant(PermissionScope::Session, true)
        );

        // The Deny row: Shift+confirm denies for the project.
        p.selected = 4;
        assert_eq!(p.selected_shift_hint(), Some("deny for project"));
        assert_eq!(p.selected_decision(true), PermissionDecision::DenyProject);
        // Plain confirm still denies once.
        assert_eq!(p.selected_decision(false), PermissionDecision::deny());
    }

    #[test]
    fn dangerous_deny_still_offers_deny_for_project() {
        let mut p = PermissionPrompt::new("run command: rm -rf build");
        // Row 0 Allow once (no variant), row 1 Deny (deny for project).
        assert_eq!(p.selected_shift_hint(), None);
        p.selected = 1;
        assert_eq!(p.selected_shift_hint(), Some("deny for project"));
        assert_eq!(p.selected_decision(true), PermissionDecision::DenyProject);
    }

    #[test]
    fn file_prompt_is_not_dangerous() {
        let p = PermissionPrompt::new("modify hardened file: Cargo.lock");
        assert!(!p.dangerous);
        assert_eq!(
            p.decide(4),
            Some(PermissionDecision::grant(PermissionScope::Always, false))
        );
    }

    #[test]
    fn footer_hints_teach_navigation_not_each_choice() {
        let full = hints(false);
        assert!(full.iter().any(|(k, _)| *k == "↑/↓"));
        assert!(full.iter().any(|(k, _)| *k == "1-5"));
        let danger = hints(true);
        assert!(danger.iter().any(|(k, _)| *k == "1/2"));
    }
}
