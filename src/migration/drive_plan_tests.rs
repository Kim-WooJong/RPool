use super::*;
use crate::migration::drive_model::{GenerationRef, SourceView};
use crate::migration::drive_test_support::{drive_file, FakeDrive};
use crate::migration::plan::plan_listed;
use crate::migration::test_support::{manifest, policy, FakeCloud};

const MIB: u64 = 1048576;
const ALL: [&str; 4] = ["a:x", "b:x", "c:x", "d:x"];

/// Drive payloads (v6 `virtual-*` or v7 `peer-v7-*`): one unaffected by
/// losing d, one with a shard on d, one with two shards on d (lost).
fn drive(v7: bool) -> (FakeCloud, FakeDrive) {
    let id = |n: char| {
        if v7 {
            format!("peer-v7-{}", n.to_string().repeat(64))
        } else {
            format!("virtual-{}", n.to_string().repeat(64))
        }
    };
    let mut cloud = FakeCloud::default();
    let files = vec![
        manifest(&id('1'), 2 * MIB, MIB, 2, 1, &["a:x", "b:x", "c:x"]),
        manifest(&id('2'), 2 * MIB, MIB, 2, 1, &["b:x", "c:x", "d:x"]),
        manifest(&id('3'), 2 * MIB, MIB, 2, 1, &["d:x", "d:x", "c:x"]),
    ];
    for m in &files {
        // Drive manifests have no replica of their own: they live in events.
        cloud.store(m, &[]);
    }
    cloud.wipe("d:x");
    let names = ["docs/kept.txt", "moved.bin", "gone.bin"];
    let files = files
        .into_iter()
        .zip(names)
        .map(|(m, name)| drive_file(name, &format!("rev-{name}"), m))
        .collect();
    (cloud, FakeDrive::new(v7, files))
}

fn plan_for(cloud: &FakeCloud, source: &dyn DriveSource) -> (Plan, Option<DrivePlan>) {
    let options = PlanOptions {
        download_mib_s: Some(10.0),
        upload_mib_s: Some(10.0),
        ..Default::default()
    };
    let (mut plan, mut listed) = plan_listed(
        cloud,
        "main",
        policy(&["a:x", "b:x", "c:x"], MIB, 2, 1),
        &InventoryStore::default(),
        &options,
        &BTreeSet::new(),
    )
    .unwrap();
    let drive = plan_drive(cloud, source, &mut plan, &mut listed, &options).unwrap();
    (plan, drive)
}

fn entry<'a>(drive: &'a DrivePlan, path: &str) -> &'a DriveEntry {
    drive.entries.iter().find(|e| e.path == path).unwrap()
}

#[test]
fn v6_drive_files_are_classified_like_archives() {
    let (cloud, source) = drive(false);
    let (plan, drive) = plan_for(&cloud, &source);
    let drive = drive.expect("the pool has a drive");
    assert_eq!(drive.migration_id, plan.migration_id);
    assert_eq!(drive.epoch, epoch_for(&plan.migration_id));
    assert_eq!(
        drive.source,
        GenerationRef {
            epoch: None,
            v7: false
        }
    );
    assert_eq!(drive.entries.len(), 3);
    let kept = entry(&drive, "docs/kept.txt");
    assert_eq!(kept.action, Action::Unaffected);
    assert!(kept.detail.as_deref().unwrap().contains("references"));
    assert_eq!(kept.key, entry_key("docs/kept.txt", "rev-docs/kept.txt"));
    let moved = entry(&drive, "moved.bin");
    assert_eq!(moved.action, Action::Relocate);
    assert!(moved.upload_bytes > 0);
    let gone = entry(&drive, "gone.bin");
    assert_eq!(gone.action, Action::Lost);
    assert_eq!(gone.losses[0].available, 1);
    assert_eq!(drive.counts.lost, 1);
    assert_eq!(drive.counts.relocate, 1);
    assert_eq!(drive.counts.unaffected, 1);
    assert!(drive.bootstrap_ok);
    assert_eq!(drive.download_bytes, moved.download_bytes);
    assert!(drive.estimated_seconds.is_some());
    assert!(drive.notes.iter().any(|n| n.contains("accept-lost")));
    assert!(drive.notes.iter().any(|n| n.contains("pending on a PC")));
    // Every remote was listed once for archives and drive together.
    let lists = cloud.lists.lock().unwrap();
    for remote in ALL {
        assert_eq!(lists.iter().filter(|r| *r == remote).count(), 1, "{remote}");
    }
}

#[test]
fn v7_unaffected_files_get_a_private_copy() {
    let (cloud, source) = drive(true);
    let (_, drive) = plan_for(&cloud, &source);
    let drive = drive.unwrap();
    assert!(drive.source.v7);
    let kept = entry(&drive, "docs/kept.txt");
    assert_eq!(kept.action, Action::Relocate);
    assert!(kept.detail.as_deref().unwrap().contains("private"));
    // Copy features unknown: a streamed copy and a readback per shard.
    assert_eq!(kept.upload_bytes, 3 * MIB);
    assert_eq!(drive.counts.unaffected, 0);
    assert_eq!(drive.counts.relocate, 2);
    assert_eq!(drive.history_limit, Some(0));
}

#[test]
fn unreadable_drive_is_left_out_with_a_note() {
    let (cloud, mut source) = drive(false);
    source.fail = true;
    let (plan, drive) = plan_for(&cloud, &source);
    assert!(drive.is_none());
    assert!(plan
        .notes
        .iter()
        .any(|n| n.contains("drive could not be checked")));
}

struct NoDrive;
impl DriveSource for NoDrive {
    fn generations(&self) -> Result<Vec<GenerationRef>> {
        Ok(vec![])
    }
    fn view(&self, _: &GenerationRef) -> Result<SourceView> {
        unreachable!()
    }
}

#[test]
fn pool_without_drive_has_no_drive_part() {
    let (cloud, _) = drive(false);
    let (_, drive) = plan_for(&cloud, &NoDrive);
    assert!(drive.is_none());
}
