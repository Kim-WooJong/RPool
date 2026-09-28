use super::sensitive::SensitiveText;
use anyhow::{bail, Result};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;

pub(crate) const SECRET_BUNDLE_SCHEMA_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CryptSecret {
    #[serde(rename = "password")]
    pub(crate) obscured_password: SensitiveText,
    #[serde(rename = "password2", default, skip_serializing_if = "Option::is_none")]
    pub(crate) obscured_password2: Option<SensitiveText>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RcloneSecrets {
    #[serde(deserialize_with = "unique_map")]
    pub(crate) crypt: BTreeMap<String, CryptSecret>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SecretBundle {
    pub(crate) schema_version: u32,
    pub(crate) rclone: RcloneSecrets,
}

impl SecretBundle {
    pub(crate) fn new(crypt: BTreeMap<String, CryptSecret>) -> Self {
        Self { schema_version: SECRET_BUNDLE_SCHEMA_VERSION, rclone: RcloneSecrets { crypt } }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.schema_version != SECRET_BUNDLE_SCHEMA_VERSION {
            bail!("unsupported crypt secret schema version");
        }
        for (name, secret) in &self.rclone.crypt {
            validate_remote_name(name)?;
            validate_obscured(secret.obscured_password.as_str())?;
            if let Some(value) = &secret.obscured_password2 {
                validate_obscured(value.as_str())?;
            }
        }
        Ok(())
    }
}

/// Syntax/provenance guard, NOT authentication of rclone's reversible encoding.
/// No decoding or recovery of crypt passwords is performed here.
pub(crate) fn validate_obscured(value: &str) -> Result<()> {
    let bytes = value.as_bytes();
    if bytes.len() < 23 || bytes.len() > 16384 || bytes.len() % 4 == 1
        || !bytes.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
    {
        bail!("invalid obscured crypt secret encoding");
    }
    Ok(())
}

/// A conservative subset of rclone names; no flags or connection strings.
pub(crate) fn validate_remote_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 128
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    {
        bail!("unsupported crypt remote name");
    }
    Ok(())
}

/// Reject duplicate remote names instead of silently keeping the last secret.
pub(crate) fn unique_map<'de, D, T>(deserializer: D) -> std::result::Result<BTreeMap<String, T>, D::Error>
where D: Deserializer<'de>, T: Deserialize<'de> {
    struct Unique<T>(PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for Unique<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("a unique-key object") }
        fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> std::result::Result<Self::Value, A::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = access.next_entry::<String, T>()? {
                if result.insert(key, value).is_some() { return Err(de::Error::custom("duplicate remote key")); }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Unique(PhantomData))
}

#[cfg(test)]
mod tests {
    use super::*;
    const ONE: &str = r#"{"schema_version":1,"rclone":{"crypt":{"one":{"password":"AAAAAAAAAAAAAAAAAAAAAAA"}}}}"#;
    #[test]
    fn optional_password2_and_wire_format() {
        let bundle: SecretBundle = serde_json::from_str(ONE).unwrap();
        assert!(bundle.validate().is_ok());
        assert!(bundle.rclone.crypt["one"].obscured_password2.is_none());
        let serialized = serde_json::to_string(&bundle).unwrap();
        assert!(serialized.contains("\"password\""));
        assert!(!serialized.contains("obscured_password"));
        assert!(!serialized.contains("password2"));
    }
    #[test]
    fn unsupported_schema_is_rejected() {
        let bundle: SecretBundle = serde_json::from_str(&ONE.replace("\"schema_version\":1", "\"schema_version\":2")).unwrap();
        assert!(bundle.validate().is_err());
    }
    #[test]
    fn missing_required_password_is_rejected() {
        assert!(serde_json::from_str::<SecretBundle>(r#"{"schema_version":1,"rclone":{"crypt":{"one":{}}}}"#).is_err());
    }
    #[test]
    fn unknown_secret_fields_are_rejected() {
        assert!(serde_json::from_str::<SecretBundle>(&ONE.replace("\"password\":", "\"unexpected\":")).is_err());
    }
    #[test]
    fn duplicate_remote_keys_are_rejected() {
        let raw = r#"{"schema_version":1,"rclone":{"crypt":{"one":{"password":"AAAAAAAAAAAAAAAAAAAAAAA"},"one":{"password":"BBBBBBBBBBBBBBBBBBBBBBB"}}}}"#;
        assert!(serde_json::from_str::<SecretBundle>(raw).is_err());
    }
    #[test]
    fn multiple_remotes_with_password2_are_supported() {
        let raw = r#"{"schema_version":1,"rclone":{"crypt":{"one":{"password":"AAAAAAAAAAAAAAAAAAAAAAA","password2":"BBBBBBBBBBBBBBBBBBBBBBB"},"two":{"password":"CCCCCCCCCCCCCCCCCCCCCCC"}}}}"#;
        let bundle: SecretBundle = serde_json::from_str(raw).unwrap();
        assert!(bundle.validate().is_ok());
        assert_eq!(bundle.rclone.crypt.len(), 2);
        assert!(bundle.rclone.crypt["one"].obscured_password2.is_some());
    }
}
