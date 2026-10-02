//! GUI state: the root `GuiState` held by the app, its startup loader and
//! the navigation enums (pages, sections, drive tabs). Used by `gui::app`.

/// The [`GuiState`] struct with every page's form state.
mod app_state;
/// Builds the initial [`GuiState`] from saved settings, pools and remote roots.
mod persistence;
/// Navigation enums for pages and their sub-sections.
mod selection;

pub(crate) use app_state::GuiState;
pub(crate) use persistence::load;
pub(crate) use selection::{DriveTab, FilesSection, MaintenanceSection, Page, StorageSection};
