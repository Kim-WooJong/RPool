//! Drive › Maintenance: local cache housekeeping while unmounted.
use super::status_bar::run;
use crate::gui::i18n::tr;
use crate::gui::state::GuiState;
use crate::gui::theme;
use eframe::egui;

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let (form, settings) = (&mut state.mount, &mut state.settings);
    ui.add_enabled_ui(!form.session.runner.is_running(), |ui| {
        theme::card_section(
            ui,
            tr("Local cache"),
            Some(tr(
                "Run while unmounted. Cloud data and pending writes are never deleted here.",
            )),
            |_| {},
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button(tr("Trim clean cache"))
                        .on_hover_text(tr(
                            "Removes verified clean cache only; pending writes and history stay.",
                        ))
                        .clicked()
                    {
                        run(form, settings, |form, rclone| form.start_action(rclone, 4));
                    }
                    if ui
                        .button(tr("Export recoverable spool"))
                        .on_hover_text(tr("Copies pending local writes out for recovery."))
                        .clicked()
                    {
                        run(form, settings, |form, rclone| form.start_action(rclone, 5));
                    }
                });
            },
        );
    });
}
