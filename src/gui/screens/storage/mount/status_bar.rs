//! Top of the screen: what is mounted where, and the primary actions.
use super::form::MountForm;
use super::sessions::Conflict;
use crate::gui::i18n::{tr, trf};
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
        form.session.notice = Some(error);
    }
}

/// Why Mount is unavailable, with a free drive letter to take if one is.
fn conflict_line(ui: &mut egui::Ui, form: &mut MountForm, conflict: &Conflict) {
    ui.add_space(theme::SUBSECTION_GAP);
    ui.horizontal_wrapped(|ui| {
        let color = theme::warning_colors(ui.visuals().dark_mode).1;
        ui.colored_label(color, conflict.message());
        if let Conflict::Mountpoint(_, Some(free)) = conflict {
            if ui
                .button(trf("Use {letter}", &[("letter", free)]))
                .clicked()
            {
                form.mountpoint = free.clone();
            }
        }
    });
}

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let (form, settings) = (&mut state.mount, &mut state.settings);
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            let (label, tone) = match (form.session.runner.is_running(), form.session.stopping) {
                (true, true) => (tr("Stopping"), StatusTone::Warning),
                (true, false) => (tr("Running"), StatusTone::Success),
                _ => (tr("Not mounted"), StatusTone::Neutral),
            };
            status_badge(ui, label, tone);
            let pool = if form.pool.is_empty() { tr("No pool selected") } else { form.pool.as_str() };
            let target = if form.mountpoint.trim().is_empty() { tr("no mountpoint") } else { form.mountpoint.trim() };
            ui.label(egui::RichText::new(format!("{pool} › {target}")).strong());
            ui.label(egui::RichText::new(if form.native_selected() { tr("· native") } else { "· WebDAV" }).weak());
        });
        ui.add_space(theme::SUBSECTION_GAP);
        let idle = !form.session.is_running();
        let mount_conflict = idle.then(|| form.conflict(0)).flatten();
        let workspace_busy = idle && form.conflict(1).is_some();
        toolbar(ui, |ui| {
            if form.session.runner.is_running() {
                if ui
                    .add_enabled(!form.session.stopping, egui::Button::new(tr("Unmount")))
                    .on_hover_text(tr("Stops gracefully. Pending local changes are kept and upload on the next start or sync."))
                    .clicked()
                {
                    if let Err(error) = form.request_stop() {
                        form.session.notice = Some(error);
                    }
                }
                if ui
                    .add_enabled(form.session.stopping, egui::Button::new(tr("Force stop")))
                    .on_hover_text(tr("Only after Unmount hangs. Can interrupt uploads; edits stay in the local cache. Restart the same workspace to recover."))
                    .clicked()
                {
                    form.session.runner.cancel();
                }
                ui.spinner();
                ui.label(if form.session.stopping {
                    tr("Stopping; pending edits stay local.")
                } else {
                    tr("Mounted. Other jobs stay available.")
                });
            } else {
                let mount = theme::primary_button(ui, mount_conflict.is_none(), tr("Mount"));
                if mount.on_hover_text(tr("Mount read/write with the settings below.")).clicked() {
                    run(form, settings, |form, rclone| form.start(rclone, false));
                }
                if ui.add_enabled(!workspace_busy, egui::Button::new(tr("Sync now"))).on_hover_text(tr("Upload pending changes and fetch metadata without mounting.")).clicked() {
                    run(form, settings, |form, rclone| form.start(rclone, true));
                }
                if ui.add_enabled(!workspace_busy, egui::Button::new(tr("Check capacity"))).on_hover_text(tr("Measure every account's quota without mounting.")).clicked() {
                    run(form, settings, |form, rclone| form.start_action(rclone, 2));
                }
            }
        });
        if let Some(conflict) = &mount_conflict {
            conflict_line(ui, form, conflict);
        }
        if let Some(notice) = &form.session.notice {
            ui.add_space(theme::SUBSECTION_GAP);
            ui.label(notice);
        }
    });
}
