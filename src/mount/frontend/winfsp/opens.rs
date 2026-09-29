//! Per-open WinFsp contexts. The context handed to WinFsp is a small integer
//! key, never a pointer, so a volume-level request (NULL context) is just 0.
use crate::mount::fs_core::HandleId;
use crate::prelude::*;
use winfsp_wrs::{FileContextKind, FileContextMode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OpenId(pub(super) usize);
impl FileContextKind for OpenId {
    const MODE: FileContextMode = FileContextMode::Descriptor;
    unsafe fn write(self, out: *mut *mut std::ffi::c_void) {
        // SAFETY: WinFsp provides a valid out pointer for the new context.
        unsafe { out.write(self.0 as *mut std::ffi::c_void) }
    }
    unsafe fn access(raw: *mut std::ffi::c_void) -> Self {
        Self(raw as usize)
    }
    unsafe fn access_for_close(raw: *mut std::ffi::c_void) -> Self {
        Self(raw as usize)
    }
}

#[derive(Clone)]
pub(super) struct Open {
    pub(super) path: String,
    pub(super) directory: bool,
    pub(super) handle: Option<HandleId>,
}

pub(super) struct Opens {
    next: usize,
    open: BTreeMap<usize, Open>,
}
impl Opens {
    pub(super) fn new() -> Self {
        Self {
            next: 0,
            open: BTreeMap::new(),
        }
    }
    pub(super) fn insert(&mut self, open: Open) -> OpenId {
        self.next += 1;
        self.open.insert(self.next, open);
        OpenId(self.next)
    }
    pub(super) fn get(&self, id: OpenId) -> Option<Open> {
        self.open.get(&id.0).cloned()
    }
    pub(super) fn remove(&mut self, id: OpenId) -> Option<Open> {
        self.open.remove(&id.0)
    }
    /// Opens below a renamed path follow it.
    pub(super) fn rename(&mut self, from: &str, to: &str) {
        let prefix = format!("{from}/");
        for open in self.open.values_mut() {
            if open.path == from {
                open.path = to.into();
            } else if let Some(rest) = open.path.strip_prefix(&prefix) {
                open.path = format!("{to}/{rest}");
            }
        }
    }
}
