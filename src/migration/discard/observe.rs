//! Strict read-only snapshot. Every error, unsupported view or bound violation
//! is uncertainty. Unlike retire's availability readers, no replica may fail.
use super::*;
use super::evidence::{Creation, single_view};
use crate::config_sync::encoding_probe::Probe;
use crate::migration::model::{Plan, Record, RecordState};
use crate::migration::drive_model::DrivePlan;
use crate::migration::retire::{guard, model::RetireObject, refs::References};
use std::time::{Duration, Instant};

/// Pinned namespaces and exact files read in one bounded observation.
pub(super) struct Snapshot {
    pub report: Report,
    pub probes: BTreeMap<String, Probe>,
    pub files: BTreeMap<String, u64>,
    pub docs: BTreeMap<String, Value>,
    raw_hashes: BTreeMap<String, String>,
    pub self_prefixes: Vec<String>,
    pub started: Instant,
    read_bytes: u64,
}

impl Snapshot {
    /// A global ten-minute deadline, plus each child has a 120-second deadline.
    fn budget(&self) -> Result<()> {
        if self.started.elapsed() > Duration::from_secs(600) { bail!("global cleanup observation deadline exceeded"); }
        if self.read_bytes > MAX_READ_BYTES { bail!("cleanup read budget exceeded (512 MiB)"); }
        Ok(())
    }
    /// Exact read; never falls back to a different crypt view or local cache.
    pub(super) fn read(&mut self, address: &str, limit: usize) -> Result<Vec<u8>> {
        self.budget()?;
        let (name, path) = split(address)?;
        let size = *self.files.get(address).context("object not in complete pinned listing")?;
        if size > limit as u64 || self.read_bytes.saturating_add(size) > MAX_READ_BYTES {
            bail!("object/read budget exceeded; retained");
        }
        self.read_bytes += size;
        let bytes = self.probes.get(name).context("unobserved namespace")?.cleanup_read(path, limit)?;
        if bytes.len() as u64 != size { bail!("object size changed during observation"); }
        self.budget()?;
        Ok(bytes)
    }
    /// Open and inventory one crypt namespace, retaining uncertainty on failure.
    fn namespace(&mut self, rclone: &str, name: &str) -> Result<()> {
        if self.probes.contains_key(name) { return Ok(()); }
        self.budget()?;
        if self.probes.len() >= MAX_NAMESPACES { bail!("namespace bound exceeded (16)"); }
        let probe = Probe::new(Path::new(rclone), name)?;
        let binding = format!("{}:{}", probe.current_encoding, probe.binding());
        self.report.namespace_binding.insert(name.into(), binding);
        if let Err(error) = single_view(&probe) {
            self.report.uncertainties.push(format!("{name}: namespace view consistency required: {error:#}"));
        }
        let listed = probe.cleanup_files(&probe.current_encoding)?;
        for item in listed {
            if item.is_dir || item.size < 0 || !safe_path(&item.path) { bail!("invalid namespace listing entry"); }
            let address = format!("{name}:{}", item.path);
            if self.files.insert(address, item.size as u64).is_some() { bail!("duplicate namespace listing address"); }
        }
        probe.ensure_unchanged()?;
        self.probes.insert(name.into(), probe);
        Ok(())
    }
    /// Read every JSON document of the observed namespace, bounded and strict.
    fn documents(&mut self) {
        let addresses: Vec<_> = self.files.keys().filter(|a| a.ends_with(".json") && !self.docs.contains_key(*a)).cloned().collect();
        for address in addresses {
            if self.docs.len() >= MAX_DOCS {
                self.report.uncertainties.push("JSON document bound exceeded (10000)".into()); break;
            }
            let loaded = self.read(&address, MAX_OBJECT_BYTES).and_then(|b| {
                self.raw_hashes.insert(address.clone(), blake3::hash(&b).to_hex().to_string());
                Ok(serde_json::from_slice(&b)?)
            });
            match loaded {
                Ok(value) => { self.docs.insert(address, value); }
                Err(error) => self.report.uncertainties.push(format!("read {address}: {error:#}")),
            }
        }
    }
    /// Every own side document replica must agree; missing copies are allowed
    /// only because all reachable namespaces were fully listed without failure.
    fn own<T: serde::de::DeserializeOwned>(&self, relative: &str) -> Result<Option<T>> {
        let mut found: Option<Value> = None;
        for prefix in &self.self_prefixes {
            let address = format!("{prefix}{relative}");
            if let Some(value) = self.docs.get(&address) {
                if found.as_ref().is_some_and(|f| f != value) { bail!("own {relative} replicas disagree"); }
                found = Some(value.clone());
            } else if self.files.contains_key(&address) { bail!("own {relative} unreadable"); }
        }
        found.map(serde_json::from_value).transpose().map_err(Into::into)
    }
}

/// Public read-only preview: private ephemeral config snapshots only. No local
/// migration cache, plan reservation, identity, lease or history is written.
pub(crate) fn preview(rclone: &str, pool: &str, id: &str, options: &Options) -> Result<Report> {
    Ok(snapshot(rclone, pool, id, options)?.report)
}

/// Internal snapshot also retains pinned readers for confirmed deletion.
pub(super) fn snapshot(rclone: &str, pool: &str, id: &str, options: &Options) -> Result<Snapshot> {
    crate::pool::validate_pool_name(pool)?;
    crate::migration::journal::validate_migration_id(id)?;
    let policy = crate::pool::load_pool_store()?.pools.get(pool).cloned().context("pool not configured")?;
    let mut s = Snapshot { report: Report { version: 1, pool: pool.into(), migration_id: id.into(),
        observed_unix: crate::utils::now_unix(), plan_fingerprint: String::new(),
        namespace_binding: BTreeMap::new(), workspaces: vec![], snapshot_fingerprint: String::new(), reference_fingerprint: String::new(), preview_token: String::new(),
        bounded_completion: false, scope: "Whole configured crypt namespaces; all JSON replicas, all manifest generations, local inventory, supplied workspaces, every other migration regardless of status. No automatic namespace repair. Local temporary private config snapshots only, removed on drop. Remote writers cannot be fenced by this PC; confirmation requires every PC stopped for the entire operation.".into(),
        uncertainties: vec![], copies: vec![], candidates: vec![], candidate_count: 0, candidate_bytes: 0,
        pool_count: 0, pool_bytes: 0, guard_refusal: None, deletion_ready: false, outcomes: BTreeMap::new() },
        probes: BTreeMap::new(), files: BTreeMap::new(), docs: BTreeMap::new(), raw_hashes: BTreeMap::new(), self_prefixes: vec![],
        started: Instant::now(), read_bytes: 0 };
    for root in crate::migration::journal::roots(pool, &policy)? {
        s.self_prefixes.push(format!("{root}/{id}/"));
    }
    let initial = crate::remote_root::apply_remote_roots(policy.remotes.clone())?;
    for root in &initial {
        let (name, _) = split(root)?;
        if let Err(e) = s.namespace(rclone, name) { s.report.uncertainties.push(format!("namespace {name}: {e:#}")); }
    }
    s.documents();
    let plan = s.own::<Plan>("plan.json")?.context("no readable cloud plan for this exact migration")?;
    if plan.migration_id != id || plan.pool != pool || plan.version != 1 { bail!("migration plan binding/version mismatch"); }
    s.report.plan_fingerprint = digest(&plan)?;
    let target_roots = crate::remote_root::apply_remote_roots(plan.target.remotes.clone())?;
    // Removed/current target namespaces must also be observed, never empty by assumption.
    for root in &target_roots {
        let (name, _) = split(root)?;
        if let Err(e) = s.namespace(rclone, name) { s.report.uncertainties.push(format!("namespace {name}: {e:#}")); }
    }
    for root in crate::migration::journal::roots(pool, &plan.target)? {
        let prefix = format!("{root}/{id}/");
        if !s.self_prefixes.contains(&prefix) { s.self_prefixes.push(prefix); }
    }
    s.documents();
    if s.own::<Plan>("plan.json")?.as_ref().map(digest).transpose()?.as_deref() != Some(&s.report.plan_fingerprint) {
        bail!("migration plan changed during observation");
    }
    classify(&mut s, &plan, &target_roots, options)?;
    Ok(s)
}

/// Inspect all own progress records, with strict content addresses and replicas.
fn records(s: &Snapshot) -> Result<Vec<Record>> {
    let mut out = BTreeMap::new();
    for (address, value) in &s.docs {
        if let Some(relative) = s.self_prefixes.iter().find_map(|p| address.strip_prefix(p)) {
            if let Some(rid) = relative.strip_prefix("records/").and_then(|p| p.strip_suffix(".json")) {
                let bytes = serde_json::to_vec(value)?;
                if s.raw_hashes.get(address).map(String::as_str) != Some(rid) { bail!("progress record content address mismatch"); }
                let record: Record = serde_json::from_slice(&bytes)?;
                if rid.len() != 64 || !rid.bytes().all(|b| b.is_ascii_hexdigit()) { bail!("invalid progress record filename"); }
                if out.get(rid).is_some_and(|old| old != &record) { bail!("record replicas disagree"); }
                out.insert(rid.to_string(), record);
            }
        }
    }
    Ok(out.into_values().collect())
}

/// Build exact new-copy candidates; original and foreign reference closures win.
fn classify(s: &mut Snapshot, plan: &Plan, target_roots: &[String], options: &Options) -> Result<()> {
    let records = records(s)?;
    if records.len()>MAX_DOCS {bail!("own progress record bound exceeded");}
    let now = s.report.observed_unix;
    if !records.iter().any(|r| r.state == RecordState::Abandoned && r.entry.is_empty()) {
        bail!("this exact migration is not observed abandoned; nothing can be discarded");
    }
    if s.own::<Value>("drive-adoption.json")?.is_some() { bail!("drive was adopted; discard-copies is forbidden"); }
    let drive = s.own::<DrivePlan>("drive-plan.json")?;
    if let Some(d) = &drive {
        if d.migration_id != plan.migration_id || d.pool != plan.pool || d.version != 1 { bail!("drive plan binding/version mismatch"); }
    }
    let abandoned_at = records.iter().filter(|r| r.state == RecordState::Abandoned).map(|r| r.ts_unix).max().unwrap_or(now);
    if now < abandoned_at.saturating_add(GRACE_SECONDS) {
        s.report.uncertainties.push("seven-day abandonment safety grace has not elapsed".into());
    }
    // Even an expired claim is NOT proof a process stopped; explicit global ack
    // and a local exclusive operation fence are additionally required.
    if records.iter().any(|r| r.state == RecordState::Claimed && now < r.ts_unix.saturating_add(crate::migration::execute::CLAIM_LEASE_SECONDS)) {
        s.report.uncertainties.push("a migration claim lease is still live".into());
    }
    let mut sources: BTreeSet<String> = plan.entries.iter().map(|e| e.archive_id.clone()).collect();
    if let Some(d) = &drive { sources.extend(d.entries.iter().map(|e| e.source_archive_id.clone())); }
    let mut outputs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for record in &records {
        if let Some(id) = &record.new_archive_id { outputs.entry(id.clone()).or_default().insert(record.entry.clone()); }
    }
    if records.iter().any(|r|matches!(r.state,RecordState::Unknown|RecordState::Orphan) && r.new_archive_id.is_none()) {
        // This is informational scope, not a guess at which directories to delete.
        s.report.scope.push_str(" Some unknown/orphan progress has no output id; unidentifiable writes are not cleanup candidates.");
    }
    let mut proofs = BTreeMap::new();
    for (id, entries) in &outputs {
        let mut view = CopyView { archive_id: id.clone(), entry: entries.iter().cloned().collect::<Vec<_>>().join(","), objects: vec![], keep_reasons: vec![] };
        if !safe_id(id) || sources.contains(id) || entries.len() != 1 {
            view.keep_reasons.push("original/reused/ambiguous identity is protected".into());
        } else {
            match s.own::<Creation>(&super::evidence::name(id)) {
                Ok(Some(proof)) => {
                    if let Err(error) = validate_proof(&proof, id, entries.iter().next().unwrap(), plan, drive.as_ref(), &records, target_roots, &s.report.namespace_binding) {
                        view.keep_reasons.push(format!("creation evidence rejected: {error:#}"));
                    } else { proofs.insert(id.clone(), proof); }
                }
                Ok(None) => view.keep_reasons.push("no pre-write absence/ownership receipt; legacy or unknown writes are preserved, even when verified".into()),
                Err(e) => view.keep_reasons.push(format!("creation evidence uncertain: {e:#}")),
            }
        }
        s.report.copies.push(view);
    }
    let owned = |address: &str, id: &str| -> bool {
        proofs.get(id).is_some_and(|p| p.roots.iter().any(|root|
            address.starts_with(&format!("{}/{id}/", root.trim_end_matches('/')))))
    };
    // A non-JSON progress file or malformed side document cannot disappear
    // behind the generic JSON scan. Detect all own unexpected journal objects.
    for address in s.files.keys() {
        if let Some(relative) = s.self_prefixes.iter().find_map(|p|address.strip_prefix(p)) {
            if !relative.ends_with(".json") || (relative.starts_with("records/") && relative[8..].contains('/')) {
                s.report.uncertainties.push(format!("unrecognized own journal object: {address}"));
            }
        }
    }
    let mut refs = References::default();
    let mut protected_objects = BTreeSet::new();
    for proof in proofs.values() {
        refs.add_json("original source payload closure", &serde_json::to_value(&proof.source)?);
        protected_objects.extend(proof.source.shards.iter().map(|shard| shard.object.clone()));
    }
    for (address, value) in &s.docs {
        if s.self_prefixes.iter().any(|p| address.starts_with(p)) { continue; }
        if address.ends_with("/manifest.json") {
            match serde_json::from_value::<Manifest>(value.clone()).and_then(|m| {
                crate::manifest::validate_manifest(&m).map_err(serde::de::Error::custom)?; Ok(m)
            }) {
                Ok(m) => {
                    if !owned(address, &m.archive_id) { refs.add_json(address, value); }
                    for shard in &m.shards {
                        match split(&shard.object) {
                            Ok((name, _)) if s.probes.contains_key(name) => {}
                            _ => refs.uncertain(format!("manifest closure outside observed namespace: {address}")),
                        }
                        if sources.contains(&m.archive_id) { protected_objects.insert(shard.object.clone()); }
                    }
                }
                Err(e) => refs.uncertain(format!("invalid manifest {address}: {e}")),
            }
        } else {
            // ALL other jobs, even complete/abandoned and same source epoch,
            // and every drive generation/history JSON remain live references.
            refs.add_json(address, value);
            if address.ends_with("/plan.json") && address.contains("/migrations-v1/") {
                match serde_json::from_value::<Plan>(value.clone()) {
                    Ok(other) => for root in crate::remote_root::apply_remote_roots(other.target.remotes)? {
                        if !s.probes.contains_key(split(&root)?.0) { refs.uncertain(format!("other migration namespace not observed: {address}")); }
                    },
                    Err(e) => refs.uncertain(format!("other migration plan unreadable: {address}: {e}")),
                }
            }
        }
    }
    // Reject partial/invalid journals of ANY job, including abandoned/completed.
    let mut journals=BTreeSet::new();
    for address in s.files.keys() {
        if let Some((base,tail))=address.split_once("/migrations-v1/") {
            let components:Vec<_>=tail.split('/').collect();
            if components.len()<3 {refs.uncertain(format!("unrecognized migration object: {address}"));continue;}
            let prefix=format!("{base}/migrations-v1/{}/{}/",components[0],components[1]);
            journals.insert(prefix);
            if components.get(2)==Some(&"records") {
                let rid=components.get(3).and_then(|s|s.strip_suffix(".json"));
                if components.len()!=4 || rid.is_none() || s.raw_hashes.get(address).map(String::as_str)!=rid
                    || s.docs.get(address).is_none_or(|v|serde_json::from_value::<Record>(v.clone()).is_err()) {
                    refs.uncertain(format!("invalid/unreadable other migration record: {address}"));
                }
            }
        }
    }
    if journals.len()>128 {refs.uncertain("migration count bound exceeded (128)".into());}
    for prefix in journals {
        let address=format!("{prefix}plan.json");
        if s.docs.get(&address).is_none_or(|v|serde_json::from_value::<Plan>(v.clone()).is_err()) {
            refs.uncertain(format!("migration lacks a readable plan: {prefix}"));
        }
    }
    inventory(s, &mut refs);
    let mut workspace_hashes = BTreeMap::new();
    let workspaces = match known_workspaces(options) {
        Ok(w)=>w,
        Err(e)=>{refs.uncertain(format!("workspace discovery: {e:#}")); options.workspaces.clone()}
    };
    s.report.workspaces=workspaces.clone();
    let mut workspace_budget=MAX_READ_BYTES;
    let mut workspace_entries=0usize;
    for workspace in &workspaces {
        if let Err(e) = workspace_tree(workspace, 0, &mut refs, &mut workspace_hashes, &mut workspace_budget, &mut workspace_entries, s.started) {
            refs.uncertain(format!("local workspace {}: {e:#}", workspace.display()));
        }
    }
    for index in 0..s.report.copies.len() {
        let id = s.report.copies[index].archive_id.clone();
        if !proofs.contains_key(&id) {
            // Inventory the exact tracked output even when it cannot be owned;
            // empty hash explicitly means not read / not eligible for deletion.
            if safe_id(&id) {
                for root in target_roots {
                    let prefix=format!("{}/{id}/",root.trim_end_matches('/'));
                    for (address,size) in s.files.iter().filter(|(a,_)|a.starts_with(&prefix)) {
                        s.report.copies[index].objects.push(Object{address:address.clone(),root:root.clone(),size:*size,
                            blake3:String::new(),archive_id:id.clone()});
                    }
                }
            }
            continue;
        }
        let objects: Vec<_> = s.files.iter().filter(|(a,_)| owned(a, &id)).map(|(a,n)| (a.clone(),*n)).collect();
        let referrers = refs.referrers(&id, objects.iter().map(|(a,_)| a.as_str()));
        s.report.copies[index].keep_reasons.extend(referrers.into_iter().map(|r| format!("referenced: {r}")));
        for (address, size) in objects {
            if protected_objects.contains(&address) {
                s.report.copies[index].keep_reasons.push(format!("original/reused shard address: {address}")); continue;
            }
            match s.read(&address, MAX_OBJECT_BYTES) {
                Ok(bytes) => {
                    let root = proofs[&id].roots.iter().find(|r| address.starts_with(&format!("{}/{id}/",r.trim_end_matches('/')))).unwrap().clone();
                    s.report.copies[index].objects.push(Object { address, root, size, blake3: blake3::hash(&bytes).to_hex().to_string(), archive_id: id.clone() });
                }
                Err(e) => {
                    let why=format!("object identity/read uncertain: {address}: {e:#}");
                    s.report.copies[index].keep_reasons.push(why.clone());
                    s.report.uncertainties.push(why);
                },
            }
        }
    }
    s.report.uncertainties.extend(refs.uncertain);
    for probe in s.probes.values() {
        if let Err(e) = probe.ensure_unchanged() { s.report.uncertainties.push(format!("namespace binding changed: {e:#}")); }
    }
    s.report.uncertainties.sort(); s.report.uncertainties.dedup();
    s.report.bounded_completion = s.report.uncertainties.is_empty();
    for copy in &s.report.copies {
        if copy.keep_reasons.is_empty() { s.report.candidates.extend(copy.objects.clone()); }
    }
    s.report.candidates.sort_by(|a,b| a.address.cmp(&b.address));
    s.report.candidates.dedup_by(|a,b| a.address == b.address);
    s.report.candidate_count = s.report.candidates.len() as u64;
    s.report.candidate_bytes = s.report.candidates.iter().map(|o|o.size).sum();
    s.report.pool_count = s.files.len() as u64;
    s.report.pool_bytes = s.files.values().try_fold(0u64,|sum,n|sum.checked_add(*n).context("pool byte totals overflow; delete refused"))?;
    let objects: Vec<_> = s.report.candidates.iter().map(|o| RetireObject {address:o.address.clone(),root:o.root.clone(),size:o.size}).collect();
    s.report.guard_refusal = guard::check(&objects, guard::Totals { objects:s.report.pool_count, bytes:s.report.pool_bytes },
        crate::migration::retire::model::DEFAULT_MAX_DELETE_PERCENT, crate::migration::retire::model::DEFAULT_MAX_DELETE_OBJECTS);
    let live_docs: BTreeMap<_,_> = s.docs.iter().filter(|(address,_)|
        !proofs.keys().any(|id|owned(address,id))).collect();
    let live_files: BTreeMap<_,_> = s.files.iter().filter(|(address,_)|
        !proofs.keys().any(|id|owned(address,id))).collect();
    s.report.reference_fingerprint = digest(&(&s.report.plan_fingerprint, &s.report.namespace_binding, &live_docs, &live_files, &workspace_hashes, &s.report.workspaces))?;
    s.report.snapshot_fingerprint = digest(&(&s.report.plan_fingerprint, &s.report.namespace_binding, &s.files, &s.docs, &workspace_hashes,
        &s.report.copies, &s.report.workspaces, &s.report.uncertainties, &s.report.guard_refusal))?;
    s.report.preview_token = format!("{}:{}", now, s.report.snapshot_fingerprint);
    s.report.deletion_ready = s.report.bounded_completion && !s.report.candidates.is_empty() && s.report.guard_refusal.is_none();
    Ok(())
}

/// Validate creation proof against BOTH frozen plans and the exact claim/attempt.
#[allow(clippy::too_many_arguments)]
fn validate_proof(p: &Creation, id: &str, entry: &str, plan: &Plan, drive: Option<&DrivePlan>, records: &[Record], roots: &[String], bindings: &BTreeMap<String,String>) -> Result<()> {
    if p.version != 1 || p.migration_id != plan.migration_id || p.pool != plan.pool || p.archive_id != id || p.entry != entry
        || p.plan_fingerprint != digest(plan)? || p.source.archive_id == id || p.roots != roots || p.roots.is_empty() {
        bail!("proof identity/plan/roots mismatch");
    }
    crate::manifest::validate_manifest(&p.source)?;
    let source_fp = crate::manifest::manifest_fingerprint(&p.source)?;
    let planned = plan.entries.iter().any(|e| e.archive_id == entry && e.archive_id == p.source.archive_id && e.fingerprint == source_fp)
        || drive.is_some_and(|d|d.entries.iter().any(|e|e.key == entry && e.source_archive_id == p.source.archive_id && e.fingerprint == source_fp));
    if !planned { bail!("source not bound to ordinary/drive plan entry"); }
    if p.observed_unix < p.claim_unix || p.observed_unix > crate::utils::now_unix() { bail!("invalid absence observation time"); }
    if !records.iter().any(|r|r.state == RecordState::Claimed && r.entry == entry && r.new_archive_id.as_deref() == Some(id)
        && r.attempt_id == p.attempt_id && r.ts_unix == p.claim_unix) { bail!("creation claim missing"); }
    if records.iter().filter(|r|r.new_archive_id.as_deref()==Some(id)).any(|r|r.entry!=entry || r.attempt_id!=p.attempt_id) {
        bail!("output identity is shared by another entry/attempt");
    }
    for root in roots {
        let name = split(root)?.0;
        if p.bindings.get(name) != bindings.get(name) || !p.bindings.contains_key(name) { bail!("creation namespace binding changed"); }
    }
    for r in records.iter().filter(|r|r.new_archive_id.as_deref()==Some(id)) {
        if let Some(location) = &r.new_manifest {
            if !roots.iter().any(|root|location == &format!("{}/{id}/manifest.json",root.trim_end_matches('/'))) {
                bail!("new_manifest is outside exact owned namespace");
            }
        }
    }
    Ok(())
}

/// Every inventory entry is live, including the candidate's own switched entry.
fn inventory(s: &mut Snapshot, refs: &mut References) {
    let result = (|| -> Result<()> {
        let inventory = crate::inventory::load_inventory()?;
        if inventory.entries.len()>MAX_DOCS {bail!("inventory reference count bound exceeded");}
        refs.add_json("local inventory", &serde_json::to_value(&inventory)?);
        for entry in inventory.entries.values() {
            let value = if !Path::new(&entry.manifest_source).is_absolute() && entry.manifest_source.contains(':') {
                serde_json::from_slice(&s.read(&entry.manifest_source, MAX_OBJECT_BYTES)?)?
            } else {
                let path = Path::new(&entry.manifest_source);
                let meta = fs::symlink_metadata(path)?;
                if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_OBJECT_BYTES as u64 { bail!("inventory manifest is not a bounded regular file"); }
                if s.read_bytes.saturating_add(meta.len())>MAX_READ_BYTES {bail!("local inventory aggregate read bound exceeded");}
                s.read_bytes+=meta.len();
                serde_json::from_slice(&fs::read(path)?)?
            };
            refs.add_json("inventory manifest", &value);
            // Include content, not just inventory metadata, in preview identity.
            s.docs.insert(format!("local-inventory:{}", entry.archive_id), value);
        }
        s.docs.insert("local-inventory-store".into(), serde_json::to_value(inventory)?);
        Ok(())
    })();
    if let Err(e) = result { refs.uncertain(format!("inventory references: {e:#}")); }
}

/// Strict bounded workspace scan. No symlinks or silently omitted deep JSON.
fn workspace_tree(root: &Path, depth: usize, refs: &mut References, hashes: &mut BTreeMap<String,String>, budget: &mut u64, entries: &mut usize, started: Instant) -> Result<()> {
    if depth > 12 || hashes.len() >= MAX_DOCS { bail!("workspace traversal bound exceeded"); }
    let meta = fs::symlink_metadata(root)?;
    if meta.file_type().is_symlink() || !meta.is_dir() { bail!("workspace must be an existing real directory"); }
    for item in fs::read_dir(root)? {
        *entries+=1;
        if *entries>20_000 || started.elapsed()>Duration::from_secs(600) { bail!("workspace traversal/deadline bound exceeded"); }
        let item = item?; let path = item.path(); let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() { bail!("workspace contains symlink; reference completeness unknown"); }
        if meta.is_dir() {
            if matches!(item.file_name().to_str(),Some("spool"|"vfs-cache"|"shard-cache")) { continue; }
            workspace_tree(&path, depth+1, refs, hashes, budget, entries, started)?;
        } else if path.extension().is_some_and(|x|x=="json") {
            if meta.len()>MAX_OBJECT_BYTES as u64 || hashes.len()>=MAX_DOCS { bail!("workspace JSON bound exceeded"); }
            if meta.len()>*budget { bail!("workspace aggregate read bound exceeded"); }
            *budget-=meta.len();
            let bytes=fs::read(&path)?; let value:Value=serde_json::from_slice(&bytes)?;
            let label=format!("workspace {}",path.display()); refs.add_json(&label,&value);
            hashes.insert(label,blake3::hash(&bytes).to_hex().to_string());
        }
    }
    Ok(())
}

/// Include all persisted GUI mount workspaces and their adjacent migration/pool
/// backups, without loading defaults, creating directories or swallowing errors.
pub(super) fn known_workspaces(options:&Options)->Result<Vec<PathBuf>> {
    let mut out=options.workspaces.clone();
    let path=crate::config::gui_settings_path()?;
    match fs::symlink_metadata(&path) {
        Ok(meta)=>{
            if !meta.is_file() || meta.file_type().is_symlink() || meta.len()>MAX_OBJECT_BYTES as u64 {bail!("invalid persisted GUI workspace inventory");}
            let value:Value=serde_json::from_slice(&fs::read(&path)?)?;
            if !value.is_object() {bail!("GUI workspace inventory is not an object");}
            if let Some(profiles)=value.get("mount_profiles") {
                let profiles=profiles.as_object().context("GUI mount_profiles is not an object")?;
                for profile in profiles.values() {
                    if !profile.is_object() {bail!("invalid GUI mount profile");}
                    if let Some(workspace)=profile.get("workspace") {
                        let workspace=workspace.as_str().context("GUI workspace is not a string")?;
                        if !workspace.trim().is_empty(){out.push(PathBuf::from(workspace));}
                    }
                }
            }
        }
        Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{},
        Err(e)=>return Err(e.into()),
    }
    let roots=out.clone();
    for root in roots {
        let Some(name)=root.file_name().and_then(|n|n.to_str()) else {bail!("invalid workspace path");};
        let parent=root.parent().filter(|p|!p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let prefixes=[format!(".{name}.migration-backup-"),format!(".{name}.pool-backup-")];
        for (n,item) in fs::read_dir(parent)?.enumerate() {
            if n>20_000 {bail!("workspace backup discovery bound exceeded");}
            let item=item?;let entry_name=item.file_name();
            if entry_name.to_str().is_some_and(|n|prefixes.iter().any(|p|n.starts_with(p))) {out.push(item.path());}
        }
    }
    out.sort();out.dedup();
    if out.len()>128 {bail!("workspace count bound exceeded (128)");}
    Ok(out)
}
