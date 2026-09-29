//! `fuser::Filesystem` over `FsCore`. Paths come from the inode table; every
//! data operation goes through a core handle.
use super::errno::errno;
use super::inodes::Inodes;
use crate::mount::fs_core::{Access, Attr, FsCore, FsError, HandleId};
use crate::prelude::*;
use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, LockOwner,
    OpenAccMode, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory,
    ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request, TimeOrNow, WriteFlags,
};
use std::ffi::OsStr;
use std::os::unix::fs::MetadataExt;
use std::time::{Duration, SystemTime};

const TTL: Duration = Duration::from_secs(1);
const BLOCK: u32 = 4096;

pub(super) struct RpoolFs {
    core: Arc<FsCore>,
    inodes: Mutex<Inodes>,
    read_only: bool,
    uid: u32,
    gid: u32,
    started: SystemTime,
}

fn join(parent: &str, name: &OsStr) -> Result<String, Errno> {
    let name = name.to_str().ok_or(Errno::EINVAL)?;
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(Errno::EINVAL);
    }
    Ok(if parent.is_empty() {
        name.into()
    } else {
        format!("{parent}/{name}")
    })
}
fn handle(fh: FileHandle) -> HandleId {
    HandleId(fh.0)
}

impl RpoolFs {
    pub(super) fn new(core: Arc<FsCore>, mountpoint: &Path, read_only: bool) -> Result<Self> {
        let owner = fs::metadata(mountpoint).context("mountpoint must exist")?;
        Ok(Self {
            core,
            inodes: Mutex::new(Inodes::new()),
            read_only,
            uid: owner.uid(),
            gid: owner.gid(),
            started: SystemTime::now(),
        })
    }
    fn inodes(&self) -> std::sync::MutexGuard<'_, Inodes> {
        self.inodes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    fn path(&self, ino: INodeNo) -> Result<String, Errno> {
        self.inodes().path(ino.0).ok_or(Errno::ENOENT)
    }
    fn child(&self, parent: INodeNo, name: &OsStr) -> Result<String, Errno> {
        join(&self.path(parent)?, name)
    }
    fn writable(&self) -> Result<(), Errno> {
        if self.read_only {
            return Err(Errno::EROFS);
        }
        Ok(())
    }
    fn attr(&self, ino: u64, attr: &Attr) -> FileAttr {
        let (kind, perm, nlink) = if attr.directory {
            (FileType::Directory, 0o755, 2)
        } else {
            (FileType::RegularFile, 0o644, 1)
        };
        FileAttr {
            ino: INodeNo(ino),
            size: attr.size,
            blocks: attr.size.div_ceil(512),
            atime: self.started,
            mtime: self.started,
            ctime: self.started,
            crtime: self.started,
            kind,
            perm: if self.read_only { perm & 0o555 } else { perm },
            nlink,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: BLOCK,
            flags: 0,
        }
    }
    /// Look up `path` and return its attributes with a (possibly new) inode.
    fn entry(&self, path: &str) -> Result<FileAttr, Errno> {
        let attr = self.core.lookup(path).map_err(errno)?;
        let ino = self.inodes().ino(path);
        Ok(self.attr(ino, &attr))
    }
    fn truncate(&self, path: &str, fh: Option<FileHandle>, size: u64) -> Result<(), FsError> {
        if let Some(fh) = fh {
            return self.core.truncate(handle(fh), size);
        }
        let temporary = self.core.open(
            path,
            Access::Write {
                truncate: false,
                append: false,
            },
            false,
            false,
        )?;
        let truncated = self.core.truncate(temporary, size);
        let released = self.core.release(temporary);
        truncated.and(released)
    }
}

impl Filesystem for RpoolFs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        match self.child(parent, name).and_then(|p| self.entry(&p)) {
            Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
            Err(e) => reply.error(e),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.path(ino).and_then(|p| self.entry(&p)) {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(e) => reply.error(e),
        }
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let result = (|| {
            let path = self.path(ino)?;
            if let Some(size) = size {
                self.writable()?;
                self.truncate(&path, fh, size).map_err(errno)?;
            }
            // Mode, owner and times are not stored; report the current attributes.
            self.entry(&path)
        })();
        match result {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(e) => reply.error(e),
        }
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let result = (|| {
            self.writable()?;
            let path = self.child(parent, name)?;
            self.core.mkdir(&path).map_err(errno)?;
            self.entry(&path)
        })();
        match result {
            Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
            Err(e) => reply.error(e),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let result = (|| {
            self.writable()?;
            let path = self.child(parent, name)?;
            self.core.delete(&path).map_err(errno)?;
            self.inodes().forget_path(&path);
            Ok(())
        })();
        match result {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let result = (|| {
            self.writable()?;
            let path = self.child(parent, name)?;
            self.core.rmdir(&path).map_err(errno)?;
            self.inodes().forget_path(&path);
            Ok(())
        })();
        match result {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let result = (|| {
            self.writable()?;
            if flags.contains(RenameFlags::RENAME_EXCHANGE) {
                return Err(Errno::EINVAL);
            }
            let from = self.child(parent, name)?;
            let to = self.child(newparent, newname)?;
            if flags.contains(RenameFlags::RENAME_NOREPLACE) && self.core.lookup(&to).is_ok() {
                return Err(Errno::EEXIST);
            }
            self.core.rename(&from, &to).map_err(errno)?;
            self.inodes().rename(&from, &to);
            Ok(())
        })();
        match result {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let result = (|| {
            let path = self.path(ino)?;
            let access = match flags.acc_mode() {
                OpenAccMode::O_RDONLY => Access::Read,
                _ => {
                    self.writable()?;
                    Access::Write {
                        truncate: flags.0 & libc::O_TRUNC != 0,
                        append: flags.0 & libc::O_APPEND != 0,
                    }
                }
            };
            self.core.open(&path, access, false, false).map_err(errno)
        })();
        match result {
            Ok(h) => reply.opened(FileHandle(h.0), FopenFlags::empty()),
            Err(e) => reply.error(e),
        }
    }

    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let result = (|| {
            self.writable()?;
            let path = self.child(parent, name)?;
            let access = Access::Write {
                truncate: flags & libc::O_TRUNC != 0,
                append: flags & libc::O_APPEND != 0,
            };
            let h = self
                .core
                .open(&path, access, true, flags & libc::O_EXCL != 0)
                .map_err(errno)?;
            match self.entry(&path) {
                Ok(attr) => Ok((h, attr)),
                Err(e) => {
                    let _ = self.core.release(h);
                    Err(e)
                }
            }
        })();
        match result {
            Ok((h, attr)) => reply.created(
                &TTL,
                &attr,
                Generation(0),
                FileHandle(h.0),
                FopenFlags::empty(),
            ),
            Err(e) => reply.error(e),
        }
    }

    fn read(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        match self.core.read_at(handle(fh), offset, size as usize) {
            Ok(bytes) => reply.data(&bytes),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        if self.read_only {
            return reply.error(Errno::EROFS);
        }
        match self.core.write_at(handle(fh), offset, data) {
            Ok(n) => reply.written(n as u32),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn flush(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        match self.core.flush(handle(fh)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        match self.core.release(handle(fh)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn fsync(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        match self.core.fsync(handle(fh)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let result = (|| {
            let path = self.path(ino)?;
            let entries = self.core.readdir(&path).map_err(errno)?;
            let mut listing = vec![
                (ino.0, FileType::Directory, ".".to_string()),
                (ino.0, FileType::Directory, "..".to_string()),
            ];
            let mut inodes = self.inodes();
            for (name, attr) in entries {
                let child = join(&path, OsStr::new(&name))?;
                let kind = if attr.directory {
                    FileType::Directory
                } else {
                    FileType::RegularFile
                };
                listing.push((inodes.ino(&child), kind, name));
            }
            Ok(listing)
        })();
        match result {
            Ok(listing) => {
                for (index, (ino, kind, name)) in
                    listing.into_iter().enumerate().skip(offset as usize)
                {
                    if reply.add(INodeNo(ino), index as u64 + 1, kind, name) {
                        break;
                    }
                }
                reply.ok();
            }
            Err(e) => reply.error(e),
        }
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let (used, total) = match self.core.statfs() {
            Some((used, Some(total))) => (used, total.max(used)),
            // Unverified capacity: report usage and no additional free space.
            _ => (0, 0),
        };
        let blocks = total.div_ceil(u64::from(BLOCK));
        let free = (total - used) / u64::from(BLOCK);
        reply.statfs(blocks, free, free, 0, 0, BLOCK, 255, BLOCK);
    }
}
