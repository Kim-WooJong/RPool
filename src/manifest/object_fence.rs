//! Sticky namespace fence, created before borrowing; no automatic removal.
//! Conservative whole-source retention protects all generations and unknown refs.
use crate::prelude::*;
use crate::storage::reader::StorageReader;
use crate::storage::writer::StorageWriter;
use crate::storage::error::{StorageError, StorageErrorKind};
const MARKER: &str = "immutable-object-fence-v1.json";
pub(crate) fn guard(reader:&StorageReader,address:&str)->Result<()> {
    let Some((remote, path))=address.split_once(':') else { return Ok(()); };
    let parts:Vec<_>=path.split('/').collect();
    // Probe every parent: archive ids have never had a required prefix.
    for end in 1..parts.len() {
    let path=format!("{remote}:{}/{}",parts[..end].join("/"),MARKER);
    match reader.read_metadata(&path) {
        Ok(_) => bail!("immutable shared-source namespace: mutation/delete blocked; originals and borrowed objects retained (space not reclaimed)"),
        Err(e) if e.downcast_ref::<StorageError>().is_some_and(|e|e.kind()==StorageErrorKind::NotFound) => {},
        Err(e)=>return Err(e.context("cannot prove namespace is unshared; mutation blocked")),
    }
    }
    Ok(())
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
