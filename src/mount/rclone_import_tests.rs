use super::*;
use crate::mount::virtual_drive::{fixture, fixture_reopen};
use std::cell::{Cell, RefCell};

#[derive(Default)]
struct Memory {
    files: BTreeMap<String, Vec<u8>>,
    dirs: Vec<String>,
    broken: RefCell<BTreeSet<String>>,
}
impl Source for Memory {
    fn list(&self) -> Result<Vec<Listed>> {
        let mut out: Vec<Listed> = self
            .dirs
            .iter()
            .map(|d| Listed {
                path: d.clone(),
                size: -1,
                is_dir: true,
                mod_time: String::new(),
            })
            .collect();
        out.extend(self.files.iter().map(|(p, b)| Listed {
            path: p.clone(),
            size: b.len() as i64,
            is_dir: false,
            mod_time: "2026-09-30T00:00:00Z".into(),
        }));
        Ok(out)
    }
    fn read(&self, rel: &str, sink: &mut dyn Write) -> Result<u64> {
        let bytes = &self.files[rel];
        if self.broken.borrow().contains(rel) {
            sink.write_all(&bytes[..bytes.len() / 2])?;
            bail!("connection reset");
        }
        sink.write_all(bytes)?;
        Ok(bytes.len() as u64)
    }
}

fn source(files: &[(&str, &[u8])]) -> Memory {
    Memory {
        files: files
            .iter()
            .map(|(p, b)| ((*p).into(), b.to_vec()))
            .collect(),
        ..Default::default()
    }
}
fn options(destination: &str, on_conflict: OnConflict) -> Options {
    Options {
        source: "old:photos".into(),
        destination: destination.into(),
        batch_bytes: 1 << 40,
        on_conflict,
    }
}
fn run(drive: &VirtualDrive, source: &Memory, options: &Options) -> (Status, usize) {
    let syncs = Cell::new(0);
    let status = import(
        drive,
        source,
        options,
        &|| false,
        &|| {
            syncs.set(syncs.get() + 1);
            Ok(())
        },
        &mut |_| {},
    )
    .unwrap();
    (status, syncs.get())
}
fn contents(drive: &VirtualDrive, path: &str) -> Vec<u8> {
    let revision = drive.view().unwrap()[path].clone();
    drive.read(&revision, 0, 1 << 20).unwrap()
}

#[test]
fn imports_the_tree_into_a_folder_and_a_rerun_adds_nothing() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    let mut src = source(&[("a.txt", b"alpha"), ("sub/b.bin", b"beta"), ("empty", b"")]);
    src.dirs = vec!["sub".into(), "sub/empty-dir".into()];
    let opts = options("Imported/old", OnConflict::Skip);
    let (status, syncs) = run(&drive, &src, &opts);
    assert_eq!(
        (status.imported, status.files_total, status.phase.as_str()),
        (3, 3, "done")
    );
    assert_eq!(syncs, 1, "one final upload");
    assert_eq!(contents(&drive, "Imported/old/a.txt"), b"alpha");
    assert_eq!(contents(&drive, "Imported/old/sub/b.bin"), b"beta");
    assert_eq!(contents(&drive, "Imported/old/empty"), b"");
    assert!(drive
        .state
        .lock()
        .unwrap()
        .directories
        .contains("Imported/old/sub/empty-dir"));
    let pending = drive.state.lock().unwrap().pending.len();
    let (again, _) = run(&drive, &src, &opts);
    assert_eq!(again.imported, 3, "reported from the journal");
    assert_eq!(
        drive.state.lock().unwrap().pending.len(),
        pending,
        "no duplicates"
    );
}

#[test]
fn existing_files_are_skipped_or_imported_next_to_them() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    let intent = drive.begin("doc.txt").unwrap();
    fs::write(drive.spool_path(&intent), b"mine").unwrap();
    drive.seal(intent).unwrap();
    let src = source(&[("doc.txt", b"theirs"), ("CON", b"reserved")]);
    let (skip, _) = run(&drive, &src, &options("", OnConflict::Skip));
    let reasons: Vec<&str> = skip.skipped.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(reasons, ["CON", "doc.txt"]);
    assert_eq!(contents(&drive, "doc.txt"), b"mine");
    let (renamed, _) = run(
        &drive,
        &src,
        &Options {
            source: "other:".into(),
            ..options("", OnConflict::Rename)
        },
    );
    assert_eq!(renamed.imported, 1);
    assert_eq!(contents(&drive, "doc (imported 1).txt"), b"theirs");
    assert_eq!(contents(&drive, "doc.txt"), b"mine");
}

#[test]
fn batches_upload_as_they_go() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    let src = source(&[("1", &[1; 60]), ("2", &[2; 60]), ("3", &[3; 60])]);
    let (_, syncs) = run(
        &drive,
        &src,
        &Options {
            batch_bytes: 100,
            ..options("", OnConflict::Skip)
        },
    );
    assert_eq!(syncs, 2, "after the second file, then the final upload");
}

#[test]
fn a_failed_read_leaves_no_write_and_is_retried() {
    let root = tempfile::tempdir().unwrap();
    let drive = fixture(root.path());
    let src = source(&[("big.bin", &[7; 4096]), ("ok.txt", b"fine")]);
    src.broken.borrow_mut().insert("big.bin".into());
    let opts = options("", OnConflict::Skip);
    let (status, _) = run(&drive, &src, &opts);
    assert_eq!(status.failed.len(), 1);
    assert!(!drive.view().unwrap().contains_key("big.bin"));
    assert_eq!(drive.state.lock().unwrap().pending.len(), 1);
    src.broken.borrow_mut().clear();
    let (retry, _) = run(&drive, &src, &opts);
    assert!(retry.failed.is_empty());
    assert_eq!(contents(&drive, "big.bin"), vec![7; 4096]);
}

#[test]
fn a_crash_after_sealing_adopts_the_write_instead_of_copying_again() {
    let root = tempfile::tempdir().unwrap();
    let opts = options("in", OnConflict::Rename);
    let src = source(&[("x.txt", b"once")]);
    {
        let drive = fixture(root.path());
        run(&drive, &src, &opts);
    }
    // Drop the final "sealed" record, as a crash right after `seal` would.
    let path = journal_path(root.path(), &opts);
    let text = fs::read_to_string(&path).unwrap();
    let kept: Vec<&str> = text.lines().filter(|l| !l.contains("\"sealed\"")).collect();
    fs::write(&path, kept.join("\n") + "\n").unwrap();
    let drive = fixture_reopen(root.path());
    let (status, _) = run(&drive, &src, &opts);
    assert_eq!(status.imported, 1);
    assert_eq!(
        drive.state.lock().unwrap().pending.len(),
        1,
        "no second copy"
    );
    assert!(!drive.view().unwrap().contains_key("in/x (imported 1).txt"));
}

#[test]
fn imported_names_keep_folder_and_extension() {
    assert_eq!(imported_name("a/b.txt", 2), "a/b (imported 2).txt");
    assert_eq!(imported_name("Makefile", 1), "Makefile (imported 1)");
}
