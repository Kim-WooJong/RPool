//! Name encodings used by rclone crypt (`filename_encoding`).
use anyhow::{bail, Result};

/// base64 RawURL, also used by `obscure`.
pub(super) use data_encoding::BASE64URL_NOPAD as BASE64_URL_NOPAD;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NameEncoding {
    /// rclone default: base32hex, lower case, no padding.
    Base32,
    /// base64 RawURL (case-sensitive remotes only).
    Base64,
}

impl NameEncoding {
    pub(super) fn encode(self, bytes: &[u8]) -> String {
        match self {
            Self::Base32 => data_encoding::BASE32HEX_NOPAD
                .encode(bytes)
                .to_ascii_lowercase(),
            Self::Base64 => BASE64_URL_NOPAD.encode(bytes),
        }
    }

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
        }
    }
}
