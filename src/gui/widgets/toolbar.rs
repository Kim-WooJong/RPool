use crate::gui::theme;
use eframe::egui;

pub(crate) fn toolbar<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal_wrapped(|ui| {
        ui.set_min_height(theme::ROW_HEIGHT);
        add_contents(ui)
    })
    .inner
}
