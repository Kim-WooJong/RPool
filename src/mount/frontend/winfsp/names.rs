//! Windows names (`\a\b`) to core paths (`a/b`).
use winfsp_wrs::{U16CStr, NTSTATUS, STATUS_OBJECT_NAME_INVALID};

/// `\a\b` → `a/b`; `\` → the root (empty path).
pub(super) fn core_path(name: &U16CStr) -> Result<String, NTSTATUS> {
    let text = name.to_string().map_err(|_| STATUS_OBJECT_NAME_INVALID)?;
    if text.contains('/') {
        return Err(STATUS_OBJECT_NAME_INVALID);
    }
    Ok(text
        .split('\\')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/"))
}
