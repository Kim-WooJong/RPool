//! Drive › Maintenance: local cache housekeeping while unmounted.
use super::status_bar::run;
use crate::gui::state::GuiState;
use crate::gui::theme;
use eframe::egui;

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let (form, settings) = (&mut state.mount, &mut state.settings);
    ui.add_enabled_ui(!form.runner.is_running(), |ui| {
        theme::card_section(
            ui,
            "Local cache",
            Some("Run while unmounted. Cloud data and pending writes are never deleted here."),
            |_| {},
            |ui| {
                if !form.virtual_drive {
                    theme::hint(
                        ui,
                        "A full replica has no shard cache or spool to maintain.",
                    );
                    return;
                }
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button("Trim clean cache")
                        .on_hover_text(
                            "Removes verified clean cache only; pending writes and history stay.",
                        )
                        .clicked()
                    {
                        run(form, settings, |form, rclone| form.start_action(rclone, 4));
                    }
                    if ui
                        .button("Export recoverable spool")
                        .on_hover_text("Copies pending local writes out for recovery.")
                        .clicked()
                    {
                        run(form, settings, |form, rclone| form.start_action(rclone, 5));
                    }
                });
            },
        );
    });
}
