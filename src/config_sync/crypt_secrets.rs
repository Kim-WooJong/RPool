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
    pub(super) kind: String,
    #[serde(default)]
    pub(super) remote: String,
    #[serde(default = "standard")]
    pub(super) filename_encryption: String,
    #[serde(default = "true_string")]
    pub(super) directory_name_encryption: String,
    #[serde(default)]
    pub(super) no_data_encryption: String,
    #[serde(default)]
    pub(super) password: Option<SensitiveText>,
    #[serde(default)]
    pub(super) password2: Option<SensitiveText>,
}
fn standard() -> String { "standard".into() }
fn true_string() -> String { "true".into() }
#[derive(Deserialize)]
#[serde(transparent)]
struct Dump(#[serde(deserialize_with = "unique_map")] BTreeMap<String, DumpRemote>);

pub(super) fn parse_dump(raw: &[u8]) -> Result<BTreeMap<String, DumpRemote>> {
    let dump: Dump = serde_json::from_slice(raw).map_err(|_| anyhow!("invalid rclone config response"))?;
    Ok(dump.0)
}

pub(super) fn read_dump(executable: &Path, config: &Path) -> Result<BTreeMap<String, DumpRemote>> {
    let mut command = rclone_command(executable, config);
    command.args(["config", "dump"]);
    let raw = execute(&mut command, |_| Ok(()), Output::Memory(MAX_SECRET_BYTES))?;
    parse_dump(&raw.0)
}

pub(super) fn bool_setting(value: &str, default: bool) -> Result<bool> {
    match value {
        "" => Ok(default),
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => bail!("unsupported crypt boolean option"),
    }
}

pub(crate) fn extract_crypt_secrets(executable: &Path, config: &Path) -> Result<(SecretBundle, Vec<PortableCryptRemote>)> {
    let mut crypt = BTreeMap::new();
    let mut portable = Vec::new();
    for (name, remote) in read_dump(executable, config)? {
        if remote.kind != "crypt" { continue; }
        if bool_setting(&remote.no_data_encryption, false)? { bail!("unencrypted crypt content is not supported"); }
        let password = remote.password.ok_or_else(|| anyhow!("crypt remote has no password"))?;
        let password2 = remote.password2.filter(|value| !value.as_str().is_empty());
        let definition = PortableCryptRemote {
            name: name.clone(), kind: "crypt".into(), remote: remote.remote,
            filename_encryption: remote.filename_encryption,
            directory_name_encryption: bool_setting(&remote.directory_name_encryption, true)?,
        };
        definition.validate_structure()?;
        portable.push(definition);
        crypt.insert(name, CryptSecret { obscured_password: password, obscured_password2: password2 });
    }
    let bundle = SecretBundle::new(crypt);
    bundle.validate()?;
    Ok((bundle, portable))
}

#[cfg(test)]
pub(crate) fn export_crypt_secret_vault(executable: &Path, config: &Path, age: &AgeEncrypt<'_>, output: &Path) -> Result<Vec<PortableCryptRemote>> {
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
        let err = parse_dump(br#"{"crypt":{"password": ["fictional-sensitive-marker"]}}"#).err().expect("expected failure");
        assert!(!err.to_string().contains("fictional-sensitive-marker"));
    }
}
