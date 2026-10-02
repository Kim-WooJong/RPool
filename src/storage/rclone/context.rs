//! rclone context construction, traffic attribution and per-remote command routing.

use super::*;

impl RcloneContext {
    /// Context for `executable` with `config`, snapshotting the current process
    /// environment. The read daemon is disabled under `cfg(test)`.
    pub(crate) fn new(executable: PathBuf, config: ConfigSelection) -> Self {
        Self {
            executable,
            config,
            environment: std::env::vars_os().collect(),
            daemon_allowed: !cfg!(test),
            traffic_alias: None,
        }
    }
    /// Counts traffic on remote `base` (a remote name) for `alias`.
    pub(crate) fn attribute_traffic(&mut self, base: &str, alias: &str) {
        self.traffic_alias = Some((base.to_owned(), alias.to_owned()));
    }
    /// The remote name `address`'s traffic is counted for.
    pub(super) fn meter_name(&self, address: &str) -> Option<String> {
        let name = remote_name(address).ok()?;
        Some(match &self.traffic_alias {
            Some((base, alias)) if base == name => alias.clone(),
            _ => name.to_owned(),
        })
    }
    /// Starts a traffic op for `address`; uploads/downloads are also paced by the
    /// global and (if it has a bandwidth limit) per-account pacer keys.
    pub(super) fn op(&self, address: &str, direction: traffic::Direction) -> traffic::Op {
        let op = traffic::Op::begin(self.meter_name(address).as_deref(), direction);
        let upload = match direction {
            traffic::Direction::Upload => true,
            traffic::Direction::Download => false,
            traffic::Direction::Other => return op,
        };
        let settings = crate::storage::account::runtime::settings();
        let mut keys = vec![pacer::Key::Global { upload }];
        if settings.has_account_overrides() {
            if let Some((name, _)) = self.account_of(address) {
                if settings
                    .store
                    .account(&name)
                    .is_some_and(|l| l.bwlimit.is_some())
                {
                    keys.push(pacer::Key::Account { name, upload });
                }
            }
        }
        op.paced(keys)
    }
    /// (account, backend type) of `address` (bottom of its crypt/alias chain).
    pub(super) fn account_of(&self, address: &str) -> Option<(String, String)> {
        let ctx = OperationContext::with_deadline(Instant::now() + Duration::from_secs(30));
        self.write_lane(&ctx, address)
    }
    /// rclone `--tpslimit` of the account behind `address`, when it has one.
    pub(super) fn tpslimit(&self, address: &str) -> Option<f64> {
        let settings = crate::storage::account::runtime::settings();
        if !settings.has_account_overrides() {
            return None;
        }
        settings.tpslimit(&self.account_of(address)?.0)
    }
    /// The shared read daemon, unless the account behind `address` has a
    /// request-rate limit: rclone applies `--tpslimit` per process, so those
    /// reads use subprocesses carrying the flag.
    pub(super) fn daemon_for(&self, address: &str) -> Option<std::sync::Arc<daemon::Daemon>> {
        if self.tpslimit(address).is_some() {
            return None;
        }
        daemon::get(self)
    }
    /// Which rclone binary and config resolve addresses (paths only, no
    /// secrets): the same address under another config is another object.
    pub(crate) fn route_identity(&self) -> String {
        let config = match &self.config {
            ConfigSelection::Inherited => self
                .environment
                .iter()
                .find(|(key, _)| key == "RCLONE_CONFIG")
                .map(|(_, value)| PathBuf::from(value)),
            ConfigSelection::File(path) => Some(path.clone()),
        };
        format!("{:?}|{:?}", self.executable, config)
    }
    /// Context using the rclone config selected by the environment (no `--config`).
    pub(crate) fn inherited(executable: &str) -> Self {
        Self::new(executable.into(), ConfigSelection::Inherited)
    }
    /// The rclone command with exactly this context's executable, config and
    /// environment (also what the read daemon starts with).
    pub(super) fn base_command(&self, args: &[OsString]) -> Command {
        let mut command = Command::new(&self.executable);
        command.env_clear().envs(self.environment.iter().cloned());
        if let ConfigSelection::File(path) = &self.config {
            command.arg("--config").arg(path);
        }
        command.args(args);
        command
    }
    /// [`Self::base_command`] plus the global bandwidth timetable, which rclone
    /// switches itself during long transfers (per rclone process; RPool's own
    /// pacer additionally caps the sum of the bytes it pipes).
    pub(super) fn command(&self, args: &[OsString]) -> Command {
        let mut command = self.base_command(&[]);
        if let Some(timetable) = &crate::storage::account::runtime::settings().store.bandwidth {
            command.arg("--bwlimit").arg(timetable);
        }
        command.args(args);
        command
    }
    /// [`Self::command`] plus per-account flags of the account behind
    /// `address`, for a call in `lane`.
    pub(super) fn command_for(&self, address: &str, args: &[OsString], lane: TpsLane) -> Command {
        let mut command = self.command(&[]);
        if let Some(tps) = self.process_tpslimit(address, lane) {
            command.arg("--tpslimit").arg(tps.to_string());
        }
        command.args(args);
        command
    }
    /// The account's requests per second split over the calls that may run
    /// at once in `lane`: rclone applies `--tpslimit` per process, and every
    /// shard transfer is its own process, so the account as a whole stays at
    /// its limit (uploads and downloads each).
    fn process_tpslimit(&self, address: &str, lane: TpsLane) -> Option<f64> {
        let total = self.tpslimit(address)?;
        let settings = crate::storage::account::runtime::settings();
        let (account, kind) = self.account_of(address)?;
        let dropbox = kind == "dropbox";
        // The speed test measures a level as if it were the cap.
        if let Some(calls) = limit::tuning_calls() {
            return Some(split_tps(total, calls));
        }
        let calls = match lane {
            TpsLane::Upload => settings
                .upload_cap(&account, dropbox)
                .unwrap_or_else(|| limit::default_write_cap(dropbox)),
            TpsLane::Download => settings
                .download_cap(&account)
                .unwrap_or_else(limit::general_cap),
            TpsLane::Other => limit::general_cap(),
        };
        Some(split_tps(total, calls))
    }
    #[cfg(test)]
    pub(crate) fn set_test_environment(&mut self, key: &str, value: &str) {
        self.environment.retain(|(k, _)| k != key);
        self.environment.push((key.into(), value.into()));
    }
    #[cfg(test)]
    pub(crate) fn allow_daemon_for_test(&mut self) {
        self.daemon_allowed = true;
    }
}

/// Which concurrency cap a call runs under (for splitting `--tpslimit`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TpsLane {
    /// Shard upload; capped by the account's upload concurrency.
    Upload,
    /// Shard download; capped by the account's download concurrency.
    Download,
    /// Any other call (listing, about, admin); capped by the general cap.
    Other,
}

/// `total` requests per second shared by up to `calls` processes (`0` =
/// uncapped: no split is possible).
pub(super) fn split_tps(total: f64, calls: usize) -> f64 {
    if calls == 0 {
        total
    } else {
        total / calls as f64
    }
}

#[cfg(test)]
mod tps_tests {
    #[test]
    fn account_rate_is_split_over_its_simultaneous_calls() {
        assert_eq!(super::split_tps(8.0, 4), 2.0);
        assert_eq!(super::split_tps(8.0, 1), 8.0);
        assert_eq!(super::split_tps(1.0, 4), 0.25);
        assert_eq!(super::split_tps(5.0, 0), 5.0, "uncapped");
    }
}
