use crate::gui::state::Page;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, page: &mut Page) {
    ui.heading("Navigation");
    ui.separator();
    nav_button(ui, page, Page::Dashboard, "Dashboard");
    nav_button(ui, page, Page::Files, "Files");
    nav_button(ui, page, Page::Storage, "Storage");
    nav_button(ui, page, Page::Jobs, "Jobs");
    nav_button(ui, page, Page::Maintenance, "Maintenance");
    ui.separator();
    nav_button(ui, page, Page::Settings, "Settings");
}

fn nav_button(ui: &mut egui::Ui, page: &mut Page, target: Page, label: &str) {
    if ui.selectable_label(*page == target, label).clicked() {
        *page = target;
    }
}
