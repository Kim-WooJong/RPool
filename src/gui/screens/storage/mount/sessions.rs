//! Several pools mounted at once: the read-only list of running sessions
//! (also for other pages), and the conflicts that keep two sessions from
//! sharing a pool, a workspace or a mountpoint.
use super::form::MountForm;
use super::session::{MountSession, SessionSpec};
use crate::gui::i18n::trf;
use crate::gui::state::GuiState;
use std::path::{Path, PathBuf};

/// One running GUI-started `rpool mount` process (a drive mount, or a sync,
/// maintenance or recovery run of that pool).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MountedSession {
    /// Pool of the session.
    pub(crate) pool: String,
    /// Workspace of the session.
    pub(crate) workspace: PathBuf,
    /// Drive letter or mount folder; empty when `mounted` is false.
    pub(crate) mountpoint: String,
    /// "FUSE", "WinFsp" or "WebDAV" for drive mounts; empty otherwise.
    pub(crate) frontend: &'static str,
    /// A drive mount, not a sync, maintenance or recovery run.
    pub(crate) mounted: bool,
    /// Unmount was requested; the process is finishing its writes.
    pub(crate) stopping: bool,
    /// The pool the Drive page currently shows.
    pub(crate) selected: bool,
}

/// Every running session, sorted by pool.
pub(crate) fn mounted_sessions(state: &GuiState) -> Vec<MountedSession> {
    state.mount.mounted_sessions()
}

/// Display name of a drive frontend ("FUSE", "WinFsp", "WebDAV").
pub(crate) fn frontend_label(frontend: crate::cli::Frontend) -> &'static str {
    use crate::cli::Frontend;
    match frontend {
        Frontend::Fuse => "FUSE",
        Frontend::Winfsp => "WinFsp",
        Frontend::Auto | Frontend::Dav => "WebDAV",
    }
}

/// Why a session cannot start next to the running ones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Conflict {
    /// The pool already runs (in this GUI).
    Pool(String),
    /// The workspace (or one inside/around it) is used by the named pool.
    Workspace(String),
    /// The mountpoint is used by the named pool; a free drive letter if one is.
    Mountpoint(String, Option<String>),
}

impl Conflict {
    /// Translated explanation of the conflict, shown in the status bar.
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Pool(pool) => trf("Pool {pool} is already mounted or busy. Unmount it first.", &[("pool", pool)]),
            Self::Workspace(pool) => trf("This workspace is already used by the running pool {pool}. Choose another workspace or unmount {pool} first.", &[("pool", pool)]),
            Self::Mountpoint(pool, Some(free)) => trf("This mountpoint is already used by pool {pool}. Free drive letter: {free}.", &[("pool", pool), ("free", free)]),
            Self::Mountpoint(pool, None) => trf("This mountpoint is already used by pool {pool}. Choose another one.", &[("pool", pool)]),
        }
    }
}

/// `R:`, `r:\` and `R:/` all name drive R.
fn drive_letter(value: &str) -> Option<char> {
    let mut chars = value.trim().chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    (chars.next() == Some(':') && matches!(chars.as_str(), "" | "\\" | "/"))
        .then(|| letter.to_ascii_uppercase())
}

/// A comparable form of a local path: the existing part resolved (symlinks,
/// `..`), trailing separators dropped, and on Windows case-insensitive.
fn path_key(value: &str) -> PathBuf {
    let path = Path::new(value.trim());
    let mut rest = Vec::new();
    let mut base = path;
    let resolved = loop {
        if let Ok(real) = std::fs::canonicalize(base) {
            break real;
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                base = parent;
            }
            _ => break path.to_path_buf(),
        }
    };
    let joined: PathBuf = if resolved == path {
        path.components().collect()
    } else {
        rest.iter().rev().fold(resolved, |acc, name| acc.join(name))
    };
    if cfg!(windows) {
        PathBuf::from(joined.to_string_lossy().replace('/', "\\").to_lowercase())
    } else {
        joined
    }
}

/// Comparable key of a mountpoint: `X:` for a drive letter, else the
/// resolved path; `None` when empty.
fn mount_key(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        return None;
    }
    Some(match drive_letter(value) {
        Some(letter) => format!("{letter}:"),
        None => path_key(value).to_string_lossy().into_owned(),
    })
}

/// One workspace inside (or equal to) the other: both would write the
/// same `.rpool` state or files.
fn workspaces_overlap(a: &str, b: &str) -> bool {
    if a.trim().is_empty() || b.trim().is_empty() {
        return false;
    }
    let (a, b) = (path_key(a), path_key(b));
    a.starts_with(&b) || b.starts_with(&a)
}

/// A drive letter no other session uses (and, on Windows, not in use), R–Z
/// first, then D–Q.
pub(crate) fn free_drive_letter(taken: &[String]) -> Option<String> {
    let taken: Vec<char> = taken.iter().filter_map(|m| drive_letter(m)).collect();
    ('R'..='Z')
        .chain('D'..='Q')
        .filter(|letter| !taken.contains(letter))
        .find(|letter| !cfg!(windows) || !Path::new(&format!("{letter}:\\")).exists())
        .map(|letter| format!("{letter}:"))
}

/// The first conflict of `wanted` with the running `others`.
pub(crate) fn conflict<'a>(
    wanted: &SessionSpec,
    others: impl IntoIterator<Item = &'a SessionSpec>,
) -> Option<Conflict> {
    let others: Vec<&SessionSpec> = others.into_iter().collect();
    if others.is_empty() {
        return None;
    }
    if let Some(other) = others.iter().find(|o| o.pool.trim() == wanted.pool.trim()) {
        return Some(Conflict::Pool(other.pool.clone()));
    }
    let mine: Vec<&String> = std::iter::once(&wanted.workspace)
        .chain(&wanted.reads)
        .collect();
    for other in &others {
        let theirs = std::iter::once(&other.workspace).chain(&other.reads);
        if theirs
            .flat_map(|t| mine.iter().map(move |m| (t, *m)))
            .any(|(t, m)| workspaces_overlap(t, m))
        {
            return Some(Conflict::Workspace(other.pool.clone()));
        }
    }
    let key = mount_key(&wanted.mountpoint)?;
    let other = others
        .iter()
        .find(|o| mount_key(&o.mountpoint).as_ref() == Some(&key))?;
    let suggestion = drive_letter(&wanted.mountpoint).and_then(|_| {
        let taken: Vec<String> = others.iter().map(|o| o.mountpoint.clone()).collect();
        free_drive_letter(&taken)
    });
    Some(Conflict::Mountpoint(other.pool.clone(), suggestion))
}

/// Mounts registered by running mount processes (also ones started from the
/// CLI or another GUI), read at most every 2 s: conflicts are checked every
/// frame. This GUI's own sessions appear here too, which is harmless.
fn external_mounts() -> Vec<SessionSpec> {
    if cfg!(test) {
        return Vec::new(); // Tests never read this machine's mount registry.
    }
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static CACHE: Mutex<Option<(Instant, Vec<SessionSpec>)>> = Mutex::new(None);
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((read, specs)) = cache.as_ref() {
        if read.elapsed() < Duration::from_secs(2) {
            return specs.clone();
        }
    }
    let specs: Vec<SessionSpec> = crate::monitor::active_mounts()
        .into_iter()
        .map(|entry| SessionSpec {
            pool: entry.pool,
            workspace: entry.workspace,
            mountpoint: entry.mountpoint,
            frontend: None,
            reads: Vec::new(),
        })
        .collect();
    *cache = Some((Instant::now(), specs.clone()));
    specs
}

impl MountForm {
    /// Sessions of other pools that still run.
    pub(super) fn other_running(&self) -> impl Iterator<Item = &SessionSpec> {
        self.background
            .values()
            .filter(|s| s.is_running())
            .filter_map(|s| s.spec.as_ref())
    }

    /// Why `action` (see `action_args`) cannot start for the selected pool now.
    pub(crate) fn conflict(&self, action: u8) -> Option<Conflict> {
        if self.session.is_running() {
            return Some(Conflict::Pool(self.pool.clone()));
        }
        let spec = self.spec_for(action);
        let external = external_mounts();
        conflict(&spec, self.other_running().chain(external.iter()))
    }

    /// The selected pool's session followed by all others, sorted by pool.
    fn all_sessions(&self) -> Vec<(&str, &MountSession)> {
        let mut all: Vec<(&str, &MountSession)> = self
            .background
            .iter()
            .map(|(pool, s)| (pool.as_str(), s))
            .collect();
        all.push((self.pool.as_str(), &self.session));
        all.sort_by(|a, b| a.0.cmp(b.0));
        all
    }

    /// Every running session (selected and background), sorted by pool.
    pub(crate) fn mounted_sessions(&self) -> Vec<MountedSession> {
        self.all_sessions()
            .into_iter()
            .filter(|(_, s)| s.is_running())
            .filter_map(|(pool, s)| {
                let spec = s.spec.as_ref()?;
                let mounted = s.is_mounted();
                Some(MountedSession {
                    pool: pool.to_string(),
                    workspace: PathBuf::from(spec.workspace.trim()),
                    mountpoint: if mounted {
                        spec.mountpoint.trim().to_string()
                    } else {
                        String::new()
                    },
                    frontend: spec
                        .frontend
                        .filter(|_| mounted)
                        .map(frontend_label)
                        .unwrap_or(""),
                    mounted,
                    stopping: s.stopping,
                    selected: pool == self.pool,
                })
            })
            .collect()
    }

    /// Finished sessions of other pools, whose result is still unread.
    pub(super) fn finished_background(&self) -> Vec<&str> {
        self.background
            .iter()
            .filter(|(_, s)| !s.is_running())
            .map(|(pool, _)| pool.as_str())
            .collect()
    }

    /// Whether any pool's session is running.
    pub(crate) fn any_running(&self) -> bool {
        self.session.is_running() || self.background.values().any(MountSession::is_running)
    }

    /// Requests a graceful unmount of `pool`'s session.
    pub(super) fn stop_pool(&mut self, pool: &str) -> Result<(), String> {
        if pool == self.pool {
            return self.session.request_stop();
        }
        match self.background.get_mut(pool) {
            Some(session) => session.request_stop(),
            None => Ok(()),
        }
    }

    /// Requests a graceful stop of every running session (window closing).
    pub(crate) fn stop_all(&mut self) {
        let sessions = std::iter::once(&mut self.session).chain(self.background.values_mut());
        for session in sessions.filter(|s| s.is_running() && !s.stopping) {
            let _ = session.request_stop();
        }
    }

    /// Replaces an unedited default drive letter another session uses.
    pub(super) fn avoid_taken_drive_letter(&mut self) {
        if let Some(Conflict::Mountpoint(_, Some(free))) = self.conflict(0) {
            self.mountpoint = free;
        }
    }
}
