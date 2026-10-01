//! Removes a run's test files and folders: `deletefile` per written file,
//! then `rmdir` of the (now empty) run folder and, if nothing else is in it,
//! of `.rpool-speedtest`. Never a purge: only addresses this run created are
//! touched. Runs even after a stop (its context is not stoppable).
use super::engine::Engine;
use super::transfer::TestFile;
use crate::storage::error::StorageErrorKind;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Whether `address` is definitely gone (a failed mutation never says so).
fn gone(engine: &Engine, address: &str) -> bool {
    matches!(
        engine.context.stat_raw(&engine.cleanup(), address),
        Err(error) if error.kind() == StorageErrorKind::NotFound
    )
}

/// Deletes the attempted files with `parallel` workers and removes the run
/// folder. Returns false when something may remain under `run_dir`.
pub(crate) fn remove_run(
    engine: &Engine,
    files: &[TestFile],
    attempted: &[AtomicBool],
    parallel: usize,
    run_dir: &str,
    test_dir: &str,
) -> bool {
    let next = AtomicUsize::new(0);
    let remaining = AtomicBool::new(false);
    std::thread::scope(|scope| {
        for _ in 0..parallel.clamp(1, files.len().max(1)) {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(file) = files.get(index) else {
                    break;
                };
                if !attempted[index].load(Ordering::Acquire) {
                    continue;
                }
                if engine
                    .context
                    .delete_raw(&engine.cleanup(), &file.address)
                    .is_err()
                    && !gone(engine, &file.address)
                {
                    remaining.store(true, Ordering::Release);
                }
            });
        }
    });
    if engine
        .context
        .rmdir_raw(&engine.cleanup(), run_dir)
        .is_err()
        && !gone(engine, run_dir)
    {
        remaining.store(true, Ordering::Release);
    }
    // Shared by other runs (other PCs): removed only when empty, errors ignored.
    let _ = engine.context.rmdir_raw(&engine.cleanup(), test_dir);
    !remaining.load(Ordering::Acquire)
}
