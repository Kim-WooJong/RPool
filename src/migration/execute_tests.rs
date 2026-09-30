use super::tests_support::{entry, plan, rec};
use super::*;
use crate::migration::model::MissingReason;

/// What a fake run did (snapshot via [`Fake::log`]).
#[derive(Default, Clone)]
struct Log {
    appended: Vec<Record>,
    built: Vec<String>,
    reverified: Vec<String>,
    switched: Vec<(String, String)>,
    ids: u64,
    said: Vec<String>,
}

#[derive(Default)]
struct Fake {
    log: Mutex<Log>,
    /// Entries whose build fails with a provider error / as unrecoverable.
    provider_error: BTreeSet<String>,
    unrecoverable: BTreeSet<String>,
    reverify_fails: BTreeSet<String>,
    changed_source: BTreeSet<String>,
    /// Appends for these entries fail (a journal that refuses every store).
    append_fails: BTreeSet<String>,
    stop_after_builds: Option<usize>,
    /// Each build takes this long (concurrency tests).
    build_sleep: Option<std::time::Duration>,
    building: std::sync::atomic::AtomicUsize,
    max_building: std::sync::atomic::AtomicUsize,
    take_over: bool,
    now: u64,
}

impl Fake {
    fn log(&self) -> Log {
        self.log.lock().unwrap().clone()
    }
    fn with<R>(&self, f: impl FnOnce(&mut Log) -> R) -> R {
        f(&mut self.log.lock().unwrap())
    }
    fn max_building(&self) -> usize {
        self.max_building.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Effects for Fake {
    fn append(&self, record: &Record) -> Result<()> {
        if self.append_fails.contains(&record.entry) {
            bail!("journal: no store accepted the record");
        }
        self.with(|l| l.appended.push(record.clone()));
        Ok(())
    }
    fn source_fingerprint(&self, entry: &Entry) -> Result<String> {
        if self.changed_source.contains(&entry.archive_id) {
            return Ok("other".into());
        }
        Ok(entry.fingerprint.clone())
    }
    fn build(&self, entry: &Entry, new_archive_id: &str) -> Result<Replacement> {
        use std::sync::atomic::Ordering::SeqCst;
        self.with(|l| l.built.push(entry.archive_id.clone()));
        if let Some(pause) = self.build_sleep {
            let now = self.building.fetch_add(1, SeqCst) + 1;
            self.max_building.fetch_max(now, SeqCst);
            std::thread::sleep(pause);
            self.building.fetch_sub(1, SeqCst);
        }
        if self.provider_error.contains(&entry.archive_id) {
            bail!("rclone: 503");
        }
        if self.unrecoverable.contains(&entry.archive_id) {
            return Err(Unrecoverable(vec![GroupLoss {
                group: 3,
                required_k: 2,
                available: 1,
                missing: vec![model::MissingShard {
                    index: 0,
                    remote: "gone:".into(),
                    reason: MissingReason::RemoteRemoved,
                }],
            }])
            .into());
        }
        Ok(Replacement {
            new_archive_id: new_archive_id.into(),
            new_manifest: format!("t:{new_archive_id}/manifest.json"),
        })
    }
    fn reverify(&self, entry: &Entry, _id: &str, _manifest: &str) -> Result<()> {
        self.with(|l| l.reverified.push(entry.archive_id.clone()));
        if self.reverify_fails.contains(&entry.archive_id) {
            bail!("manifest missing");
        }
        Ok(())
    }
    fn switch(&self, entry: &Entry, replacement: &Replacement) -> Result<()> {
        self.with(|l| {
            l.switched
                .push((entry.archive_id.clone(), replacement.new_archive_id.clone()))
        });
        Ok(())
    }
    fn take_over(&self) -> bool {
        self.take_over
    }
    fn stop_requested(&self) -> bool {
        self.stop_after_builds
            .is_some_and(|n| self.with(|l| l.built.len()) >= n)
    }
    fn now(&self) -> u64 {
        self.now
    }
    fn new_id(&self) -> Result<String> {
        Ok(self.with(|l| {
            l.ids += 1;
            format!("id{}", l.ids)
        }))
    }
    fn say(&self, line: &str) {
        self.with(|l| l.said.push(line.to_string()));
    }
}

fn fake() -> Fake {
    Fake {
        now: 1_000_000,
        ..Default::default()
    }
}

fn states(f: &Fake, id: &str) -> Vec<RecordState> {
    f.log()
        .appended
        .iter()
        .filter(|r| r.entry == id)
        .map(|r| r.state)
        .collect()
}

#[test]
fn fresh_run_claims_verifies_and_switches_every_movable_entry() {
    let p = plan(vec![
        entry("a", Action::Relocate, 10),
        entry("b", Action::Reencode, 5),
        entry("u", Action::Unaffected, 1),
    ]);
    let f = fake();
    let s = run_core(&p, &[], "pc1", &f, Some(1)).unwrap();
    assert_eq!(s.switched_now, 2);
    assert_eq!(f.log().built, vec!["b", "a"], "smaller first");
    for id in ["a", "b"] {
        assert_eq!(
            states(&f, id),
            vec![
                RecordState::Claimed,
                RecordState::Verified,
                RecordState::Switched
            ]
        );
    }
    let log = f.log();
    let claim = log.appended.iter().find(|r| r.entry == "b").unwrap();
    assert!(claim
        .new_archive_id
        .as_deref()
        .unwrap()
        .starts_with("migrate-"));
    assert!(log
        .said
        .iter()
        .any(|l| l.starts_with("Migration 1/2: b.bin reencode")));
    s.finish().unwrap();
}

#[test]
fn resume_skips_switched_reverifies_verified_and_retries_claimed() {
    let p = plan(vec![
        entry("s", Action::Relocate, 1),
        entry("v", Action::Relocate, 2),
        entry("c", Action::Relocate, 3),
        entry("n", Action::Relocate, 4),
    ]);
    let now = 1_000_000;
    let records = vec![
        rec("s", RecordState::Claimed, 1, "pc1"),
        rec("s", RecordState::Verified, 2, "pc1"),
        rec("s", RecordState::Switched, 3, "pc1"),
        rec("v", RecordState::Claimed, 4, "pc1"),
        rec("v", RecordState::Verified, 5, "pc1"),
        // Our own interrupted claim, and a stale claim from another PC.
        rec("c", RecordState::Claimed, now - 10, "pc1"),
        rec(
            "n",
            RecordState::Claimed,
            now - CLAIM_LEASE_SECONDS - 1,
            "pc2",
        ),
    ];
    let f = fake();
    let s = run_core(&p, &records, "pc1", &f, Some(1)).unwrap();
    assert_eq!(s.already_switched, 1);
    assert_eq!(f.log().reverified, vec!["v"]);
    assert_eq!(f.log().built, vec!["c", "n"]);
    assert!(states(&f, "s").is_empty());
    assert_eq!(states(&f, "v"), vec![RecordState::Switched]);
    assert_eq!(f.log().switched[0], ("v".into(), "new-v-5".into()));
    for id in ["c", "n"] {
        assert_eq!(
            states(&f, id),
            vec![
                RecordState::Orphan,
                RecordState::Claimed,
                RecordState::Verified,
                RecordState::Switched
            ]
        );
    }
    // A second run over everything recorded does nothing.
    let mut all = records.clone();
    all.extend(f.log().appended.clone());
    let g = fake();
    let again = run_core(&p, &all, "pc1", &g, Some(1)).unwrap();
    assert_eq!(again.already_switched, 4);
    assert!(g.log().appended.is_empty() && g.log().built.is_empty());
}

#[test]
fn fresh_claim_from_other_pc_is_left_alone() {
    let p = plan(vec![entry("a", Action::Relocate, 1)]);
    let records = vec![rec("a", RecordState::Claimed, 1_000_000 - 60, "pc2")];
    let f = fake();
    let s = run_core(&p, &records, "pc1", &f, Some(1)).unwrap();
    assert_eq!(s.claimed_elsewhere, 1);
    assert!(f.log().built.is_empty() && f.log().appended.is_empty());
    assert!(s.finish().is_err());
}

#[test]
fn take_over_rebuilds_a_fresh_claim_of_a_stopped_pc() {
    let p = plan(vec![entry("a", Action::Relocate, 1)]);
    let records = vec![rec("a", RecordState::Claimed, 1_000_000 - 60, "pc2")];
    let mut f = fake();
    f.take_over = true;
    let s = run_core(&p, &records, "pc1", &f, Some(1)).unwrap();
    assert_eq!(s.claimed_elsewhere, 0);
    assert_eq!(f.log().built, vec!["a"]);
    assert_eq!(*states(&f, "a").last().unwrap(), RecordState::Switched);
    assert!(s.finish().is_ok());
}

#[test]
fn failed_reverification_rebuilds() {
    let p = plan(vec![entry("v", Action::Reencode, 1)]);
    let records = vec![rec("v", RecordState::Verified, 5, "pc1")];
    let mut f = fake();
    f.reverify_fails.insert("v".into());
    run_core(&p, &records, "pc1", &f, Some(1)).unwrap();
    assert_eq!(f.log().built, vec!["v"]);
    assert_eq!(
        states(&f, "v"),
        vec![
            RecordState::Claimed,
            RecordState::Verified,
            RecordState::Switched
        ]
    );
}

#[test]
fn lowest_margin_first_then_size() {
    let loss = |available| GroupLoss {
        group: 0,
        required_k: 4,
        available,
        missing: vec![],
    };
    let mut tight = entry("tight", Action::Relocate, 100);
    tight.losses = vec![loss(5), loss(4)];
    let mut loose = entry("loose", Action::Relocate, 1);
    loose.losses = vec![loss(6)];
    let entries = vec![
        entry("big", Action::Relocate, 50),
        loose,
        entry("small", Action::Reencode, 2),
        tight,
        entry("lost", Action::Lost, 0),
    ];
    let order: Vec<_> = order(&entries)
        .iter()
        .map(|e| e.archive_id.as_str())
        .collect();
    assert_eq!(order, vec!["tight", "loose", "small", "big"]);
}

#[test]
fn plan_losses_recorded_once_and_run_losses_and_unknowns_recorded() {
    let mut lost = entry("L", Action::Lost, 7);
    lost.losses = vec![GroupLoss {
        group: 1,
        required_k: 2,
        available: 0,
        missing: vec![],
    }];
    let p = plan(vec![
        lost,
        entry("x", Action::Relocate, 1),
        entry("y", Action::Relocate, 2),
        entry("z", Action::Relocate, 3),
        entry("w", Action::Relocate, 4),
    ]);
    let mut f = fake();
    f.unrecoverable.insert("x".into());
    f.provider_error.insert("y".into());
    f.changed_source.insert("z".into());
    let s = run_core(&p, &[], "pc1", &f, Some(1)).unwrap();
    assert_eq!(states(&f, "L"), vec![RecordState::Lost]);
    assert_eq!(f.log().appended[0].losses.len(), 1);
    assert_eq!(
        states(&f, "x"),
        vec![RecordState::Claimed, RecordState::Lost]
    );
    let log = f.log();
    let x_lost = log
        .appended
        .iter()
        .find(|r| r.entry == "x" && r.state == RecordState::Lost)
        .unwrap();
    assert_eq!(x_lost.losses[0].missing[0].remote, "gone:");
    assert_eq!(
        states(&f, "y"),
        vec![RecordState::Claimed, RecordState::Unknown]
    );
    assert_eq!(states(&f, "z"), vec![RecordState::Unknown]);
    assert!(
        f.log().built.iter().all(|id| id != "z"),
        "changed source is never built"
    );
    assert_eq!(states(&f, "w").last(), Some(&RecordState::Switched));
    assert_eq!(s.lost, 2);
    assert_eq!(s.unknown.len(), 2);
    assert!(s.clone().finish().is_err(), "unknown entries make run fail");

    // Resume: lost entries are not re-recorded, unknown ones are retried.
    let mut all = f.log().appended.clone();
    let mut g = fake();
    g.now += 10;
    let s2 = run_core(&p, &all, "pc1", &g, Some(1)).unwrap();
    assert!(states(&g, "L").is_empty() && states(&g, "x").is_empty());
    assert_eq!(g.log().built, vec!["y", "z"]);
    assert!(s2.unknown.is_empty());
    all.extend(g.log().appended);
    // The earlier Unknown never hides the later Switched.
    let folded = progress(&all);
    assert!(matches!(folded["y"], Progress::Switched(_)));
}

#[test]
fn stop_file_stops_between_entries() {
    let p = plan(vec![
        entry("a", Action::Relocate, 1),
        entry("b", Action::Relocate, 2),
    ]);
    let mut f = fake();
    f.stop_after_builds = Some(1);
    let s = run_core(&p, &[], "pc1", &f, Some(1)).unwrap();
    assert!(s.stopped);
    assert_eq!(f.log().built, vec!["a"]);
    assert_eq!(states(&f, "a").last(), Some(&RecordState::Switched));
    assert!(s.finish().is_err());
}

#[test]
fn abandoned_migration_refuses_to_run() {
    let p = plan(vec![entry("a", Action::Relocate, 1)]);
    let records = vec![rec("", RecordState::Abandoned, 1, "pc1")];
    let f = fake();
    assert!(run_core(&p, &records, "pc1", &f, Some(1)).is_err());
    assert!(f.log().appended.is_empty());
}

#[test]
fn plan_time_unknown_entries_make_run_fail_without_work() {
    let p = plan(vec![entry("q", Action::Unknown, 1)]);
    let f = fake();
    let s = run_core(&p, &[], "pc1", &f, Some(1)).unwrap();
    assert!(f.log().appended.is_empty());
    assert!(s.finish().is_err());
}

// ---------------------------------------------------------------------------
// Concurrency.

fn eight() -> Plan {
    plan(
        (1..=8)
            .map(|i| entry(&format!("e{i}"), Action::Relocate, i))
            .collect(),
    )
}

fn slow(ms: u64) -> Fake {
    Fake {
        build_sleep: Some(std::time::Duration::from_millis(ms)),
        ..fake()
    }
}

#[test]
fn parallel_workers_overlap_builds() {
    let p = eight();
    let f = slow(200);
    let started = std::time::Instant::now();
    let s = run_core(&p, &[], "pc1", &f, Some(4)).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(f.max_building(), 4, "four builds at once");
    // Two slots of 200 ms, not eight (1.6 s); generous for loaded machines.
    assert!(
        elapsed < std::time::Duration::from_millis(1200),
        "{elapsed:?}"
    );
    assert_eq!(s.switched_now, 8);
    assert_eq!(s.total, 8);
    for i in 1..=8 {
        assert_eq!(
            states(&f, &format!("e{i}")),
            vec![
                RecordState::Claimed,
                RecordState::Verified,
                RecordState::Switched
            ],
            "each archive's own sequence stays ordered"
        );
    }
    // Every progress line is intact and every label is present once.
    let said = f.log().said;
    for i in 1..=8 {
        let prefix = format!("Migration {i}/8: e{i}.bin relocate -> migrate-");
        assert_eq!(said.iter().filter(|l| l.starts_with(&prefix)).count(), 1);
    }
    s.finish().unwrap();
}

#[test]
fn default_parallel_is_bounded_by_the_work() {
    assert_eq!(effective_parallel(None, 100), DEFAULT_PARALLEL);
    assert_eq!(effective_parallel(None, 2), 2);
    assert_eq!(effective_parallel(None, 0), 1);
    assert_eq!(effective_parallel(Some(0), 10), 1);
    assert_eq!(effective_parallel(Some(99), 100), MAX_PARALLEL);
    assert_eq!(effective_parallel(Some(8), 3), 3);
}

#[test]
fn one_worker_reproduces_the_sequential_order() {
    let p = plan(vec![
        entry("a", Action::Relocate, 30),
        entry("b", Action::Reencode, 10),
        entry("c", Action::Relocate, 20),
    ]);
    let f = fake();
    run_core(&p, &[], "pc1", &f, Some(1)).unwrap();
    let log = f.log();
    assert_eq!(log.built, vec!["b", "c", "a"]);
    assert_eq!(
        log.said,
        vec![
            "Migration 1/3: b.bin reencode -> migrate-id2",
            "switched b -> migrate-id2",
            "Migration 2/3: c.bin relocate -> migrate-id4",
            "switched c -> migrate-id4",
            "Migration 3/3: a.bin relocate -> migrate-id6",
            "switched a -> migrate-id6",
        ]
    );
    let entries: Vec<_> = log.appended.iter().map(|r| r.entry.as_str()).collect();
    assert_eq!(entries, vec!["b", "b", "b", "c", "c", "c", "a", "a", "a"]);
}

#[test]
fn stop_request_stops_dispatch_and_lets_in_flight_archives_finish() {
    let p = eight();
    let f = Fake {
        stop_after_builds: Some(2),
        ..slow(100)
    };
    let s = run_core(&p, &[], "pc1", &f, Some(4)).unwrap();
    assert!(s.stopped);
    let built = f.log().built;
    assert!(
        (2..=4).contains(&built.len()),
        "only in-flight archives: {built:?}"
    );
    for id in &built {
        assert_eq!(states(&f, id).last(), Some(&RecordState::Switched));
    }
    assert_eq!(s.switched_now, built.len());
    let stops = f
        .log()
        .said
        .iter()
        .filter(|l| *l == "migration_stop_requested=true")
        .count();
    assert_eq!(stops, 1);
    assert!(s.finish().is_err());
}

#[test]
fn summary_counts_match_the_sequential_run() {
    let mk = |sleep| {
        let mut f = slow(sleep);
        f.unrecoverable.insert("e2".into());
        f.provider_error.insert("e5".into());
        f.changed_source.insert("e7".into());
        f
    };
    let records = vec![
        rec("e3", RecordState::Claimed, 1, "pc1"),
        rec("e3", RecordState::Verified, 2, "pc1"),
        rec("e3", RecordState::Switched, 3, "pc1"),
        rec("e8", RecordState::Claimed, 1_000_000 - 60, "pc2"),
    ];
    let p = eight();
    let sequential = run_core(&p, &records, "pc1", &mk(0), Some(1)).unwrap();
    let parallel = run_core(&p, &records, "pc1", &mk(50), Some(4)).unwrap();
    assert_eq!(parallel, sequential);
    assert_eq!(parallel.switched_now, 3);
    assert_eq!(parallel.already_switched, 1);
    assert_eq!(parallel.lost, 1);
    assert_eq!(parallel.unknown, vec!["e5.bin", "e7.bin"]);
    assert_eq!(parallel.claimed_elsewhere, 1);
    assert!(!parallel.stopped);
}

#[test]
fn fatal_append_error_propagates_after_in_flight_archives_finish() {
    let p = eight();
    let mut f = slow(100);
    f.append_fails.insert("e1".into());
    let error = run_core(&p, &[], "pc1", &f, Some(4)).unwrap_err();
    assert!(
        format!("{error:#}").contains("no store accepted"),
        "{error:#}"
    );
    let log = f.log();
    assert!(!log.built.contains(&"e1".to_string()), "claim failed first");
    // Workers that were already building finish their archive; nothing new
    // is dispatched after the failure.
    assert!(log.built.len() <= 4, "{:?}", log.built);
    for id in &log.built {
        assert_eq!(states(&f, id).last(), Some(&RecordState::Switched));
    }
}

#[test]
fn only_entries_whose_latest_attempt_is_unknown_are_retried() {
    let p = plan(vec![
        entry("a", Action::Relocate, 10),
        entry("b", Action::Relocate, 10),
        entry("c", Action::Relocate, 10),
        entry("d", Action::Lost, 10),
        entry("e", Action::Unknown, 10),
    ]);
    let records = vec![
        rec("a", RecordState::Claimed, 1, "pc1"),
        rec("a", RecordState::Unknown, 2, "pc1"),
        rec("b", RecordState::Unknown, 1, "pc1"),
        rec("b", RecordState::Claimed, 2, "pc1"),
        rec("b", RecordState::Verified, 3, "pc1"),
        rec("b", RecordState::Switched, 4, "pc1"),
        rec("d", RecordState::Lost, 1, "pc1"),
    ];
    let retry: Vec<_> = unknown_entries(&p, &records)
        .into_iter()
        .map(|e| e.archive_id)
        .collect();
    // b recovered later, c never ran, d is lost and e was not checked at plan time.
    assert_eq!(retry, vec!["a"]);
}
