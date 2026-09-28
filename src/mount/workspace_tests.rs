use super::*;

pub(super) fn fixture() -> (tempfile::TempDir, Workspace) {
    let root = tempfile::tempdir().unwrap();
    let metadata = root.path().join(".rpool");
    let files = root.path().join("files");
    fs::create_dir_all(metadata.join("transactions")).unwrap();
    fs::create_dir_all(metadata.join("archives")).unwrap();
    fs::create_dir(&files).unwrap();
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(metadata.join("workspace.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let catalog = Catalog {
        version: 1,
        pool: "test".into(),
        policy: PoolDefinition {
            remotes: vec!["synthetic:".into()],
            ..Default::default()
        },
        entries: BTreeMap::new(),
        directories: BTreeSet::new(),
        shared: None,
    };
    atomic_json(&metadata.join("catalog.json"), &catalog).unwrap();
    (
        root,
        Workspace {
            rclone: "never-executed".into(),
            files,
            metadata,
            catalog,
            _lock: lock,
        },
    )
}

pub(super) fn fake_upload(path: &Path, id: &str) -> Result<Manifest> {
    let bytes = fs::read(path)?;
    let shards = if bytes.is_empty() {
        vec![]
    } else {
        vec![Shard {
            index: 0,
            offset: 0,
            size: bytes.len() as u64,
            remote: "synthetic:".into(),
            object: "synthetic:object".into(),
            blake3: blake3::hash(&bytes).to_hex().to_string(),
            kind: ShardKind::Data,
            group: 0,
            slot: 0,
        }]
    };
    Ok(Manifest {
        version: 2,
        archive_id: id.into(),
        original_name: path.file_name().unwrap().to_string_lossy().into_owned(),
        original_size: bytes.len() as u64,
        shard_size: 1024,
        created_unix: 0,
        content_root_blake3: crate::manifest::content_root_v2(
            bytes.len() as u64,
            1024,
            &None,
            &shards,
        ),
        coding: None,
        shards,
    })
}

#[test]
fn edits_deletes_empty_files_and_directories_are_durable() {
    let (root, mut workspace) = fixture();
    fs::write(workspace.files.join("hello"), b"abc").unwrap();
    fs::write(workspace.files.join("empty"), []).unwrap();
    fs::create_dir(workspace.files.join("directory")).unwrap();
    assert_eq!(workspace.sync_with(fake_upload, false).unwrap().uploaded, 2);
    assert_eq!(
        workspace
            .sync_with(|_, _| panic!("unchanged file uploaded"), false)
            .unwrap()
            .unchanged,
        2
    );
    fs::write(workspace.files.join("hello"), b"xyz").unwrap();
    assert_eq!(workspace.sync_with(fake_upload, false).unwrap().uploaded, 1);
    fs::remove_file(workspace.files.join("hello")).unwrap();
    assert_eq!(workspace.sync_with(fake_upload, false).unwrap().deleted, 1);
    assert!(workspace.catalog.entries["hello"].deleted);
    assert!(workspace.catalog.directories.contains("directory"));
    assert_eq!(
        fs::read_dir(workspace.metadata.join("archives"))
            .unwrap()
            .count(),
        3
    );
    drop(workspace);
    let reopened = Workspace::open("never-executed", "test", root.path(), vec![]).unwrap();
    assert!(reopened.catalog.entries["hello"].deleted);
    assert_eq!(reopened.catalog.policy.remotes, vec!["synthetic:"]);
}

#[test]
fn failed_upload_reuses_identity_and_never_commits_or_loses_local_content() {
    let (_root, mut workspace) = fixture();
    fs::write(workspace.files.join("id.json"), b"abc").unwrap();
    let mut first_id = String::new();
    assert!(workspace
        .sync_with(
            |_, id| {
                first_id = id.into();
                bail!("offline")
            },
            false
        )
        .is_err());
    assert!(workspace.catalog.entries.is_empty());
    assert_eq!(fs::read(workspace.files.join("id.json")).unwrap(), b"abc");
    workspace
        .sync_with(
            |path, id| {
                assert_eq!(id, first_id);
                fake_upload(path, id)
            },
            false,
        )
        .unwrap();
    assert_eq!(workspace.catalog.entries.len(), 1);
}

#[test]
fn edit_during_upload_is_uploaded_next_pass() {
    let (_root, mut workspace) = fixture();
    let path = workspace.files.join("hello");
    fs::write(&path, b"abc").unwrap();
    workspace
        .sync_with(
            |stage, id| {
                fs::write(&path, b"xyz")?;
                fake_upload(stage, id)
            },
            false,
        )
        .unwrap();
    assert_eq!(
        workspace.catalog.entries["hello"].hash,
        blake3::hash(b"abc").to_hex().to_string()
    );
    assert_eq!(workspace.sync_with(fake_upload, false).unwrap().uploaded, 1);
}

#[test]
fn scan_failure_cannot_tombstone_and_missing_root_is_refused() {
    let (root, mut workspace) = fixture();
    fs::write(workspace.files.join("hello"), b"abc").unwrap();
    workspace.sync_with(fake_upload, false).unwrap();
    fs::remove_dir_all(&workspace.files).unwrap();
    assert!(workspace.sync_with(fake_upload, false).is_err());
    assert!(!workspace.catalog.entries["hello"].deleted);
    drop(workspace);
    assert!(Workspace::open("never-executed", "test", root.path(), vec![]).is_err());
}

#[test]
fn lock_pool_binding_and_unmanaged_paths_are_refused() {
    let (root, workspace) = fixture();
    assert!(Workspace::open("never-executed", "test", root.path(), vec![]).is_err());
    drop(workspace);
    assert!(Workspace::open("never-executed", "other", root.path(), vec![]).is_err());
    let unmanaged = tempfile::tempdir().unwrap();
    fs::write(unmanaged.path().join("important"), b"retain").unwrap();
    assert!(Workspace::open("never-executed", "test", unmanaged.path(), vec![]).is_err());
    assert_eq!(
        fs::read(unmanaged.path().join("important")).unwrap(),
        b"retain"
    );
}

#[test]
fn portable_names_reject_traversal_devices_and_ads() {
    for name in [
        "../escape",
        "a//b",
        "/absolute",
        "a\\b",
        "CON.txt",
        "NUL",
        "a:stream",
        "trailing.",
        "trailing ",
        "COM1",
        "LPT²",
    ] {
        assert!(validate_relative(name).is_err(), "{name}");
    }
    assert!(validate_relative("디렉터리/file.txt").is_ok());
}

#[cfg(unix)]
#[test]
fn links_and_case_collisions_are_refused() {
    let (_root, workspace) = fixture();
    std::os::unix::fs::symlink("/tmp", workspace.files.join("outside")).unwrap();
    assert!(scan(&workspace.files).is_err());
    fs::remove_file(workspace.files.join("outside")).unwrap();
    fs::write(workspace.files.join("Hello"), b"abc").unwrap();
    fs::write(workspace.files.join("hello"), b"abc").unwrap();
    // A case-insensitive host already prevents two names; test only when both exist.
    if fs::read_dir(&workspace.files).unwrap().count() == 2 {
        assert!(scan(&workspace.files).is_err());
    }
}
