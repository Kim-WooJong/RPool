//! Status (work package C): fold the journal of each migration into a
//! summary, including the list of unrecoverable files.
use super::execute::{is_abandoned, progress, Progress};
use super::journal::{discover, Journal};
use super::model::{Action, LostFile, MigrationStatus, Plan, Record};
use crate::prelude::*;

/// Migrations of `pool` found in the cloud (or only `migration_id`), newest first.
pub(crate) fn status(
    rclone: &str,
    pool: &str,
    migration_id: Option<&str>,
) -> Result<Vec<MigrationStatus>> {
    let ids = match migration_id {
        Some(id) => vec![id.to_string()],
        None => discover(rclone, pool)?,
    };
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let journal = Journal::open(rclone, pool, &id)?;
        let Some(plan) = journal.load_plan()? else {
            if migration_id.is_some() {
                bail!("migration not found in the cloud: {id}");
            }
            continue;
        };
        let records = journal.records()?;
        out.push(summarize(&plan, &records));
    }
    out.sort_by(|a, b| {
        b.created_unix
            .cmp(&a.created_unix)
            .then_with(|| b.migration_id.cmp(&a.migration_id))
    });
    Ok(out)
}

/// Builds the status of one migration from its frozen plan and records.
pub(crate) fn summarize(plan: &Plan, records: &[Record]) -> MigrationStatus {
    let state = progress(records);
    let mut lost: BTreeMap<String, LostFile> = BTreeMap::new();
    for entry in plan.entries.iter().filter(|e| e.action == Action::Lost) {
        lost.insert(
            entry.archive_id.clone(),
            LostFile {
                archive_id: entry.archive_id.clone(),
                original_name: entry.original_name.clone(),
                size: entry.size,
                groups: entry.losses.clone(),
                detected: "plan".into(),
            },
        );
    }
    let (mut to_move, mut switched, mut verified, mut failed_unknown, mut movable_left) =
        (0, 0, 0, 0, 0);
    for entry in &plan.entries {
        if !matches!(entry.action, Action::Relocate | Action::Reencode) {
            continue;
        }
        to_move += 1;
        match state.get(&entry.archive_id) {
            Some(Progress::Switched(_)) => switched += 1,
            Some(Progress::Verified(_)) => {
                verified += 1;
                movable_left += 1;
            }
            Some(Progress::Lost(r)) => {
                lost.entry(entry.archive_id.clone()).or_insert(LostFile {
                    archive_id: entry.archive_id.clone(),
                    original_name: entry.original_name.clone(),
                    size: entry.size,
                    groups: r.losses.clone(),
                    detected: "run".into(),
                });
            }
            Some(Progress::Unknown(_)) => {
                failed_unknown += 1;
                movable_left += 1;
            }
            Some(Progress::Claimed(_)) | None => movable_left += 1,
        }
    }
    // PCs by most recent activity.
    let mut last_by_pc: BTreeMap<&str, u64> = BTreeMap::new();
    for r in records {
        let slot = last_by_pc.entry(r.pc_id.as_str()).or_insert(0);
        *slot = (*slot).max(r.ts_unix);
    }
    let mut pcs: Vec<(&str, u64)> = last_by_pc.into_iter().collect();
    pcs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let abandoned = is_abandoned(records);
    MigrationStatus {
        migration_id: plan.migration_id.clone(),
        pool: plan.pool.clone(),
        created_unix: plan.created_unix,
        created_by: plan.created_by.clone(),
        counts: plan.counts.clone(),
        to_move,
        switched,
        verified,
        failed_unknown,
        lost: lost.into_values().collect(),
        abandoned,
        complete: !abandoned && movable_left == 0,
        pcs: pcs.iter().map(|(pc, _)| pc.to_string()).collect(),
        last_activity_unix: records
            .iter()
            .map(|r| r.ts_unix)
            .max()
            .unwrap_or(plan.created_unix),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::execute::tests_support::{entry, plan, rec};
    use crate::migration::model::{GroupLoss, RecordState};

    #[test]
    fn aggregates_plan_and_records() {
        let loss = GroupLoss {
            group: 0,
            required_k: 2,
            available: 1,
            missing: vec![],
        };
        let mut lost_plan = entry("L", Action::Lost, 5);
        lost_plan.losses = vec![loss.clone()];
        let p = plan(vec![
            entry("a", Action::Relocate, 1),
            entry("b", Action::Reencode, 2),
            entry("c", Action::Relocate, 3),
            entry("d", Action::Relocate, 4),
            entry("u", Action::Unaffected, 4),
            lost_plan,
        ]);
        let mut run_lost = rec("d", RecordState::Lost, 8, "pc2");
        run_lost.losses = vec![loss];
        let records = vec![
            rec("a", RecordState::Claimed, 1, "pc1"),
            rec("a", RecordState::Unknown, 2, "pc1"),
            rec("a", RecordState::Switched, 5, "pc1"),
            rec("b", RecordState::Verified, 6, "pc2"),
            rec("c", RecordState::Claimed, 3, "pc1"),
            rec("c", RecordState::Unknown, 4, "pc1"),
            rec("L", RecordState::Lost, 7, "pc1"),
            run_lost,
        ];
        let s = summarize(&p, &records);
        assert_eq!(s.to_move, 4);
        assert_eq!(s.switched, 1);
        assert_eq!(s.verified, 1);
        assert_eq!(s.failed_unknown, 1);
        assert_eq!(s.lost.len(), 2);
        let detected: Vec<_> = s
            .lost
            .iter()
            .map(|l| (l.archive_id.as_str(), l.detected.as_str()))
            .collect();
        assert_eq!(detected, vec![("L", "plan"), ("d", "run")]);
        assert!(!s.complete);
        assert_eq!(s.pcs, vec!["pc2".to_string(), "pc1".to_string()]);
        assert_eq!(s.last_activity_unix, 8);
        assert!(!s.abandoned);
    }

    #[test]
    fn complete_when_all_movable_switched_or_lost_and_abandoned_flag() {
        let p = plan(vec![
            entry("a", Action::Relocate, 1),
            entry("d", Action::Relocate, 4),
        ]);
        let mut records = vec![
            rec("a", RecordState::Switched, 5, "pc1"),
            rec("d", RecordState::Lost, 6, "pc1"),
        ];
        let s = summarize(&p, &records);
        assert!(s.complete);
        records.push(rec("", RecordState::Abandoned, 9, "pc1"));
        let s = summarize(&p, &records);
        assert!(s.abandoned && !s.complete);
        let empty = summarize(&p, &[]);
        assert_eq!(empty.last_activity_unix, p.created_unix);
        assert!(empty.pcs.is_empty());
    }
}
