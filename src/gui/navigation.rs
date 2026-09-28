use crate::gui::state::Page;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, page: &mut Page) {
    ui.add_space(12.0);
    ui.label(egui::RichText::new("RPool").size(26.0).strong());
    ui.label(egui::RichText::new("Storage workspace").small().weak());
    ui.add_space(24.0);
    ui.label(egui::RichText::new("WORKSPACE").small().weak());
    nav_button(ui, page, Page::Dashboard, "Overview");
    nav_button(ui, page, Page::Files, "Files");
    nav_button(ui, page, Page::Storage, "Storage");
    nav_button(ui, page, Page::Jobs, "Activity");
    ui.add_space(18.0);
    ui.label(egui::RichText::new("MANAGE").small().weak());
    nav_button(ui, page, Page::Maintenance, "Maintenance");
    nav_button(ui, page, Page::Settings, "Settings");
}

fn nav_button(ui: &mut egui::Ui, page: &mut Page, target: Page, label: &str) {
    let button = egui::Button::new(label).selected(*page == target);
    if ui.add_sized([ui.available_width(), 38.0], button).clicked() {
        *page = target;
    }
}
