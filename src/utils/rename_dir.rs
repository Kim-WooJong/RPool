//! Renaming a workspace directory as a whole (drive workspace switches and
//! pool transitions).
//!
//! On Windows a directory cannot be renamed while any program has a file in
//! it open: the rename fails with "The process cannot access the file because
//! it is being used by another process" (os error 32) or access denied
//! (os error 5). Virus scanners, the search indexer and Explorer previews
//! hold files briefly, so the rename is retried for a while; if it still
//! fails, the error says what usually holds the folder and how to find it.

/// How long a busy folder is retried.
const RETRY_FOR: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether `error` means another program holds a file in the folder.
fn busy(error: &std::io::Error) -> bool {
    cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32))
}

/// Renames directory `from` to `to`, retrying while another program holds a
/// file in it (Windows); other errors return at once.
pub(crate) fn rename_dir(from: &std::path::Path, to: &std::path::Path) -> anyhow::Result<()> {
    rename_dir_with(from, to, RETRY_FOR, &|a, b| std::fs::rename(a, b))
}

/// [`rename_dir`] with an injectable rename and retry period (tests).
fn rename_dir_with(
    from: &std::path::Path,
    to: &std::path::Path,
    retry_for: std::time::Duration,
    rename: &dyn Fn(&std::path::Path, &std::path::Path) -> std::io::Result<()>,
) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    let mut delay = std::time::Duration::from_millis(250);
    loop {
        match rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if busy(&error) && started.elapsed() < retry_for => {
                std::thread::sleep(delay);
                delay = (delay * 2).min(std::time::Duration::from_secs(5));
            }
            Err(error) if busy(&error) => {
                return Err(anyhow::anyhow!(
                    "{} cannot be renamed: another program still has a file in it open ({error}). \
                     Stop every mount of this drive (also in other RPool windows), close Explorer \
                     windows and editors showing files from it, end leftover rpool.exe/rclone.exe \
                     processes in Task Manager, then retry. To see which program it is: Resource \
                     Monitor > CPU > Associated Handles, search for the folder name. Nothing was \
                     changed.",
                    from.display()
                ))
            }
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::path::Path;

    #[test]
    fn renames_and_passes_other_errors_through() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::create_dir(&a).unwrap();
        rename_dir(&a, &b).unwrap();
        assert!(b.is_dir() && !a.exists());
        let error = rename_dir(&a, &dir.path().join("c")).unwrap_err();
        assert!(error.downcast_ref::<std::io::Error>().is_some());
    }

    #[cfg(windows)]
    #[test]
    fn a_briefly_busy_folder_is_retried_and_a_held_one_explained() {
        let calls = Cell::new(0);
        let busy_twice = |_: &Path, _: &Path| {
            calls.set(calls.get() + 1);
            if calls.get() <= 2 {
                Err(std::io::Error::from_raw_os_error(32))
            } else {
                Ok(())
            }
        };
        let long = std::time::Duration::from_secs(30);
        rename_dir_with(Path::new("a"), Path::new("b"), long, &busy_twice).unwrap();
        assert_eq!(calls.get(), 3);
        let held = |_: &Path, _: &Path| Err(std::io::Error::from_raw_os_error(32));
        let error = rename_dir_with(
            Path::new("a"),
            Path::new("b"),
            std::time::Duration::ZERO,
            &held,
        )
        .unwrap_err();
        assert!(format!("{error}").contains("Resource Monitor"), "{error}");
    }

    #[cfg(not(windows))]
    #[test]
    fn errors_other_than_windows_sharing_are_not_retried() {
        let calls = Cell::new(0);
        let failing = |_: &Path, _: &Path| {
            calls.set(calls.get() + 1);
            Err(std::io::Error::from_raw_os_error(32))
        };
        let long = std::time::Duration::from_secs(30);
        assert!(rename_dir_with(Path::new("a"), Path::new("b"), long, &failing).is_err());
        assert_eq!(calls.get(), 1);
    }
}
