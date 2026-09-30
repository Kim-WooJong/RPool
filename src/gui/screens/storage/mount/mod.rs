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
mod log;
mod maintenance;
mod options;
mod status_bar;
#[cfg(test)]
mod tests;
pub(crate) mod transitions;
mod view;

use args::build_args;
pub(crate) use form::MountForm;
pub(crate) use view::show;
