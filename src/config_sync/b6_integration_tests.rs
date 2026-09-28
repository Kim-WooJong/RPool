//! B6 real-tool integration tests.
//!
//! These tests are deliberately ignored by default because they execute the
//! externally installed `rclone`, `age`, and `age-keygen` binaries. They never
//! use the user's normal rclone config: every scenario is isolated in a fresh
//! temporary directory and passes an explicit `--config` path.
//!
//! Optional executable overrides:
//! - RPOOL_TEST_RCLONE_BIN
//! - RPOOL_TEST_AGE_BIN
//! - RPOOL_TEST_AGE_KEYGEN_BIN
//!
//! Suggested manual run after normal compilation succeeds:
//! `cargo test b6_real_ -- --ignored --test-threads=1`

use super::age_vault::{AgeDecrypt, AgeEncrypt};
use super::crypt_generate::generate_new_remote_secrets;
use super::crypt_restore::restore_crypt_vault;
use super::crypt_secrets::{export_crypt_secret_vault, extract_crypt_secrets};
use super::secret_process::{execute, rclone_command, Output};
use super::transaction::TransactionOutcome;
use crate::models::secrets::{CryptSecret, SecretBundle};
use crate::models::sensitive::SensitiveBytes;
use crate::models::{
    Placement, PoolStore, PortableConfig, PortableCryptRemote, PortableGuiSettings,
    RemoteRootStore, CONFIG_SYNC_FORMAT, CONFIG_SYNC_VERSION,
};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const LOCAL_REMOTE: &str = "b6_local";

#[derive(Clone, Copy)]
struct RemoteSpec {
    name: &'static str,
    backing: &'static str,
    password2: bool,
}

struct Tools {
    rclone: PathBuf,
    age: PathBuf,
    age_keygen: PathBuf,
}

impl Tools {
    fn from_env() -> Self {
        Self {
            rclone: env_path("RPOOL_TEST_RCLONE_BIN", "rclone"),
            age: env_path("RPOOL_TEST_AGE_BIN", "age"),
            age_keygen: env_path("RPOOL_TEST_AGE_KEYGEN_BIN", "age-keygen"),
        }
    }

    fn preflight(&self) -> Result<()> {
        require_tool(&self.rclone, &["version"])?;
        require_tool(&self.age, &["--version"])?;
        require_tool(&self.age_keygen, &["--version"])?;
        Ok(())
    }
}

struct Scenario {
    _temp: tempfile::TempDir,
    tools: Tools,
    source_config: PathBuf,
    target_config: PathBuf,
    artifact_root: PathBuf,
    vault: PathBuf,
    identity: PathBuf,
    wrong_identity: PathBuf,
    recipient: String,
    portable: PortableConfig,
    original_secrets: SecretBundle,
    sentinels: Vec<(String, Vec<u8>)>,
}

fn env_path(name: &str, fallback: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(fallback))
}

fn require_tool(executable: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new(executable)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| anyhow!("required B6 test executable is unavailable"))?;
    if !status.success() {
        bail!("required B6 test executable failed its version check");
    }
    Ok(())
}

fn run_rclone<I, S>(rclone: &Path, config: &Path, args: I) -> Result<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = rclone_command(rclone, config);
    command.args(args);
    let output = execute(&mut command, |_| Ok(()), Output::Memory(OUTPUT_LIMIT))?;
    Ok(output.0.clone())
}

fn rclone_path(path: &Path) -> Result<String> {
    let absolute = path
        .canonicalize()
        .context("cannot canonicalize B6 backing path")?;
    let text = absolute
        .to_str()
        .ok_or_else(|| anyhow!("B6 path is not valid Unicode"))?;
    Ok(text.replace('\\', "/"))
}

fn create_local_remote(rclone: &Path, config: &Path) -> Result<()> {
    run_rclone(
        rclone,
        config,
        [
            "config",
            "create",
            LOCAL_REMOTE,
            "local",
            "--non-interactive",
            "--no-output",
        ],
    )?;
    Ok(())
}

fn create_crypt_remote(
    rclone: &Path,
    config: &Path,
    portable: &PortableCryptRemote,
    secret: &CryptSecret,
    include_password2: bool,
) -> Result<()> {
    let mut command = rclone_command(rclone, config);
    command
        .args([
            "config",
            "create",
            "--no-obscure",
            "--non-interactive",
            "--no-output",
            "--",
        ])
        .arg(&portable.name)
        .arg("crypt")
        .arg("remote")
        .arg(&portable.remote)
        .arg("filename_encryption")
        .arg(&portable.filename_encryption)
        .arg("directory_name_encryption")
        .arg(if portable.directory_name_encryption {
            "true"
        } else {
            "false"
        })
        .arg("password")
        .arg(secret.obscured_password.as_str());
    if include_password2 {
        let password2 = secret
            .obscured_password2
            .as_ref()
            .ok_or_else(|| anyhow!("B6 source fixture expected password2"))?;
        command.arg("password2").arg(password2.as_str());
    }
    execute(&mut command, |_| Ok(()), Output::Memory(OUTPUT_LIMIT))?;
    Ok(())
}

fn write_age_identity(age_keygen: &Path, identity: &Path) -> Result<String> {
    let status = Command::new(age_keygen)
        .arg("--output")
        .arg(identity)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| anyhow!("cannot start age-keygen"))?;
    if !status.success() {
        bail!("age-keygen failed to create B6 identity");
    }

    let output = Command::new(age_keygen)
        .arg("-y")
        .arg(identity)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| anyhow!("cannot derive age recipient"))?;
    if !output.status.success() {
        bail!("age-keygen failed to derive B6 recipient");
    }
    let recipient = std::str::from_utf8(&output.stdout)
        .map_err(|_| anyhow!("age recipient is not UTF-8"))?
        .trim();
    if recipient.is_empty() || recipient.chars().any(char::is_whitespace) {
        bail!("age-keygen returned an invalid B6 recipient");
    }
    Ok(recipient.to_owned())
}

fn config_digest(path: &Path) -> Result<blake3::Hash> {
    let bytes = SensitiveBytes(fs::read(path).context("cannot read B6 config for digest")?);
    Ok(blake3::hash(&bytes.0))
}

fn directory_entries(path: &Path) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(path).context("cannot inspect B6 config directory")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        names.insert(name);
    }
    Ok(names)
}

fn portable_config(crypt_remotes: Vec<PortableCryptRemote>) -> PortableConfig {
    PortableConfig {
        format: CONFIG_SYNC_FORMAT.to_owned(),
        version: CONFIG_SYNC_VERSION,
        exported_at_unix: 0,
        pools: PoolStore::default(),
        remote_roots: RemoteRootStore::default(),
        gui: PortableGuiSettings {
            default_remote_path: String::new(),
            remotes: Vec::new(),
            shard_mib: 1,
            workers: 1,
            retries: 0,
            data_shards: 1,
            parity_shards: 0,
            placement: Placement::RoundRobin,
        },
        secret_vault: None,
        crypt_remotes,
    }
}

fn assert_generated_secret_independence(bundle: &SecretBundle, names: &[String]) -> Result<()> {
    let mut seen = BTreeSet::<Vec<u8>>::new();
    for name in names {
        let secret = bundle
            .rclone
            .crypt
            .get(name)
            .ok_or_else(|| anyhow!("generated B6 secret is missing"))?;
        let password = secret.obscured_password.as_str().as_bytes().to_vec();
        let password2 = secret
            .obscured_password2
            .as_ref()
            .ok_or_else(|| anyhow!("generated B6 password2 is missing"))?
            .as_str()
            .as_bytes()
            .to_vec();
        if password == password2 {
            bail!("B6 generated password and password2 are not independent");
        }
        if !seen.insert(password) || !seen.insert(password2) {
            bail!("B6 generated crypt secrets were reused across remotes");
        }
    }
    Ok(())
}
fn exact_secret_equality(left: &SecretBundle, right: &SecretBundle) -> Result<()> {
    if left.schema_version != right.schema_version
        || left.rclone.crypt.len() != right.rclone.crypt.len()
    {
        bail!("B6 secret bundle shape changed");
    }
    for (name, expected) in &left.rclone.crypt {
        let restored = right
            .rclone
            .crypt
            .get(name)
            .ok_or_else(|| anyhow!("B6 restored secret bundle is missing a remote"))?;
        if expected.obscured_password.as_str().as_bytes()
            != restored.obscured_password.as_str().as_bytes()
        {
            bail!("B6 password obscured bytes changed");
        }
        let expected2 = expected
            .obscured_password2
            .as_ref()
            .map(|value| value.as_str().as_bytes());
        let restored2 = restored
            .obscured_password2
            .as_ref()
            .map(|value| value.as_str().as_bytes());
        if expected2 != restored2 {
            bail!("B6 password2 obscured bytes changed");
        }
    }
    Ok(())
}

impl Scenario {
    fn new(specs: &[RemoteSpec]) -> Result<Self> {
        let tools = Tools::from_env();
        tools.preflight()?;
        let temp = tempfile::tempdir().context("cannot create B6 test directory")?;
        let root = temp.path();
        let config_dir = root.join("configs");
        let backing_root = root.join("backing");
        let input_root = root.join("input");
        let identity_root = root.join("identity");
        let artifact_root = root.join("portable-artifact");
        fs::create_dir_all(&config_dir)?;
        fs::create_dir_all(&backing_root)?;
        fs::create_dir_all(&input_root)?;
        fs::create_dir_all(&identity_root)?;
        fs::create_dir_all(artifact_root.join("secrets"))?;

        let identity = identity_root.join("b6-identity.txt");
        let wrong_identity = identity_root.join("b6-wrong-identity.txt");
        let recipient = write_age_identity(&tools.age_keygen, &identity)?;
        let _wrong_recipient = write_age_identity(&tools.age_keygen, &wrong_identity)?;

        let source_config = config_dir.join("source.conf");
        let target_config = config_dir.join("target.conf");
        create_local_remote(&tools.rclone, &source_config)?;
        create_local_remote(&tools.rclone, &target_config)?;

        let names: Vec<String> = specs.iter().map(|spec| spec.name.to_owned()).collect();
        let generated_source = generate_new_remote_secrets(&tools.rclone, &source_config, &names)?;
        assert_generated_secret_independence(&generated_source, &names)?;

        let mut expected_portable = Vec::with_capacity(specs.len());
        let mut sentinels = Vec::with_capacity(specs.len());
        for (index, spec) in specs.iter().enumerate() {
            let backing = backing_root.join(spec.backing);
            fs::create_dir_all(&backing)?;
            let remote = format!("{}:{}", LOCAL_REMOTE, rclone_path(&backing)?);
            let definition = PortableCryptRemote {
                name: spec.name.to_owned(),
                kind: "crypt".to_owned(),
                remote,
                filename_encryption: "standard".to_owned(),
                directory_name_encryption: true,
            };
            let generated = generated_source
                .rclone
                .crypt
                .get(spec.name)
                .ok_or_else(|| anyhow!("generated B6 secret is missing"))?;
            create_crypt_remote(
                &tools.rclone,
                &source_config,
                &definition,
                generated,
                spec.password2,
            )?;

            let sentinel_name = format!("sentinel-{index}.bin");
            let sentinel_path = input_root.join(&sentinel_name);
            let sentinel = format!("rpool-b6-sentinel-v1:{index}:{}\n", spec.name).into_bytes();
            fs::write(&sentinel_path, &sentinel)?;
            let destination = format!("{}:{}", spec.name, sentinel_name);
            // Use an explicit command because the source and destination have different OsStr lifetimes.
            let mut copy = rclone_command(&tools.rclone, &source_config);
            copy.arg("copyto").arg(&sentinel_path).arg(&destination);
            execute(&mut copy, |_| Ok(()), Output::Memory(OUTPUT_LIMIT))?;
            sentinels.push((destination, sentinel));
            expected_portable.push(definition);
        }

        let vault = artifact_root.join("secrets").join("rclone.age");
        let encrypt = AgeEncrypt {
            executable: &tools.age,
            recipient: &recipient,
        };
        let exported = export_crypt_secret_vault(&tools.rclone, &source_config, &encrypt, &vault)?;
        if exported.len() != expected_portable.len() {
            bail!("B6 exported crypt remote count changed");
        }
        for expected in &expected_portable {
            let actual = exported
                .iter()
                .find(|remote| remote.name == expected.name)
                .ok_or_else(|| anyhow!("B6 portable export is missing a crypt remote"))?;
            if actual.kind != expected.kind
                || actual.remote != expected.remote
                || actual.filename_encryption != expected.filename_encryption
                || actual.directory_name_encryption != expected.directory_name_encryption
            {
                bail!("B6 portable crypt definition changed");
            }
        }
        let (original_secrets, _) = extract_crypt_secrets(&tools.rclone, &source_config)?;

        let generated_target = generate_new_remote_secrets(&tools.rclone, &target_config, &names)?;
        for definition in &exported {
            let dummy = generated_target
                .rclone
                .crypt
                .get(&definition.name)
                .ok_or_else(|| anyhow!("generated B6 target secret is missing"))?;
            // Always seed password2 in the target. If the source omitted it, B4
            // must remove this stale salt during restore.
            create_crypt_remote(&tools.rclone, &target_config, definition, dummy, true)?;
        }

        Ok(Self {
            _temp: temp,
            tools,
            source_config,
            target_config,
            artifact_root,
            vault,
            identity,
            wrong_identity,
            recipient,
            portable: portable_config(exported),
            original_secrets,
            sentinels,
        })
    }

    fn decrypt(&self) -> AgeDecrypt<'_> {
        AgeDecrypt {
            executable: &self.tools.age,
            identity: &self.identity,
            artifact_root: &self.artifact_root,
        }
    }

    fn wrong_decrypt(&self) -> AgeDecrypt<'_> {
        AgeDecrypt {
            executable: &self.tools.age,
            identity: &self.wrong_identity,
            artifact_root: &self.artifact_root,
        }
    }

    fn encrypt(&self) -> AgeEncrypt<'_> {
        AgeEncrypt {
            executable: &self.tools.age,
            recipient: &self.recipient,
        }
    }

    fn restore(&self, vault: &Path, decrypt: &AgeDecrypt<'_>) -> Result<TransactionOutcome> {
        let encrypt = self.encrypt();
        restore_crypt_vault(
            &self.tools.rclone,
            &self.target_config,
            &self.portable,
            vault,
            decrypt,
            &encrypt,
        )
    }

    fn assert_roundtrip(&self) -> Result<()> {
        let before = extract_crypt_secrets(&self.tools.rclone, &self.target_config)?.0;
        if exact_secret_equality(&self.original_secrets, &before).is_ok() {
            bail!("B6 target unexpectedly started with the source keys");
        }
        for (remote_path, _) in &self.sentinels {
            if run_rclone(
                &self.tools.rclone,
                &self.target_config,
                ["cat", remote_path.as_str()],
            )
            .is_ok()
            {
                bail!("B6 sentinel was readable before key restore");
            }
        }

        let config_dir = self
            .target_config
            .parent()
            .ok_or_else(|| anyhow!("B6 target config has no parent"))?;
        let before_entries = directory_entries(config_dir)?;
        let decrypt = self.decrypt();
        let first = self.restore(&self.vault, &decrypt)?;
        if !matches!(first, TransactionOutcome::Committed { .. }) {
            bail!("B6 first restore did not commit");
        }
        let after_entries = directory_entries(config_dir)?;
        let lock_name = format!(
            "{}.rpool-lock",
            self.target_config
                .file_name()
                .ok_or_else(|| anyhow!("B6 target config has no file name"))?
                .to_string_lossy()
        );
        let unexpected: Vec<&String> = after_entries
            .difference(&before_entries)
            .filter(|name| name.as_str() != lock_name.as_str())
            .collect();
        if !unexpected.is_empty() {
            bail!("B6 restore left unexpected sibling files");
        }
        let restored = extract_crypt_secrets(&self.tools.rclone, &self.target_config)?.0;
        exact_secret_equality(&self.original_secrets, &restored)?;

        for (remote_path, expected) in &self.sentinels {
            let actual = run_rclone(
                &self.tools.rclone,
                &self.target_config,
                ["cat", remote_path.as_str()],
            )?;
            if actual.as_slice() != expected.as_slice() {
                bail!("B6 decrypted sentinel content changed");
            }
        }

        let after_first = config_digest(&self.target_config)?;
        let second = self.restore(&self.vault, &decrypt)?;
        if second != TransactionOutcome::NoChanges {
            bail!("B6 repeated restore was not idempotent");
        }
        if config_digest(&self.target_config)? != after_first {
            bail!("B6 idempotent restore changed config bytes");
        }
        Ok(())
    }
}

#[test]
#[ignore = "B6 real integration: requires rclone, age, and age-keygen"]
fn b6_real_single_crypt_roundtrip_exact_obscured_and_idempotent() -> Result<()> {
    let scenario = Scenario::new(&[RemoteSpec {
        name: "crypt_one",
        backing: "one",
        password2: true,
    }])?;
    scenario.assert_roundtrip()
}

#[test]
#[ignore = "B6 real integration: requires rclone, age, and age-keygen"]
fn b6_real_multiple_crypt_remotes_and_optional_password2() -> Result<()> {
    let scenario = Scenario::new(&[
        RemoteSpec {
            name: "crypt_one",
            backing: "one",
            password2: true,
        },
        RemoteSpec {
            name: "crypt_two",
            backing: "two",
            password2: false,
        },
    ])?;
    scenario.assert_roundtrip()?;
    let restored = extract_crypt_secrets(&scenario.tools.rclone, &scenario.target_config)?.0;
    if restored.rclone.crypt["crypt_two"]
        .obscured_password2
        .is_some()
    {
        bail!("B6 restore failed to preserve the source password2 absence");
    }
    Ok(())
}

#[test]
#[ignore = "B6 real integration: requires rclone, age, and age-keygen"]
fn b6_real_wrong_age_identity_fails_before_config_mutation() -> Result<()> {
    let scenario = Scenario::new(&[RemoteSpec {
        name: "crypt_one",
        backing: "one",
        password2: true,
    }])?;
    let before = config_digest(&scenario.target_config)?;
    let wrong = scenario.wrong_decrypt();
    if scenario.restore(&scenario.vault, &wrong).is_ok() {
        bail!("B6 wrong identity unexpectedly decrypted the vault");
    }
    if config_digest(&scenario.target_config)? != before {
        bail!("B6 wrong identity changed rclone config");
    }
    Ok(())
}

#[test]
#[ignore = "B6 real integration: requires rclone, age, and age-keygen"]
fn b6_real_missing_age_identity_fails_before_config_mutation() -> Result<()> {
    let scenario = Scenario::new(&[RemoteSpec {
        name: "crypt_one",
        backing: "one",
        password2: true,
    }])?;
    let before = config_digest(&scenario.target_config)?;
    let missing = scenario
        .artifact_root
        .parent()
        .unwrap()
        .join("identity")
        .join("missing.txt");
    let decrypt = AgeDecrypt {
        executable: &scenario.tools.age,
        identity: &missing,
        artifact_root: &scenario.artifact_root,
    };
    if scenario.restore(&scenario.vault, &decrypt).is_ok() {
        bail!("B6 missing identity unexpectedly restored the vault");
    }
    if config_digest(&scenario.target_config)? != before {
        bail!("B6 missing identity changed rclone config");
    }
    Ok(())
}

#[test]
#[ignore = "B6 real integration: requires rclone, age, and age-keygen"]
fn b6_real_corrupted_age_vault_fails_before_config_mutation() -> Result<()> {
    let scenario = Scenario::new(&[RemoteSpec {
        name: "crypt_one",
        backing: "one",
        password2: true,
    }])?;
    let corrupted = scenario.artifact_root.join("secrets").join("corrupted.age");
    let mut ciphertext = fs::read(&scenario.vault)?;
    let last = ciphertext
        .last_mut()
        .ok_or_else(|| anyhow!("B6 vault is unexpectedly empty"))?;
    *last ^= 0x01;
    fs::write(&corrupted, ciphertext)?;

    let before = config_digest(&scenario.target_config)?;
    let decrypt = scenario.decrypt();
    if scenario.restore(&corrupted, &decrypt).is_ok() {
        bail!("B6 corrupted vault unexpectedly restored");
    }
    if config_digest(&scenario.target_config)? != before {
        bail!("B6 corrupted vault changed rclone config");
    }
    Ok(())
}

#[test]
#[ignore = "B6 real integration: requires rclone, age, and age-keygen"]
fn b6_real_invalid_secret_schema_fails_before_config_mutation() -> Result<()> {
    let scenario = Scenario::new(&[RemoteSpec {
        name: "crypt_one",
        backing: "one",
        password2: true,
    }])?;
    let invalid = scenario
        .artifact_root
        .join("secrets")
        .join("invalid-schema.age");
    let encrypt = scenario.encrypt();
    encrypt.write_snapshot(
        &invalid,
        br#"{"schema_version":999,"rclone":{"crypt":{"crypt_one":{"password":"AAAAAAAAAAAAAAAAAAAAAAA"}}}}"#,
    )?;

    let before = config_digest(&scenario.target_config)?;
    let decrypt = scenario.decrypt();
    if scenario.restore(&invalid, &decrypt).is_ok() {
        bail!("B6 invalid schema unexpectedly restored");
    }
    if config_digest(&scenario.target_config)? != before {
        bail!("B6 invalid schema changed rclone config");
    }
    Ok(())
}

#[test]
#[ignore = "B6 real integration: requires rclone, age, and age-keygen"]
fn b6_real_missing_required_password_fails_before_config_mutation() -> Result<()> {
    let scenario = Scenario::new(&[RemoteSpec {
        name: "crypt_one",
        backing: "one",
        password2: true,
    }])?;
    let invalid = scenario
        .artifact_root
        .join("secrets")
        .join("missing-password.age");
    let encrypt = scenario.encrypt();
    encrypt.write_snapshot(
        &invalid,
        br#"{"schema_version":1,"rclone":{"crypt":{"crypt_one":{}}}}"#,
    )?;

    let before = config_digest(&scenario.target_config)?;
    let decrypt = scenario.decrypt();
    if scenario.restore(&invalid, &decrypt).is_ok() {
        bail!("B6 missing password unexpectedly restored");
    }
    if config_digest(&scenario.target_config)? != before {
        bail!("B6 missing password changed rclone config");
    }
    Ok(())
}

#[test]
#[ignore = "B6 real integration: requires rclone, age, and age-keygen"]
fn b6_real_portable_secret_remote_mismatch_fails_before_config_mutation() -> Result<()> {
    let scenario = Scenario::new(&[RemoteSpec {
        name: "crypt_one",
        backing: "one",
        password2: true,
    }])?;
    let before = config_digest(&scenario.target_config)?;
    let mut portable = scenario.portable.clone();
    portable.crypt_remotes[0].name = "crypt_other".to_owned();
    let decrypt = scenario.decrypt();
    let encrypt = scenario.encrypt();
    if restore_crypt_vault(
        &scenario.tools.rclone,
        &scenario.target_config,
        &portable,
        &scenario.vault,
        &decrypt,
        &encrypt,
    )
    .is_ok()
    {
        bail!("B6 remote-name mismatch unexpectedly restored");
    }
    if config_digest(&scenario.target_config)? != before {
        bail!("B6 remote-name mismatch changed rclone config");
    }
    Ok(())
}

#[test]
#[ignore = "B6 real integration: requires rclone, age, and age-keygen"]
fn b6_real_target_non_crypt_type_fails_before_restore_mutation() -> Result<()> {
    let scenario = Scenario::new(&[RemoteSpec {
        name: "crypt_one",
        backing: "one",
        password2: true,
    }])?;
    let mut config = SensitiveBytes(fs::read(&scenario.target_config)?);
    let needle = b"type = crypt";
    let replacement = b"type = local";
    let position = config
        .0
        .windows(needle.len())
        .position(|window| window == needle)
        .ok_or_else(|| anyhow!("B6 target fixture has no crypt type line"))?;
    if replacement.len() > needle.len() {
        bail!("B6 test replacement is unexpectedly larger");
    }
    config.0.splice(
        position..position + needle.len(),
        replacement.iter().copied(),
    );
    fs::write(&scenario.target_config, &config.0)?;

    let before = config_digest(&scenario.target_config)?;
    let decrypt = scenario.decrypt();
    if scenario.restore(&scenario.vault, &decrypt).is_ok() {
        bail!("B6 non-crypt target unexpectedly restored");
    }
    if config_digest(&scenario.target_config)? != before {
        bail!("B6 non-crypt target changed during failed restore");
    }
    Ok(())
}

#[test]
#[ignore = "B6 real integration: requires rclone, age, and age-keygen"]
fn b6_real_repeated_export_preserves_exact_logical_secret() -> Result<()> {
    let scenario = Scenario::new(&[
        RemoteSpec {
            name: "crypt_one",
            backing: "one",
            password2: true,
        },
        RemoteSpec {
            name: "crypt_two",
            backing: "two",
            password2: false,
        },
    ])?;
    let second_vault = scenario
        .artifact_root
        .join("secrets")
        .join("rclone-repeat.age");
    let encrypt = scenario.encrypt();
    let exported = export_crypt_secret_vault(
        &scenario.tools.rclone,
        &scenario.source_config,
        &encrypt,
        &second_vault,
    )?;
    if exported.len() != scenario.portable.crypt_remotes.len() {
        bail!("B6 repeated export changed portable crypt definition count");
    }
    for expected in &scenario.portable.crypt_remotes {
        let actual = exported
            .iter()
            .find(|remote| remote.name == expected.name)
            .ok_or_else(|| anyhow!("B6 repeated export lost a portable crypt definition"))?;
        if actual.kind != expected.kind
            || actual.remote != expected.remote
            || actual.filename_encryption != expected.filename_encryption
            || actual.directory_name_encryption != expected.directory_name_encryption
        {
            bail!("B6 repeated export changed portable crypt definitions");
        }
    }
    let decrypt = scenario.decrypt();
    let first = decrypt.read_bundle(&scenario.vault)?;
    let second = decrypt.read_bundle(&second_vault)?;
    exact_secret_equality(&scenario.original_secrets, &first)?;
    exact_secret_equality(&scenario.original_secrets, &second)?;
    exact_secret_equality(&first, &second)?;
    Ok(())
}

#[test]
#[ignore = "B6 real integration: requires rclone, age, and age-keygen"]
fn b6_real_leading_hyphen_secret_is_not_an_option() -> Result<()> {
    let tools = Tools::from_env();
    tools.preflight()?;
    let temp = tempfile::tempdir()?;
    let config = temp.path().join("config.conf");
    create_local_remote(&tools.rclone, &config)?;
    // Obscure uses a random IV. Keep synthetic output private; never log secrets.
    let mut chosen = None;
    for _ in 0..2048 {
        let raw = run_rclone(
            &tools.rclone,
            &config,
            ["obscure", "public-regression-fixture"],
        )?;
        let text = String::from_utf8(raw)?.trim().to_owned();
        if text.starts_with('-') {
            chosen = Some(text);
            break;
        }
    }
    let value = chosen.ok_or_else(|| anyhow!("could not construct leading-hyphen fixture"))?;
    let secret = CryptSecret {
        obscured_password: crate::models::sensitive::SensitiveText::new(value.clone()),
        obscured_password2: None,
    };
    let portable = PortableCryptRemote {
        name: "hyphen".into(),
        kind: "crypt".into(),
        remote: format!("{}:{}", LOCAL_REMOTE, rclone_path(temp.path())?),
        filename_encryption: "standard".into(),
        directory_name_encryption: true,
    };
    create_crypt_remote(&tools.rclone, &config, &portable, &secret, false)?;
    let mut update = rclone_command(&tools.rclone, &config);
    super::crypt_restore::configure_secret_update(&mut update, "hyphen", &secret);
    execute(&mut update, |_| Ok(()), Output::Memory(OUTPUT_LIMIT))?;
    let dump = super::crypt_secrets::read_dump(&tools.rclone, &config)?;
    if dump
        .get("hyphen")
        .and_then(|r| r.password.as_ref())
        .map(|s| s.as_str())
        != Some(value.as_str())
    {
        bail!("leading-hyphen secret was changed");
    }
    Ok(())
}
