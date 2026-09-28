//! Legacy command composition; provider facts are owned by admin.
use super::admin::{BackendAdmin, RcloneAdmin};
use crate::prelude::*;
pub(crate) fn list_crypt_remotes(rclone: &str) -> Result<Vec<String>> {
    Ok(RcloneAdmin::inherited(rclone).catalog()?.crypt_remotes())
}
