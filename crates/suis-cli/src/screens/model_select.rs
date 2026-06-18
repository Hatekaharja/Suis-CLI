//! The model selection screen and its view-model.
//!
//! Lists discovered providers as collapsible groups: each provider shows as a
//! single header row, collapsed by default, and expanding it (Enter) reveals its
//! models with a capability badge. Collapsing keeps the screen usable when a
//! remote provider lists hundreds of models. A live name filter searches across
//! every provider and auto-expands the ones with matches. The view-model
//! ([`ModelSelect`]) is pure and unit-tested; [`render`] draws it.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::screens::provider_form::TestState;
use crate::theme;
use crate::widgets::{footer, list_frame};
use suis_core::{ModelScope, ProviderEntry};
#[cfg(test)]
use suis_providers::DiscoveryResult;
use suis_providers::{Capabilities, Model, ProbeOutcome, Provider, ProviderStatus, TransportType};

/// One selectable (provider, model) pair.
#[derive(Debug, Clone)]
pub struct ModelEntry {
    /// Owning provider id (e.g. `"ollama"`).
    pub provider_id: String,
    /// Provider display name.
    pub provider_name: String,
    /// Provider endpoint URL.
    pub endpoint: String,
    /// The provider's transport, used to build a client once selected.
    pub transport: TransportType,
    /// The provider's resolved API key, threaded into the transport on select.
    pub api_key: Option<String>,
    /// The provider's key env-var name, threaded into the transport so a remote
    /// auth failure can name what to check (20.2).
    pub api_key_env: Option<String>,
    /// The model itself.
    pub model: Model,
}

impl ModelEntry {
    /// The capability badge shown next to the model name. Capabilities are no
    /// longer probed at startup, so an unverified model is *assumed* chat-only
    /// and shows a muted `[chat]` until the user verifies it at selection time;
    /// a verified model shows its real `[tools]`/`[chat]` in the accent colour.
    pub fn badge(&self) -> &'static str {
        if self.caps_unknown() {
            "[chat]"
        } else if self.model.capabilities.tool_use {
            "[tools]"
        } else {
            "[chat]"
        }
    }

    /// Whether this model's capabilities are unresolved: it is unverified, so
    /// the consent-gated probe (the chat-screen "Verify capabilities?" prompt)
    /// has not run yet and its badge is shown muted.
    pub fn caps_unknown(&self) -> bool {
        !self.model.verified
    }
}

/// A provider and its models, drawn as one collapsible group.
#[derive(Debug, Clone)]
struct ProviderGroup {
    /// Stable provider id.
    id: String,
    /// Provider display name.
    name: String,
    /// Provider endpoint URL.
    endpoint: String,
    /// The provider's transport, kept so an offline header can be re-probed
    /// (Enter) without its models present to carry it.
    transport: TransportType,
    /// The provider's resolved API key (if any), for the re-probe entry.
    api_key: Option<String>,
    /// The provider's key env-var name (if any), for the re-probe entry — so an
    /// env-backed provider re-resolves the variable on retry.
    api_key_env: Option<String>,
    /// Every in-scope model for this provider. Empty until the probe lands (or
    /// for a provider that is offline / errored / has no in-scope models).
    models: Vec<ModelEntry>,
    /// This provider's discovery status, shown as a coloured dot on the header:
    /// `checking…` while its probe is in flight, then online / offline /
    /// connection-issue / auth-failed. Only an [`Online`](ProviderStatus::Online)
    /// group with models is expandable; the rest are inert status rows.
    status: ProviderStatus,
    /// Whether the group's models are shown. Collapsed by default so a remote
    /// provider listing hundreds of models doesn't flood the picker; a filter
    /// expands matching groups regardless of this flag.
    expanded: bool,
}

impl ProviderGroup {
    /// Whether this group can be expanded to reveal models: it is online and has
    /// at least one in-scope model. Offline / checking / errored groups are
    /// inert — their header is a status row only.
    fn is_expandable(&self) -> bool {
        self.status == ProviderStatus::Online && !self.models.is_empty()
    }

    /// This group as a persistable entry, for the edit form and connection test.
    /// A provider shown in the picker is enabled by definition (disabled ones are
    /// never probed). Env-backed keys round-trip the variable name, not the
    /// resolved secret — mirroring `ProviderRow::to_entry`.
    fn to_entry(&self) -> ProviderEntry {
        let (api_key_env, api_key) = match &self.api_key_env {
            Some(env) => (Some(env.clone()), None),
            None => (None, self.api_key.clone()),
        };
        ProviderEntry {
            id: self.id.clone(),
            endpoint: self.endpoint.clone(),
            transport: self.transport.as_str().to_string(),
            enabled: true,
            name: (self.name != self.id).then(|| self.name.clone()),
            api_key_env,
            api_key,
        }
    }
}

/// A navigable row in the picker: a provider header, one of its models, or the
/// trailing "add a provider" action. Carries only indices, so it is `Copy` and
/// borrows nothing — the view-model rebuilds the row list on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowRef {
    /// A provider header (index into `groups`).
    Provider(usize),
    /// A model under a provider (`groups` index, `models` index).
    Model(usize, usize),
    /// The always-present "+ Add a provider…" action row.
    AddProvider,
}

/// The model-selection view-model: the provider groups plus filter/cursor state.
///
/// Filtering is type-to-filter and display-only: any printable key narrows the
/// list live ([`push_filter`](Self::push_filter)), auto-expanding the providers
/// with matches. The underlying groups are fixed at construction (expansion and
/// cursor aside) and reset on screen entry.
#[derive(Debug, Clone, Default)]
pub struct ModelSelect {
    groups: Vec<ProviderGroup>,
    /// Cursor position within the current [`visible_rows`](Self::visible_rows).
    cursor: usize,
    /// Current filter text; empty means "show everything".
    pub filter: String,
    /// The project's model scope, retained so online results streamed in after
    /// construction ([`apply_outcome`](Self::apply_outcome)) are filtered the
    /// same way the blocking path filtered them at build time.
    scope: ModelScope,
    /// In-list connection test (`Ctrl+T` on a provider header), shown only while
    /// the cursor stays on the row it was started on (see [`test_target`]).
    pub test: TestState,
    /// The provider id the in-list test was started on, so its result clears
    /// once the cursor moves off that header. Mirrors `ProvidersView`.
    test_target: Option<String>,
}

impl ModelSelect {
    /// Build the view-model from live discovery results, showing every model.
    /// A thin convenience over [`from_results_scoped`](Self::from_results_scoped)
    /// for tests and callers without a project scope.
    #[cfg(test)]
    pub fn from_results(results: &[DiscoveryResult]) -> Self {
        Self::from_results_scoped(results, &ModelScope::All)
    }

    /// Build the view-model from live discovery results, honoring the project's
    /// [`ModelScope`] (20.3): a project scoped to a list of models shows only
    /// those, regardless of how many the provider lists — keeping a remote
    /// provider's flood usable. `ModelScope::All` keeps every model. Providers
    /// left with no in-scope models are dropped entirely.
    ///
    /// Test-only since the streaming path replaced it: production builds the
    /// skeleton with [`from_plan`](Self::from_plan) and folds outcomes in via
    /// [`apply_outcome`](Self::apply_outcome).
    #[cfg(test)]
    pub fn from_results_scoped(results: &[DiscoveryResult], scope: &ModelScope) -> Self {
        let mut groups = Vec::new();
        for result in results {
            let models: Vec<ModelEntry> = result
                .models
                .iter()
                .filter(|model| scope_allows(scope, &model.model_id))
                .map(|model| ModelEntry {
                    provider_id: result.provider.id.clone(),
                    provider_name: provider_label(&result.provider),
                    endpoint: result.provider.endpoint.clone(),
                    transport: result.provider.transport,
                    api_key: result.provider.api_key.clone(),
                    api_key_env: result.provider.api_key_env.clone(),
                    model: model.clone(),
                })
                .collect();
            if models.is_empty() {
                continue;
            }
            groups.push(ProviderGroup {
                id: result.provider.id.clone(),
                name: provider_label(&result.provider),
                endpoint: result.provider.endpoint.clone(),
                transport: result.provider.transport,
                api_key: result.provider.api_key.clone(),
                api_key_env: result.provider.api_key_env.clone(),
                models,
                // A result is, by definition, an online provider.
                status: ProviderStatus::Online,
                expanded: false,
            });
        }
        ModelSelect {
            groups,
            cursor: 0,
            filter: String::new(),
            scope: scope.clone(),
            test: TestState::Idle,
            test_target: None,
        }
    }

    /// Build the picker skeleton from the providers discovery *will* probe: every
    /// provider shows immediately as a collapsed header marked "checking…", with
    /// no models yet. Probe outcomes then resolve each row via
    /// [`apply_outcome`](Self::apply_outcome). This is what lets the picker open
    /// instantly instead of blocking on the slowest endpoint.
    pub fn from_plan(providers: &[Provider], scope: &ModelScope) -> Self {
        let groups = providers
            .iter()
            .map(|p| ProviderGroup {
                id: p.id.clone(),
                name: provider_label(p),
                endpoint: p.endpoint.clone(),
                transport: p.transport,
                api_key: p.api_key.clone(),
                api_key_env: p.api_key_env.clone(),
                models: Vec::new(),
                status: ProviderStatus::Checking,
                expanded: false,
            })
            .collect();
        ModelSelect {
            groups,
            cursor: 0,
            filter: String::new(),
            // Carry the scope so streamed online results are filtered like the
            // blocking path's were.
            scope: scope.clone(),
            test: TestState::Idle,
            test_target: None,
        }
    }

    /// Fold one streamed probe outcome into the matching provider group: update
    /// its status and, for an online provider, populate its in-scope models —
    /// without disturbing the cursor, filter, or other groups' expansion. An
    /// outcome for an unknown id (shouldn't happen) is ignored.
    pub fn apply_outcome(&mut self, outcome: &ProbeOutcome) {
        let id = outcome.id();
        let Some(group) = self.groups.iter_mut().find(|g| g.id == id) else {
            return;
        };
        group.status = outcome.status();
        if let ProbeOutcome::Online(result) = outcome {
            // Refresh the provider's connection details so a later re-probe (or
            // selection) uses what the live probe reported.
            group.endpoint = result.provider.endpoint.clone();
            group.transport = result.provider.transport;
            group.api_key = result.provider.api_key.clone();
            group.api_key_env = result.provider.api_key_env.clone();
            group.models = result
                .models
                .iter()
                .filter(|model| scope_allows(&self.scope, &model.model_id))
                .map(|model| ModelEntry {
                    provider_id: result.provider.id.clone(),
                    provider_name: provider_label(&result.provider),
                    endpoint: result.provider.endpoint.clone(),
                    transport: result.provider.transport,
                    api_key: result.provider.api_key.clone(),
                    api_key_env: result.provider.api_key_env.clone(),
                    model: model.clone(),
                })
                .collect();
        } else {
            group.models.clear();
            group.expanded = false;
        }
    }

    /// Whether there are no providers (hence no models) at all. A test helper
    /// now that an empty picker is a normal state (it opens on the add-provider
    /// row) rather than a startup error.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Expand `provider_id`'s group and move the cursor to its first model. Used
    /// to honor the global config's preferred default provider on startup, so it
    /// opens unfolded with a model under the cursor.
    pub fn focus_provider(&mut self, provider_id: &str) {
        let Some(gi) = self.groups.iter().position(|g| g.id == provider_id) else {
            return;
        };
        self.groups[gi].expanded = true;
        for (idx, row) in self.visible_rows().iter().enumerate() {
            if matches!(row, RowRef::Model(g, 0) if *g == gi) {
                self.cursor = idx;
                return;
            }
        }
    }

    /// Every model matching the current filter, flattened across providers and
    /// in display order. Ignores expansion (the filter is what narrows here).
    /// A test/assertion helper — the screen renders from
    /// [`visible_rows`](Self::visible_rows), not this.
    #[cfg(test)]
    pub fn filtered(&self) -> Vec<&ModelEntry> {
        let needle = self.filter.to_ascii_lowercase();
        self.groups
            .iter()
            .flat_map(|g| g.models.iter())
            .filter(|m| self.filter.is_empty() || model_matches(m, &needle))
            .collect()
    }

    /// The currently highlighted model, if the cursor is on a model row. `None`
    /// on a provider header or the "add a provider" row.
    pub fn selected(&self) -> Option<&ModelEntry> {
        match self.visible_rows().get(self.cursor) {
            Some(RowRef::Model(gi, mi)) => Some(&self.groups[*gi].models[*mi]),
            _ => None,
        }
    }

    /// Whether the cursor is on the "+ Add a provider…" row, the always-present
    /// action pinned below the providers. Selecting it opens the add-provider
    /// form rather than starting a chat session.
    pub fn is_add_provider_selected(&self) -> bool {
        matches!(
            self.visible_rows().get(self.cursor),
            Some(RowRef::AddProvider)
        )
    }

    /// If the cursor is on a provider header, flip its expansion and report
    /// `true`; otherwise (a model or the add row) report `false`. Enter routes
    /// through this so a header toggles and a model selects. A no-op while a
    /// filter is active, where groups are force-expanded for the search.
    pub fn toggle_selected_provider(&mut self) -> bool {
        if let Some(RowRef::Provider(gi)) = self.visible_rows().get(self.cursor).copied() {
            // An offline / checking / errored provider is an inert status row:
            // it has no models to reveal, so Enter does nothing on it.
            if !self.groups[gi].is_expandable() {
                return false;
            }
            self.groups[gi].expanded = !self.groups[gi].expanded;
            self.clamp_cursor();
            true
        } else {
            false
        }
    }

    /// If the cursor is on an *inert* provider header (offline / connection
    /// issue / auth failed — anything not expandable, and not already mid-probe),
    /// mark it re-checking and return the entry to re-probe; otherwise `None`.
    /// Enter routes through this after [`toggle_selected_provider`], so a model
    /// row falls through to selection and an online header still expands.
    pub fn begin_reprobe_selected(&mut self) -> Option<ProviderEntry> {
        let RowRef::Provider(gi) = self.visible_rows().get(self.cursor).copied()? else {
            return None;
        };
        let group = &mut self.groups[gi];
        if group.is_expandable() || group.status == ProviderStatus::Checking {
            return None;
        }
        group.status = ProviderStatus::Checking;
        Some(group.to_entry())
    }

    /// The provider header under the cursor, if any: its persistable entry (for
    /// the edit form and the connection test). `None` on a model or the add row.
    pub fn selected_provider_entry(&self) -> Option<ProviderEntry> {
        let RowRef::Provider(gi) = self.visible_rows().get(self.cursor).copied()? else {
            return None;
        };
        Some(self.groups[gi].to_entry())
    }

    /// The id of the provider header under the cursor, if any (for delete and to
    /// scope the connection-test display). `None` on a model or the add row.
    pub fn selected_provider_id(&self) -> Option<String> {
        let RowRef::Provider(gi) = self.visible_rows().get(self.cursor).copied()? else {
            return None;
        };
        Some(self.groups[gi].id.clone())
    }

    /// Remove the provider and its models from the picker entirely (custom
    /// delete) or disable it (default provider). Updates the cursor so it stays
    /// on a valid row.
    pub fn remove_provider(&mut self, id: &str) {
        self.groups.retain(|g| g.id != id);
        self.clamp_cursor();
        // Also clear any test state tied to this provider.
        if self.test_target.as_deref() == Some(id) {
            self.test_target = None;
            self.test = TestState::Idle;
        }
    }

    /// The provider with `id` as a persistable entry, for reconstructing a removed
    /// default's row. `None` if no such provider is shown.
    pub fn entry_for(&self, id: &str) -> Option<ProviderEntry> {
        self.groups
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.to_entry())
    }

    /// Every shown provider id, for the edit form's uniqueness check.
    pub fn provider_ids(&self) -> Vec<String> {
        self.groups.iter().map(|g| g.id.clone()).collect()
    }

    /// Whether the cursor is on a provider header (so the footer can advertise the
    /// header-only management keys, and input can route them).
    pub fn on_provider_header(&self) -> bool {
        matches!(
            self.visible_rows().get(self.cursor),
            Some(RowRef::Provider(_))
        )
    }

    /// Mark a connection test as started on the highlighted provider header.
    pub fn begin_test(&mut self) {
        if let Some(id) = self.selected_provider_id() {
            self.test_target = Some(id);
            self.test = TestState::Testing;
        }
    }

    /// Record a finished connection test (shown only while its row stays
    /// selected — see [`visible_test`](Self::visible_test)).
    pub fn set_test(&mut self, state: TestState) {
        self.test = state;
    }

    /// The test state to display, suppressed once the cursor leaves the header the
    /// test was started on. Mirrors `ProvidersView::visible_test`.
    pub fn visible_test(&self) -> &TestState {
        match (&self.test_target, self.selected_provider_id()) {
            (Some(target), Some(id)) if *target == id => &self.test,
            _ => &TestState::Idle,
        }
    }

    /// Move the cursor down one, wrapping at the end.
    pub fn move_down(&mut self) {
        let len = self.row_count();
        self.cursor = (self.cursor + 1) % len;
    }

    /// Move the cursor up one, wrapping at the start.
    pub fn move_up(&mut self) {
        let len = self.row_count();
        self.cursor = (self.cursor + len - 1) % len;
    }

    /// Append a character to the filter, landing the cursor on the first match.
    pub fn push_filter(&mut self, c: char) {
        self.filter.push(c);
        self.cursor = self.first_model_cursor();
    }

    /// Remove the last filter character, re-homing the cursor onto a match.
    pub fn pop_filter(&mut self) {
        self.filter.pop();
        self.cursor = self.first_model_cursor();
    }

    /// Clear the filter entirely (Esc's first press), resetting the cursor to
    /// the top (the first provider header).
    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.cursor = 0;
    }

    /// Record probed capabilities for one model (the consent-gated probe),
    /// flipping it verified so its badge brightens from the muted assumed
    /// `[chat]` to its real capabilities and the selection carries them.
    pub fn set_caps(&mut self, provider_id: &str, model_id: &str, caps: Capabilities) {
        for group in self.groups.iter_mut().filter(|g| g.id == provider_id) {
            for entry in group
                .models
                .iter_mut()
                .filter(|m| m.model.model_id == model_id)
            {
                entry.model.capabilities = caps;
                entry.model.verified = true;
            }
        }
    }

    /// The navigable rows for the current filter and expansion state: each shown
    /// provider header, the models of expanded (or filter-matched) groups, then
    /// the trailing add-provider row. While filtering, a group with no match is
    /// hidden and a matching group is force-expanded to show only its matches.
    fn visible_rows(&self) -> Vec<RowRef> {
        let filtering = !self.filter.is_empty();
        let needle = self.filter.to_ascii_lowercase();
        let mut rows = Vec::new();
        for (gi, group) in self.groups.iter().enumerate() {
            let matches: Vec<usize> = group
                .models
                .iter()
                .enumerate()
                .filter(|(_, m)| !filtering || model_matches(m, &needle))
                .map(|(mi, _)| mi)
                .collect();
            if filtering && matches.is_empty() {
                continue;
            }
            rows.push(RowRef::Provider(gi));
            if filtering || group.expanded {
                for mi in matches {
                    rows.push(RowRef::Model(gi, mi));
                }
            }
        }
        rows.push(RowRef::AddProvider);
        rows
    }

    /// The number of navigable rows (always at least the add-provider row).
    fn row_count(&self) -> usize {
        self.visible_rows().len()
    }

    /// The zero-based index of the cursor's row within the rendered lines (see
    /// [`build_lines`]). The mapping is one line per row, except the
    /// add-provider row, which [`build_lines`] precedes with a blank spacer
    /// line; the leading empty-state notice (shown only when nothing but the add
    /// row exists) shifts everything down by one. Used to drive the list's
    /// scroll offset so the cursor stays on screen.
    pub fn selected_line(&self) -> usize {
        let rows = self.visible_rows();
        let only_add = rows.iter().all(|r| matches!(r, RowRef::AddProvider));
        let mut line = if only_add { 1 } else { 0 };
        for (idx, row) in rows.iter().enumerate() {
            // The add-provider row is drawn after a blank spacer line.
            let lead = usize::from(matches!(row, RowRef::AddProvider));
            if idx == self.cursor {
                return line + lead;
            }
            line += lead + 1;
        }
        line
    }

    /// The index of the first model row, for homing the cursor after a filter
    /// edit. Falls back to the top when nothing is expanded (no models shown).
    fn first_model_cursor(&self) -> usize {
        self.visible_rows()
            .iter()
            .position(|r| matches!(r, RowRef::Model(..)))
            .unwrap_or(0)
    }

    /// Keep the cursor within the navigable range after the rows change. There
    /// is always at least the add-provider row, so the range is never empty.
    fn clamp_cursor(&mut self) {
        let len = self.row_count();
        if self.cursor >= len {
            self.cursor = len - 1;
        }
    }
}

/// Whether a model's display name or id contains the (lowercased) needle.
fn model_matches(entry: &ModelEntry, needle: &str) -> bool {
    entry
        .model
        .display_name
        .to_ascii_lowercase()
        .contains(needle)
        || entry.model.model_id.to_ascii_lowercase().contains(needle)
}

/// Whether `model_id` is allowed by `scope`. `All` admits everything; `List`
/// admits an exact id or an ollama-style tag of a listed base (`qwen3-coder`
/// matches `qwen3-coder:latest`), so a short project scope keeps working across
/// a provider's tag variants.
fn scope_allows(scope: &ModelScope, model_id: &str) -> bool {
    match scope {
        ModelScope::All => true,
        ModelScope::List(ids) => ids
            .iter()
            .any(|s| model_id == s || model_id.starts_with(&format!("{s}:"))),
    }
}

/// The status dot glyph, label, and colour for a provider header. Green ● for a
/// live provider, a hollow ○ for one that is simply offline, and a red ● for a
/// reachable-but-broken endpoint (timed out / server error) or rejected
/// credentials — so a sleeping LAN box reads differently from a closed port. A
/// dim ◌ marks a probe still in flight.
fn status_display(status: ProviderStatus) -> (&'static str, &'static str, ratatui::style::Color) {
    match status {
        ProviderStatus::Checking => ("◌", "checking…", theme::TEXT_DIM),
        ProviderStatus::Online => ("●", "online", theme::ACCENT),
        ProviderStatus::Offline => ("○", "offline", theme::TEXT_FAINT),
        ProviderStatus::ConnectionIssue => ("●", "connection issue", theme::DANGER),
        ProviderStatus::AuthFailed => ("●", "auth failed", theme::DANGER),
    }
}

/// A human-readable provider label, falling back to the id.
fn provider_label(provider: &Provider) -> String {
    if provider.name.is_empty() {
        provider.id.clone()
    } else {
        provider.name.clone()
    }
}

/// Render the model-selection screen.
/// The ASCII-art banner shown at startup, embedded at compile time so the
/// binary needs no runtime asset. Trailing blank lines are trimmed at use.
const LOGO: &str = include_str!("../../../../LOGO.txt");

/// The banner's natural width in columns (it is centered within wider areas).
const LOGO_WIDTH: u16 = 72;

/// Render the picker. `can_return` is true when the picker was opened mid-session
/// (a model is already chosen), so Esc returns to chat rather than quitting — the
/// footer labels it accordingly.
pub fn render(frame: &mut Frame, area: Rect, state: &ModelSelect, can_return: bool) {
    // The logo replaces the plain title when the terminal has room for it;
    // otherwise we fall back to the one-line "Select a Model" header.
    let logo_lines: Vec<&str> = LOGO.trim_end_matches('\n').lines().collect();
    let logo_height = logo_lines.len() as u16;
    let show_logo = area.width >= LOGO_WIDTH && area.height >= logo_height + 6;
    let header_height = if show_logo { logo_height } else { 2 };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height), // logo banner or title
            Constraint::Min(1),                // list
            Constraint::Length(1),             // footer
        ])
        .split(area);

    if show_logo {
        render_logo(frame, chunks[0], &logo_lines);
    } else {
        list_frame::render_title(frame, chunks[0], "Select a Model");
    }

    let lines = build_lines(state);
    // Scroll the list so the highlighted row stays visible when the provider
    // groups expand past the viewport. The block's borders take a row top and
    // bottom, so the inner height is two less than the chunk.
    let inner_height = chunks[1].height.saturating_sub(2);
    let offset = scroll_offset(state.selected_line(), lines.len(), inner_height as usize);
    let list = Paragraph::new(lines)
        .block(list_frame::block())
        .scroll((offset, 0));
    frame.render_widget(list, chunks[1]);

    if !state.filter.is_empty() {
        let filter = format!("{}_", state.filter);
        footer::render(
            frame,
            chunks[2],
            &[
                ("filter:", filter.as_str()),
                ("Enter", "select"),
                ("Esc", "clear"),
            ],
        );
    } else if state.on_provider_header() {
        // A provider header offers the management keys; model rows do not.
        let exit = if can_return { "back" } else { "quit" };
        footer::render(
            frame,
            chunks[2],
            &[
                ("Enter", "expand/refresh"),
                ("^E", "edit"),
                ("^D", "delete"),
                ("^T", "test"),
                ("Esc", exit),
            ],
        );
    } else {
        let exit = if can_return { "back" } else { "quit" };
        footer::render(
            frame,
            chunks[2],
            &[
                ("Enter", "select"),
                ("↑/↓", "navigate"),
                ("type", "filter"),
                ("Esc", exit),
            ],
        );
    }
}

/// Render the ASCII logo, horizontally centered within `area`. The art carries
/// its own internal spacing, so the whole block is left-aligned inside a
/// centered, logo-width sub-rect rather than per-line centered.
fn render_logo(frame: &mut Frame, area: Rect, logo_lines: &[&str]) {
    let lines: Vec<Line<'static>> = logo_lines
        .iter()
        .map(|line| {
            Line::from(Span::styled(
                line.to_string(),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ))
        })
        .collect();

    let width = LOGO_WIDTH.min(area.width);
    let centered = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y,
        width,
        height: area.height,
    };
    frame.render_widget(Paragraph::new(lines), centered);
}

/// The vertical scroll offset that keeps line `selected` within a `viewport`-row
/// window over `total` lines. Returns `0` while everything fits; once the cursor
/// passes the bottom edge it scrolls just enough to keep the cursor on the last
/// visible row, clamped so it never scrolls past the final line.
fn scroll_offset(selected: usize, total: usize, viewport: usize) -> u16 {
    if viewport == 0 || total <= viewport {
        return 0;
    }
    let max = total - viewport;
    let offset = selected.saturating_sub(viewport - 1).min(max);
    offset as u16
}

/// Build the collapsible, highlighted list lines for the current view: provider
/// headers (with an expand/collapse hint), the models of open groups, and the
/// always-present "add a provider" action row.
fn build_lines(state: &ModelSelect) -> Vec<Line<'static>> {
    let rows = state.visible_rows();
    let filtering = !state.filter.is_empty();
    let needle = state.filter.to_ascii_lowercase();

    let mut lines = Vec::new();

    // With no provider or model rows there is nothing but the add row; show why.
    if rows.iter().all(|r| matches!(r, RowRef::AddProvider)) {
        let notice = if filtering {
            "no matching models"
        } else {
            "No providers found — Suis looks for Ollama (:11434) and LM Studio (:1234). Press Enter on the row below to add one."
        };
        lines.push(list_frame::empty_line(notice));
    }

    for (idx, row) in rows.iter().enumerate() {
        let selected = idx == state.cursor;
        match row {
            RowRef::Provider(gi) => {
                let group = &state.groups[*gi];
                let (marker_style, marker) = list_frame::row_style(selected);
                let expandable = group.is_expandable();
                let expanded = expandable && (filtering || group.expanded);
                // Only an expandable (online, with models) row gets a chevron;
                // an inert status row keeps the column aligned with a space.
                let chevron = if !expandable {
                    "  "
                } else if expanded {
                    "▾ "
                } else {
                    "▸ "
                };
                let name_style = if selected {
                    Style::default()
                        .fg(theme::TEXT_BRIGHT)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(theme::WARN)
                        .add_modifier(Modifier::BOLD)
                };
                let (dot, label, status_color) = status_display(group.status);
                // The expand/collapse affordance only applies to online rows; an
                // offline / errored row offers a retry instead, while a row whose
                // probe is still in flight shows just its status word.
                let hint = if !expandable {
                    if group.status == ProviderStatus::Checking {
                        String::new()
                    } else {
                        "  · enter to retry".to_string()
                    }
                } else if filtering {
                    let count = group
                        .models
                        .iter()
                        .filter(|m| model_matches(m, &needle))
                        .count();
                    format!("  ({count} matching)")
                } else if group.expanded {
                    "  · enter to collapse".to_string()
                } else {
                    format!("  · {} models · enter to expand", group.models.len())
                };
                lines.push(Line::from(vec![
                    Span::styled(marker, marker_style),
                    Span::styled(chevron, name_style),
                    Span::styled(format!("{} ({})  ", group.name, group.endpoint), name_style),
                    Span::styled(format!("{dot} "), Style::default().fg(status_color)),
                    Span::styled(label.to_string(), Style::default().fg(status_color)),
                    Span::styled(hint, Style::default().fg(theme::TEXT_DIM)),
                ]));
            }
            RowRef::Model(gi, mi) => {
                let entry = &state.groups[*gi].models[*mi];
                let (name_style, marker) = list_frame::row_style(selected);
                // An unverified model's capabilities are assumed, not measured,
                // so its badge reads in a muted gray; a verified one uses accent.
                let badge = if entry.caps_unknown() {
                    Span::styled(
                        entry.badge().to_string(),
                        Style::default().fg(theme::TEXT_DIM),
                    )
                } else {
                    list_frame::badge(entry.badge())
                };
                lines.push(Line::from(vec![
                    Span::styled(marker, name_style),
                    // An extra indent nests the model under its provider header.
                    Span::styled("  ", name_style),
                    Span::styled(format!("{:<28}", entry.model.display_name), name_style),
                    badge,
                ]));
            }
            RowRef::AddProvider => {
                let (style, marker) = list_frame::row_style(selected);
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled(marker, style),
                    Span::styled("+ Add a provider…", style),
                ]));
            }
        }
    }

    // A `Ctrl+T` connection test's status, while its header stays selected.
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
    use suis_providers::{Capabilities, Provider};

    fn result(
        id: &str,
        endpoint: &str,
        transport: TransportType,
        models: &[(&str, bool)],
    ) -> DiscoveryResult {
        DiscoveryResult {
            provider: Provider {
                id: id.into(),
                name: id.into(),
                endpoint: endpoint.into(),
                transport,
                enabled: true,
                api_key: None,
                api_key_env: None,
            },
            models: models
                .iter()
                .map(|(name, tools)| {
                    // Verified (advertised) models, so the badge reflects their
                    // real tool_use rather than the muted "assumed chat" badge.
                    Model::verified_caps(
                        id,
                        *name,
                        Capabilities {
                            chat: true,
                            streaming: true,
                            tool_use: *tools,
                            structured_output: false,
                        },
                    )
                })
                .collect(),
        }
    }

    fn fixture() -> ModelSelect {
        let results = vec![
            result(
                "ollama",
                "http://localhost:11434",
                TransportType::Ollama,
                &[("qwen3-coder:latest", true), ("llama3:8b", false)],
            ),
            result(
                "lmstudio",
                "http://localhost:1234",
                TransportType::OpenAiCompatible,
                &[("llama-3-8b-instruct", false)],
            ),
        ];
        ModelSelect::from_results(&results)
    }

    fn render_to_string(w: u16, h: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let state = fixture();
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &state, false))
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
    fn logo_shows_when_terminal_has_room() {
        let screen = render_to_string(90, 30);
        // The art uses full-block glyphs; the plain title is replaced.
        assert!(screen.contains('█'), "logo not rendered");
        assert!(
            !screen.contains("Select a Model"),
            "title should be replaced"
        );
    }

    #[test]
    fn falls_back_to_title_when_too_small() {
        let screen = render_to_string(60, 20);
        assert!(!screen.contains('█'), "logo should be hidden when small");
        assert!(screen.contains("Select a Model"), "title fallback missing");
    }

    #[test]
    fn providers_start_collapsed() {
        let state = fixture();
        assert!(!state.is_empty());
        // The two providers are headers; no model is exposed yet, so the cursor
        // (on the first provider) selects nothing.
        assert!(state.selected().is_none());
        // Every model is still reachable via the flat filter helper.
        assert_eq!(state.filtered().len(), 3);
    }

    #[test]
    fn expanding_a_provider_reveals_its_models_then_collapses() {
        let mut state = fixture();
        // Cursor starts on the first provider (ollama). Expand it.
        assert!(state.toggle_selected_provider());
        // The first model now sits directly below the header.
        state.move_down();
        assert_eq!(
            state.selected().unwrap().model.model_id,
            "qwen3-coder:latest"
        );
        state.move_down();
        assert_eq!(state.selected().unwrap().model.model_id, "llama3:8b");
        // Back to the header and collapse it; the models disappear again.
        state.move_up();
        state.move_up();
        assert!(state.toggle_selected_provider());
        state.move_down();
        // Down from the first (collapsed) header lands on the second provider,
        // not a model.
        assert!(state.selected().is_none());
    }

    #[test]
    fn enter_on_a_model_does_not_toggle() {
        let mut state = fixture();
        state.toggle_selected_provider(); // expand ollama
        state.move_down(); // onto its first model
        assert!(state.selected().is_some());
        // A model row is not a provider header, so the toggle reports false.
        assert!(!state.toggle_selected_provider());
    }

    #[test]
    fn badge_reflects_tool_use() {
        let state = fixture();
        let qwen = state
            .filtered()
            .into_iter()
            .find(|e| e.model.model_id == "qwen3-coder:latest")
            .unwrap();
        assert_eq!(qwen.badge(), "[tools]");
        let llama = state
            .filtered()
            .into_iter()
            .find(|e| e.model.model_id == "llama3:8b")
            .unwrap();
        assert_eq!(llama.badge(), "[chat]");
    }

    #[test]
    fn navigation_wraps_through_headers_and_the_add_row() {
        let mut state = fixture();
        // Collapsed: rows are [ollama, lmstudio, add-provider].
        assert!(state.selected().is_none(), "first provider header");
        state.move_up(); // wrap to the add-provider row
        assert!(state.is_add_provider_selected());
        state.move_up(); // the second (lmstudio) header
        assert!(state.selected().is_none());
        assert!(!state.is_add_provider_selected());
        state.move_down(); // back to the add row
        assert!(state.is_add_provider_selected());
        state.move_down(); // wrap to the first header
        assert!(state.selected().is_none());
    }

    #[test]
    fn add_provider_row_is_present_even_with_no_models() {
        let mut state = ModelSelect::from_results(&[]);
        assert!(state.is_empty(), "no providers");
        // The cursor starts on the only row there is: the add-provider action.
        assert!(state.is_add_provider_selected());
        assert!(state.selected().is_none());
        // Navigating stays on it (it is the sole row).
        state.move_down();
        assert!(state.is_add_provider_selected());
    }

    #[test]
    fn filter_auto_expands_matches_and_homes_the_cursor() {
        let mut state = fixture();
        for c in "instruct".chars() {
            state.push_filter(c);
        }
        // One model matches, and the cursor homes onto it (not the header).
        assert_eq!(state.filtered().len(), 1);
        assert_eq!(
            state.selected().unwrap().model.model_id,
            "llama-3-8b-instruct"
        );
    }

    #[test]
    fn filter_is_case_insensitive() {
        let mut state = fixture();
        for c in "QWEN".chars() {
            state.push_filter(c);
        }
        assert_eq!(state.filtered().len(), 1);
    }

    #[test]
    fn empty_results_is_empty() {
        let state = ModelSelect::from_results(&[]);
        assert!(state.is_empty());
        assert!(state.selected().is_none());
    }

    #[test]
    fn transport_is_carried_through() {
        let state = fixture();
        let lm = state
            .filtered()
            .into_iter()
            .find(|e| e.provider_id == "lmstudio")
            .unwrap();
        assert_eq!(lm.transport, TransportType::OpenAiCompatible);
        assert_eq!(lm.endpoint, "http://localhost:1234");
    }

    /// A keyed (remote) provider result with one unverified model, for the
    /// consent/badge cases.
    fn remote_result() -> DiscoveryResult {
        DiscoveryResult {
            provider: Provider {
                id: "openrouter".into(),
                name: "OpenRouter".into(),
                endpoint: "https://openrouter.ai/api".into(),
                transport: TransportType::OpenAiCompatible,
                enabled: true,
                api_key: Some("sk-x".into()),
                api_key_env: Some("OPENROUTER_API_KEY".into()),
            },
            models: vec![Model::new(
                "openrouter",
                "qwen/qwen3-coder",
                Capabilities::discovery_default(),
            )],
        }
    }

    #[test]
    fn unverified_remote_model_reads_caps_unknown() {
        let state = ModelSelect::from_results(&[remote_result()]);
        let entry = state.filtered()[0];
        assert!(entry.caps_unknown());
        // Unverified models are assumed chat-only and badged in muted gray.
        assert_eq!(entry.badge(), "[chat]");
        // The key env-var name rides along for the auth hint.
        assert_eq!(entry.api_key_env.as_deref(), Some("OPENROUTER_API_KEY"));
    }

    #[test]
    fn set_caps_flips_verified_and_clears_the_badge() {
        let mut state = ModelSelect::from_results(&[remote_result()]);
        state.set_caps(
            "openrouter",
            "qwen/qwen3-coder",
            Capabilities {
                tool_use: true,
                ..Capabilities::discovery_default()
            },
        );
        let entry = state.filtered()[0];
        assert!(!entry.caps_unknown(), "now verified");
        assert_eq!(entry.badge(), "[tools]");
    }

    #[test]
    fn model_scope_hides_out_of_scope_models() {
        let results = vec![result(
            "ollama",
            "http://localhost:11434",
            TransportType::Ollama,
            &[("qwen3-coder:latest", true), ("llama3:8b", false)],
        )];
        let scope = ModelScope::List(vec!["qwen3-coder".into()]);
        let state = ModelSelect::from_results_scoped(&results, &scope);
        // Only the scoped model survives; the tag variant matches its base.
        assert_eq!(state.filtered().len(), 1);
        assert_eq!(state.filtered()[0].model.model_id, "qwen3-coder:latest");
    }

    #[test]
    fn focus_provider_expands_and_homes_on_its_first_model() {
        let mut state = fixture();
        state.focus_provider("lmstudio");
        // The preferred provider opens with a model under the cursor.
        assert_eq!(
            state.selected().unwrap().model.model_id,
            "llama-3-8b-instruct"
        );
    }

    #[test]
    fn selected_line_tracks_the_cursor_through_the_rendered_lines() {
        let mut state = fixture();
        // Collapsed rows: [ollama(0), lmstudio(1), add(2)]. The add row is drawn
        // after a blank spacer, so its highlighted line is one past its index.
        assert_eq!(state.selected_line(), 0);
        state.move_down();
        assert_eq!(state.selected_line(), 1);
        state.move_down(); // onto the add row
        assert!(state.is_add_provider_selected());
        assert_eq!(
            state.selected_line(),
            3,
            "add row sits after a blank spacer"
        );

        // Expand ollama: lines become [ollama, qwen, llama3, lmstudio, _, add].
        state.clear_filter(); // cursor back to the first header
        state.toggle_selected_provider();
        state.move_down();
        assert_eq!(state.selected_line(), 1, "first model under its header");
    }

    #[test]
    fn empty_picker_selects_the_add_row_below_the_notice() {
        // Only the add row exists, preceded by the empty-state notice and a
        // blank spacer: notice(0), blank(1), add(2).
        let state = ModelSelect::from_results(&[]);
        assert!(state.is_add_provider_selected());
        assert_eq!(state.selected_line(), 2);
    }

    #[test]
    fn scroll_offset_keeps_the_cursor_in_view() {
        // Everything fits: no scroll.
        assert_eq!(scroll_offset(0, 5, 10), 0);
        assert_eq!(scroll_offset(4, 5, 10), 0);
        // Cursor within the first viewport stays pinned to the top.
        assert_eq!(scroll_offset(5, 40, 10), 0);
        assert_eq!(scroll_offset(9, 40, 10), 0);
        // Past the bottom edge, scroll just enough to keep the cursor visible.
        assert_eq!(scroll_offset(10, 40, 10), 1);
        assert_eq!(scroll_offset(20, 40, 10), 11);
        // Never past the final line (max offset = total - viewport).
        assert_eq!(scroll_offset(39, 40, 10), 30);
        // A zero-height viewport is a no-op rather than a panic.
        assert_eq!(scroll_offset(5, 40, 0), 0);
    }

    #[test]
    fn clear_filter_resets_filter_and_collapses() {
        let mut state = fixture();
        for c in "instruct".chars() {
            state.push_filter(c);
        }
        assert_eq!(state.filtered().len(), 1);
        state.clear_filter();
        assert!(state.filter.is_empty());
        assert_eq!(state.filtered().len(), 3);
        // Collapsed again: the cursor rests on the first provider header.
        assert!(state.selected().is_none());
        assert!(!state.is_add_provider_selected());
    }

    /// A skeleton provider (no models), as the plan hands them to `from_plan`.
    fn skel(id: &str) -> Provider {
        Provider {
            id: id.into(),
            name: id.into(),
            endpoint: format!("http://localhost/{id}"),
            transport: TransportType::Ollama,
            enabled: true,
            api_key: None,
            api_key_env: None,
        }
    }

    #[test]
    fn from_plan_opens_every_provider_checking_and_inert() {
        let plan = vec![skel("ollama"), skel("lmstudio")];
        let state = ModelSelect::from_plan(&plan, &ModelScope::All);
        // Both providers show as headers, none selectable (no models yet) and
        // none expandable while their probe is still in flight.
        for group in &state.groups {
            assert_eq!(group.status, ProviderStatus::Checking);
            assert!(!group.is_expandable(), "a checking row is inert");
        }
        assert!(state.selected().is_none());
    }

    #[test]
    fn online_outcome_populates_models_offline_stays_inert() {
        let plan = vec![skel("ollama"), skel("lmstudio")];
        let mut state = ModelSelect::from_plan(&plan, &ModelScope::All);

        // ollama comes online with two models; lmstudio's port is dead.
        state.apply_outcome(&ProbeOutcome::Online(result(
            "ollama",
            "http://localhost:11434",
            TransportType::Ollama,
            &[("qwen3-coder:latest", true), ("llama3:8b", false)],
        )));
        state.apply_outcome(&ProbeOutcome::Offline {
            id: "lmstudio".into(),
        });

        let ollama = state.groups.iter().find(|g| g.id == "ollama").unwrap();
        assert_eq!(ollama.status, ProviderStatus::Online);
        assert!(ollama.is_expandable());
        assert_eq!(ollama.models.len(), 2);

        let lm = state.groups.iter().find(|g| g.id == "lmstudio").unwrap();
        assert_eq!(lm.status, ProviderStatus::Offline);
        assert!(!lm.is_expandable(), "an offline row never expands");

        // Expanding the online provider reveals a model; the offline one can't.
        assert!(state.toggle_selected_provider(), "ollama header toggles");
        state.move_down();
        assert_eq!(
            state.selected().unwrap().model.model_id,
            "qwen3-coder:latest"
        );
    }

    #[test]
    fn enter_on_an_offline_header_reprobes_in_the_model_list() {
        let plan = vec![skel("lmstudio")];
        let mut state = ModelSelect::from_plan(&plan, &ModelScope::All);
        state.apply_outcome(&ProbeOutcome::Offline {
            id: "lmstudio".into(),
        });

        // Cursor on the (offline) header: it can't expand, so Enter re-probes.
        assert!(
            !state.toggle_selected_provider(),
            "an offline header doesn't expand"
        );
        let entry = state
            .begin_reprobe_selected()
            .expect("an offline header yields a re-probe entry");
        assert_eq!(entry.id, "lmstudio");
        // The header flips to checking until the fresh outcome lands.
        let group = state.groups.iter().find(|g| g.id == "lmstudio").unwrap();
        assert_eq!(group.status, ProviderStatus::Checking);
    }

    #[test]
    fn reprobe_is_for_inert_headers_only() {
        let plan = vec![skel("ollama")];
        let mut state = ModelSelect::from_plan(&plan, &ModelScope::All);
        // A probe is already in flight (checking): Enter does not re-probe.
        assert!(state.begin_reprobe_selected().is_none());

        // Online with a model: the header expands instead of re-probing…
        state.apply_outcome(&ProbeOutcome::Online(result(
            "ollama",
            "http://localhost:11434",
            TransportType::Ollama,
            &[("qwen3-coder:latest", true)],
        )));
        assert!(
            state.begin_reprobe_selected().is_none(),
            "online header expands"
        );
        // …and a model row is a selection target, not a re-probe target.
        state.toggle_selected_provider();
        state.move_down();
        assert!(state.selected().is_some());
        assert!(
            state.begin_reprobe_selected().is_none(),
            "a model row is not re-probed"
        );
    }

    #[test]
    fn reprobe_back_online_repopulates_models() {
        let plan = vec![skel("ollama")];
        let mut state = ModelSelect::from_plan(&plan, &ModelScope::All);
        state.apply_outcome(&ProbeOutcome::Offline {
            id: "ollama".into(),
        });
        // Retry it…
        assert!(state.begin_reprobe_selected().is_some());
        // …and the fresh online outcome (the same path the event loop drives)
        // repopulates the group's models.
        state.apply_outcome(&ProbeOutcome::Online(result(
            "ollama",
            "http://localhost:11434",
            TransportType::Ollama,
            &[("qwen3-coder:latest", true)],
        )));
        let group = state.groups.iter().find(|g| g.id == "ollama").unwrap();
        assert_eq!(group.status, ProviderStatus::Online);
        assert_eq!(group.models.len(), 1);
        assert!(group.is_expandable());
    }

    #[test]
    fn connection_issue_outcome_is_inert_and_red() {
        let plan = vec![skel("ollama-mac")];
        let mut state = ModelSelect::from_plan(&plan, &ModelScope::All);
        state.apply_outcome(&ProbeOutcome::ConnectionIssue {
            id: "ollama-mac".into(),
        });
        let group = &state.groups[0];
        assert_eq!(group.status, ProviderStatus::ConnectionIssue);
        assert!(!group.is_expandable());
        // The header renders its status in the danger colour.
        let (_, label, color) = status_display(group.status);
        assert_eq!(label, "connection issue");
        assert_eq!(color, theme::DANGER);
    }

    #[test]
    fn apply_outcome_honors_scope() {
        let plan = vec![skel("ollama")];
        let scope = ModelScope::List(vec!["qwen3-coder".into()]);
        let mut state = ModelSelect::from_plan(&plan, &scope);
        state.apply_outcome(&ProbeOutcome::Online(result(
            "ollama",
            "http://localhost:11434",
            TransportType::Ollama,
            &[("qwen3-coder:latest", true), ("llama3:8b", false)],
        )));
        // Only the in-scope model survives the streamed fold.
        assert_eq!(state.groups[0].models.len(), 1);
        assert_eq!(
            state.groups[0].models[0].model.model_id,
            "qwen3-coder:latest"
        );
    }
}
