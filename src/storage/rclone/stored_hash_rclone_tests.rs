//! Upload verification by provider hash against the installed rclone and a
//! local base remote (local reports md5/sha1, like many providers).
use super::*;

fn context(temp: &tempfile::TempDir) -> RcloneContext {
    let data = temp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let conf = temp.path().join("rclone.conf");
    std::fs::write(&conf, "[hash_base]\ntype = local\n").unwrap();
    let rclone: PathBuf = std::env::var_os("RPOOL_TEST_RCLONE")
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "/opt/homebrew/bin/rclone".into()
            } else {
                "rclone".into()
            }
        })
        .into();
    let mut context = RcloneContext::new(rclone, ConfigSelection::File(conf));
    context.environment.retain(|(k, _)| {
        let k = k.to_string_lossy().to_ascii_uppercase();
        !k.starts_with("RCLONE_") && !k.starts_with("RPOOL_RCLONE")
    });
    context
}

#[test]
#[ignore = "requires rclone"]
fn an_upload_the_provider_hashes_identically_needs_no_readback() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(&temp);
    let address = format!("hash_base:{}/data/obj.bin", temp.path().display());
    let bytes: Vec<u8> = (0..3_000_000u32).map(|i| (i * 31 % 251) as u8).collect();
    let receipt = context
        .write_ungated(
            &OperationContext::none(),
            &address,
            &mut std::io::Cursor::new(&bytes),
            Some(bytes.len() as u64),
            &WriteOptions::default(),
        )
        .unwrap();
    assert_eq!(receipt.size, bytes.len() as u64);
    assert!(
        receipt.hash_verified,
        "local reports sha1/md5 of the stored bytes"
    );
    assert_eq!(
        std::fs::read(temp.path().join("data/obj.bin")).unwrap(),
        bytes
    );
}

#[test]
#[ignore = "requires rclone"]
fn one_listing_proves_a_whole_archive_and_catches_a_bad_object() {
    let temp = tempfile::tempdir().unwrap();
    let context = context(&temp);
    let root = format!("hash_base:{}/data/arch", temp.path().display());
    let options = WriteOptions {
        defer_hash_check: true,
        ..WriteOptions::default()
    };
    let mut expected = Vec::new();
    for (i, dir) in ["data", "data", "parity"].iter().enumerate() {
        let address = format!("{root}/{dir}/{i}.bin");
        let bytes = vec![i as u8 + 1; 100_000 + i];
        let receipt = context
            .write_ungated(
                &OperationContext::none(),
                &address,
                &mut std::io::Cursor::new(&bytes),
                Some(bytes.len() as u64),
                &options,
            )
            .unwrap();
        assert!(!receipt.hash_verified, "deferred");
        expected.push(receipt.stored_hash.expect("hash to check later"));
    }
    let ctx = OperationContext::none();
    assert_eq!(
        context.check_stored_hashes(&ctx, &expected),
        [true, true, true]
    );
    // Same size, different bytes on the provider: only that one fails.
    std::fs::write(temp.path().join("data/arch/data/1.bin"), vec![9u8; 100_001]).unwrap();
    assert_eq!(
        context.check_stored_hashes(&ctx, &expected),
        [true, false, true]
    );
}
