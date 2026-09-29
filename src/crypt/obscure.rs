//! rclone `fs/config/obscure`: AES-256-CTR under a fixed, public key with a
//! random IV prefix, then base64 RawURL. This is obfuscation, not encryption.
use super::encoding::BASE64_URL_NOPAD;
use aes::cipher::{KeyIvInit, StreamCipher};
use anyhow::{anyhow, bail, Result};
use std::fmt;
use zeroize::Zeroizing;

type Aes256Ctr = ctr::Ctr128BE<aes::Aes256>;

/// rclone's `cryptKey`; public by design.
const OBSCURE_KEY: [u8; 32] = [
    0x9c, 0x93, 0x5b, 0x48, 0x73, 0x0a, 0x55, 0x4d, 0x6b, 0xfd, 0x7c, 0x63, 0xc8, 0x86, 0xa9, 0x2b,
    0xd3, 0x90, 0x19, 0x8e, 0xb8, 0x12, 0x8a, 0xfb, 0xf4, 0xde, 0x16, 0x2b, 0x8b, 0x95, 0xf6, 0x38,
];
const IV_SIZE: usize = 16;

/// A revealed secret, zeroized on drop and redacted in `Debug`. Holds bytes
/// because rclone's `Reveal` returns a Go string that need not be UTF-8 and
/// scrypt consumes the raw bytes.
pub(crate) struct SensitiveString(Zeroizing<Vec<u8>>);

impl SensitiveString {
    pub(crate) fn empty() -> Self {
        Self(Zeroizing::new(Vec::new()))
    }
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
impl AsRef<[u8]> for SensitiveString {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for SensitiveString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SensitiveString(<redacted>)")
    }
}

/// `obscure.Obscure` with a fresh random IV.
pub(crate) fn obscure(plain: impl AsRef<[u8]>) -> Result<String> {
    let mut iv = [0u8; IV_SIZE];
    getrandom::fill(&mut iv).map_err(|_| anyhow!("OS random generator failed"))?;
    obscure_with_iv(plain.as_ref(), iv)
}

pub(super) fn obscure_with_iv(plain: &[u8], iv: [u8; IV_SIZE]) -> Result<String> {
    if plain.len() > i32::MAX as usize - IV_SIZE {
        bail!("value too large");
    }
    let mut buffer = Zeroizing::new(Vec::with_capacity(IV_SIZE + plain.len()));
    buffer.extend_from_slice(&iv);
    buffer.extend_from_slice(plain);
    Aes256Ctr::new(&OBSCURE_KEY.into(), &iv.into()).apply_keystream(&mut buffer[IV_SIZE..]);
    Ok(BASE64_URL_NOPAD.encode(&buffer))
}

/// `obscure.Reveal`; error texts match rclone's.
pub(crate) fn reveal(obscured: &str) -> Result<SensitiveString> {
    let mut buffer = Zeroizing::new(BASE64_URL_NOPAD.decode(obscured.as_bytes()).map_err(
        |error| anyhow!("base64 decode failed when revealing password - is it obscured?: {error}"),
    )?);
    if buffer.len() < IV_SIZE {
        bail!("input too short when revealing password - is it obscured?");
    }
    let mut iv = [0u8; IV_SIZE];
    iv.copy_from_slice(&buffer[..IV_SIZE]);
    Aes256Ctr::new(&OBSCURE_KEY.into(), &iv.into()).apply_keystream(&mut buffer[IV_SIZE..]);
    Ok(SensitiveString(Zeroizing::new(buffer[IV_SIZE..].to_vec())))
}
