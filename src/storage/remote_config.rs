//! Legacy command composition; provider facts are owned by admin.
use super::admin::{BackendAdmin, RcloneAdmin};
use crate::prelude::*;
/// Names of the crypt remotes in the inherited rclone config. Used by `rpool provider health`
/// when no remotes are given.
pub(crate) fn list_crypt_remotes(rclone: &str) -> Result<Vec<String>> {
    Ok(RcloneAdmin::inherited(rclone).catalog()?.crypt_remotes())
}
