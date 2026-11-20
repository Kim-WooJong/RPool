//! Sticky namespace fence, created before borrowing; no automatic removal.
//! Conservative whole-source retention protects all generations and unknown refs.
use crate::prelude::*;
use crate::storage::reader::StorageReader;
use crate::storage::writer::StorageWriter;
use crate::storage::error::{StorageError, StorageErrorKind};
const MARKER: &str = "immutable-object-fence-v1.json";
/// Every parent marker address `guard` must prove absent before mutating `address`.
/// Probe every parent: archive ids have never had a required prefix.
pub(crate) fn probe_addresses(address:&str)->Vec<String> {
    let Some((remote, path))=address.split_once(':') else { return Vec::new(); };
    let parts:Vec<_>=path.split('/').collect();
    (1..parts.len()).map(|end| format!("{remote}:{}/{}",parts[..end].join("/"),MARKER)).collect()
}
pub(crate) fn guard(reader:&StorageReader,address:&str)->Result<()> {
    for path in probe_addresses(address) {
    match reader.read_metadata(&path) {
        Ok(_) => bail!("immutable shared-source namespace: mutation/delete blocked; originals and borrowed objects retained (space not reclaimed)"),
        Err(e) if e.downcast_ref::<StorageError>().is_some_and(|e|e.kind()==StorageErrorKind::NotFound) => {},
        Err(e)=>return Err(e.context("cannot prove namespace is unshared; mutation blocked")),
    }
    }
    Ok(())
}
/// Test fixture routing only (never a guard bypass): binds every fence address
/// `guard` probes for the already-bound objects to one separate, initially empty
/// memory backend. Absence is then the typed backend `NotFound` a real remote
/// returns, instead of an unbound synthetic route, and per-operation fault
/// schedules on the objects' own backend keep their original call ordinals.
/// A marker written into the returned backend still blocks mutation.
#[cfg(test)]
pub(crate) fn bind_absent_fences(
    registry:&mut crate::storage::registry::BackendRegistry,
    bindings:&mut std::collections::BTreeMap<String,crate::storage::reference::ObjectRef>,
)->std::sync::Arc<crate::storage::memory::MemoryBackend> {
    use crate::storage::reference::{BackendId,ObjectKey,ObjectRef};
    use crate::storage::traits::StorageBackend;
    let fences=std::sync::Arc::new(crate::storage::memory::MemoryBackend::new(BackendId::new("absent-object-fences").unwrap()));
    registry.register(fences.clone()).unwrap();
    let probes:Vec<String>=bindings.keys().filter(|raw|!raw.ends_with(MARKER)).flat_map(|raw|probe_addresses(raw)).collect();
    for (n,probe) in probes.into_iter().enumerate() {
        bindings.entry(probe).or_insert_with(||ObjectRef::new(fences.id(),ObjectKey::new(format!("fence-{n}")).unwrap()));
    }
    fences
}
pub(crate) fn protect(storage:&StorageWriter,source:&Manifest)->Result<()> {
    let bytes=br#"{"capability":"immutable-borrowed-objects-v1","retention":"whole-source-sticky"}"#;
    for remote in super::manifest_remotes(source) {
        let path=crate::utils::remote_join(&remote,&format!("{}/{MARKER}",source.archive_id));
        match storage.reader().read_metadata(&path) {
            Ok(existing) if existing==bytes=>{ continue; },
            Ok(_)=>bail!("conflicting immutable source fence"),
            Err(e) if e.downcast_ref::<StorageError>().is_some_and(|e|e.kind()==StorageErrorKind::NotFound)=>{ storage.write_bytes(&path,bytes,1)?; },
            Err(e)=>return Err(e),
        }
        if storage.reader().read_metadata(&path)? != bytes { bail!("source fence fresh read mismatch"); }
    }
    Ok(())
}
