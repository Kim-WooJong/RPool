use super::manual_options;
use super::state::UploadTargetMode;
use super::target;
use crate::gui::i18n::tr;
use crate::gui::state::GuiState;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    egui::CollapsingHeader::new(tr("Advanced"))
        .id_salt("upload-advanced")
        .default_open(false)
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    tr("Normal uploads should use a storage pool. Manual destinations and one-off policy overrides are available here."),
                )
                .weak(),
            );

            ui.add_space(8.0);
            ui.label(egui::RichText::new(tr("Upload target")).strong());
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut state.upload.target_mode,
                    UploadTargetMode::Pool,
                    tr("Storage pool"),
                );
                ui.selectable_value(
                    &mut state.upload.target_mode,
                    UploadTargetMode::Manual,
                    tr("Manual one-off"),
                );
            });

            ui.add_space(6.0);
            match state.upload.target_mode {
                UploadTargetMode::Pool => pool_override(ui, state),
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
    ui.label(egui::RichText::new(tr("Archive identity")).strong());
    if state.upload.items.len() <= 1 {
        egui::Grid::new("upload-advanced-archive-id")
            .num_columns(2)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                ui.label(tr("Archive ID"));
                ui.add(
                    egui::TextEdit::singleline(&mut state.upload.archive_id)
                        .desired_width(280.0)
                        .hint_text(tr("Generated automatically when empty")),
                );
                ui.end_row();
            });
    } else {
        state.upload.archive_id.clear();
        ui.label(
            egui::RichText::new(tr(
                "Archive IDs are generated automatically for multi-file uploads.",
            ))
            .weak(),
        );
    }
}

/// Pool mode: optionally override the pool's policy for this upload only.
fn pool_override(ui: &mut egui::Ui, state: &mut GuiState) {
    let pool = state
        .pool_definitions
        .get(state.upload.pool_name.trim())
        .cloned();
    let mut on = state.upload.policy_override.is_some();
    let toggle = ui.add_enabled(
        pool.is_some(),
        egui::Checkbox::new(&mut on, tr("Override the pool's policy for this upload")),
    );
    if toggle.changed() {
        state.upload.policy_override = on
            .then(|| pool.as_ref().map(super::state::UploadPolicy::of_pool))
            .flatten();
    }
    let Some(policy) = &mut state.upload.policy_override else {
        crate::gui::theme::hint(ui, tr("The selected pool owns destinations, erasure-coding policy, placement, workers and retries."));
        return;
    };
    egui::Grid::new("upload-pool-override")
        .num_columns(2)
        .spacing([16.0, 8.0])
        .show(ui, |ui| {
            ui.label(tr("Shard size (MiB)"));
            ui.add(
                egui::DragValue::new(&mut policy.shard_mib)
                    .range(1..=crate::config::constants::MAX_SHARD_MIB),
            );
            ui.end_row();
            ui.label(tr("Data shards (K)"));
            ui.add(egui::DragValue::new(&mut policy.data_shards).range(1..=255));
            ui.end_row();
            ui.label(tr("Parity shards (M)"));
            ui.add(egui::DragValue::new(&mut policy.parity_shards).range(0..=254));
            ui.end_row();
            ui.label(tr("Placement"));
            egui::ComboBox::from_id_salt("upload-override-placement")
                .selected_text(policy.placement.label())
                .show_ui(ui, |ui| {
                    use crate::models::Placement;
                    for placement in [
                        Placement::RoundRobin,
                        Placement::FreeRatio,
                        Placement::Resilient,
                        Placement::CapacityFirst,
                    ] {
                        ui.selectable_value(&mut policy.placement, placement, placement.label());
                    }
                });
            ui.end_row();
            ui.label(tr("Workers"));
            ui.add(egui::DragValue::new(&mut policy.workers).range(1..=256));
            ui.end_row();
            ui.label(tr("Retries"));
            ui.add(egui::DragValue::new(&mut policy.retries).range(0..=100));
            ui.end_row();
        });
    crate::gui::theme::hint(ui, tr("Only this upload uses these values; the pool and your saved defaults are unchanged. Destinations still come from the pool."));
}
