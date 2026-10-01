//! Base `rpool mount` arguments shared by every mount action.
use std::ffi::OsString;
use std::path::Path;

#[allow(clippy::too_many_arguments)] // one parameter per mount CLI flag, mapped 1:1 to arguments
pub(super) fn build_args(
    pool: &str,
    workspace: &Path,
    mountpoint: &str,
    shared_root: &str,
    worker_name: &str,
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
    if !shared_root.is_empty() {
        args.push(format!("--shared-root={shared_root}").into());
    }
    if !worker_name.is_empty() {
        args.push(format!("--worker-name={worker_name}").into());
    }
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
