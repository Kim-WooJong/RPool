//! Polled state of the Monitoring page: the running mounts (every 2 s),
//! their live status (every 1 s), the live samples of the last minutes per
//! account, and per card the selected tab and history range.
use super::history::{HistoryState, Range};
use super::ring::{Ring, Sample};
#[cfg(not(test))]
use super::source::LiveSource;
use super::source::MonitorSource;
use crate::monitor::model::{MountEntry, NetStatus};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the list of running mounts is re-read.
pub(crate) const MOUNTS_EVERY: Duration = Duration::from_secs(2);
/// How often each mount's live status is re-read (also the app's wake-up
/// interval while a pool is mounted).
pub(crate) const STATUS_EVERY: Duration = Duration::from_secs(1);

/// Tab shown on a mount card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CardTab {
    /// Live rates and sparklines (the default).
    #[default]
    Live,
    /// History bar charts.
    History,
}

/// One running mount on the Monitoring page with its live data and card state.
pub(crate) struct MountLive {
    /// Registry entry of the mount.
    pub entry: MountEntry,
    /// Last status read (`None` until one could be read).
    pub status: Option<NetStatus>,
    /// Live samples per remote address.
    pub rings: BTreeMap<String, Ring>,
    /// Selected card tab.
    pub tab: CardTab,
    /// Selected history range.
    pub range: Range,
    /// History chart of `range` and its loading state.
    pub history: HistoryState,
}

impl MountLive {
    /// A mount just found, with empty live data and default tab and range.
    fn new(entry: MountEntry) -> Self {
        Self {
            entry,
            status: None,
            rings: BTreeMap::new(),
            tab: CardTab::default(),
            range: Range::default(),
            history: HistoryState::default(),
        }
    }

    /// Stores a newly read status and appends its rates to the rings.
    pub(crate) fn record(&mut self, status: Option<NetStatus>) {
        if let Some(status) = &status {
            for remote in &status.remotes {
                self.rings
                    .entry(remote.remote.clone())
                    .or_default()
                    .push(Sample {
                        unix: status.updated_unix,
                        up: remote.upload_rate_10s,
                        down: remote.download_rate_10s,
                    });
            }
        }
        self.status = status;
    }
}

/// State of the Monitoring page, in `GuiState::monitoring`; polled by
/// `monitoring::show` and the app.
pub(crate) struct MonitoringState {
    /// Where mounts, statuses and history come from.
    pub source: Arc<dyn MonitorSource>,
    /// Running mounts, in the order the source lists them.
    pub mounts: Vec<MountLive>,
    /// When the mount list was last read.
    mounts_read: Option<Instant>,
    /// When the statuses were last read.
    status_read: Option<Instant>,
}

impl Default for MonitoringState {
    /// The mounts of this PC; tests never read the real registry.
    fn default() -> Self {
        #[cfg(not(test))]
        let source: Arc<dyn MonitorSource> = Arc::new(LiveSource);
        #[cfg(test)]
        let source: Arc<dyn MonitorSource> = Arc::new(super::source::FixedSource::default());
        Self::with_source(source)
    }
}

impl MonitoringState {
    /// State reading from `source`; nothing is read until the first `poll`.
    pub(crate) fn with_source(source: Arc<dyn MonitorSource>) -> Self {
        Self {
            source,
            mounts: Vec::new(),
            mounts_read: None,
            status_read: None,
        }
    }

    /// Re-reads what is due at `now`. Cheap: a registry folder and one small
    /// JSON file per mount.
    pub(crate) fn poll(&mut self, now: Instant) {
        if self
            .mounts_read
            .is_none_or(|at| now.duration_since(at) >= MOUNTS_EVERY)
        {
            self.mounts_read = Some(now);
            self.reconcile(self.source.active_mounts());
        }
        if self
            .status_read
            .is_none_or(|at| now.duration_since(at) >= STATUS_EVERY)
        {
            self.status_read = Some(now);
            for mount in &mut self.mounts {
                let status = self.source.read_status(&mount.entry);
                mount.record(status);
            }
        }
        for mount in &mut self.mounts {
            mount.history.poll(now);
        }
    }

    /// Keeps cards (and their samples) of mounts still running, adds new
    /// ones in registry order and drops ended ones.
    pub(crate) fn reconcile(&mut self, entries: Vec<MountEntry>) {
        let mut old: Vec<MountLive> = std::mem::take(&mut self.mounts);
        for entry in entries {
            let mount = match old.iter().position(|m| m.entry.id == entry.id) {
                Some(index) => {
                    let mut mount = old.swap_remove(index);
                    mount.entry = entry;
                    mount
                }
                None => {
                    let mut mount = MountLive::new(entry);
                    // Show a new mount right away instead of after 1 s.
                    let status = self.source.read_status(&mount.entry);
                    mount.record(status);
                    mount
                }
            };
            self.mounts.push(mount);
        }
    }

    /// Starts a background history load of card `index` when its shown
    /// range is missing or older than the refresh interval.
    pub(crate) fn ensure_history(&mut self, index: usize, now: Instant, now_unix: u64) {
        let source = self.source.clone();
        let Some(mount) = self.mounts.get_mut(index) else {
            return;
        };
        if mount.history.needs_load(mount.range, now) {
            let workspace = PathBuf::from(&mount.entry.workspace);
            mount
                .history
                .start(source, workspace, mount.range, now_unix);
        }
    }

    /// Loads the history of every card synchronously (fixtures and tests).
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn load_history_now(&mut self, now: Instant, now_unix: u64) {
        for mount in &mut self.mounts {
            let workspace = PathBuf::from(&mount.entry.workspace);
            mount
                .history
                .load_now(&*self.source, &workspace, mount.range, now_unix, now);
        }
    }

    /// Whether any account moves data right now (faster repaints for the
    /// "Uploading" pulse).
    pub(crate) fn busy(&self) -> bool {
        self.mounts.iter().any(|mount| {
            mount.status.as_ref().is_some_and(|status| {
                status.remotes.iter().any(|remote| {
                    super::view_model::activity(remote) != super::view_model::Activity::Idle
                })
            })
        })
    }
}
