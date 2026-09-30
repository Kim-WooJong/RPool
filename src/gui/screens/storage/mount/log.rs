//! Mount and write-back log, collapsed unless a mount or sync is running.
use super::form::MountForm;
use crate::gui::theme;
use eframe::egui;

pub(super) fn show(ui: &mut egui::Ui, form: &MountForm) {
    egui::CollapsingHeader::new("Log")
        .id_salt("mount-log-header")
        .default_open(form.runner.is_running())
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("mount-log")
                .max_height(theme::TASK_CONSOLE_HEIGHT)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in form.runner.logs() {
                        ui.monospace(&line.text);
                    }
                });
        });
}
