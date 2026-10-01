//! Portable durable-file primitives.
//!
//! Windows `FlushFileBuffers` (what `File::sync_all` calls) needs a handle with
//! write access: `File::open(path)?.sync_all()` fails there with
//! ERROR_ACCESS_DENIED (os error 5), while Unix `fsync` accepts a read-only
//! descriptor. Windows renames can also fail briefly while an antivirus or
//! indexer holds the destination open without FILE_SHARE_DELETE.
use crate::prelude::*;
use std::io;
use std::time::Duration;

/// Backoff for transient Windows sharing failures; about two seconds in total.
pub(crate) const TRANSIENT_RETRY_DELAYS: [Duration; 8] = [
    Duration::from_millis(10),
    Duration::from_millis(20),
    Duration::from_millis(40),
    Duration::from_millis(80),
    Duration::from_millis(160),
    Duration::from_millis(320),
    Duration::from_millis(640),
    Duration::from_millis(730),
];

/// Opens an existing regular file so that `sync_all` works on this OS. Never
/// creates or truncates. Unix: read-only, as before. Windows: write access,
/// which std opens with FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
/// so our own open spool handles do not conflict with it.
pub(crate) fn open_for_sync(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        OpenOptions::new().write(true).open(path)
    }
    #[cfg(not(windows))]
    {
        File::open(path)
    }
}

/// fsync an existing regular file by path.
pub(crate) fn sync_file(path: &Path) -> Result<()> {
    open_for_sync(path)
        .and_then(|file| file.sync_all())
        .with_context(|| format!("flush {}", path.display()))
}

/// A sharing failure another process (antivirus, indexer, backup) usually
/// clears within milliseconds. Always false off Windows.
pub(crate) fn is_transient_sharing_error(error: &io::Error) -> bool {
    // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION.
    cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

/// Runs `op` until it succeeds, fails non-transiently, or `delays` run out.
/// Only for idempotent steps whose failure leaves no partial effect, such as an
/// atomic rename.
pub(crate) fn retry_transient<T>(
    delays: &[Duration],
    mut op: impl FnMut() -> io::Result<T>,
    transient: impl Fn(&io::Error) -> bool,
) -> io::Result<T> {
    for delay in delays {
        match op() {
            Err(error) if transient(&error) => std::thread::sleep(*delay),
            result => return result,
        }
    }
    op()
}

/// Atomically replaces `path` with `temp`, retrying transient sharing failures.
/// A failed persist keeps the temporary file and leaves `path` unchanged, so a
/// retry is safe.
pub(crate) fn persist_replacing(temp: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    let mut temp = Some(temp);
    retry_transient(
        &TRANSIENT_RETRY_DELAYS,
        || {
            let file = temp.take().ok_or_else(|| io::Error::other("persisted"))?;
            file.persist(path).map(drop).map_err(|error| {
                temp = Some(error.file);
                error.error
            })
        },
        is_transient_sharing_error,
    )
    .with_context(|| format!("replace {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn denied() -> io::Error {
        io::Error::from(io::ErrorKind::PermissionDenied)
    }

    #[test]
    fn retry_transient_retries_only_transient_failures() {
        let mut calls = 0;
        let result = retry_transient(
            &[Duration::ZERO; 3],
            || {
                calls += 1;
                if calls < 3 {
                    Err(denied())
                } else {
                    Ok(calls)
                }
            },
            |_| true,
        );
        assert_eq!(result.unwrap(), 3);

        let mut calls = 0;
        let result: io::Result<()> = retry_transient(
            &[Duration::ZERO; 3],
            || {
                calls += 1;
                Err(denied())
            },
            |_| false,
        );
        assert!(result.is_err());
        assert_eq!(calls, 1, "a permanent failure is never retried");

        let mut calls = 0;
        let result: io::Result<()> = retry_transient(
            &[Duration::ZERO; 3],
            || {
                calls += 1;
                Err(denied())
            },
            |_| true,
        );
        assert!(result.is_err());
        assert_eq!(calls, 4, "bounded: one try per delay plus a last one");
    }

    #[test]
    fn transient_classification_is_windows_only() {
        let error = io::Error::from_raw_os_error(32);
        assert_eq!(is_transient_sharing_error(&error), cfg!(windows));
        assert!(!is_transient_sharing_error(&io::Error::other("x")));
    }

    #[test]
    fn sync_file_works_while_another_writer_holds_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("content");
        let mut writer = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        writer.write_all(b"spool").unwrap();
        sync_file(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"spool", "never truncates");
    }

    #[test]
    fn persist_replacing_replaces_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(&path, b"old").unwrap();
        let mut temp = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        temp.write_all(b"new").unwrap();
        persist_replacing(temp, &path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }

    /// The observed failure: Windows refuses to flush a read-only handle.
    #[cfg(windows)]
    #[test]
    fn windows_read_only_handle_cannot_flush() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("content");
        fs::write(&path, b"x").unwrap();
        let error = File::open(&path).unwrap().sync_all().unwrap_err();
        assert_eq!(error.raw_os_error(), Some(5));
        open_for_sync(&path).unwrap().sync_all().unwrap();
    }
}
