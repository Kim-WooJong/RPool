//! Shard size conversion and validation shared by CLI, pool store, GUI and mount code.

use crate::config::constants::MAX_SHARD_MIB;
use anyhow::{bail, Result};
use std::num::NonZeroU64;

pub(crate) const MIB: u64 = 1024 * 1024;

/// Converts a MiB shard size to bytes, rejecting 0, overflow and values above
/// [`MAX_SHARD_MIB`]. This is the only place MiB→bytes shard conversion happens.
pub(crate) fn shard_bytes(mib: u64) -> Result<NonZeroU64> {
    validate_shard_mib(mib)?;
    let bytes = mib
        .checked_mul(MIB)
        .and_then(NonZeroU64::new)
        .ok_or_else(|| anyhow::anyhow!("shard size overflow"))?;
    Ok(bytes)
}

/// Validates a MiB shard size against the shared 1..=MAX_SHARD_MIB range.
pub(crate) fn validate_shard_mib(mib: u64) -> Result<()> {
    if mib == 0 {
        bail!("shard size must be greater than zero MiB");
    }
    if mib > MAX_SHARD_MIB {
        bail!("shard size {mib} MiB exceeds the maximum of {MAX_SHARD_MIB} MiB");
    }
    Ok(())
}

/// rclone crypt file header: 8-byte magic + 24-byte nonce.
const CRYPT_HEADER: u64 = 32;
/// rclone crypt plaintext block size.
const CRYPT_BLOCK_DATA: u64 = 64 * 1024;
/// Poly1305 tag added to every (including a trailing partial) crypt block.
const CRYPT_BLOCK_TAG: u64 = 16;

/// Size of the object a crypt remote stores for `plain` bytes (rclone `EncryptedSize`).
pub(crate) fn crypt_size(plain: u64) -> u64 {
    let blocks = plain / CRYPT_BLOCK_DATA;
    let residue = plain % CRYPT_BLOCK_DATA;
    let mut size = CRYPT_HEADER + blocks * (CRYPT_BLOCK_TAG + CRYPT_BLOCK_DATA);
    if residue != 0 {
        size += CRYPT_BLOCK_TAG + residue;
    }
    size
}

/// Largest whole-MiB shard whose crypt object fits in `limit` bytes (0 if none fits).
pub(crate) fn max_shard_mib_for_object_limit(limit: u64) -> u64 {
    let per_mib = (MIB / CRYPT_BLOCK_DATA) * (CRYPT_BLOCK_TAG + CRYPT_BLOCK_DATA);
    limit.saturating_sub(CRYPT_HEADER) / per_mib
}

/// Rejects shard sizes whose encrypted provider object would exceed `max_object_bytes`.
/// Data and parity shards share the same plaintext size, so one check covers both.
pub(crate) fn check_object_limit(shard_bytes: u64, max_object_bytes: Option<u64>) -> Result<()> {
    let Some(limit) = max_object_bytes else {
        return Ok(());
    };
    if limit == 0 {
        bail!("max_object_bytes must be greater than zero");
    }
    let object = crypt_size(shard_bytes);
    if object > limit {
        bail!(
            "encrypted shard object is {object} bytes ({shard_bytes} plaintext), exceeding max_object_bytes {limit}; use a shard size of at most {} MiB",
            max_shard_mib_for_object_limit(limit)
        );
    }
    Ok(())
}

/// In-memory plaintext shard size, stored in bytes.
///
/// Serde compatibility: MiB-aligned sizes serialize as a bare integer MiB value (the
/// historical `shard_mib` field), so existing pool stores, virtual-drive state and
/// older binaries keep working. Deserialization accepts that integer (MiB) or a
/// string such as `"64MiB"`, `"64M"`, `"64 MiB"` or `"67108864B"`. Non-aligned sizes
/// (not producible through current inputs) serialize as a `"<bytes>B"` string.
/// Upper-bound validation lives in [`ShardSize::validate`] / `validate_pool`, not in
/// deserialization, so out-of-range stored values surface as a clear validation error
/// instead of making the whole store unreadable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ShardSize(NonZeroU64);

impl ShardSize {
    /// Validated constructor from MiB (1..=MAX_SHARD_MIB).
    pub(crate) fn from_mib(mib: u64) -> Result<Self> {
        shard_bytes(mib).map(Self)
    }

    /// Unbounded (but non-zero, overflow-checked) constructor used by deserialization.
    fn from_mib_unbounded(mib: u64) -> Result<Self> {
        mib.checked_mul(MIB)
            .and_then(NonZeroU64::new)
            .map(Self)
            .ok_or_else(|| {
                anyhow::anyhow!("shard size must be a non-zero MiB value without overflow")
            })
    }

    pub(crate) fn from_bytes(bytes: u64) -> Result<Self> {
        NonZeroU64::new(bytes)
            .map(Self)
            .ok_or_else(|| anyhow::anyhow!("shard size must be greater than zero"))
    }

    pub(crate) fn bytes(self) -> u64 {
        self.0.get()
    }

    /// Exact MiB value, if MiB aligned.
    pub(crate) fn exact_mib(self) -> Option<u64> {
        self.bytes().is_multiple_of(MIB).then(|| self.bytes() / MIB)
    }

    /// MiB value rounded up (for numeric UI fields).
    pub(crate) fn mib_ceil(self) -> u64 {
        self.bytes().div_ceil(MIB)
    }

    /// Checks the shared 1..=MAX_SHARD_MIB bound (MiB-aligned sizes only).
    pub(crate) fn validate(self) -> Result<NonZeroU64> {
        match self.exact_mib() {
            Some(mib) => shard_bytes(mib),
            None => bail!(
                "shard size {} bytes is not a whole number of MiB",
                self.bytes()
            ),
        }
    }

    fn parse(text: &str) -> Result<Self> {
        let t = text.trim();
        let split = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
        let (num, unit) = t.split_at(split);
        let value: u64 = num
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid shard size: {text:?}"))?;
        match unit.trim().to_ascii_lowercase().as_str() {
            "" | "mib" | "m" => Self::from_mib_unbounded(value),
            "b" => Self::from_bytes(value),
            _ => bail!("invalid shard size unit in {text:?}; use MiB, M or B"),
        }
    }
}

impl Default for ShardSize {
    fn default() -> Self {
        Self::from_mib(crate::config::constants::DEFAULT_SHARD_MIB)
            .expect("default shard size is valid")
    }
}

impl std::fmt::Display for ShardSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.exact_mib() {
            Some(mib) => write!(f, "{mib}"),
            None => write!(f, "{}B", self.bytes()),
        }
    }
}

impl serde::Serialize for ShardSize {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.exact_mib() {
            Some(mib) => serializer.serialize_u64(mib),
            None => serializer.serialize_str(&format!("{}B", self.bytes())),
        }
    }
}

impl<'de> serde::Deserialize<'de> for ShardSize {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Mib(u64),
            Text(String),
        }
        let result = match Raw::deserialize(deserializer)? {
            Raw::Mib(mib) => Self::from_mib_unbounded(mib),
            Raw::Text(text) => Self::parse(&text),
        };
        result.map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_bytes_converts_and_bounds() {
        assert_eq!(shard_bytes(64).unwrap().get(), 67_108_864);
        assert_eq!(shard_bytes(220).unwrap().get(), 230_686_720);
        assert_eq!(
            shard_bytes(MAX_SHARD_MIB).unwrap().get(),
            MAX_SHARD_MIB * MIB
        );
        assert!(shard_bytes(0).is_err());
        assert!(shard_bytes(MAX_SHARD_MIB + 1).is_err());
        assert!(shard_bytes(u64::MAX).is_err());
    }

    #[test]
    fn crypt_size_matches_rclone_framing() {
        assert_eq!(crypt_size(0), 32);
        assert_eq!(crypt_size(1), 32 + 16 + 1);
        assert_eq!(crypt_size(CRYPT_BLOCK_DATA), 32 + 16 + CRYPT_BLOCK_DATA);
        assert_eq!(
            crypt_size(CRYPT_BLOCK_DATA + 1),
            32 + 2 * 16 + CRYPT_BLOCK_DATA + 1
        );
        assert_eq!(crypt_size(64 * MIB), 67_125_280);
        assert_eq!(crypt_size(220 * MIB), 230_743_072);
    }

    #[test]
    fn object_limit_rejects_oversized_encrypted_shards() {
        // Box Free style 250 MB (decimal) and 250 MiB limits both accept 64 and 220 MiB.
        for limit in [250_000_000u64, 250 * MIB] {
            check_object_limit(64 * MIB, Some(limit)).unwrap();
            check_object_limit(220 * MIB, Some(limit)).unwrap();
        }
        check_object_limit(4096 * MIB, None).unwrap();
        // Exact boundary: the encrypted size itself fits, one byte less does not.
        let exact = crypt_size(64 * MIB);
        check_object_limit(64 * MIB, Some(exact)).unwrap();
        let error = check_object_limit(64 * MIB, Some(exact - 1))
            .unwrap_err()
            .to_string();
        assert!(error.contains("at most 63 MiB"), "{error}");
        assert!(check_object_limit(MIB, Some(0)).is_err());
        assert_eq!(max_shard_mib_for_object_limit(250_000_000), 238);
        assert_eq!(max_shard_mib_for_object_limit(exact), 64);
        assert_eq!(max_shard_mib_for_object_limit(10), 0);
        for limit in [250_000_000u64, exact, 5 * 1024 * MIB] {
            let mib = max_shard_mib_for_object_limit(limit);
            assert!(crypt_size(mib * MIB) <= limit);
            assert!(crypt_size((mib + 1) * MIB) > limit);
        }
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct Holder {
        #[serde(rename = "shard_mib", alias = "shard_size")]
        shard: ShardSize,
    }

    #[test]
    fn shard_size_serde_is_backward_compatible() {
        for mib in [1u64, 64, 220, MAX_SHARD_MIB] {
            let old = format!("{{\"shard_mib\":{mib}}}");
            let parsed: Holder = serde_json::from_str(&old).unwrap();
            assert_eq!(parsed.shard.bytes(), mib * MIB);
            assert_eq!(serde_json::to_string(&parsed).unwrap(), old);
        }
        for text in [
            "\"64MiB\"",
            "\"64M\"",
            "\"64 MiB\"",
            "\"67108864B\"",
            "\"64\"",
        ] {
            let parsed: Holder =
                serde_json::from_str(&format!("{{\"shard_size\":{text}}}")).unwrap();
            assert_eq!(parsed.shard, ShardSize::from_mib(64).unwrap(), "{text}");
            assert_eq!(
                serde_json::to_string(&parsed).unwrap(),
                "{\"shard_mib\":64}"
            );
        }
        assert!(serde_json::from_str::<Holder>("{\"shard_mib\":0}").is_err());
        assert!(serde_json::from_str::<Holder>("{\"shard_mib\":18446744073709551615}").is_err());
        assert!(serde_json::from_str::<Holder>("{\"shard_size\":\"64GB\"}").is_err());
        let odd = ShardSize::from_bytes(MIB + 1).unwrap();
        let json = serde_json::to_string(&Holder { shard: odd }).unwrap();
        assert_eq!(json, "{\"shard_mib\":\"1048577B\"}");
        assert_eq!(serde_json::from_str::<Holder>(&json).unwrap().shard, odd);
        assert!(odd.validate().is_err());
        assert!(ShardSize::from_mib(MAX_SHARD_MIB + 1).is_err());
        assert_eq!(ShardSize::default().bytes(), 67_108_864);
    }
}
