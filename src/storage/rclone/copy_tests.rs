//! `copy_object` and the copy-verification queries against a fake rclone
//! shell script (unix only).
use super::*;
use crate::storage::error::StorageErrorKind;
use std::os::unix::fs::PermissionsExt;

const FAKE: &str = r#"#!/bin/sh
config="$2"; shift 2
last=""; for a in "$@"; do last="$a"; done
case "$1" in
  config) cat "$config" ;;
  lsjson)
    case " $* " in
      *" --hash "*) printf '{"Size":10,"IsDir":false,"Hashes":{"sha1":"ABC"}}' ;;
      *) case "$last" in *exists*) printf '{"Size":6,"IsDir":false}' ;; *) exit 3 ;; esac ;;
    esac ;;
  copyto) echo "$*" > "$config.copied" ;;
  backend)
    case "$last" in
      c1:) printf '{"Hashes":[],"Features":{"Copy":true}}' ;;
      *) printf '{"Hashes":["md5","sha1","none"],"Features":{"Copy":true}}' ;;
    esac ;;
  cryptdecode) printf '%s \t enc/%s\n' "$last" "$last" ;;
  *) exit 2 ;;
esac
"#;

fn fake() -> (tempfile::TempDir, RcloneContext, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("rclone");
    std::fs::write(&exe, FAKE).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config = dir.path().join("config.json");
    std::fs::write(
        &config,
        r#"{"c1":{"type":"crypt","remote":"b1:/root"},"plain":{"type":"local"},"b1":{"type":"local"}}"#,
    )
    .unwrap();
    let mut context = RcloneContext::new(exe, ConfigSelection::File(config.clone()));
    context.environment.retain(|(k, _)| {
        !k.to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("RCLONE_")
    });
    (dir, context, config.with_extension("json.copied"))
}

#[test]
fn copy_object_refuses_existing_and_non_crypt_destinations() {
    let (_dir, context, copied) = fake();
    let ctx = OperationContext::none();
    let error = context.copy_object(&ctx, "c1:a", "c1:exists").unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::AlreadyExists);
    let error = context.copy_object(&ctx, "c1:a", "plain:new").unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::InvalidInput);
    assert!(!copied.exists(), "nothing was copied");
    context.copy_object(&ctx, "c1:a/x", "c1:b/x").unwrap();
    let args = std::fs::read_to_string(&copied).unwrap();
    assert!(args.starts_with("copyto --ignore-existing"), "{args}");
    assert!(args.trim_end().ends_with("-- c1:a/x c1:b/x"), "{args}");
}

#[test]
fn crypt_copy_queries_resolve_base_object_and_hash() {
    let (_dir, context, _) = fake();
    let ctx = OperationContext::none();
    let caps = context.crypt_copy_capabilities(&ctx, "c1:x").unwrap();
    assert!(caps.server_side_copy);
    assert_eq!(caps.hashes, ["sha1", "md5"]);
    assert_eq!(caps.base, "b1:/root");
    assert!(context.crypt_copy_capabilities(&ctx, "plain:x").is_err());
    let base = context
        .crypt_base_object(&ctx, "c1:arch/data/1.bin", &caps.base)
        .unwrap();
    assert_eq!(base, "b1:/root/enc/arch/data/1.bin");
    assert_eq!(
        context.object_hash(&ctx, &base, &caps.hashes).unwrap(),
        Some((10, "sha1:abc".to_owned()))
    );
    assert_eq!(
        context.object_hash(&ctx, &base, &["md5".into()]).unwrap(),
        None,
        "an advertised hash the object does not report"
    );
    assert_eq!(context.object_hash(&ctx, &base, &[]).unwrap(), None);
}

#[test]
fn copy_query_parsers_are_strict() {
    let features =
        parse_backend_features(br#"{"Hashes":["md5"],"Features":{"Copy":false}}"#).unwrap();
    assert_eq!(
        features,
        BackendFeatures {
            copy: false,
            hashes: vec!["md5".into()]
        }
    );
    assert!(preferred_hashes(&[]).is_empty());
    assert_eq!(preferred_hashes(&["crc32".into(), "md5".into()]), ["md5"]);
    assert_eq!(preferred_hashes(&["crc32".into()]), ["crc32"]);
    let local: Vec<String> = ["md5", "sha1", "whirlpool", "sha256", "blake3"]
        .map(String::from)
        .to_vec();
    assert_eq!(preferred_hashes(&local), ["sha256", "sha1", "md5"]);
    assert_eq!(parse_cryptdecode(b"a/b \t x/y\n", "a/b").unwrap(), "x/y");
    for bad in [
        &b""[..],
        b"a/b x/y",
        b"other \t x",
        b"a/b \t /abs",
        b"a/b \t x/../y",
        b"a/b \t r:x",
        b"a/b \t x\na/b \t y",
    ] {
        assert!(parse_cryptdecode(bad, "a/b").is_err(), "{bad:?}");
    }
    assert_eq!(join_base("b:", "x"), "b:x");
    assert_eq!(join_base("/root/", "x"), "/root/x");
    assert_eq!(join_base("b:/root", "x"), "b:/root/x");
    let md5 = ["sha1".to_owned(), "md5".to_owned()];
    assert!(parse_object_hash(br#"{"Size":1,"IsDir":true}"#, &md5).is_err());
    assert_eq!(
        parse_object_hash(br#"{"Size":2,"IsDir":false,"Hashes":{"md5":"AB"}}"#, &md5).unwrap(),
        Some((2, "md5:ab".to_owned()))
    );
}
