//! `winfsp_wrs::FileSystemInterface` over `FsCore`.
//!
//! Durability: Cleanup (last handle closed; cached writes were flushed first
//! because the volume flushes and purges on cleanup) and Flush seal the file.
//! Close releases the core handle. Delete happens at Cleanup with the DELETE flag.
use super::super::names::{after_marker, find, join, resolve, split, write_window};
use super::names::core_path;
use super::opens::{Open, OpenId, Opens};
use super::status::status;
use crate::mount::fs_core::{Access, Attr, FsCore, HandleId};
use crate::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};
use winfsp_wrs::{
    filetime_now, u16cstr, u16str, CleanupFlags, CreateFileInfo, CreateOptions, DirInfo,
    FileAccessRights, FileAttributes, FileInfo, FileSystemInterface, PSecurityDescriptor,
    SecurityDescriptor, U16CStr, VolumeInfo, WriteMode, NTSTATUS, STATUS_ACCESS_DENIED,
    STATUS_DIRECTORY_NOT_EMPTY, STATUS_END_OF_FILE, STATUS_FILE_IS_A_DIRECTORY,
    STATUS_INVALID_DEVICE_REQUEST, STATUS_INVALID_HANDLE, STATUS_MEDIA_WRITE_PROTECTED,
    STATUS_NOT_A_DIRECTORY, STATUS_OBJECT_NAME_COLLISION,
};

const ALLOCATION_UNIT: u64 = 4096;

pub(super) struct RpoolWinFs {
    core: Arc<FsCore>,
    opens: Mutex<Opens>,
    read_only: bool,
    security: SecurityDescriptor,
    started: u64,
    stopped: Arc<AtomicBool>,
}

impl RpoolWinFs {
    pub(super) fn new(
        core: Arc<FsCore>,
        read_only: bool,
        stopped: Arc<AtomicBool>,
    ) -> Result<Self> {
        // Owner/group Administrators; full access for SYSTEM, Administrators, Everyone.
        let security = SecurityDescriptor::from_wstr(u16cstr!(
            "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;WD)"
        ))
        .map_err(|e| anyhow!("security descriptor: {e}"))?;
        Ok(Self {
            core,
            opens: Mutex::new(Opens::new()),
            read_only,
            security,
            started: filetime_now(),
            stopped,
        })
    }
    fn opens(&self) -> std::sync::MutexGuard<'_, Opens> {
        self.opens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    fn open_of(&self, id: OpenId) -> Result<Open, NTSTATUS> {
        self.opens().get(id).ok_or(STATUS_INVALID_HANDLE)
    }
    fn handle_of(&self, id: OpenId) -> Result<HandleId, NTSTATUS> {
        self.open_of(id)?
            .handle
            .ok_or(STATUS_INVALID_DEVICE_REQUEST)
    }
    fn writable(&self) -> Result<(), NTSTATUS> {
        if self.read_only {
            return Err(STATUS_MEDIA_WRITE_PROTECTED);
        }
        Ok(())
    }
    fn attributes(&self, attr: &Attr) -> FileAttributes {
        if attr.directory {
            FileAttributes::DIRECTORY
        } else if self.read_only {
            FileAttributes::ARCHIVE | FileAttributes::READONLY
        } else {
            FileAttributes::ARCHIVE
        }
    }
    fn info(&self, attr: &Attr) -> FileInfo {
        let mut info = FileInfo::default();
        info.set_file_attributes(self.attributes(attr))
            .set_file_size(attr.size)
            .set_allocation_size(attr.size.div_ceil(ALLOCATION_UNIT) * ALLOCATION_UNIT)
            .set_time(self.started);
        info
    }
    /// Attributes of an open: a file through its handle, a directory by path.
    fn open_info(&self, open: &Open) -> Result<FileInfo, NTSTATUS> {
        let attr = match open.handle {
            Some(handle) => self.core.stat(handle),
            None => self.core.lookup(&open.path),
        };
        Ok(self.info(&attr.map_err(status)?))
    }
    fn size(&self, handle: HandleId) -> Result<u64, NTSTATUS> {
        Ok(self.core.stat(handle).map_err(status)?.size)
    }
}

impl FileSystemInterface for RpoolWinFs {
    type FileContext = OpenId;

    const GET_VOLUME_INFO_DEFINED: bool = true;
    fn get_volume_info(&self) -> Result<VolumeInfo, NTSTATUS> {
        let (total, free) = match self.core.statfs() {
            Some((used, Some(total))) => (total.max(used), total.max(used) - used),
            _ => (0, 0),
        };
        VolumeInfo::new(total, free, u16str!("RPool")).map_err(|_| STATUS_INVALID_DEVICE_REQUEST)
    }

    const GET_SECURITY_BY_NAME_DEFINED: bool = true;
    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        _find_reparse_point: impl Fn() -> Option<FileAttributes>,
    ) -> Result<(FileAttributes, PSecurityDescriptor, bool), NTSTATUS> {
        let path = resolve(&self.core, &core_path(file_name)?);
        let attr = self.core.lookup(&path).map_err(status)?;
        Ok((self.attributes(&attr), self.security.as_ptr(), false))
    }

    const GET_SECURITY_DEFINED: bool = true;
    fn get_security(&self, _file_context: OpenId) -> Result<PSecurityDescriptor, NTSTATUS> {
        Ok(self.security.as_ptr())
    }

    const CREATE_DEFINED: bool = true;
    fn create(
        &self,
        file_name: &U16CStr,
        create_file_info: CreateFileInfo,
        _security_descriptor: SecurityDescriptor,
    ) -> Result<(OpenId, FileInfo), NTSTATUS> {
        self.writable()?;
        let (parent, leaf) = split(&self.core, &core_path(file_name)?);
        if find(&self.core, &parent, &leaf).is_some() {
            return Err(STATUS_OBJECT_NAME_COLLISION);
        }
        let path = join(&parent, &leaf);
        let open = if create_file_info
            .create_options
            .is(CreateOptions::FILE_DIRECTORY_FILE)
        {
            self.core.mkdir(&path).map_err(status)?;
            Open {
                path,
                directory: true,
                handle: None,
            }
        } else {
            let access = Access::Write {
                truncate: false,
                append: false,
            };
            let handle = self.core.open(&path, access, true, true).map_err(status)?;
            Open {
                path,
                directory: false,
                handle: Some(handle),
            }
        };
        let info = self.open_info(&open)?;
        Ok((self.opens().insert(open), info))
    }

    const OPEN_DEFINED: bool = true;
    fn open(
        &self,
        file_name: &U16CStr,
        create_options: CreateOptions,
        granted_access: FileAccessRights,
    ) -> Result<(OpenId, FileInfo), NTSTATUS> {
        let path = resolve(&self.core, &core_path(file_name)?);
        let attr = self.core.lookup(&path).map_err(status)?;
        if attr.directory && create_options.is(CreateOptions::FILE_NON_DIRECTORY_FILE) {
            return Err(STATUS_FILE_IS_A_DIRECTORY);
        }
        if !attr.directory && create_options.is(CreateOptions::FILE_DIRECTORY_FILE) {
            return Err(STATUS_NOT_A_DIRECTORY);
        }
        let open = if attr.directory {
            Open {
                path,
                directory: true,
                handle: None,
            }
        } else {
            let write = FileAccessRights::FILE_WRITE_DATA.0 | FileAccessRights::FILE_APPEND_DATA.0;
            let access = if !self.read_only && granted_access.0 & write != 0 {
                Access::Write {
                    truncate: false,
                    append: false,
                }
            } else {
                Access::Read
            };
            let handle = self
                .core
                .open(&path, access, false, false)
                .map_err(status)?;
            Open {
                path,
                directory: false,
                handle: Some(handle),
            }
        };
        let info = self.open_info(&open)?;
        Ok((self.opens().insert(open), info))
    }

    const OVERWRITE_DEFINED: bool = true;
    fn overwrite(
        &self,
        file_context: OpenId,
        _file_attributes: FileAttributes,
        _replace_file_attributes: bool,
        _allocation_size: u64,
    ) -> Result<FileInfo, NTSTATUS> {
        self.writable()?;
        let handle = self.handle_of(file_context)?;
        self.core.truncate(handle, 0).map_err(status)?;
        self.open_info(&self.open_of(file_context)?)
    }

    const CLEANUP_DEFINED: bool = true;
    fn cleanup(&self, file_context: OpenId, _file_name: Option<&U16CStr>, flags: CleanupFlags) {
        let Ok(open) = self.open_of(file_context) else {
            return;
        };
        let (operation, result) = if flags.is(CleanupFlags::DELETE) {
            if open.directory {
                ("rmdir", self.core.rmdir(&open.path))
            } else {
                ("delete", self.core.delete(&open.path))
            }
        } else {
            match open.handle {
                Some(handle) => ("flush", self.core.flush(handle)),
                None => return,
            }
        };
        // Cleanup cannot fail; unsealed bytes stay in the spool for recovery.
        if let Err(error) = result {
            eprintln!(
                "RPool WinFsp cleanup ({operation}) of {}: {error}",
                open.path
            );
        }
    }

    const CLOSE_DEFINED: bool = true;
    fn close(&self, file_context: OpenId) {
        let Some(open) = self.opens().remove(file_context) else {
            return;
        };
        if let Some(handle) = open.handle {
            if let Err(error) = self.core.release(handle) {
                eprintln!("RPool WinFsp close (release) of {}: {error}", open.path);
            }
        }
    }

    const READ_DEFINED: bool = true;
    fn read(
        &self,
        file_context: OpenId,
        buffer: &mut [u8],
        offset: u64,
    ) -> Result<usize, NTSTATUS> {
        let handle = self.handle_of(file_context)?;
        let bytes = self
            .core
            .read_at(handle, offset, buffer.len())
            .map_err(status)?;
        if bytes.is_empty() && !buffer.is_empty() {
            return Err(STATUS_END_OF_FILE);
        }
        buffer[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }

    const WRITE_DEFINED: bool = true;
    fn write(
        &self,
        file_context: OpenId,
        buffer: &[u8],
        mode: WriteMode,
    ) -> Result<(usize, FileInfo), NTSTATUS> {
        self.writable()?;
        let open = self.open_of(file_context)?;
        let handle = open.handle.ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
        let (offset, constrained) = match mode {
            WriteMode::Normal { offset } => (Some(offset), false),
            WriteMode::WriteToEOF => (None, false),
            WriteMode::ConstrainedIO { offset } => (Some(offset), true),
        };
        let size = self.size(handle)?;
        let (offset, len) = write_window(offset, constrained, size, buffer.len());
        if len == 0 {
            return Ok((0, self.open_info(&open)?));
        }
        let written = self
            .core
            .write_at(handle, offset, &buffer[..len])
            .map_err(status)?;
        Ok((written, self.open_info(&open)?))
    }

    const FLUSH_DEFINED: bool = true;
    fn flush(&self, file_context: OpenId) -> Result<FileInfo, NTSTATUS> {
        if file_context.0 == 0 {
            // Volume flush: every acknowledged write is already sealed.
            return Ok(FileInfo::default());
        }
        let open = self.open_of(file_context)?;
        if let Some(handle) = open.handle {
            self.core.flush(handle).map_err(status)?;
        }
        self.open_info(&open)
    }

    const GET_FILE_INFO_DEFINED: bool = true;
    fn get_file_info(&self, file_context: OpenId) -> Result<FileInfo, NTSTATUS> {
        self.open_info(&self.open_of(file_context)?)
    }

    const SET_BASIC_INFO_DEFINED: bool = true;
    fn set_basic_info(
        &self,
        file_context: OpenId,
        _file_attributes: FileAttributes,
        _creation_time: u64,
        _last_access_time: u64,
        _last_write_time: u64,
        _change_time: u64,
    ) -> Result<FileInfo, NTSTATUS> {
        // Attributes and times are not stored.
        self.open_info(&self.open_of(file_context)?)
    }

    const SET_FILE_SIZE_DEFINED: bool = true;
    fn set_file_size(
        &self,
        file_context: OpenId,
        new_size: u64,
        set_allocation_size: bool,
    ) -> Result<FileInfo, NTSTATUS> {
        self.writable()?;
        let open = self.open_of(file_context)?;
        let handle = open.handle.ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
        // An allocation below the file size truncates; a larger one is a hint.
        if !set_allocation_size || new_size < self.size(handle)? {
            self.core.truncate(handle, new_size).map_err(status)?;
        }
        self.open_info(&open)
    }

    const SET_DELETE_DEFINED: bool = true;
    fn set_delete(
        &self,
        file_context: OpenId,
        _file_name: &U16CStr,
        delete_file: bool,
    ) -> Result<(), NTSTATUS> {
        if !delete_file {
            return Ok(());
        }
        self.writable()?;
        let open = self.open_of(file_context)?;
        if open.directory && !self.core.readdir(&open.path).map_err(status)?.is_empty() {
            return Err(STATUS_DIRECTORY_NOT_EMPTY);
        }
        Ok(())
    }

    const RENAME_DEFINED: bool = true;
    fn rename(
        &self,
        file_context: OpenId,
        _file_name: &U16CStr,
        new_file_name: &U16CStr,
        replace_if_exists: bool,
    ) -> Result<(), NTSTATUS> {
        self.writable()?;
        let from = self.open_of(file_context)?.path;
        let (parent, leaf) = split(&self.core, &core_path(new_file_name)?);
        let mut to = join(&parent, &leaf);
        if let Some(existing) = find(&self.core, &parent, &leaf) {
            // A case-only rename of the same entry is allowed.
            if existing != from {
                if !replace_if_exists {
                    return Err(STATUS_OBJECT_NAME_COLLISION);
                }
                if self.core.lookup(&existing).map_err(status)?.directory {
                    return Err(STATUS_ACCESS_DENIED);
                }
                to = existing;
            }
        }
        self.core.rename(&from, &to).map_err(status)?;
        self.opens().rename(&from, &to);
        Ok(())
    }

    const READ_DIRECTORY_DEFINED: bool = true;
    fn read_directory(
        &self,
        file_context: OpenId,
        marker: Option<&U16CStr>,
        mut add_dir_info: impl FnMut(DirInfo) -> bool,
    ) -> Result<(), NTSTATUS> {
        let open = self.open_of(file_context)?;
        if !open.directory {
            return Err(STATUS_NOT_A_DIRECTORY);
        }
        let mut listing = Vec::new();
        if !open.path.is_empty() {
            let folder = self.info(&Attr {
                id: None,
                size: 0,
                directory: true,
                tag: String::new(),
            });
            listing.push((".".to_string(), folder));
            listing.push(("..".to_string(), folder));
        }
        for (name, attr) in self.core.readdir(&open.path).map_err(status)? {
            listing.push((name, self.info(&attr)));
        }
        // One bytewise order for listing and marker, so resuming is exact.
        let marker = marker.map(|m| m.to_string_lossy());
        for (name, info) in after_marker(listing, marker.as_deref()) {
            if !add_dir_info(DirInfo::from_str(info, &name)) {
                break;
            }
        }
        Ok(())
    }

    const DISPATCHER_STOPPED_DEFINED: bool = true;
    fn dispatcher_stopped(&self, _normally: bool) {
        self.stopped.store(true, Ordering::Release);
    }
}
