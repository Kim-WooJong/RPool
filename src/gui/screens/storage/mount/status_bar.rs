//! Top of the screen: what is mounted where, and the primary actions.
use super::form::MountForm;
use crate::gui::settings::GuiSettings;
use crate::gui::state::GuiState;
use crate::gui::theme;
use crate::gui::widgets::{status_badge, toolbar, StatusTone};
use eframe::egui;

/// Saves this pool's settings, then starts `action`; errors become the notice.
pub(super) fn run(
    form: &mut MountForm,
    settings: &mut GuiSettings,
    action: impl FnOnce(&mut MountForm, &str) -> Result<(), String>,
) {
    let result = form
        .save_mount_settings(settings)
        .and_then(|()| action(form, &settings.rclone));
    if let Err(error) = result {
        form.notice = Some(error);
    }
}

fn mode(form: &MountForm) -> &'static str {
    match (form.virtual_drive, form.pool_sync, form.bounded_shared) {
        (false, _, _) => "full local replica",
        (true, true, _) => "online drive · automatic pool sync",
        (true, false, true) => "online drive · legacy shared (bounded)",
        (true, false, false) if !form.shared_root.trim().is_empty() => {
            "online drive · legacy shared"
        }
        (true, false, false) => "online drive · this PC only",
    }
}

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let (form, settings) = (&mut state.mount, &mut state.settings);
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (label, tone) = match (form.runner.is_running(), form.stopping) {
                (true, true) => ("Stopping", StatusTone::Warning),
                (true, false) => ("Running", StatusTone::Success),
                _ => ("Not mounted", StatusTone::Neutral),
            };
            status_badge(ui, label, tone);
            let pool = if form.pool.is_empty() { "No pool selected" } else { form.pool.as_str() };
            let target = if form.mountpoint.trim().is_empty() { "no mountpoint" } else { form.mountpoint.trim() };
            ui.label(egui::RichText::new(format!("{pool} → {target}")).strong());
            ui.label(egui::RichText::new(mode(form)).weak());
            ui.label(egui::RichText::new(if form.native_selected() { "· native" } else { "· WebDAV" }).weak());
        });
        ui.add_space(theme::SUBSECTION_GAP);
        toolbar(ui, |ui| {
            if form.runner.is_running() {
                if ui
                    .add_enabled(!form.stopping, egui::Button::new("Unmount"))
                    .on_hover_text("Stops gracefully. Pending local changes are kept and upload on the next start or sync.")
                    .clicked()
                {
                    if let Err(error) = form.request_stop() {
                        form.notice = Some(error);
                    }
                }
                if ui
                    .add_enabled(form.stopping, egui::Button::new("Force stop"))
                    .on_hover_text("Only after Unmount hangs. Can interrupt uploads; edits stay in the local cache. Restart the same workspace to recover.")
                    .clicked()
                {
                    form.runner.cancel();
                }
                ui.spinner();
                ui.label(if form.stopping {
                    "Stopping; pending edits stay local."
                } else {
                    "Mounted. Other jobs stay available."
                });
            } else {
                let mount = ui.add(egui::Button::new(egui::RichText::new("Mount").strong()).min_size(egui::vec2(96.0, theme::CONTROL_HEIGHT)));
                if mount.on_hover_text("Mount read/write with the settings below.").clicked() {
                    run(form, settings, |form, rclone| form.start(rclone, false));
                }
                if ui.button("Sync now").on_hover_text("Upload pending changes and fetch metadata without mounting.").clicked() {
                    run(form, settings, |form, rclone| form.start(rclone, true));
                }
                if ui.button("Check capacity").on_hover_text("Measure every account's quota without mounting.").clicked() {
                    run(form, settings, |form, rclone| form.start_action(rclone, 2));
                }
            }
        });
        if let Some(notice) = &form.notice {
            ui.add_space(theme::SUBSECTION_GAP);
            ui.label(notice);
        }
    });
}
