//! Quarantine ("fossil", as in Duplicacy's two-step pruning) state from the
//! cleanup records. A fossil only records the exact objects; they stay where
//! they are, fully readable, until a later run past the grace deletes them.
//!
//! Folding per item, over the records of every PC (order-free, duplicates
//! harmless):
//! - the active quarantine is the newest `Fossil` (by time, then id), except
//!   that a quarantine whose deletion started always wins;
//! - `Restore` / `Cancelled` release it (it may become a candidate again),
//!   unless deletion already started: `Deleting` is the point of no return
//!   (a `Cancelled` written right after `Deleting`, before any object was
//!   deleted, still releases it);
//! - `Deleted` records accumulate the deleted objects; `Purged` ends it.
use super::model::{FossilState, FossilView, Item, RetireKind, RetireRecord};
use crate::prelude::*;

/// Folded state of one item's active quarantine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ItemFossil {
    /// The active `Fossil` record: objects, grace and quarantine time.
    pub fossil: RetireRecord,
    /// A `Restore` record was seen for this generation.
    pub restored: bool,
    /// A `Cancelled` record was seen for this generation.
    pub cancelled: bool,
    /// A `Deleting` record was seen (point of no return).
    pub deleting: bool,
    /// Addresses recorded as deleted by `Deleted` records.
    pub deleted: BTreeSet<String>,
    /// A `Purged` record ended the quarantine.
    pub purged: bool,
}

impl ItemFossil {
    /// Deletion has started and was not cancelled before the first object.
    pub(crate) fn deletion_started(&self) -> bool {
        self.purged || (self.deleting && !(self.cancelled && self.deleted.is_empty()))
    }

    /// No longer in quarantine (restored or cancelled before deletion).
    pub(crate) fn released(&self) -> bool {
        !self.deletion_started() && (self.restored || self.cancelled)
    }

    /// Unix time (s) from which deletion is allowed: quarantine time plus grace.
    pub(crate) fn due_unix(&self) -> u64 {
        self.fossil
            .ts_unix
            .saturating_add(self.fossil.grace_seconds)
    }

    /// `None` when released.
    pub(crate) fn state(&self, now: u64) -> Option<FossilState> {
        if self.purged {
            Some(FossilState::Purged)
        } else if self.deletion_started() {
            Some(FossilState::Deleting)
        } else if self.released() {
            None
        } else if now >= self.due_unix() {
            Some(FossilState::Due)
        } else {
            Some(FossilState::Waiting)
        }
    }

    /// Objects of the fossil not yet recorded as deleted.
    pub(crate) fn remaining(&self) -> Vec<super::model::RetireObject> {
        self.fossil
            .objects
            .iter()
            .filter(|o| !self.deleted.contains(&o.address))
            .cloned()
            .collect()
    }

    /// Report view of the quarantine at `now`; `None` when released.
    pub(crate) fn view(&self, now: u64) -> Option<FossilView> {
        let state = self.state(now)?;
        Some(FossilView {
            item: Item {
                archive_id: self.fossil.item.clone(),
                kind: self.fossil.item_kind,
                original_name: self.fossil.original_name.clone(),
                replacement: self.fossil.replacement.clone(),
                objects: self.fossil.objects.clone(),
            },
            fossil_id: self.fossil.fossil_id.clone(),
            state,
            since_unix: self.fossil.ts_unix,
            due_unix: self.due_unix(),
            by_pc: self.fossil.pc_id.clone(),
            remaining: self.remaining().len(),
            restore_refused: self.restored && self.deletion_started(),
            blocked: None,
        })
    }
}

/// Active quarantine per item id.
pub(crate) fn fold(records: &[RetireRecord]) -> BTreeMap<String, ItemFossil> {
    // Generations: (item, fossil_id) -> records.
    let mut generations: BTreeMap<(&str, &str), Vec<&RetireRecord>> = BTreeMap::new();
    for record in records {
        generations
            .entry((record.item.as_str(), record.fossil_id.as_str()))
            .or_default()
            .push(record);
    }
    let mut out: BTreeMap<String, ItemFossil> = BTreeMap::new();
    for ((item, _), group) in generations {
        // A generation without its Fossil record (a lost replica) is ignored.
        let Some(fossil) = group
            .iter()
            .filter(|r| r.kind == RetireKind::Fossil)
            .min_by(|a, b| (a.ts_unix, &a.pc_id).cmp(&(b.ts_unix, &b.pc_id)))
        else {
            continue;
        };
        let has = |kind| group.iter().any(|r| r.kind == kind);
        let folded = ItemFossil {
            fossil: (*fossil).clone(),
            restored: has(RetireKind::Restore),
            cancelled: has(RetireKind::Cancelled),
            deleting: has(RetireKind::Deleting),
            deleted: group
                .iter()
                .filter(|r| r.kind == RetireKind::Deleted)
                .flat_map(|r| r.objects.iter().map(|o| o.address.clone()))
                .collect(),
            purged: has(RetireKind::Purged),
        };
        let replace = match out.get(item) {
            None => true,
            Some(current) => {
                let rank = |f: &ItemFossil| {
                    (
                        f.deletion_started(),
                        f.fossil.ts_unix,
                        f.fossil.fossil_id.clone(),
                    )
                };
                rank(&folded) > rank(current)
            }
        };
        if replace {
            out.insert(item.to_string(), folded);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::retire::model::{ItemKind, RetireObject, RECORD_VERSION};

    pub(crate) fn rec(item: &str, kind: RetireKind, fossil: &str, ts: u64) -> RetireRecord {
        RetireRecord {
            version: RECORD_VERSION,
            kind,
            item: item.into(),
            item_kind: ItemKind::Original,
            fossil_id: fossil.into(),
            pc_id: "pc".into(),
            ts_unix: ts,
            grace_seconds: 100,
            objects: if kind == RetireKind::Fossil {
                vec![
                    RetireObject {
                        address: "r:a/1".into(),
                        size: 1,
                        root: "r:".into(),
                    },
                    RetireObject {
                        address: "r:a/2".into(),
                        size: 2,
                        root: "r:".into(),
                    },
                ]
            } else if kind == RetireKind::Deleted {
                vec![RetireObject {
                    address: "r:a/1".into(),
                    size: 1,
                    root: "r:".into(),
                }]
            } else {
                vec![]
            },
            replacement: None,
            original_name: "a.bin".into(),
            detail: None,
        }
    }

    #[test]
    fn grace_restore_and_point_of_no_return() {
        let f = fold(&[rec("a", RetireKind::Fossil, "f1", 10)]);
        assert_eq!(f["a"].state(50), Some(FossilState::Waiting));
        assert_eq!(f["a"].state(110), Some(FossilState::Due));
        let f = fold(&[
            rec("a", RetireKind::Fossil, "f1", 10),
            rec("a", RetireKind::Restore, "f1", 20),
        ]);
        assert_eq!(f["a"].state(500), None);
        // Quarantined again later: the new generation counts.
        let f = fold(&[
            rec("a", RetireKind::Fossil, "f1", 10),
            rec("a", RetireKind::Restore, "f1", 20),
            rec("a", RetireKind::Fossil, "f2", 30),
        ]);
        assert_eq!(f["a"].fossil.fossil_id, "f2");
        assert_eq!(f["a"].state(60), Some(FossilState::Waiting));
        // Deleting wins over a late restore; deleted objects accumulate.
        let f = fold(&[
            rec("a", RetireKind::Fossil, "f1", 10),
            rec("a", RetireKind::Deleting, "f1", 200),
            rec("a", RetireKind::Restore, "f1", 201),
            rec("a", RetireKind::Deleted, "f1", 202),
        ]);
        let view = f["a"].view(300).unwrap();
        assert_eq!(view.state, FossilState::Deleting);
        assert!(view.restore_refused);
        assert_eq!(view.remaining, 1);
        // Cancelled right after Deleting, before any object: released.
        let f = fold(&[
            rec("a", RetireKind::Fossil, "f1", 10),
            rec("a", RetireKind::Deleting, "f1", 200),
            rec("a", RetireKind::Cancelled, "f1", 201),
        ]);
        assert_eq!(f["a"].state(300), None);
        let f = fold(&[
            rec("a", RetireKind::Fossil, "f1", 10),
            rec("a", RetireKind::Purged, "f1", 300),
        ]);
        assert_eq!(f["a"].state(300), Some(FossilState::Purged));
    }
}
