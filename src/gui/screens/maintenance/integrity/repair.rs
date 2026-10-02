//! Repair card of Maintenance › Integrity: pick recoverable groups from the last
//! scrub snapshot and reconstruct them with `rpool repair`.

use super::state::IntegrityForm;
use crate::gui::i18n::tr;
use crate::gui::task::TaskRunner;
use crate::gui::widgets::{status_badge, StatusTone};
use eframe::egui;
use std::ffi::OsString;

/// Draws the group table (only recoverable groups can be ticked) and starts the
/// repair. Requires a snapshot of the same manifest; with an old snapshot that
/// lacks group data it asks for a new scrub. Called by `integrity::show`.
pub(crate) fn show(
    ui: &mut egui::Ui,
    form: &mut IntegrityForm,
    task: &mut TaskRunner,
    rclone: &str,
    workers: usize,
    retries: u32,
) {
    ui.label(egui::RichText::new(tr("Repair")).strong());
    let Some(snapshot) = form.snapshot.as_ref() else {
        ui.label(egui::RichText::new(tr("Run a scrub before selecting repair groups.")).weak());
        return;
    };
    let groups = snapshot.groups.clone();
    let degraded_groups = snapshot.degraded_groups;
    let target_matches = snapshot.manifest_source.trim() == form.manifest.trim();
    if !target_matches {
        ui.label(egui::RichText::new(tr("The selected manifest does not match the last scrub snapshot. Run a new scrub before repair.")).weak());
        return;
    }

    if groups.is_empty() && degraded_groups > 0 {
        ui.label(
            egui::RichText::new(tr(
                "This integrity snapshot predates group-level repair data. Run a new scrub first.",
            ))
            .weak(),
        );
        return;
    }

    ui.horizontal(|ui| {
        if ui.button(tr("Select all recoverable")).clicked() {
            form.selected_groups = groups
                .iter()
                .filter(|group| group.is_recoverable())
                .map(|group| group.group)
                .collect();
        }
        if ui.button(tr("Clear")).clicked() {
            form.selected_groups.clear();
        }
        ui.checkbox(&mut form.repair_dry_run, tr("Dry run"));
    });

    egui::Grid::new("repair-groups-grid")
        .striped(true)
        .show(ui, |ui| {
            ui.strong(tr("Repair"));
            ui.strong(tr("Group"));
            ui.strong(tr("Status"));
            ui.strong(tr("Bad shards"));
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

    ui.small(tr("Repair always performs a full BLAKE3 preflight before reconstruction, even when the last scrub was Quick."));
    let can_start =
        !task.is_running() && !form.manifest.trim().is_empty() && !form.selected_groups.is_empty();
    if ui
        .add_enabled(can_start, egui::Button::new(tr("Repair selected groups")))
        .clicked()
    {
        form.error = start(form, task, rclone, workers, retries).err();
        form.notice = None;
    }
}

/// Starts the "Repair" task with `args`.
fn start(
    form: &IntegrityForm,
    task: &mut TaskRunner,
    rclone: &str,
    workers: usize,
    retries: u32,
) -> Result<(), String> {
    task.start_rpool("Repair", rclone, args(form, workers, retries))
}

/// `rpool repair <manifest> --workers N --retries N [--dry-run] [--quick]
/// --group G…` for the selected groups; `--quick` follows the scrub mode.
fn args(form: &IntegrityForm, workers: usize, retries: u32) -> Vec<OsString> {
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
    // Repair re-checks with the same depth the scrub above used.
    if form.quick {
        args.push(OsString::from("--quick"));
    }
    for group in &form.selected_groups {
        args.push(OsString::from("--group"));
        args.push(OsString::from(group.to_string()));
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_uses_the_scrub_depth() {
        use clap::Parser;
        let mut form = IntegrityForm {
            manifest: "a.json".into(),
            ..Default::default()
        };
        assert!(!args(&form, 4, 2).iter().any(|a| a == "--quick"));
        form.quick = true;
        let argv = args(&form, 4, 2);
        assert!(argv.iter().any(|a| a == "--quick"));
        let argv = std::iter::once(OsString::from("rpool")).chain(argv);
        assert!(crate::cli::Cli::try_parse_from(argv).is_ok());
    }
}
