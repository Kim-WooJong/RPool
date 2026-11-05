//! Baseline-compatible harness: copy this file and its test module declaration
//! into c01edfb's write.rs; no optimization-only APIs are used here.
//! Run `cargo test --release --bin rpool write_start_benchmark -- --ignored --nocapture`.
use super::*;
use crate::mount::fs_core::{Access, FsCore};
use crate::mount::virtual_tests::content;
use std::time::{Duration, Instant};

fn populated(root: &Path, count: usize) -> VirtualDrive {
    let drive = fixture(root);
    {
        let mut state = drive.state.lock().unwrap();
        for i in 0..count {
            let event = Event {
                version: 1,
                device: "benchmark".into(),
                worker: "benchmark".into(),
                path: format!("d{}/file-{i}", i / 100),
                parents: vec![],
                content: Some(content(b"")),
            };
            state.events.insert(event.id().unwrap(), event);
        }
        state.save(root).unwrap();
    }
    drive
}

#[test]
#[ignore = "synthetic local-disk measurement, no timing pass gate"]
fn repeated_begin_write_start_benchmark() {
    for count in [1000, 10000] {
        let root = tempfile::tempdir().unwrap();
        let drive = populated(root.path(), count);
        let visible = drive.visible_revision("d0/file-0").unwrap().unwrap();
        let warm = drive.begin_intent("d0/file-0", Some(&visible)).unwrap();
        drive.discard_unsealed(&warm).unwrap();
        let before = drive.state.lock().unwrap().generation;
        let mut elapsed = Duration::ZERO;
        let rounds = 50;
        for _ in 0..rounds {
            let start = Instant::now();
            let intent = drive.begin_intent("d0/file-0", Some(&visible)).unwrap();
            elapsed += start.elapsed();
            drive.discard_unsealed(&intent).unwrap();
        }
        let saves = drive.state.lock().unwrap().generation - before;
        eprintln!(
            "begin events={count} rounds={rounds} namespace_saves={saves} mean_us={:.3}",
            elapsed.as_secs_f64() * 1e6 / f64::from(rounds)
        );
    }
}

#[test]
#[ignore = "synthetic local-disk measurement, no timing pass gate"]
fn ide_save_write_start_benchmark() {
    for count in [1000, 10000] {
        let root = tempfile::tempdir().unwrap();
        let mut drive = populated(root.path(), count);
        // Native pool-sync mode records actual reads rather than DAV open pins.
        // The placeholder root is never contacted by these local operations.
        drive.pool_sync_roots = vec!["unused:benchmark".into()];
        {
            let mut state = drive.state.lock().unwrap();
            state.version = 6;
            state.save(root.path()).unwrap();
        }
        let drive = Arc::new(drive);
        let core = FsCore::new(drive.clone()).unwrap();
        let access = Access::Write {
            truncate: true,
            append: false,
        };
        // No cloud byte reads, mounts or rclone: emulate repeated editor saves
        // into one scratch file, followed by a temp-file atomic replacement.
        let before = drive.state.lock().unwrap().generation;
        let start = Instant::now();
        let rounds = 20;
        for round in 0..rounds {
            let handle = core.open("scratch", access, true, false).unwrap();
            core.write_at(handle, 0, format!("save-{round}").as_bytes())
                .unwrap();
            core.fsync(handle).unwrap();
            core.release(handle).unwrap();
        }
        let elapsed = start.elapsed();
        let saves = drive.state.lock().unwrap().generation - before;
        eprintln!(
            "ide-direct events={count} rounds={rounds} namespace_saves={saves} mean_us={:.3}",
            elapsed.as_secs_f64() * 1e6 / f64::from(rounds)
        );
        let before = drive.state.lock().unwrap().generation;
        let start = Instant::now();
        for round in 0..rounds {
            let handle = core.open("scratch.tmp", access, true, false).unwrap();
            core.write_at(handle, 0, format!("atomic-{round}").as_bytes())
                .unwrap();
            core.release(handle).unwrap();
            core.rename("scratch.tmp", "scratch").unwrap();
        }
        let elapsed = start.elapsed();
        let saves = drive.state.lock().unwrap().generation - before;
        eprintln!(
            "ide-atomic events={count} rounds={rounds} namespace_saves={saves} mean_us={:.3}",
            elapsed.as_secs_f64() * 1e6 / f64::from(rounds)
        );
    }
}
