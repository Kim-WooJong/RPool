//! The cached visible namespace answers exactly as the full `view`, and any
//! namespace change (including direct state edits) reaches the next answer.
use super::*;
use crate::mount::fs_core::FsCore;
use crate::mount::virtual_tests::content;

fn event(
    d: &VirtualDrive,
    path: &str,
    bytes: Option<&[u8]>,
    parents: Vec<String>,
    worker: &str,
) -> String {
    let event = Event {
        version: 1,
        device: worker.into(),
        worker: worker.into(),
        path: path.into(),
        parents,
        content: bytes.map(content),
    };
    let id = event.id().unwrap();
    d.state.lock().unwrap().events.insert(id.clone(), event);
    id
}
fn write(d: &VirtualDrive, path: &str, bytes: &[u8]) {
    let intent = d.begin(path).unwrap();
    fs::write(d.spool_path(&intent), bytes).unwrap();
    d.seal(intent).unwrap();
}
/// The listing logic the frontends used over the full `view`.
fn listed(view: &BTreeMap<String, Revision>, prefix: &str) -> Vec<(String, Option<String>)> {
    let mut entries = BTreeMap::new();
    for (path, r) in view {
        if let Some(relative) = path.strip_prefix(prefix) {
            match relative.split_once('/') {
                Some((child, _)) => entries.insert(child.to_owned(), None),
                None => entries.insert(relative.to_owned(), Some(r.id().to_owned())),
            };
        }
    }
    entries.into_iter().collect()
}
fn assert_matches_full_view(d: &VirtualDrive, probes: &[&str]) {
    let full = d.view().unwrap();
    let view = d.visible().unwrap();
    for (path, revision) in &full {
        assert_eq!(
            view.file(path),
            Some((revision.id(), revision.size())),
            "{path}"
        );
        assert_eq!(
            d.visible_revision(path).unwrap().map(|r| r.id().to_owned()),
            Some(revision.id().to_owned())
        );
    }
    let mut prefixes: BTreeSet<String> = [String::new()].into();
    for path in full
        .keys()
        .map(String::as_str)
        .chain(probes.iter().copied())
    {
        assert_eq!(view.file(path).is_some(), full.contains_key(path), "{path}");
        assert_eq!(
            d.visible_revision(path).unwrap().is_some(),
            full.contains_key(path)
        );
        let mut prefix = String::new();
        for part in path.split('/') {
            prefix.push_str(part);
            prefix.push('/');
            prefixes.insert(prefix.clone());
        }
    }
    for prefix in &prefixes {
        let children: Vec<_> = view
            .children(prefix)
            .into_iter()
            .map(|(child, file)| (child, file.map(|(id, _)| id.to_owned())))
            .collect();
        assert_eq!(children, listed(&full, prefix), "listing of {prefix:?}");
        assert_eq!(
            view.has_descendant(prefix),
            full.keys().any(|p| p.starts_with(prefix.as_str())),
            "{prefix}"
        );
    }
    let used = d.visible_used().unwrap();
    let state = d.state.lock().unwrap();
    assert_eq!(used, state.visible_logical_used().unwrap());
    assert_eq!(view.directories(), &*state.directories);
    assert_eq!(view.generation(), state.generation);
}
/// Committed heads, conflicts, deletions, pending saves and deletions, and
/// explicit directories, in both namespace versions.
fn scenario(version: u32) -> (tempfile::TempDir, VirtualDrive) {
    let temp = tempfile::tempdir().unwrap();
    let d = fixture(temp.path());
    d.state.lock().unwrap().version = version;
    for path in [
        "a.txt", "d/x", "d/y", "d/sub/z", "d0", "d.txt", "e/f/g", "q/r",
    ] {
        event(&d, path, Some(path.as_bytes()), vec![], "a");
    }
    let gone = event(&d, "gone", Some(b"gone"), vec![], "a");
    event(&d, "gone", None, vec![gone], "a");
    let base = event(&d, "c.txt", Some(b"base"), vec![], "a");
    event(&d, "c.txt", Some(b"mine"), vec![base.clone()], "a");
    event(&d, "c.txt", Some(b"theirs"), vec![base], "b");
    write(&d, "d/x", b"local edit");
    write(&d, "new/n1", b"new");
    write(&d, "q/r", b"first");
    write(&d, "q/r", b"second");
    d.delete("d/y").unwrap();
    d.delete("e/f/g").unwrap();
    d.create_directory("empty/dir").unwrap();
    (temp, d)
}

#[test]
fn cached_answers_equal_the_full_view() {
    let probes = [
        "missing",
        "d/missing",
        "e/f",
        "empty/dir",
        "gone",
        "nothing/below",
    ];
    for version in [3, 6] {
        let (_temp, d) = scenario(version);
        assert_matches_full_view(&d, &probes);
        // Session pins (local-only fixture) take the full-view path.
        let revision = d.visible_revision("a.txt").unwrap().unwrap();
        d.pin_read("a.txt", &revision).unwrap();
        event(&d, "a.txt", Some(b"peer"), vec![revision.id().into()], "b");
        assert!(d.view().unwrap().keys().any(|p| p.contains("incoming")));
        assert_matches_full_view(&d, &probes);
    }
}

#[test]
fn every_state_change_reaches_the_next_answer() {
    let (_temp, d) = scenario(6);
    let revision = d.state.lock().unwrap().revision();
    assert!(!d.state.lock().unwrap().pending.is_empty());
    assert_eq!(
        d.state.lock().unwrap().revision(),
        revision,
        "reads do not invalidate"
    );
    d.state.lock().unwrap().generation += 0;
    assert_eq!(
        d.state.lock().unwrap().revision(),
        revision + 1,
        "writes do"
    );
    assert!(d.visible().unwrap().file("late").is_none());
    let late = event(&d, "late", Some(b"late"), vec![], "peer");
    assert!(d.visible().unwrap().file("late").is_some());
    // Same event count, different events.
    {
        let mut s = d.state.lock().unwrap();
        let removed = s.events.remove(&late).unwrap();
        assert_eq!(removed.path, "late");
    }
    event(&d, "other", Some(b"other"), vec![], "peer");
    let view = d.visible().unwrap();
    assert!(view.file("late").is_none());
    assert!(view.file("other").is_some());
    // Pending-only and directory-only changes.
    write(&d, "late", b"again");
    assert_eq!(d.visible().unwrap().file("late").map(|f| f.1), Some(5));
    d.state.lock().unwrap().directories.insert("fresh".into());
    assert!(d.visible().unwrap().directories().contains("fresh"));
    assert_matches_full_view(&d, &[]);
}

#[test]
fn pending_changes_reuse_the_committed_projection() {
    let (_temp, d) = scenario(6);
    let (guard, before, overlay) = d.locked_visible().unwrap();
    drop(guard);
    write(&d, "another", b"bytes");
    let (guard, after, changed) = d.locked_visible().unwrap();
    drop(guard);
    assert!(
        Arc::ptr_eq(&before, &after),
        "events unchanged: no reprojection"
    );
    assert!(!Arc::ptr_eq(&overlay, &changed));
    let (guard, _, same) = d.locked_visible().unwrap();
    drop(guard);
    assert!(
        Arc::ptr_eq(&changed, &same),
        "nothing changed: nothing rebuilt"
    );
}

#[test]
fn quota_charges_new_writes_like_before() {
    let (_temp, d) = scenario(6);
    let state = d.state.lock().unwrap();
    let mut status = d.measure_capacity_offline(&state);
    drop(state);
    status.additional_estimate = 1_000_000;
    *d.capacity.lock().unwrap() = Some(status);
    let (used, total) = d.quota().unwrap();
    assert_eq!(
        used,
        d.state.lock().unwrap().visible_logical_used().unwrap()
    );
    assert_eq!(total, Some(used + 1_000_000));
    write(&d, "big", &[7u8; 1000]);
    event(&d, "peer-file", Some(&[1u8; 300]), vec![], "peer");
    let (used, total) = d.quota().unwrap();
    assert_eq!(total, Some(used + 1_000_000 - 1000 - 300));
}
/// A file half uploaded is reserved only for its missing shards, on their
/// planned accounts: the uploaded half is already in the accounts' usage.
#[test]
fn uploads_in_progress_reserve_only_their_missing_shards() {
    let (_temp, d) = scenario(6);
    write(&d, "big", &[7u8; 1000]);
    let intent = d
        .state
        .lock()
        .unwrap()
        .pending
        .iter()
        .find(|i| i.path == "big")
        .cloned()
        .unwrap();
    let eligible = vec!["a:pool".to_string(), "b:pool".to_string()];
    // Not started: no plan, so the caller reserves the whole file.
    assert_eq!(d.upload_remaining(&intent, &eligible), None);
    let key = blake3::hash(&serde_json::to_vec(&eligible).unwrap())
        .to_hex()
        .to_string();
    let dir = d
        .spool_path(&intent)
        .parent()
        .unwrap()
        .join("upload")
        .join(format!("eligible-{key}"));
    fs::create_dir_all(&dir).unwrap();
    let staged = dir.join("big");
    let shard = |index: u32, remote: &str| crate::models::PlanShard {
        index,
        offset: 0,
        size: 400,
        remote: remote.into(),
        object: format!("o{index}"),
        kind: Default::default(),
        group: 0,
        slot: 0,
    };
    let plan = crate::models::UploadPlan {
        version: 1,
        archive_id: "x".into(),
        source_size: 1000,
        shard_size: 400,
        remotes: eligible.clone(),
        placement: crate::models::Placement::RoundRobin,
        coding: None,
        shards: vec![shard(0, "a:pool"), shard(1, "b:pool"), shard(2, "a:pool")],
    };
    let plan_path = crate::utils::append_suffix(&staged, ".rpool.upload.json");
    crate::utils::save_json_atomic(&plan_path, &plan).unwrap();
    // Planned, nothing uploaded: every shard on its account.
    assert_eq!(
        d.upload_remaining(&intent, &eligible),
        Some(vec![
            ("a:pool".into(), 400),
            ("b:pool".into(), 400),
            ("a:pool".into(), 400)
        ])
    );
    let journal = serde_json::json!({
        "version": 1,
        "plan_fingerprint": "f",
        "completed": {"0": {"index": 0, "offset": 0, "size": 400, "remote": "a:pool",
                            "object": "o0", "blake3": "h"}},
        "updated_unix": 0
    });
    fs::write(
        crate::utils::append_suffix(&staged, ".rpool.upload.state.json"),
        journal.to_string(),
    )
    .unwrap();
    assert_eq!(
        d.upload_remaining(&intent, &eligible),
        Some(vec![("b:pool".into(), 400), ("a:pool".into(), 400)])
    );
    // A plan for other accounts is not resumed: reserve the whole file.
    assert_eq!(d.upload_remaining(&intent, &["c:pool".to_string()]), None);
    // Manifest complete: nothing left to reserve.
    fs::write(crate::utils::append_suffix(&staged, ".rpool.json"), "{}").unwrap();
    assert_eq!(d.upload_remaining(&intent, &eligible), Some(vec![]));

    let target = |remote: &str, free| crate::storage::admin::budget::TargetBudget {
        remote: remote.into(),
        backing: remote.into(),
        capacity_domain: remote.into(),
        failure_domain: None,
        declared: true,
        total: 10_000,
        free,
    };
    let mut status = CapacityStatus {
        targets: vec![target("a:pool", 5000), target("b:pool", 3000)],
        ..CapacityStatus::default()
    };
    status
        .reserve_remaining(&[
            ("b:pool".into(), 400),
            ("a:pool".into(), 400),
            ("gone:".into(), 999),
        ])
        .unwrap();
    let free: Vec<u64> = status.targets.iter().map(|t| t.free).collect();
    assert_eq!(
        free,
        [4600, 2600],
        "only the missing shards, on their accounts"
    );
    assert_eq!(status.pending_physical_reservation, 800);
}

/// A failed pack upload leaves every member pending (spool kept), and a
/// lone member uses the single-file path.
#[test]
fn failed_pack_upload_keeps_every_member_pending() {
    let (_temp, d) = scenario(6);
    write(&d, "a.txt", b"first small file");
    write(&d, "b.txt", b"second small file");
    let members: Vec<Intent> = d
        .state
        .lock()
        .unwrap()
        .pending
        .iter()
        .filter(|i| i.path == "a.txt" || i.path == "b.txt")
        .cloned()
        .collect();
    assert_eq!(members.len(), 2);
    let results = d.upload_pack(
        &members,
        &|staged: &Path, id: &str| {
            assert!(id.starts_with("virtual-pack-"));
            // The staged pack is both files back to back.
            assert_eq!(
                fs::read(staged).unwrap(),
                b"first small filesecond small file"
            );
            bail!("provider unavailable")
        },
        &|_| panic!("no single upload for two members"),
    );
    assert!(results.iter().all(|r| r
        .as_ref()
        .is_err_and(|e| e.to_string().contains("provider unavailable"))));
    let pending = d.state.lock().unwrap().pending.clone();
    for member in &members {
        assert!(
            pending.iter().any(|i| i.id == member.id),
            "{} still pending",
            member.path
        );
        assert!(d.spool_path(member).exists(), "spool image kept");
    }
    // One member left: uploaded on its own.
    let single_called = std::sync::atomic::AtomicBool::new(false);
    let results = d.upload_pack(
        &members[..1],
        &|_: &Path, _: &str| panic!("no pack for one member"),
        &|_| {
            single_called.store(true, std::sync::atomic::Ordering::SeqCst);
            bail!("offline")
        },
    );
    assert!(single_called.load(std::sync::atomic::Ordering::SeqCst));
    assert!(results[0].is_err());
}

impl VirtualDrive {
    fn measure_capacity_offline(&self, state: &Namespace) -> CapacityStatus {
        CapacityStatus {
            observed_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            eligible: vec!["one".into()],
            pending_ids: state.pending.iter().map(|i| i.id.clone()).collect(),
            namespace_event_ids: state.events.keys().cloned().collect(),
            ..CapacityStatus::default()
        }
    }
}

/// Browsing a few thousand files: one projection, then cheap lookups. The
/// bound is generous (debug build, shared CI machine); the printed numbers
/// are the point.
#[test]
fn browsing_many_files_projects_once() {
    let temp = tempfile::tempdir().unwrap();
    let mut drive = fixture(temp.path());
    drive.pool_sync_roots = vec!["crypt:pool".into()];
    drive.state.lock().unwrap().version = 6;
    for i in 0..2000 {
        event(
            &drive,
            &format!("dir{}/file{i}.txt", i % 40),
            Some(b"x"),
            vec![],
            "a",
        );
    }
    let drive = Arc::new(drive);
    let core = FsCore::new(drive.clone()).unwrap();
    let started = std::time::Instant::now();
    drive.visible().unwrap();
    let first = started.elapsed();
    let started = std::time::Instant::now();
    for i in 0..2000 {
        let attr = core.lookup(&format!("dir{}/file{i}.txt", i % 40)).unwrap();
        assert!(!attr.directory);
        core.statfs();
    }
    for dir in 0..40 {
        assert_eq!(core.readdir(&format!("dir{dir}")).unwrap().len(), 50);
    }
    assert_eq!(core.readdir("").unwrap().len(), 40);
    let cached = started.elapsed();
    eprintln!("first projection {first:?}; 2000 lookups + statfs + 41 listings {cached:?}");
    assert!(cached < std::time::Duration::from_secs(20), "{cached:?}");
}
