//! The account-limit rows of the provider cards, re-read from the shared
//! usage ledger and the limits file every few seconds (mounts and CLI runs
//! update them from other processes).
use crate::gui::state::GuiState;
use crate::provider::limits_view::{self, AccountRef, AccountStatus};
use crate::storage::account::limits::LimitsStore;
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
pub(crate) struct LimitsCache {
    loaded: Option<Instant>,
    pub(crate) store: LimitsStore,
    pub(crate) rows: Vec<AccountStatus>,
    pub(crate) error: Option<String>,
    /// Sample data (tests, snapshots): never re-read from disk.
    pub(crate) frozen: bool,
}

impl LimitsCache {
    pub(crate) fn row(&self, account: &str) -> Option<&AccountStatus> {
        self.rows.iter().find(|row| row.account == account)
    }
    /// Re-reads the next time the providers page is drawn.
    pub(crate) fn invalidate(&mut self) {
        self.loaded = None;
    }
}

/// The accounts the cards show, with their backend types and crypts.
pub(crate) fn accounts(state: &GuiState) -> Vec<AccountRef> {
    state
        .backing_remotes
        .iter()
        .map(|name| AccountRef {
            name: name.clone(),
            kind: state
                .provider_details
                .kinds
                .get(name)
                .cloned()
                .unwrap_or_default(),
            crypts: state
                .provider_details
                .crypts
                .get(name)
                .cloned()
                .unwrap_or_default(),
        })
        .collect()
}

/// Reloads when due. Unit tests never read the user's config directory.
pub(crate) fn refresh(state: &mut GuiState) {
    let cache = &state.providers.limits;
    if cfg!(test) || cache.frozen || cache.loaded.is_some_and(|at| at.elapsed() < REFRESH) {
        return;
    }
    let accounts = accounts(state);
    let (store, mut error) = match crate::storage::account::store::load_limits() {
        Ok(store) => (store, None),
        Err(e) => (LimitsStore::default(), Some(format!("{e:#}"))),
    };
    let ledger = match crate::storage::account::runtime::ledger().map(|l| l.load()) {
        Some(Ok(data)) => data,
        Some(Err(e)) => {
            error.get_or_insert(format!("{e:#}"));
            Default::default()
        }
        None => Default::default(),
    };
    let rows = limits_view::build(&accounts, &ledger, &store, crate::utils::now_unix());
    let cache = &mut state.providers.limits;
    cache.store = store;
    cache.rows = rows;
    cache.error = error;
    cache.loaded = Some(Instant::now());
}
