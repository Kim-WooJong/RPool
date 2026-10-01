//! Loopback-only authenticated bridge. DAV handles pin immutable revisions.
use super::{
    namespace::Intent,
    virtual_drive::{Revision, VirtualDrive},
};
use crate::prelude::*;
use bytes::{Buf, Bytes};
use dav_server::{davpath::DavPath, fs::*, DavHandler};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

mod filesystem;
mod handle;
mod server;
#[cfg(test)]
mod tests;

use filesystem::VirtualFs;
use handle::Handle;
#[cfg(test)]
use server::endpoint;
pub(crate) use server::Server;

#[derive(Default)]
struct WriteStats {
    put_attempts: AtomicU64,
    put_successes: AtomicU64,
    patch_attempts: AtomicU64,
    ranged_attempts: AtomicU64,
    write_opens: AtomicU64,
    truncating_opens: AtomicU64,
    body_bytes: AtomicU64,
    baseline_copy_bytes: AtomicU64,
    seals: AtomicU64,
    incomplete: AtomicU64,
}

fn failure(e: impl std::fmt::Display) -> FsError {
    eprintln!("Virtual filesystem: {e}");
    FsError::GeneralFailure
}
fn path(p: &DavPath) -> FsResult<String> {
    let raw = p
        .as_rel_ospath()
        .to_str()
        .ok_or(FsError::Forbidden)?
        .trim_end_matches('/');
    if !raw.is_empty() {
        super::namespace::valid_path(raw).map_err(|_| FsError::Forbidden)?;
    }
    Ok(raw.into())
}
#[derive(Clone, Debug)]
struct Meta {
    size: u64,
    directory: bool,
    tag: String,
}
impl DavMetaData for Meta {
    fn len(&self) -> u64 {
        self.size
    }
    fn is_dir(&self) -> bool {
        self.directory
    }
    fn modified(&self) -> FsResult<SystemTime> {
        // Stable revision-derived timestamp: same-size replacement must invalidate
        // clients that compare only size/mtime instead of DAV ETags.
        let hash = blake3::hash(self.tag.as_bytes());
        let n = u64::from_le_bytes(hash.as_bytes()[..8].try_into().unwrap());
        Ok(UNIX_EPOCH + std::time::Duration::from_secs(946684800 + n % 1893456000))
    }
    fn etag(&self) -> Option<String> {
        Some(self.tag.clone())
    }
}
struct Entry {
    name: String,
    meta: Meta,
}
impl DavDirEntry for Entry {
    fn name(&self) -> Vec<u8> {
        self.name.as_bytes().to_vec()
    }
    fn metadata(&self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        Box::pin(async move { Ok(Box::new(self.meta.clone()) as _) })
    }
}
