use super::tests_support::{entry, plan, rec};
use super::*;
use crate::migration::model::MissingReason;

#[derive(Default)]
struct Fake {
    appended: Vec<Record>,
    built: Vec<String>,
    reverified: Vec<String>,
    switched: Vec<(String, String)>,
    /// Entries whose build fails with a provider error / as unrecoverable.
    provider_error: BTreeSet<String>,
    unrecoverable: BTreeSet<String>,
    reverify_fails: BTreeSet<String>,
    changed_source: BTreeSet<String>,
    stop_after_builds: Option<usize>,
    take_over: bool,
    now: u64,
    ids: u64,
    said: Vec<String>,
}

impl Effects for Fake {
    fn append(&mut self, record: &Record) -> Result<()> {
        self.appended.push(record.clone());
        Ok(())
    }
    fn source_fingerprint(&mut self, entry: &Entry) -> Result<String> {
        if self.changed_source.contains(&entry.archive_id) {
            return Ok("other".into());
        }
        Ok(entry.fingerprint.clone())
    }
    fn build(&mut self, entry: &Entry, new_archive_id: &str) -> Result<Replacement> {
        self.built.push(entry.archive_id.clone());
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
    fn reverify(&mut self, entry: &Entry, _id: &str, _manifest: &str) -> Result<()> {
        self.reverified.push(entry.archive_id.clone());
        if self.reverify_fails.contains(&entry.archive_id) {
            bail!("manifest missing");
        }
        Ok(())
    }
    fn switch(&mut self, entry: &Entry, replacement: &Replacement) -> Result<()> {
        self.switched
            .push((entry.archive_id.clone(), replacement.new_archive_id.clone()));
        Ok(())
    }
    fn take_over(&self) -> bool {
        self.take_over
    }
    fn stop_requested(&self) -> bool {
        self.stop_after_builds
            .is_some_and(|n| self.built.len() >= n)
    }
    fn now(&self) -> u64 {
        self.now
    }
    fn new_id(&mut self) -> Result<String> {
        self.ids += 1;
        Ok(format!("id{}", self.ids))
    }
    fn say(&mut self, line: &str) {
        self.said.push(line.to_string());
    }
}

fn fake() -> Fake {
    Fake {
        now: 1_000_000,
        ..Default::default()
    }
}

fn states(f: &Fake, id: &str) -> Vec<RecordState> {
    f.appended
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
    let mut f = fake();
    let s = run_core(&p, &[], "pc1", &mut f).unwrap();
    assert_eq!(s.switched_now, 2);
    assert_eq!(f.built, vec!["b", "a"], "smaller first");
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
    let claim = f.appended.iter().find(|r| r.entry == "b").unwrap();
    assert!(claim
        .new_archive_id
        .as_deref()
        .unwrap()
        .starts_with("migrate-"));
    assert!(f
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
    let mut f = fake();
    let s = run_core(&p, &records, "pc1", &mut f).unwrap();
    assert_eq!(s.already_switched, 1);
    assert_eq!(f.reverified, vec!["v"]);
    assert_eq!(f.built, vec!["c", "n"]);
    assert!(states(&f, "s").is_empty());
    assert_eq!(states(&f, "v"), vec![RecordState::Switched]);
    assert_eq!(f.switched[0], ("v".into(), "new-v-5".into()));
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
    all.extend(f.appended.clone());
    let mut g = fake();
    let again = run_core(&p, &all, "pc1", &mut g).unwrap();
    assert_eq!(again.already_switched, 4);
    assert!(g.appended.is_empty() && g.built.is_empty());
}

#[test]
fn fresh_claim_from_other_pc_is_left_alone() {
    let p = plan(vec![entry("a", Action::Relocate, 1)]);
    let records = vec![rec("a", RecordState::Claimed, 1_000_000 - 60, "pc2")];
    let mut f = fake();
    let s = run_core(&p, &records, "pc1", &mut f).unwrap();
    assert_eq!(s.claimed_elsewhere, 1);
    assert!(f.built.is_empty() && f.appended.is_empty());
    assert!(s.finish().is_err());
}

#[test]
fn take_over_rebuilds_a_fresh_claim_of_a_stopped_pc() {
    let p = plan(vec![entry("a", Action::Relocate, 1)]);
    let records = vec![rec("a", RecordState::Claimed, 1_000_000 - 60, "pc2")];
    let mut f = fake();
    f.take_over = true;
    let s = run_core(&p, &records, "pc1", &mut f).unwrap();
    assert_eq!(s.claimed_elsewhere, 0);
    assert_eq!(f.built, vec!["a"]);
    assert_eq!(*states(&f, "a").last().unwrap(), RecordState::Switched);
    assert!(s.finish().is_ok());
}

#[test]
fn failed_reverification_rebuilds() {
    let p = plan(vec![entry("v", Action::Reencode, 1)]);
    let records = vec![rec("v", RecordState::Verified, 5, "pc1")];
    let mut f = fake();
    f.reverify_fails.insert("v".into());
    run_core(&p, &records, "pc1", &mut f).unwrap();
    assert_eq!(f.built, vec!["v"]);
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
    let s = run_core(&p, &[], "pc1", &mut f).unwrap();
    assert_eq!(states(&f, "L"), vec![RecordState::Lost]);
    assert_eq!(f.appended[0].losses.len(), 1);
    assert_eq!(
        states(&f, "x"),
        vec![RecordState::Claimed, RecordState::Lost]
    );
    let x_lost = f
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
        f.built.iter().all(|id| id != "z"),
        "changed source is never built"
    );
    assert_eq!(states(&f, "w").last(), Some(&RecordState::Switched));
    assert_eq!(s.lost, 2);
    assert_eq!(s.unknown.len(), 2);
    assert!(s.clone().finish().is_err(), "unknown entries make run fail");

    // Resume: lost entries are not re-recorded, unknown ones are retried.
    let mut all = f.appended.clone();
    let mut g = fake();
    g.now += 10;
    let s2 = run_core(&p, &all, "pc1", &mut g).unwrap();
    assert!(states(&g, "L").is_empty() && states(&g, "x").is_empty());
    assert_eq!(g.built, vec!["y", "z"]);
    assert!(s2.unknown.is_empty());
    all.extend(g.appended);
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
    let s = run_core(&p, &[], "pc1", &mut f).unwrap();
    assert!(s.stopped);
    assert_eq!(f.built, vec!["a"]);
    assert_eq!(states(&f, "a").last(), Some(&RecordState::Switched));
    assert!(s.finish().is_err());
}

#[test]
fn abandoned_migration_refuses_to_run() {
    let p = plan(vec![entry("a", Action::Relocate, 1)]);
    let records = vec![rec("", RecordState::Abandoned, 1, "pc1")];
    let mut f = fake();
    assert!(run_core(&p, &records, "pc1", &mut f).is_err());
    assert!(f.appended.is_empty());
}

#[test]
fn plan_time_unknown_entries_make_run_fail_without_work() {
    let p = plan(vec![entry("q", Action::Unknown, 1)]);
    let mut f = fake();
    let s = run_core(&p, &[], "pc1", &mut f).unwrap();
    assert!(f.appended.is_empty());
    assert!(s.finish().is_err());
}
