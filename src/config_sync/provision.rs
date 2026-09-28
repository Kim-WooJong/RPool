//! Add-only crypt provisioning. Keys never appear in argv, diagnostics or history.
//! Existing configuration bytes and remote keys are preserved, with optimistic
//! conflict detection and a same-directory atomic replacement.
use super::crypt_secrets::read_dump;
use super::secret_process::{execute, Output};
use crate::models::secrets::{validate_obscured, validate_remote_name};
use crate::models::sensitive::{SensitiveBytes, SensitiveText};
use anyhow::{anyhow, bail, Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) struct CryptSetup {
    pub(crate) name: String,
    pub(crate) provider: String,
    pub(crate) root: String,
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
        if self.root.starts_with('/')
            || self.root.contains(['\\', ':'])
            || self.root.chars().any(char::is_control)
            || self.root.split('/').any(|s| s == ".." || s == ".")
            || self.root.trim() != self.root
        {
            bail!(
                "root must be a relative provider folder without traversal or control characters"
            );
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
            .filter(|s| !s.trim().is_empty())
            .next_back()
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

pub(crate) fn create_crypt(executable: &Path, setup: &CryptSetup) -> Result<String> {
    setup.validate()?;
    let config = config_path(executable)?;
    create_crypt_at(executable, setup, &config)
}

fn create_crypt_at(executable: &Path, setup: &CryptSetup, config: &Path) -> Result<String> {
    setup.validate()?;
    if fs::symlink_metadata(&config)?.file_type().is_symlink() {
        bail!("symlink config files are not supported for automatic setup");
    }
    let lock_path = config.with_extension("rpool-provision.lock");
    let file = fs::OpenOptions::new().write(true).create_new(true).open(&lock_path)
        .context("Another setup may be running. Close config editors; inspect any stale rpool-provision.lock before retrying")?;
    drop(file); // Close before the RAII cleanup removes it (required on Windows).
    let _lock = Lock(lock_path);
    let original = SensitiveBytes(fs::read(&config)?);
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
    let unique = random_hex(128)?;
    let backing = format!(
        "{}:{}{}rpool-crypt-{}",
        setup.provider,
        setup.root.trim_end_matches('/'),
        if setup.root.is_empty() { "" } else { "/" },
        unique.as_str()
    );
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
    if SensitiveBytes(fs::read(&config)?).0 != original.0 {
        bail!("configuration changed during setup; nothing was replaced. Close other config tools and retry");
    }
    stage.persist(&config).map_err(|_| {
        anyhow!("cannot atomically install new crypt config; existing config preserved")
    })?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all().context("New config installed, but directory sync failed; verify and back up configuration before uploading")?;
    Ok(backing)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_executable() -> PathBuf {
        static FIXTURE: std::sync::OnceLock<(tempfile::TempDir, PathBuf)> = std::sync::OnceLock::new();
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
    if staged && text.contains("# fail-stage") { std::process::exit(9); }
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
    // All fixture values are ASCII, so Rust string escaping is valid JSON here.
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
                let backing = create_crypt_at(&tool, &s, &config).unwrap();
                let created = fs::read(&config).unwrap();
                assert!(created.starts_with(original));
                assert!(backing.starts_with("cloud:rpool/rpool-crypt-"));
                let dump = read_dump(&tool, &config).unwrap();
                assert_eq!(dump[&s.name].filename_encryption, mode);
                assert_eq!(dump[&s.name].directory_name_encryption, directory.to_string());
                assert_eq!(dump[&s.name].remote, backing);
                assert_eq!(dump["old_crypt"].password.as_ref().unwrap().as_str(), "existing-key");
                assert_eq!(dump["old_crypt"].password2.as_ref().unwrap().as_str(), "existing-salt");
                #[cfg(unix)] {
                    use std::os::unix::fs::PermissionsExt;
                    assert_eq!(fs::metadata(&config).unwrap().permissions().mode() & 0o777, 0o600);
                }
                s.name = s.name.to_ascii_uppercase();
                assert!(create_crypt_at(&tool, &s, &config).unwrap_err().to_string().contains("already exists"));
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
            let error = create_crypt_at(&tool, &setup(), &config).unwrap_err().to_string();
            let expected = if marker == "concurrent-edit" {
                assert!(error.contains("changed during setup"));
                "# changed by another editor\n[cloud]\ntype = drive\n"
            } else { original.as_str() };
            assert_eq!(fs::read_to_string(&config).unwrap(), expected);
            assert!(!error.contains("fictional-token"));
            assert!(!config.with_extension("rpool-provision.lock").exists());
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "staging files must be cleaned");
        }
    }

    fn setup() -> CryptSetup {
        CryptSetup {
            name: "cloud_crypt".into(),
            provider: "cloud".into(),
            root: "rpool".into(),
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
    fn rejects_config_injection_and_unsafe_root() {
        for root in [
            "../old",
            "/absolute",
            "a\nb",
            "a\\b",
            "remote:path",
            "a/./b",
        ] {
            let mut s = setup();
            s.root = root.into();
            assert!(s.validate().is_err());
        }
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
        create_crypt_at(&tool, &s, &config).unwrap();
        let created = SensitiveBytes(fs::read(&config).unwrap());
        assert!(created.0.starts_with(original.as_bytes()));
        assert!(create_crypt_at(&tool, &s, &config).is_err());
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
        assert!(create_crypt_at(&tool, &invalid, &config).is_err());
        assert!(created.0 == fs::read(&config).unwrap());
    }
}
