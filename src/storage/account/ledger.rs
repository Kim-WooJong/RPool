//! Machine-local ledger of what this computer uploaded to each cloud account
//! (rolling 24 h in hourly buckets), when each remote last answered, and
//! provider-reported upload pauses.
//!
//! Several RPool processes (mounts, CLI runs, the GUI) share one account, so
//! the ledger is one JSON file guarded by a separate lock file: writers hold
//! an exclusive lock for read-modify-replace, readers a shared lock. The
//! lock file is never replaced, so the lock stays valid across the atomic
//! replacement of the data file.
use crate::prelude::*;
use crate::utils::{append_suffix, read_json, save_json_atomic};

pub(crate) const LEDGER_VERSION: u32 = 1;
/// Length of the rolling upload window.
pub(crate) const WINDOW_SECONDS: u64 = 24 * 3600;
const HOUR: u64 = 3600;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AccountUsage {
    /// (unix hour, bytes uploaded during that hour), oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hours: Vec<(u64, u64)>,
    /// The provider itself refused uploads for its limit; RPool waits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_pause_until: Option<u64>,
    /// Last successful "Keep alive" of this account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_keepalive: Option<u64>,
}

impl AccountUsage {
    /// The moment the bytes of `hour` leave the window. A bucket stays one
    /// hour longer than its earliest byte would, so the window is never
    /// shorter than the provider's.
    pub(crate) fn expires_at(hour: u64) -> u64 {
        (hour + 1) * HOUR + WINDOW_SECONDS
    }
    pub(crate) fn add(&mut self, now: u64, bytes: u64) {
        self.prune(now);
        if bytes == 0 {
            return;
        }
        let hour = now / HOUR;
        match self.hours.last_mut() {
            Some((last, total)) if *last == hour => *total = total.saturating_add(bytes),
            _ => self.hours.push((hour, bytes)),
        }
    }
    /// Drops buckets that left the window (and any from the future, which a
    /// clock jump could have left behind).
    pub(crate) fn prune(&mut self, now: u64) {
        self.hours
            .retain(|(hour, _)| Self::expires_at(*hour) > now && *hour <= now / HOUR);
        self.hours.sort_unstable();
    }
    /// Bytes uploaded in the window ending at `now`.
    pub(crate) fn used(&self, now: u64) -> u64 {
        self.live(now).map(|(_, bytes)| bytes).sum()
    }
    /// (expiry time, bytes) of the buckets still in the window, oldest first.
    pub(crate) fn live(&self, now: u64) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.hours
            .iter()
            .filter(move |(hour, _)| Self::expires_at(*hour) > now && *hour <= now / HOUR)
            .map(|(hour, bytes)| (Self::expires_at(*hour), *bytes))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct LedgerData {
    pub version: u32,
    /// Keyed by account: the bottom remote of a crypt/alias chain.
    #[serde(default)]
    pub accounts: BTreeMap<String, AccountUsage>,
    /// Last successful operation per rclone remote name (crypt or base).
    #[serde(default)]
    pub activity: BTreeMap<String, u64>,
}

impl LedgerData {
    pub(crate) fn account(&mut self, name: &str) -> &mut AccountUsage {
        self.accounts.entry(name.to_owned()).or_default()
    }
    /// Latest activity among `names` (an account and its crypt remotes).
    pub(crate) fn last_activity<'a>(
        &self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Option<u64> {
        names
            .into_iter()
            .filter_map(|name| self.activity.get(name.trim_end_matches(':')).copied())
            .max()
    }
    pub(crate) fn note_activity(&mut self, name: &str, at: u64) {
        let slot = self
            .activity
            .entry(name.trim_end_matches(':').to_owned())
            .or_default();
        *slot = (*slot).max(at);
    }
}

/// The ledger file at one path.
#[derive(Debug, Clone)]
pub(crate) struct Ledger {
    path: PathBuf,
}

impl Ledger {
    pub(crate) fn at(path: PathBuf) -> Self {
        Self { path }
    }
    fn lock_file(&self) -> Result<File> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(append_suffix(&self.path, ".lock"))
            .context("cannot open account usage lock")
    }
    fn read(&self) -> Result<LedgerData> {
        if !self.path.exists() {
            return Ok(LedgerData {
                version: LEDGER_VERSION,
                ..Default::default()
            });
        }
        let data: LedgerData = read_json(&self.path).context("invalid account usage ledger")?;
        if data.version != LEDGER_VERSION {
            bail!("unsupported account usage ledger version {}", data.version);
        }
        Ok(data)
    }
    /// Current contents (empty when the file does not exist yet).
    pub(crate) fn load(&self) -> Result<LedgerData> {
        let lock = self.lock_file()?;
        lock.lock_shared().context("cannot lock account usage")?;
        let data = self.read();
        drop(lock);
        data
    }
    /// Read-modify-replace under the exclusive lock. An unreadable ledger is
    /// started over: losing local counters must never block uploads.
    pub(crate) fn update<T>(&self, change: impl FnOnce(&mut LedgerData) -> T) -> Result<T> {
        let lock = self.lock_file()?;
        lock.lock().context("cannot lock account usage")?;
        let mut data = self.read().unwrap_or_else(|_| LedgerData {
            version: LEDGER_VERSION,
            ..Default::default()
        });
        let out = change(&mut data);
        save_json_atomic(&self.path, &data)?;
        drop(lock);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_roll_out_of_the_24_hour_window() {
        let mut usage = AccountUsage::default();
        let t0 = 1_000 * HOUR + 600; // 10 minutes into hour 1000
        usage.add(t0, 100);
        usage.add(t0 + 60, 50);
        usage.add(t0 + 2 * HOUR, 7);
        assert_eq!(usage.hours, [(1000, 150), (1002, 7)]);
        assert_eq!(usage.used(t0 + 3 * HOUR), 157);
        // Hour 1000 counts until 24 h after its end, never shorter.
        let first_expiry = AccountUsage::expires_at(1000);
        assert_eq!(first_expiry, 1001 * HOUR + WINDOW_SECONDS);
        assert!(first_expiry >= t0 + 60 + WINDOW_SECONDS);
        assert_eq!(usage.used(first_expiry - 1), 157);
        assert_eq!(usage.used(first_expiry), 7);
        assert_eq!(usage.used(AccountUsage::expires_at(1002)), 0);
        usage.prune(AccountUsage::expires_at(1002));
        assert!(usage.hours.is_empty());
        // Buckets from the future (clock moved back) are not counted.
        usage.hours.push((5000, 9));
        assert_eq!(usage.used(t0), 0);
    }

    #[test]
    fn activity_is_kept_per_remote_and_the_latest_wins() {
        let mut data = LedgerData::default();
        data.note_activity("gd:", 10);
        data.note_activity("gd_crypt", 30);
        data.note_activity("gd", 5);
        assert_eq!(data.last_activity(["gd", "gd_crypt:"]), Some(30));
        assert_eq!(data.last_activity(["gd"]), Some(10));
        assert_eq!(data.last_activity(["other"]), None);
    }

    #[test]
    fn concurrent_writers_from_separate_handles_never_lose_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account_usage.json");
        let now = 2_000 * HOUR;
        // Each thread opens its own lock file handle, as separate processes do.
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let path = path.clone();
                scope.spawn(move || {
                    let ledger = Ledger::at(path);
                    for _ in 0..25 {
                        ledger.update(|d| d.account("gd").add(now, 3)).unwrap();
                    }
                });
            }
        });
        let data = Ledger::at(path.clone()).load().unwrap();
        assert_eq!(data.accounts["gd"].used(now), 8 * 25 * 3);
        // A corrupt file restarts the ledger instead of failing uploads.
        fs::write(&path, b"{not json").unwrap();
        assert!(Ledger::at(path.clone()).load().is_err());
        Ledger::at(path.clone())
            .update(|d| d.account("gd").add(now, 1))
            .unwrap();
        assert_eq!(Ledger::at(path).load().unwrap().accounts["gd"].used(now), 1);
    }
}
