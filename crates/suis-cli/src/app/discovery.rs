//! Live provider-discovery state for the session.
//!
//! Startup no longer blocks on discovery: the slowest probe (a sleeping LAN
//! host eats the full connect timeout) used to gate the whole UI behind it.
//! Instead the picker opens immediately on a *skeleton* of every provider that
//! will be probed ([`DiscoveryState::planning`]), each row reading "checking…",
//! and a background task streams [`ProbeOutcome`]s in
//! ([`DiscoveryState::apply`]) so a responsive provider resolves in
//! milliseconds while a dead one resolves later — never holding up the UI.
//!
//! This is the single source of truth the model picker and `/providers` rebuild
//! from; the model picker also folds outcomes in-place so it can update a row's
//! status without disturbing the cursor or expansion.

use std::collections::{BTreeMap, HashSet};

use suis_core::{ProviderConfig, ProviderEntry};
use suis_providers::{
    DiscoveryResult, ProbeOutcome, Provider, ProviderIssue, ProviderRegistry, ProviderStatus,
};

/// Everything known about provider discovery this session: the providers that
/// were (or are being) probed, each one's current status, the online results
/// (with their models), the credential rejections, and any config load issues.
#[derive(Debug, Clone, Default)]
pub struct DiscoveryState {
    /// Every provider that will be probed (built-in defaults ∪ enabled config
    /// entries), as a display-ready skeleton — present before any probe lands.
    pub planned: Vec<Provider>,
    /// Each planned provider's current status, keyed by id. A provider starts
    /// [`Checking`](ProviderStatus::Checking) and resolves as its probe reports.
    pub statuses: BTreeMap<String, ProviderStatus>,
    /// The online providers (those that answered), carrying their model lists.
    pub results: Vec<DiscoveryResult>,
    /// Ids that answered but rejected the credentials (401/403).
    pub auth_failed: HashSet<String>,
    /// Configuration problems found while planning (unknown transport, bad
    /// endpoint, duplicate id), surfaced in `/providers`.
    pub issues: Vec<ProviderIssue>,
}

impl DiscoveryState {
    /// The initial skeleton for `config`: every planned provider marked
    /// [`Checking`](ProviderStatus::Checking), no results yet. The picker renders
    /// this the instant the app opens, then [`apply`](Self::apply) resolves rows.
    pub fn planning(config: &ProviderConfig) -> Self {
        let (planned, issues) = ProviderRegistry::plan(config);
        let statuses = planned
            .iter()
            .map(|p| (p.id.clone(), ProviderStatus::Checking))
            .collect();
        DiscoveryState {
            planned,
            statuses,
            results: Vec::new(),
            auth_failed: HashSet::new(),
            issues,
        }
    }

    /// Fold one streamed probe outcome in: update the provider's status and, for
    /// an online provider, record (replacing any prior) its result; for an auth
    /// failure, remember the id. A `Checking` row not present in `planned`
    /// (shouldn't happen) is still recorded so its status is never lost.
    pub fn apply(&mut self, outcome: ProbeOutcome) {
        let id = outcome.id().to_string();
        self.statuses.insert(id.clone(), outcome.status());
        self.auth_failed.remove(&id);
        self.results.retain(|r| r.provider.id != id);
        match outcome {
            ProbeOutcome::Online(result) => self.results.push(result),
            ProbeOutcome::AuthFailed { id, .. } => {
                self.auth_failed.insert(id);
            }
            ProbeOutcome::Offline { .. }
            | ProbeOutcome::Unparsable { .. }
            | ProbeOutcome::ConnectionIssue { .. } => {}
        }
    }

    /// The ids that answered online, for the providers-screen merge.
    pub fn online_ids(&self) -> HashSet<String> {
        self.results.iter().map(|r| r.provider.id.clone()).collect()
    }

    /// The merged provider list (live discovery ∪ stored config) for
    /// `/providers`, so a stored-but-offline provider still appears.
    pub fn merged(&self, stored: &[ProviderEntry]) -> Vec<Provider> {
        ProviderRegistry::merge(&self.results, stored)
    }

    /// Every known provider id (planned ∪ already-resolved), for the add-form's
    /// uniqueness check.
    pub fn known_ids(&self) -> Vec<String> {
        self.planned.iter().map(|p| p.id.clone()).collect()
    }

    /// Forget a provider that was removed in-session. Persistence has already
    /// dropped (or disabled) the entry; this clears the live discovery snapshot
    /// too so the add-form's uniqueness check does not keep treating a deleted
    /// custom id as taken until restart.
    pub fn remove_provider(&mut self, id: &str) {
        self.planned.retain(|p| p.id != id);
        self.statuses.remove(id);
        self.results.retain(|r| r.provider.id != id);
        self.auth_failed.remove(id);
        self.issues.retain(|issue| issue.id != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use suis_core::ProviderEntry;
    use suis_providers::{Capabilities, Model, TransportType};

    fn config(ids: &[&str]) -> ProviderConfig {
        ProviderConfig {
            providers: ids
                .iter()
                .map(|id| ProviderEntry {
                    id: (*id).into(),
                    endpoint: format!("http://localhost/{id}"),
                    transport: "ollama".into(),
                    enabled: true,
                    name: None,
                    api_key_env: None,
                    api_key: None,
                })
                .collect(),
        }
    }

    fn online_result(id: &str) -> DiscoveryResult {
        DiscoveryResult {
            provider: Provider {
                id: id.into(),
                name: id.into(),
                endpoint: format!("http://localhost/{id}"),
                transport: TransportType::Ollama,
                enabled: true,
                api_key: None,
                api_key_env: None,
            },
            models: vec![Model::new(id, "m1", Capabilities::discovery_default())],
        }
    }

    #[test]
    fn planning_marks_everything_checking() {
        let state = DiscoveryState::planning(&config(&["a", "b"]));
        assert!(state
            .statuses
            .values()
            .all(|s| *s == ProviderStatus::Checking));
        assert!(state.results.is_empty());
    }

    #[test]
    fn apply_resolves_status_and_records_results() {
        let mut state = DiscoveryState::planning(&config(&["a", "b"]));
        state.apply(ProbeOutcome::Online(online_result("a")));
        state.apply(ProbeOutcome::ConnectionIssue { id: "b".into() });

        assert_eq!(state.statuses["a"], ProviderStatus::Online);
        assert_eq!(state.statuses["b"], ProviderStatus::ConnectionIssue);
        assert_eq!(state.online_ids(), ["a".to_string()].into_iter().collect());
        assert_eq!(state.results.len(), 1);
    }

    #[test]
    fn apply_replaces_a_prior_result_for_the_same_id() {
        // A re-probe (e.g. after editing a provider) must not duplicate rows.
        let mut state = DiscoveryState::planning(&config(&["a"]));
        state.apply(ProbeOutcome::Online(online_result("a")));
        state.apply(ProbeOutcome::Offline { id: "a".into() });
        assert_eq!(state.statuses["a"], ProviderStatus::Offline);
        assert!(
            state.results.is_empty(),
            "the stale online result is dropped"
        );
        assert!(state.online_ids().is_empty());
    }

    #[test]
    fn removing_provider_forgets_live_discovery_state() {
        let mut state = DiscoveryState::planning(&config(&["work"]));
        state.apply(ProbeOutcome::Online(online_result("work")));
        state.auth_failed.insert("work".into());
        state.issues.push(ProviderIssue {
            id: "work".into(),
            field: "id".into(),
            reason: "duplicate id".into(),
        });

        state.remove_provider("work");

        assert!(
            !state.known_ids().contains(&"work".to_string()),
            "deleted id is available to the add form again"
        );
        assert!(!state.statuses.contains_key("work"));
        assert!(state.results.is_empty());
        assert!(!state.auth_failed.contains("work"));
        assert!(state.issues.is_empty());
        assert!(
            state.merged(&[]).is_empty(),
            "stale live result must not keep the provider visible"
        );
    }
}
