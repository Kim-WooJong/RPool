use super::state::IntegrityForm;
use crate::gui::task::TaskRunner;
use crate::gui::widgets::{status_badge, StatusTone};
use eframe::egui;
use std::ffi::OsString;

pub(crate) fn show(
    ui: &mut egui::Ui,
    form: &mut IntegrityForm,
    task: &mut TaskRunner,
    rclone: &str,
    workers: usize,
    retries: u32,
) {
    ui.label(egui::RichText::new("Repair").strong());
    let Some(snapshot) = form.snapshot.as_ref() else {
        ui.label(egui::RichText::new("Run a scrub before selecting repair groups.").weak());
        return;
    };
    let groups = snapshot.groups.clone();
    let degraded_groups = snapshot.degraded_groups;
    let target_matches = snapshot.manifest_source.trim() == form.manifest.trim();
    if !target_matches {
        ui.label(egui::RichText::new("The selected manifest does not match the last scrub snapshot. Run a new scrub before repair.").weak());
        return;
    }

    if groups.is_empty() && degraded_groups > 0 {
        ui.label(
            egui::RichText::new(
                "This integrity snapshot predates group-level repair data. Run a new scrub first.",
            )
            .weak(),
        );
        return;
    }

    ui.horizontal(|ui| {
        if ui.button("Select all recoverable").clicked() {
            form.selected_groups = groups
                .iter()
                .filter(|group| group.is_recoverable())
                .map(|group| group.group)
                .collect();
        }
        if ui.button("Clear").clicked() {
            form.selected_groups.clear();
        }
        ui.checkbox(&mut form.repair_dry_run, "Dry run");
    });

    egui::Grid::new("repair-groups-grid")
        .striped(true)
        .show(ui, |ui| {
            ui.strong("Repair");
            ui.strong("Group");
            ui.strong("Status");
            ui.strong("Bad shards");
            ui.end_row();
            for group in &groups {
                let enabled = group.is_recoverable();
                let mut selected = form.selected_groups.contains(&group.group);
                if ui
                    .add_enabled(enabled, egui::Checkbox::new(&mut selected, ""))
                    .changed()
                {
                    if selected {
                        form.selected_groups.insert(group.group);
                    } else {
                        form.selected_groups.remove(&group.group);
                    }
                }
                ui.label(group.group.to_string());
                let tone = if group.has_provider_error() || group.is_unrecoverable() {
                    StatusTone::Error
                } else if group.is_recoverable() {
                    StatusTone::Warning
                } else {
                    StatusTone::Success
                };
                status_badge(ui, &group.status, tone);
                ui.label(group.bad_shards.to_string());
                ui.end_row();
            }
        });

    ui.small("Repair always performs a full BLAKE3 preflight before reconstruction, even when the last scrub was Quick.");
    let can_start =
        !task.is_running() && !form.manifest.trim().is_empty() && !form.selected_groups.is_empty();
    if ui
        .add_enabled(can_start, egui::Button::new("Repair selected groups"))
        .clicked()
    {
        form.error = start(form, task, rclone, workers, retries).err();
        form.notice = None;
    }
}

fn start(
    form: &IntegrityForm,
    task: &mut TaskRunner,
    rclone: &str,
    workers: usize,
    retries: u32,
) -> Result<(), String> {
    let mut args = vec![
        OsString::from("repair"),
        OsString::from(form.manifest.trim()),
        OsString::from("--workers"),
        OsString::from(workers.to_string()),
        OsString::from("--retries"),
        OsString::from(retries.to_string()),
    ];
    if form.repair_dry_run {
        args.push(OsString::from("--dry-run"));
    }
    for group in &form.selected_groups {
        args.push(OsString::from("--group"));
        args.push(OsString::from(group.to_string()));
    }
    task.start_rpool("Repair", rclone, args)
}
