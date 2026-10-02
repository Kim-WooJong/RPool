//! Hash of the bytes RPool streams to a provider, in a form the provider
//! itself reports (`rclone lsjson --hash`). Comparing the two after an upload
//! proves the provider stored exactly those bytes without reading the object
//! back: one metadata call instead of downloading the whole shard.
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256};

/// Hash kinds RPool can compute, in preference order (all are collision
/// resistant enough to detect corruption; the provider picks what it offers).
pub(crate) const SUPPORTED: [&str; 4] = ["blake3", "sha1", "md5", "dropbox"];

/// Dropbox `content_hash`: SHA-256 of the concatenated SHA-256 digests of
/// each 4 MiB block.
const DROPBOX_BLOCK: usize = 4 * 1024 * 1024;

enum State {
    Blake3(Box<blake3::Hasher>),
    Sha1(Sha1),
    Md5(Md5),
    Dropbox {
        blocks: Sha256,
        block: Sha256,
        filled: usize,
    },
}

pub(crate) struct StreamHash {
    kind: &'static str,
    state: State,
}

impl StreamHash {
    /// A hasher for the first kind in [`SUPPORTED`] the provider reports.
    pub(crate) fn for_provider(reported: &[String]) -> Option<Self> {
        let kind = SUPPORTED
            .into_iter()
            .find(|kind| reported.iter().any(|r| r.eq_ignore_ascii_case(kind)))?;
        let state = match kind {
            "blake3" => State::Blake3(Box::default()),
            "sha1" => State::Sha1(Sha1::new()),
            "md5" => State::Md5(Md5::new()),
            _ => State::Dropbox {
                blocks: Sha256::new(),
                block: Sha256::new(),
                filled: 0,
            },
        };
        Some(Self { kind, state })
    }

    pub(crate) fn kind(&self) -> &'static str {
        self.kind
    }

    pub(crate) fn update(&mut self, mut bytes: &[u8]) {
        match &mut self.state {
            State::Blake3(h) => {
                h.update(bytes);
            }
            State::Sha1(h) => h.update(bytes),
            State::Md5(h) => h.update(bytes),
            State::Dropbox {
                blocks,
                block,
                filled,
            } => {
                while !bytes.is_empty() {
                    let take = (DROPBOX_BLOCK - *filled).min(bytes.len());
                    block.update(&bytes[..take]);
                    *filled += take;
                    bytes = &bytes[take..];
                    if *filled == DROPBOX_BLOCK {
                        blocks.update(std::mem::take(block).finalize());
                        *filled = 0;
                    }
                }
            }
        }
    }

    /// Lower-case hex, as rclone prints it.
    pub(crate) fn finish(self) -> String {
        let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        match self.state {
            State::Blake3(h) => h.finalize().to_hex().to_string(),
            State::Sha1(h) => hex(&h.finalize()),
            State::Md5(h) => hex(&h.finalize()),
            State::Dropbox {
                mut blocks,
                block,
                filled,
            } => {
                if filled > 0 {
                    blocks.update(block.finalize());
                }
                hex(&blocks.finalize())
            }
        }
    }
}

/// What the provider must report for one uploaded object (deferred check).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Expected {
    /// The object's address on the provider (the crypt's base remote).
    pub(crate) address: String,
    pub(crate) kind: String,
    pub(crate) size: u64,
    /// Lower-case hex.
    pub(crate) value: String,
}

/// `read` passes the bytes through and hashes them.
pub(crate) struct Hashing<'a> {
    pub(crate) inner: &'a mut dyn std::io::Read,
    pub(crate) hash: Option<StreamHash>,
}

impl std::io::Read for Hashing<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buffer)?;
        if let Some(hash) = &mut self.hash {
            hash.update(&buffer[..n]);
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(kind: &str, data: &[u8], chunk: usize) -> String {
        let mut h = StreamHash::for_provider(&[kind.to_owned()]).unwrap();
        for part in data.chunks(chunk.max(1)) {
            h.update(part);
        }
        h.finish()
    }

    #[test]
    fn known_vectors_and_chunking_do_not_change_the_result() {
        assert_eq!(digest("md5", b"abc", 1), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            digest("sha1", b"abc", 2),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            digest("blake3", b"abc", 3),
            blake3::hash(b"abc").to_hex().to_string()
        );
        // Dropbox: one block -> sha256(sha256(data)); two blocks hash both.
        let one = digest("dropbox", b"abc", 1);
        let inner = Sha256::digest(b"abc");
        assert_eq!(one, format!("{:x}", Sha256::digest(inner)));
        let data = vec![7u8; DROPBOX_BLOCK + 10];
        let mut outer = Sha256::new();
        outer.update(Sha256::digest(&data[..DROPBOX_BLOCK]));
        outer.update(Sha256::digest(&data[DROPBOX_BLOCK..]));
        assert_eq!(
            digest("dropbox", &data, 1 << 20),
            format!("{:x}", outer.finalize())
        );
        assert_eq!(
            digest("dropbox", &data, 777),
            digest("dropbox", &data, 1 << 22)
        );
    }

    #[test]
    fn provider_preference_and_unknown_kinds() {
        let pick = |r: &[&str]| {
            StreamHash::for_provider(&r.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .map(|h| h.kind())
        };
        assert_eq!(pick(&["md5", "sha1"]), Some("sha1"));
        assert_eq!(pick(&["MD5"]), Some("md5"));
        assert_eq!(pick(&["dropbox"]), Some("dropbox"));
        assert_eq!(pick(&["whirlpool"]), None);
        assert_eq!(pick(&[]), None);
    }
}
