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
            match previous {
                LeaseState::Launching => bail!("mount launch outcome is uncertain; confirm no rclone process or driver mount uses this workspace before manually removing {} (retain all files/cache)", lease.path.display()),
                LeaseState::ShutdownUncertain { pid } => bail!("macOS NFS shutdown for PID {pid} is uncertain; inspect kernel I/O and mount state before manually clearing {} (retain all files/cache)", lease.path.display()),
                LeaseState::Running { pid } => {
                    if process_alive(pid).context("cannot prove previous mount process exited; lease retained")? {
                        // A crashed RPool's orphan is stopped cleanly; anything else is left alone.
                        #[cfg(unix)]
                        if super::orphan::stop_orphan(pid, cache, target)? {
                            lease.clear()?;
                        } else {
                            bail!("previous mount process PID {pid} may still be active; stop it before restarting this workspace");
                        }
                        #[cfg(not(unix))]
                        bail!("previous mount process PID {pid} may still be active; stop it before restarting this workspace");
                    } else {
                    #[cfg(target_os = "macos")]
                    bail!("previous macOS NFS mount process PID {pid} exited without a confirmed clean shutdown; inspect kernel I/O and mount state before manually clearing {} (retain all files/cache)", lease.path.display());
                    // The recorded PID does not exist. Never signal/kill a possibly reused PID.
                    #[cfg(not(target_os = "macos"))]
                    lease.clear_if_unmounted(target)?;
                    }
                }
            }
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

#[cfg(target_os = "macos")]
/// True if the macOS mount table (read without waiting on NFS servers) has a mount at `target`.
pub(super) fn macos_mount_attached(target: &Path) -> Result<bool> {
    use std::os::unix::ffi::OsStrExt;

    unsafe extern "C" {
        fn getmntinfo_r_np(mntbufp: *mut *mut libc::statfs, flags: libc::c_int) -> libc::c_int;
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
    let attached = mounts.iter().any(|mount| {
        let name = &mount.f_mntonname;
        let len = name
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(name.len());
        name[..len]
            .iter()
            .map(|&byte| byte as u8)
            .eq(target.iter().copied())
    });
    unsafe { libc::free(table.cast()) };
    Ok(attached)
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
