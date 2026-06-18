//! The provider enable/disable screen and its view-model.
//!
//! Lists every known provider (live discovery merged with stored config) with a
//! checkbox reflecting its `enabled` flag. Toggling persists to `providers.json`
//! and is honored on the next launch's discovery. The view-model
//! ([`ProvidersView`]) is pure and unit-tested; [`render`] draws it.

use std::collections::HashSet;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use suis_core::{ProviderConfig, ProviderEntry};
use suis_providers::{ProbeOutcome, Provider, ProviderIssue, ProviderStatus, TransportType};

use crate::screens::provider_form::TestState;
use crate::theme;
use crate::widgets::{footer, list_frame};

/// One row in the providers list.
#[derive(Debug, Clone)]
pub struct ProviderRow {
    /// Stable provider id (e.g. `"ollama"`).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Base endpoint URL.
    pub endpoint: String,
    /// Transport kind, preserved so persistence round-trips correctly.
    pub transport: TransportType,
    /// Whether the provider is enabled (the toggle this screen edits).
    pub enabled: bool,
    /// Whether the provider responded during this run's discovery.
    pub online: bool,
    /// A re-probe (Enter) is in flight for this row; shown as "checking…" and
    /// cleared when the fresh outcome lands.
    pub checking: bool,
    /// The provider was reachable but did not answer correctly — it timed out or
    /// returned a server error. Distinct from plain offline so it reads red.
    pub connection_issue: bool,
    /// The provider responded but rejected the credentials (401/403).
    pub auth_failed: bool,
    /// A key env-var was configured but resolved to nothing (unset/empty).
    pub key_env_unresolved: bool,
    /// Configured key env-var name, preserved so a toggle round-trips it.
    api_key_env: Option<String>,
    /// Literal key fallback, preserved so a toggle round-trips it. A resolved
    /// env key is never persisted here (see [`ProvidersView::to_config`]).
    api_key: Option<String>,
}

impl ProviderRow {
    /// The configured key env-var name, if any.
    pub fn api_key_env(&self) -> Option<&str> {
        self.api_key_env.as_deref()
    }

    /// This row as a persistable entry, preserving its auth fields without
    /// resolving — used for the in-list connection test and as the edit source.
    pub fn to_entry(&self) -> ProviderEntry {
        let (api_key_env, api_key) = match &self.api_key_env {
            Some(env) => (Some(env.clone()), None),
            None => (None, self.api_key.clone()),
        };
        ProviderEntry {
            id: self.id.clone(),
            endpoint: self.endpoint.clone(),
            transport: self.transport.as_str().to_string(),
            enabled: self.enabled,
            name: (self.name != self.id).then(|| self.name.clone()),
            api_key_env,
            api_key,
        }
    }
}

/// A configuration problem from loading `providers.json`, rendered dimmed so
/// the user sees exactly what is wrong and where, without it blocking the rest.
#[derive(Debug, Clone)]
pub struct IssueRow {
    pub id: String,
    pub reason: String,
}

/// The providers view-model: the full row list plus cursor state.
#[derive(Debug, Clone, Default)]
pub struct ProvidersView {
    rows: Vec<ProviderRow>,
    issues: Vec<IssueRow>,
    cursor: usize,
    /// The state of an in-list connection test (`t`), keyed to the row it was
    /// started on so a result is only shown while that row stays selected.
    pub test: TestState,
    /// The id the in-list test was started on, so the result clears when the
    /// cursor moves elsewhere.
    test_target: Option<String>,
}

impl ProvidersView {
    /// Build from a merged provider list and the set of ids that responded
    /// during discovery, with no load issues, auth failures, or connection
    /// issues.
    #[cfg(test)]
    pub fn from_providers(providers: &[Provider], online: &HashSet<String>) -> Self {
        Self::from_parts(providers, online, &HashSet::new(), &HashSet::new(), &[])
    }

    /// Build from a merged provider list (live discovery ∪ stored config), the
    /// ids that responded, the ids that rejected credentials, the ids that were
    /// reachable but errored (timeout / 5xx), and the configuration issues found
    /// while loading.
    pub fn from_parts(
        providers: &[Provider],
        online: &HashSet<String>,
        auth_failed: &HashSet<String>,
        connection_issue: &HashSet<String>,
        issues: &[ProviderIssue],
    ) -> Self {
        let rows = providers
            .iter()
            .map(|p| ProviderRow {
                id: p.id.clone(),
                name: if p.name.is_empty() {
                    p.id.clone()
                } else {
                    p.name.clone()
                },
                endpoint: p.endpoint.clone(),
                transport: p.transport,
                enabled: p.enabled,
                online: online.contains(&p.id),
                checking: false,
                connection_issue: connection_issue.contains(&p.id),
                auth_failed: auth_failed.contains(&p.id),
                key_env_unresolved: p.key_env_unresolved(),
                api_key_env: p.api_key_env.clone(),
                api_key: p.api_key.clone(),
            })
            .collect();
        let issues = issues
            .iter()
            .map(|i| IssueRow {
                id: i.id.clone(),
                reason: i.reason.clone(),
            })
            .collect();
        ProvidersView {
            rows,
            issues,
            cursor: 0,
            ..Default::default()
        }
    }

    /// The highlighted row, if any.
    pub fn selected(&self) -> Option<&ProviderRow> {
        self.rows.get(self.cursor)
    }

    /// The highlighted row as a persistable entry, for the connection test and
    /// the edit source.
    pub fn selected_entry(&self) -> Option<ProviderEntry> {
        self.selected().map(ProviderRow::to_entry)
    }

    /// Every configured id, for the form's uniqueness check.
    pub fn ids(&self) -> Vec<String> {
        self.rows.iter().map(|r| r.id.clone()).collect()
    }

    /// Mark a connection test as started on the highlighted row.
    pub fn begin_test(&mut self) {
        if let Some(row) = self.rows.get(self.cursor) {
            self.test_target = Some(row.id.clone());
            self.test = TestState::Testing;
        }
    }

    /// Record a finished connection test (shown only while its row stays
    /// selected).
    pub fn set_test(&mut self, state: TestState) {
        self.test = state;
    }

    /// The test state to display, suppressed when the cursor has moved off the
    /// row the test was started on.
    pub fn visible_test(&self) -> &TestState {
        match (&self.test_target, self.selected()) {
            (Some(target), Some(row)) if target == &row.id => &self.test,
            _ => &TestState::Idle,
        }
    }

    /// The load issues, in display order.
    pub fn issues(&self) -> &[IssueRow] {
        &self.issues
    }

    /// The rows, in display order.
    pub fn rows(&self) -> &[ProviderRow] {
        &self.rows
    }

    /// Whether there is nothing at all to show (no providers and no issues).
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.issues.is_empty()
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

    /// Mark the highlighted row as re-probing (Enter): clears its stale status so
    /// the screen reads "checking…" until [`apply_outcome`](Self::apply_outcome)
    /// resolves it. Returns the id being probed, if any.
    pub fn begin_reprobe(&mut self) -> Option<String> {
        let row = self.rows.get_mut(self.cursor)?;
        row.checking = true;
        Some(row.id.clone())
    }

    /// Fold a finished single-provider re-probe into its row, in place — cursor
    /// and order are preserved — so a provider that just came online flips its
    /// status without restarting Suis.
    pub fn apply_outcome(&mut self, outcome: &ProbeOutcome) {
        let status = outcome.status();
        if let Some(row) = self.rows.iter_mut().find(|r| r.id == outcome.id()) {
            row.checking = false;
            row.online = status == ProviderStatus::Online;
            row.connection_issue = status == ProviderStatus::ConnectionIssue;
            row.auth_failed = status == ProviderStatus::AuthFailed;
        }
    }

    /// Flip the highlighted provider's enabled flag.
    pub fn toggle_selected(&mut self) {
        if let Some(row) = self.rows.get_mut(self.cursor) {
            row.enabled = !row.enabled;
        }
    }

    /// The persistable config reflecting the current toggles. The display name
    /// is only emitted when it differs from the id, and auth fields round-trip
    /// without resolving — an env-backed provider re-emits `api_key_env`, never
    /// the resolved secret (a resolved env key is dropped here).
    pub fn to_config(&self) -> ProviderConfig {
        ProviderConfig {
            providers: self
                .rows
                .iter()
                .map(|r| {
                    let (api_key_env, api_key) = match &r.api_key_env {
                        Some(env) => (Some(env.clone()), None),
                        None => (None, r.api_key.clone()),
                    };
                    ProviderEntry {
                        id: r.id.clone(),
                        endpoint: r.endpoint.clone(),
                        transport: r.transport.as_str().to_string(),
                        enabled: r.enabled,
                        name: (r.name != r.id).then(|| r.name.clone()),
                        api_key_env,
                        api_key,
                    }
                })
                .collect(),
        }
    }
}

/// Render the providers screen. During first-run `onboarding` the screen is
/// shown before the model picker, so the title and footer frame it as a setup
/// step ("continue") rather than the mid-session `/providers` view ("back").
pub fn render(frame: &mut Frame, area: Rect, state: &ProvidersView, onboarding: bool) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // title
            Constraint::Min(1),    // list
            Constraint::Length(1), // footer
        ])
        .split(area);

    let title = if onboarding {
        "Providers — set up, then continue"
    } else {
        "Providers"
    };
    list_frame::render_title(frame, chunks[0], title);

    let list = Paragraph::new(build_lines(state)).block(list_frame::block());
    frame.render_widget(list, chunks[1]);

    let mut hints: Vec<(&str, &str)> = vec![
        ("Space", "toggle"),
        ("a", "add"),
        ("e", "edit"),
        ("d", "remove"),
        ("t", "test"),
    ];
    // Outside onboarding, Enter re-probes the selected provider (refresh its
    // online/offline status without restarting); in onboarding Enter is "continue".
    if onboarding {
        hints.push(("Enter", "continue"));
    } else {
        hints.push(("Enter", "refresh"));
        hints.push(("Esc", "back"));
    }
    footer::render(frame, chunks[2], &hints);
}

/// Build the highlighted list lines for the current view.
fn build_lines(state: &ProvidersView) -> Vec<Line<'static>> {
    if state.is_empty() {
        return vec![list_frame::empty_line(
            "no providers configured or discovered",
        )];
    }

    let mut lines: Vec<Line<'static>> = state
        .rows()
        .iter()
        .enumerate()
        .map(|(idx, row)| {
            let (name_style, marker) = list_frame::row_style(idx == state.cursor);
            let checkbox = if row.enabled { "[x] " } else { "[ ] " };
            // A re-probe in flight shows first; then auth failure and an
            // unresolved key env take precedence over plain online/offline so the
            // user knows what to fix.
            let (status, status_style) = if row.checking {
                ("checking…", Style::default().fg(theme::WARN))
            } else if row.auth_failed {
                ("auth failed", Style::default().fg(theme::DANGER))
            } else if row.key_env_unresolved {
                ("key env not set", Style::default().fg(theme::TEXT_FAINT))
            } else if row.online {
                ("online", Style::default().fg(theme::ACCENT))
            } else if row.connection_issue {
                ("connection issue", Style::default().fg(theme::DANGER))
            } else {
                ("offline", Style::default().fg(theme::TEXT_FAINT))
            };
            Line::from(vec![
                Span::styled(format!("{marker}{checkbox}"), name_style),
                Span::styled(format!("{:<14}", row.name), name_style),
                Span::styled(
                    format!("{:<28}", row.endpoint),
                    Style::default().fg(theme::TEXT_FAINT),
                ),
                Span::styled(status.to_string(), status_style),
            ])
        })
        .collect();

    // Load issues render dimmed below the providers: `id — reason`.
    for issue in state.issues() {
        lines.push(Line::from(Span::styled(
            format!("    {} — {}", issue.id, issue.reason),
            Style::default().fg(theme::TEXT_FAINT),
        )));
    }

    // An in-list connection test's status, while its row stays selected.
    match state.visible_test() {
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

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str, endpoint: &str, transport: TransportType, enabled: bool) -> Provider {
        Provider {
            id: id.into(),
            name: id.into(),
            endpoint: endpoint.into(),
            transport,
            enabled,
            api_key: None,
            api_key_env: None,
        }
    }

    fn fixture() -> ProvidersView {
        let providers = vec![
            provider(
                "ollama",
                "http://localhost:11434",
                TransportType::Ollama,
                true,
            ),
            provider(
                "lmstudio",
                "http://localhost:1234",
                TransportType::OpenAiCompatible,
                false,
            ),
        ];
        let online: HashSet<String> = ["ollama".to_string()].into_iter().collect();
        ProvidersView::from_providers(&providers, &online)
    }

    #[test]
    fn builds_rows_with_online_flag() {
        let view = fixture();
        assert_eq!(view.rows().len(), 2);
        assert!(view.rows()[0].online); // ollama discovered
        assert!(!view.rows()[1].online); // lmstudio stored only
        assert!(view.rows()[0].enabled);
        assert!(!view.rows()[1].enabled);
    }

    #[test]
    fn toggle_flips_selected_enabled() {
        let mut view = fixture();
        assert!(view.rows()[0].enabled);
        view.toggle_selected();
        assert!(!view.rows()[0].enabled);
        view.toggle_selected();
        assert!(view.rows()[0].enabled);
    }

    #[test]
    fn toggle_follows_cursor() {
        let mut view = fixture();
        view.move_down();
        view.toggle_selected();
        // lmstudio (initially disabled) is now enabled; ollama untouched.
        assert!(view.rows()[0].enabled);
        assert!(view.rows()[1].enabled);
    }

    #[test]
    fn navigation_wraps() {
        let mut view = fixture();
        view.move_up(); // wrap to last
        view.toggle_selected();
        assert!(view.rows()[1].enabled);
    }

    #[test]
    fn to_config_round_trips_toggles() {
        let mut view = fixture();
        view.move_down();
        view.toggle_selected(); // enable lmstudio
        let config = view.to_config();
        assert_eq!(config.providers.len(), 2);
        let lm = config
            .providers
            .iter()
            .find(|e| e.id == "lmstudio")
            .unwrap();
        assert!(lm.enabled);
        assert_eq!(lm.transport, "openai");
        assert_eq!(lm.endpoint, "http://localhost:1234");
    }

    #[test]
    fn reprobe_brings_a_provider_online_in_place() {
        use suis_providers::DiscoveryResult;
        let mut view = fixture();
        view.move_down(); // select lmstudio (offline)
        assert!(!view.rows()[1].online);

        let id = view.begin_reprobe().unwrap();
        assert_eq!(id, "lmstudio");
        assert!(view.rows()[1].checking, "re-probe marks the row checking");

        let result = DiscoveryResult {
            provider: provider(
                "lmstudio",
                "http://localhost:1234",
                TransportType::OpenAiCompatible,
                true,
            ),
            models: vec![],
        };
        view.apply_outcome(&ProbeOutcome::Online(result));

        assert!(view.rows()[1].online, "the row flips to online");
        assert!(!view.rows()[1].checking, "checking clears on resolution");
        // The cursor stays put so the user keeps their place.
        assert_eq!(view.selected().unwrap().id, "lmstudio");
    }

    #[test]
    fn reprobe_offline_clears_online_and_checking() {
        let mut view = fixture(); // ollama online at index 0
        assert!(view.rows()[0].online);
        view.begin_reprobe();
        view.apply_outcome(&ProbeOutcome::Offline {
            id: "ollama".into(),
        });
        assert!(!view.rows()[0].online);
        assert!(!view.rows()[0].checking);
    }

    #[test]
    fn empty_view_is_empty() {
        let view = ProvidersView::from_providers(&[], &HashSet::new());
        assert!(view.is_empty());
    }

    #[test]
    fn issues_are_carried_and_keep_view_non_empty() {
        let issues = vec![ProviderIssue {
            id: "myproxy".into(),
            field: "transport".into(),
            reason: "unknown transport \"openai-compat\"".into(),
        }];
        let view = ProvidersView::from_parts(
            &[],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &issues,
        );
        assert!(
            !view.is_empty(),
            "an issue alone is still something to show"
        );
        assert_eq!(view.issues().len(), 1);
        assert_eq!(view.issues()[0].id, "myproxy");
    }

    #[test]
    fn auth_failed_and_key_flags_surface_on_rows() {
        let mut keyed = provider(
            "openrouter",
            "https://openrouter.ai/api",
            TransportType::OpenAiCompatible,
            true,
        );
        keyed.api_key_env = Some("OPENROUTER_API_KEY".into());
        // Env named but unresolved => key_env_unresolved.
        let auth_failed: HashSet<String> = ["lmstudio".to_string()].into_iter().collect();
        let lm = provider(
            "lmstudio",
            "http://localhost:1234",
            TransportType::OpenAiCompatible,
            true,
        );
        let view = ProvidersView::from_parts(
            &[keyed, lm],
            &HashSet::new(),
            &auth_failed,
            &HashSet::new(),
            &[],
        );
        let or = &view.rows()[0];
        assert!(or.key_env_unresolved);
        let lm = &view.rows()[1];
        assert!(lm.auth_failed);
    }

    #[test]
    fn to_config_round_trips_key_env_without_resolving() {
        let mut keyed = provider(
            "work",
            "https://proxy.example/v1",
            TransportType::OpenAiCompatible,
            true,
        );
        keyed.name = "Work Proxy".into();
        keyed.api_key_env = Some("WORK_KEY".into());
        keyed.api_key = Some("resolved-secret".into());
        let view = ProvidersView::from_providers(&[keyed], &HashSet::new());
        let config = view.to_config();
        let entry = &config.providers[0];
        assert_eq!(entry.name.as_deref(), Some("Work Proxy"));
        assert_eq!(entry.api_key_env.as_deref(), Some("WORK_KEY"));
        // The resolved secret must never be written back as a literal.
        assert_eq!(entry.api_key, None);
    }
}
