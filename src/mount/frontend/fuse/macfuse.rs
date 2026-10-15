//! macOS mounting through macFUSE. fuser is built without a mount
//! implementation (`macos-no-mount`), so RPool loads macFUSE's libfuse at
//! runtime, lets it run `mount_macfuse` (`fuse_mount`) and serves the
//! channel's descriptor with `fuser::Session::from_fd`. The mount completes
//! once that session answers the kernel's INIT. Builds and PCs without
//! macFUSE therefore still work; `--frontend auto` uses WebDAV there.
//!
//! macFUSE 5 no longer mounts through `fuse_mount_compat25` (it returns -1
//! at once), which is why every macFUSE mount used to fail and `auto` fell
//! back to WebDAV. The kernel backend is tried first: it is the mature one
//! and works wherever the kernel extension is allowed. The FSKit backend
//! (macFUSE 5+, macOS 15.4+, no kernel extension) is the fallback, only when
//! its file system extension is enabled: while it is disabled, macFUSE waits
//! forever instead of failing. Each attempt is bounded by [`MOUNT_WAIT`];
//! an attempt that does not answer cannot be cancelled safely, so it ends
//! this mount process ([`MountAbandoned`]) instead of falling back.
//!
//! Mounts use `daemon_timeout` from `bounded`: with macFUSE's default (60 s)
//! one slow cloud download made macFUSE eject the whole drive.
use crate::prelude::*;
use std::ffi::{c_char, c_int, c_void, CString};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::time::Duration;

/// Longest wait for macFUSE to hand over a mount channel.
const MOUNT_WAIT: Duration = Duration::from_secs(30);
/// The FSKit module macFUSE uses for RPool's volumes; while it is disabled,
/// an FSKit mount waits forever (macFUSE logs "… macfuse-local not enabled").
const FSKIT_MODULE: &str = "io.macfuse.app.fsmodule.macfuse-local";

/// Marks a mount attempt that macFUSE never answered. It may still complete
/// later and cannot be cancelled safely, so this process must not mount
/// anything else at that mountpoint (`--frontend auto` does not fall back to
/// WebDAV); exiting closes the attempt.
#[derive(Debug)]
pub(crate) struct MountAbandoned;
impl std::fmt::Display for MountAbandoned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("macFUSE did not answer the mount request; stop and mount again")
    }
}

/// macFUSE bundle path; its presence is part of [`installed`].
pub(crate) const BUNDLE: &str = "/Library/Filesystems/macfuse.fs";
/// libfuse 2 dylibs installed by macFUSE, tried in order.
const LIBRARIES: [&str; 2] = [
    "/usr/local/lib/libfuse.2.dylib",
    "/usr/local/lib/libfuse.dylib",
];
/// Hint appended when macFUSE is missing.
const INSTALL_HINT: &str =
    "install macFUSE from https://macfuse.github.io/ and restart RPool, or use --frontend dav";
/// How to enable the macFUSE FSKit file system extension.
const FSKIT_HINT: &str = "open macFUSE once (/Library/Filesystems/macfuse.fs/Contents/Resources/macfuse.app) so it registers its file system extension, then turn macFUSE on under System Settings > General > Login Items & Extensions > File System Extensions (https://github.com/macfuse/macfuse/wiki/Getting-Started)";
/// What the kernel backend needs from macOS security settings.
const KERNEL_HINT: &str = "the kernel backend needs the macFUSE kernel extension allowed in System Settings > Privacy & Security (on Apple silicon also reduced security in Startup Security Utility)";

/// Whether macFUSE's bundle and libfuse are installed (cheap file checks).
pub(crate) fn installed() -> bool {
    Path::new(BUNDLE).is_dir() && LIBRARIES.iter().any(|lib| Path::new(lib).is_file())
}

/// Which macFUSE backend serves the mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Backend {
    /// FSKit file system extension (no kernel extension).
    FsKit,
    /// macFUSE kernel extension.
    Kernel,
}

#[repr(C)]
/// libfuse's `struct fuse_args`.
struct FuseArgs {
    /// Number of arguments.
    argc: c_int,
    /// Argument strings.
    argv: *const *const c_char,
    /// Nonzero if libfuse owns `argv`; 0 here (libfuse may replace it with
    /// its own parsed copy, which it then owns).
    allocated: c_int,
}
/// C signature of libfuse 2.6+ `fuse_mount`: mountpoint and args → channel or NULL.
type MountFn = unsafe extern "C" fn(*const c_char, *mut FuseArgs) -> *mut c_void;
/// C signature of `fuse_chan_fd`: the channel's FUSE descriptor.
type ChanFdFn = unsafe extern "C" fn(*mut c_void) -> c_int;

/// The libfuse entry points RPool uses.
#[derive(Clone, Copy)]
struct Libfuse {
    /// `fuse_mount`.
    mount: MountFn,
    /// `fuse_chan_fd`.
    chan_fd: ChanFdFn,
}

/// `fuse_mount` and `fuse_chan_fd` from a loaded libfuse. The library is
/// never unloaded, so the pointers stay valid for the process.
fn libfuse() -> Result<Libfuse> {
    static LOADED: std::sync::OnceLock<Result<(usize, usize), String>> = std::sync::OnceLock::new();
    let loaded = LOADED.get_or_init(|| {
        let mut failures = Vec::new();
        for path in LIBRARIES {
            let name = CString::new(path).expect("static path");
            // SAFETY: dlopen/dlsym with valid C strings; the handle is leaked
            // on purpose so the symbols stay mapped.
            let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                failures.push(format!("{path}: {}", dl_error()));
                continue;
            }
            let mount = unsafe { libc::dlsym(handle, c"fuse_mount".as_ptr()) };
            let chan_fd = unsafe { libc::dlsym(handle, c"fuse_chan_fd".as_ptr()) };
            if mount.is_null() || chan_fd.is_null() {
                failures.push(format!("{path}: {}", dl_error()));
                continue;
            }
            return Ok((mount as usize, chan_fd as usize));
        }
        Err(failures.join("; "))
    });
    match loaded {
        // SAFETY: the symbols have these C signatures in libfuse 2.6+ (macFUSE).
        Ok((mount, chan_fd)) => Ok(unsafe {
            Libfuse {
                mount: std::mem::transmute::<usize, MountFn>(*mount),
                chan_fd: std::mem::transmute::<usize, ChanFdFn>(*chan_fd),
            }
        }),
        Err(e) => bail!("macFUSE's libfuse cannot be loaded ({e}): {INSTALL_HINT}"),
    }
}

/// Whether macFUSE's FSKit module is enabled ("+" in `pluginkit -m`, set by
/// the switch under System Settings > General > Login Items & Extensions >
/// File System Extensions).
fn fskit_enabled() -> bool {
    std::process::Command::new("/usr/bin/pluginkit")
        .args(["-m", "-i", FSKIT_MODULE])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .is_ok_and(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.trim_start().starts_with('+'))
        })
}

/// Last `dlerror()` message.
fn dl_error() -> String {
    // SAFETY: dlerror returns NULL or a NUL-terminated thread-local string.
    let message = unsafe { libc::dlerror() };
    if message.is_null() {
        "unknown dlopen error".into()
    } else {
        unsafe { std::ffi::CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    }
}

/// libfuse's `-o` option string. Commas and backslashes in the volume name are
/// escaped for libfuse's option parser.
pub(super) fn options(volume: &str, read_only: bool, backend: Backend) -> String {
    let mut volname = String::new();
    for c in volume.chars().filter(|c| !c.is_control()) {
        if c == ',' || c == '\\' {
            volname.push('\\');
        }
        volname.push(c);
    }
    if volname.is_empty() {
        volname.push_str("RPool");
    }
    let mut options = vec![
        "fsname=rpool".to_string(),
        format!("volname={volname}"),
        "default_permissions".into(),
        // Finder metadata: no `._*` / `.DS_Store` files and no com.apple.*
        // xattrs reach the drive (they would sync to every PC).
        "noappledouble".into(),
        "noapplexattr".into(),
        "nodev".into(),
        "nosuid".into(),
        format!("daemon_timeout={}", super::bounded::DAEMON_TIMEOUT_SECONDS),
    ];
    if read_only {
        options.push("ro".into());
    }
    if backend == Backend::FsKit {
        options.push("backend=fskit".into());
    }
    options.join(",")
}

/// `major.minor` from a dotted version such as `15.4.1`.
pub(super) fn major_minor(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |m| m.parse().ok())?;
    Some((major, minor))
}

/// The value of `key` in an XML property list (macFUSE's Info.plist).
pub(super) fn plist_string<'a>(plist: &'a str, key: &str) -> Option<&'a str> {
    let rest = &plist[plist.find(&format!("<key>{key}</key>"))?..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")?;
    Some(rest[start..start + end].trim())
}

/// FSKit needs macOS 15.4+ and macFUSE 5+. Pure, so it is unit tested.
pub(super) fn fskit_supported(macos: Option<(u32, u32)>, macfuse: Option<(u32, u32)>) -> bool {
    matches!(macos, Some(v) if v >= (15, 4)) && matches!(macfuse, Some((major, _)) if major >= 5)
}

/// macOS product version (`kern.osproductversion`) as `(major, minor)`.
fn macos_version() -> Option<(u32, u32)> {
    let mut buffer = [0u8; 32];
    let mut len = buffer.len();
    // SAFETY: sysctlbyname writes at most `len` bytes into `buffer`.
    let status = unsafe {
        libc::sysctlbyname(
            c"kern.osproductversion".as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return None;
    }
    let text = std::str::from_utf8(&buffer[..len]).ok()?;
    major_minor(text.trim_end_matches('\0'))
}

/// Installed macFUSE version from its bundle Info.plist.
fn macfuse_version() -> Option<(u32, u32)> {
    let plist = fs::read_to_string(Path::new(BUNDLE).join("Contents/Info.plist")).ok()?;
    major_minor(plist_string(&plist, "CFBundleShortVersionString")?)
}

/// Calls libfuse's `fuse_mount` with `-o options` and returns a duplicate of
/// the channel's FUSE descriptor. The channel and its own descriptor stay
/// with libfuse (the channel is leaked on purpose): libfuse closes that
/// descriptor itself when the mount fails later, so handing it to fuser could
/// close an unrelated, reused descriptor. Both are close-on-exec, so spawned
/// rclone processes never hold the volume open.
fn mount_blocking(
    lib: Libfuse,
    mountpoint: &CString,
    options: &CString,
) -> std::io::Result<OwnedFd> {
    let argv = [c"rpool".as_ptr(), c"-o".as_ptr(), options.as_ptr()];
    let mut args = FuseArgs {
        argc: argv.len() as c_int,
        argv: argv.as_ptr(),
        allocated: 0,
    };
    // SAFETY: valid C strings that outlive the call; libfuse never frees an
    // `allocated == 0` argv and copies what it keeps.
    let channel = unsafe { (lib.mount)(mountpoint.as_ptr(), &mut args) };
    if channel.is_null() {
        return Err(std::io::Error::other(
            "mount_macfuse refused the mount (its messages are above)",
        ));
    }
    // SAFETY: `channel` is a live libfuse channel.
    let fd = unsafe { (lib.chan_fd)(channel) };
    if fd < 0 {
        return Err(std::io::Error::other(
            "mount_macfuse refused the mount (its messages are above)",
        ));
    }
    // SAFETY: plain fcntl calls on the channel's open descriptor.
    let duplicate = unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0)
    };
    if duplicate < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a fresh descriptor owned only by us.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

/// [`mount_blocking`] bounded by `limit`. On timeout the attempt is left to
/// finish on its own thread (a late duplicate is dropped); the caller must
/// treat it as [`MountAbandoned`].
fn mount_with(
    lib: Libfuse,
    mountpoint: &CString,
    options: &str,
    limit: Duration,
) -> std::io::Result<OwnedFd> {
    let options = CString::new(options).map_err(std::io::Error::other)?;
    let target = mountpoint.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("macfuse-mount".into())
        .spawn(move || {
            // A late descriptor is dropped (closed) with the failed send.
            let _ = sender.send(mount_blocking(lib, &target, &options));
        })?;
    receiver.recv_timeout(limit).unwrap_or_else(|_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("macFUSE did not answer within {}s", limit.as_secs()),
        ))
    })
}

/// Runs `work` on its own thread and waits at most `limit`; `None` when it
/// did not finish (it is left to finish alone).
fn bounded<T: Send + 'static>(
    limit: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("macfuse-probe".into())
        .spawn(move || {
            let _ = sender.send(work());
        })
        .ok()?;
    receiver.recv_timeout(limit).ok()
}

/// What occupies `mountpoint` before a mount.
#[derive(Debug, PartialEq, Eq)]
enum Occupant {
    /// Nothing is mounted there.
    Free,
    /// An RPool macFUSE volume whose serving process is gone (it answers
    /// "Device not configured"), for example after RPool was killed.
    DeadRpool,
    /// Anything else (a live mount, another file system), described.
    Other(String),
}

/// Classifies the mount at `mountpoint`. Only an RPool macFUSE volume is
/// probed (its listing, bounded): a dead one answers ENXIO at once.
fn occupant(mountpoint: &Path) -> Result<Occupant> {
    let Some((fstype, source)) = crate::mount::adapter::macos_mount_source(mountpoint)? else {
        return Ok(Occupant::Free);
    };
    if fstype != "macfuse" || source != "rpool" {
        return Ok(Occupant::Other(format!("{fstype} {source}")));
    }
    let target = mountpoint.to_path_buf();
    let listed = bounded(Duration::from_secs(5), move || {
        std::fs::read_dir(target)
            .map(|_| ())
            .map_err(|e| e.raw_os_error())
    });
    Ok(match listed {
        Some(Err(Some(libc::ENXIO))) => Occupant::DeadRpool,
        Some(Ok(())) => Occupant::Other("a running RPool drive".into()),
        _ => Occupant::Other("an RPool drive that does not answer".into()),
    })
}

/// Makes `mountpoint` free for a new mount: a dead RPool macFUSE volume left
/// by a killed mount process is unmounted (nothing can be served from it any
/// more; local data stays in the workspace); any other mount is refused.
/// Called before anything else touches the mountpoint.
pub(super) fn clear_dead_volume(mountpoint: &Path) -> Result<()> {
    match occupant(mountpoint)? {
        Occupant::Free => Ok(()),
        Occupant::Other(what) => bail!(
            "{} already has a mounted volume ({what}); unmount it or choose another mountpoint",
            mountpoint.display()
        ),
        Occupant::DeadRpool => {
            eprintln!(
                "Removing the disconnected RPool volume left at {} by a mount that ended abruptly",
                mountpoint.display()
            );
            let target = CString::new(mountpoint.as_os_str().as_bytes())
                .map_err(|_| anyhow!("mountpoint contains a NUL character"))?;
            // SAFETY: valid C string; forced, because nothing serves the volume.
            let _ = bounded(Duration::from_secs(10), move || unsafe {
                libc::unmount(target.as_ptr(), libc::MNT_FORCE)
            });
            if occupant(mountpoint)? != Occupant::Free {
                // FSKit volumes belong to the system's file system daemon.
                let path = mountpoint.to_path_buf();
                let _ = bounded(Duration::from_secs(20), move || {
                    std::process::Command::new("/usr/sbin/diskutil")
                        .args(["unmount", "force"])
                        .arg(path)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                });
            }
            if occupant(mountpoint)? != Occupant::Free {
                bail!(
                    "the disconnected RPool volume at {0} could not be removed; run `diskutil unmount force {0}` and mount again",
                    mountpoint.display()
                );
            }
            Ok(())
        }
    }
}

/// Mount `mountpoint` with macFUSE: the kernel backend first, then FSKit if
/// its extension is enabled. Returns the FUSE descriptor and the backend.
pub(super) fn mount(
    mountpoint: &Path,
    volume: &str,
    read_only: bool,
) -> Result<(OwnedFd, Backend)> {
    if !installed() {
        bail!("macFUSE is not installed on this Mac: {INSTALL_HINT}");
    }
    let lib = libfuse()?;
    let target = CString::new(mountpoint.as_os_str().as_bytes())
        .map_err(|_| anyhow!("mountpoint contains a NUL character"))?;
    let kernel = match mount_with(
        lib,
        &target,
        &options(volume, read_only, Backend::Kernel),
        MOUNT_WAIT,
    ) {
        Ok(fd) => return Ok((fd, Backend::Kernel)),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            return Err(anyhow!("macFUSE kernel backend: {e}").context(MountAbandoned))
        }
        Err(e) => e,
    };
    let (macos, macfuse) = (macos_version(), macfuse_version());
    if !fskit_supported(macos, macfuse) {
        bail!(
            "macFUSE mount failed (kernel backend: {kernel}); {KERNEL_HINT}. The FSKit backend needs macOS 15.4+ and macFUSE 5+ (found macOS {}, macFUSE {}). Or use --frontend dav",
            version_text(macos),
            version_text(macfuse)
        );
    }
    if !fskit_enabled() {
        bail!(
            "macFUSE mount failed (kernel backend: {kernel}); {KERNEL_HINT}. Or, to use the FSKit backend instead, {FSKIT_HINT}. Or use --frontend dav"
        );
    }
    eprintln!("macFUSE kernel backend failed ({kernel}); trying the FSKit backend");
    match mount_with(
        lib,
        &target,
        &options(volume, read_only, Backend::FsKit),
        MOUNT_WAIT,
    ) {
        Ok(fd) => Ok((fd, Backend::FsKit)),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            Err(anyhow!("macFUSE FSKit backend: {e}").context(MountAbandoned))
        }
        Err(fskit) => bail!(
            "macFUSE mount failed: kernel backend: {kernel}; FSKit backend: {fskit}. {KERNEL_HINT}. Or use --frontend dav"
        ),
    }
}

/// `major.minor` or `unknown` for messages.
fn version_text(version: Option<(u32, u32)>) -> String {
    version.map_or("unknown".into(), |(major, minor)| {
        format!("{major}.{minor}")
    })
}

/// Unmount a macFUSE volume: unmount(2) first, then `diskutil unmount`
/// (FSKit volumes are owned by the system's file system daemon). Never forced:
/// a busy volume reports an error and stays mounted.
pub(super) fn unmount(mountpoint: &Path) -> Result<()> {
    let target = CString::new(mountpoint.as_os_str().as_bytes())
        .map_err(|_| anyhow!("mountpoint contains a NUL character"))?;
    // SAFETY: valid C string; no flags (not forced).
    if unsafe { libc::unmount(target.as_ptr(), 0) } == 0 {
        return Ok(());
    }
    let direct = std::io::Error::last_os_error();
    if direct.raw_os_error() == Some(libc::EINVAL) && !is_mounted(mountpoint) {
        return Ok(()); // already unmounted (for example from Finder)
    }
    let output = std::process::Command::new("/usr/sbin/diskutil")
        .arg("unmount")
        .arg(mountpoint)
        .output()
        .context("running diskutil unmount")?;
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "unmount {}: {direct}; diskutil: {}",
        mountpoint.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

/// Whether `mountpoint` is the root of a mounted volume (its device differs
/// from its parent's).
fn is_mounted(mountpoint: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let parent = mountpoint.parent().unwrap_or(mountpoint);
    match (fs::metadata(mountpoint), fs::metadata(parent)) {
        (Ok(here), Ok(up)) => here.dev() != up.dev(),
        _ => false,
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn options_escape_the_volume_name_and_select_the_backend() {
        let fskit = options("My, pool\\x", false, Backend::FsKit);
        assert!(fskit.contains("volname=My\\, pool\\\\x,"), "{fskit}");
        assert!(fskit.ends_with(",backend=fskit"), "{fskit}");
        assert!(fskit.contains("noappledouble") && fskit.contains("noapplexattr"));
        assert!(!fskit.contains(",ro"));
        // macFUSE's 60 s default ejected the drive during one slow download.
        assert!(fskit.contains(",daemon_timeout=600,"), "{fskit}");
        let kernel = options("", true, Backend::Kernel);
        assert!(kernel.contains("volname=RPool"), "{kernel}");
        assert!(
            kernel.ends_with(",ro") && !kernel.contains("backend="),
            "{kernel}"
        );
    }

    #[test]
    fn fskit_needs_macos_15_4_and_macfuse_5() {
        assert!(fskit_supported(Some((15, 4)), Some((5, 0))));
        assert!(fskit_supported(Some((27, 0)), Some((5, 4))));
        assert!(!fskit_supported(Some((15, 3)), Some((5, 4))));
        assert!(!fskit_supported(Some((26, 0)), Some((4, 10))));
        assert!(!fskit_supported(None, Some((5, 0))));
        assert!(!fskit_supported(Some((26, 0)), None));
    }

    #[test]
    fn versions_parse_from_sysctl_and_info_plist() {
        assert_eq!(major_minor("15.4.1"), Some((15, 4)));
        assert_eq!(major_minor("27"), Some((27, 0)));
        assert_eq!(major_minor("x.1"), None);
        let plist = "<dict>\n\t<key>CFBundleName</key>\n\t<string>macFUSE</string>\n\t<key>CFBundleShortVersionString</key>\n\t<string>5.4.0</string>\n</dict>";
        assert_eq!(
            plist_string(plist, "CFBundleShortVersionString"),
            Some("5.4.0")
        );
        assert_eq!(plist_string(plist, "Missing"), None);
    }
}
