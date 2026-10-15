//! A native mount whose rclone server is gone.
//!
//! On macOS the kernel keeps a hard NFS mount whose server exited and retries
//! it forever: every process touching the volume (Finder, a shell whose prompt
//! reads the folder) hangs until the volume is force-unmounted or the Mac
//! reboots. Once the server process is proven gone nothing more can reach it,
//! so releasing the volume loses nothing that could still be delivered; saves
//! that reached rclone stay in its VFS cache for the next start's recovery.
//!
//! Only such a dead rclone NFS volume is ever released: an NFS volume from
//! `localhost` that no running rclone serves. A live volume, or any other
//! kind of volume, is never unmounted here.
//!
//! A later start of the workspace proves the previous mount gone the same
//! way (no rclone of this workspace runs, no dead volume of it is left)
//! instead of refusing until someone clears the lease by hand.
use super::*;

/// Grace for rclone's own unmount (Finder Eject) to leave the mount table.
#[cfg(target_os = "macos")]
const DETACH_WAIT: Duration = Duration::from_secs(3);
/// Upper bound for one unmount command; a hung one is abandoned.
#[cfg(target_os = "macos")]
const UNMOUNT_WAIT: Duration = Duration::from_secs(20);

/// Makes sure no volume is left at `target` after its NFS server (the owned
/// rclone, already reaped) exited: waits briefly for rclone's own unmount,
/// then force-unmounts. True when the OS no longer lists a mount there.
#[cfg(target_os = "macos")]
pub(super) fn release_dead_mount(target: &Path) -> Result<bool> {
    let deadline = Instant::now() + DETACH_WAIT;
    while macos_mount_attached(target)? {
        if Instant::now() >= deadline {
            return force_unmount(target);
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(true)
}

/// Runs `program args target`, waiting at most [`UNMOUNT_WAIT`]; a command
/// stuck in kernel I/O is abandoned, never waited for. Returns its stderr
/// when it finished.
#[cfg(target_os = "macos")]
pub(super) fn run_bounded(program: &str, args: &[&str], target: &Path) -> Option<String> {
    let mut errors = tempfile::tempfile().ok()?;
    let mut child = Command::new(program)
        .args(args)
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(errors.try_clone().ok()?))
        .spawn()
        .ok()?;
    let deadline = Instant::now() + UNMOUNT_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    }
    let mut text = String::new();
    errors.seek(SeekFrom::Start(0)).ok()?;
    errors.take(4096).read_to_string(&mut text).ok()?;
    Some(text.trim().to_owned())
}

/// `diskutil unmount force` (what rclone itself uses), then `umount -f`, each
/// bounded; true once the mount table no longer lists `target`. Callers have
/// proven the volume's server gone.
#[cfg(target_os = "macos")]
pub(super) fn force_unmount(target: &Path) -> Result<bool> {
    eprintln!(
        "The NFS server of {} is gone; force-unmounting the volume so Finder and other apps do not hang. Unsent saves stay in the VFS cache for the next start.",
        target.display()
    );
    for (program, args) in [
        ("/usr/sbin/diskutil", &["unmount", "force"][..]),
        ("/sbin/umount", &["-f"][..]),
    ] {
        let errors = run_bounded(program, args, target);
        if !macos_mount_attached(target)? {
            return Ok(true);
        }
        eprintln!(
            "{program} {} {}: {}",
            args.join(" "),
            target.display(),
            match errors.as_deref() {
                None => "did not finish",
                Some("") => "volume still attached",
                Some(text) => text,
            }
        );
    }
    Ok(false)
}

/// Command lines of all processes. `-A -ww` lists every process with its
/// full command line on macOS and Linux (procps).
#[cfg(unix)]
fn process_commands() -> Result<Vec<String>> {
    let output = Command::new("ps")
        .args(["-A", "-ww", "-o", "command="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("cannot list processes; mount lease retained")?;
    if !output.status.success() {
        bail!("cannot list processes; mount lease retained");
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

/// True when no process runs an rclone mount of the workspace whose VFS
/// cache is `cache`. Matching the command line, not a recorded PID, also
/// covers an unknown PID (`Launching`) and a reused one.
#[cfg(unix)]
pub(super) fn no_workspace_rclone(cache: &Path) -> Result<bool> {
    Ok(!process_commands()?
        .iter()
        .any(|command| super::orphan::is_workspace_mount(command, cache)))
}

/// True when some process may serve a mount at `target`: its command line
/// names rclone or `nfsmount` and has `target` as a whole argument. A target
/// that is not valid UTF-8 counts as served (never released).
#[cfg(unix)]
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only macOS releases dead NFS volumes")
)]
pub(super) fn served(commands: &[String], target: &Path) -> bool {
    let Some(target) = target.to_str() else {
        return true;
    };
    let middle = format!(" {target} ");
    let last = format!(" {target}");
    commands.iter().any(|command| {
        (command.contains("rclone") || command.contains("nfsmount"))
            && (command.contains(&middle) || command.ends_with(&last))
    })
}

/// What the OS lists at a mountpoint.
#[cfg(target_os = "macos")]
enum Volume {
    /// Nothing is mounted there.
    Free,
    /// An NFS volume from `localhost` that no running rclone serves: what a
    /// dead rclone NFS server leaves behind. Touching it hangs.
    Dead,
    /// Any other volume, described as `type source`: live, or not rclone's.
    Other(String),
}

/// Classifies the volume at `target` from the mount table (never touching
/// `target` itself) and `commands`.
#[cfg(target_os = "macos")]
fn volume_at(target: &Path, commands: &[String]) -> Result<Volume> {
    Ok(match macos_mount_source(target)? {
        None => Volume::Free,
        Some((fstype, source))
            if fstype == "nfs" && source.starts_with("localhost:") && !served(commands, target) =>
        {
            Volume::Dead
        }
        Some((fstype, source)) => Volume::Other(format!("{fstype} {source}")),
    })
}

/// What is left of a previous mount of this workspace, `None` when it has
/// provably ended: no rclone of the workspace runs and (macOS) no dead
/// rclone NFS volume is left at `targets` (one is force-unmounted first).
/// Live or unrelated volumes there are not this mount's and are left alone.
#[cfg(unix)]
pub(super) fn previous_mount_remains(cache: &Path, targets: &[&Path]) -> Result<Option<String>> {
    let commands = process_commands()?;
    if commands
        .iter()
        .any(|command| super::orphan::is_workspace_mount(command, cache))
    {
        return Ok(Some(
            "an rclone mount of this workspace is still running; unmount it first".into(),
        ));
    }
    #[cfg(target_os = "macos")]
    for target in targets {
        if matches!(volume_at(target, &commands)?, Volume::Dead) && !force_unmount(target)? {
            return Ok(Some(format!(
                "its volume at {0} could not be released; run `diskutil unmount force {0}` and mount again",
                target.display()
            )));
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = targets;
    Ok(None)
}

/// Windows: an unknown launch cannot be proven ended here (no process
/// listing); the lease keeps refusing.
#[cfg(not(unix))]
pub(super) fn previous_mount_remains(_cache: &Path, _targets: &[&Path]) -> Result<Option<String>> {
    Ok(Some(
        "its launch outcome is unknown; confirm no rclone process or drive of this workspace remains, then remove the lease file".into(),
    ))
}

/// macOS, before anything touches `target`: a stat of a volume whose NFS
/// server is gone hangs. A dead rclone NFS volume at the mountpoint this
/// workspace recorded, with no rclone of the workspace running, is what its
/// previous mount left behind and is force-unmounted. Any other volume there
/// (live, another workspace's, another kind) is refused, never unmounted.
#[cfg(target_os = "macos")]
pub(super) fn clear_stale_volume(metadata: &Path, cache: &Path, target: &Path) -> Result<()> {
    // `/a/b/` and `/a/b` name the same mountpoint; the mount table lists `/a/b`.
    let target: PathBuf = target.components().collect();
    let commands = process_commands()?;
    match volume_at(&target, &commands)? {
        Volume::Free => return Ok(()),
        Volume::Other(volume) => bail!(
            "{} already has a mounted volume ({volume}); unmount it or choose another mountpoint",
            target.display()
        ),
        Volume::Dead => {}
    }
    let recorded =
        super::lease::read_identity(metadata)?.is_some_and(|identity| identity.target == target);
    if !recorded {
        bail!(
            "{0} has a disconnected NFS volume that this workspace did not mount; run `diskutil unmount force {0}` or choose another mountpoint",
            target.display()
        );
    }
    if commands
        .iter()
        .any(|command| super::orphan::is_workspace_mount(command, cache))
    {
        bail!(
            "this workspace is still mounted at {}; unmount it before mounting again",
            target.display()
        );
    }
    if !force_unmount(&target)? {
        bail!(
            "the volume left at {0} by the previous mount could not be released; run `diskutil unmount force {0}` and mount again",
            target.display()
        );
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_mountpoint_is_served_only_by_an_rclone_naming_it_as_an_argument() {
        let commands = [
            "/opt/homebrew/bin/rclone nfsmount :webdav: /Users/a/RPool --cache-dir /w/vfs-cache"
                .to_string(),
            "rclone nfsmount :webdav: /Volumes/With Space".to_string(),
            "vim /Users/a/Other".to_string(),
        ];
        assert!(served(&commands, Path::new("/Users/a/RPool")));
        assert!(served(&commands, Path::new("/Volumes/With Space")));
        // A prefix, a non-rclone process, or nothing at all.
        assert!(!served(&commands, Path::new("/Users/a/RPoo")));
        assert!(!served(&commands, Path::new("/Users/a/Other")));
        assert!(!served(&[], Path::new("/Users/a/RPool")));
    }

    #[test]
    fn process_listing_includes_processes_without_a_terminal() {
        let commands = process_commands().unwrap();
        let own = std::env::current_exe().unwrap();
        let name = own.file_name().unwrap().to_str().unwrap();
        assert!(commands.iter().any(|c| c.contains(name)), "{commands:?}");
    }
}
