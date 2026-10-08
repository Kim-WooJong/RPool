//! What one provider card shows, gathered from the discovered remotes.

use crate::gui::state::GuiState;
use crate::gui::widgets::StatusTone;
use crate::models::QuotaReport;

/// Data of one provider card; built by `cards`, drawn by `card::show`.
#[derive(Debug, Clone)]
pub(crate) struct ProviderCard<'a> {
    /// Backing remote name.
    pub(crate) name: &'a str,
    /// Backend type, e.g. `drive` or `dropbox`.
    pub(crate) kind: Option<&'a str>,
    /// Crypt remotes that store their data in this provider.
    pub(crate) crypts: &'a [String],
    /// Last usage report of the account, if any.
    pub(crate) report: Option<&'a QuotaReport>,
    /// Encryption status text (`encryption_status`).
    pub(crate) status: &'static str,
    /// Tone of the status badge.
    pub(crate) tone: StatusTone,
    /// Encryption is missing: offer manual setup for this provider.
    pub(crate) missing: bool,
    /// Default path RPool uses on this provider (`remote_roots`), if set.
    pub(crate) location: Option<&'a str>,
    /// Upload budget, pause and activity of this account.
    pub(crate) limits: Option<&'a crate::provider::limits_view::AccountStatus>,
}

impl ProviderCard<'_> {
    /// Used share of the account, when the provider reports a total.
    pub(crate) fn ratio(&self) -> Option<f32> {
        let report = self.report?;
        match (report.used, report.total) {
            (Some(used), Some(total)) if total > 0 => {
                Some((used as f64 / total as f64).clamp(0.0, 1.0) as f32)
            }
            _ => None,
        }
    }
}

/// One card per backing provider, in the discovered order. `running` is
/// whether automatic encryption setup is running now.
pub(crate) fn cards(state: &GuiState, running: bool) -> Vec<ProviderCard<'_>> {
    let form = &state.providers;
    let scope = pool_scope(state);
    state
        .backing_remotes
        .iter()
        .filter(|name| {
            scope
                .as_ref()
                .is_none_or(|names| names.contains(name.as_str()))
        })
        .map(|name| {
            let missing = form.missing_encryption.contains(name);
            let status = super::encryption_status(
                form.discovery_known,
                missing,
                running,
                form.encryption_failed,
            );
            let tone = if !form.discovery_known {
                StatusTone::Neutral
            } else if !missing {
                StatusTone::Success
            } else if running {
                StatusTone::Info
            } else {
                StatusTone::Warning
            };
            ProviderCard {
                name,
                kind: state.provider_details.kinds.get(name).map(String::as_str),
                location: state.remote_roots.get(name).map(String::as_str),
                crypts: state
                    .provider_details
                    .crypts
                    .get(name)
                    .map_or(&[], Vec::as_slice),
                report: state
                    .usage_reports
                    .iter()
                    .find(|report| report_account(&report.remote) == name),
                status,
                tone,
                missing: form.discovery_known && missing,
                limits: form.limits.row(name),
            }
        })
        .collect()
}

/// With a pool chosen as the health monitor's scope, the providers that
/// pool uses: the backing remotes of its crypt remotes (or the remote itself
/// when it is a backing remote). `None` = no pool scope, show every provider.
pub(crate) fn pool_scope(state: &GuiState) -> Option<std::collections::BTreeSet<&str>> {
    let pool = state.providers.health_pool.trim();
    if pool.is_empty() {
        return None;
    }
    let definition = state.pool_definitions.get(pool)?;
    let name = |remote: &str| remote.split_once(':').map_or(remote, |(n, _)| n).to_owned();
    let used: std::collections::BTreeSet<String> =
        definition.remotes.iter().map(|r| name(r)).collect();
    Some(
        state
            .backing_remotes
            .iter()
            .filter(|backing| {
                used.contains(backing.as_str())
                    || state
                        .provider_details
                        .crypts
                        .get(backing.as_str())
                        .is_some_and(|crypts| crypts.iter().any(|c| used.contains(&name(c))))
            })
            .map(String::as_str)
            .collect(),
    )
}

/// Account name of a capacity report. Reports name the remote with its colon
/// and, for backends whose quota depends on the folder (SFTP, WebDAV, …),
/// with the provider's default path too: `koofr_1:/data` belongs to `koofr_1`.
fn report_account(remote: &str) -> &str {
    remote.split_once(':').map_or(remote, |(name, _)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pool_scope_shows_only_that_pools_providers() {
        let mut state = GuiState::new(Default::default(), Default::default(), Default::default());
        super::super::sample::providers(&mut state);
        state.pool_definitions.insert(
            "family".into(),
            crate::models::PoolDefinition {
                remotes: vec!["gdrive_2_crypt:".into(), "pcloud_crypt:rpool".into()],
                ..Default::default()
            },
        );
        assert_eq!(cards(&state, false).len(), 7);
        state.providers.health_pool = "family".into();
        let names: Vec<&str> = cards(&state, false).iter().map(|c| c.name).collect();
        assert_eq!(names, ["gdrive_2", "pcloud"]);
        // An unknown pool (e.g. just removed) falls back to every provider.
        state.providers.health_pool = "gone".into();
        assert_eq!(cards(&state, false).len(), 7);
    }

    #[test]
    fn cards_join_kind_crypts_usage_and_status() {
        let mut state = GuiState::new(Default::default(), Default::default(), Default::default());
        super::super::sample::providers(&mut state);
        let shown = cards(&state, false);
        assert_eq!(shown.len(), state.backing_remotes.len());
        let first = &shown[0];
        assert_eq!(first.name, "gdrive_1");
        assert_eq!(first.kind, Some("drive"));
        assert_eq!(first.crypts, ["gdrive_1_crypt:".to_string()]);
        assert!(first.report.is_some());
        assert!(first.ratio().unwrap() > 0.0);
        assert_eq!(first.tone, StatusTone::Success);
        let pending = shown.iter().find(|c| c.missing).unwrap();
        assert_eq!(pending.tone, StatusTone::Warning);
        assert!(pending.crypts.is_empty());
        // A provider that cannot report capacity has no ratio.
        let unknown = shown.iter().find(|c| c.name == "box_archive").unwrap();
        assert!(unknown.report.unwrap().error.is_some());
        assert_eq!(unknown.ratio(), None);

        // A provider with its own default path still finds its report.
        let path_report = state
            .usage_reports
            .iter_mut()
            .find(|r| r.remote == "gdrive_1:")
            .unwrap();
        path_report.remote = "gdrive_1:/data".into();
        assert!(cards(&state, false)[0].report.is_some());
        assert_eq!(report_account("koofr_1:/data/rpool"), "koofr_1");
        assert_eq!(report_account("koofr_1:"), "koofr_1");

        state.providers.discovery_known = false;
        let shown = cards(&state, false);
        assert!(shown
            .iter()
            .all(|c| c.tone == StatusTone::Neutral && !c.missing));
    }
}
