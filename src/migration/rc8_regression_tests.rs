//! Independent RC.8 regression inputs. Register under migration, not plan.
//! No live cloud or user configuration. Most cases are memory-only; the Unix
//! drive case uses an owned tempdir and env-cleared child with a fake executable.
//! No process-global environment mutation and no real rclone/network fallback.
//! These assertions describe required behavior, not the observed broken output.
use super::enumerate::{Cloud, CopyFeatures, RemoteListing};
use super::model::{Action, Entry, MinimalAck, Record, RecordState};
use super::plan::{plan_listed, plan_with, PlanOptions};
use super::storage_budget::*;
use super::test_support::{manifest, policy, FakeCloud};
use crate::prelude::*;
use crate::storage::admin::budget::TargetBudget;

const MIB: u64 = 1 << 20;
const ROOTS: [&str; 4] = ["rc8-a:fixture", "rc8-b:fixture", "rc8-c:fixture", "rc8-d:fixture"];
const SMB_CAUSE: &str = "SMB response: I/O error not defined in standard FsRtl filter; information lost during conversion";

// Unlike FakeCloud::read, this adapter never checks Path::exists or reads a
// local inventory file. Missing/unavailable objects are errors, never empties.
struct MemoryCloud {
    inner: FakeCloud,
}
impl MemoryCloud {
    fn with_source(source: &Manifest) -> Self {
        let mut inner = FakeCloud::default();
        inner.store(source, &ROOTS);
        Self { inner }
    }
}
impl Cloud for MemoryCloud {
    fn configured(&self) -> Option<BTreeSet<String>> {
        Some(ROOTS.iter().map(|r| r.split(':').next().unwrap().to_owned()).collect())
    }
    fn list(&self, remote: &str) -> RemoteListing {
        self.inner.lists.lock().unwrap().push(remote.to_owned());
        self.inner.listings.get(remote).cloned()
            .unwrap_or_else(|| RemoteListing::Failed(format!("unavailable fixture root: {remote}")))
    }
    fn read(&self, address: &str) -> Result<Vec<u8>> {
        self.inner.reads.lock().unwrap().push(address.to_owned());
        self.inner.files.get(address).cloned().context("fixture metadata unavailable")
    }
    fn probe_full(&self, _: &Manifest, _: usize) -> Result<Vec<(Shard, Probe)>> {
        bail!("fixture does not permit a full probe")
    }
    fn failure_domain(&self, remote: &str) -> Option<String> { Some(remote.to_owned()) }
    fn copy_features(&self, _: &str) -> Option<CopyFeatures> { None }
    fn quota_ok(&self, _: &PoolDefinition, _: &[Vec<PhysicalSpec>]) -> Option<bool> { Some(true) }
    fn quotas(&self, target: &PoolDefinition) -> Option<Vec<TargetBudget>> {
        Some(budgets(target))
    }
}
fn budgets(target: &PoolDefinition) -> Vec<TargetBudget> {
    target.remotes.iter().map(|remote| TargetBudget {
        remote: remote.clone(), backing: remote.clone(),
        capacity_domain: format!("capacity/{remote}"),
        failure_domain: Some(remote.clone()), declared: true,
        total: 1 << 40, free: 1 << 39, shard_cap: None,
    }).collect()
}
fn source() -> Manifest { manifest("rc8-fixed-source", 2 * MIB, MIB, 2, 1, &ROOTS[..3]) }
fn planned(cloud: &MemoryCloud, target: PoolDefinition, options: &PlanOptions) -> super::model::Plan {
    plan_with(cloud, "rc8-fixture-pool", target, &InventoryStore::default(), options).unwrap()
}
fn only_entry(plan: &super::model::Plan) -> &Entry {
    assert_eq!(plan.entries.len(), 1);
    &plan.entries[0]
}
fn full_entry(source: &Manifest, target: &PoolDefinition) -> Entry {
    let mut entry = super::execute::tests_support::entry(&source.archive_id, Action::Relocate, source.original_size);
    entry.fingerprint = crate::manifest::manifest_fingerprint(source).unwrap();
    entry.exact = Some(super::exact::freeze(source, target, &mut entry.moves, false, &BTreeSet::new()).unwrap());
    entry.exact.as_mut().unwrap().bindings = budgets(target).into_iter()
        .map(|q| (q.remote, (q.backing, q.capacity_domain, q.failure_domain))).collect();
    entry
}

#[test]
fn rc8_normal_removed_root_freezes_every_full_copy_destination_without_rebalance() {
    let source = source();
    let cloud = MemoryCloud::with_source(&source);
    let before = cloud.inner.files.clone();
    let target = policy(&[ROOTS[0], ROOTS[1], ROOTS[3]], MIB, 2, 1);
    let options = PlanOptions::default();
    assert!(!options.minimal_transfer && !options.rebalance);
    let plan = planned(&cloud, target, &options);
    let entry = only_entry(&plan);
    assert_eq!(entry.action, Action::Relocate);
    let exact = entry.exact.as_ref().expect("ordinary removed-root relocation needs a usable output contract");
    assert_eq!(exact.objects.len(), source.shards.len());
    assert!(exact.objects.iter().all(|o| !o.keep_ref && plan.target.remotes.contains(&o.destination_root)));
    assert_eq!(entry.moves.len(), source.shards.len(), "all execution destinations must be frozen");
    super::exact::validate(entry, &source, &plan.target).unwrap();
    let preview = super::storage_preview::preview(&budgets(&plan.target), plan.entries.clone());
    assert!(preview.admitted, "ordinary FullCopy must not be unsupported: {preview:?}");
    assert_eq!(cloud.inner.files, before, "planning must retain every original object");
}

#[test]
fn rc8_new_reencode_and_reapply_cost_whole_outputs_and_are_admissible() {
    let source = source();
    let cloud = MemoryCloud::with_source(&source);
    // With fixed 1 MiB shards, 2 MiB at K=1,M=2 produces six MiB, not
    // the original three MiB. Reapply K=2,M=1 still recreates all three MiB.
    for (reapply, k, m, expected_payload) in [(false, 1, 2, 6 * MIB), (true, 2, 1, 3 * MIB)] {
        let mut target = policy(&ROOTS, MIB, k, m);
        target.placement = Placement::FreeRatioUnbounded;
        let options = PlanOptions { reapply_placement: reapply, ..Default::default() };
        let plan = planned(&cloud, target, &options);
        let entry = only_entry(&plan);
        assert_eq!(entry.action, if reapply { Action::ReapplyPlacement } else { Action::Reencode });
        let specs = super::storage_preview::specs(entry, &budgets(&plan.target), "rc8-new-output")
            .expect("normal new full-output plans cannot be permanently blocked by exact=None");
        let cost = calculate_entry_cost(&specs).unwrap();
        assert!(cost.outputs.iter().all(|o| o.spec.ownership == OutputOwnership::Created));
        let payload_created: u64 = cost.outputs.iter()
            .filter(|o| !o.spec.object_id.contains("/metadata/"))
            .map(|o| o.actual_created_bytes).sum();
        assert!(payload_created >= expected_payload, "old source layout must not substitute for new payload: {cost:?}");
        let created: u64 = cost.additional_physical_bytes.values().sum();
        assert!(created > expected_payload, "whole output plus nonzero encryption/metadata overhead: {cost:?}");
        assert!(plan.new_storage_bytes >= expected_payload);
        let preview = super::storage_preview::preview(&budgets(&plan.target), plan.entries.clone());
        assert!(preview.admitted, "complete target quotas and full output cost must admit: {preview:?}");
    }
}

#[test]
fn rc8_worker_only_changes_preserve_exact_but_layout_or_source_changes_fail() {
    let source = source();
    let target = policy(&ROOTS[..3], MIB, 2, 1);
    let entry = full_entry(&source, &target);
    super::exact::validate(&entry, &source, &target).unwrap();
    for workers in [1, 3, 17] {
        // This is the target mutation RunOptions::throttle performs for
        // --transfers; no bandwidth process-global setting is touched.
        let mut throttled = target.clone();
        throttled.workers = workers;
        super::exact::validate(&entry, &source, &throttled)
            .expect("transfer/worker scheduling alone is not output layout");
    }
    let mut changed_targets = Vec::new();
    let mut changed = target.clone(); changed.remotes[2] = ROOTS[3].into(); changed_targets.push(changed);
    let mut changed = target.clone(); changed.data_shards = 1; changed_targets.push(changed);
    let mut changed = target.clone(); changed.parity_shards = 2; changed_targets.push(changed);
    let mut changed = target.clone(); changed.placement = Placement::FreeRatioUnbounded; changed_targets.push(changed);
    let mut changed = target.clone(); changed.native_crypt = !changed.native_crypt; changed_targets.push(changed);
    for changed in changed_targets {
        assert!(super::exact::validate(&entry, &source, &changed).is_err(), "real layout changes must require replan");
    }
    let mut changed_source = source.clone();
    changed_source.created_unix += 1;
    assert!(super::exact::validate(&entry, &changed_source, &target).is_err(), "worker exemption must not bypass source fingerprint");
}

#[test]
fn rc8_full_entry_scratch_is_not_source_bytes_times_created_shards() {
    let source = source();
    let target = policy(&ROOTS[..3], MIB, 2, 1);
    let entry = full_entry(&source, &target);
    let specs = super::storage_preview::specs(&entry, &budgets(&target), "rc8-scratch").unwrap();
    let cost = calculate_entry_cost(&specs).unwrap();
    let source_bytes: u64 = source.shards.iter().map(|s| s.size).sum();
    let metadata_scratch: u64 = specs.iter().filter(|o| o.object_id.contains("/metadata/"))
        .map(|o| o.local_scratch_bytes.unwrap()).sum();
    // The known full-entry staging bound is 3 * source_bytes. The bound
    // belongs to the entry, not to each of its N created shard outputs.
    assert!(cost.local_scratch_peak_bytes >= source_bytes, "do not fix duplicate charge by dropping scratch to zero");
    assert!(cost.local_scratch_peak_bytes <= 3 * source_bytes + metadata_scratch,
        "entry staging duplicated once per created shard: {cost:?}");
    let declared = entry.exact.as_ref().unwrap().additional_bytes;
    let charged: u64 = cost.additional_physical_bytes.values().sum();
    assert!(charged <= declared, "crypto overhead must not be charged again as metadata: charged={charged} declared={declared}");
}

#[derive(Default)]
struct ResumeTrace {
    appended: Vec<Record>,
    fingerprinted: Vec<String>,
    built: Vec<String>,
    reverified: Vec<String>,
    switched: Vec<String>,
    ids: u64,
}
#[derive(Default)]
struct MemoryEffects { trace: Mutex<ResumeTrace> }
impl super::execute::Effects for MemoryEffects {
    fn append(&self, r: &Record) -> Result<()> {
        self.trace.lock().unwrap().appended.push(r.clone()); Ok(())
    }
    fn source_fingerprint(&self, e: &Entry) -> Result<String> {
        self.trace.lock().unwrap().fingerprinted.push(e.archive_id.clone());
        Ok(e.fingerprint.clone())
    }
    fn build(&self, e: &Entry, id: &str) -> Result<super::execute::Replacement> {
        self.trace.lock().unwrap().built.push(e.archive_id.clone());
        Ok(super::execute::Replacement { new_archive_id: id.into(), new_manifest: format!("rc8-output:{id}/manifest.json") })
    }
    fn reverify(&self, e: &Entry, _: &str, _: &str) -> Result<()> {
        self.trace.lock().unwrap().reverified.push(e.archive_id.clone()); Ok(())
    }
    fn switch(&self, e: &Entry, _: &super::execute::Replacement) -> Result<()> {
        self.trace.lock().unwrap().switched.push(e.archive_id.clone()); Ok(())
    }
    fn stop_requested(&self) -> bool { false }
    fn now(&self) -> u64 { 1_000_000 }
    fn new_id(&self) -> Result<String> {
        let mut trace = self.trace.lock().unwrap(); trace.ids += 1; Ok(format!("fixed-id-{}", trace.ids))
    }
    fn say(&self, _: &str) {}
}

#[test]
fn rc8_resume_core_never_rebuilds_switched_or_verified_legacy_entries() {
    use super::execute::tests_support::{entry, plan, rec};
    let plan = plan(vec![entry("done", Action::Relocate, 1), entry("verified", Action::Relocate, 2), entry("pending", Action::Relocate, 3)]);
    assert!(plan.entries.iter().all(|e| e.exact.is_none()), "legacy fixture intentionally has no new exact contracts");
    let legacy_bytes = serde_json::to_vec(&plan).unwrap();
    let records = vec![rec("done", RecordState::Switched, 10, "old-pc"), rec("verified", RecordState::Verified, 20, "old-pc")];
    let effects = MemoryEffects::default();
    let summary = super::execute::run_core(&plan, &records, "fixture-pc", &effects, Some(1)).unwrap();
    assert_eq!(summary.already_switched, 1);
    assert_eq!(summary.switched_now, 2);
    let trace = effects.trace.lock().unwrap();
    assert_eq!(trace.built, ["pending"]);
    assert_eq!(trace.fingerprinted, ["pending"]);
    assert_eq!(trace.reverified, ["verified"]);
    assert_eq!(trace.appended.iter().filter(|r| r.entry == "done").count(), 0, "already switched has zero writes");
    assert_eq!(trace.appended.iter().filter(|r| r.entry == "verified").count(), 1, "verified appends only switched, no new claim/build");
    assert_eq!(serde_json::to_vec(&plan).unwrap(), legacy_bytes, "the immutable input plan remains unchanged");
    drop(trace);
    let complete: Vec<_> = ["done", "verified", "pending"].into_iter()
        .map(|id| rec(id, RecordState::Switched, 30, "old-pc")).collect();
    let effects = MemoryEffects::default();
    let summary = super::execute::run_core(&plan, &complete, "fixture-pc", &effects, Some(1)).unwrap();
    assert_eq!(summary.already_switched, 3);
    let trace = effects.trace.lock().unwrap();
    assert!(trace.appended.is_empty() && trace.built.is_empty() && trace.switched.is_empty() && trace.fingerprinted.is_empty() && trace.reverified.is_empty());
    assert_eq!(trace.ids, 0, "completed resume must not start even an attempt");
}

// Top-level resume (2026-11-20 adapter, replaces the former API-gap note):
// the production `execute::run` now calls `load_resume` + `resume_entries`
// over its opened Journal. This drives those same functions over a real
// `Journal` whose write-once stores count every create, with effects that
// append through the journal exactly as `LiveEffects` does. Output
// preparation/admission happens only inside `Effects::build` (production:
// `LiveEffects::build` → `exact::prepare` + `storage_preview::admit_entry`),
// so `admitted` below is the set of entries that would be charged.
#[derive(Default)]
struct CountingStore { objects: Mutex<BTreeMap<String, Vec<u8>>>, creates: Mutex<Vec<String>> }
impl super::journal::JournalStore for CountingStore {
    fn label(&self) -> String { "rc8-counting:".into() }
    fn list_migrations(&self) -> Result<Vec<String>> {
        Ok(self.objects.lock().unwrap().keys().filter_map(|k| k.split_once('/').map(|(m, _)| m.to_owned())).collect::<BTreeSet<_>>().into_iter().collect())
    }
    fn read(&self, migration: &str, rel: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.objects.lock().unwrap().get(&format!("{migration}/{rel}")).cloned())
    }
    fn create(&self, migration: &str, rel: &str, bytes: &[u8]) -> Result<Option<Vec<u8>>> {
        self.creates.lock().unwrap().push(rel.to_owned());
        let mut objects = self.objects.lock().unwrap();
        let key = format!("{migration}/{rel}");
        if let Some(existing) = objects.get(&key) { return Ok(Some(existing.clone())); }
        objects.insert(key, bytes.to_vec());
        Ok(None)
    }
    fn list_records(&self, migration: &str) -> Result<Vec<String>> {
        let prefix = format!("{migration}/records/");
        Ok(self.objects.lock().unwrap().keys().filter_map(|k| k.strip_prefix(&prefix)?.strip_suffix(".json")).map(str::to_owned).collect())
    }
}
impl CountingStore {
    fn mark(&self) -> usize { self.creates.lock().unwrap().len() }
    fn created_since(&self, mark: usize) -> Vec<String> { self.creates.lock().unwrap()[mark..].to_vec() }
    fn plan_bytes(&self) -> Option<Vec<u8>> { self.objects.lock().unwrap().get("mig/plan.json").cloned() }
    fn records(&self) -> Vec<Record> {
        self.objects.lock().unwrap().iter().filter(|(k, _)| k.starts_with("mig/records/")).map(|(_, v)| serde_json::from_slice(v).unwrap()).collect()
    }
}
#[derive(Default)]
struct TopLevelTrace { admitted: Vec<String>, fingerprinted: Vec<String>, reverified: Vec<String>, switched: Vec<String> }
struct JournalEffects<'a> { journal: &'a super::journal::Journal, failing_reverify: &'a [&'a str], trace: Mutex<TopLevelTrace>, ids: Mutex<u64> }
impl super::execute::Effects for JournalEffects<'_> {
    fn append(&self, r: &Record) -> Result<()> { self.journal.append(r) }
    fn source_fingerprint(&self, e: &Entry) -> Result<String> {
        self.trace.lock().unwrap().fingerprinted.push(e.archive_id.clone()); Ok(e.fingerprint.clone())
    }
    fn build(&self, e: &Entry, id: &str) -> Result<super::execute::Replacement> {
        assert!(e.exact.is_none(), "the immutable legacy entry reaches build unchanged; contracts are derived per build");
        self.trace.lock().unwrap().admitted.push(e.archive_id.clone());
        Ok(super::execute::Replacement { new_archive_id: id.into(), new_manifest: format!("rc8-output:{id}/manifest.json") })
    }
    fn reverify(&self, e: &Entry, _: &str, _: &str) -> Result<()> {
        self.trace.lock().unwrap().reverified.push(e.archive_id.clone());
        if self.failing_reverify.contains(&e.archive_id.as_str()) { bail!("fixture: recorded replacement no longer verifies"); }
        Ok(())
    }
    fn switch(&self, e: &Entry, _: &super::execute::Replacement) -> Result<()> {
        self.trace.lock().unwrap().switched.push(e.archive_id.clone()); Ok(())
    }
    fn stop_requested(&self) -> bool { false }
    fn now(&self) -> u64 { 1_000_000 }
    fn new_id(&self) -> Result<String> { let mut n = self.ids.lock().unwrap(); *n += 1; Ok(format!("toplevel-id-{n}")) }
    fn say(&self, _: &str) {}
}
fn sorted(mut v: Vec<String>) -> Vec<String> { v.sort(); v }

#[test]
fn rc8_top_level_resume_admits_only_pending_and_never_rewrites_the_journal_plan() {
    use super::execute::tests_support::{entry, plan, rec};
    use super::execute::{load_resume, resume_entries};
    for failing_reverify in [&[][..], &["verified"][..]] {
        let legacy = plan(vec![entry("done", Action::Relocate, 1), entry("verified", Action::Relocate, 2), entry("pending", Action::Relocate, 3)]);
        assert!(legacy.entries.iter().all(|e| e.exact.is_none()));
        let plan_bytes = serde_json::to_vec(&legacy).unwrap();
        let plan_fingerprint = blake3::hash(&plan_bytes).to_hex().to_string();
        let stores = [Arc::new(CountingStore::default()), Arc::new(CountingStore::default())];
        for store in &stores { store.objects.lock().unwrap().insert("mig/plan.json".into(), plan_bytes.clone()); }
        let cloud: Vec<Arc<dyn super::journal::JournalStore>> = stores.iter().map(|s| s.clone() as _).collect();
        let journal = super::journal::Journal::with_stores("p", "mig", cloud, None).unwrap();
        journal.append(&rec("done", RecordState::Switched, 10, "old-pc")).unwrap();
        journal.append(&rec("verified", RecordState::Verified, 20, "old-pc")).unwrap();
        let marks: Vec<usize> = stores.iter().map(|s| s.mark()).collect();

        let (loaded, records) = load_resume(&journal, "p", "mig").unwrap();
        assert_eq!(serde_json::to_vec(&loaded).unwrap(), plan_bytes, "journal plan is loaded verbatim");
        assert_eq!(records.len(), 2);
        let effects = JournalEffects { journal: &journal, failing_reverify, trace: Mutex::default(), ids: Mutex::new(0) };
        let summary = resume_entries(&journal, &loaded, &records, "fixture-pc", &effects, Some(1), None).unwrap();
        assert_eq!((summary.already_switched, summary.switched_now, summary.unknown.len()), (1, 2, 0));
        let trace = effects.trace.lock().unwrap();
        assert_eq!(trace.reverified, ["verified"], "Verified re-reads its recorded proof");
        if failing_reverify.is_empty() {
            assert_eq!(trace.admitted, ["pending"], "only the pending entry is prepared/admitted");
            assert_eq!(trace.fingerprinted, ["pending"]);
        } else {
            assert_eq!(sorted(trace.admitted.clone()), ["pending", "verified"], "a failed re-verification needs a fresh admission");
            assert_eq!(sorted(trace.fingerprinted.clone()), ["pending", "verified"]);
        }
        assert_eq!(sorted(trace.switched.clone()), ["pending", "verified"]);
        drop(trace);
        for (store, mark) in stores.iter().zip(&marks) {
            let created = store.created_since(*mark);
            assert!(created.iter().all(|rel| rel.starts_with("records/")), "resume writes records only, never plan.json: {created:?}");
            assert_eq!(store.plan_bytes().as_deref(), Some(plan_bytes.as_slice()));
            let new: Vec<Record> = store.records().into_iter().filter(|r| r.pc_id == "fixture-pc").collect();
            assert_eq!(new.iter().filter(|r| r.entry == "done").count(), 0, "finished entry: zero journal writes");
            let verified_states: Vec<_> = new.iter().filter(|r| r.entry == "verified").map(|r| r.state).collect();
            if failing_reverify.is_empty() {
                assert_eq!(verified_states, [RecordState::Switched], "Verified appends only Switched, no claim/build");
            } else {
                assert!(verified_states.contains(&RecordState::Claimed) && verified_states.contains(&RecordState::Switched));
            }
        }
        assert_eq!(blake3::hash(&stores[0].plan_bytes().unwrap()).to_hex().to_string(), plan_fingerprint);

        // Resume after everything switched: no admission, build or write at all.
        let marks: Vec<usize> = stores.iter().map(|s| s.mark()).collect();
        let (loaded, records) = load_resume(&journal, "p", "mig").unwrap();
        let effects = JournalEffects { journal: &journal, failing_reverify, trace: Mutex::default(), ids: Mutex::new(0) };
        let summary = resume_entries(&journal, &loaded, &records, "fixture-pc", &effects, Some(1), None).unwrap();
        assert_eq!((summary.already_switched, summary.switched_now), (3, 0));
        let trace = effects.trace.lock().unwrap();
        assert!(trace.admitted.is_empty() && trace.fingerprinted.is_empty() && trace.reverified.is_empty() && trace.switched.is_empty());
        assert_eq!(*effects.ids.lock().unwrap(), 0, "finished resume starts no attempt");
        for (store, mark) in stores.iter().zip(&marks) {
            assert!(store.created_since(*mark).is_empty(), "finished resume performs zero journal writes");
            assert_eq!(store.plan_bytes().as_deref(), Some(plan_bytes.as_slice()));
        }
    }
}

struct FailedDrive { fail_generation_listing: bool }
impl super::drive_source::DriveSource for FailedDrive {
    fn generations(&self) -> Result<Vec<super::drive_model::GenerationRef>> {
        if self.fail_generation_listing { bail!("Cloud1 fixture root: {SMB_CAUSE}"); }
        Ok(vec![super::drive_model::GenerationRef { epoch: None }])
    }
    fn view(&self, _: &super::drive_model::GenerationRef) -> Result<super::drive_model::SourceView> {
        Err(anyhow!("{SMB_CAUSE}")).context("Cloud1 fixture root metadata read failed")
    }
}

#[test]
fn rc8_included_drive_metadata_failure_is_blocking_and_retains_first_cause() {
    #[cfg(unix)]
    if let Some(root) = std::env::var_os("RC8_FS_FIXTURE_CHILD") {
        runtime_drive_failure_child(&PathBuf::from(root));
        return;
    }
    // Empty archive side is intentional: a failed included drive must not
    // turn into a successful-looking zero-entry plan. Both discovery and
    // record-read failure exercise the production DriveSource boundary.
    let cloud = MemoryCloud { inner: FakeCloud::default() };
    for fail_generation_listing in [true, false] {
        let options = PlanOptions::default();
        assert!(!options.skip_drive);
        let (mut plan, mut listed) = plan_listed(&cloud, "rc8-fixture-pool",
            policy(&ROOTS, MIB, 2, 1), &InventoryStore::default(), &options, &BTreeSet::new()).unwrap();
        let result = super::drive_plan::plan_drive(&cloud, &FailedDrive { fail_generation_listing }, &mut plan, &mut listed, &options);
        let error = result.expect_err("included drive failure cannot become Ok(None) / archive-only success");
        let chain = format!("{error:#}");
        assert!(chain.contains(SMB_CAUSE), "first provider cause must survive context wrapping: {chain}");
        assert!(chain.contains("Cloud1 fixture root"));
    }
    #[cfg(unix)]
    runtime_drive_failure_isolated();
}

// These helpers are called only by the single drive regression above.
// A fresh test process isolates lazy account settings/config caches too.
#[cfg(unix)]
fn runtime_drive_failure_isolated() {
    use std::os::unix::fs::PermissionsExt;
    let owned = tempfile::Builder::new().prefix("rpool-rc8-drive-fixture-").tempdir().unwrap();
    let root = owned.path().canonicalize().unwrap();
    for name in ["home", "config", "tmp"] { fs::create_dir(root.join(name)).unwrap(); }
    fs::write(root.join("rclone.conf"), b"# owned empty fixture config; fake executable only\n").unwrap();
    let fake = root.join("fake-rclone");
    let script = r#"#!/bin/sh
printf '%s\n' "$*" >> "$RC8_FS_LOG"
case "$*" in
  *Cloud1:fixture*events-v6*) printf '%s\n' '@CAUSE@' >&2; exit 1 ;;
esac
case "$1" in
  lsjson) printf '%s\n' '[]' ;;
  listremotes) printf '%s\n' 'Base1:' 'Base2:' 'Base3:' 'Cloud1:' 'Cloud2:' 'Cloud3:' ;;
  config) printf '%s\n' '@CONFIG@' ;;
  about) printf '%s\n' '{"total":1099511627776,"used":549755813888,"free":549755813888}' ;;
  *) printf '%s\n' "unexpected fake-rclone command: $*" >&2; exit 91 ;;
esac
"#.replace("@CAUSE@", SMB_CAUSE).replace("@CONFIG@", &fixture_crypt_config(&root));
    fs::write(&fake, script).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("rc8_included_drive_metadata_failure_is_blocking_and_retains_first_cause")
        .arg("--test-threads=1").arg("--nocapture")
        .env_clear().env("PATH", "/usr/bin:/bin")
        .env("HOME", root.join("home")).env("TMPDIR", root.join("tmp"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("RPOOL_CONFIG_DIR", root.join("config"))
        .env("RCLONE_CONFIG", root.join("rclone.conf"))
        .env("RC8_FS_LOG", root.join("calls.log"))
        .env("RC8_FS_FIXTURE_CHILD", &root).current_dir(&root).output().unwrap();
    assert!(output.status.success(), "isolated runtime fixture failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(root.join("child-complete").is_file(), "the child test filter must actually execute its assertions");
}

/// Mechanical adapter (fixture owner transfer, 2026-11-20): production metadata
/// transport requires encrypted destinations, so the fake `config dump` names
/// Cloud1/2/3 as `crypt` remotes over private `local` backing directories
/// inside the owned tempdir. The fixture password is generated here, obscured
/// with the production codec, and never belongs to a user configuration. The
/// crypt gate itself is unchanged and still runs before any listing.
#[cfg(unix)]
fn fixture_crypt_config(root: &Path) -> String {
    let password = crate::crypt::obscure::obscure("rc8-disposable-fixture-only").unwrap();
    let mut dump = serde_json::Map::new();
    for n in 1..=3 {
        let base = root.join(format!("base{n}"));
        fs::create_dir_all(&base).unwrap();
        dump.insert(format!("Base{n}"), serde_json::json!({ "type": "local" }));
        dump.insert(format!("Cloud{n}"), serde_json::json!({
            "type": "crypt",
            "remote": format!("Base{n}:{}", base.display()),
            "password": password,
        }));
    }
    let text = serde_json::Value::Object(dump).to_string();
    assert!(!text.contains('\''), "fixture config must stay a single-quoted shell literal");
    text
}

#[cfg(unix)]
fn runtime_drive_failure_child(root: &Path) {
    use super::drive_source::{CloudDrive, DriveSource};
    let target = policy(&["Cloud1:fixture", "Cloud2:fixture", "Cloud3:fixture"], MIB, 2, 1);
    let mut store = PoolStore::default();
    store.pools.insert("rc8-fixture-pool".into(), target.clone());
    fs::write(root.join("config/pools.json"), serde_json::to_vec(&store).unwrap()).unwrap();
    let fake = root.join("fake-rclone");
    let fake = fake.to_str().unwrap();
    let source = CloudDrive::new(fake, "rc8-fixture-pool", &target);
    // Adapter (2026-11-20): production never exposes captured rclone stderr in
    // error text (storage::rclone::process::run; the real incident surfaced
    // exactly this redacted form). Only the Cloud1 events listing exits 1 in
    // this fake (unexpected commands exit 91, other replicas list []), so the
    // classified exit-1 provider failure is the injected first cause.
    let injected = |error: &anyhow::Error, what: &str| {
        let chain = format!("{error:#}");
        assert!(chain.contains("rclone failed (exit Some(1))"), "{what} must preserve the injected Cloud1 provider failure: {chain}");
        assert!(!chain.contains(SMB_CAUSE), "{what} must keep provider stderr redacted: {chain}");
        assert!(chain.contains("metadata replica Cloud1"), "{what} must identify the failed replica without its physical path: {chain}");
        assert!(!chain.contains("crypt destination") && !chain.contains("non-crypt"), "{what} stopped at the crypt gate: {chain}");
    };
    let error = source.view(&super::drive_model::GenerationRef { epoch: None })
        .expect_err("a single failing required replica must not become an empty/partial drive view");
    injected(&error, "the real transport/read path");
    let options = PlanOptions::default();
    let result = super::plan::plan_with_drive(fake, "rc8-fixture-pool", &options);
    let error = result.expect_err("shared GUI/CLI planner must not publish a successful zero-entry included-drive plan");
    injected(&error, "the shared GUI/CLI planner");
    let included = fs::read_to_string(root.join("calls.log")).unwrap();
    assert!(included.lines().any(|l| l.contains("Cloud1:fixture") && l.contains("events-v6")),
        "the included-drive read must pass the crypt gate and reach the injected Cloud1 listing: {included}");
    fs::write(root.join("calls.log"), b"").unwrap();
    let no_drive = PlanOptions { skip_drive: true, ..Default::default() };
    let (_, drive) = super::plan::plan_with_drive(fake, "rc8-fixture-pool", &no_drive)
        .expect("explicit --no-drive permits archive-only planning despite drive metadata failure");
    assert!(drive.is_none());
    let calls = fs::read_to_string(root.join("calls.log")).unwrap();
    assert!(!calls.contains("events-v6"), "explicit no-drive must not probe drive records: {calls}");
    fs::write(root.join("child-complete"), b"all assertions reached\n").unwrap();
}

#[test]
fn rc8_alias_capacity_and_unknown_cost_never_spend_original_retire_credit() {
    let observation = |remote: &str| CapacityObservation { remote: remote.into(), capacity_domain: "same-account".into(),
        total_bytes: Some(1000 * MIB), free_bytes: Some(100 * MIB), metadata_reserve_bytes: 0, reserved_bytes: 0 };
    let quotas = [observation("alias-a:fixture"), observation("alias-b:fixture")];
    let output = |entry: &str, remote: &str| OutputSpec {
        entry_id: entry.into(), mode: ExecutionMode::Full, destination_remote: remote.into(),
        capacity_domain: "same-account".into(), object_id: format!("{entry}/new-whole-shard"),
        ownership: OutputOwnership::Created, method: CreationMethod::ClientCopy,
        size: ObjectSize::Stored { whole_object_bytes: 60 * MIB }, borrowed_from: None,
        logical_move_in_bytes: 60 * MIB,
        logical_move_out_bytes: [("same-account".into(), 60 * MIB)].into_iter().collect(),
        local_scratch_bytes: Some(60 * MIB),
    };
    let first = output("first", "alias-a:fixture");
    let one = preview_storage(&quotas, std::slice::from_ref(&first));
    assert!(one.admitted);
    assert_eq!(one.disks.len(), 1);
    assert_eq!(one.disks[0].current_free_bytes, Some(100 * MIB), "aliases must not double free bytes");
    assert_eq!(one.disks[0].headroom_bytes, Some(i128::from(40 * MIB)));
    let second = output("second", "alias-b:fixture");
    let two = preview_storage(&quotas, &[first.clone(), second]);
    assert!(!two.admitted, "serial entries keep both originals; outgoing logical credit cannot fund writes");
    assert_eq!(two.disks[0].additional_created_bytes, Some(120 * MIB));
    assert_eq!(two.disks[0].headroom_bytes, Some(-i128::from(20 * MIB)));
    assert_eq!(two.local_scratch_peak_bytes, Some(60 * MIB), "serial entry scratch reuses its peak, not its sum");
    let mut unknown = first;
    unknown.size = ObjectSize::Unknown { reason: "full output size unavailable".into() };
    let preview = preview_storage(&quotas, &[unknown]);
    assert!(!preview.admitted);
    assert_eq!(preview.disks[0].additional_created_bytes, None, "unknown cost is not zero");
    assert_eq!(preview.local_scratch_peak_bytes, None);
}

#[test]
fn rc8_minimal_ack_and_layout_mismatch_remain_fail_closed_without_full_fallback() {
    use super::execute::tests_support::{entry, plan};
    let mut plan = plan(vec![entry("borrowed-v3", Action::RelocateMinimal, MIB)]);
    assert!(super::exact::ack(&plan).is_err());
    for (upgraded, stopped) in [(false, true), (true, false), (false, false)] {
        plan.minimal_ack = Some(MinimalAck { all_pcs_upgraded: upgraded, all_pcs_stopped: stopped });
        assert!(super::exact::ack(&plan).is_err());
    }
    plan.minimal_ack = Some(MinimalAck { all_pcs_upgraded: true, all_pcs_stopped: true });
    super::exact::ack(&plan).unwrap();
    let source = source();
    let cloud = MemoryCloud::with_source(&source);
    let options = PlanOptions { minimal_transfer: true, all_pcs_upgraded: true, all_pcs_stopped: true, ..Default::default() };
    let changed = planned(&cloud, policy(&ROOTS, MIB, 1, 2), &options);
    let blocked = only_entry(&changed);
    assert_eq!(blocked.action, Action::Unknown, "minimal layout mismatch must not silently become full Reencode");
    assert!(blocked.detail.as_deref().unwrap().contains("never silently re-encodes"));
    let preview = super::storage_preview::preview(&budgets(&changed.target), changed.entries.clone());
    assert!(!preview.admitted, "blocked minimal work cannot become an admitted zero-cost no-op");
}
