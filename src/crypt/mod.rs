//! Byte-compatible implementation of rclone's `crypt` backend format
//! (rclone `backend/crypt/cipher.go`, `fs/config/obscure`). Keys, obscured
//! passwords, name encryption and the streaming data format live here; the
//! storage backend that uses them is milestone M2 of
//! `docs/NATIVE_MOUNT_CRYPT_PLAN.md`.
//!
//! Unsupported rclone settings (`no_data_encryption`, `pass_bad_blocks`,
//! unknown modes/encodings/options) are refused by
//! [`options::CryptConfig`]. Version-suffixed names (`--b2-versions`) are not handled.
#![cfg_attr(
    not(test),
    expect(dead_code, reason = "native crypt backend (M2) not wired yet")
)]

mod base32768;
pub(crate) mod cipher;
pub(crate) mod data;
mod eme;
mod encoding;
mod names;
pub(crate) mod obscure;
pub(crate) mod options;

#[cfg(test)]
mod oracle_tests;
#[cfg(test)]
mod tests;
