//! One directory of content-addressed metadata objects (`<id>.json`), the
//! narrow boundary checkpoint code uses. Implemented by `SharedTransport`;
//! tests use an in-memory fake.
use anyhow::Result;

pub(crate) trait ObjectDir {
    /// Every valid `(id, size)`; a missing directory is empty. An unreachable
    /// replica is an error, never an empty listing.
    fn list(&self) -> Result<Vec<(String, u64)>>;
    /// Exact bytes of `id`, verified against the id hash and listed size.
    fn read(&self, id: &str, size: u64) -> Result<Vec<u8>>;
    /// Writes `bytes` at its id with readback; identical bytes are a no-op,
    /// different bytes at an existing id are refused.
    fn publish(&self, id: &str, bytes: &[u8]) -> Result<()>;
    /// Removes `id`; already absent is success.
    fn remove(&self, id: &str) -> Result<()>;
}

pub(crate) fn object_id(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

pub(crate) fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    /// In-memory directory; `fail` simulates an unreachable replica.
    #[derive(Default)]
    pub(crate) struct Dir {
        pub objects: RefCell<BTreeMap<String, Vec<u8>>>,
        pub fail: Cell<bool>,
        pub reads: Cell<usize>,
        pub removes: Cell<usize>,
    }
    impl ObjectDir for Dir {
        fn list(&self) -> Result<Vec<(String, u64)>> {
            if self.fail.get() {
                anyhow::bail!("offline");
            }
            Ok(self
                .objects
                .borrow()
                .iter()
                .map(|(id, b)| (id.clone(), b.len() as u64))
                .collect())
        }
        fn read(&self, id: &str, size: u64) -> Result<Vec<u8>> {
            if self.fail.get() {
                anyhow::bail!("offline");
            }
            self.reads.set(self.reads.get() + 1);
            let bytes = self
                .objects
                .borrow()
                .get(id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("object not found"))?;
            if object_id(&bytes) != id || bytes.len() as u64 != size {
                anyhow::bail!("invalid shared event identity, hash, or size");
            }
            Ok(bytes)
        }
        fn publish(&self, id: &str, bytes: &[u8]) -> Result<()> {
            if self.fail.get() {
                anyhow::bail!("offline");
            }
            if object_id(bytes) != id {
                anyhow::bail!("invalid publication");
            }
            self.objects.borrow_mut().insert(id.into(), bytes.to_vec());
            Ok(())
        }
        fn remove(&self, id: &str) -> Result<()> {
            if self.fail.get() {
                anyhow::bail!("offline");
            }
            self.removes.set(self.removes.get() + 1);
            self.objects.borrow_mut().remove(id);
            Ok(())
        }
    }
}
