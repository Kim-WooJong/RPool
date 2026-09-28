use super::manual_options;
use super::state::UploadTargetMode;
use super::target;
use crate::gui::state::GuiState;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    egui::CollapsingHeader::new("Advanced")
        .id_salt("upload-advanced")
        .default_open(false)
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "Normal uploads should use a storage pool. Manual destinations and one-off policy overrides are available here.",
                )
                .weak(),
            );

            ui.add_space(8.0);
            ui.label(egui::RichText::new("Upload target").strong());
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut state.upload.target_mode,
                    UploadTargetMode::Pool,
                    "Storage pool",
                );
                ui.selectable_value(
                    &mut state.upload.target_mode,
                    UploadTargetMode::Manual,
                    "Manual one-off",
                );
            });

            ui.add_space(6.0);
            match state.upload.target_mode {
                UploadTargetMode::Pool => {
                    ui.label(
                        egui::RichText::new(
                            "The selected pool owns destinations, erasure-coding policy, placement, workers, and retries.",
                        )
                        .weak(),
                    );
                }
                UploadTargetMode::Manual => {
                    target::show_manual_destinations(ui, state);
                    manual_options::show(ui, state);
                }
            }

            ui.add_space(10.0);
            show_archive_id(ui, state);
        });
}

fn show_archive_id(ui: &mut egui::Ui, state: &mut GuiState) {
    ui.label(egui::RichText::new("Archive identity").strong());
    if state.upload.items.len() <= 1 {
        egui::Grid::new("upload-advanced-archive-id")
            .num_columns(2)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                ui.label("Archive ID");
                ui.add(
                    egui::TextEdit::singleline(&mut state.upload.archive_id)
                        .desired_width(280.0)
                        .hint_text("Generated automatically when empty"),
                );
                ui.end_row();
            });
    } else {
        state.upload.archive_id.clear();
        ui.label(
            egui::RichText::new("Archive IDs are generated automatically for multi-file uploads.")
                .weak(),
        );
    }
}
