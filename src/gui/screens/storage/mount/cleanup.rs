//! Drive › History cleanup: preview → confirm → delete obsolete versions,
//! for local online drives only.
use super::status_bar::run;
use crate::gui::state::GuiState;
use crate::gui::theme;
use eframe::egui;

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let (form, settings) = (&mut state.mount, &mut state.settings);
    if !form.retention_allowed() {
        theme::card_section(
            ui,
            "History cleanup",
            None,
            |_| {},
            |ui| {
                theme::hint(ui, "Available for online drives with Sync = This PC only. Pool-sync drives collect history automatically with v7 (Options › Version history).");
            },
        );
        return;
    }
    ui.add_enabled_ui(!form.runner.is_running(), |ui| {
        theme::card_section(ui, "Delete obsolete versions", Some("Removes tracked obsolete versions from the cloud for this workspace. Current files, pending writes and unresolved conflicts are kept."), |_| {}, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Keep previous versions per file");
                ui.add(egui::DragValue::new(&mut form.keep_previous).range(0..=10000));
            });
            ui.add_space(theme::SUBSECTION_GAP);
            ui.label(egui::RichText::new("Step 1").strong());
            if ui
                .button("Preview obsolete versions")
                .on_hover_text("Lists what would be removed. Nothing is uploaded or deleted.")
                .clicked()
            {
                run(form, settings, |form, rclone| form.start_action(rclone, 7));
            }
            let previewed = form.retention_previewed.as_ref() == Some(&form.retention_key());
            ui.label(egui::RichText::new("Step 2").strong());
            ui.add_enabled_ui(previewed, |ui| {
                ui.checkbox(
                    &mut form.retention_confirmed,
                    "These archives belong only to this workspace (no other PC or pool uses them)",
                );
            });
            if !previewed {
                theme::hint(ui, "Run the preview for the current limit first; changing the limit, pool or workspace needs a new preview.");
            }
            ui.label(egui::RichText::new("Step 3").strong());
            if theme::danger_button(ui, form.retention_ready(), "Delete obsolete versions")
                .on_hover_text("Deletes exact remote objects. Resumable; provider trash or versioning may delay quota recovery.")
                .clicked()
            {
                run(form, settings, |form, rclone| form.start_action(rclone, 8));
            }
        });
    });
}
