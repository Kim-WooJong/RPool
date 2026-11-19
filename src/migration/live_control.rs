//! Live run controls of `pool migrate run`: archives at once, shard transfers
//! per archive and the run's bandwidth limit can change while the run goes on.
//! Only runtime scheduling changes: the plan, target policy, keys, journal and
//! every id stay as they are, and no archive in progress is cancelled.
//!
//! Each run owns one local control folder (the GUI's temporary run folder,
//! `run --control-dir`, or `<config>/migrations/<id>/live/<run-id>`) with two
//! files, each replaced atomically (temp file + rename):
//! - `live-state.json`, written only by the run: its identity (migration,
//!   pool, run id, pid, PC), a heartbeat, the values in force and the outcome
//!   of the last request (the ack);
//! - `live-control.json`, written only by a controller (GUI, `migrate tune`):
//!   a request naming the migration and the run id, with a sequence number.
//!
//! The run reads the request about once a second. It rejects a request for
//! another migration or run, an old sequence number and invalid values
//! (e.g. a bandwidth timetable that does not parse): the previous values stay
//! in force and the rejection is reported in the state file.
//!
//! When a change takes effect:
//! - archives at once: at the admission of the next archive (a free slot).
//!   Raising it starts waiting archives right away; lowering it never cancels
//!   archives in progress, it only holds new ones back until fewer run.
//! - shard transfers: archives that start after the change; an archive in
//!   progress keeps the count it started with. 0 = the pool's workers.
//! - bandwidth: RPool's own paced transfers (streams already open included)
//!   within about a second; copies that rclone makes between accounts by
//!   itself get the new per-stream limit when their next copy starts.
use super::execute::{random_hex, DEFAULT_PARALLEL, MAX_PARALLEL};
use crate::prelude::*;
use crate::storage::account::bandwidth::Timetable;
use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Condvar, PoisonError};
use std::time::{Duration, Instant};

/// Version of the two control files.
pub(crate) const PROTOCOL: u32 = 1;
/// File the run writes (identity, heartbeat, values in force, ack).
pub(crate) const STATE_FILE: &str = "live-state.json";
/// File a controller writes (one change request).
pub(crate) const CONTROL_FILE: &str = "live-control.json";
/// A run whose heartbeat is older than this is not running any more.
pub(crate) const STALE_SECONDS: u64 = 15;
/// Upper bound of shard transfers per archive (same as `run --transfers`).
pub(crate) const MAX_TRANSFERS: usize = 256;
/// How often the run looks for a request.
const POLL: Duration = Duration::from_millis(1000);
/// How often the run refreshes its heartbeat without other changes.
const HEARTBEAT: Duration = Duration::from_secs(2);
/// How long a waiting worker sleeps before checking stop/limits again.
const ADMIT_WAIT: Duration = Duration::from_millis(250);

/// The three runtime values of a run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Settings {
    /// Archives migrated at once (`1..=MAX_PARALLEL`).
    pub parallel: usize,
    /// Shard transfers per archive; 0 = the pool's workers.
    pub transfers: usize,
    /// Bandwidth limit of the whole run (`--bwlimit` syntax); empty = none.
    #[serde(default)]
    pub bwlimit: String,
}

impl Settings {
    /// Checks the ranges and parses the bandwidth limit (`None` = no limit).
    pub(crate) fn validate(&self) -> Result<Option<Timetable>> {
        if !(1..=MAX_PARALLEL).contains(&self.parallel) {
            bail!(
                "archives at once must be between 1 and {MAX_PARALLEL} (got {})",
                self.parallel
            );
        }
        if self.transfers > MAX_TRANSFERS {
            bail!(
                "shard transfers must be between 0 and {MAX_TRANSFERS} (got {})",
                self.transfers
            );
        }
        parse_bwlimit(&self.bwlimit)
    }

    /// The same settings with the bandwidth text trimmed (`off` = none).
    pub(crate) fn normalized(mut self) -> Self {
        let text = self.bwlimit.trim();
        self.bwlimit = if text.eq_ignore_ascii_case("off") {
            String::new()
        } else {
            text.to_string()
        };
        self
    }
}

/// Parses a run bandwidth limit; empty or `off` = no limit.
pub(crate) fn parse_bwlimit(text: &str) -> Result<Option<Timetable>> {
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("off") {
        return Ok(None);
    }
    Timetable::parse(text)
        .map(Some)
        .with_context(|| format!("invalid bandwidth limit {text:?}"))
}

/// One change request (`live-control.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Request {
    /// [`PROTOCOL`].
    pub protocol: u32,
    /// Migration the request is for.
    pub migration_id: String,
    /// Run the request is for (from its state file).
    pub run_id: String,
    /// Increases with every request to the same run.
    pub seq: u64,
    /// When the controller wrote it, Unix seconds.
    pub issued_unix: u64,
    /// The complete new values.
    pub settings: Settings,
}

/// Outcome of the last request the run read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Ack {
    /// Sequence number of that request.
    pub seq: u64,
    /// Whether it was applied.
    pub accepted: bool,
    /// Why it was rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// When the run decided, Unix seconds.
    pub ts_unix: u64,
}

/// What the run reports (`live-state.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RunState {
    /// [`PROTOCOL`].
    pub protocol: u32,
    /// Migration being run.
    pub migration_id: String,
    /// Its pool.
    pub pool: String,
    /// Random id of this run (process), fresh for every start.
    pub run_id: String,
    /// Process id of the run.
    pub pid: u32,
    /// PC name as recorded in the journal.
    pub pc_id: String,
    /// RPool version of the run.
    pub version: String,
    /// When the run started, Unix seconds.
    pub started_unix: u64,
    /// Last sign of life, Unix seconds.
    pub heartbeat_unix: u64,
    /// The run has finished (or stopped) and reads no more requests.
    pub ended: bool,
    /// The pool's workers (what transfers 0 means).
    pub pool_workers: usize,
    /// Values in force.
    pub applied: Settings,
    /// Sequence number of the request that set `applied` (0 = start values).
    pub applied_seq: u64,
    /// Highest sequence number handled (applied or rejected).
    pub last_seq: u64,
    /// Outcome of the last request read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ack: Option<Ack>,
    /// Archives in progress now.
    pub active_archives: usize,
}

impl RunState {
    /// Whether the run is still going and reading requests at `now`.
    pub(crate) fn is_live(&self, now: u64) -> bool {
        self.protocol == PROTOCOL
            && !self.ended
            && now.saturating_sub(self.heartbeat_unix) <= STALE_SECONDS
    }

    /// Shard transfers a new archive gets with these values.
    pub(crate) fn effective_transfers(&self) -> usize {
        effective_transfers(self.applied.transfers, self.pool_workers)
    }

    /// Where request `seq` stands in this state.
    pub(crate) fn ack_of(&self, seq: u64) -> AckState {
        match &self.last_ack {
            Some(ack) if ack.seq == seq && ack.accepted => AckState::Applied,
            Some(ack) if ack.seq == seq => {
                AckState::Rejected(ack.error.clone().unwrap_or_default())
            }
            _ if self.last_seq >= seq => AckState::Superseded,
            _ => AckState::Pending,
        }
    }
}

/// Where one request stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AckState {
    /// Not read by the run yet.
    Pending,
    /// In force.
    Applied,
    /// Refused; the previous values stay.
    Rejected(String),
    /// A later request was handled after it.
    Superseded,
}

/// Shard transfers for `transfers` (0 = the pool's workers).
pub(crate) fn effective_transfers(transfers: usize, pool_workers: usize) -> usize {
    if transfers == 0 {
        pool_workers.max(1)
    } else {
        transfers
    }
}

/// Reason a request is refused, before its values are applied.
fn check(state: &RunState, request: &Request) -> Result<Option<Timetable>> {
    if request.protocol != PROTOCOL {
        bail!("unsupported control protocol {}", request.protocol);
    }
    if request.migration_id != state.migration_id {
        bail!(
            "request is for migration {}, this run is {}",
            request.migration_id,
            state.migration_id
        );
    }
    if request.run_id != state.run_id {
        bail!(
            "request is for run {}, this run is {} (stale control file?)",
            request.run_id,
            state.run_id
        );
    }
    if request.seq <= state.last_seq {
        bail!(
            "old request {} (already handled {})",
            request.seq,
            state.last_seq
        );
    }
    request.settings.validate()
}

/// Writes `value` as JSON to `path` via a unique temp file renamed into
/// place, so readers see either the old or the new file, never a partial one.
pub(crate) fn write_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let name = path
        .file_name()
        .context("control file without a name")?
        .to_string_lossy()
        .into_owned();
    let tmp = path.with_file_name(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        random_hex(4)?
    ));
    fs::write(&tmp, bytes)?;
    // Windows refuses to replace a file another process has open for a
    // moment (a reader): retry briefly.
    let mut last = None;
    for _ in 0..20 {
        match fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(error) => {
                last = Some(error);
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
    let _ = fs::remove_file(&tmp);
    Err(anyhow!(
        "cannot replace {}: {}",
        path.display(),
        last.map_or_else(String::new, |e| e.to_string())
    ))
}

/// Reads the run state in `dir`; `None` when there is none (yet).
pub(crate) fn read_state(dir: &Path) -> Result<Option<RunState>> {
    match fs::read(dir.join(STATE_FILE)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
            format!("unreadable {}", dir.join(STATE_FILE).display())
        })?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Reads the pending request in `dir`, if any.
fn read_request(dir: &Path) -> Option<Request> {
    serde_json::from_slice(&fs::read(dir.join(CONTROL_FILE)).ok()?).ok()
}

/// Asks the live run described by `state` (in `dir`) to use `settings`.
/// Invalid values are refused here, before anything is written. Returns the
/// request's sequence number; [`RunState::ack_of`] tells its outcome.
pub(crate) fn submit(dir: &Path, state: &RunState, settings: Settings) -> Result<u64> {
    let settings = settings.normalized();
    settings.validate()?;
    if !state.is_live(crate::utils::now_unix()) {
        bail!("the run is not running any more (no recent heartbeat)");
    }
    let previous = read_request(dir)
        .filter(|r| r.run_id == state.run_id)
        .map_or(0, |r| r.seq);
    let seq = state.last_seq.max(previous) + 1;
    write_atomic(
        &dir.join(CONTROL_FILE),
        &Request {
            protocol: PROTOCOL,
            migration_id: state.migration_id.clone(),
            run_id: state.run_id.clone(),
            seq,
            issued_unix: crate::utils::now_unix(),
            settings,
        },
    )?;
    Ok(seq)
}

/// Folder of automatic control folders of `migration_id` on this PC.
fn auto_root(migration_id: &str) -> Result<PathBuf> {
    Ok(crate::config::app_config_dir()?
        .join("migrations")
        .join(migration_id)
        .join("live"))
}

/// Live runs of `pool`/`migration_id` on this PC with an automatic control
/// folder (runs started with `--control-dir` are found only through it).
pub(crate) fn discover(pool: &str, migration_id: &str) -> Result<Vec<(PathBuf, RunState)>> {
    let root = auto_root(migration_id)?;
    let Ok(entries) = fs::read_dir(&root) else {
        return Ok(Vec::new());
    };
    let now = crate::utils::now_unix();
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if let Ok(Some(state)) = read_state(&dir) {
            if state.is_live(now) && state.pool == pool && state.migration_id == migration_id {
                out.push((dir, state));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

// ---------------------------------------------------------------------------
// Run side.

/// The run's live values, shared by its workers and the control thread.
pub(crate) struct LiveControl {
    /// Control folder.
    dir: PathBuf,
    /// The pool's workers (what transfers 0 means).
    pool_workers: usize,
    /// Archives admitted at once now.
    parallel: AtomicUsize,
    /// Shard transfers for the next archive (0 = pool workers).
    transfers: AtomicUsize,
    /// Archives in progress; guarded with `freed` for admission.
    active: Mutex<usize>,
    /// Signalled when a slot frees up or the limit changes.
    freed: Condvar,
    /// What the state file says (the ack included).
    state: Mutex<RunState>,
    /// Raw bytes of the last request read (each version is handled once).
    last_raw: Mutex<Option<Vec<u8>>>,
}

impl std::fmt::Debug for LiveControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveControl")
            .field("dir", &self.dir)
            .field("parallel", &self.parallel)
            .field("transfers", &self.transfers)
            .finish_non_exhaustive()
    }
}

/// A slot of one archive in progress; frees itself when dropped.
pub(crate) struct Slot<'a> {
    /// The control the slot belongs to.
    control: &'a LiveControl,
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        let mut active = self
            .control
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *active = active.saturating_sub(1);
        drop(active);
        self.control.freed.notify_all();
    }
}

impl LiveControl {
    /// Archives admitted at once now.
    pub(crate) fn parallel_limit(&self) -> usize {
        self.parallel.load(SeqCst).clamp(1, MAX_PARALLEL)
    }

    /// Shard transfers an archive starting now gets.
    pub(crate) fn transfers_for_next_archive(&self) -> usize {
        effective_transfers(self.transfers.load(SeqCst), self.pool_workers)
    }

    /// `target` with the shard transfers of an archive starting now.
    pub(crate) fn target_for_next_archive<'t>(
        live: Option<&LiveControl>,
        target: &'t PoolDefinition,
    ) -> Cow<'t, PoolDefinition> {
        match live {
            Some(live) => {
                let mut target = target.clone();
                target.workers = live.transfers_for_next_archive();
                Cow::Owned(target)
            }
            None => Cow::Borrowed(target),
        }
    }

    /// Waits for a free slot under the current limit. `None` when `abort`
    /// becomes true first (stop requested, run halted, no work left).
    pub(crate) fn admit(&self, abort: &dyn Fn() -> bool) -> Option<Slot<'_>> {
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if abort() {
                return None;
            }
            if *active < self.parallel_limit() {
                *active += 1;
                return Some(Slot { control: self });
            }
            active = self
                .freed
                .wait_timeout(active, ADMIT_WAIT)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Applies checked values: admission, next archives, process bandwidth.
    fn apply(&self, settings: &Settings, table: Option<Timetable>) {
        self.parallel.store(settings.parallel, SeqCst);
        self.transfers.store(settings.transfers, SeqCst);
        let streams =
            settings.parallel * effective_transfers(settings.transfers, self.pool_workers);
        match table {
            Some(table) => crate::storage::rclone::process_limit::set(table, streams),
            None => crate::storage::rclone::process_limit::clear(),
        }
        self.freed.notify_all();
    }

    /// Reads a new request (if any) and decides it; returns whether the
    /// state changed.
    fn poll_request(&self) -> bool {
        let Ok(raw) = fs::read(self.dir.join(CONTROL_FILE)) else {
            return false;
        };
        {
            let mut last = self
                .last_raw
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if last.as_deref() == Some(raw.as_slice()) {
                return false;
            }
            *last = Some(raw.clone());
        }
        let now = crate::utils::now_unix();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let request: Request = match serde_json::from_slice(&raw) {
            Ok(request) => request,
            Err(error) => {
                println!("migration_live_control_rejected error=unreadable request: {error}");
                state.last_ack = Some(Ack {
                    seq: 0,
                    accepted: false,
                    error: Some(format!("unreadable request: {error}")),
                    ts_unix: now,
                });
                return true;
            }
        };
        match check(&state, &request) {
            Ok(table) => {
                let settings = request.settings.clone().normalized();
                self.apply(&settings, table);
                println!(
                    "migration_live_control_applied seq={} parallel={} transfers={} (next archives: {}) bwlimit={}",
                    request.seq,
                    settings.parallel,
                    settings.transfers,
                    effective_transfers(settings.transfers, self.pool_workers),
                    if settings.bwlimit.is_empty() {
                        "off"
                    } else {
                        settings.bwlimit.as_str()
                    }
                );
                state.applied = settings;
                state.applied_seq = request.seq;
                state.last_seq = request.seq;
                state.last_ack = Some(Ack {
                    seq: request.seq,
                    accepted: true,
                    error: None,
                    ts_unix: now,
                });
            }
            Err(error) => {
                println!(
                    "migration_live_control_rejected seq={} error={error:#} (previous values kept)",
                    request.seq
                );
                // Only a request addressed to this run consumes its number.
                let ours = request.run_id == state.run_id
                    && request.migration_id == state.migration_id
                    && request.seq > state.last_seq;
                if ours {
                    state.last_seq = request.seq;
                }
                state.last_ack = Some(Ack {
                    seq: request.seq,
                    accepted: false,
                    error: Some(format!("{error:#}")),
                    ts_unix: now,
                });
            }
        }
        true
    }

    /// Writes the state file with a fresh heartbeat (and `ended`).
    fn write_state(&self, ended: bool) {
        let active = *self.active.lock().unwrap_or_else(PoisonError::into_inner);
        let snapshot = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.heartbeat_unix = crate::utils::now_unix();
            state.active_archives = active;
            state.ended = ended;
            state.clone()
        };
        if let Err(error) = write_atomic(&self.dir.join(STATE_FILE), &snapshot) {
            eprintln!("[warning] live control state not written: {error:#}");
        }
    }
}

/// A run's live control and its polling thread; dropping it marks the run
/// ended (and removes an automatic control folder).
pub(crate) struct LiveRun {
    /// Shared with the run's workers.
    control: Arc<LiveControl>,
    /// Tells the polling thread to finish.
    closing: Arc<AtomicBool>,
    /// The polling thread.
    thread: Option<std::thread::JoinHandle<()>>,
    /// The folder was created for this run (removed at the end).
    owned_dir: bool,
}

impl LiveRun {
    /// Opens the control folder `dir` (or an automatic one) for a new run
    /// with the `initial` values, writes the first state and starts polling.
    /// Refuses a folder another live run still reports in.
    pub(crate) fn open(
        dir: Option<&Path>,
        pool: &str,
        migration_id: &str,
        pool_workers: usize,
        initial: Settings,
    ) -> Result<Self> {
        let run_id = random_hex(8)?;
        let owned_dir = dir.is_none();
        let dir = match dir {
            Some(dir) => dir.to_path_buf(),
            None => auto_root(migration_id)?.join(&run_id),
        };
        fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create control folder {}", dir.display()))?;
        let now = crate::utils::now_unix();
        if let Ok(Some(other)) = read_state(&dir) {
            if other.is_live(now) {
                bail!(
                    "control folder {} is in use by run {} (pid {}) of migration {}",
                    dir.display(),
                    other.run_id,
                    other.pid,
                    other.migration_id
                );
            }
        }
        // A request left by an earlier run must never apply to this one.
        let _ = fs::remove_file(dir.join(CONTROL_FILE));
        if owned_dir {
            remove_stale_auto_dirs(migration_id, now);
        }
        let initial = initial.normalized();
        let state = RunState {
            protocol: PROTOCOL,
            migration_id: migration_id.to_string(),
            pool: pool.to_string(),
            run_id: run_id.clone(),
            pid: std::process::id(),
            pc_id: super::execute::pc_id(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            started_unix: now,
            heartbeat_unix: now,
            ended: false,
            pool_workers: pool_workers.max(1),
            applied: initial.clone(),
            applied_seq: 0,
            last_seq: 0,
            last_ack: None,
            active_archives: 0,
        };
        let control = Arc::new(LiveControl {
            dir: dir.clone(),
            pool_workers: pool_workers.max(1),
            parallel: AtomicUsize::new(initial.parallel.clamp(1, MAX_PARALLEL)),
            transfers: AtomicUsize::new(initial.transfers),
            active: Mutex::new(0),
            freed: Condvar::new(),
            state: Mutex::new(state),
            last_raw: Mutex::new(None),
        });
        control.write_state(false);
        let closing = Arc::new(AtomicBool::new(false));
        let thread = {
            let control = control.clone();
            let closing = closing.clone();
            std::thread::Builder::new()
                .name("rpool-migrate-live".into())
                .spawn(move || {
                    let mut beat = Instant::now();
                    while !closing.load(SeqCst) {
                        let changed = control.poll_request();
                        if changed || beat.elapsed() >= HEARTBEAT {
                            control.write_state(false);
                            beat = Instant::now();
                        }
                        std::thread::sleep(POLL);
                    }
                })?
        };
        println!(
            "migration_live_control dir={} run_id={run_id} (change while running: GUI run step, or `rpool pool migrate tune {pool} --id {migration_id} --control-dir <dir>`)",
            dir.display()
        );
        Ok(Self {
            control,
            closing,
            thread: Some(thread),
            owned_dir,
        })
    }

    /// The control shared with the run's workers.
    pub(crate) fn control(&self) -> Arc<LiveControl> {
        self.control.clone()
    }
}

impl Drop for LiveRun {
    fn drop(&mut self) {
        self.closing.store(true, SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.control.write_state(true);
        if self.owned_dir {
            let _ = fs::remove_dir_all(&self.control.dir);
        }
    }
}

/// Removes automatic control folders of runs that stopped reporting long ago
/// (crashed or killed); live ones are left alone.
fn remove_stale_auto_dirs(migration_id: &str, now: u64) {
    let Ok(root) = auto_root(migration_id) else {
        return;
    };
    let Ok(entries) = fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let stale = match read_state(&dir) {
            Ok(Some(state)) => {
                state.ended || now.saturating_sub(state.heartbeat_unix) > 4 * STALE_SECONDS
            }
            _ => false,
        };
        if stale {
            let _ = fs::remove_dir_all(&dir);
        }
    }
}

/// The initial values of a run from its options (what `run` uses).
pub(crate) fn initial_settings(
    parallel: Option<usize>,
    transfers: Option<usize>,
    bwlimit: Option<&str>,
) -> Settings {
    Settings {
        parallel: parallel.unwrap_or(DEFAULT_PARALLEL).clamp(1, MAX_PARALLEL),
        transfers: transfers.unwrap_or(0).min(MAX_TRANSFERS),
        bwlimit: bwlimit.unwrap_or_default().trim().to_string(),
    }
    .normalized()
}
