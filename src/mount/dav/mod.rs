//! Loopback-only authenticated bridge. DAV handles pin immutable revisions.
use super::{
    namespace::Intent,
    virtual_drive::{Revision, VirtualDrive},
};
use crate::prelude::*;
use bytes::{Buf, Bytes};
use dav_server::{davpath::DavPath, fs::*, DavHandler};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// `DavFileSystem` implementation over the virtual drive.
mod filesystem;
/// Open file handles.
mod handle;
/// Server lifecycle and persistent endpoint.
mod server;
#[cfg(test)]
mod tests;

use filesystem::VirtualFs;
use handle::Handle;
#[cfg(test)]
use server::endpoint;
pub(crate) use server::Server;

#[derive(Default)]
/// Diagnostic counters of DAV writes, printed as a summary when the server stops.
struct WriteStats {
    /// Authorized PUT requests.
    put_attempts: AtomicU64,
    /// PUT requests that returned success.
    put_successes: AtomicU64,
    /// Authorized PATCH requests.
    patch_attempts: AtomicU64,
    /// Authorized requests with a `Content-Range` header.
    ranged_attempts: AtomicU64,
    /// Files opened for writing.
    write_opens: AtomicU64,
    /// Write opens that truncated the file.
    truncating_opens: AtomicU64,
    /// Request body bytes written to spool files.
    body_bytes: AtomicU64,
    /// Bytes copied from the previous revision into a new spool file (non-truncating opens).
    baseline_copy_bytes: AtomicU64,
    /// Write intents sealed on flush.
    seals: AtomicU64,
    /// Writes rejected because fewer body bytes arrived than expected.
    incomplete: AtomicU64,
}

/// Logs `e` and maps it to `FsError::GeneralFailure`.
fn failure(e: impl std::fmt::Display) -> FsError {
    eprintln!("Virtual filesystem: {e}");
    FsError::GeneralFailure
}
/// Drive path of a DAV path without the trailing slash; invalid names are `Forbidden`.
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
/// DAV metadata of a file or directory.
struct Meta {
    /// Size in bytes (0 for directories).
    size: u64,
    /// Is a directory.
    directory: bool,
    /// ETag: revision/intent id for files, `dir-<generation>` for directories; also seeds `modified`.
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
/// One directory listing entry.
struct Entry {
    /// Entry name (last path component).
    name: String,
    /// Entry metadata.
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
