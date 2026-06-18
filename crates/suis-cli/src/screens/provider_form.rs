//! The add / edit provider form and its view-model.
//!
//! Opened from `/providers` with `a` (add) or `e` (edit). The Add flow opens on
//! a preset chooser (19.3); choosing a preset — or Custom — drops the user into
//! the same five-control form: name, derived id, endpoint, a transport picker,
//! and the optional API-key env-var name. Validation reuses the loader's own
//! rules (18.3) so the form cannot save what `/providers` would later flag, and
//! `t` tests the draft connection (19.2) without persisting anything.
//!
//! The view-model ([`ProviderForm`]) is pure and unit-tested; [`render`] draws
//! it. Persistence, re-probing, and the async test run live in the app layer.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use suis_core::ProviderEntry;
use suis_providers::{endpoint_problem, ProbeOutcome, ProviderPreset, TransportType, PRESETS};

use crate::theme;
use crate::widgets::{footer, list_frame};

/// Which control the form's focus is on. The text fields and the transport
/// picker are navigated in this order with Tab / Up / Down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    Name,
    Id,
    Endpoint,
    Transport,
    ApiKeyEnv,
}

/// Field navigation order (also the on-screen order).
const FIELD_ORDER: [FormField; 5] = [
    FormField::Name,
    FormField::Id,
    FormField::Endpoint,
    FormField::Transport,
    FormField::ApiKeyEnv,
];

/// Whether the form is creating a new entry or editing an existing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormMode {
    /// Adding a brand-new provider (opens on the preset chooser).
    Add,
    /// Editing the entry with `original_id`, preserving its `enabled` flag.
    Edit { original_id: String, enabled: bool },
}

/// The result of a manual connection test, ready to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestOutcome {
    /// The endpoint answered with a model list.
    Online { count: usize, sample: Vec<String> },
    /// The endpoint rejected the credentials; carries the key env-var name (if
    /// any) so the hint can name what to check.
    AuthFailed { key_env: Option<String> },
    /// The endpoint connected and responded, but not in the chosen protocol.
    WrongLanguage,
    /// The endpoint accepted the connection but timed out or returned a server
    /// error — reachable but not answering correctly.
    ConnectionIssue,
    /// The endpoint could not be reached (the port refused the connection).
    Unreachable,
}

impl TestOutcome {
    /// Map a registry probe outcome into a renderable test result, attaching the
    /// draft's key env-var name to an auth failure so the hint is actionable.
    pub fn from_probe(outcome: ProbeOutcome, key_env: Option<String>) -> Self {
        match outcome {
            ProbeOutcome::Online(result) => {
                let count = result.models.len();
                let sample = result
                    .models
                    .iter()
                    .take(3)
                    .map(|m| m.model_id.clone())
                    .collect();
                TestOutcome::Online { count, sample }
            }
            ProbeOutcome::AuthFailed { .. } => TestOutcome::AuthFailed { key_env },
            ProbeOutcome::Unparsable { .. } => TestOutcome::WrongLanguage,
            ProbeOutcome::Offline { .. } => TestOutcome::Unreachable,
            ProbeOutcome::ConnectionIssue { .. } => TestOutcome::ConnectionIssue,
        }
    }

    /// The one-line status and its colour, shared by the form and the provider
    /// list's in-place test rendering.
    pub fn status_line(&self) -> (String, Color) {
        match self {
            TestOutcome::Online { count, sample } => {
                let names = if sample.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", sample.join(", "))
                };
                (format!("✓ online — {count} models{names}"), theme::ACCENT)
            }
            TestOutcome::AuthFailed { key_env } => {
                let hint = match key_env {
                    Some(env) => format!(" — check ${env}"),
                    None => " — set an API key env".to_string(),
                };
                (format!("✗ auth failed{hint}"), theme::DANGER)
            }
            TestOutcome::WrongLanguage => (
                "✗ connected, but the response did not parse — wrong transport?".to_string(),
                theme::DANGER,
            ),
            TestOutcome::ConnectionIssue => (
                "✗ reachable but not answering — timed out or server error".to_string(),
                theme::DANGER,
            ),
            TestOutcome::Unreachable => ("✗ unreachable".to_string(), theme::DANGER),
        }
    }
}

/// The state of the (optional) connection test.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TestState {
    /// No test run yet.
    #[default]
    Idle,
    /// A probe is in flight.
    Testing,
    /// A probe completed with this outcome.
    Done(TestOutcome),
}

/// The add/edit provider form view-model.
#[derive(Debug, Clone)]
pub struct ProviderForm {
    mode: FormMode,
    /// While true the form shows the preset chooser instead of the fields.
    choosing_preset: bool,
    /// Cursor within the preset chooser (0..=PRESETS.len(), the last being
    /// Custom).
    preset_cursor: usize,

    name: String,
    id: String,
    /// Once the user edits the id directly, it stops tracking the name's slug.
    id_edited: bool,
    endpoint: String,
    transport: TransportType,
    api_key_env: String,

    focus: FormField,
    /// Ids already taken (excluding this entry's own, when editing), for the
    /// uniqueness check.
    existing_ids: Vec<String>,
    /// The last validation failure: the offending field and why.
    error: Option<(FormField, String)>,
    /// The connection-test state.
    pub test: TestState,
}

impl ProviderForm {
    /// A fresh Add form, opening on the preset chooser. `existing_ids` is every
    /// id already configured, for the uniqueness check.
    pub fn new_add(existing_ids: Vec<String>) -> Self {
        ProviderForm {
            mode: FormMode::Add,
            choosing_preset: true,
            preset_cursor: 0,
            name: String::new(),
            id: String::new(),
            id_edited: false,
            endpoint: String::new(),
            transport: TransportType::OpenAiCompatible,
            api_key_env: String::new(),
            focus: FormField::Name,
            existing_ids,
            error: None,
            test: TestState::Idle,
        }
    }

    /// An Edit form prefilled from an existing entry. `existing_ids` must
    /// exclude this entry's own id so re-saving it unchanged is allowed.
    #[allow(clippy::too_many_arguments)]
    pub fn new_edit(
        original_id: String,
        enabled: bool,
        name: String,
        id: String,
        endpoint: String,
        transport: TransportType,
        api_key_env: Option<String>,
        existing_ids: Vec<String>,
    ) -> Self {
        ProviderForm {
            mode: FormMode::Edit {
                original_id,
                enabled,
            },
            choosing_preset: false,
            preset_cursor: 0,
            name,
            // An id distinct from the slug of the name is treated as
            // user-chosen, so editing the name won't silently rewrite it.
            id_edited: true,
            id,
            endpoint,
            transport,
            api_key_env: api_key_env.unwrap_or_default(),
            focus: FormField::Name,
            existing_ids,
            error: None,
            test: TestState::Idle,
        }
    }

    /// Whether the preset chooser is the active step.
    pub fn choosing_preset(&self) -> bool {
        self.choosing_preset
    }

    /// The chooser rows: every preset followed by Custom.
    pub fn preset_labels(&self) -> Vec<String> {
        let mut labels: Vec<String> = PRESETS
            .iter()
            .map(|p| format!("{} ({})", p.name, p.endpoint))
            .collect();
        labels.push("Custom — start from a blank form".to_string());
        labels
    }

    /// The chooser cursor position.
    pub fn preset_cursor(&self) -> usize {
        self.preset_cursor
    }

    /// Move the chooser cursor, wrapping. The range is `PRESETS.len() + 1` to
    /// include the trailing Custom row.
    pub fn preset_move(&mut self, delta: isize) {
        let len = (PRESETS.len() + 1) as isize;
        let cur = self.preset_cursor as isize;
        self.preset_cursor = (cur + delta).rem_euclid(len) as usize;
    }

    /// Commit the highlighted chooser row: prefill from the preset (or leave the
    /// form blank for Custom) and advance to the fields, focused on the name.
    pub fn choose_preset(&mut self) {
        if let Some(preset) = PRESETS.get(self.preset_cursor) {
            self.apply_preset(preset);
        }
        // Custom (the trailing row) leaves every field blank.
        self.choosing_preset = false;
        self.focus = FormField::Name;
        self.test = TestState::Idle;
    }

    /// Prefill the form fields from a preset, deriving the id from the name.
    fn apply_preset(&mut self, preset: &ProviderPreset) {
        self.name = preset.name.to_string();
        self.endpoint = preset.endpoint.to_string();
        self.transport = preset.transport;
        self.api_key_env = preset.key_env.unwrap_or("").to_string();
        self.id_edited = false;
        self.resync_id();
    }

    /// The current focus.
    pub fn focus(&self) -> FormField {
        self.focus
    }

    /// Move focus to the next control, wrapping.
    pub fn focus_next(&mut self) {
        let pos = FIELD_ORDER
            .iter()
            .position(|f| *f == self.focus)
            .unwrap_or(0);
        self.focus = FIELD_ORDER[(pos + 1) % FIELD_ORDER.len()];
    }

    /// Move focus to the previous control, wrapping.
    pub fn focus_prev(&mut self) {
        let pos = FIELD_ORDER
            .iter()
            .position(|f| *f == self.focus)
            .unwrap_or(0);
        self.focus = FIELD_ORDER[(pos + FIELD_ORDER.len() - 1) % FIELD_ORDER.len()];
    }

    /// Type a character into the focused text field. No-op on the transport
    /// picker. Editing the name re-derives the id until the id is hand-edited;
    /// editing the id marks it hand-edited.
    pub fn push_char(&mut self, c: char) {
        match self.focus {
            FormField::Name => {
                self.name.push(c);
                self.resync_id();
            }
            FormField::Id => {
                self.id.push(c);
                self.id_edited = true;
            }
            FormField::Endpoint => self.endpoint.push(c),
            FormField::ApiKeyEnv => self.api_key_env.push(c),
            FormField::Transport => {}
        }
        self.dirty();
    }

    /// Delete the last character of the focused text field.
    pub fn backspace(&mut self) {
        match self.focus {
            FormField::Name => {
                self.name.pop();
                self.resync_id();
            }
            FormField::Id => {
                self.id.pop();
                self.id_edited = true;
            }
            FormField::Endpoint => {
                self.endpoint.pop();
            }
            FormField::ApiKeyEnv => {
                self.api_key_env.pop();
            }
            FormField::Transport => {}
        }
        self.dirty();
    }

    /// Advance the transport picker to the next language, wrapping (only when
    /// the picker is focused). Iterating [`TransportType::ALL`] means a new
    /// transport appears here with no change to the form — the transport layer
    /// is the single source of which languages exist.
    pub fn toggle_transport(&mut self) {
        if self.focus == FormField::Transport {
            let all = TransportType::ALL;
            let pos = all.iter().position(|t| *t == self.transport).unwrap_or(0);
            self.transport = all[(pos + 1) % all.len()];
            self.dirty();
        }
    }

    /// Re-derive the id from the name's slug, unless the id was hand-edited.
    fn resync_id(&mut self) {
        if !self.id_edited {
            self.id = slugify(&self.name);
        }
    }

    /// A draft edit invalidates a prior validation error and stale test result.
    fn dirty(&mut self) {
        self.error = None;
        self.test = TestState::Idle;
    }

    /// The current draft as an entry, regardless of validity, for the test path
    /// (which probes whatever is on screen). The id falls back to a placeholder
    /// so a test before naming still targets the endpoint.
    pub fn draft_entry(&self) -> ProviderEntry {
        ProviderEntry {
            id: if self.id.is_empty() {
                "draft".to_string()
            } else {
                self.id.clone()
            },
            endpoint: self.endpoint.clone(),
            transport: self.transport.as_str().to_string(),
            enabled: true,
            name: (!self.name.is_empty() && self.name != self.id).then(|| self.name.clone()),
            api_key_env: self.key_env_opt(),
            api_key: None,
        }
    }

    /// The key env-var name as an `Option`, empty string meaning "none".
    fn key_env_opt(&self) -> Option<String> {
        let trimmed = self.api_key_env.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }

    /// Validate the draft with the loader's own rules and return the entry to
    /// persist, or record (and return) the offending field + reason. The
    /// `enabled` flag is preserved across an edit and set true for an add.
    pub fn validate(&mut self) -> Result<ProviderEntry, (FormField, String)> {
        let id = self.id.trim().to_string();
        if id.is_empty() {
            return Err(self.fail(FormField::Id, "id is required (derived from the name)"));
        }
        if self.existing_ids.iter().any(|e| e == &id) {
            return Err(self.fail(FormField::Id, format!("id {id:?} is already in use")));
        }
        if let Some(reason) = endpoint_problem(&self.endpoint) {
            return Err(self.fail(FormField::Endpoint, reason));
        }

        let enabled = match &self.mode {
            FormMode::Add => true,
            FormMode::Edit { enabled, .. } => *enabled,
        };
        let name = self.name.trim();
        self.error = None;
        Ok(ProviderEntry {
            id: id.clone(),
            endpoint: self.endpoint.trim().to_string(),
            transport: self.transport.as_str().to_string(),
            enabled,
            name: (!name.is_empty() && name != id).then(|| name.to_string()),
            api_key_env: self.key_env_opt(),
            api_key: None,
        })
    }

    /// Record a validation failure, focusing the offending field, and return it.
    fn fail(&mut self, field: FormField, reason: impl Into<String>) -> (FormField, String) {
        let reason = reason.into();
        self.focus = field;
        self.error = Some((field, reason.clone()));
        (field, reason)
    }

    /// The original id when editing (for replacing the entry in-place), else
    /// `None`.
    pub fn original_id(&self) -> Option<&str> {
        match &self.mode {
            FormMode::Edit { original_id, .. } => Some(original_id),
            FormMode::Add => None,
        }
    }

    /// Mark a test as started.
    pub fn begin_test(&mut self) {
        self.test = TestState::Testing;
    }
}

/// Slugify a display name into an id: lowercase, non-alphanumerics collapse to
/// single dashes, with no leading/trailing dash.
fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(c.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    out
}

/// Render the provider form (or its preset chooser).
pub fn render(frame: &mut Frame, area: Rect, form: &ProviderForm) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // title
            Constraint::Min(1),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    if form.choosing_preset {
        render_chooser(frame, chunks[0], chunks[1], chunks[2], form);
    } else {
        render_fields(frame, chunks[0], chunks[1], chunks[2], form);
    }
}

/// Render the preset chooser step.
fn render_chooser(
    frame: &mut Frame,
    title: Rect,
    body: Rect,
    footer_area: Rect,
    form: &ProviderForm,
) {
    list_frame::render_title(frame, title, "Add a Provider — choose a preset");

    let labels = form.preset_labels();
    let lines: Vec<Line<'static>> = labels
        .iter()
        .enumerate()
        .map(|(idx, label)| {
            let (style, marker) = list_frame::row_style(idx == form.preset_cursor());
            Line::from(vec![
                Span::styled(marker, style),
                Span::styled(label.clone(), style),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(list_frame::block()), body);

    footer::render(
        frame,
        footer_area,
        &[("Enter", "choose"), ("↑/↓", "navigate"), ("Esc", "cancel")],
    );
}

/// Render the field form step.
fn render_fields(
    frame: &mut Frame,
    title: Rect,
    body: Rect,
    footer_area: Rect,
    form: &ProviderForm,
) {
    let heading = if form.original_id().is_some() {
        "Edit Provider"
    } else {
        "Add Provider"
    };
    list_frame::render_title(frame, title, heading);

    let mut lines: Vec<Line<'static>> = vec![
        field_line("Name", &form.name, form.focus() == FormField::Name, false),
        field_line("Id", &form.id, form.focus() == FormField::Id, false),
        field_line(
            "Endpoint",
            &form.endpoint,
            form.focus() == FormField::Endpoint,
            false,
        ),
        transport_line(form.transport(), form.focus() == FormField::Transport),
        field_line(
            "API key env",
            &form.api_key_env,
            form.focus() == FormField::ApiKeyEnv,
            true,
        ),
    ];

    // Validation error, if any.
    if let Some((_, reason)) = form.error() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("⚠ {reason}"),
            Style::default().fg(theme::DANGER),
        )));
    }

    // Connection-test status.
    match &form.test {
        TestState::Idle => {}
        TestState::Testing => {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "testing…",
                Style::default().fg(theme::WARN),
            )));
        }
        TestState::Done(outcome) => {
            let (text, color) = outcome.status_line();
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(text, Style::default().fg(color))));
        }
    }

    frame.render_widget(Paragraph::new(lines).block(list_frame::block()), body);

    footer::render(
        frame,
        footer_area,
        &[
            ("Tab/↑/↓", "field"),
            ("←/→/Space", "transport"),
            ("t", "test"),
            ("Enter", "save"),
            ("Esc", "cancel"),
        ],
    );
}

/// One labelled text field row, with the value shown (key env unset shows a
/// faint placeholder; secrets are never echoed because the form never holds a
/// literal key — only the env-var name, which is not secret).
fn field_line(label: &str, value: &str, focused: bool, optional: bool) -> Line<'static> {
    let (value_style, caret) = if focused {
        (
            Style::default()
                .fg(theme::TEXT_BRIGHT)
                .add_modifier(Modifier::BOLD),
            "_",
        )
    } else {
        (Style::default().fg(theme::TEXT), "")
    };
    let marker = if focused { "> " } else { "  " };
    let shown = if value.is_empty() && !focused {
        let hint = if optional { "(optional)" } else { "(empty)" };
        return Line::from(vec![
            Span::styled(marker, Style::default().fg(theme::TEXT_DIM)),
            Span::styled(
                format!("{label:<12} "),
                Style::default().fg(theme::TEXT_DIM),
            ),
            Span::styled(hint.to_string(), Style::default().fg(theme::TEXT_FAINT)),
        ]);
    } else {
        format!("{value}{caret}")
    };
    Line::from(vec![
        Span::styled(marker, Style::default().fg(theme::TEXT_DIM)),
        Span::styled(
            format!("{label:<12} "),
            Style::default().fg(theme::TEXT_DIM),
        ),
        Span::styled(shown, value_style),
    ])
}

/// The transport picker row, showing both languages with the chosen one marked.
fn transport_line(transport: TransportType, focused: bool) -> Line<'static> {
    let marker = if focused { "> " } else { "  " };
    let mut spans = vec![
        Span::styled(marker, Style::default().fg(theme::TEXT_DIM)),
        Span::styled("Transport    ", Style::default().fg(theme::TEXT_DIM)),
    ];
    for &kind in TransportType::ALL {
        let selected = kind == transport;
        let style = if selected {
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::TEXT_FAINT)
        };
        let glyph = if selected { "(•) " } else { "( ) " };
        spans.push(Span::styled(format!("{glyph}{}  ", kind.as_str()), style));
    }
    Line::from(spans)
}

impl ProviderForm {
    /// The current transport (for rendering).
    pub fn transport(&self) -> TransportType {
        self.transport
    }

    /// The current validation error, if any (for rendering).
    pub fn error(&self) -> Option<&(FormField, String)> {
        self.error.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add_form() -> ProviderForm {
        let mut form = ProviderForm::new_add(vec!["ollama".to_string()]);
        // Skip the chooser by picking Custom (the trailing row).
        form.preset_cursor = PRESETS.len();
        form.choose_preset();
        form
    }

    #[test]
    fn slugify_lowercases_and_dashes() {
        assert_eq!(slugify("Work Proxy"), "work-proxy");
        assert_eq!(slugify("Ollama #2!"), "ollama-2");
        assert_eq!(slugify("  spaced  out  "), "spaced-out");
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn add_opens_on_preset_chooser() {
        let form = ProviderForm::new_add(vec![]);
        assert!(form.choosing_preset());
        // Presets plus the trailing Custom row.
        assert_eq!(form.preset_labels().len(), PRESETS.len() + 1);
    }

    #[test]
    fn choosing_a_preset_prefills_the_documented_fields() {
        let mut form = ProviderForm::new_add(vec![]);
        // OpenRouter is the second preset.
        form.preset_cursor = 1;
        form.choose_preset();
        assert!(!form.choosing_preset());
        assert_eq!(form.name, "OpenRouter");
        assert_eq!(form.endpoint, "https://openrouter.ai/api");
        assert_eq!(form.transport, TransportType::OpenAiCompatible);
        assert_eq!(form.api_key_env, "OPENROUTER_API_KEY");
        // The id is derived from the name.
        assert_eq!(form.id, "openrouter");
        assert_eq!(form.focus(), FormField::Name);
    }

    #[test]
    fn custom_starts_blank() {
        let form = add_form();
        assert!(!form.choosing_preset());
        assert_eq!(form.name, "");
        assert_eq!(form.id, "");
        assert_eq!(form.endpoint, "");
    }

    #[test]
    fn typing_name_derives_id_until_id_is_edited() {
        let mut form = add_form();
        for c in "Work Proxy".chars() {
            form.push_char(c);
        }
        assert_eq!(form.id, "work-proxy");

        // Hand-edit the id: it detaches from the name's slug.
        form.focus = FormField::Id;
        form.backspace(); // "work-prox"
        form.push_char('y'); // "work-proxy"
        assert_eq!(form.id, "work-proxy");
        // Now editing the name no longer rewrites the id.
        form.focus = FormField::Name;
        form.push_char('!');
        assert_eq!(form.id, "work-proxy", "id stays hand-edited");
    }

    #[test]
    fn field_navigation_order_wraps() {
        let mut form = add_form();
        assert_eq!(form.focus(), FormField::Name);
        form.focus_next();
        assert_eq!(form.focus(), FormField::Id);
        form.focus_next();
        assert_eq!(form.focus(), FormField::Endpoint);
        form.focus_next();
        assert_eq!(form.focus(), FormField::Transport);
        form.focus_next();
        assert_eq!(form.focus(), FormField::ApiKeyEnv);
        form.focus_next();
        assert_eq!(form.focus(), FormField::Name, "wraps");
        form.focus_prev();
        assert_eq!(form.focus(), FormField::ApiKeyEnv, "wraps back");
    }

    #[test]
    fn transport_cycles_through_all_languages_only_when_focused() {
        let mut form = add_form();
        assert_eq!(form.transport(), TransportType::OpenAiCompatible);
        form.toggle_transport(); // focus is Name → no-op
        assert_eq!(form.transport(), TransportType::OpenAiCompatible);

        // Focused, the picker cycles through every transport in ALL order and
        // wraps — so the third language (Anthropic) is reachable with no
        // form-specific knowledge of which transports exist.
        form.focus = FormField::Transport;
        let start = form.transport();
        let mut seen = vec![start];
        for _ in 0..TransportType::ALL.len() - 1 {
            form.toggle_transport();
            seen.push(form.transport());
        }
        assert!(seen.contains(&TransportType::Ollama));
        assert!(seen.contains(&TransportType::OpenAiCompatible));
        assert!(seen.contains(&TransportType::Anthropic));
        // One more toggle wraps back to where it started.
        form.toggle_transport();
        assert_eq!(form.transport(), start, "cycling wraps");
    }

    #[test]
    fn transport_picker_lists_all_three_languages() {
        let form = add_form();
        let screen = render_to_string(&form, 80, 18);
        assert!(screen.contains("ollama"));
        assert!(screen.contains("openai"));
        assert!(screen.contains("anthropic"));
    }

    #[test]
    fn validate_blocks_empty_endpoint_and_names_the_field() {
        let mut form = add_form();
        for c in "My Box".chars() {
            form.push_char(c);
        }
        // Endpoint left empty.
        let err = form.validate().unwrap_err();
        assert_eq!(err.0, FormField::Endpoint);
        assert_eq!(
            form.focus(),
            FormField::Endpoint,
            "focus moves to the fault"
        );
        assert!(form.error().is_some());
    }

    #[test]
    fn validate_blocks_bad_url() {
        let mut form = add_form();
        for c in "Box".chars() {
            form.push_char(c);
        }
        form.focus = FormField::Endpoint;
        for c in "not a url".chars() {
            form.push_char(c);
        }
        let err = form.validate().unwrap_err();
        assert_eq!(err.0, FormField::Endpoint);
    }

    #[test]
    fn validate_blocks_duplicate_id() {
        let mut form = ProviderForm::new_add(vec!["ollama".to_string()]);
        form.preset_cursor = PRESETS.len();
        form.choose_preset();
        for c in "Ollama".chars() {
            form.push_char(c); // id slugs to "ollama", which is taken
        }
        form.focus = FormField::Endpoint;
        for c in "http://localhost:11434".chars() {
            form.push_char(c);
        }
        let err = form.validate().unwrap_err();
        assert_eq!(err.0, FormField::Id);
        assert!(err.1.contains("already in use"));
    }

    #[test]
    fn validate_produces_the_expected_entry() {
        let mut form = add_form();
        for c in "Work Proxy".chars() {
            form.push_char(c);
        }
        form.focus = FormField::Endpoint;
        for c in "https://proxy.example/v1".chars() {
            form.push_char(c);
        }
        form.focus = FormField::ApiKeyEnv;
        for c in "WORK_KEY".chars() {
            form.push_char(c);
        }
        let entry = form.validate().expect("valid");
        assert_eq!(entry.id, "work-proxy");
        assert_eq!(entry.endpoint, "https://proxy.example/v1");
        assert_eq!(entry.transport, "openai");
        assert_eq!(entry.name.as_deref(), Some("Work Proxy"));
        assert_eq!(entry.api_key_env.as_deref(), Some("WORK_KEY"));
        // The UI never writes a literal key.
        assert_eq!(entry.api_key, None);
        assert!(entry.enabled);
    }

    #[test]
    fn edit_preserves_enabled_and_allows_resaving_same_id() {
        let mut form = ProviderForm::new_edit(
            "work".into(),
            false, // disabled entry being edited
            "Work".into(),
            "work".into(),
            "https://proxy.example/v1".into(),
            TransportType::OpenAiCompatible,
            Some("WORK_KEY".into()),
            vec!["ollama".to_string()], // own id excluded
        );
        assert!(!form.choosing_preset());
        let entry = form.validate().expect("same id is allowed on edit");
        assert_eq!(entry.id, "work");
        assert!(!entry.enabled, "edit preserves the disabled flag");
        assert_eq!(form.original_id(), Some("work"));
    }

    #[test]
    fn test_outcomes_render_each_class() {
        let online = TestOutcome::Online {
            count: 14,
            sample: vec!["gpt-4o".into(), "gpt-4o-mini".into()],
        };
        let (line, color) = online.status_line();
        assert!(line.contains("14 models"));
        assert!(line.contains("gpt-4o"));
        assert_eq!(color, theme::ACCENT);

        let auth = TestOutcome::AuthFailed {
            key_env: Some("OPENROUTER_API_KEY".into()),
        };
        assert!(auth.status_line().0.contains("$OPENROUTER_API_KEY"));

        assert!(TestOutcome::WrongLanguage
            .status_line()
            .0
            .contains("wrong transport"));
        assert!(TestOutcome::Unreachable
            .status_line()
            .0
            .contains("unreachable"));
    }

    fn render_to_string(form: &ProviderForm, w: u16, h: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), form))
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
    fn chooser_renders_presets_and_custom() {
        let form = ProviderForm::new_add(vec![]);
        let screen = render_to_string(&form, 80, 16);
        assert!(screen.contains("choose a preset"));
        assert!(screen.contains("OpenRouter"));
        assert!(screen.contains("Custom"));
    }

    #[test]
    fn field_form_renders_values_and_test_status() {
        let mut form = add_form();
        for c in "Work".chars() {
            form.push_char(c);
        }
        form.test = TestState::Done(TestOutcome::Online {
            count: 3,
            sample: vec!["a".into()],
        });
        let screen = render_to_string(&form, 80, 18);
        assert!(screen.contains("Add Provider"));
        assert!(screen.contains("Transport"));
        assert!(screen.contains("online — 3 models"));
    }

    #[test]
    fn editing_clears_a_stale_test_result() {
        let mut form = add_form();
        form.test = TestState::Done(TestOutcome::Unreachable);
        form.push_char('x');
        assert_eq!(form.test, TestState::Idle, "an edit invalidates the test");
    }
}
