//! Sensitive process buffers. Deliberately no Debug or Display implementations.
//! Clearing is best-effort: this is not a claim to wipe allocator copies, swap,
//! subprocess memory, or compiler-generated copies.
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct SensitiveText(String);

impl SensitiveText {
    pub(crate) fn new(value: String) -> Self { Self(value) }
    pub(crate) fn as_str(&self) -> &str { &self.0 }
}

impl Drop for SensitiveText {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.fill(0);
    }
}

pub(crate) struct SensitiveBytes(pub(crate) Vec<u8>);

impl Drop for SensitiveBytes {
    fn drop(&mut self) { self.0.fill(0); }
}
