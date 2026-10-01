//! The Drive page: mount status and primary actions, connection settings, a
//! capacity verdict, then tabs for options, history cleanup, imports and
//! maintenance. Pool-change and recovery steps live in Storage › Account
//! changes (`transitions`).
mod args;
mod cache_recovery;
mod capacity_panel;
mod cleanup;
mod conflicts;
mod drive_section;
mod form;
mod identity_summary;
mod import;
mod layout_notice;
mod log;
mod maintenance;
mod options;
mod session;
#[cfg(test)]
pub(crate) mod session_tests;
pub(crate) mod sessions;
mod sessions_strip;
mod status_bar;
#[cfg(test)]
mod tests;
pub(crate) mod transitions;
mod view;

use args::build_args;
pub(crate) use form::MountForm;
#[allow(unused_imports)] // For the Monitoring page.
pub(crate) use sessions::{mounted_sessions, MountedSession};
pub(crate) use view::show;
