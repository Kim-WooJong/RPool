//! `pool migrate restore`: take items out of the cleanup quarantine during
//! the grace period. The objects never moved, so a restore is one journal
//! record; once deletion of an item started it can no longer be restored.
use super::fossil::fold;
use super::io::RetireIo;
use super::model::{FossilState, RetireKind, RetireRecord, RECORD_VERSION};
use crate::prelude::*;

/// Restores `items` (archive ids), or every quarantined item with `all`.
/// Returns the restored ids; fails when nothing could be restored.
pub(crate) fn restore(io: &dyn RetireIo, items: &[String], all: bool) -> Result<Vec<String>> {
    io.plan()?;
    let now = io.now();
    let fossils = fold(&io.retire_records()?);
    let wanted: Vec<String> = if all {
        fossils
            .iter()
            .filter(|(_, f)| matches!(f.state(now), Some(FossilState::Waiting | FossilState::Due)))
            .map(|(id, _)| id.clone())
            .collect()
    } else {
        items.to_vec()
    };
    let mut restored = vec![];
    let mut refused = vec![];
    for id in &wanted {
        let Some(fossil) = fossils.get(id) else {
            refused.push(format!("{id}: not in quarantine"));
            continue;
        };
        match fossil.state(now) {
            Some(FossilState::Waiting | FossilState::Due) => {
                let head = &fossil.fossil;
                io.append(&RetireRecord {
                    version: RECORD_VERSION,
                    kind: RetireKind::Restore,
                    item: head.item.clone(),
                    item_kind: head.item_kind,
                    fossil_id: head.fossil_id.clone(),
                    pc_id: io.pc_id(),
                    ts_unix: now,
                    grace_seconds: 0,
                    objects: vec![],
                    replacement: head.replacement.clone(),
                    original_name: head.original_name.clone(),
                    detail: Some("restored by user".into()),
                })?;
                io.say(&format!("restored {id}"));
                restored.push(id.clone());
            }
            Some(FossilState::Deleting | FossilState::Purged) => {
                refused.push(format!("{id}: deletion already started"))
            }
            None => refused.push(format!("{id}: not in quarantine")),
        }
    }
    for line in &refused {
        io.say(&format!("not restored {line}"));
    }
    if restored.is_empty() && !refused.is_empty() {
        bail!("nothing restored: {}", refused.join("; "));
    }
    Ok(restored)
}
