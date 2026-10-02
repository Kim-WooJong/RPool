//! Page section header widget.

use crate::gui::theme;
use eframe::egui;

/// Title with optional description; thin wrapper over [`theme::page_header`].
pub(crate) fn section_header(ui: &mut egui::Ui, title: &str, description: Option<&str>) {
    theme::page_header(ui, title, description);
}
