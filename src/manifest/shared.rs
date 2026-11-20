//! Immutable reference capability; legacy bytes are never rewritten.
use crate::prelude::*;

pub(crate) fn content_root_v3(manifest: &Manifest) -> Result<String> {
    let mut h = blake3::Hasher::new();
    h.update(b"rpool-immutable-objects-v3\0");
    h.update(super::content_root_v2(manifest.original_size, manifest.shard_size, &manifest.coding, &manifest.shards).as_bytes());
    h.update(manifest.archive_id.as_bytes());
    h.update(&serde_json::to_vec(&manifest.shared)?);
    Ok(h.finalize().to_hex().to_string())
}

pub(crate) fn validate_shared(m: &Manifest) -> Result<()> {
    let shared = m.shared.as_ref().context("v3 requires immutable-object ownership")?;
    if shared.capability != "immutable-borrowed-objects-v1" || (shared.source_fingerprint.len() != 64 || !shared.source_fingerprint.bytes().all(|b|b.is_ascii_hexdigit())) || shared.objects.len() != m.shards.len() { bail!("unsupported/incomplete immutable object capability"); }
    let mut indexes = BTreeSet::new();
    for (s, o) in m.shards.iter().zip(&shared.objects) {
        if o.index != s.index || !indexes.insert(o.index) || (o.owner.is_empty() || o.owner.contains(['/',':','\\']) || o.owner == "." || o.owner == "..") || o.object != s.object || o.remote != s.remote || o.size != s.size || o.blake3 != s.blake3 || !o.object.starts_with(&crate::utils::remote_join(&o.remote, &format!("{}/", o.owner))) { bail!("immutable object identity/owner mismatch"); }
    }
    if content_root_v3(m)? != m.content_root_blake3 { bail!("v3 ownership content-root mismatch"); }
    let mut legacy = m.clone();
    legacy.version = 2;
    legacy.shared = None;
    legacy.content_root_blake3 = super::content_root_v2(m.original_size, m.shard_size, &m.coding, &m.shards);
    super::validate::validate_manifest_v2(&legacy)
}
