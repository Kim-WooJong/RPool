//! The native egui GUI (`rpool gui`): app shell, screens, background tasks,
//! settings, theme, i18n and widgets. Entry point: `launch`.
/// Application shell and frame loop.
mod app;
mod config_tidy;
mod console;
pub(crate) mod i18n;
#[cfg(test)]
mod layout_tests;
mod navigation;
/// One module per page (dashboard, files, storage, monitoring, ...).
mod screens;
/// Saved GUI settings (`gui.json`).
mod settings;
#[cfg(debug_assertions)]
mod snapshot;
/// Screen state shared across pages.
#[path = "state/mod.rs"]
mod state;
/// Background job runner for long operations.
mod task;
mod theme;
/// Background provider discovery and usage refresh.
mod usage_refresh;
/// Reusable UI pieces (status badges, capacity bars, banners, task console).
mod widgets;

pub(crate) use app::launch;
pub(crate) use settings::{load as load_settings, save as save_settings, GuiSettings};
