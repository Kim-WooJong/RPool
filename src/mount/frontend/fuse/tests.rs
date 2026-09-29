//! Kernel-level FUSE tests on a fixture workspace. They need /dev/fuse and
//! mount permission, so they are ignored by default; run them in the Linux
//! container: `cargo test --bin rpool frontend::fuse -- --ignored --test-threads=1`.
use super::*;
use crate::mount::virtual_drive::{fixture, fixture_reopen};
use std::io::{Read, Seek, SeekFrom, Write};

struct Mounted {
    session: Option<BackgroundSession>,
    root: tempfile::TempDir,
    mnt: tempfile::TempDir,
}
impl Mounted {
    fn new(read_only: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let mnt = tempfile::tempdir().unwrap();
        let core = Arc::new(FsCore::new(Arc::new(fixture(root.path()))).unwrap());
        let session = session(core, mnt.path(), read_only).unwrap();
        Self {
            session: Some(session),
            root,
            mnt,
        }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.mnt.path().join(name)
    }
    fn unmount(&mut self) {
        if let Some(session) = self.session.take() {
            session.umount_and_join().unwrap();
        }
    }
    /// Unmount, then reopen the workspace as a new process would.
    fn remount(&mut self) {
        self.unmount();
        let core = Arc::new(FsCore::new(Arc::new(fixture_reopen(self.root.path()))).unwrap());
        self.session = Some(session(core, self.mnt.path(), false).unwrap());
    }
}
impl Drop for Mounted {
    fn drop(&mut self) {
        self.unmount();
    }
}
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7919 % 251) as u8).collect()
}

#[test]
#[ignore = "requires /dev/fuse"]
fn files_round_trip_through_the_kernel_and_survive_remount() {
    let mut m = Mounted::new(false);
    fs::write(m.path("small.txt"), b"hello fuse").unwrap();
    let big = pattern(3 * 1024 * 1024 + 17);
    fs::write(m.path("big.bin"), &big).unwrap();
    assert_eq!(fs::read(m.path("small.txt")).unwrap(), b"hello fuse");
    assert_eq!(fs::read(m.path("big.bin")).unwrap(), big);
    assert_eq!(
        fs::metadata(m.path("big.bin")).unwrap().len(),
        big.len() as u64
    );
    fs::create_dir(m.path("dir")).unwrap();
    fs::write(m.path("dir/inner"), b"inner").unwrap();
    let mut names: Vec<_> = fs::read_dir(m.mnt.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["big.bin", "dir", "small.txt"]);
    m.remount();
    assert_eq!(fs::read(m.path("small.txt")).unwrap(), b"hello fuse");
    assert_eq!(fs::read(m.path("big.bin")).unwrap(), big);
    assert_eq!(fs::read(m.path("dir/inner")).unwrap(), b"inner");
}

#[test]
#[ignore = "requires /dev/fuse"]
fn edits_truncate_append_rename_and_delete_behave_like_posix() {
    let m = Mounted::new(false);
    fs::write(m.path("f"), b"0123456789").unwrap();
    let mut f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(m.path("f"))
        .unwrap();
    f.seek(SeekFrom::Start(2)).unwrap();
    f.write_all(b"AB").unwrap();
    f.sync_all().unwrap();
    f.set_len(6).unwrap();
    drop(f);
    assert_eq!(fs::read(m.path("f")).unwrap(), b"01AB45");
    let mut a = fs::OpenOptions::new()
        .append(true)
        .open(m.path("f"))
        .unwrap();
    a.write_all(b"!").unwrap();
    drop(a);
    assert_eq!(fs::read(m.path("f")).unwrap(), b"01AB45!");
    fs::write(m.path("g"), b"old g").unwrap();
    fs::rename(m.path("f"), m.path("g")).unwrap();
    assert_eq!(fs::read(m.path("g")).unwrap(), b"01AB45!");
    assert!(!m.path("f").exists());
    fs::create_dir_all(m.path("x/y")).unwrap();
    fs::write(m.path("x/y/z"), b"z").unwrap();
    fs::rename(m.path("x"), m.path("w")).unwrap();
    assert_eq!(fs::read(m.path("w/y/z")).unwrap(), b"z");
    assert_eq!(
        fs::remove_dir(m.path("w/y")).unwrap_err().raw_os_error(),
        Some(libc::ENOTEMPTY)
    );
    fs::remove_file(m.path("w/y/z")).unwrap();
    fs::remove_dir(m.path("w/y")).unwrap();
    fs::remove_file(m.path("g")).unwrap();
    assert!(!m.path("g").exists());
    let excl = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(m.path("w"));
    assert_eq!(excl.unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
}

#[test]
#[ignore = "requires /dev/fuse"]
fn an_open_file_keeps_its_bytes_after_overwrite_and_unlink() {
    let m = Mounted::new(false);
    fs::write(m.path("doc"), b"original bytes").unwrap();
    let mut reader = fs::File::open(m.path("doc")).unwrap();
    fs::write(m.path("doc.tmp"), b"replacement").unwrap();
    fs::rename(m.path("doc.tmp"), m.path("doc")).unwrap();
    let mut seen = Vec::new();
    reader.read_to_end(&mut seen).unwrap();
    assert_eq!(seen, b"original bytes");
    assert_eq!(fs::read(m.path("doc")).unwrap(), b"replacement");
    fs::remove_file(m.path("doc")).unwrap();
    reader.seek(SeekFrom::Start(9)).unwrap();
    let mut tail = String::new();
    reader.read_to_string(&mut tail).unwrap();
    assert_eq!(tail, "bytes");
}

#[test]
#[ignore = "requires /dev/fuse"]
fn concurrent_writers_to_different_files() {
    let m = Mounted::new(false);
    std::thread::scope(|scope| {
        for i in 0..8 {
            let path = m.path(&format!("file-{i}"));
            scope.spawn(move || {
                let bytes = pattern(256 * 1024 + i);
                fs::write(&path, &bytes).unwrap();
                assert_eq!(fs::read(&path).unwrap(), bytes);
            });
        }
    });
    assert_eq!(fs::read_dir(m.mnt.path()).unwrap().count(), 8);
}

#[test]
#[ignore = "requires /dev/fuse"]
fn read_only_mount_refuses_changes() {
    let mut m = Mounted::new(false);
    fs::write(m.path("kept"), b"kept").unwrap();
    m.unmount();
    let core = Arc::new(FsCore::new(Arc::new(fixture_reopen(m.root.path()))).unwrap());
    m.session = Some(session(core, m.mnt.path(), true).unwrap());
    assert_eq!(fs::read(m.path("kept")).unwrap(), b"kept");
    let error = fs::write(m.path("new"), b"x").unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EROFS));
    assert_eq!(
        fs::remove_file(m.path("kept")).unwrap_err().raw_os_error(),
        Some(libc::EROFS)
    );
}
