//! Individual crypt edits. No source deletion, key rotation or provider-wide changes.
use super::crypt_secrets::{read_dump, DumpRemote};
use super::plaintext_config::edit_location;
use super::provision::{config_path, Lock};
use super::secret_process::rclone_command;
use crate::models::secrets::validate_remote_name;
use crate::models::sensitive::SensitiveBytes;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

#[derive(serde::Serialize)]
pub(crate) struct Change {
    pub(crate) remote: String,
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) alias: Option<String>,
    pub(crate) source_preserved: bool,
    pub(crate) copied_files: u64,
    pub(crate) copied_bytes: u64,
}

struct Edit {
    config: PathBuf,
    original: SensitiveBytes,
    modified: std::time::SystemTime,
    dump: BTreeMap<String, DumpRemote>,
    _provision: Lock,
    _restore: File,
    _workspaces: Vec<File>,
    _use_lease: File,
}

fn optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if !meta.is_file() || meta.file_type().is_symlink() { bail!("local settings must be regular non-symlink files"); }
            Ok(Some(fs::read(path)?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Fail closed on unreadable registry/leases. Never clean, stop or recover a mount.
fn idle() -> Result<Vec<File>> {
    let dir = crate::config::app_config_dir()?;
    let mut workspaces = BTreeSet::new();
    let registry = dir.join("mounts");
    match fs::read_dir(&registry) {
        Ok(entries) => for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") { continue; }
            let bytes = optional(&path)?.context("mount registry entry disappeared; retry")?;
            let mount: crate::monitor::model::MountEntry = serde_json::from_slice(&bytes).context("unreadable mount registry; stop all mounts and repair registry first")?;
            if crate::monitor::registry::pid_alive(mount.pid) { bail!("A mount is active on this PC; stop all mounts and jobs before editing locations"); }
            workspaces.insert(PathBuf::from(mount.workspace));
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
        Err(e) => return Err(e).context("cannot inspect active mounts"),
    }
    if let Some(bytes) = optional(&dir.join("gui.json"))? {
        let gui: Value = serde_json::from_slice(&bytes).context("invalid GUI settings")?;
        if let Some(profiles) = gui.get("mount_profiles").and_then(Value::as_object) {
            for profile in profiles.values() {
                if let Some(path) = profile.get("workspace").and_then(Value::as_str).filter(|p| !p.is_empty()) { workspaces.insert(PathBuf::from(path)); }
            }
        }
    }
    let mut guards = Vec::new();
    for workspace in workspaces {
        // The durable native lease also fences a surviving child after its parent exits.
        if optional(&workspace.join(".rpool/mount-process.json"))?.is_some() {
            bail!("A workspace has an unresolved mount lease; stop/recover it explicitly before location editing");
        }
        for path in [workspace.join("virtual.lock"), workspace.join(".rpool/mount-process.lock")] {
            if optional(&path)?.is_some() {
                let file = OpenOptions::new().read(true).write(true).open(path)?;
                file.try_lock().context("workspace is active; stop mounts and jobs first")?;
                guards.push(file);
            }
        }
    }
    Ok(guards)
}

impl Edit {
    fn begin(executable: &Path, name: &str, stopped: bool) -> Result<Self> {
        validate_remote_name(name)?;
        if !stopped { bail!("Stop mounts, uploads and jobs on ALL PCs, then confirm with --all-pcs-stopped. RPool never stops processes automatically"); }
        let use_lease = super::location_use::acquire(true)?;
        let config = config_path(executable)?;
        super::transaction::files::regular_file(&config)?;
        let provision = config.with_extension("rpool-provision.lock");
        drop(OpenOptions::new().write(true).create_new(true).open(&provision).context("Another setup is running; inspect stale rpool-provision.lock before retrying")?);
        let provision = Lock(provision);
        let restore_path = super::transaction::files::sibling(&config, ".rpool-lock")?;
        if fs::symlink_metadata(&restore_path).is_ok() { super::transaction::files::regular_file(&restore_path)?; }
        let restore = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(restore_path)?;
        restore.try_lock().context("another config restore is active")?;
        if super::transaction::files::exists(&super::transaction::files::recovery_path(&config)?)? { bail!("Finish interrupted config restoration before editing locations"); }
        let workspaces = idle()?;
        let modified = fs::metadata(&config)?.modified()?;
        let original = SensitiveBytes(fs::read(&config)?);
        if original.0.windows(b"RCLONE_ENCRYPT_".len()).any(|s| s == b"RCLONE_ENCRYPT_") {
            bail!("Encrypted config is not supported by location editing. Keep it encrypted; use the official rclone wizard with a separately verified migration. No plaintext backup was created");
        }
        let dump = read_dump(executable, &config)?;
        if dump.get(name).map(|r| r.kind.as_str()) != Some("crypt") { bail!("exact existing crypt remote name required (not an alias or provider)"); }
        let edit = Self { config, original, modified, dump, _provision: provision, _restore: restore, _workspaces: workspaces, _use_lease: use_lease };
        edit.unchanged()?;
        Ok(edit)
    }
    fn unchanged(&self) -> Result<()> {
        super::transaction::files::regular_file(&self.config)?;
        if fs::metadata(&self.config)?.modified()? != self.modified || SensitiveBytes(fs::read(&self.config)?).0 != self.original.0 { bail!("rclone config changed; edit refused, original source retained"); }
        Ok(())
    }
    fn stage(&self, executable: &Path, bytes: &[u8]) -> Result<tempfile::NamedTempFile> {
        let mut stage = tempfile::NamedTempFile::new_in(self.config.parent().context("config parent missing")?)?;
        stage.write_all(bytes)?;
        stage.as_file().sync_all()?;
        read_dump(executable, stage.path()).context("candidate config validation failed")?;
        Ok(stage)
    }
    fn commit(&self, stage: tempfile::NamedTempFile) -> Result<()> {
        self.unchanged()?;
        // Preserve original permissions; temp files start private and no backup is created.
        fs::set_permissions(stage.path(), fs::metadata(&self.config)?.permissions())?;
        stage.persist(&self.config).map_err(|_| anyhow!("atomic config install failed; previous configuration retained"))?;
        #[cfg(unix)]
        if File::open(self.config.parent().unwrap()).and_then(|directory| directory.sync_all()).is_err() {
            let restored = (|| -> Result<()> {
                let mut rollback = tempfile::NamedTempFile::new_in(self.config.parent().context("config parent missing")?)?;
                rollback.write_all(&self.original.0)?;
                rollback.as_file().sync_all()?;
                rollback.persist(&self.config).map_err(|_| anyhow!("atomic rollback failed"))?;
                Ok(())
            })();
            if restored.is_ok() {
                let _ = File::open(self.config.parent().unwrap()).and_then(|directory| directory.sync_all());
                bail!("config directory sync failed; previous configuration restored, source preserved");
            }
            bail!("config directory sync and rollback failed; inspect saved crypt/alias state before continuing; source is preserved");
        }
        Ok(())
    }
}

struct Local {
    path: PathBuf,
    before: Vec<u8>,
    after: Vec<u8>,
}
fn address(value: &mut Value, old: &str, new: &str) {
    if let Some(text) = value.as_str() {
        if let Some(path) = text.strip_prefix(&format!("{old}:")) { *value = Value::String(format!("{new}:{path}")); }
    }
}
/// Only mutable configuration addresses, never archive/event/journal strings.
fn locals(old: &str, new: &str) -> Result<Vec<Local>> {
    let dir = crate::config::app_config_dir()?;
    let mut files = Vec::new();
    for file in ["remote_roots.json", "provider_domains.json", "pools.json", "gui.json"] {
        let path = dir.join(file);
        let Some(before) = optional(&path)? else { continue; };
        let mut value: Value = serde_json::from_slice(&before).context("invalid local settings; nothing changed")?;
        match file {
            "pools.json" => if let Some(pools) = value.get_mut("pools").and_then(Value::as_object_mut) {
                for pool in pools.values_mut() { if let Some(remotes) = pool.get_mut("remotes").and_then(Value::as_array_mut) { for remote in remotes { address(remote, old, new); } } }
            },
            "gui.json" => if let Some(remotes) = value.get_mut("remotes").and_then(Value::as_array_mut) { for remote in remotes { address(remote, old, new); } },
            "remote_roots.json" | "provider_domains.json" => {
                let key = if file == "remote_roots.json" { "roots" } else { "remotes" };
                if let Some(map) = value.get_mut(key).and_then(Value::as_object_mut) {
                    if map.contains_key(old) {
                        if map.keys().any(|k| k.eq_ignore_ascii_case(new)) { bail!("new name collides with local root/domain identity; nothing changed"); }
                        // Keep the alias identity too: old references must retain the same root/capacity.
                        map.insert(new.into(), map[old].clone());
                    }
                }
            },
            _ => unreachable!(),
        }
        let after = serde_json::to_vec_pretty(&value)?;
        if serde_json::from_slice::<Value>(&before)? != value { files.push(Local { path, before, after }); }
    }
    Ok(files)
}
fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut stage = tempfile::NamedTempFile::new_in(path.parent().context("local settings parent missing")?)?;
    stage.write_all(bytes)?;
    stage.as_file().sync_all()?;
    stage.persist(path).map_err(|_| anyhow!("cannot atomically save local settings"))?;
    Ok(())
}

pub(crate) fn rename(executable: &Path, old: &str, new: &str, stopped: bool) -> Result<Change> {
    validate_remote_name(new)?;
    if new.len() == 1 { bail!("single-letter remote names are ambiguous with Windows drives"); }
    if old.eq_ignore_ascii_case(new) { bail!("same-name and case-only renames are refused"); }
    let edit = Edit::begin(executable, old, stopped)?;
    if edit.dump.keys().any(|n| n.eq_ignore_ascii_case(new)) { bail!("remote name already exists; no overwrite"); }
    let local = locals(old, new)?;
    let bytes = edit_location(&edit.original.0, old, Some(new), None)?;
    let stage = edit.stage(executable, &bytes.0)?;
    let check = read_dump(executable, stage.path())?;
    if check.get(old).map(|r| (r.kind.as_str(), r.remote.as_str())) != Some(("alias", format!("{new}:").as_str())) || check.get(new).map(|r| r.kind.as_str()) != Some("crypt") { bail!("renamed crypt/alias validation failed"); }
    for file in &local { if optional(&file.path)?.as_deref() != Some(file.before.as_slice()) { bail!("local settings changed; nothing replaced"); } }
    // Compatibility metadata is installed before exposing the new crypt name.
    // Old entries remain, so interruption here cannot hide old references.
    let mut metadata_applied: Vec<&Local> = Vec::new();
    for file in local.iter().filter(|f| f.path.file_name().is_some_and(|n| n == "remote_roots.json" || n == "provider_domains.json")) {
        let result = (|| -> Result<()> {
            if optional(&file.path)?.as_deref() != Some(file.before.as_slice()) { bail!("local metadata changed during rename"); }
            atomic(&file.path, &file.after)
        })();
        if let Err(error) = result {
            for previous in metadata_applied.iter().rev() {
                if optional(&previous.path).ok().flatten().as_deref() == Some(previous.after.as_slice()) { let _ = atomic(&previous.path, &previous.before); }
            }
            return Err(error).context("compatibility metadata could not be staged; crypt/old references unchanged");
        }
        metadata_applied.push(file);
    }
    if let Err(error) = edit.commit(stage) {
        // Metadata additions are harmless, and retaining them also covers a
        // successful rename whose final directory fsync alone failed.
        return Err(error).context("inspect crypt+alias state; compatibility metadata retained");
    }
    let mut applied: Vec<&Local> = Vec::new();
    for file in local.iter().filter(|f| f.path.file_name().is_some_and(|n| n == "pools.json" || n == "gui.json")) {
        let result = (|| -> Result<()> {
            if optional(&file.path)?.as_deref() != Some(file.before.as_slice()) { bail!("local settings changed during rename"); }
            atomic(&file.path, &file.after)
        })();
        if result.is_err() {
            for previous in applied.iter().rev() {
                if optional(&previous.path).ok().flatten().as_deref() == Some(previous.after.as_slice()) { let _ = atomic(&previous.path, &previous.before); }
            }
            // Do NOT undo crypt+alias: even a partial local write remains readable via either name.
            bail!("Crypt renamed to {new}, compatibility alias {old}: retained. Local settings update failed; prior local writes rolled back where safe. Both names remain readable; inspect pools/gui/root/domain settings before retrying");
        }
        applied.push(file);
    }
    Ok(Change { remote: new.into(), from: old.into(), to: new.into(), alias: Some(format!("{old}: -> {new}:")), source_preserved: true, copied_files: 0, copied_bytes: 0 })
}

#[derive(serde::Deserialize)]
struct Size { count: u64, bytes: u64 }
fn size(executable: &Path, config: &Path, root: &str) -> Result<Size> {
    let output = rclone_command(executable, config).args(["size", "--json", "--", root]).stdin(Stdio::null()).stderr(Stdio::null()).output().context("cannot start location listing")?;
    if !output.status.success() { bail!("Cannot prove {root} empty/readable (missing, denied or unknown). Create/check the folder explicitly, then retry; configuration unchanged"); }
    serde_json::from_slice(&output.stdout).context("invalid location size response")
}
fn root(path: &str) -> Result<String> {
    if path.trim() != path || path.chars().any(char::is_control) || path.contains('\\') || path.contains(':') { bail!("ambiguous physical path; use a plain slash-separated folder on the same connection"); }
    let parts: Vec<_> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.iter().any(|p| p.trim() != *p) { bail!("path components with surrounding whitespace are refused"); }
    if parts.is_empty() || parts.iter().any(|p| matches!(*p, "." | ".." | "~")) { bail!("connection/drive root and dot-component paths are refused"); }
    // Be conservative on all backends, including case-insensitive providers.
    Ok(parts.join("/").to_lowercase())
}
fn overlap(a: &str, b: &str) -> bool { a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/")) }

pub(crate) fn path(executable: &Path, name: &str, path: &str, copy_existing: bool, stopped: bool) -> Result<Change> {
    let edit = Edit::begin(executable, name, stopped)?;
    let from = edit.dump[name].remote.clone();
    let (provider, old_path) = from.split_once(':').context("crypt backing must be a configured connection and folder")?;
    validate_remote_name(provider)?;
    if provider.len() == 1 { bail!("single-letter connections can be confused with local drives; path edit refused"); }
    let base = edit.dump.get(provider).context("crypt backing connection missing")?;
    if matches!(base.kind.as_str(), "crypt" | "alias" | "chunk" | "chunker" | "union" | "combine" | "hasher" | "compress" | "cache") { bail!("wrapped backing paths are ambiguous; individual path editing requires a direct concrete connection"); }
    let old_key = root(old_path)?;
    let new_key = root(path)?;
    if old_path.starts_with('/') != path.starts_with('/') { bail!("source/destination must use the same absolute/relative path style"); }
    if overlap(&old_key, &new_key) { bail!("same, nested or overlapping source/destination roots are refused"); }
    for (other, remote) in &edit.dump {
        if other == name || remote.kind != "crypt" { continue; }
        let Some((base, folder)) = remote.remote.split_once(':') else { bail!("another crypt location cannot be resolved; path edit refused"); };
        // Unknown wrappers can hide the selected root: refuse rather than infer independence.
        let kind = edit.dump.get(base).map(|r| r.kind.as_str()).unwrap_or("");
        if matches!(kind, "alias" | "crypt" | "chunker" | "union" | "combine" | "hasher" | "compress" | "cache" | "") { bail!("another crypt uses an unresolved wrapper; path overlap cannot be excluded"); }
        if base.eq_ignore_ascii_case(provider) {
            let key = root(folder).context("another crypt uses the connection root; shared-root copying refused")?;
            if overlap(&old_key, &key) || overlap(&new_key, &key) { bail!("source/destination overlaps another crypt location; no copy performed"); }
        }
    }
    // A Drive root_id/SMB share/container is not a movable folder. Require
    // a child below the provider-defined container for namespace backends.
    if matches!(base.kind.as_str(), "s3" | "b2" | "azureblob" | "swift" | "smb") {
        let old_container = old_key.split('/').next().unwrap_or_default();
        let new_container = new_key.split('/').next().unwrap_or_default();
        if !old_key.contains('/') || !new_key.contains('/') || old_container != new_container {
            bail!("connection container/share roots cannot be relocated; choose folders within the same container/share");
        }
    }
    let to = format!("{provider}:{path}");
    let bytes = edit_location(&edit.original.0, name, None, Some(&to))?;
    let stage = edit.stage(executable, &bytes.0)?;
    let candidate = read_dump(executable, stage.path())?;
    if candidate.get(name).map(|r| r.remote.as_str()) != Some(to.as_str()) { bail!("candidate crypt backing validation failed"); }
    // A private plaintext-config snapshot prevents external config edits from
    // redirecting the raw COPY/check while optimistic validation is in flight.
    let snapshot = edit.stage(executable, &edit.original.0)?;
    let source = size(executable, snapshot.path(), &from)?;
    if source.count > 0 && !copy_existing { bail!("Source contains {} files; --copy-existing is required. Source and configuration unchanged", source.count); }
    if size(executable, snapshot.path(), &to)?.count != 0 { bail!("destination is not empty; no overwrite permitted"); }
    edit.unchanged()?;
    let recovery = || format!("Configuration not switched. Original source {from} retained. Partial destination {to} may remain: inspect/recover it manually or choose another empty destination; RPool never deletes it");
    if source.count > 0 {
        let status = rclone_command(executable, snapshot.path()).args(["copy", "--immutable", "--", &from, &to]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().with_context(recovery)?;
        if !status.success() { bail!("Copy failed. {}", recovery()); }
    }
    // Download-based comparison, not provider hashes or count/size alone.
    let status = rclone_command(executable, snapshot.path()).args(["check", "--download", "--", &from, &to]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().with_context(recovery)?;
    if !status.success() { bail!("Download integrity check failed. {}", recovery()); }
    let after = size(executable, snapshot.path(), &from).with_context(recovery)?;
    let target = size(executable, snapshot.path(), &to).with_context(recovery)?;
    if after.count != source.count || after.bytes != source.bytes || target.count != source.count || target.bytes != source.bytes { bail!("Source/destination changed during copy/check. {}", recovery()); }
    edit.commit(stage).with_context(recovery)?;
    Ok(Change { remote: name.into(), from, to, alias: None, source_preserved: true, copied_files: source.count, copied_bytes: source.bytes })
}
