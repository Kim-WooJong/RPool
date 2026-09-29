use super::route::NativeCrypt;
use crate::crypt::obscure::obscure;
use crate::storage::error::StorageErrorKind;
use crate::storage::rclone::{ConfigSelection, RcloneContext};
use crate::storage::traits::OperationContext;
use serde_json::{json, Value};

const PASSWORD: &str = "route secret";

fn ctx() -> OperationContext {
    OperationContext::none()
}

fn context() -> RcloneContext {
    RcloneContext::new("rclone-not-run".into(), ConfigSelection::Inherited)
}

fn dump(crypt: Value) -> Value {
    json!({
        "base": {"type": "local"},
        "wrap": {"type": "Alias", "remote": "base:"},
        "plain": {"type": "local"},
        "c": crypt,
    })
}

fn crypt_section(remote: &str) -> Value {
    json!({"type": "crypt", "remote": remote, "password": obscure(PASSWORD).unwrap()})
}

fn router(crypt: Value) -> NativeCrypt {
    NativeCrypt::with_dump(context(), dump(crypt))
}

fn refused(router: &NativeCrypt, raw: &str) -> String {
    let error = router.ensure(&ctx(), raw).unwrap_err();
    assert_eq!(error.kind(), StorageErrorKind::InvalidInput, "{raw}");
    let text = error.to_string();
    assert!(!text.contains(PASSWORD) && !text.contains(&obscure(PASSWORD).unwrap()));
    text
}

#[test]
fn supported_crypt_remote_routes_to_its_base() {
    let router = router(crypt_section("base:enc"));
    router.ensure(&ctx(), "c:").unwrap();
    let (backend, key) = router.route(&ctx(), "c:pool/a/shard-1").unwrap().unwrap();
    assert_eq!(key.as_str(), "pool/a/shard-1");
    assert_eq!(backend.id().as_str(), "native-crypt-c");
}

#[test]
fn non_portable_keys_fall_back_to_the_rclone_crypt_route() {
    let router = router(crypt_section("base:enc"));
    for raw in [
        "c:pool/한글",
        "c:a//b",
        "c:./a",
        "c:a/./b",
        "c:a/",
        "c:a/../b",
    ] {
        assert!(router.route(&ctx(), raw).unwrap().is_none(), "{raw}");
    }
}

#[test]
fn destinations_that_are_not_supported_crypt_remotes_are_refused() {
    let router = router(crypt_section("base:enc"));
    refused(&router, "plain:pool");
    refused(&router, "missing:pool");
    refused(&router, ":local:/tmp/pool");
    for option in ["no_data_encryption", "pass_bad_blocks"] {
        let mut section = crypt_section("base:enc");
        section[option] = json!("true");
        refused(&self::router(section), "c:pool");
    }
    let mut unknown = crypt_section("base:enc");
    unknown["future_option"] = json!("x");
    refused(&self::router(unknown), "c:pool");
    // rclone reads a present empty value as set, not as the default.
    for option in [
        "suffix",
        "filename_encryption",
        "filename_encoding",
        "directory_name_encryption",
    ] {
        let mut section = crypt_section("base:enc");
        section[option] = json!("");
        refused(&self::router(section), "c:pool");
    }
    let mut wrong_password = crypt_section("base:enc");
    wrong_password["password"] = json!("not-obscured");
    refused(&self::router(wrong_password), "c:pool");
}

#[test]
fn bases_that_could_hide_another_layer_are_refused() {
    for base in ["wrap:enc", "c:inner", ":local:/tmp/enc", "missing:enc"] {
        refused(&router(crypt_section(base)), "c:pool");
    }
}

#[test]
fn crypt_and_config_environment_overrides_are_refused() {
    for key in [
        "RCLONE_CRYPT_PASSWORD",
        "RCLONE_CRYPT_FILENAME_ENCRYPTION",
        "RCLONE_CONFIG_C_REMOTE",
    ] {
        let mut context = context();
        context.set_test_environment(key, "x");
        let router = NativeCrypt::with_dump(context, dump(crypt_section("base:enc")));
        refused(&router, "c:pool");
    }
    let mut context = context();
    context.set_test_environment("RCLONE_CONFIG_PASS", "x");
    NativeCrypt::with_dump(context, dump(crypt_section("base:enc")))
        .ensure(&ctx(), "c:pool")
        .unwrap();
}

/// End to end with the installed rclone: a native writer publishes shards onto a
/// local base, the writer's own readback goes through rclone crypt, and rclone
/// reads the same bytes under the same names. `RPOOL_TEST_RCLONE` overrides the binary.
#[test]
#[ignore = "requires rclone"]
fn native_writer_objects_read_back_through_rclone_crypt() {
    let rclone = std::env::var_os("RPOOL_TEST_RCLONE").unwrap_or_else(|| {
        if cfg!(target_os = "macos") {
            "/opt/homebrew/bin/rclone".into()
        } else {
            "rclone".into()
        }
    });
    let temp = tempfile::tempdir().unwrap();
    let base_dir = temp.path().join("base");
    std::fs::create_dir(&base_dir).unwrap();
    let conf = temp.path().join("rclone.conf");
    std::fs::write(
        &conf,
        format!(
            "[base]\ntype = local\n\n[c]\ntype = crypt\nremote = base:{}\npassword = {}\n",
            base_dir.join("enc").display(),
            obscure(PASSWORD).unwrap()
        ),
    )
    .unwrap();
    let context = RcloneContext::new(rclone.clone().into(), ConfigSelection::File(conf.clone()));
    let writer = crate::storage::writer::StorageWriter::native(context);
    assert!(writer.is_native());

    let shard = (0..3 * 65536 + 5)
        .map(|i| (i * 131 % 251) as u8)
        .collect::<Vec<_>>();
    writer.write_bytes("c:pool/a/shard-1", &shard, 2).unwrap();
    writer.write_bytes("c:pool/b/empty", &[], 2).unwrap();
    // Unchanged content is recognized and not rewritten.
    writer.write_bytes("c:pool/a/shard-1", &shard, 2).unwrap();

    let cat = |name: &str| {
        let output = std::process::Command::new(&rclone)
            .args(["cat", name, "--config"])
            .arg(&conf)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    assert_eq!(cat("c:pool/a/shard-1"), shard);
    assert!(cat("c:pool/b/empty").is_empty());

    // The base holds only ciphertext under encrypted names.
    let mut files = Vec::new();
    let mut pending = vec![base_dir.join("enc")];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    assert_eq!(files.len(), 2);
    for path in &files {
        let relative = path
            .strip_prefix(&base_dir)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            !["pool", "shard-1", "empty"]
                .iter()
                .any(|n| relative.contains(n)),
            "{relative}"
        );
        let bytes = std::fs::read(path).unwrap();
        assert!(bytes.starts_with(b"RCLONE\0\0"));
        assert!(!bytes.windows(64).any(|w| w == &shard[..64]));
    }
}
