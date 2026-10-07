//! A mount's rclone that outlived its RPool (crash, SIGKILL): the next mount
//! of the same workspace stops it cleanly instead of refusing to start.
//!
//! Only a process proven to be this workspace's orphan is touched: its
//! command line is an rclone mount naming this workspace's VFS cache
//! (`--cache-dir`) and its parent is no longer a live `rpool`. It gets
//! SIGTERM, on which rclone flushes what it can, unmounts and exits; its
//! unsent saves stay in the VFS cache for the cache recovery of the next
//! start. The orphan counts as stopped only once it has exited and (macOS)
//! the OS no longer lists the mount; otherwise the lease keeps refusing.
use super::*;

/// How long the orphan gets to unmount and exit after SIGTERM.
const STOP_WAIT: Duration = Duration::from_secs(30);

/// `(parent pid, command line)` of a running `pid`, `None` when gone or unknown.
#[cfg(unix)]
fn ps(pid: u32) -> Option<(u32, String)> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid=,command="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let (ppid, command) = text.split_once(char::is_whitespace)?;
    Some((ppid.trim().parse().ok()?, command.trim().to_owned()))
}

/// True when `command` is an rclone mount of the workspace whose VFS cache is `cache`.
pub(super) fn is_workspace_mount(command: &str, cache: &Path) -> bool {
    let Some(cache) = cache.to_str() else {
        return false;
    };
    let mount = command.contains(" nfsmount :webdav: ") || command.contains(" mount :webdav: ");
    mount && command.contains(&format!("--cache-dir {cache}"))
}

/// Stops `pid` if it is this workspace's orphaned rclone mount (see the
/// module docs). `Ok(true)`: it was, and it is gone (and unmounted);
/// `Ok(false)`: not provably ours or still running, leave the lease as is.
#[cfg(unix)]
pub(super) fn stop_orphan(pid: u32, cache: &Path, target: &Path) -> Result<bool> {
    let Some((parent, command)) = ps(pid) else {
        return Ok(false);
    };
    if !is_workspace_mount(&command, cache) {
        return Ok(false);
    }
    let owner_alive = parent > 1
        && ps(parent).is_some_and(|(_, parent_command)| parent_command.contains("rpool"));
    if owner_alive {
        return Ok(false);
    }
    eprintln!(
        "Stopping rclone PID {pid} left running by a previous mount of this workspace that ended abruptly; its unsent saves stay in the VFS cache"
    );
    let _ = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let deadline = Instant::now() + STOP_WAIT;
    while process_alive(pid)? {
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(200));
    }
    #[cfg(target_os = "macos")]
    while lease::macos_mount_attached(target)? {
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(200));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = target;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_workspaces_rclone_mount_matches() {
        let cache = Path::new("/w/ws1/vfs-cache");
        assert!(is_workspace_mount(
            "rclone nfsmount :webdav: /m --vfs-cache-mode full --cache-dir /w/ws1/vfs-cache --rc",
            cache
        ));
        assert!(is_workspace_mount(
            "/usr/bin/rclone mount :webdav: /m --cache-dir /w/ws1/vfs-cache",
            cache
        ));
        // Another workspace, a prefix of the path, or not a mount.
        assert!(!is_workspace_mount(
            "rclone nfsmount :webdav: /m --cache-dir /w/ws2/vfs-cache",
            cache
        ));
        assert!(!is_workspace_mount(
            "rclone rcd --cache-dir /w/ws1/vfs-cache",
            cache
        ));
        assert!(!is_workspace_mount("vim /w/ws1/vfs-cache", cache));
    }

    /// A live process that is not an rclone mount of this workspace is left alone.
    #[cfg(unix)]
    #[test]
    fn a_foreign_live_process_is_never_signalled() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let stopped =
            stop_orphan(child.id(), Path::new("/w/ws1/vfs-cache"), Path::new("/m")).unwrap();
        assert!(!stopped);
        assert!(child.try_wait().unwrap().is_none(), "still running");
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
