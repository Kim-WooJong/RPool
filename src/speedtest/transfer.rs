//! Parallel upload / read-back of one remote's test files.
//!
//! Uploads use the same raw primitives as `put`: the gated rclone crypt
//! write (`rclone rcat` after the crypt check), or for `--native-crypt` pools
//! RPool's own encryption onto the crypt base. Reads always go through rclone
//! crypt via `read_raw`, which uses the shared read daemon when available,
//! like real RPool reads. Data is streamed (see `stream`), never buffered.
use super::engine::{one_line, Engine};
use super::progress::{Position, Transfer};
use super::stream::{TestSource, VerifySink};
use crate::storage::traits::WriteOptions;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One file of the test.
pub(crate) struct TestFile {
    pub address: String,
    pub size: u64,
}

/// Outcome of one phase over all files.
pub(crate) struct Phase<T> {
    pub wall: Duration,
    /// Per file: None = not started (an earlier file failed or a stop).
    pub results: Vec<Option<Result<T, String>>>,
}

impl<T> Phase<T> {
    /// First error, if any file failed.
    pub(crate) fn error(&self) -> Option<&str> {
        self.results.iter().find_map(|r| match r {
            Some(Err(e)) => Some(e.as_str()),
            _ => None,
        })
    }
    pub(crate) fn complete(&self) -> bool {
        self.results.iter().all(|r| matches!(r, Some(Ok(_))))
    }
}

/// Runs `step` for every file with `parallel` workers. After the first
/// failure (or a stop) no further file is started.
fn run<T: Send>(
    engine: &Engine,
    at: &Position<'_>,
    verb: &str,
    files: &[TestFile],
    parallel: usize,
    progress: &Transfer,
    step: impl Fn(usize, &TestFile) -> Result<T, String> + Sync,
) -> Phase<T> {
    let total: u64 = files.iter().map(|f| f.size).sum();
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let running = AtomicUsize::new(parallel.clamp(1, files.len().max(1)));
    let results: Mutex<Vec<Option<Result<T, String>>>> =
        Mutex::new((0..files.len()).map(|_| None).collect());
    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..running.load(Ordering::Relaxed) {
            scope.spawn(|| {
                loop {
                    if failed.load(Ordering::Acquire) || engine.cancelled() || engine.draining() {
                        break;
                    }
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(file) = files.get(index) else {
                        break;
                    };
                    let outcome = step(index, file);
                    if outcome.is_err() {
                        failed.store(true, Ordering::Release);
                    } else {
                        progress.files_done.fetch_add(1, Ordering::Relaxed);
                    }
                    if let Ok(mut results) = results.lock() {
                        results[index] = Some(outcome);
                    }
                }
                running.fetch_sub(1, Ordering::AcqRel);
            });
        }
        progress.watch(at, verb, files.len(), total, || {
            running.load(Ordering::Acquire) == 0
        });
    });
    let wall = started.elapsed();
    Phase {
        wall,
        results: results.into_inner().unwrap_or_else(|e| e.into_inner()),
    }
}

fn failure(engine: &Engine, what: &str, error: impl std::fmt::Display) -> String {
    if engine.cancelled() {
        return "cancelled".into();
    }
    one_line(&format!("{what}: {error}"))
}

/// Writes every file; each result is the digest of the bytes sent.
/// `attempted[i]` is set before file `i` is written (it may exist after).
pub(crate) fn upload(
    engine: &Engine,
    at: &Position<'_>,
    files: &[TestFile],
    parallel: usize,
    seed: &[u8; 32],
    attempted: &[AtomicBool],
) -> (Phase<blake3::Hash>, Transfer) {
    let progress = Transfer::new();
    let phase = upload_into(engine, at, files, parallel, seed, attempted, &progress);
    (phase, progress)
}

/// [`upload`] counting into `progress`, which the caller may watch.
pub(crate) fn upload_into(
    engine: &Engine,
    at: &Position<'_>,
    files: &[TestFile],
    parallel: usize,
    seed: &[u8; 32],
    attempted: &[AtomicBool],
    progress: &Transfer,
) -> Phase<blake3::Hash> {
    run(
        engine,
        at,
        "uploading",
        files,
        parallel,
        progress,
        |i, file| {
            attempted[i].store(true, Ordering::Release);
            let mut source =
                TestSource::new(seed, i as u64, file.size, &progress.moved, &engine.cancel);
            let ctx = engine.transfer(file.size);
            let options = WriteOptions::default();
            let written = match &engine.native {
                Some(native) => match native.route(&ctx, &file.address) {
                    Ok(Some((backend, key))) => backend.write(&ctx, &key, &mut source, &options),
                    Ok(None) => {
                        engine
                            .context
                            .write_raw(&ctx, &file.address, &mut source, None, &options)
                    }
                    Err(error) => Err(error),
                },
                None => engine
                    .context
                    .write_raw(&ctx, &file.address, &mut source, None, &options),
            };
            match written {
                Ok(receipt) if receipt.size == file.size && source.remaining() == 0 => {
                    Ok(source.digest())
                }
                Ok(receipt) => Err(failure(
                    engine,
                    "upload",
                    format_args!("wrote {} of {} bytes", receipt.size, file.size),
                )),
                Err(error) => Err(failure(engine, "upload", error)),
            }
        },
    )
}

/// Reads every file back and compares it with its upload digest.
pub(crate) fn download(
    engine: &Engine,
    at: &Position<'_>,
    files: &[TestFile],
    parallel: usize,
    digests: &[blake3::Hash],
) -> (Phase<()>, Transfer) {
    let progress = Transfer::new();
    let phase = download_into(engine, at, files, parallel, digests, &progress);
    (phase, progress)
}

/// [`download`] counting into `progress`, which the caller may watch.
pub(crate) fn download_into(
    engine: &Engine,
    at: &Position<'_>,
    files: &[TestFile],
    parallel: usize,
    digests: &[blake3::Hash],
    progress: &Transfer,
) -> Phase<()> {
    run(
        engine,
        at,
        "downloading",
        files,
        parallel,
        progress,
        |i, file| {
            let mut sink = VerifySink::new(file.size, &progress.moved, &engine.cancel);
            let ctx = engine.transfer(file.size);
            match engine
                .context
                .read_raw(&ctx, &file.address, None, &mut sink)
            {
                Ok(_) if sink.matches(&digests[i]) => Ok(()),
                Ok(_) if sink.count() != file.size => Err(failure(
                    engine,
                    "download",
                    format_args!("read {} of {} bytes", sink.count(), file.size),
                )),
                Ok(_) => Err(failure(
                    engine,
                    "verify",
                    "read-back data differs from what was written",
                )),
                Err(error) => Err(failure(engine, "download", error)),
            }
        },
    )
}
