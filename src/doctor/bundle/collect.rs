//! Gathers the (already redacted) files of a diagnostics bundle.
use super::redact::{redact_json_bytes, redact_json_lines, redact_text};
use super::{Collected, Entry, Sources};
use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Tail of the current mount log per workspace.
pub(crate) const MOUNT_LOG_BYTES: u64 = 4 * 1024 * 1024;
/// Tail of the previous mount log per workspace.
pub(crate) const PREVIOUS_LOG_BYTES: u64 = 1024 * 1024;
/// Tail of the newest per-minute network history file per workspace.
pub(crate) const HISTORY_TAIL_BYTES: u64 = 256 * 1024;
/// Tail of the operation history (`history.jsonl`).
pub(crate) const OPERATION_HISTORY_BYTES: u64 = 256 * 1024;
/// All log and history tails together.
pub(crate) const TOTAL_LOG_BYTES: u64 = 32 * 1024 * 1024;
/// Workspaces and registry entries looked at.
const MAX_WORKSPACES: usize = 16;
const MAX_REGISTRY_ENTRIES: usize = 64;
/// Small JSON configuration files larger than this are not RPool's.
const MAX_CONFIG_BYTES: u64 = 8 * 1024 * 1024;

/// RPool configuration files copied (JSON-redacted) into `config/`.
const CONFIG_FILES: &[&str] = &[
    "pools.json",
    "gui.json",
    "remote_roots.json",
    "provider_domains.json",
    "integrity.json",
];

pub(crate) fn collect(sources: &Sources) -> Collected {
    let mut out = Collected::default();
    out.entries.push(Entry::new(
        "rpool-build.txt",
        build_info(sources.now_unix).into_bytes(),
        "generated",
    ));
    collect_rclone(sources, &mut out);
    match serde_json::to_vec_pretty(&sources.doctor) {
        Ok(json) => out.entries.push(Entry::new(
            "doctor.json",
            redact_json_bytes(&json),
            "rpool doctor, run while exporting",
        )),
        Err(error) => out.skipped.push(format!("doctor.json: {error}")),
    }
    let dir = &sources.config_dir;
    for name in CONFIG_FILES {
        add_json_file(&mut out, &dir.join(name), &format!("config/{name}"));
    }
    let mut budget = TOTAL_LOG_BYTES;
    add_tail(
        &mut out,
        &dir.join("history.jsonl"),
        "config/history-tail.jsonl",
        OPERATION_HISTORY_BYTES,
        &mut budget,
        true,
    );
    let registry = collect_registry(&mut out, &dir.join(crate::monitor::model::REGISTRY_DIR));
    let workspaces = workspaces(dir, &registry);
    for (index, workspace) in workspaces.iter().enumerate() {
        collect_workspace(&mut out, index, workspace, &mut budget);
    }
    out
}

fn build_info(now_unix: u64) -> String {
    let features: Vec<&str> = [
        ("winfsp", cfg!(feature = "winfsp")),
        ("opendal-prototype", cfg!(feature = "opendal-prototype")),
    ]
    .into_iter()
    .filter_map(|(name, on)| on.then_some(name))
    .collect();
    format!(
        "rpool {}\nprofile: {}\nos: {}\narch: {}\nfamily: {}\nfeatures: {}\nexported_unix: {}\n",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::consts::FAMILY,
        if features.is_empty() {
            "none".to_string()
        } else {
            features.join(", ")
        },
        now_unix,
    )
}

fn collect_rclone(sources: &Sources, out: &mut Collected) {
    let Some(rclone) = sources.rclone else {
        out.skipped
            .push("rclone-version.txt, rclone-config-redacted.txt: --local-only".into());
        return;
    };
    match rclone.version() {
        Ok(text) => out.entries.push(Entry::new(
            "rclone-version.txt",
            redact_text(&text).into_bytes(),
            "rclone version",
        )),
        Err(error) => out.skipped.push(format!(
            "rclone-version.txt: {}",
            redact_text(&format!("{error:#}"))
        )),
    }
    // Only rclone's own redacted view is ever read; never `config dump/show`.
    match rclone.config_redacted() {
        Ok(text) => out.entries.push(Entry::new(
            "rclone-config-redacted.txt",
            redact_text(&text).into_bytes(),
            "rclone config redacted, then RPool's redaction pass",
        )),
        Err(error) => out.skipped.push(format!(
            "rclone-config-redacted.txt: {} (needs rclone v1.64.0 or newer)",
            redact_text(&format!("{error:#}"))
        )),
    }
}

fn read_bounded(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(std::io::Error::other("file too large"));
    }
    Ok(Some(bytes))
}

fn add_json_file(out: &mut Collected, path: &Path, name: &str) {
    match read_bounded(path) {
        Ok(Some(bytes)) => out.entries.push(Entry::new(
            name,
            redact_json_bytes(&bytes),
            &format!("{} (secret fields removed)", path.display()),
        )),
        Ok(None) => out.skipped.push(format!("{name}: not present")),
        Err(error) => out.skipped.push(format!("{name}: {error}")),
    }
}

/// The last `limit` bytes of `path` (from a line start), redacted.
fn add_tail(
    out: &mut Collected,
    path: &Path,
    name: &str,
    limit: u64,
    budget: &mut u64,
    json_lines: bool,
) {
    let limit = limit.min(*budget);
    if limit == 0 {
        out.skipped.push(format!("{name}: total log budget used"));
        return;
    }
    match read_tail(path, limit) {
        Ok(Some((bytes, total))) => {
            *budget = budget.saturating_sub(bytes.len() as u64);
            let text = String::from_utf8_lossy(&bytes);
            let redacted = if json_lines {
                redact_json_lines(&text)
            } else {
                redact_text(&text)
            };
            let note = if total > bytes.len() as u64 {
                format!(
                    "{}: last {} of {} bytes",
                    path.display(),
                    bytes.len(),
                    total
                )
            } else {
                format!("{}: whole file", path.display())
            };
            out.entries
                .push(Entry::new(name, redacted.into_bytes(), &note));
        }
        Ok(None) => out.skipped.push(format!("{name}: not present")),
        Err(error) => out.skipped.push(format!("{name}: {error}")),
    }
}

/// `(tail, file length)`; the tail starts after the first newline when cut.
pub(crate) fn read_tail(path: &Path, limit: u64) -> std::io::Result<Option<(Vec<u8>, u64)>> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Ok(None);
    }
    let total = metadata.len();
    let start = total.saturating_sub(limit);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes)?;
    if start > 0 {
        let cut = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |at| at + 1);
        bytes.drain(..cut);
    }
    Ok(Some((bytes, total)))
}

/// Registry entries (`<config>/mounts/<id>.json`) as redacted JSON; returns
/// their workspaces.
fn collect_registry(out: &mut Collected, dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        out.skipped.push("config/mounts/: not present".into());
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    let mut workspaces = Vec::new();
    for path in files.into_iter().take(MAX_REGISTRY_ENTRIES) {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Ok(Some(bytes)) = read_bounded(&path) {
            if let Ok(entry) = serde_json::from_slice::<crate::monitor::model::MountEntry>(&bytes) {
                workspaces.push(entry.workspace);
            }
        }
        add_json_file(out, &path, &format!("config/mounts/{file_name}"));
    }
    workspaces
}

/// Workspaces of the GUI mount profiles and of running mounts.
fn workspaces(config_dir: &Path, registry: &[String]) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let profiles = read_bounded(&config_dir.join("gui.json"))
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let from_profiles = profiles
        .as_ref()
        .and_then(|gui| gui.get("mount_profiles"))
        .and_then(|profiles| profiles.as_object())
        .into_iter()
        .flat_map(|profiles| profiles.values())
        .filter_map(|profile| profile.get("workspace")?.as_str().map(str::to_string));
    registry
        .iter()
        .cloned()
        .chain(from_profiles)
        .filter(|workspace| !workspace.trim().is_empty())
        .filter(|workspace| seen.insert(workspace.clone()))
        .take(MAX_WORKSPACES)
        .map(PathBuf::from)
        .collect()
}

fn collect_workspace(out: &mut Collected, index: usize, workspace: &Path, budget: &mut u64) {
    let prefix = format!("workspaces/{index}");
    let meta = crate::monitor::files::metadata_dir(workspace);
    out.entries.push(Entry::new(
        &format!("{prefix}/workspace.txt"),
        redact_text(&format!("{}\n", workspace.display())).into_bytes(),
        "workspace path",
    ));
    add_tail(
        out,
        &meta.join("rclone-mount.log"),
        &format!("{prefix}/rclone-mount.log"),
        MOUNT_LOG_BYTES,
        budget,
        false,
    );
    add_tail(
        out,
        &meta.join("rclone-mount.previous.log"),
        &format!("{prefix}/rclone-mount.previous.log"),
        PREVIOUS_LOG_BYTES,
        budget,
        false,
    );
    add_json_file(
        out,
        &crate::monitor::files::status_path(workspace),
        &format!("{prefix}/net-status.json"),
    );
    let history = crate::monitor::files::history_dir(workspace);
    let newest = std::fs::read_dir(&history).ok().and_then(|entries| {
        entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .max()
    });
    match newest {
        Some(path) => {
            let file = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            add_tail(
                out,
                &path,
                &format!("{prefix}/net-history/{file}"),
                HISTORY_TAIL_BYTES,
                budget,
                true,
            );
        }
        None => out
            .skipped
            .push(format!("{prefix}/net-history/: not present")),
    }
}
