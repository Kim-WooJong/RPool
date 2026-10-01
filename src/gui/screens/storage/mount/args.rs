//! Base `rpool mount` arguments shared by every mount action.
use std::ffi::OsString;
use std::path::Path;

pub(super) fn build_args(
    pool: &str,
    workspace: &Path,
    mountpoint: &str,
    manifests: &[String],
    interval: u64,
    stop: &Path,
    sync_only: bool,
) -> Vec<OsString> {
    let mut args = vec![
        "mount".into(),
        format!("--pool={pool}").into(),
        "--workspace".into(),
        workspace.as_os_str().into(),
        "--interval-seconds".into(),
        interval.to_string().into(),
        "--stop-file".into(),
        stop.as_os_str().into(),
    ];
    if sync_only {
        args.push("--sync-only".into());
    } else {
        args.extend([OsString::from("--mountpoint"), mountpoint.into()]);
    }
    for source in manifests {
        args.push(format!("--manifest={source}").into());
    }
    args
}
