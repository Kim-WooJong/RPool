//! Reusable GUI widgets shared by the screens: capacity bars, file pickers,
//! progress, remote selection, headers, badges, the task console and toolbars.

pub(crate) mod account_identities;
/// Horizontal capacity bars (used/free).
mod capacity_bar;
/// Text field with a Browse… file dialog.
mod file_field;
/// Background capacity query and summary for pool options.
pub(crate) mod pool_capacity;
/// Progress bar with bytes, rate and ETA of a task.
mod progress_view;
pub(crate) mod rclone_banner;
/// Editor for a list of crypt remote targets.
mod remote_selector;
/// Page/section title with optional description.
mod section_header;
/// Small colored status pill.
mod status_badge;
/// Bottom console with the running task's output.
mod task_console;
/// Wrapping row of controls.
mod toolbar;

pub(crate) use capacity_bar::{capacity_bar_colored, capacity_bar_sized};
pub(crate) use file_field::{local_file_field, output_file_field};
pub(crate) use progress_view::progress_view;
pub(crate) use remote_selector::remote_selector;
pub(crate) use section_header::section_header;
pub(crate) use status_badge::{status_badge, StatusTone};
pub(crate) use task_console::task_console;
pub(crate) use toolbar::toolbar;
