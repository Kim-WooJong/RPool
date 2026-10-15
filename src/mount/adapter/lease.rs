//! Durable mount-process lease that blocks unsafe restarts.

use super::*;

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
/// Identity of a workspace's native mount, persisted as `.rpool/mount-identity.json`.
/// A restart with different values is refused so pending VFS writes stay with their cache.
pub(super) struct MountIdentity {
    /// Record format version (currently 1).
    pub(super) version: u32,
    /// Canonical anchor directory inside the workspace (see `MountConfig::anchor_dir`).
    pub(super) source: PathBuf,
    /// Canonical rclone VFS cache directory.
    pub(super) cache: PathBuf,
    /// Mount target: canonical directory, or upper-cased drive letter on Windows.
    pub(super) target: PathBuf,
    /// rclone `--vfs-cache-mode` in use (always `"full"`).
    pub(super) cache_mode: String,
    #[serde(default)]
    /// BLAKE3 of the WebDAV URL and bearer token; `None` in records written before it existed.
    pub(super) backend_identity: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
/// Durable state of the owned rclone process in `.rpool/mount-process.json`.
/// Anything but a clean, confirmed exit blocks the next start (see [`MountLease::prepare`]).
pub(super) enum LeaseState {
    /// Recorded just before spawning; outcome unknown if still present on the next start.
    Launching,
    /// rclone child `pid` was spawned and recorded.
    Running {
        /// Process id of the spawned rclone child.
        pid: u32,
    },
    /// rclone child `pid` exited (or was left alive) without a confirmed graceful macOS NFS unmount.
    ShutdownUncertain {
        /// Process id of the rclone child whose exit was not confirmed.
        pid: u32,
    },
}

/// Exclusive per-workspace mount lease: holds `.rpool/mount-process.lock` and points at the
/// lease record. Owned by `MountProcess`.
pub(super) struct MountLease {
    /// Lease record path (`.rpool/mount-process.json`).
    pub(super) path: PathBuf,
    // Serializes adapter startup even if a caller accidentally omits Workspace ownership.
    /// Locked `mount-process.lock` file handle; unlocked on drop.
    pub(super) _lock: std::fs::File,
}

impl Drop for MountLease {
    fn drop(&mut self) {
        // On Unix a concurrently forked child can briefly retain this open file
        // description until exec, even with close-on-exec set. Closing only our
        // descriptor can therefore delay release past the lease lifetime.
        // The durable lease record independently fences uncertain/live mounts.
        let _ = self._lock.unlock();
    }
}

impl MountLease {
    /// Locks the workspace's mount lease, refuses if a previous mount may still be live or
    /// uncertain, and checks or creates `mount-identity.json`. Called by `MountProcess::start`.
    pub(super) fn prepare(
        source: &Path,
        cache: &Path,
        target: &Path,
        webdav: &(String, String),
    ) -> Result<Self> {
        let metadata = source
            .parent()
            .context("workspace root missing")?
            .join(".rpool");
        let info =
            std::fs::symlink_metadata(&metadata).context("managed workspace metadata missing")?;
        if !info.is_dir() || info.file_type().is_symlink() {
            bail!("mount metadata must be a real directory");
        }
        let lock_path = metadata.join("mount-process.lock");
        reject_link(&lock_path)?;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        lock.try_lock()
            .context("another mount adapter owns this workspace")?;
        let lease = Self {
            path: metadata.join("mount-process.json"),
            _lock: lock,
        };
        reject_link(&lease.path)?;
        if lease.path.exists() {
            let previous: LeaseState = serde_json::from_slice(&std::fs::read(&lease.path)?)
                .context("invalid mount lease; inspect surviving mount before manual recovery")?;
            lease.recover_previous(&previous, &metadata, cache, target)?;
        }
        #[cfg(windows)]
        let target = PathBuf::from(target.to_string_lossy().to_ascii_uppercase());
        #[cfg(not(windows))]
        let target = target.canonicalize()?;
        let identity = MountIdentity {
            version: 1,
            source: source.into(),
            cache: cache.into(),
            target,
            cache_mode: "full".into(),
            backend_identity: Some(
                blake3::hash(format!("webdav-other:{}:{}", webdav.0, webdav.1).as_bytes())
                    .to_hex()
                    .to_string(),
            ),
        };
        let identity_path = metadata.join("mount-identity.json");
        reject_link(&identity_path)?;
        if identity_path.exists() {
            let previous: MountIdentity = serde_json::from_slice(&std::fs::read(identity_path)?)?;
            if previous != identity {
                bail!("mount source/cache/target identity changed; restart with the original workspace and mountpoint to preserve pending VFS writes");
            }
        } else {
            durable_json(&identity_path, &identity)?;
        }
        Ok(lease)
    }

    /// Clears the lease of a previous mount that did not stop cleanly once it
    /// has provably ended, else refuses. A recorded rclone that still runs is
    /// stopped only when it is this workspace's orphan (`orphan`). Otherwise
    /// the previous mount counts as ended when no rclone of this workspace
    /// runs and no volume is left at the requested or recorded mountpoint (a
    /// volume whose server is gone is force-unmounted, see `release`). Its
    /// unsent saves stay in the VFS cache; the mount recovers them on start.
    fn recover_previous(
        &self,
        previous: &LeaseState,
        metadata: &Path,
        cache: &Path,
        target: &Path,
    ) -> Result<()> {
        let pid = match previous {
            LeaseState::Launching => None,
            LeaseState::Running { pid } | LeaseState::ShutdownUncertain { pid } => Some(*pid),
        };
        if let Some(pid) = pid {
            if process_alive(pid)
                .context("cannot prove previous mount process exited; lease retained")?
            {
                // A crashed RPool's orphan is stopped cleanly; a live owner is
                // left alone. A PID now running something else was reused and
                // is never signalled; the checks below decide.
                #[cfg(unix)]
                if super::orphan::runs_workspace_mount(pid, cache)
                    && !super::orphan::stop_orphan(pid, cache, target)?
                {
                    bail!("previous mount process PID {pid} may still be active; stop it before restarting this workspace");
                }
                #[cfg(not(unix))]
                bail!("previous mount process PID {pid} may still be active; stop it before restarting this workspace");
            }
        }
        // Windows: a recorded PID that no longer exists ended the drive with it
        // (job object); never signal a possibly reused PID.
        #[cfg(not(unix))]
        if matches!(previous, LeaseState::Running { .. }) {
            return self.clear();
        }
        let recorded = read_identity(metadata)?.map(|identity| identity.target);
        let mut targets = vec![target];
        if let Some(recorded) = recorded.as_deref().filter(|r| *r != target) {
            targets.push(recorded);
        }
        match super::release::previous_mount_remains(cache, &targets)? {
            None => {
                eprintln!("The previous mount of this workspace ended without a clean stop; no rclone process or volume of it remains, so the workspace is reopened. Its unsent saves are recovered from the VFS cache.");
                self.clear()
            }
            Some(reason) => bail!(
                "previous mount of this workspace has not ended: {reason}. Local files and cache are retained ({})",
                self.path.display()
            ),
        }
    }

    /// Durably writes `state` to the lease record.
    pub(super) fn record(&self, state: &LeaseState) -> Result<()> {
        durable_json(&self.path, state)
    }

    /// Removes the lease record (missing is fine) and syncs its directory.
    pub(super) fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => sync_metadata_directory(self.path.parent().context("lease parent missing")?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Clears the lease unless macOS still lists a mount at `target`.
    pub(super) fn clear_if_unmounted(&self, target: &Path) -> Result<()> {
        #[cfg(target_os = "macos")]
        if macos_mount_attached(target)? {
            bail!(
                "native mount remains attached at {}; mount lease retained",
                target.display()
            );
        }
        #[cfg(not(target_os = "macos"))]
        let _ = target;
        self.clear()
    }
}

/// The workspace's recorded mount identity (`mount-identity.json` in
/// `metadata`), `None` when there is none yet.
pub(super) fn read_identity(metadata: &Path) -> Result<Option<MountIdentity>> {
    let path = metadata.join("mount-identity.json");
    reject_link(&path)?;
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(target_os = "macos")]
/// True if the macOS mount table (read without waiting on NFS servers) has a mount at `target`.
pub(super) fn macos_mount_attached(target: &Path) -> Result<bool> {
    Ok(macos_mount_source(target)?.is_some())
}

#[cfg(target_os = "macos")]
/// File system type and source (`f_mntfromname`) of the mount at `target`,
/// read from the mount table without waiting on NFS servers; `None` when
/// nothing is mounted there. Never touches `target` itself.
pub(crate) fn macos_mount_source(target: &Path) -> Result<Option<(String, String)>> {
    use std::os::unix::ffi::OsStrExt;

    unsafe extern "C" {
        fn getmntinfo_r_np(mntbufp: *mut *mut libc::statfs, flags: libc::c_int) -> libc::c_int;
    }
    /// A NUL-terminated `statfs` name field as bytes.
    fn field(name: &[libc::c_char]) -> Vec<u8> {
        name.iter()
            .take_while(|&&byte| byte != 0)
            .map(|&byte| byte as u8)
            .collect()
    }
    let mut table: *mut libc::statfs = std::ptr::null_mut();
    // MNT_NOWAIT avoids querying an unreachable NFS server. The _r_np variant
    // owns a fresh buffer per call, unlike getmntinfo's shared static buffer.
    let count = unsafe { getmntinfo_r_np(&mut table, libc::MNT_NOWAIT) };
    if count <= 0 || table.is_null() {
        if !table.is_null() {
            unsafe { libc::free(table.cast()) };
        }
        bail!("cannot verify macOS mount table; mount lease retained");
    }
    let mounts = unsafe { std::slice::from_raw_parts(table, count as usize) };
    let target = target.as_os_str().as_bytes();
    let found = mounts
        .iter()
        .find(|mount| field(&mount.f_mntonname) == target)
        .map(|mount| {
            (
                String::from_utf8_lossy(&field(&mount.f_fstypename)).into_owned(),
                String::from_utf8_lossy(&field(&mount.f_mntfromname)).into_owned(),
            )
        });
    unsafe { libc::free(table.cast()) };
    Ok(found)
}

#[cfg(all(test, target_os = "macos"))]
mod mount_table_tests {
    use super::*;

    #[test]
    fn attached_mount_retains_lease_until_kernel_table_clears() {
        let root = tempfile::tempdir().unwrap();
        let lease = MountLease {
            path: root.path().join("mount-process.json"),
            _lock: std::fs::File::create(root.path().join("mount-process.lock")).unwrap(),
        };
        lease.record(&LeaseState::Running { pid: 1 }).unwrap();
        assert!(macos_mount_attached(Path::new("/")).unwrap());
        assert!(lease.clear_if_unmounted(Path::new("/")).is_err());
        assert!(lease.path.exists());
        assert!(!macos_mount_attached(root.path()).unwrap());
        lease.clear_if_unmounted(root.path()).unwrap();
        assert!(!lease.path.exists());
    }
}

/// Fails if `path` exists but is a symlink or not a regular file; missing is fine.
pub(super) fn reject_link(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!("mount state must be a regular file: {}", path.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Atomically writes pretty JSON via a synced temp file in the same directory, then syncs it.
pub(super) fn durable_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let parent = path.parent().context("mount metadata parent missing")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, value)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    sync_metadata_directory(parent)
}

/// fsyncs a directory so renames/removals in it are durable (no-op off Unix).
pub(super) fn sync_metadata_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    std::fs::File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
