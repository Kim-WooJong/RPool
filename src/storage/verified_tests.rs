//! `verify_unchanged` against a fake rclone (unix only): an object read back in
//! full is not downloaded again while its stat fingerprint is unchanged.
use super::rclone::{ConfigSelection, RcloneContext};
use super::reader::StorageReader;
use crate::prelude::*;
use std::os::unix::fs::PermissionsExt;

const FAKE: &str = r#"#!/bin/sh
dir=$(dirname "$0"); shift 2
case "$1" in
  lsjson)
    size=$(wc -c < "$dir/data" | tr -d ' ')
    if [ -s "$dir/modtime" ]; then
      printf '{"Size":%s,"IsDir":false,"ModTime":"%s"}' "$size" "$(cat "$dir/modtime")"
    else
      printf '{"Size":%s,"IsDir":false}' "$size"
    fi ;;
  cat) echo read >> "$dir/reads"; cat "$dir/data" ;;
  *) exit 2 ;;
esac
"#;

struct Fake {
    dir: tempfile::TempDir,
    shard: Shard,
}
impl Fake {
    fn new(object: &str, modtime: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("rclone");
        fs::write(&exe, FAKE).unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        fs::write(dir.path().join("data"), b"shard-bytes").unwrap();
        fs::write(dir.path().join("modtime"), modtime).unwrap();
        let shard = super::writer::descriptor(
            object,
            11,
            blake3::hash(b"shard-bytes").to_hex().to_string(),
        );
        Self { dir, shard }
    }
    fn reader(&self) -> StorageReader {
        StorageReader::with_rclone_context(RcloneContext::new(
            self.dir.path().join("rclone"),
            ConfigSelection::File(self.dir.path().join("config.json")),
        ))
    }
    fn reads(&self) -> usize {
        fs::read_to_string(self.dir.path().join("reads"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }
    fn set(&self, name: &str, value: &[u8]) {
        fs::write(self.dir.path().join(name), value).unwrap();
    }
}

#[test]
fn unchanged_verified_object_is_not_downloaded_again() {
    let fake = Fake::new("c1:archive/data/00000000.bin", "2026-10-01T00:00:00Z");
    // A fresh reader (as each sync step creates) shares the process memory.
    fake.reader().verify_unchanged(&fake.shard).unwrap();
    assert_eq!(fake.reads(), 1, "first verification reads in full");
    fake.reader().verify_unchanged(&fake.shard).unwrap();
    fake.reader().verify_unchanged(&fake.shard).unwrap();
    assert_eq!(fake.reads(), 1, "unchanged object is only stat-ed");
    // Explicit verification never trusts memory.
    fake.reader().verify(&fake.shard, true).unwrap();
    assert_eq!(fake.reads(), 2);
}

#[test]
fn upload_readback_counts_and_a_changed_object_is_read_again() {
    let fake = Fake::new("c1:archive/data/00000001.bin", "2026-10-01T00:00:00Z");
    // The full readback after an upload (`verify(_, true)`) is remembered.
    fake.reader().verify(&fake.shard, true).unwrap();
    fake.reader().verify_unchanged(&fake.shard).unwrap();
    assert_eq!(fake.reads(), 1);
    // Rewritten on the provider: new modification time, same content.
    fake.set("modtime", b"2026-10-01T00:00:05Z");
    fake.reader().verify_unchanged(&fake.shard).unwrap();
    assert_eq!(fake.reads(), 2);
    // Replaced by different bytes of the same size: detected, and not remembered.
    fake.set("modtime", b"2026-10-01T00:00:09Z");
    fake.set("data", b"other-bytes");
    assert!(fake.reader().verify_unchanged(&fake.shard).is_err());
    assert_eq!(fake.reads(), 3);
    fake.set("data", b"shard-bytes");
    fake.reader().verify_unchanged(&fake.shard).unwrap();
    assert_eq!(
        fake.reads(),
        4,
        "a failed verification leaves no proof behind"
    );
    // A size change fails without downloading.
    fake.set("data", b"shard-bytes!");
    assert!(fake.reader().verify_unchanged(&fake.shard).is_err());
    assert_eq!(fake.reads(), 4);
}

#[test]
fn objects_without_modification_time_are_always_read() {
    let fake = Fake::new("c1:archive/data/00000002.bin", "");
    fake.reader().verify_unchanged(&fake.shard).unwrap();
    fake.reader().verify_unchanged(&fake.shard).unwrap();
    assert_eq!(fake.reads(), 2);
}

#[test]
fn forgotten_objects_are_read_again() {
    let fake = Fake::new("c1:archive/data/00000003.bin", "2026-10-01T00:00:00Z");
    let reader = fake.reader();
    reader.verify_unchanged(&fake.shard).unwrap();
    reader.forget_verified(&fake.shard.object);
    reader.verify_unchanged(&fake.shard).unwrap();
    assert_eq!(fake.reads(), 2);
}
