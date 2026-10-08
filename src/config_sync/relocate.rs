//! Moving a provider's storage location (`rpool provider location`): the
//! provider's default path and the backing location of every crypt remote
//! that wraps that path change together. Encrypted files already there are
//! refused unless the caller asks to move them; names stay readable after a
//! move because crypt names are relative to the crypt root.
use super::crypt_secrets::read_dump;
use super::provision::{clean_command, config_path, Lock};
use crate::models::RemoteRootStore;
use crate::remote_root::{
    apply_remote_root_with_store, load_remote_root_store, normalize_root, remote_name,
    save_remote_root_store,
};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// What changing a provider's location found and did.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct LocationChange {
    /// Base provider name.
    pub(crate) provider: String,
    /// Previous location (`provider:path`).
    pub(crate) from: String,
    /// New location.
    pub(crate) to: String,
    /// Crypt remotes repointed from `from` to `to`.
    pub(crate) crypts: Vec<String>,
    /// Crypt remotes over this provider at another path; left unchanged.
    pub(crate) skipped: Vec<String>,
    /// Objects moved from the old location.
    pub(crate) moved_files: u64,
    /// Bytes moved from the old location.
    pub(crate) moved_bytes: u64,
    /// Whether anything was changed (false when the location is the same).
    pub(crate) changed: bool,
    /// The old folder could not be listed (permission denied), so it was
    /// repointed without checking for files; rclone's reason.
    pub(crate) unchecked: Option<String>,
}

/// Objects under a location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
struct Size {
    /// Number of objects.
    count: u64,
    /// Total size in bytes.
    bytes: u64,
}

/// Sets `provider`'s location to `path` (empty = the provider root) and
/// repoints the crypt remotes that wrap the old location. Refused while the
/// old location holds files unless `move_existing`, which first moves them
/// (into an empty new location) with `rclone move`.
pub(crate) fn set_provider_location(
    executable: &Path,
    provider: &str,
    path: &str,
    move_existing: bool,
) -> Result<LocationChange> {
    let provider = remote_name(provider)?.to_string();
    let config = config_path(executable)?;
    if fs::symlink_metadata(&config)?.file_type().is_symlink() {
        bail!("symlink config files are not supported for automatic setup");
    }
    let lock_path = config.with_extension("rpool-provision.lock");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .context("Another setup may be running; retry after it finishes")?;
    let _lock = Lock(lock_path);

    let dump = read_dump(executable, &config)?;
    let kinds: BTreeMap<String, (String, String)> = dump
        .iter()
        .map(|(name, entry)| (name.clone(), (entry.kind.clone(), entry.remote.clone())))
        .collect();
    let mut roots = load_remote_root_store()?;
    let mut change = plan(&provider, path, &kinds, &roots)?;
    if !change.changed {
        return Ok(change);
    }
    if !change.crypts.is_empty() {
        let old = match size(executable, &change.from)? {
            Listing::Known(size) => size,
            Listing::Missing => Size { count: 0, bytes: 0 },
            Listing::Denied(reason) if !move_existing => {
                eprintln!("[location] {} cannot be listed ({reason}); repointing without checking for files", change.from);
                change.unchecked = Some(reason);
                Size { count: 0, bytes: 0 }
            }
            Listing::Denied(reason) => bail!("cannot move files out of {}: it cannot be listed ({reason}). Change the location without moving, or fix the folder's permissions.", change.from),
        };
        if old.count > 0 {
            if !move_existing {
                bail!(
                    "{} already holds {} file(s) ({} bytes) for {}; moving the location would hide them. Move them along (--move-existing) or empty the folder first.",
                    change.from, old.count, old.bytes, change.crypts.join(", ")
                );
            }
            if nested(&change.from, &change.to) {
                bail!("cannot move files between {} and {}: one folder contains the other", change.from, change.to);
            }
            match size(executable, &change.to)? {
                Listing::Known(target) if target.count > 0 => {
                    bail!("{} already holds {} file(s); choose an empty folder", change.to, target.count)
                }
                Listing::Denied(reason) => bail!("cannot move files into {}: it cannot be listed ({reason})", change.to),
                _ => {}
            }
            let mut mover = clean_command(executable);
            mover.args(["move", "--delete-empty-src-dirs", "--"]);
            mover.arg(&change.from).arg(&change.to);
            let status = mover.status().context("cannot start rclone move")?;
            if !status.success() {
                bail!("rclone move from {} to {} failed; nothing was repointed, files may be split between both folders. Re-run to finish the move.", change.from, change.to);
            }
            let left = size(executable, &change.from)?.known(&change.from)?.count;
            let arrived = size(executable, &change.to)?.known(&change.to)?;
            if left != 0 || arrived.count != old.count || arrived.bytes != old.bytes {
                bail!("move check failed: {} file(s) left in {}, {} of {} file(s) in {}; nothing was repointed", left, change.from, arrived.count, old.count, change.to);
            }
            change.moved_files = old.count;
            change.moved_bytes = old.bytes;
        }
    }
    for crypt in &change.crypts {
        let mut update = clean_command(executable);
        update.args(["config", "update", crypt, "remote", &change.to, "--non-interactive"]);
        let output = update.output().context("cannot start rclone config update")?;
        if !output.status.success() {
            bail!("rclone could not update {crypt}");
        }
    }
    let after = read_dump(executable, &config)?;
    for crypt in &change.crypts {
        if after.get(crypt).map(|r| r.remote.as_str()) != Some(change.to.as_str()) {
            bail!("rclone did not record the new location for {crypt}");
        }
    }
    let root = change.to.split_once(':').map(|(_, p)| p).unwrap_or_default();
    if root.is_empty() {
        roots.roots.remove(&provider);
    } else {
        roots.roots.insert(provider, root.to_string());
    }
    save_remote_root_store(&roots)?;
    Ok(change)
}

/// Works out the old and new location and which crypt remotes follow it.
/// `remotes` maps each configured remote to its (type, wrapped remote).
fn plan(
    provider: &str,
    path: &str,
    remotes: &BTreeMap<String, (String, String)>,
    roots: &RemoteRootStore,
) -> Result<LocationChange> {
    let (kind, _) = remotes
        .get(provider)
        .ok_or_else(|| anyhow!("{provider} is not a configured remote"))?;
    if matches!(
        kind.to_ascii_lowercase().as_str(),
        "crypt" | "alias" | "chunker" | "union" | "combine"
    ) {
        bail!("{provider} is a {kind} remote; change the location of the provider it wraps");
    }
    let path = path.trim();
    if path.chars().any(char::is_control) {
        bail!("location must not contain control characters");
    }
    let from = apply_remote_root_with_store(&format!("{provider}:"), roots)?;
    let to = if path.is_empty() {
        format!("{provider}:")
    } else {
        format!("{provider}:{}", normalize_root(path)?)
    };
    let mut change = LocationChange {
        provider: provider.to_string(),
        changed: from != to,
        from,
        to,
        ..LocationChange::default()
    };
    for (name, (kind, wrapped)) in remotes {
        if !kind.eq_ignore_ascii_case("crypt") || remote_name(wrapped).ok() != Some(provider) {
            continue;
        }
        if same_location(wrapped, &change.from) {
            change.crypts.push(name.clone());
        } else {
            change.skipped.push(name.clone());
        }
    }
    Ok(change)
}

/// `a` and `b` name the same folder, ignoring a trailing `/`.
fn same_location(a: &str, b: &str) -> bool {
    a.trim().trim_end_matches('/') == b.trim().trim_end_matches('/')
}

/// Whether one location is inside the other (moving would recurse).
fn nested(a: &str, b: &str) -> bool {
    let a = format!("{}/", a.trim_end_matches('/'));
    let b = format!("{}/", b.trim_end_matches('/'));
    let (a, b) = (a.replace(":/", ":"), b.replace(":/", ":"));
    let root = |s: &str| s.ends_with(":/") || s.ends_with(':');
    root(&a) || root(&b) || a.starts_with(&b) || b.starts_with(&a)
}

/// What `rclone size` found at a location.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Listing {
    /// Objects and bytes under the folder.
    Known(Size),
    /// The folder does not exist (yet).
    Missing,
    /// Listing was refused (permission denied); rclone's reason.
    Denied(String),
}

impl Listing {
    /// Counts a missing folder as empty; a denied listing is an error.
    fn known(self, location: &str) -> Result<Size> {
        match self {
            Listing::Known(size) => Ok(size),
            Listing::Missing => Ok(Size { count: 0, bytes: 0 }),
            Listing::Denied(reason) => bail!("cannot list {location}: {reason}"),
        }
    }
}

/// `rclone size --json` of a location. Exit code 3 or a "not found" error is
/// a missing folder, a permission error is [`Listing::Denied`]; other
/// failures carry rclone's error lines.
fn size(executable: &Path, location: &str) -> Result<Listing> {
    let mut cmd = clean_command(executable);
    cmd.args(["size", "--json", "--"]).arg(location);
    let output = cmd.output().context("cannot start rclone size")?;
    if output.status.code() == Some(3) {
        return Ok(Listing::Missing);
    }
    if !output.status.success() {
        return classify_failure(location, error_lines(&output.stderr));
    }
    serde_json::from_slice(&output.stdout)
        .map(Listing::Known)
        .context("invalid rclone size response")
}

/// Sorts a failed listing by rclone's error text.
fn classify_failure(location: &str, detail: String) -> Result<Listing> {
    let lower = detail.to_ascii_lowercase();
    if lower.contains("permission denied") || lower.contains("access denied") || lower.contains("forbidden") {
        return Ok(Listing::Denied(detail));
    }
    if lower.contains("not found") {
        return Ok(Listing::Missing);
    }
    bail!("cannot list {location}: {detail}")
}

/// The last few non-empty lines of rclone's stderr, joined with ` | `.
fn error_lines(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    match lines.len() {
        0 => "rclone gave no error text".to_string(),
        n => lines[n.saturating_sub(3)..].join(" | "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_failures_are_classified() {
        let denied = classify_failure("nas:/mnt/HDD1", "ERROR : lost+found: permission denied".into()).unwrap();
        assert!(matches!(denied, Listing::Denied(_)));
        assert!(denied.known("nas:/mnt/HDD1").is_err());
        assert_eq!(classify_failure("nas:/x", "directory not found".into()).unwrap(), Listing::Missing);
        assert!(classify_failure("nas:/x", "connection refused".into()).is_err());
        assert_eq!(Listing::Missing.known("nas:/x").unwrap(), Size { count: 0, bytes: 0 });
    }

    #[test]
    fn error_lines_keeps_the_last_three() {
        assert_eq!(error_lines(b""), "rclone gave no error text");
        assert_eq!(error_lines(b"a\n\nb\nc\nd\n"), "b | c | d");
    }

    fn remotes(entries: &[(&str, &str, &str)]) -> BTreeMap<String, (String, String)> {
        entries
            .iter()
            .map(|(n, k, r)| (n.to_string(), (k.to_string(), r.to_string())))
            .collect()
    }

    fn roots(entries: &[(&str, &str)]) -> RemoteRootStore {
        let mut store = RemoteRootStore::default();
        for (n, p) in entries {
            store.roots.insert(n.to_string(), p.to_string());
        }
        store
    }

    #[test]
    fn plan_repoints_only_crypts_at_the_old_location() {
        let all = remotes(&[
            ("nas", "sftp", ""),
            ("nas_crypt", "crypt", "nas:/disk1/rpool"),
            ("nas_old", "crypt", "nas:/elsewhere"),
            ("box_crypt", "crypt", "box:rpool"),
        ]);
        let change = plan("nas", "/disk2/rpool/", &all, &roots(&[("nas", "/disk1/rpool")])).unwrap();
        assert_eq!(change.from, "nas:/disk1/rpool");
        assert_eq!(change.to, "nas:/disk2/rpool");
        assert_eq!(change.crypts, ["nas_crypt"]);
        assert_eq!(change.skipped, ["nas_old"]);
        assert!(change.changed);
    }

    #[test]
    fn plan_without_root_and_to_provider_root() {
        let all = remotes(&[("nas", "sftp", ""), ("nas_crypt", "crypt", "nas:")]);
        let change = plan("nas", "/d1", &all, &RemoteRootStore::default()).unwrap();
        assert_eq!((change.from.as_str(), change.to.as_str()), ("nas:", "nas:/d1"));
        assert_eq!(change.crypts, ["nas_crypt"]);
        let back = plan("nas", "  ", &all, &roots(&[("nas", "/d1")])).unwrap();
        assert_eq!(back.to, "nas:");
        assert!(back.crypts.is_empty() && back.skipped == ["nas_crypt"]);
        let same = plan("nas", "/d1/", &all, &roots(&[("nas", "/d1")])).unwrap();
        assert!(!same.changed);
    }

    #[test]
    fn plan_rejects_wrappers_and_unknown_remotes() {
        let all = remotes(&[("nas_crypt", "crypt", "nas:")]);
        assert!(plan("nas_crypt", "/x", &all, &RemoteRootStore::default()).is_err());
        assert!(plan("missing", "/x", &all, &RemoteRootStore::default()).is_err());
    }

    #[test]
    fn nested_locations_are_detected() {
        assert!(nested("nas:/a", "nas:/a/b"));
        assert!(nested("nas:/a/b/", "nas:/a"));
        assert!(nested("nas:", "nas:/a"));
        assert!(!nested("nas:/a", "nas:/ab"));
        assert!(!nested("nas:/disk1/rpool", "nas:/disk2/rpool"));
    }
}
