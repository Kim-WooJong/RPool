use crate::gui::theme;
use eframe::egui;

pub(crate) fn section_header(ui: &mut egui::Ui, title: &str, description: Option<&str>) {
    ui.label(
        egui::RichText::new(title)
            .size(theme::SECTION_TITLE_SIZE)
            .strong(),
    );
    if let Some(description) = description {
        ui.label(egui::RichText::new(description).weak());
    }
    ui.add_space(theme::SUBSECTION_GAP);
}
