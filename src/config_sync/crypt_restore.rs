use super::age_vault::{AgeDecrypt, AgeEncrypt};
use super::crypt_secrets::{bool_setting, read_dump, DumpRemote};
use super::secret_process::{execute, rclone_command, Output};
use super::transaction::{self, ConfigDriver, TransactionOutcome};
use crate::models::secrets::SecretBundle;
use crate::models::{PortableConfig, PortableCryptRemote};
use anyhow::{anyhow, bail, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) fn validate_matching_names(
    portable: &[PortableCryptRemote],
    secrets: &SecretBundle,
) -> Result<()> {
    secrets.validate()?;
    let mut names = BTreeSet::new();
    for remote in portable {
        remote.validate_structure()?;
        if remote.kind != "crypt" || !names.insert(remote.name.as_str()) {
            bail!("invalid or duplicate portable crypt remote");
        }
        if remote.remote.is_empty() || remote.remote.chars().any(char::is_control) {
            bail!("invalid crypt backing remote");
        }
    }
    if !names
        .iter()
        .copied()
        .eq(secrets.rclone.crypt.keys().map(String::as_str))
    {
        bail!("portable config and secret remote names do not match");
    }
    Ok(())
}

fn validate_target(
    portable: &[PortableCryptRemote],
    current: &BTreeMap<String, DumpRemote>,
) -> Result<()> {
    for expected in portable {
        let remote = current
            .get(&expected.name)
            .ok_or_else(|| anyhow!("target crypt remote is missing"))?;
        if remote.kind != "crypt" {
            bail!("target remote is not crypt");
        }
        if remote.remote != expected.remote
            || remote.filename_encryption != expected.filename_encryption
            || bool_setting(&remote.directory_name_encryption, true)?
                != expected.directory_name_encryption
            || bool_setting(&remote.no_data_encryption, false)?
            || super::crypt_secrets::encoding_setting(&remote.filename_encoding)
                != expected.filename_encoding
        {
            bail!("target crypt structure does not match portable config");
        }
    }
    Ok(())
}

fn exact_secrets(current: &BTreeMap<String, DumpRemote>, secrets: &SecretBundle) -> bool {
    secrets.rclone.crypt.iter().all(|(name, expected)| {
        current
            .get(name)
            .map(|remote| {
                remote.password.as_ref().map(|s| s.as_str().as_bytes())
                    == Some(expected.obscured_password.as_str().as_bytes())
                    && remote
                        .password2
                        .as_ref()
                        .map(|s| s.as_str())
                        .filter(|s| !s.is_empty())
                        == expected.obscured_password2.as_ref().map(|s| s.as_str())
            })
            .unwrap_or(false)
    })
}

struct RcloneDriver<'a> {
    executable: &'a Path,
    portable: &'a [PortableCryptRemote],
    secrets: &'a SecretBundle,
}

impl ConfigDriver for RcloneDriver<'_> {
    fn preflight(&self, config: &Path) -> Result<bool> {
        // `config dump` is read-only and works for both plaintext and encrypted
        // rclone.conf. Successful parsing also proves an encrypted config was
        // unlocked before any mutation is attempted.
        let current = read_dump(self.executable, config)?;
        validate_target(self.portable, &current)?;
        Ok(exact_secrets(&current, self.secrets))
    }

    fn apply_to_encrypted_stage(&self, config: &Path) -> Result<()> {
        // The transaction owns an encrypted staging file, never the live config.
        validate_target(self.portable, &read_dump(self.executable, config)?)?;
        for (name, secret) in &self.secrets.rclone.crypt {
            let mut command = rclone_command(self.executable, config);
            configure_secret_update(&mut command, name, secret);
            // Obscured CLI arguments remain visible to sufficiently privileged OS
            // process inspection. They must never enter app logs or task history.
            execute(&mut command, |_| Ok(()), Output::Memory(1024 * 1024))?;
        }
        self.verify(config)
    }

    fn build_plaintext_candidate(
        &self,
        original: &[u8],
    ) -> Result<crate::models::sensitive::SensitiveBytes> {
        // rclone Save() creates plaintext sibling temp/backup files for a
        // plaintext config. The B5 plaintext path therefore performs the two
        // allowlisted key edits in RAM and relies on rclone for pre/post-read
        // validation instead of invoking its persistent writer.
        super::plaintext_config::apply_crypt_secrets(original, self.secrets)
    }

    fn verify(&self, config: &Path) -> Result<()> {
        let current = read_dump(self.executable, config)?;
        validate_target(self.portable, &current)?;
        if !exact_secrets(&current, self.secrets) {
            bail!("restored obscured values do not match");
        }
        Ok(())
    }
}

/// B7 dry-run/preflight entry point. Decrypts and validates the vault and target
/// structure, but does not create recovery material or mutate rclone.conf.
pub(crate) fn preflight_crypt_vault(
    executable: &Path,
    config: &Path,
    portable: &PortableConfig,
    vault: &Path,
    decrypt: &AgeDecrypt<'_>,
) -> Result<bool> {
    super::validate_bundle(portable)?;
    let secrets = decrypt.read_bundle(vault)?;
    validate_matching_names(&portable.crypt_remotes, &secrets)?;
    let driver = RcloneDriver {
        executable,
        portable: &portable.crypt_remotes,
        secrets: &secrets,
    };
    driver.preflight(config)
}

/// The only non-test restore entry point. Validation/decryption precede all config
/// mutations, and every update goes through the B5 staging transaction.
pub(crate) fn restore_crypt_vault(
    executable: &Path,
    config: &Path,
    portable: &PortableConfig,
    vault: &Path,
    decrypt: &AgeDecrypt<'_>,
    snapshot_encrypt: &AgeEncrypt<'_>,
) -> Result<TransactionOutcome> {
    super::validate_bundle(portable)?;
    let secrets = decrypt.read_bundle(vault)?;
    validate_matching_names(&portable.crypt_remotes, &secrets)?;
    let driver = RcloneDriver {
        executable,
        portable: &portable.crypt_remotes,
        secrets: &secrets,
    };
    let snapshot = transaction::AgeSnapshot {
        encrypt: snapshot_encrypt,
        decrypt,
    };
    transaction::run(config, &driver, &snapshot)
}

// Obscured values are base64url and can start with '-'. End option parsing before
// any remote/key/value, preserving exact values instead of rotating or escaping keys.
pub(super) fn configure_secret_update(
    command: &mut std::process::Command,
    name: &str,
    secret: &crate::models::secrets::CryptSecret,
) {
    command
        .args([
            "config",
            "update",
            "--no-obscure",
            "--non-interactive",
            "--no-output",
            "--",
        ])
        .arg(name)
        .arg("password")
        .arg(secret.obscured_password.as_str())
        .arg("password2")
        .arg(
            secret
                .obscured_password2
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or(""),
        );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::secrets::CryptSecret;
    use crate::models::sensitive::SensitiveText;
    fn portable(name: &str) -> PortableCryptRemote {
        PortableCryptRemote {
            name: name.into(),
            kind: "crypt".into(),
            remote: "cloud:rpool".into(),
            filename_encryption: "standard".into(),
            directory_name_encryption: true,
            filename_encoding: "base32".into(),
        }
    }
    fn secrets(name: &str) -> SecretBundle {
        SecretBundle::new(BTreeMap::from([(
            name.into(),
            CryptSecret {
                obscured_password: SensitiveText::new("AAAAAAAAAAAAAAAAAAAAAAA".into()),
                obscured_password2: None,
            },
        )]))
    }
    #[test]
    fn portable_and_secret_remote_names_must_match() {
        assert!(validate_matching_names(&[portable("one")], &secrets("two")).is_err());
    }
    #[test]
    fn portable_duplicate_remote_names_are_rejected() {
        assert!(
            validate_matching_names(&[portable("one"), portable("one")], &secrets("one")).is_err()
        );
    }
    #[test]
    fn target_type_must_be_crypt_before_restore() {
        let current =
            super::super::crypt_secrets::parse_dump(br#"{"one":{"type":"drive"}}"#).unwrap();
        assert!(validate_target(&[portable("one")], &current).is_err());
    }
    #[test]
    fn different_backing_root_is_rejected() {
        let current = super::super::crypt_secrets::parse_dump(
            br#"{"one":{"type":"crypt","remote":"other:rpool"}}"#,
        )
        .unwrap();
        assert!(validate_target(&[portable("one")], &current).is_err());
    }
    #[test]
    fn missing_password2_matches_empty_but_not_an_existing_salt() {
        let empty = super::super::crypt_secrets::parse_dump(
            br#"{"one":{"type":"crypt","password":"AAAAAAAAAAAAAAAAAAAAAAA","password2":""}}"#,
        )
        .unwrap();
        let nonempty = super::super::crypt_secrets::parse_dump(br#"{"one":{"type":"crypt","password":"AAAAAAAAAAAAAAAAAAAAAAA","password2":"BBBBBBBBBBBBBBBBBBBBBBB"}}"#).unwrap();
        assert!(exact_secrets(&empty, &secrets("one")));
        assert!(!exact_secrets(&nonempty, &secrets("one")));
    }
    #[test]
    fn exact_obscured_comparison_rejects_any_changed_byte() {
        let changed = super::super::crypt_secrets::parse_dump(
            br#"{"one":{"type":"crypt","password":"AAAAAAAAAAAAAAAAAAAAAAB"}}"#,
        )
        .unwrap();
        assert!(!exact_secrets(&changed, &secrets("one")));
    }
}
