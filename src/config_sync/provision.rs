//! Add-only crypt provisioning. Keys never appear in argv, diagnostics or history.
//! Existing configuration bytes and remote keys are preserved, with optimistic
//! conflict detection and a same-directory atomic replacement.
use super::crypt_secrets::read_dump;
use super::secret_process::{execute, Output};
use crate::models::secrets::{validate_obscured, validate_remote_name};
use crate::models::sensitive::{SensitiveBytes, SensitiveText};
use crate::models::RemoteRootStore;
use crate::remote_root::{apply_remote_root_with_store, load_remote_root_store};
use anyhow::{anyhow, bail, Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Defaults apply only to newly created crypt remotes, never existing keys.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct EncryptionDefaults {
    pub(crate) entropy_bits: usize,
    pub(crate) filename_encryption: String,
    pub(crate) directory_encryption: bool,
    /// Legacy compatibility field; ignored when selecting the backing path.
    pub(crate) root: String,
}
impl Default for EncryptionDefaults {
    fn default() -> Self {
        Self {
            entropy_bits: 1024,
            filename_encryption: "standard".into(),
            directory_encryption: true,
            root: String::new(),
        }
    }
}
impl EncryptionDefaults {
    pub(crate) fn validate(&self) -> Result<()> {
        self.setup("new_crypt".into(), "provider".into()).validate()
    }
    pub(crate) fn ensure_args(&self) -> Vec<std::ffi::OsString> {
        [
            format!("--entropy-bits={}", self.entropy_bits),
            format!("--filename-encryption={}", self.filename_encryption),
            format!("--directory-encryption={}", self.directory_encryption),
        ]
        .into_iter()
        .map(Into::into)
        .collect()
    }
    fn setup(&self, name: String, provider: String) -> CryptSetup {
        CryptSetup {
            name,
            provider,
            entropy_bits: self.entropy_bits,
            filename_encryption: self.filename_encryption.clone(),
            directory_encryption: self.directory_encryption,
        }
    }
}

pub(crate) struct CryptSetup {
    pub(crate) name: String,
    pub(crate) provider: String,
    pub(crate) entropy_bits: usize,
    pub(crate) filename_encryption: String,
    pub(crate) directory_encryption: bool,
}
impl CryptSetup {
    fn validate(&self) -> Result<()> {
        validate_remote_name(&self.name)?;
        validate_remote_name(&self.provider)?;
        if self.name.eq_ignore_ascii_case(&self.provider) {
            bail!("crypt name must differ from provider name");
        }
        if !matches!(self.entropy_bits, 128 | 256 | 512 | 1024) {
            bail!("entropy must be 128, 256, 512 or 1024 bits");
        }
        if !matches!(
            self.filename_encryption.as_str(),
            "standard" | "obfuscate" | "off"
        ) {
            bail!("unsupported filename encryption mode");
        }
        Ok(())
    }
}
fn clean_command(executable: &Path) -> Command {
    let mut cmd = Command::new(executable);
    for (key, _) in std::env::vars_os() {
        let upper = key.to_string_lossy().to_ascii_uppercase();
        if (upper.starts_with("RCLONE_")
            && upper != "RCLONE_CONFIG"
            && upper != "RCLONE_CONFIG_PASS")
            || upper.starts_with("_RCLONE_")
        {
            cmd.env_remove(key);
        }
    }
    cmd.args([
        "--ask-password=false",
        "--log-level",
        "ERROR",
        "--log-file",
        "",
    ]);
    cmd
}
fn config_path(executable: &Path) -> Result<PathBuf> {
    let mut cmd = clean_command(executable);
    cmd.args(["config", "file"]);
    let raw = execute(&mut cmd, |_| Ok(()), Output::Memory(64 * 1024))?;
    let text = std::str::from_utf8(&raw.0).map_err(|_| anyhow!("invalid config path response"))?;
    let path = PathBuf::from(
        text.lines()
            .rfind(|s| !s.trim().is_empty())
            .unwrap_or_default()
            .trim(),
    );
    if !path.is_absolute() || !path.is_file() {
        bail!("Connect a provider with rclone config first; no existing config file was found");
    }
    Ok(path)
}
fn random_hex(bits: usize) -> Result<SensitiveText> {
    let mut bytes = SensitiveBytes(vec![0; bits / 8]);
    getrandom::fill(&mut bytes.0).map_err(|_| anyhow!("OS random generator failed"))?;
    Ok(SensitiveText::new(
        bytes.0.iter().map(|b| format!("{b:02x}")).collect(),
    ))
}
fn obscure(executable: &Path, value: &SensitiveText) -> Result<SensitiveText> {
    let mut cmd = clean_command(executable);
    cmd.args(["obscure", "-"]);
    let raw = execute(
        &mut cmd,
        |stdin| {
            stdin
                .write_all(value.as_str().as_bytes())
                .map_err(|_| anyhow!("cannot send generated key"))
        },
        Output::Memory(16384),
    )?;
    let text = std::str::from_utf8(&raw.0)
        .map_err(|_| anyhow!("invalid obscure response"))?
        .trim();
    validate_obscured(text)?;
    Ok(SensitiveText::new(text.to_owned()))
}
fn candidate(
    original: &[u8],
    setup: &CryptSetup,
    backing: &str,
    password: &SensitiveText,
    salt: &SensitiveText,
) -> Result<SensitiveBytes> {
    let text = std::str::from_utf8(original).map_err(|_| anyhow!("unsupported config encoding"))?;
    if text.starts_with('\u{feff}') || text.contains("RCLONE_ENCRYPT_V") {
        bail!("Automatic setup currently needs a plaintext rclone config. For an encrypted config, use the official rclone config wizard to add crypt; do not decrypt your config for this feature.");
    }
    // Reject a collision independently of rclone's parser, including case variants.
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            if name.trim().eq_ignore_ascii_case(&setup.name) {
                bail!("remote name already exists; existing keys will not be replaced");
            }
        }
    }
    let mut result = SensitiveBytes(original.to_vec());
    result.0.extend_from_slice(format!("\n[{}]\ntype = crypt\nremote = {}\nfilename_encryption = {}\ndirectory_name_encryption = {}\npassword = ", setup.name, backing, setup.filename_encryption, setup.directory_encryption).as_bytes());
    result.0.extend_from_slice(password.as_str().as_bytes());
    result.0.extend_from_slice(b"\npassword2 = ");
    result.0.extend_from_slice(salt.as_str().as_bytes());
    result.0.extend_from_slice(b"\n");
    Ok(result)
}
struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Use the provider's configured default exactly; never append crypt-only folders.
fn crypt_backing(setup: &CryptSetup, roots: &RemoteRootStore) -> Result<String> {
    let backing = apply_remote_root_with_store(&format!("{}:", setup.provider), roots)?;
    if backing.chars().any(char::is_control) {
        bail!("provider default path must not contain control characters");
    }
    Ok(backing)
}

pub(crate) fn create_crypt(executable: &Path, setup: &CryptSetup) -> Result<String> {
    setup.validate()?;
    let config = config_path(executable)?;
    let roots = load_remote_root_store()?;
    create_crypt_at(executable, setup, &config, &roots)
}

fn create_crypt_at(
    executable: &Path,
    setup: &CryptSetup,
    config: &Path,
    roots: &RemoteRootStore,
) -> Result<String> {
    setup.validate()?;
    if fs::symlink_metadata(config)?.file_type().is_symlink() {
        bail!("symlink config files are not supported for automatic setup");
    }
    let lock_path = config.with_extension("rpool-provision.lock");
    let file = fs::OpenOptions::new().write(true).create_new(true).open(&lock_path)
        .context("Another setup may be running. Close config editors; inspect any stale rpool-provision.lock before retrying")?;
    drop(file); // Close before the RAII cleanup removes it (required on Windows).
    let _lock = Lock(lock_path);
    create_crypt_locked(executable, setup, config, roots)
}

fn create_crypt_locked(
    executable: &Path,
    setup: &CryptSetup,
    config: &Path,
    roots: &RemoteRootStore,
) -> Result<String> {
    let original = SensitiveBytes(fs::read(config)?);
    if original
        .0
        .windows(b"RCLONE_ENCRYPT_V".len())
        .any(|w| w == b"RCLONE_ENCRYPT_V")
    {
        bail!("Encrypted config: use the official rclone config wizard to add crypt. Automatic setup does not decrypt existing configs.");
    }
    let current = read_dump(executable, config)?;
    if current
        .keys()
        .any(|name| name.eq_ignore_ascii_case(&setup.name))
    {
        bail!("remote name already exists; existing keys will not be replaced");
    }
    let provider = current
        .get(&setup.provider)
        .ok_or_else(|| anyhow!("provider not found; connect and refresh first"))?;
    if provider.kind == "crypt" || provider.kind.is_empty() {
        bail!("choose a non-crypt backing provider");
    }
    let backing = crypt_backing(setup, roots)?;
    let password = obscure(executable, &random_hex(setup.entropy_bits)?)?;
    let salt = obscure(executable, &random_hex(256)?)?;
    let bytes = candidate(&original.0, setup, &backing, &password, &salt)?;
    let parent = config
        .parent()
        .ok_or_else(|| anyhow!("config has no parent"))?;
    let mut stage = tempfile::NamedTempFile::new_in(parent)?;
    stage.write_all(&bytes.0)?;
    stage.as_file().sync_all()?;
    let check = read_dump(executable, stage.path())?;
    let added = check
        .get(&setup.name)
        .ok_or_else(|| anyhow!("new crypt configuration failed validation"))?;
    if check.len() != current.len() + 1
        || added.kind != "crypt"
        || added.remote != backing
        || added.password.as_ref().map(|p| p.as_str()) != Some(password.as_str())
        || added.password2.as_ref().map(|p| p.as_str()) != Some(salt.as_str())
    {
        bail!("new crypt configuration failed validation");
    }
    if SensitiveBytes(fs::read(config)?).0 != original.0 {
        bail!("configuration changed during setup; nothing was replaced. Close other config tools and retry");
    }
    stage.persist(config).map_err(|_| {
        anyhow!("cannot atomically install new crypt config; existing config preserved")
    })?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all().context("New config installed, but directory sync failed; verify and back up configuration before uploading")?;
    Ok(backing)
}

/// An add-only reconciliation result. No keys, tokens or backing folders leave
/// the provisioning boundary.
#[derive(Debug, Default, serde::Serialize)]
pub(crate) struct EncryptionReport {
    pub(crate) created: Vec<String>,
    pub(crate) existing: Vec<String>,
    pub(crate) failed: Vec<String>,
}

pub(crate) fn ensure_encryption(
    executable: &Path,
    defaults: &EncryptionDefaults,
) -> Result<EncryptionReport> {
    defaults.validate()?;
    let config = config_path(executable)?;
    let roots = load_remote_root_store()?;
    ensure_encryption_at(executable, &config, defaults, &roots)
}

fn ensure_encryption_at(
    executable: &Path,
    config: &Path,
    defaults: &EncryptionDefaults,
    roots: &RemoteRootStore,
) -> Result<EncryptionReport> {
    defaults.validate()?;
    if fs::symlink_metadata(config)?.file_type().is_symlink() {
        bail!("symlink config files are not supported for automatic setup");
    }
    let lock_path = config.with_extension("rpool-provision.lock");
    let lock = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .context("Another setup may be running; retry after it finishes")?;
    drop(lock);
    let _lock = Lock(lock_path);
    let current = read_dump(executable, config)?;
    let mut report = EncryptionReport::default();
    let mut reserved: std::collections::BTreeSet<String> =
        current.keys().map(|n| n.to_ascii_lowercase()).collect();
    let facts: serde_json::Map<String, serde_json::Value> = current.iter().map(|(name, entry)| {
        (name.clone(), serde_json::json!({"type": entry.kind, "remote": entry.remote,
            "no_data_encryption": if entry.no_data_encryption.is_empty() { "false" } else { &entry.no_data_encryption }}))
    }).collect();
    let catalog = crate::storage::admin::RemoteCatalog::parse(&serde_json::Value::Object(facts))?;
    let missing = catalog.missing_encryption_remotes();
    for (provider, entry) in &current {
        if entry.kind.is_empty()
            || matches!(
                entry.kind.to_ascii_lowercase().as_str(),
                "crypt" | "alias" | "chunk" | "chunker" | "union" | "combine"
            )
        {
            continue;
        }
        if !missing.contains(provider) {
            report.existing.push(provider.clone());
            continue;
        }
        let stem = format!("{provider}_crypt");
        let mut name = stem.clone();
        let mut suffix = 2usize;
        while reserved.contains(&name.to_ascii_lowercase()) {
            name = format!("{stem}_{suffix}");
            suffix += 1;
        }
        reserved.insert(name.to_ascii_lowercase());
        let setup = defaults.setup(name.clone(), provider.clone());
        // Each addition is atomic. Retain successful additions on a later
        // failure; the next reconciliation recognizes them and never rotates.
        if setup
            .validate()
            .and_then(|()| create_crypt_locked(executable, &setup, config, roots))
            .is_ok()
        {
            report.created.push(name);
        } else {
            // Never forward subprocess/config contents into task diagnostics.
            report.failed.push(provider.clone());
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_executable() -> PathBuf {
        static FIXTURE: std::sync::OnceLock<(tempfile::TempDir, PathBuf)> =
            std::sync::OnceLock::new();
        FIXTURE.get_or_init(|| {
            let dir = tempfile::Builder::new().prefix("rpool provision fixture ").tempdir().unwrap();
            let source = dir.path().join("fixture.rs");
            // A native subprocess, not a shell mock: require exact public argv,
            // consume generated keys only through stdin, and parse the staged INI.
            fs::write(&source, r##"
use std::{collections::BTreeMap, fs, io::{self, Read}, path::Path};
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let quiet = ["--ask-password=false", "--log-level", "ERROR", "--log-file", ""];
    if args.first().map(String::as_str) != Some("--config") {
        assert_eq!(&args[..5], quiet);
        assert_eq!(&args[5..], ["obscure", "-"]);
        let mut key = String::new();
        io::stdin().read_to_string(&mut key).unwrap();
        assert!([32, 64, 128, 256].contains(&key.len()));
        assert!(key.bytes().all(|b| b.is_ascii_hexdigit()));
        // Valid-shaped but deliberately synthetic obscured output.
        println!("AAAAAAAAAAAAAAAAAAAAAAA");
        return;
    }
    assert_eq!(&args[2..7], quiet);
    assert_eq!(&args[7..], ["config", "dump"]);
    let path = Path::new(&args[1]);
    let text = fs::read_to_string(path).unwrap();
    let staged = path.file_name().unwrap() != "rclone.conf";
    if staged && (text.contains("# fail-stage") || text.contains("[zfail_crypt]")) { std::process::exit(9); }
    if staged && text.contains("# concurrent-edit") {
        fs::write(path.parent().unwrap().join("rclone.conf"), "# changed by another editor\n[cloud]\ntype = drive\n").unwrap();
    }
    let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut name = String::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') || line.is_empty() { continue; }
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            name = section.to_owned(); sections.entry(name.clone()).or_default();
        } else if let Some((key, value)) = line.split_once('=') {
            sections.get_mut(&name).unwrap().insert(key.trim().into(), value.trim().into());
        }
    }
    // Fixture values contain no Rust-only escapes; Unicode is emitted verbatim.
    let json = sections.iter().map(|(name, fields)| {
        let fields = fields.iter().map(|(k,v)| format!("{k:?}:{v:?}")).collect::<Vec<_>>().join(",");
        format!("{name:?}:{{{fields}}}")
    }).collect::<Vec<_>>().join(",");
    println!("{{{json}}}");
}
"##).unwrap();
            let exe = dir.path().join(if cfg!(windows) { "fake rclone.exe" } else { "fake rclone" });
            let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
                .arg("--edition=2021").arg(&source).arg("-o").arg(&exe).output().unwrap();
            assert!(output.status.success(), "fixture compilation: {}", String::from_utf8_lossy(&output.stderr));
            (dir, exe)
        }).1.clone()
    }

    #[test]
    fn backing_uses_exact_provider_default_and_ignores_legacy_parent() {
        for (default, expected) in [
            (None, "cloud:"),
            (Some(""), "cloud:"),
            (Some("/data"), "cloud:/data"),
            (Some("base"), "cloud:base"),
            (Some("/"), "cloud:/"),
            (Some("/absolute/base/"), "cloud:/absolute/base/"),
            (Some("팀 공간/자료"), "cloud:팀 공간/자료"),
            (Some("folder:"), "cloud:folder:"),
        ] {
            let mut roots = RemoteRootStore::default();
            if let Some(default) = default {
                roots.roots.insert("cloud".into(), default.into());
            }
            roots.roots.insert("other".into(), "wrong".into());
            for legacy_parent in ["", "rpool", "nested/", "/ignored legacy path"] {
                let defaults = EncryptionDefaults {
                    root: legacy_parent.into(),
                    ..EncryptionDefaults::default()
                };
                let setup = defaults.setup("cloud_crypt".into(), "cloud".into());
                setup.validate().unwrap();
                assert_eq!(crypt_backing(&setup, &roots).unwrap(), expected);
            }
        }
        let mut roots = RemoteRootStore::default();
        roots
            .roots
            .insert("cloud".into(), "base\npassword = injected".into());
        assert!(crypt_backing(&setup(), &roots).is_err());
    }

    #[test]
    fn manual_and_automatic_use_provider_roots_without_relocating_existing_crypt() {
        let tool = fixture_executable();
        for (default, expected) in [
            (None, "cloud:"),
            (Some("/data"), "cloud:/data"),
            (Some("/"), "cloud:/"),
            (Some("relative folder"), "cloud:relative folder"),
            (Some("/기본 폴더"), "cloud:/기본 폴더"),
        ] {
            for automatic in [false, true] {
                let mut roots = RemoteRootStore::default();
                if let Some(default) = default {
                    roots.roots.insert("cloud".into(), default.into());
                }
                roots.roots.insert("other".into(), "separate root".into());
                let dir = tempfile::tempdir().unwrap();
                let config = dir.path().join("rclone.conf");
                let original = "[cloud]\ntype = drive\n[other]\ntype = dropbox\n[other_crypt]\ntype = crypt\nremote = other:legacy\npassword = old-key\npassword2 = old-salt\n";
                fs::write(&config, original).unwrap();
                if automatic {
                    let defaults = EncryptionDefaults {
                        root: "ignored/legacy-parent".into(),
                        ..EncryptionDefaults::default()
                    };
                    let report = ensure_encryption_at(&tool, &config, &defaults, &roots).unwrap();
                    assert_eq!(report.created, ["cloud_crypt"]);
                    assert_eq!(report.existing, ["other"]);
                    assert!(report.failed.is_empty());
                } else {
                    let setup = setup();
                    assert_eq!(
                        create_crypt_at(&tool, &setup, &config, &roots).unwrap(),
                        expected
                    );
                }
                let dump = read_dump(&tool, &config).unwrap();
                assert_eq!(dump["cloud_crypt"].remote, expected);
                assert_eq!(dump["other_crypt"].remote, "other:legacy");
                assert_eq!(
                    dump["other_crypt"].password.as_ref().unwrap().as_str(),
                    "old-key"
                );
                assert_eq!(
                    dump["other_crypt"].password2.as_ref().unwrap().as_str(),
                    "old-salt"
                );
                let created = fs::read(&config).unwrap();
                assert!(created.starts_with(original.as_bytes()));
                roots
                    .roots
                    .insert("cloud".into(), "/changed default".into());
                let report =
                    ensure_encryption_at(&tool, &config, &EncryptionDefaults::default(), &roots)
                        .unwrap();
                assert!(report.created.is_empty());
                assert!(report.failed.is_empty());
                assert_eq!(fs::read(&config).unwrap(), created);
            }
        }
    }

    #[test]
    fn subprocess_provision_preserves_keys_modes_and_private_permissions() {
        let tool = fixture_executable();
        for mode in ["standard", "obfuscate", "off"] {
            for directory in [true, false] {
                let dir = tempfile::tempdir().unwrap();
                let config = dir.path().join("rclone.conf");
                let original = b"# keep exact bytes\r\n[cloud]\r\ntype = drive\r\ntoken = fictional-token\r\n[old_crypt]\r\ntype = crypt\r\npassword = existing-key\r\npassword2 = existing-salt\r\n";
                fs::write(&config, original).unwrap();
                let mut s = setup();
                s.filename_encryption = mode.into();
                s.directory_encryption = directory;
                let backing =
                    create_crypt_at(&tool, &s, &config, &RemoteRootStore::default()).unwrap();
                let created = fs::read(&config).unwrap();
                assert!(created.starts_with(original));
                assert_eq!(backing, "cloud:");
                let dump = read_dump(&tool, &config).unwrap();
                assert_eq!(dump[&s.name].filename_encryption, mode);
                assert_eq!(
                    dump[&s.name].directory_name_encryption,
                    directory.to_string()
                );
                assert_eq!(dump[&s.name].remote, backing);
                assert_eq!(
                    dump["old_crypt"].password.as_ref().unwrap().as_str(),
                    "existing-key"
                );
                assert_eq!(
                    dump["old_crypt"].password2.as_ref().unwrap().as_str(),
                    "existing-salt"
                );
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    assert_eq!(
                        fs::metadata(&config).unwrap().permissions().mode() & 0o777,
                        0o600
                    );
                }
                s.name = s.name.to_ascii_uppercase();
                assert!(
                    create_crypt_at(&tool, &s, &config, &RemoteRootStore::default())
                        .unwrap_err()
                        .to_string()
                        .contains("already exists")
                );
                assert_eq!(fs::read(&config).unwrap(), created);
                assert!(!config.with_extension("rpool-provision.lock").exists());
            }
        }
    }

    #[test]
    fn subprocess_failures_and_concurrent_edits_do_not_replace_config() {
        let tool = fixture_executable();
        for marker in ["fail-stage", "concurrent-edit"] {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join("rclone.conf");
            let original = format!("# {marker}\n[cloud]\ntype = drive\ntoken = fictional-token\n");
            fs::write(&config, &original).unwrap();
            let error = create_crypt_at(&tool, &setup(), &config, &RemoteRootStore::default())
                .unwrap_err()
                .to_string();
            let expected = if marker == "concurrent-edit" {
                assert!(error.contains("changed during setup"));
                "# changed by another editor\n[cloud]\ntype = drive\n"
            } else {
                original.as_str()
            };
            assert_eq!(fs::read_to_string(&config).unwrap(), expected);
            assert!(!error.contains("fictional-token"));
            assert!(!config.with_extension("rpool-provision.lock").exists());
            assert_eq!(
                fs::read_dir(dir.path()).unwrap().count(),
                1,
                "staging files must be cleaned"
            );
        }
    }

    #[test]
    fn ensure_is_add_only_collision_safe_and_idempotent() {
        let tool = fixture_executable();
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("rclone.conf");
        let original = "[cloud]\ntype = drive\n[other]\ntype = dropbox\n[alias]\ntype = alias\nremote = other:nested/folder\n[existing]\ntype = crypt\nremote = alias:encrypted\npassword = old-key\n[CLOUD_CRYPT]\ntype = alias\nremote = cloud:unrelated\n";
        fs::write(&config, original).unwrap();
        let report = ensure_encryption_at(
            &tool,
            &config,
            &EncryptionDefaults::default(),
            &RemoteRootStore::default(),
        )
        .unwrap();
        assert_eq!(report.created, ["cloud_crypt_2"]);
        assert_eq!(report.existing, ["other"]);
        assert!(report.failed.is_empty());
        let created = fs::read(&config).unwrap();
        assert!(created.starts_with(original.as_bytes()));
        let dump = read_dump(&tool, &config).unwrap();
        assert_eq!(dump["cloud_crypt_2"].filename_encryption, "standard");
        assert_eq!(dump["cloud_crypt_2"].directory_name_encryption, "true");
        assert_eq!(
            dump["existing"].password.as_ref().unwrap().as_str(),
            "old-key"
        );
        let again = ensure_encryption_at(
            &tool,
            &config,
            &EncryptionDefaults::default(),
            &RemoteRootStore::default(),
        )
        .unwrap();
        assert!(again.created.is_empty());
        assert_eq!(again.existing, ["cloud", "other"]);
        assert_eq!(fs::read(&config).unwrap(), created);
    }

    #[test]
    fn ensure_partial_failure_keeps_success_and_retry_never_rotates() {
        let tool = fixture_executable();
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("rclone.conf");
        fs::write(&config, "[cloud]\ntype = drive\n[zfail]\ntype = drive\n").unwrap();
        let report = ensure_encryption_at(
            &tool,
            &config,
            &EncryptionDefaults::default(),
            &RemoteRootStore::default(),
        )
        .unwrap();
        assert_eq!(report.created, ["cloud_crypt"]);
        assert_eq!(report.failed, ["zfail"]);
        let first = fs::read(&config).unwrap();
        let again = ensure_encryption_at(
            &tool,
            &config,
            &EncryptionDefaults::default(),
            &RemoteRootStore::default(),
        )
        .unwrap();
        assert!(again.created.is_empty());
        assert_eq!(again.existing, ["cloud"]);
        assert_eq!(again.failed, ["zfail"]);
        assert_eq!(fs::read(&config).unwrap(), first);
    }

    #[test]
    fn ensure_reports_failure_without_secret_or_config_damage() {
        let tool = fixture_executable();
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("rclone.conf");
        let original = "# fail-stage\n[cloud]\ntype = drive\ntoken = fictional-secret\n";
        fs::write(&config, original).unwrap();
        let report = ensure_encryption_at(
            &tool,
            &config,
            &EncryptionDefaults::default(),
            &RemoteRootStore::default(),
        )
        .unwrap();
        assert_eq!(report.failed, ["cloud"]);
        assert!(report.created.is_empty());
        assert!(!format!("{report:?}").contains("fictional-secret"));
        assert_eq!(fs::read_to_string(&config).unwrap(), original);
    }

    #[test]
    fn defaults_drive_entropy_and_cli_options() {
        let defaults = EncryptionDefaults::default();
        assert_eq!(
            defaults.setup("crypt".into(), "cloud".into()).entropy_bits,
            1024
        );
        assert_eq!(
            defaults.ensure_args(),
            [
                "--entropy-bits=1024",
                "--filename-encryption=standard",
                "--directory-encryption=true"
            ]
            .map(std::ffi::OsString::from)
        );
        let mut custom = defaults;
        custom.entropy_bits = 128;
        custom.root = String::new();
        assert!(custom.validate().is_ok());
        assert_eq!(
            custom.setup("crypt".into(), "cloud".into()).entropy_bits,
            128
        );
        custom.filename_encryption = "unknown".into();
        assert!(custom.validate().is_err());
    }

    #[test]
    fn ensure_arguments_round_trip_through_cli() {
        use crate::cli::{Cli, Commands, ProviderCommands};
        use clap::Parser;
        for root in ["", "with spaces/암호화", "-leading/폴더 space"] {
            let defaults = EncryptionDefaults {
                root: root.into(),
                entropy_bits: 512,
                filename_encryption: "obfuscate".into(),
                directory_encryption: false,
            };
            defaults.validate().unwrap();
            let mut argv: Vec<std::ffi::OsString> =
                ["rpool", "provider", "ensure-encryption", "--json"]
                    .map(Into::into)
                    .to_vec();
            argv.extend(defaults.ensure_args());
            // Legacy CLI values remain accepted, but are not emitted by defaults.
            argv.push(format!("--root={root}").into());
            let parsed = Cli::try_parse_from(argv).unwrap();
            match parsed.command.unwrap() {
                Commands::Provider(args) => match args.command {
                    ProviderCommands::EnsureEncryption {
                        root,
                        entropy_bits,
                        filename_encryption,
                        directory_encryption,
                        json,
                    } => {
                        assert!(json);
                        assert_eq!(
                            EncryptionDefaults {
                                root,
                                entropy_bits,
                                filename_encryption,
                                directory_encryption
                            },
                            defaults
                        );
                    }
                    _ => panic!("expected ensure-encryption"),
                },
                _ => panic!("expected provider"),
            }
        }
    }

    #[test]
    fn ensure_applies_custom_defaults_without_rotating_existing_keys() {
        let tool = fixture_executable();
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("rclone.conf");
        fs::write(&config, "[cloud]\ntype = drive\n").unwrap();
        let defaults = EncryptionDefaults {
            root: "private/nested".into(),
            entropy_bits: 1024,
            filename_encryption: "off".into(),
            directory_encryption: false,
        };
        let report =
            ensure_encryption_at(&tool, &config, &defaults, &RemoteRootStore::default()).unwrap();
        assert_eq!(report.created, ["cloud_crypt"]);
        let dump = read_dump(&tool, &config).unwrap();
        assert_eq!(dump["cloud_crypt"].remote, "cloud:");
        assert_eq!(dump["cloud_crypt"].filename_encryption, "off");
        assert_eq!(dump["cloud_crypt"].directory_name_encryption, "false");
        let created = fs::read(&config).unwrap();
        let again = ensure_encryption_at(
            &tool,
            &config,
            &EncryptionDefaults::default(),
            &RemoteRootStore::default(),
        )
        .unwrap();
        assert!(again.created.is_empty());
        assert_eq!(fs::read(&config).unwrap(), created);
        let mut invalid = defaults;
        invalid.entropy_bits = 12;
        assert!(
            ensure_encryption_at(&tool, &config, &invalid, &RemoteRootStore::default()).is_err()
        );
        assert_eq!(fs::read(&config).unwrap(), created);
    }

    fn setup() -> CryptSetup {
        CryptSetup {
            name: "cloud_crypt".into(),
            provider: "cloud".into(),
            entropy_bits: 256,
            filename_encryption: "standard".into(),
            directory_encryption: true,
        }
    }
    #[test]
    fn provisioning_preserves_original_bytes_and_rejects_collisions() {
        let s = setup();
        let secret = SensitiveText::new("AAAAAAAAAAAAAAAAAAAAAAA".into());
        let original = b"# keep\r\n[cloud]\r\ntype = drive\r\ntoken = existing\r\n";
        let result = candidate(original, &s, "cloud:rpool/new", &secret, &secret).unwrap();
        assert!(result.0.starts_with(original));
        assert!(candidate(
            b"[CLOUD_CRYPT]\npassword = old\n",
            &s,
            "cloud:new",
            &secret,
            &secret
        )
        .is_err());
        assert!(candidate(b"RCLONE_ENCRYPT_V0:\n", &s, "cloud:new", &secret, &secret).is_err());
    }
    #[test]
    fn rejects_config_injection_and_invalid_encryption_options() {
        let mut s = setup();
        s.name = "x]\npassword = injected".into();
        assert!(s.validate().is_err());
        let mut s = setup();
        s.entropy_bits = 12;
        assert!(s.validate().is_err());
        let mut s = setup();
        s.filename_encryption = "unknown".into();
        assert!(s.validate().is_err());
    }
    #[test]
    fn generated_secret_lengths_match_entropy() {
        for bits in [128, 256, 512, 1024] {
            assert_eq!(random_hex(bits).unwrap().as_str().len(), bits / 4);
        }
        assert!(random_hex(256).unwrap().as_str() != random_hex(256).unwrap().as_str());
    }
    #[test]
    #[ignore = "requires real rclone; isolated local alias only"]
    fn real_provision_roundtrip_and_existing_config_preservation() {
        let tool = std::env::var_os("RPOOL_TEST_RCLONE_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| "rclone".into());
        let dir = tempfile::tempdir().unwrap();
        let backing = dir.path().join("backing");
        fs::create_dir(&backing).unwrap();
        let config = dir.path().join("rclone.conf");
        let original = format!(
            "# preserved\n[cloud]\ntype = alias\nremote = {}\n",
            backing.display()
        );
        fs::write(&config, &original).unwrap();
        let s = setup();
        create_crypt_at(&tool, &s, &config, &RemoteRootStore::default()).unwrap();
        let created = SensitiveBytes(fs::read(&config).unwrap());
        assert!(created.0.starts_with(original.as_bytes()));
        assert!(create_crypt_at(&tool, &s, &config, &RemoteRootStore::default()).is_err());
        assert!(created.0 == fs::read(&config).unwrap());
        assert!(!config.with_extension("rpool-provision.lock").exists());
        let input = dir.path().join("input.txt");
        fs::write(&input, b"roundtrip content").unwrap();
        let mut upload = super::super::secret_process::rclone_command(&tool, &config);
        upload
            .arg("copyto")
            .arg(&input)
            .arg("cloud_crypt:hello.txt");
        execute(&mut upload, |_| Ok(()), Output::Memory(65536)).unwrap();
        let mut read = super::super::secret_process::rclone_command(&tool, &config);
        read.args(["cat", "cloud_crypt:hello.txt"]);
        let result = execute(&mut read, |_| Ok(()), Output::Memory(65536)).unwrap();
        assert!(result.0 == b"roundtrip content");
        let mut invalid = setup();
        invalid.name = "rejected".into();
        invalid.provider = "missing".into();
        assert!(create_crypt_at(&tool, &invalid, &config, &RemoteRootStore::default()).is_err());
        assert!(created.0 == fs::read(&config).unwrap());
    }
}
