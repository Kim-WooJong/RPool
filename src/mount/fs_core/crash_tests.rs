//! Crash matrix: every crash point × every acknowledging operation. After the
//! injected failure the core and drive are dropped (no release) and the
//! workspace is reopened from disk.
use super::*;
use crate::mount::crash;
use crate::mount::virtual_drive::{fixture, fixture_reopen, VirtualDrive};
use crate::prelude::*;

const POINTS: &[&str] = &[
    "spool.partial_write",
    "seal.before_fsync",
    "seal.after_intent_record",
    "seal.before_namespace_save",
    "seal.after_namespace_save",
    "namespace.after_previous",
    "durable.before_persist",
    "namespace.journal_torn",
    "namespace.after_journal_append",
    "rename.before_namespace_save",
    "delete.before_namespace_save",
];
const TRUNC: Access = Access::Write {
    truncate: true,
    append: false,
};

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Overwrite,
    Create,
    Rename,
    Delete,
}

fn put(core: &FsCore, path: &str, bytes: &[u8]) -> FsResult<()> {
    let handle = core.open(path, TRUNC, true, false)?;
    let written = core.write_at(handle, 0, bytes).map(|_| ());
    let released = core.release(handle);
    written.and(released)
}
fn read(core: &FsCore, path: &str) -> Option<Vec<u8>> {
    let handle = core.open(path, Access::Read, false, false).ok()?;
    let bytes = core.read_at(handle, 0, 1 << 20).unwrap();
    core.release(handle).unwrap();
    Some(bytes)
}
fn assert_sealed_images_intact(drive: &VirtualDrive) {
    for intent in drive.state.lock().unwrap().pending.iter() {
        if intent.spool.is_some() {
            let path = drive.spool_path(intent);
            assert_eq!(
                crate::utils::hash_file_range(&path, 0, intent.size).unwrap(),
                intent.hash
            );
        }
    }
}

/// Like a process that dies at its first failure: stop issuing operations and
/// leave handles open. `Ok` only when `fsync` acknowledged the bytes.
fn save(core: &FsCore, path: &str, bytes: &[u8]) -> FsResult<()> {
    let handle = core.open(path, TRUNC, true, false)?;
    core.write_at(handle, 0, bytes)?;
    core.fsync(handle)?;
    core.release(handle)
}

/// Runs the scenario with `point` armed; returns whether it was acknowledged.
fn run(core: &FsCore, scenario: Scenario, point: &'static str) -> bool {
    crash::arm(point);
    let result = match scenario {
        Scenario::Overwrite => save(core, "a", b"version two"),
        Scenario::Create => save(core, "new", b"created"),
        Scenario::Rename => core.rename("a", "moved"),
        Scenario::Delete => core.delete("a"),
    };
    crash::disarm();
    result.is_ok()
}

fn check(core: &FsCore, scenario: Scenario, acknowledged: bool, label: &str) {
    let a = read(core, "a");
    let old = Some(b"version one".to_vec());
    match scenario {
        Scenario::Overwrite => {
            let new = Some(b"version two".to_vec());
            assert!(a == new || (!acknowledged && a == old), "{label}: {a:?}");
        }
        Scenario::Create => {
            assert_eq!(a, old, "{label}");
            let created = read(core, "new");
            assert!(
                created.as_deref() == Some(b"created") || (!acknowledged && created.is_none()),
                "{label}: {created:?}"
            );
        }
        Scenario::Rename => {
            let moved = read(core, "moved");
            let done = a.is_none() && moved == old;
            let undone = a == old && moved.is_none();
            assert!(
                done || (!acknowledged && undone),
                "{label}: {a:?} {moved:?}"
            );
        }
        Scenario::Delete => {
            assert!(a.is_none() || (!acknowledged && a == old), "{label}: {a:?}");
        }
    }
}

/// A local or pool-sync (v6) fixture core, fresh or reopened from disk.
fn open_core(root: &Path, reopen: bool, peer: bool) -> FsCore {
    let mut drive = if reopen {
        fixture_reopen(root)
    } else {
        fixture(root)
    };
    if peer {
        drive.pool_sync_roots = vec!["crypt:pool".into()];
        drive.state.lock().unwrap().version = 6;
    }
    FsCore::new(Arc::new(drive)).unwrap()
}

#[test]
fn every_crash_point_preserves_acknowledged_data_and_never_mixes_revisions() {
    let scenarios = [
        Scenario::Overwrite,
        Scenario::Create,
        Scenario::Rename,
        Scenario::Delete,
    ];
    let mut injected = 0;
    for peer in [false, true] {
        for &point in POINTS {
            for scenario in scenarios {
                let label = format!("{point} × {scenario:?} (pool sync: {peer})");
                let root = tempfile::tempdir().unwrap();
                let core = open_core(root.path(), false, peer);
                put(&core, "a", b"version one").unwrap();
                let acknowledged = run(&core, scenario, point);
                injected += usize::from(!acknowledged);
                if acknowledged {
                    check(&core, scenario, true, &format!("{label} (live)"));
                }
                drop(core);
                let core = open_core(root.path(), true, peer);
                check(&core, scenario, acknowledged, &label);
                assert_sealed_images_intact(&core.drive);
                // The workspace keeps working after recovery.
                put(&core, "after", b"recovered").unwrap();
                assert_eq!(
                    read(&core, "after").as_deref(),
                    Some(&b"recovered"[..]),
                    "{label}"
                );
                assert_sealed_images_intact(&core.drive);
            }
        }
    }
    // Each point fires in at least one scenario; unreachable pairs pass through.
    assert!(
        injected >= POINTS.len(),
        "only {injected} injected failures"
    );
}
