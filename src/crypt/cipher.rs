//! rclone crypt key material (`backend/crypt/cipher.go` `Cipher.Key`).
use super::encoding::NameEncoding;
use super::options::{CryptConfig, NameMode};
use aes::cipher::KeyInit;
use aes::Aes256;
use anyhow::{anyhow, Result};
use zeroize::Zeroizing;

/// rclone's default salt, used when `password2` is empty.
const DEFAULT_SALT: [u8; 16] = [
    0xA8, 0x0D, 0xF4, 0x3A, 0x8F, 0xBD, 0x03, 0x08, 0xA7, 0xCA, 0xB8, 0x3E, 0x58, 0x1F, 0x86, 0xB1,
];
/// Length of the XSalsa20-Poly1305 data key (first part of the scrypt output).
const DATA_KEY: usize = 32;
/// Length of the AES-256 name key used by EME (second part).
const NAME_KEY: usize = 32;
/// Length of the EME name tweak (last part).
const NAME_TWEAK: usize = 16;

/// Keys and name settings of one crypt remote. Key bytes are zeroized on drop and
/// never printed.
pub(crate) struct Cipher {
    /// Key for file content secretboxes (`crypt::data`).
    pub(super) data_key: Zeroizing<[u8; DATA_KEY]>,
    /// AES-256 key for name encryption; its byte sum also seeds obfuscation.
    pub(super) name_key: Zeroizing<[u8; NAME_KEY]>,
    /// EME tweak for standard name encryption.
    pub(super) name_tweak: [u8; NAME_TWEAK],
    /// AES-256 block cipher keyed with `name_key`, prebuilt for EME.
    pub(super) name_block: Aes256,
    /// `filename_encryption` mode.
    pub(super) name_mode: NameMode,
    /// Whether directory segments are encrypted too (`directory_name_encryption`).
    pub(super) directory_name_encryption: bool,
    /// Text encoding of encrypted names (`filename_encoding`).
    pub(super) name_encoding: NameEncoding,
    /// Suffix appended to file names when `name_mode` is `Off`; empty for `suffix = none`.
    pub(super) suffix: String,
}

impl std::fmt::Debug for Cipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cipher")
            .field("name_mode", &self.name_mode)
            .field("name_encoding", &self.name_encoding)
            .finish_non_exhaustive()
    }
}

impl Cipher {
    /// Derive keys from the crypt remote's password and salt with scrypt
    /// (N=2^14, r=8, p=1), as rclone does. Called by `storage::native_crypt::route`
    /// when opening a crypt remote natively.
    pub(crate) fn new(config: &CryptConfig) -> Result<Self> {
        let mut key = Zeroizing::new([0u8; DATA_KEY + NAME_KEY + NAME_TWEAK]);
        // rclone uses an all-zero key for an empty password (only reachable in tests).
        if !config.password.is_empty() {
            let salt: &[u8] = if config.salt.is_empty() {
                &DEFAULT_SALT
            } else {
                config.salt.as_bytes()
            };
            // `len` only matters for PHC strings; `scrypt()` fills `key` whatever its size.
            let params = scrypt::Params::new(14, 8, 1, scrypt::Params::RECOMMENDED_LEN)
                .map_err(|_| anyhow!("invalid scrypt parameters"))?;
            scrypt::scrypt(config.password.as_bytes(), salt, &params, key.as_mut())
                .map_err(|_| anyhow!("scrypt key derivation failed"))?;
        }
        Ok(Self::from_key(
            &key,
            config.name_mode,
            config.directory_name_encryption,
            config.name_encoding,
            config.suffix.clone().unwrap_or_default(),
        ))
    }

    /// Build a cipher from an already-derived 80-byte key (data key, name key,
    /// name tweak in that order). Used by `new` and by tests with fixed keys.
    pub(super) fn from_key(
        key: &[u8; DATA_KEY + NAME_KEY + NAME_TWEAK],
        name_mode: NameMode,
        directory_name_encryption: bool,
        name_encoding: NameEncoding,
        suffix: String,
    ) -> Self {
        let mut data_key = Zeroizing::new([0u8; DATA_KEY]);
        let mut name_key = Zeroizing::new([0u8; NAME_KEY]);
        let mut name_tweak = [0u8; NAME_TWEAK];
        data_key.copy_from_slice(&key[..DATA_KEY]);
        name_key.copy_from_slice(&key[DATA_KEY..DATA_KEY + NAME_KEY]);
        name_tweak.copy_from_slice(&key[DATA_KEY + NAME_KEY..]);
        let name_block = Aes256::new(name_key.as_ref().into());
        Self {
            data_key,
            name_key,
            name_tweak,
            name_block,
            name_mode,
            directory_name_encryption,
            name_encoding,
            suffix,
        }
    }
}
