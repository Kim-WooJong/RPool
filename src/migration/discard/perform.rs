//! Confirmed exact allowlist deletion with a local immutable reservation and
//! per-object durable intent/outcome ledger. No cloud journal writes.
use super::*;
use super::observe::{snapshot, Snapshot};

/// A reservation is never edited: only a separate receipt is appended per object.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Reservation {
    version: u32,
    pool: String,
    migration_id: String,
    preview_token: String,
    reference_fingerprint: String,
    objects: Vec<Object>,
    all_pcs_stopped: bool,
    all_local_workspaces_listed: bool,
}

/// Intent and completion receipts have distinct create-only names.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Receipt {
    version: u32,
    object: Object,
    state: String,
    observed_unix: u64,
}

/// Run the read-only preview, or an explicitly confirmed deletion. A local
/// exclusive operation lease cannot fence another PC: the user's global stop
/// acknowledgement remains a required precondition, not an automatic stop.
pub(crate) fn run(rclone: &str, pool: &str, id: &str, options: &Options) -> Result<Report> {
    if !options.confirm { return preview(rclone, pool, id, options); }
    if !options.all_pcs_stopped || !options.all_local_workspaces_listed {
        bail!("--confirm requires --all-pcs-stopped and --all-local-workspaces-listed; stop all mounts/uploads/migrations/writers on every PC and enumerate every local workspace; no automatic stop/adopt/abandon");
    }
    let token = options.preview_token.as_deref().context("--confirm requires --preview-token from a read-only preview")?;
    let (issued, fingerprint) = token.split_once(':').context("invalid preview token")?;
    let issued: u64 = issued.parse()?;
    let now = crate::utils::now_unix();
    if issued > now || now.saturating_sub(issued) > PREVIEW_SECONDS { bail!("preview expired; preview again"); }
    // Application deliberately skips its usual shared lease for this command.
    let _local_writer_fence = crate::config_sync::location_use::acquire(true)?;
    let _workspace_fences = lock_workspaces(&super::observe::known_workspaces(options)?)?;
    let mut initial = snapshot(rclone, pool, id, options)?;
    if initial.report.snapshot_fingerprint != fingerprint { bail!("stale preview: plan/state/binding/object set/bytes/references changed; preview again"); }
    if !initial.report.bounded_completion { bail!("uncertain scope; deletion forbidden: {}", initial.report.uncertainties.join("; ")); }
    if initial.report.guard_refusal.is_some() && !options.force {
        bail!("mass-delete guard: {}; --force only overrides this numerical guard, never ownership/references/uncertainty", initial.report.guard_refusal.as_deref().unwrap());
    }
    if initial.report.candidates.is_empty() { return Ok(initial.report); }
    let dir = ledger_dir(pool, id)?;
    // Before ANY new deletion, reconcile ALL older unresolved intents. An
    // inaccessible object cannot be called already gone or silently retried.
    reconcile(&dir, &mut initial, issued)?;
    let reservation = Reservation { version:1, pool:pool.into(),migration_id:id.into(),preview_token:token.into(),
        reference_fingerprint:initial.report.reference_fingerprint.clone(),objects:initial.report.candidates.clone(),
        all_pcs_stopped:true,all_local_workspaces_listed:true };
    let reservation_id = digest(&reservation)?;
    create_only(&dir.join(format!("plan-{reservation_id}.json")), &reservation)?;
    let mut result = initial.report.clone();
    let expected = initial.report.candidates.clone();
    let reference = initial.report.reference_fingerprint.clone();
    let mut removed = BTreeSet::new();
    for object in &expected {
        // Full re-observation per object: never continue using references from
        // before a previous delete. Interrupted partial manifests are not needed
        // as proof: pre-write receipt already establishes namespace ownership.
        let mut fresh = snapshot(rclone, pool, id, options)?;
        if !fresh.report.bounded_completion || fresh.report.reference_fingerprint != reference {
            bail!("live references/journal/namespace changed or became uncertain; stopped before {}", object.address);
        }
        let remaining: Vec<_> = expected.iter().filter(|o|!removed.contains(&o.address)).cloned().collect();
        if fresh.report.candidates != remaining { bail!("exact candidate object set/content changed; preview again"); }
        let (name,path) = split(&object.address)?;
        let probe = fresh.probes.get(name).context("pinned namespace disappeared")?;
        super::evidence::single_view(probe)?;
        if probe.cleanup_size(path)? != Some(object.size) { bail!("exact object size/presence changed"); }
        let bytes = fresh.read(&object.address, MAX_OBJECT_BYTES)?;
        if blake3::hash(&bytes).to_hex().as_str() != object.blake3 { bail!("exact object bytes changed"); }
        let object_id = digest(&(reservation_id.as_str(), object))?;
        create_only(&dir.join(format!("intent-{object_id}.json")), &Receipt {
            version:1,object:object.clone(),state:"delete-intent-unknown-until-observed".into(),observed_unix:crate::utils::now_unix() })?;
        let probe = fresh.probes.get(name).context("missing namespace")?;
        let deletion = probe.cleanup_delete(path);
        // Even a successful child exit needs an exact not-found observation.
        match probe.cleanup_size(path) {
            Ok(None) => {
                create_only(&dir.join(format!("done-{object_id}.json")), &Receipt {
                    version:1,object:object.clone(),state:"observed-not-found".into(),observed_unix:crate::utils::now_unix() })?;
                removed.insert(object.address.clone());
                result.outcomes.insert(object.address.clone(),"deleted; exact not-found observed".into());
            }
            Ok(Some(_)) | Err(_) => {
                // Intent itself is the durable unknown-outcome receipt. Leave
                // it unresolved; no retry or following item in this invocation.
                bail!("delete outcome unknown/not absent for {}; intent {} preserved; reconcile with a new preview before any retry (child success: {})", object.address,object_id,deletion.is_ok());
            }
        }
    }
    Ok(result)
}

/// Existing workspace lock inodes only; no namespace loads or repair writes.
fn lock_workspaces(workspaces: &[PathBuf]) -> Result<Vec<File>> {
    let mut held=vec![];
    for root in workspaces {
        let path=root.join("virtual.lock");
        let meta=fs::symlink_metadata(&path).context("workspace lock missing; cannot prove stopped")?;
        if !meta.is_file() || meta.file_type().is_symlink() { bail!("workspace lock must be a real file"); }
        let file=OpenOptions::new().read(true).write(true).open(path)?;
        file.try_lock().context("workspace is still mounted/busy")?;
        held.push(file);
    }
    Ok(held)
}

/// Private local ledger. Never follows existing symlink components.
fn ledger_dir(pool:&str,id:&str)->Result<PathBuf> {
    let mut path=crate::config::app_config_dir()?;
    for component in ["discard-copies".to_string(),digest(&(pool,id))?] {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink()=>{},
            Ok(_)=>bail!("unsafe local cleanup ledger directory"),
            Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{
                let mut builder=fs::DirBuilder::new();
                #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; builder.mode(0o700); }
                builder.create(&path)?;
            }
            Err(e)=>return Err(e.into()),
        }
    }
    Ok(path)
}

/// Atomic create-only receipt, fsynced before a delete can be issued.
fn create_only(path:&Path,value:&impl Serialize)->Result<()> {
    let bytes=serde_json::to_vec(value)?;
    if bytes.len()>32*1024*1024 { bail!("cleanup reservation/receipt bound exceeded"); }
    if let Ok(meta)=fs::symlink_metadata(path) {
        if !meta.is_file() || meta.file_type().is_symlink() { bail!("unsafe cleanup receipt"); }
        let existing:Value=serde_json::from_slice(&fs::read(path)?)?;
        if existing != serde_json::to_value(value)? { bail!("immutable receipt already differs; preview/reconcile again"); }
        return Ok(());
    }
    let mut file=tempfile::NamedTempFile::new_in(path.parent().context("ledger parent missing")?)?;
    file.write_all(&bytes)?;file.as_file().sync_all()?;
    file.persist_noclobber(path).map_err(|e|e.error)?;
    #[cfg(unix)] File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

/// Reconcile every prior intent first. A still-present unchanged object receives
/// an explicit reconciliation receipt, allowing only a NEW confirmed attempt.
fn reconcile(dir:&Path,s:&mut Snapshot,preview_issued:u64)->Result<()> {
    let entries=fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    if entries.len()>MAX_DOCS { bail!("local receipt bound exceeded; retain all"); }
    for entry in entries {
        let name=entry.file_name();let Some(name)=name.to_str() else { bail!("invalid ledger filename"); };
        let Some(id)=name.strip_prefix("intent-").and_then(|s|s.strip_suffix(".json")) else { continue; };
        let meta=fs::symlink_metadata(entry.path())?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len()>32*1024*1024 { bail!("unsafe receipt"); }
        let receipt:Receipt=serde_json::from_slice(&fs::read(entry.path())?)?;
        let done=dir.join(format!("done-{id}.json"));
        match fs::symlink_metadata(&done) {
            Ok(meta)=>{
                if !meta.is_file() || meta.file_type().is_symlink() || meta.len()>32*1024*1024 {bail!("unsafe completion receipt");}
                let completed:Receipt=serde_json::from_slice(&fs::read(done)?)?;
                if completed.object!=receipt.object || !completed.state.contains("not-found") {bail!("completion receipt mismatch");}
                continue;
            }
            Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{},
            Err(e)=>return Err(e.into()),
        }
        let (remote,path)=split(&receipt.object.address)?;
        let probe=s.probes.get(remote).context("prior unknown delete outside observed namespace")?;
        match probe.cleanup_size(path)? {
            None=>create_only(&dir.join(format!("done-{id}.json")),&Receipt { version:1,object:receipt.object,
                state:"already-gone: exact not-found observed in reconciliation".into(),observed_unix:crate::utils::now_unix() })?,
            Some(size)=>{
                if size!=receipt.object.size { bail!("prior unknown-delete object changed; retain"); }
                let bytes=s.read(&receipt.object.address,MAX_OBJECT_BYTES)?;
                if blake3::hash(&bytes).to_hex().as_str()!=receipt.object.blake3 { bail!("prior unknown-delete bytes changed; retain"); }
                // Unique receipt per observation; cannot overwrite a past decision.
                let observed=crate::utils::now_unix();
                create_only(&dir.join(format!("reconciled-{id}-{observed}.json")),&Receipt {version:1,object:receipt.object.clone(),
                    state:"still-present-unchanged; new preview/confirmation required before retry".into(),observed_unix:observed})?;
                if preview_issued<=receipt.observed_unix {bail!("prior unknown delete reconciled present; generate a NEW preview before retry");}
            }
        }
    }
    Ok(())
}
