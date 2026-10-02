mod app;
mod config_tidy;
mod console;
pub(crate) mod i18n;
#[cfg(test)]
mod layout_tests;
mod navigation;
mod screens;
mod settings;
#[cfg(debug_assertions)]
mod snapshot;
#[path = "state/mod.rs"]
mod state;
mod task;
mod theme;
mod usage_refresh;
mod widgets;

pub(crate) use app::launch;
pub(crate) use settings::{load as load_settings, save as save_settings, GuiSettings};
