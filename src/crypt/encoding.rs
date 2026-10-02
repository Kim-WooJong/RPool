//! Name encodings used by rclone crypt (`filename_encoding`).
use anyhow::{bail, Result};

/// base64 RawURL, also used by `obscure`.
pub(super) use data_encoding::BASE64URL_NOPAD as BASE64_URL_NOPAD;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Encoding of encrypted name bytes into a path segment (`filename_encoding`).
/// Parsed by `CryptConfig::from_section` and stored in `Cipher`.
pub(crate) enum NameEncoding {
    /// rclone default: base32hex, lower case, no padding.
    Base32,
    /// base64 RawURL (case-sensitive remotes only).
    Base64,
    /// base32768: about a quarter of the characters of base32, for remotes
    /// that limit path length in characters (Windows servers, OneDrive).
    Base32768,
}

impl NameEncoding {
    /// Encode encrypted name bytes as text; used by `Cipher::encrypt_segment`.
    pub(super) fn encode(self, bytes: &[u8]) -> String {
        match self {
            Self::Base32 => data_encoding::BASE32HEX_NOPAD
                .encode(bytes)
                .to_ascii_lowercase(),
            Self::Base64 => BASE64_URL_NOPAD.encode(bytes),
            Self::Base32768 => super::base32768::encode(bytes),
        }
    }

    /// Decode a name segment back to bytes; errors on malformed input. Used by
    /// `Cipher::decrypt_segment`.
    pub(super) fn decode(self, text: &str) -> Result<Vec<u8>> {
        match self {
            Self::Base32 => {
                // rclone rejects padded input and decodes case-insensitively.
                if text.ends_with('=') {
                    bail!("bad base32 filename encoding");
                }
                data_encoding::BASE32HEX_NOPAD
                    .decode(text.to_ascii_uppercase().as_bytes())
                    .map_err(|_| anyhow::anyhow!("bad base32 filename encoding"))
            }
            Self::Base64 => BASE64_URL_NOPAD
                .decode(text.as_bytes())
                .map_err(|_| anyhow::anyhow!("bad base64 filename encoding")),
            Self::Base32768 => super::base32768::decode(text)
                .ok_or_else(|| anyhow::anyhow!("bad base32768 filename encoding")),
        }
    }
}
