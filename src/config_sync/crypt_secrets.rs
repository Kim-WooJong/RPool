//! Reads crypt remote settings and obscured passwords from rclone's
//! `config dump` without keeping any other credential fields. Used by export
//! (extract the secret bundle) and by `crypt_restore` (compare and verify).
#[cfg(test)]
use super::age_vault::AgeEncrypt;
use super::secret_process::{execute, rclone_command, Output, MAX_SECRET_BYTES};
use crate::models::secrets::{unique_map, CryptSecret, SecretBundle};
use crate::models::sensitive::SensitiveText;
use crate::models::PortableCryptRemote;
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// This is deliberately not serde_json::Value. Unknown credential fields are
/// skipped by serde and are never stored in a general-purpose dump model.
#[derive(Deserialize)]
pub(super) struct DumpRemote {
    #[serde(rename = "type", default)]
    /// Backend type (`crypt`, `drive`, ...).
    pub(super) kind: String,
    #[serde(default)]
    /// Wrapped remote of a crypt remote (`provider:path`).
    pub(super) remote: String,
    #[serde(default = "standard")]
    /// rclone crypt `filename_encryption`; rclone's default `standard` when absent.
    pub(super) filename_encryption: String,
    #[serde(default = "true_string")]
    /// rclone crypt `directory_name_encryption` as text; default `true`.
    pub(super) directory_name_encryption: String,
    #[serde(default)]
    /// rclone crypt `no_data_encryption` as text; empty means false.
    pub(super) no_data_encryption: String,
    /// Empty when rclone's default (`base32`) applies.
    #[serde(default)]
    pub(super) filename_encoding: String,
    #[serde(default)]
    /// Obscured crypt password.
    pub(super) password: Option<SensitiveText>,
    #[serde(default)]
    /// Obscured crypt salt; may be absent or empty.
    pub(super) password2: Option<SensitiveText>,
}
/// serde default for `filename_encryption`.
fn standard() -> String {
    "standard".into()
}
/// serde default for `directory_name_encryption`.
fn true_string() -> String {
    "true".into()
}
#[derive(Deserialize)]
#[serde(transparent)]
/// Whole `config dump` object; duplicate remote names are rejected.
struct Dump(#[serde(deserialize_with = "unique_map")] BTreeMap<String, DumpRemote>);

/// Parses `config dump` JSON; the error never echoes the input.
pub(super) fn parse_dump(raw: &[u8]) -> Result<BTreeMap<String, DumpRemote>> {
    let dump: Dump =
        serde_json::from_slice(raw).map_err(|_| anyhow!("invalid rclone config response"))?;
    Ok(dump.0)
}

/// Runs `rclone config dump` against `config` and parses the result.
pub(super) fn read_dump(executable: &Path, config: &Path) -> Result<BTreeMap<String, DumpRemote>> {
    let mut command = rclone_command(executable, config);
    command.args(["config", "dump"]);
    let raw = execute(&mut command, |_| Ok(()), Output::Memory(MAX_SECRET_BYTES))?;
    parse_dump(&raw.0)
}

/// rclone leaves `filename_encoding` out for its default.
pub(super) fn encoding_setting(value: &str) -> String {
    if value.is_empty() {
        crate::config_sync::provision::DEFAULT_FILENAME_ENCODING.into()
    } else {
        value.into()
    }
}

/// Parses an rclone boolean option (`true`/`1`, `false`/`0`, empty = `default`).
pub(super) fn bool_setting(value: &str, default: bool) -> Result<bool> {
    match value {
        "" => Ok(default),
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => bail!("unsupported crypt boolean option"),
    }
}

/// Collects every crypt remote of `config`: its portable structure and its
/// obscured passwords. Refuses crypt remotes without data encryption or password.
/// Called by `config_sync::export`.
pub(crate) fn extract_crypt_secrets(
    executable: &Path,
    config: &Path,
) -> Result<(SecretBundle, Vec<PortableCryptRemote>)> {
    let mut crypt = BTreeMap::new();
    let mut portable = Vec::new();
    for (name, remote) in read_dump(executable, config)? {
        if remote.kind != "crypt" {
            continue;
        }
        if bool_setting(&remote.no_data_encryption, false)? {
            bail!("unencrypted crypt content is not supported");
        }
        let password = remote
            .password
            .ok_or_else(|| anyhow!("crypt remote has no password"))?;
        let password2 = remote.password2.filter(|value| !value.as_str().is_empty());
        let definition = PortableCryptRemote {
            name: name.clone(),
            kind: "crypt".into(),
            remote: remote.remote,
            filename_encryption: remote.filename_encryption,
            directory_name_encryption: bool_setting(&remote.directory_name_encryption, true)?,
            filename_encoding: encoding_setting(&remote.filename_encoding),
        };
        definition.validate_structure()?;
        portable.push(definition);
        crypt.insert(
            name,
            CryptSecret {
                obscured_password: password,
                obscured_password2: password2,
            },
        );
    }
    let bundle = SecretBundle::new(crypt);
    bundle.validate()?;
    Ok((bundle, portable))
}

#[cfg(test)]
pub(crate) fn export_crypt_secret_vault(
    executable: &Path,
    config: &Path,
    age: &AgeEncrypt<'_>,
    output: &Path,
) -> Result<Vec<PortableCryptRemote>> {
    let (secrets, portable) = extract_crypt_secrets(executable, config)?;
    age.write_bundle(output, &secrets)?;
    Ok(portable)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dump_parser_skips_other_backend_credentials() {
        let raw = br#"{"cloud":{"type":"drive","token":{"secret":"fictional-token"}},"crypt":{"type":"crypt","password":"AAAAAAAAAAAAAAAAAAAAAAA"}}"#;
        let parsed = parse_dump(raw).expect("fixture must parse");
        assert!(parsed["cloud"].password.is_none());
        assert!(parsed["crypt"].password.is_some());
    }
    #[test]
    fn dump_parser_rejects_duplicate_remote_names() {
        assert!(parse_dump(br#"{"crypt":{"type":"crypt"},"crypt":{"type":"crypt"}}"#).is_err());
    }
    #[test]
    fn dump_errors_do_not_echo_bad_input() {
        let err = parse_dump(br#"{"crypt":{"password": ["fictional-sensitive-marker"]}}"#)
            .err()
            .expect("expected failure");
        assert!(!err.to_string().contains("fictional-sensitive-marker"));
    }
}
