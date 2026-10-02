//! macOS mounting through macFUSE. fuser is built without a mount
//! implementation (`macos-no-mount`), so RPool loads macFUSE's libfuse at
//! runtime, lets it run `mount_macfuse` and serves the returned descriptor
//! with `fuser::Session::from_fd`. Builds and PCs without macFUSE therefore
//! still work; `--frontend auto` uses WebDAV there.
//!
//! The FSKit backend (macFUSE 5+, macOS 15.4+) is tried first because it needs
//! no kernel extension; the kernel backend is the fallback.
use crate::prelude::*;
use std::ffi::{c_char, c_int, CString};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;

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
    /// Nonzero if libfuse owns `argv`; always 0 here.
    allocated: c_int,
}
/// C signature of `fuse_mount_compat25`: mountpoint and args → FUSE descriptor or -1.
type MountFn = unsafe extern "C" fn(*const c_char, *const FuseArgs) -> c_int;

/// `fuse_mount_compat25` from a loaded libfuse. The library is never
/// unloaded, so the pointer stays valid for the process.
fn mount_fn() -> Result<MountFn> {
    static LOADED: std::sync::OnceLock<Result<usize, String>> = std::sync::OnceLock::new();
    let loaded = LOADED.get_or_init(|| {
        let mut failures = Vec::new();
        for path in LIBRARIES {
            let name = CString::new(path).expect("static path");
            // SAFETY: dlopen/dlsym with valid C strings; the handle is leaked
            // on purpose so the symbol stays mapped.
            let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if handle.is_null() {
                failures.push(format!("{path}: {}", dl_error()));
                continue;
            }
            let symbol = unsafe { libc::dlsym(handle, c"fuse_mount_compat25".as_ptr()) };
            if symbol.is_null() {
                failures.push(format!("{path}: {}", dl_error()));
                continue;
            }
            return Ok(symbol as usize);
        }
        Err(failures.join("; "))
    });
    match loaded {
        // SAFETY: the symbol has this C signature in libfuse 2.x (macFUSE).
        Ok(symbol) => Ok(unsafe { std::mem::transmute::<usize, MountFn>(*symbol) }),
        Err(e) => bail!("macFUSE's libfuse cannot be loaded ({e}): {INSTALL_HINT}"),
    }
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

/// Calls libfuse's mount with `-o options`; returns the owned FUSE descriptor.
fn mount_with(mount: MountFn, mountpoint: &CString, options: &str) -> std::io::Result<OwnedFd> {
    let options = CString::new(options).map_err(std::io::Error::other)?;
    let argv = [c"rpool".as_ptr(), c"-o".as_ptr(), options.as_ptr()];
    let args = FuseArgs {
        argc: argv.len() as c_int,
        argv: argv.as_ptr(),
        allocated: 0,
    };
    // SAFETY: valid C strings that outlive the call; libfuse copies the
    // arguments it keeps (`allocated == 0`).
    let fd = unsafe { mount(mountpoint.as_ptr(), &args) };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        return Err(match error.raw_os_error() {
            Some(0) | None => std::io::Error::other("mount_macfuse failed"),
            _ => error,
        });
    }
    // SAFETY: libfuse returned a fresh descriptor that we now own.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Mount `mountpoint` with macFUSE, FSKit first. Returns the FUSE descriptor.
pub(super) fn mount(
    mountpoint: &Path,
    volume: &str,
    read_only: bool,
) -> Result<(OwnedFd, Backend)> {
    if !installed() {
        bail!("macFUSE is not installed on this Mac: {INSTALL_HINT}");
    }
    let mount = mount_fn()?;
    let target = CString::new(mountpoint.as_os_str().as_bytes())
        .map_err(|_| anyhow!("mountpoint contains a NUL character"))?;
    let (macos, macfuse) = (macos_version(), macfuse_version());
    let mut fskit_error = None;
    if fskit_supported(macos, macfuse) {
        match mount_with(mount, &target, &options(volume, read_only, Backend::FsKit)) {
            Ok(fd) => return Ok((fd, Backend::FsKit)),
            Err(e) => {
                eprintln!("macFUSE FSKit backend failed ({e}); trying the macFUSE kernel backend. To use FSKit, {FSKIT_HINT}.");
                fskit_error = Some(e);
            }
        }
    } else {
        eprintln!(
            "macFUSE FSKit backend needs macOS 15.4+ and macFUSE 5+ (found macOS {}, macFUSE {}); using the macFUSE kernel backend",
            version_text(macos),
            version_text(macfuse)
        );
    }
    match mount_with(mount, &target, &options(volume, read_only, Backend::Kernel)) {
        Ok(fd) => Ok((fd, Backend::Kernel)),
        Err(e) => match fskit_error {
            Some(fskit) => bail!(
                "macFUSE mount failed: FSKit backend: {fskit}; kernel backend: {e}. To use FSKit, {FSKIT_HINT}; {KERNEL_HINT}. Or use --frontend dav"
            ),
            None => bail!(
                "macFUSE mount failed ({e}); {KERNEL_HINT}. Or use --frontend dav"
            ),
        },
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
