//! Mount screen: status and primary actions first, essential drive settings,
//! a capacity verdict, account identities, then advanced options and the log.
mod advanced;
mod args;
mod cache_recovery;
mod capacity_panel;
mod conflicts;
mod drive_section;
mod form;
mod identity_summary;
mod log;
mod status_bar;
#[cfg(test)]
mod tests;
mod view;

use args::build_args;
pub(crate) use form::MountForm;
pub(crate) use view::show;
