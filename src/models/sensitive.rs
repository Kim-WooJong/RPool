//! Sensitive process buffers. Deliberately no Debug or Display implementations.
//! Clearing is best-effort: this is not a claim to wipe allocator copies, swap,
//! subprocess memory, or compiler-generated copies.
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(transparent)]
/// A secret string (e.g. an obscured crypt password) that is zeroed on drop
/// and serialized transparently as a plain JSON string.
pub(crate) struct SensitiveText(String);

impl SensitiveText {
    /// Takes ownership of `value`.
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }
    /// The secret text; callers must not log it.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl Drop for SensitiveText {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.fill(0);
    }
}

/// Secret bytes (generated keys, decrypted config) zeroed on drop.
pub(crate) struct SensitiveBytes(pub(crate) Vec<u8>);

impl Drop for SensitiveBytes {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}
