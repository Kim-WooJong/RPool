//! Parsing of one rclone crypt remote section (as returned by `rclone config dump`).
//! Anything RPool cannot reproduce byte-for-byte is refused.
use super::encoding::NameEncoding;
use super::obscure::{reveal, SensitiveString};
use anyhow::{anyhow, bail, Result};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NameMode {
    Standard,
    Obfuscate,
    Off,
}

/// Options that only affect rclone's own listing/server-side behaviour and do not
/// change the stored bytes.
const IGNORED_KEYS: &[&str] = &[
    "description",
    "server_side_across_configs",
    "show_mapping",
    "strict_names",
];

#[derive(Debug)]
pub(crate) struct CryptConfig {
    /// The crypt remote's base (`remote =`), e.g. `box:archive`.
    pub(crate) remote: String,
    pub(crate) password: SensitiveString,
    /// `password2` (salt); empty means rclone's built-in default salt.
    pub(crate) salt: SensitiveString,
    pub(crate) name_mode: NameMode,
    pub(crate) directory_name_encryption: bool,
    pub(crate) name_encoding: NameEncoding,
    /// File suffix used with `filename_encryption = off` (`None` for `suffix = none`).
    pub(crate) suffix: Option<String>,
}

fn boolean(section: &BTreeMap<String, String>, key: &str, default: bool) -> Result<bool> {
    match section.get(key).map(String::as_str) {
        None => Ok(default),
        Some("true" | "1") => Ok(true),
        Some("false" | "0") => Ok(false),
        Some(_) => bail!("unsupported value for crypt option {key}"),
    }
}

impl CryptConfig {
    /// Builds a config from a crypt section. Secret values are revealed in memory
    /// only; error messages never include option values.
    pub(crate) fn from_section(section: &BTreeMap<String, String>) -> Result<Self> {
        const KNOWN: &[&str] = &[
            "type",
            "remote",
            "password",
            "password2",
            "filename_encryption",
            "directory_name_encryption",
            "filename_encoding",
            "suffix",
            "no_data_encryption",
            "pass_bad_blocks",
        ];
        for key in section.keys() {
            if !KNOWN.contains(&key.as_str()) && !IGNORED_KEYS.contains(&key.as_str()) {
                bail!("unsupported crypt option {key}");
            }
        }
        if section.get("type").map(String::as_str) != Some("crypt") {
            bail!("remote is not a crypt remote");
        }
        if boolean(section, "no_data_encryption", false)? {
            bail!("crypt no_data_encryption is not supported");
        }
        if boolean(section, "pass_bad_blocks", false)? {
            bail!("crypt pass_bad_blocks is not supported");
        }
        let remote = section
            .get("remote")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("crypt remote has no base remote"))?
            .clone();
        let password = match section.get("password").map(String::as_str) {
            None | Some("") => bail!("crypt remote has no password"),
            Some(obscured) => {
                reveal(obscured).map_err(|_| anyhow!("cannot reveal crypt password"))?
            }
        };
        let salt = match section.get("password2").map(String::as_str) {
            None | Some("") => SensitiveString::empty(),
            Some(obscured) => {
                reveal(obscured).map_err(|_| anyhow!("cannot reveal crypt password2"))?
            }
        };
        let name_mode = match section.get("filename_encryption").map(String::as_str) {
            None | Some("standard") => NameMode::Standard,
            Some("obfuscate") => NameMode::Obfuscate,
            Some("off") => NameMode::Off,
            Some(_) => bail!("unsupported crypt filename_encryption"),
        };
        let name_encoding = match section.get("filename_encoding").map(String::as_str) {
            None | Some("base32") => NameEncoding::Base32,
            Some("base64") => NameEncoding::Base64,
            Some("base32768") => NameEncoding::Base32768,
            Some(_) => bail!("unsupported crypt filename_encoding"),
        };
        let suffix = match section.get("suffix").map(String::as_str) {
            None => Some(".bin".to_string()),
            Some("none") => None,
            Some(value) if value.starts_with('.') && !value.contains('/') => Some(value.into()),
            Some(_) => bail!("unsupported crypt suffix"),
        };
        Ok(Self {
            remote,
            password,
            salt,
            name_mode,
            directory_name_encryption: boolean(section, "directory_name_encryption", true)?,
            name_encoding,
            suffix,
        })
    }
}
