use crate::gui::theme;
use eframe::egui;

pub(crate) fn section_header(ui: &mut egui::Ui, title: &str, description: Option<&str>) {
    theme::page_header(ui, title, description);
}
