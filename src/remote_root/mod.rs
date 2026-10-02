//! Per-remote default paths ("remote roots"): a bare `name:` remote is
//! expanded to `name:<root>` from the remote-roots config file. Managed by
//! `rpool remote-root` and the GUI Settings; applied wherever pools resolve
//! their remotes (uploads, mounts, migrations, reprocess).
/// Reading the remote-roots config file.
mod load;
/// Setting and removing a remote's default path.
mod manage;
/// Expanding remotes with their configured default path.
mod resolve;
/// Writing the remote-roots config file.
mod save;

pub(crate) use load::load_remote_root_store;
pub(crate) use manage::{remove_remote_root, set_remote_root};
pub(crate) use resolve::{
    apply_remote_root, apply_remote_root_with_store, apply_remote_roots, remote_name,
};
pub(crate) use save::save_remote_root_store;
