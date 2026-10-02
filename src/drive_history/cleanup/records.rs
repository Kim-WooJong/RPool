//! The cleanup journal: immutable, content-addressed records published
//! beside the purge marks (`<generation root>/history/events/<blake3>.json`
//! on every metadata replica of the newest drive generation), so every PC
//! sees what is marked, being deleted and deleted. Older RPool never lists
//! `history/`; newer RPool ignores kinds it does not know (`marks::purged`
//! only reads `purge`).
//!
//! Folding per archive, over the records of every PC (order-free,
//! duplicates harmless):
//! - every `Mark` naming the archive starts a grace period of its own;
//!   a `Cancel` naming the same (archive, mark) ends it. The archive waits
//!   until the earliest remaining mark is due.
//! - `Deleting` is the point of no return, unless the deleting run itself
//!   released it again (`Cancel` with `release_deleting`, written before
//!   any of its objects was deleted). A user `--cancel` never does.
//! - `Deleted` ends it (every object of the archive is gone).
use super::super::marks::MarkStore;
use crate::prelude::*;

/// Record format version; records with another format are skipped on read.
pub(crate) const FORMAT: u32 = 1;
/// `kind` of cleanup records, distinguishing them from purge marks in the same store.
pub(crate) const KIND: &str = "cleanup";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Journal step a record represents (see the module docs for folding rules).
pub(crate) enum Step {
    /// Start a grace period for the named archives.
    Mark,
    /// End a mark's grace period (or, with `release_deleting`, undo a run's `Deleting`).
    Cancel,
    /// Deletion of the named archives started (point of no return).
    Deleting,
    /// Every object of the named archives is gone.
    Deleted,
}

/// Size of an archive when it was marked (for reports).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Info {
    /// Stored objects of the archive.
    pub objects: u64,
    /// Stored bytes of the archive.
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One immutable journal record, published content-addressed by `publish`.
pub(crate) struct Record {
    /// Always `FORMAT`.
    pub format: u32,
    /// Always [`KIND`].
    pub kind: String,
    /// What this record does.
    pub step: Step,
    /// Random id of the mark (`Mark`, `Cancel`) or of the deletion run
    /// (`Deleting`, its `Cancel`, `Deleted`).
    pub mark: String,
    /// Archive folder name -> its size when recorded.
    pub archives: BTreeMap<String, Info>,
    /// `Mark`: seconds until the archives may be deleted.
    #[serde(default)]
    pub grace_seconds: u64,
    /// `Cancel` of a deletion run's own `Deleting` (nothing deleted yet).
    #[serde(default)]
    pub release_deleting: bool,
    /// PC/worker that published the record.
    pub worker: String,
    /// Publication time (unix seconds).
    pub unix: u64,
}

impl Record {
    /// A record with no grace period and `release_deleting` off.
    pub(crate) fn new(
        step: Step,
        mark: &str,
        archives: BTreeMap<String, Info>,
        worker: &str,
        unix: u64,
    ) -> Self {
        Self {
            format: FORMAT,
            kind: KIND.into(),
            step,
            mark: mark.into(),
            archives,
            grace_seconds: 0,
            release_deleting: false,
            worker: worker.into(),
            unix,
        }
    }
}

/// Every cleanup record over the replicas (union; other kinds and formats
/// are skipped).
pub(crate) fn read(stores: &[&dyn MarkStore]) -> Result<Vec<Record>> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for store in stores {
        for (id, bytes) in store.all()? {
            if !seen.insert(id) {
                continue;
            }
            if let Ok(record) = serde_json::from_slice::<Record>(&bytes) {
                if record.format == FORMAT && record.kind == KIND {
                    out.push(record);
                }
            }
        }
    }
    Ok(out)
}

/// Publishes `record` to every replica (retryable: same bytes, same id).
pub(crate) fn publish(stores: &[&dyn MarkStore], record: &Record) -> Result<()> {
    if stores.is_empty() {
        bail!("metadata destinations missing");
    }
    let bytes = serde_json::to_vec(record)?;
    let id = blake3::hash(&bytes).to_hex().to_string();
    for store in stores {
        store.publish(&id, &bytes)?;
    }
    Ok(())
}

/// Folded cleanup state of one archive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct State {
    /// Marks still pending: mark id -> due time.
    pub marks: BTreeMap<String, u64>,
    /// Deletion started (point of no return) and not finished.
    pub deleting: bool,
    /// Every object is gone; overrides marks and `deleting`.
    pub deleted: bool,
    /// Size when last marked.
    pub info: Info,
}

impl State {
    /// Earliest time the archive may be deleted (`None`: not marked).
    pub(crate) fn due(&self) -> Option<u64> {
        self.marks.values().min().copied()
    }
    /// Its data must no longer be offered for restore.
    pub(crate) fn unrestorable(&self) -> bool {
        self.deleting || self.deleted
    }
}

/// Fold records of every PC into per-archive state (order-free); archives
/// with nothing pending are dropped. Used by `execute::run` and `unrestorable`.
pub(crate) fn fold(records: &[Record]) -> BTreeMap<String, State> {
    let mut cancelled: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut released: BTreeSet<(&str, &str)> = BTreeSet::new();
    for record in records.iter().filter(|r| r.step == Step::Cancel) {
        for archive in record.archives.keys() {
            cancelled.insert((archive.as_str(), record.mark.as_str()));
            if record.release_deleting {
                released.insert((archive.as_str(), record.mark.as_str()));
            }
        }
    }
    let mut out: BTreeMap<String, State> = BTreeMap::new();
    let mut marked_at: BTreeMap<&str, u64> = BTreeMap::new();
    for record in records {
        for (archive, info) in &record.archives {
            let key = (archive.as_str(), record.mark.as_str());
            let state = out.entry(archive.clone()).or_default();
            match record.step {
                Step::Mark if !cancelled.contains(&key) => {
                    state
                        .marks
                        .insert(record.mark.clone(), record.unix + record.grace_seconds);
                    let newest = marked_at.entry(archive).or_insert(0);
                    if record.unix >= *newest {
                        *newest = record.unix;
                        state.info = *info;
                    }
                }
                Step::Deleting if !released.contains(&key) => state.deleting = true,
                Step::Deleted => state.deleted = true,
                _ => {}
            }
        }
    }
    for state in out.values_mut() {
        if state.deleted {
            state.deleting = false;
            state.marks.clear();
        }
    }
    out.retain(|_, s| s.deleted || s.deleting || !s.marks.is_empty());
    out
}

/// Archives whose data is gone or going (restore must refuse them).
pub(crate) fn unrestorable(records: &[Record]) -> BTreeSet<String> {
    fold(records)
        .into_iter()
        .filter(|(_, s)| s.unrestorable())
        .map(|(a, _)| a)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::super::marks::{fake::Store, Mark};
    use super::*;

    fn info() -> BTreeMap<String, Info> {
        [("virtual-a".to_string(), Info::default())].into()
    }

    #[test]
    fn fold_marks_cancels_and_deletion() {
        let mut mark = Record::new(Step::Mark, "m1", info(), "pc", 100);
        mark.grace_seconds = 50;
        let later = Record {
            unix: 120,
            mark: "m2".into(),
            ..mark.clone()
        };
        let state = fold(&[mark.clone(), later.clone()]);
        assert_eq!(state["virtual-a"].due(), Some(150));
        // Cancelling the first mark leaves the second one's grace.
        let cancel = Record::new(Step::Cancel, "m1", info(), "pc", 130);
        let state = fold(&[mark.clone(), later.clone(), cancel.clone()]);
        assert_eq!(state["virtual-a"].due(), Some(170));
        let cancel2 = Record::new(Step::Cancel, "m2", info(), "pc", 130);
        assert!(fold(&[mark.clone(), later, cancel, cancel2]).is_empty());
        // Deleting wins over a user cancel; only the run's own release undoes it.
        let deleting = Record::new(Step::Deleting, "run", info(), "pc", 200);
        let user = Record::new(Step::Cancel, "m1", info(), "pc", 210);
        let state = fold(&[mark.clone(), deleting.clone(), user]);
        assert!(state["virtual-a"].deleting && state["virtual-a"].unrestorable());
        let mut own = Record::new(Step::Cancel, "run", info(), "pc", 210);
        own.release_deleting = true;
        let state = fold(&[mark.clone(), deleting.clone(), own]);
        assert!(!state["virtual-a"].deleting);
        assert_eq!(state["virtual-a"].due(), Some(150));
        let done = Record::new(Step::Deleted, "run", info(), "pc", 220);
        let state = fold(&[mark, deleting, done.clone()]);
        assert!(state["virtual-a"].deleted && !state["virtual-a"].deleting);
        assert_eq!(unrestorable(&[done]), ["virtual-a".to_string()].into());
    }

    #[test]
    fn records_share_the_mark_store_with_purge_marks() {
        let (a, b) = (Store::default(), Store::default());
        let stores: Vec<&dyn MarkStore> = vec![&a, &b];
        let record = Record::new(Step::Mark, "m", info(), "pc", 1);
        publish(&stores, &record).unwrap();
        super::super::super::marks::publish(&stores, &Mark::purge(["x".into()].into(), "pc", 1))
            .unwrap();
        assert_eq!(read(&stores).unwrap(), vec![record]);
        // Purge marks do not see cleanup records.
        assert_eq!(
            super::super::super::marks::purged(&stores).unwrap(),
            ["x".to_string()].into()
        );
        assert!(publish(&[], &Record::new(Step::Mark, "m", info(), "pc", 1)).is_err());
    }
}
