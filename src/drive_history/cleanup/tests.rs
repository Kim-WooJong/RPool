//! Cleanup runs over fake stores and a fake clock: candidate selection,
//! dry run, mark → grace → delete, re-references, unreadable sources, the
//! mass-delete guard, resume after an interruption, cancel.
use super::execute::{run, CleanupIo, Options};
use super::records::{self, Record, Step};
use super::select::{select, World};
use crate::drive_history::marks::{fake::Store, MarkStore};
use crate::drive_history::model::{CleanupMode, CleanupReport, CleanupSettings, Retention};
use crate::drive_history::retention::DAY;
use crate::drive_history::source_v6::fixture::{add, event};
use crate::migration::enumerate::RemoteListing;
use crate::migration::retire::refs::References;
use crate::mount::history_bridge::Event;
use crate::prelude::*;
use std::cell::{Cell, RefCell};

const ROOT: &str = "crypt:";

/// Runs before one observation (a change another PC makes meanwhile).
type Hook = Box<dyn Fn(&Fake)>;

/// A drive with its events, record times, stored objects and journal.
struct Fake {
    events: RefCell<BTreeMap<String, Event>>,
    times: RefCell<BTreeMap<String, u64>>,
    purged: RefCell<BTreeSet<String>>,
    /// Stored objects under `crypt:` (path -> size).
    objects: RefCell<BTreeMap<String, u64>>,
    refs: RefCell<References>,
    uncertain: RefCell<Vec<String>>,
    store: Store,
    now: Cell<u64>,
    deleted: RefCell<Vec<String>>,
    /// Fail the delete after this many successful ones.
    fail_after: Cell<Option<usize>>,
    observed: Cell<usize>,
    /// Runs before the n-th observation (1-based).
    hook: RefCell<Option<(usize, Hook)>>,
    ids: Cell<u64>,
}

impl Fake {
    fn new() -> Self {
        Self {
            events: RefCell::default(),
            times: RefCell::default(),
            purged: RefCell::default(),
            objects: RefCell::default(),
            refs: RefCell::default(),
            uncertain: RefCell::default(),
            store: Store::default(),
            now: Cell::new(0),
            deleted: RefCell::default(),
            fail_after: Cell::new(None),
            observed: Cell::new(0),
            hook: RefCell::new(None),
            ids: Cell::new(0),
        }
    }

    /// Adds a revision (stores its objects) and returns its id.
    fn rev(&self, path: &str, parents: &[&str], text: Option<&str>, time: Option<u64>) -> String {
        let e = event("pc", path, parents, text);
        self.store_objects(&e);
        let id = add(&mut self.events.borrow_mut(), e);
        if let Some(time) = time {
            self.times.borrow_mut().insert(id.clone(), time);
        }
        id
    }

    fn store_objects(&self, e: &Event) {
        if let Some(content) = &e.content {
            let m = &content.manifest;
            let mut objects = self.objects.borrow_mut();
            objects.insert(format!("{}/manifest.json", m.archive_id), 100);
            for shard in &m.shards {
                let rel = shard.object.strip_prefix(ROOT).unwrap().to_owned();
                objects.insert(rel, shard.size);
            }
        }
    }

    fn content(&self, id: &str) -> crate::mount::history_bridge::Content {
        self.events.borrow()[id].content.clone().unwrap()
    }

    /// A restore: a new revision at `path` (after `parent`) with `old`'s bytes.
    fn restore(&self, path: &str, parent: &str, old: &str, time: u64) -> String {
        let mut e = event("pc", path, &[parent], None);
        e.content = Some(self.content(old));
        let id = add(&mut self.events.borrow_mut(), e);
        self.times.borrow_mut().insert(id.clone(), time);
        id
    }

    fn archive(&self, id: &str) -> String {
        self.content(id).manifest.archive_id
    }

    fn stored(&self, archive: &str) -> bool {
        self.objects
            .borrow()
            .keys()
            .any(|p| p.starts_with(&format!("{archive}/")))
    }

    fn journal(&self) -> Vec<Record> {
        let stores: Vec<&dyn MarkStore> = vec![&self.store];
        records::read(&stores).unwrap()
    }

    fn world(&self) -> World {
        let history = crate::drive_history::source_v6::build(
            self.events.borrow().clone(),
            &self.times.borrow(),
            &BTreeSet::new(),
            self.now.get(),
            self.purged.borrow().clone(),
        )
        .unwrap();
        World {
            histories: vec![("generation".into(), history)],
            local_kept: Vec::new(),
            refs: self.refs.borrow().clone(),
            listings: [(
                ROOT.to_string(),
                RemoteListing::Listed(self.objects.borrow().clone()),
            )]
            .into(),
            uncertain: self.uncertain.borrow().clone(),
        }
    }
}

impl CleanupIo for Fake {
    fn records(&self) -> Result<Vec<Record>> {
        Ok(self.journal())
    }
    fn publish(&self, record: &Record) -> Result<()> {
        let stores: Vec<&dyn MarkStore> = vec![&self.store];
        records::publish(&stores, record)
    }
    fn observe(&self) -> Result<World> {
        let n = self.observed.get() + 1;
        self.observed.set(n);
        if let Some((at, hook)) = &*self.hook.borrow() {
            if *at == n {
                hook(self);
            }
        }
        Ok(self.world())
    }
    fn delete(&self, address: &str) -> Result<()> {
        if let Some(left) = self.fail_after.get() {
            if left == 0 {
                bail!("provider unavailable");
            }
            self.fail_after.set(Some(left - 1));
        }
        let rel = address.strip_prefix(ROOT).unwrap();
        self.objects.borrow_mut().remove(rel);
        self.deleted.borrow_mut().push(address.into());
        Ok(())
    }
    fn now(&self) -> u64 {
        self.now.get()
    }
    fn new_id(&self) -> Result<String> {
        self.ids.set(self.ids.get() + 1);
        Ok(format!("id{}", self.ids.get()))
    }
    fn worker(&self) -> String {
        "pc".into()
    }
}

fn retention() -> Retention {
    Retention {
        trash_days: 30,
        keep_versions: 1,
        version_days: 0,
    }
}

fn options(confirm: bool) -> Options {
    let mut options = Options::new(retention(), CleanupSettings::default());
    options.confirm = confirm;
    options
}

fn go(fake: &Fake, confirm: bool) -> CleanupReport {
    run(fake, "p", &options(confirm)).unwrap()
}

/// Revisions of the standard drive (see `drive`).
struct Drive {
    v1: String,
    v3: String,
    d1: String,
    t1: String,
    p1: String,
    u1: String,
    m1: String,
    i1: String,
    s1: String,
}

/// f.txt v1→v2→v3 (keep 1 version: v1 goes), d.txt deleted day 2 (expired
/// by day 40), t.txt deleted day 35 (still in the trash), p.txt purged,
/// u.txt deleted at an unknown time, m.txt / i.txt expired but referenced
/// by a migration / an inventory manifest, s.txt's old version shares a
/// shard with the current one.
fn drive(fake: &Fake) -> Drive {
    let v1 = fake.rev("f.txt", &[], Some("one"), Some(DAY));
    let v2 = fake.rev("f.txt", &[&v1], Some("two"), Some(2 * DAY));
    let v3 = fake.rev("f.txt", &[&v2], Some("three"), Some(3 * DAY));
    let d1 = fake.rev("d.txt", &[], Some("gone"), Some(DAY));
    fake.rev("d.txt", &[&d1], None, Some(2 * DAY));
    let t1 = fake.rev("t.txt", &[], Some("in trash"), Some(DAY));
    fake.rev("t.txt", &[&t1], None, Some(35 * DAY));
    let p1 = fake.rev("p.txt", &[], Some("purged"), Some(DAY));
    let p2 = fake.rev("p.txt", &[&p1], None, Some(35 * DAY));
    fake.purged.borrow_mut().insert(p2);
    let u1 = fake.rev("u.txt", &[], Some("unknown"), None);
    fake.rev("u.txt", &[&u1], None, None);
    let m1 = fake.rev("m.txt", &[], Some("migrating"), Some(DAY));
    fake.rev("m.txt", &[&m1], None, Some(2 * DAY));
    let i1 = fake.rev("i.txt", &[], Some("indexed"), Some(DAY));
    fake.rev("i.txt", &[&i1], None, Some(2 * DAY));
    let s1 = fake.rev("s.txt", &[], Some("base"), Some(DAY));
    let s2 = fake.rev("s.txt", &[&s1], Some("mid"), Some(2 * DAY));
    // s3 (current) reuses s1's shard object, like an incremental upload
    // whose unchanged groups stay in the base's archive.
    let mut s3 = event("pc", "s.txt", &[&s2], None);
    let mut shared = fake.content(&s1);
    shared.manifest.archive_id = "virtual-s3".into();
    s3.content = Some(shared);
    fake.store_objects(&s3);
    let s3 = add(&mut fake.events.borrow_mut(), s3);
    fake.times.borrow_mut().insert(s3, 3 * DAY);
    let mut refs = References::default();
    refs.add_text("migration abc (in progress)", &fake.archive(&m1));
    let mut indexed = fake.content(&i1).manifest.clone();
    indexed.archive_id = "regular-archive".into();
    refs.add_manifest("inventory entry regular-archive", &indexed);
    *fake.refs.borrow_mut() = refs;
    fake.now.set(40 * DAY);
    Drive {
        v1,
        v3,
        d1,
        t1,
        p1,
        u1,
        m1,
        i1,
        s1,
    }
}

#[test]
fn candidates_respect_every_kind_of_reference() {
    let fake = Fake::new();
    let d = drive(&fake);
    let selection = select(&fake.world(), &retention(), fake.now.get());
    assert!(selection.uncertain.is_empty(), "{:?}", selection.uncertain);
    let expected: BTreeSet<String> = [&d.v1, &d.d1, &d.p1]
        .iter()
        .map(|id| fake.archive(id))
        .collect();
    let found: BTreeSet<String> = selection.candidates.keys().cloned().collect();
    assert_eq!(found, expected);
    for kept in [&d.v3, &d.t1, &d.u1, &d.m1, &d.i1, &d.s1] {
        assert!(!found.contains(&fake.archive(kept)));
    }
    let archive = &selection.candidates[&fake.archive(&d.v1)];
    assert!(archive.version);
    assert!(archive.objects[0].address.ends_with("/manifest.json"));
    // Unknown times never expire, even with 1-day limits.
    let short = Retention {
        trash_days: 1,
        keep_versions: 0,
        version_days: 1,
    };
    let selection = select(&fake.world(), &short, fake.now.get());
    assert!(!selection.candidates.contains_key(&fake.archive(&d.u1)));
    assert!(selection.candidates.contains_key(&fake.archive(&d.t1)));
    // Unlimited retention keeps everything except purged data.
    let unlimited = Retention {
        trash_days: 0,
        keep_versions: 0,
        version_days: 0,
    };
    let selection = select(&fake.world(), &unlimited, fake.now.get());
    let found: BTreeSet<String> = selection.candidates.keys().cloned().collect();
    assert_eq!(found, [fake.archive(&d.p1)].into());
}

#[test]
fn dry_run_writes_and_deletes_nothing() {
    let fake = Fake::new();
    drive(&fake);
    let before = fake.objects.borrow().clone();
    let report = go(&fake, false);
    assert_eq!(report.mode, CleanupMode::Preview);
    assert_eq!(report.candidates.archives, 3);
    assert_eq!(report.candidates.files, 3);
    assert!(report.candidates.bytes > 0);
    assert_eq!(report.accounts.len(), 1);
    assert_eq!(report.accounts[0].account, "crypt");
    assert!(fake.store.0.borrow().is_empty());
    assert_eq!(*fake.objects.borrow(), before);
}

#[test]
fn mark_then_grace_then_delete() {
    let fake = Fake::new();
    let d = drive(&fake);
    let start = fake.now.get();
    let report = go(&fake, true);
    assert_eq!(report.mode, CleanupMode::Applied);
    assert_eq!(report.candidates.archives, 3);
    assert_eq!(report.next_deletion_unix, Some(start + 7 * DAY));
    assert!(fake.deleted.borrow().is_empty());
    let journal = fake.journal();
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0].step, Step::Mark);
    // Inside the grace period: waiting, nothing deleted, no new mark.
    fake.now.set(start + 3 * DAY);
    let report = go(&fake, true);
    assert_eq!(
        (report.candidates.archives, report.waiting.archives),
        (0, 3)
    );
    assert_eq!(report.next_deletion_unix, Some(start + 7 * DAY));
    assert!(fake.deleted.borrow().is_empty());
    assert_eq!(fake.journal().len(), 1);
    // A preview past the grace shows it as due.
    fake.now.set(start + 8 * DAY);
    let preview = go(&fake, false);
    assert_eq!(preview.due.archives, 3);
    assert!(fake.deleted.borrow().is_empty());
    let report = go(&fake, true);
    assert_eq!(report.deleted.archives, 3);
    assert_eq!(report.deleted.objects, 6);
    for gone in [&d.v1, &d.d1, &d.p1] {
        assert!(!fake.stored(&fake.archive(gone)));
    }
    for kept in [&d.v3, &d.t1, &d.u1, &d.m1, &d.i1, &d.s1] {
        assert!(fake.stored(&fake.archive(kept)), "{kept}");
    }
    // Manifest replicas go before the shards of each archive.
    let deleted = fake.deleted.borrow().clone();
    assert!(deleted[0].ends_with("/manifest.json"));
    let steps: Vec<Step> = fake.journal().iter().map(|r| r.step).collect();
    assert!(steps.contains(&Step::Deleting) && steps.contains(&Step::Deleted));
    let state = records::fold(&fake.journal());
    assert!(state.values().all(|s| s.deleted));
    assert_eq!(records::unrestorable(&fake.journal()).len(), 3);
    // Nothing left to do.
    let report = go(&fake, true);
    assert_eq!(
        (
            report.candidates.archives,
            report.due.archives,
            report.deleted.archives
        ),
        (0, 0, 0)
    );
}

#[test]
fn a_reference_during_the_grace_period_releases_the_mark() {
    let fake = Fake::new();
    let d = drive(&fake);
    go(&fake, true);
    // Version one is restored (made current again) before the grace ends.
    fake.restore("f.txt", &d.v3, &d.v1, 41 * DAY);
    fake.now.set(48 * DAY);
    let report = go(&fake, true);
    assert_eq!(report.released, 1);
    assert_eq!(report.deleted.archives, 2);
    assert!(fake.stored(&fake.archive(&d.v1)));
    assert!(!records::fold(&fake.journal()).contains_key(&fake.archive(&d.v1)));
}

#[test]
fn a_reference_found_by_the_recheck_before_deleting_keeps_the_archive() {
    let fake = Fake::new();
    let d = drive(&fake);
    go(&fake, true);
    fake.now.set(48 * DAY);
    let (v1, v3) = (d.v1.clone(), d.v3.clone());
    // The second observation of the deleting run sees a restore.
    *fake.hook.borrow_mut() = Some((
        2,
        Box::new(move |f: &Fake| {
            f.restore("f.txt", &v3, &v1, 47 * DAY);
        }),
    ));
    fake.observed.set(0);
    let report = go(&fake, true);
    assert_eq!(report.deleted.archives, 2);
    assert!(fake.stored(&fake.archive(&d.v1)));
    let state = records::fold(&fake.journal());
    assert!(!state.contains_key(&fake.archive(&d.v1)), "{state:?}");
    assert!(!records::unrestorable(&fake.journal()).contains(&fake.archive(&d.v1)));
}

#[test]
fn an_unreadable_source_postpones_everything() {
    let fake = Fake::new();
    drive(&fake);
    fake.uncertain
        .borrow_mut()
        .push("manifest of x: timeout".into());
    let report = go(&fake, true);
    assert_eq!(report.mode, CleanupMode::Postponed);
    assert_eq!(report.postponed, vec!["manifest of x: timeout".to_string()]);
    assert!(fake.store.0.borrow().is_empty());
    // Marked data past its grace is not deleted while a source is unreadable.
    fake.uncertain.borrow_mut().clear();
    go(&fake, true);
    fake.now.set(60 * DAY);
    fake.refs
        .borrow_mut()
        .uncertain("inventory entry y: unreadable".into());
    let report = go(&fake, true);
    assert_eq!(report.mode, CleanupMode::Postponed);
    assert_eq!(report.waiting.archives, 3);
    assert!(fake.deleted.borrow().is_empty());
}

#[test]
fn the_mass_delete_guard_needs_force() {
    let fake = Fake::new();
    drive(&fake);
    go(&fake, true);
    fake.now.set(60 * DAY);
    let mut limited = options(true);
    limited.max_objects = 2;
    let report = run(&fake, "p", &limited).unwrap();
    assert!(report.guard.as_deref().unwrap().contains("limit of 2"));
    assert!(fake.deleted.borrow().is_empty());
    // The preview tells the GUI that a second confirmation is needed.
    limited.confirm = false;
    assert!(run(&fake, "p", &limited).unwrap().guard.is_some());
    limited.confirm = true;
    limited.force = true;
    let report = run(&fake, "p", &limited).unwrap();
    assert_eq!(report.deleted.archives, 3);
    // Half of the drive's stored objects is the default share limit.
    let fake = Fake::new();
    let v1 = fake.rev("a.txt", &[], Some("1"), Some(DAY));
    fake.rev("a.txt", &[&v1], None, Some(DAY));
    fake.now.set(40 * DAY);
    go(&fake, true);
    fake.now.set(50 * DAY);
    let report = go(&fake, true);
    assert!(report.guard.unwrap().contains("more than 50%"));
    assert!(fake.deleted.borrow().is_empty());
}

#[test]
fn an_interrupted_deletion_resumes_and_cannot_be_cancelled() {
    let fake = Fake::new();
    drive(&fake);
    go(&fake, true);
    fake.now.set(48 * DAY);
    fake.fail_after.set(Some(3));
    let error = run(&fake, "p", &options(true)).unwrap_err();
    assert!(format!("{error:#}").contains("resumes"));
    let state = records::fold(&fake.journal());
    let deleted = state.values().filter(|s| s.deleted).count();
    let deleting = state.values().filter(|s| s.deleting).count();
    assert_eq!((deleted, deleting), (1, 2), "{state:?}");
    assert_eq!(records::unrestorable(&fake.journal()).len(), 3);
    // A user cancel does not stop a started deletion.
    let mut cancel = options(false);
    cancel.cancel = true;
    let report = run(&fake, "p", &cancel).unwrap();
    assert_eq!(report.mode, CleanupMode::Cancelled);
    assert_eq!(report.released, 0);
    assert!(report
        .notes
        .iter()
        .any(|n| n.contains("cannot be cancelled")));
    fake.fail_after.set(None);
    let report = go(&fake, true);
    assert_eq!(report.deleted.archives, 2);
    assert!(records::fold(&fake.journal()).values().all(|s| s.deleted));
}

#[test]
fn cancel_drops_pending_marks() {
    let fake = Fake::new();
    drive(&fake);
    go(&fake, true);
    let mut cancel = options(false);
    cancel.cancel = true;
    let report = run(&fake, "p", &cancel).unwrap();
    assert_eq!(report.released, 3);
    assert!(records::fold(&fake.journal()).is_empty());
    fake.now.set(60 * DAY);
    let preview = go(&fake, false);
    assert_eq!((preview.candidates.archives, preview.due.archives), (3, 0));
    assert!(fake.deleted.borrow().is_empty());
}

#[test]
fn deleted_data_is_no_longer_restorable() {
    let fake = Fake::new();
    let d = drive(&fake);
    go(&fake, true);
    fake.now.set(48 * DAY);
    go(&fake, true);
    let mut history = fake.world().histories.remove(0).1;
    assert!(history.payloads.contains_key(&d.v1));
    crate::drive_history::load::forget_cleaned(
        &mut history,
        &records::unrestorable(&fake.journal()),
    );
    assert!(!history.payloads.contains_key(&d.v1));
    assert!(history.payloads.contains_key(&d.v3));
    let versions = crate::drive_history::versions::list(&history, "f.txt").unwrap();
    let v1 = versions.iter().find(|v| v.id == d.v1).unwrap();
    assert!(!v1.restorable);
}

#[test]
fn mount_log_lines() {
    let fake = Fake::new();
    drive(&fake);
    let report = go(&fake, true);
    let line = super::log_line(&report).unwrap();
    assert!(line.contains("marked"), "{line}");
    fake.now.set(41 * DAY);
    assert!(super::log_line(&go(&fake, true)).is_none());
}
