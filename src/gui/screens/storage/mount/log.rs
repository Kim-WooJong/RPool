//! Mount and write-back log, in its own bounded scroll.
use super::form::MountForm;
use crate::gui::i18n::{tr, trf};
use crate::gui::theme;
use eframe::egui;

pub(super) fn show(ui: &mut egui::Ui, form: &MountForm) {
    let lines = form.session.runner.logs().len();
    egui::CollapsingHeader::new(trf("Log · {lines} lines", &[("lines", &lines)]))
        .id_salt("mount-log-header")
        .default_open(form.session.runner.is_running())
        .show(ui, |ui| {
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical()
                    .id_salt("mount-log")
                    .max_height(theme::list_height(ui.ctx().content_rect().height()))
                    .auto_shrink([false, true])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if lines == 0 {
                            theme::hint(ui, tr("No output yet."));
                        }
                        for line in form.session.runner.logs() {
                            ui.monospace(&line.text);
                        }
                    });
            });
        });
}
