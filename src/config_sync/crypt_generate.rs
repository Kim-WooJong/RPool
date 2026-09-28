//! New-remote generation only. Restore never calls this module.
use super::crypt_secrets::read_dump;
use super::secret_process::{execute, Output};
use crate::models::secrets::{validate_obscured, validate_remote_name, CryptSecret, SecretBundle};
use crate::models::sensitive::{SensitiveBytes, SensitiveText};
use anyhow::{anyhow, bail, Result};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::Command;

pub(crate) const GENERATED_SECRET_BITS: usize = 1024;
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn encode_random(bytes: &[u8]) -> SensitiveText {
    let mut encoded = String::with_capacity((bytes.len() * 8 + 5) / 6);
    let mut bits = 0u32;
    let mut count = 0u32;
    for &byte in bytes {
        bits = (bits << 8) | u32::from(byte);
        count += 8;
        while count >= 6 {
            count -= 6;
            encoded.push(BASE64[((bits >> count) & 63) as usize] as char);
        }
        bits &= (1u32 << count) - 1;
    }
    if count != 0 {
        encoded.push(BASE64[((bits << (6 - count)) & 63) as usize] as char);
    }
    SensitiveText::new(encoded)
}

fn generate_one(executable: &Path) -> Result<SensitiveText> {
    let mut random = SensitiveBytes(vec![0; GENERATED_SECRET_BITS / 8]);
    getrandom::fill(&mut random.0).map_err(|_| anyhow!("OS random generator failed"))?;
    let generated = encode_random(&random.0);
    let mut command = Command::new(executable);
    for (key, _) in std::env::vars_os() {
        let upper = key.to_string_lossy().to_ascii_uppercase();
        if upper.starts_with("RCLONE_") || upper.starts_with("_RCLONE_") {
            command.env_remove(key);
        }
    }
    command.args(["--log-level", "ERROR", "--log-file", "", "obscure", "-"]);
    let raw = execute(
        &mut command,
        |stdin| {
            stdin
                .write_all(generated.as_str().as_bytes())
                .map_err(|_| anyhow!("cannot send generated secret to rclone"))
        },
        Output::Memory(16384),
    )?;
    let value = std::str::from_utf8(&raw.0)
        .map_err(|_| anyhow!("invalid obscure response"))?
        .trim_end_matches(|c| c == '\r' || c == '\n');
    validate_obscured(value)?;
    Ok(SensitiveText::new(value.to_owned()))
}

/// Does not create remotes or rotate keys. Any future provisioning path must
/// additionally verify that the destination has no existing crypt data and must
/// vault the generated keys before creating the remote.
pub(crate) fn generate_new_remote_secrets(
    executable: &Path,
    config: &Path,
    names: &[String],
) -> Result<SecretBundle> {
    let existing = read_dump(executable, config)?;
    let mut secrets = BTreeMap::new();
    for name in names {
        validate_remote_name(name)?;
        if existing.contains_key(name) || secrets.contains_key(name) {
            bail!("refusing crypt key replacement or duplicate remote");
        }
        let password = generate_one(executable)?;
        let password2 = generate_one(executable)?;
        secrets.insert(
            name.clone(),
            CryptSecret {
                obscured_password: password,
                obscured_password2: Some(password2),
            },
        );
    }
    let bundle = SecretBundle::new(secrets);
    bundle.validate()?;
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generation_representation_is_171_characters_for_1024_bits() {
        let encoded = encode_random(&[0u8; 128]);
        assert_eq!(encoded.as_str().len(), 171);
        assert!(!encoded.as_str().contains('='));
    }
    #[test]
    fn encoding_matches_known_base64url_vectors() {
        assert!(encode_random(b"f").as_str() == "Zg");
        assert!(encode_random(b"fo").as_str() == "Zm8");
        assert!(encode_random(b"foo").as_str() == "Zm9v");
    }
}
