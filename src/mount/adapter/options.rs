//! rclone mount command options: volume label, VFS cache policy and mount command.

use super::*;

/// A volume label from a pool name: Windows labels hold at most 32 characters,
/// and separators or quotes could break the mount option string.
pub(in crate::mount) fn volume_label(pool: &str) -> String {
    let label: String = pool
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    c,
                    ',' | '\\' | '/' | ':' | '"' | '=' | '*' | '?' | '<' | '>' | '|'
                )
        })
        .take(32)
        .collect();
    let label = label.trim().to_string();
    if label.is_empty() {
        "RPool".into()
    } else {
        label
    }
}

/// The NFS server closes a VFS handle after each WRITE RPC. Without delayed
/// write-back, every close uploads the growing whole file through WebDAV.
pub(in crate::mount) fn vfs_cache_policy() -> (&'static str, &'static str) {
    ("full", "60s")
}

/// rclone subcommand for a native mount: `nfsmount` on macOS, `mount` elsewhere.
pub(super) fn native_mount_command() -> &'static str {
    if cfg!(target_os = "macos") {
        "nfsmount"
    } else {
        "mount"
    }
}

#[cfg(target_os = "macos")]
/// Fails unless `rclone version` reports v1.65 or newer (needed for `nfsmount`).
pub(super) fn check_nfsmount_version(rclone: &str) -> Result<()> {
    let output = Command::new(rclone)
        .arg("version")
        .output()
        .context("cannot run rclone; macOS NFS mounting requires rclone v1.65 or newer")?;
    let first = String::from_utf8_lossy(&output.stdout);
    let version = first
        .lines()
        .next()
        .unwrap_or("")
        .strip_prefix("rclone v")
        .and_then(|v| {
            let mut parts = v.split('.');
            Some((
                parts.next()?.parse::<u32>().ok()?,
                parts.next()?.parse::<u32>().ok()?,
            ))
        });
    if !output.status.success()
        || !version.is_some_and(|(major, minor)| major > 1 || (major == 1 && minor >= 65))
    {
        bail!(
            "macOS NFS mounting requires rclone v1.65 or newer with nfsmount support; detected {}",
            first.lines().next().unwrap_or("unknown version")
        );
    }
    Ok(())
}

// rclone owns native cache eviction: never remove dirty/open native files ourselves.
/// Adds rclone VFS cache size, free-space floor (GiB) and poll interval options.
pub(super) fn configure_cache(command: &mut Command, limit_gib: u64, min_free_gib: u64) {
    command.args([
        "--vfs-cache-max-size",
        &format!("{limit_gib}G"),
        "--vfs-cache-min-free-space",
        &format!("{min_free_gib}G"),
        "--vfs-cache-poll-interval",
        "5s",
    ]);
}

#[cfg(test)]
mod cache_option_tests {
    use super::*;
    #[test]
    fn forwards_bounded_native_cache_and_disk_headroom_without_touching_files() {
        let mut command = Command::new("unused");
        configure_cache(&mut command, 7, 3);
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "--vfs-cache-max-size",
                "7G",
                "--vfs-cache-min-free-space",
                "3G",
                "--vfs-cache-poll-interval",
                "5s"
            ]
        );
    }
}
