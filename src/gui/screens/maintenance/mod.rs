//! The Maintenance page: Archive check (verify / status forms), Integrity
//! (scrub and repair), Metadata (manifests and inventory) and Diagnostics
//! (`rpool doctor`). Entry points `show` and `handle_task_completion`.

/// Diagnostics tab: runs the doctor checks.
pub(crate) mod diagnostics;
/// Integrity tab: scrub, repair and the integrity summary.
#[path = "integrity/mod.rs"]
pub(crate) mod integrity;
/// Metadata tab: manifest tools and the inventory.
#[path = "metadata/mod.rs"]
pub(crate) mod metadata;

pub(crate) use diagnostics::SystemForm;
pub(crate) use integrity::IntegrityForm;
pub(crate) use metadata::ManifestForm;

use crate::gui::i18n::{tr, trf};
use crate::gui::state::{GuiState, MaintenanceSection};
use crate::gui::task::{JobStatus, TaskRunner};
use eframe::egui;

/// Draws the Maintenance tabs and the selected section. Called by the app for
/// `Page::Maintenance`.
pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    crate::gui::theme::tabs(
        ui,
        &mut state.maintenance_section,
        &[
            (MaintenanceSection::Archive, tr("Archive check")),
            (MaintenanceSection::Integrity, tr("Integrity")),
            (MaintenanceSection::Metadata, tr("Metadata")),
            (MaintenanceSection::Diagnostics, tr("Diagnostics")),
        ],
    );
    match state.maintenance_section {
        MaintenanceSection::Archive => {
            crate::gui::theme::page_body(ui, "health-archive", |ui| {
                crate::gui::screens::files::verify::show(ui, state, task);
                ui.add_space(crate::gui::theme::SECTION_GAP);
                crate::gui::screens::files::status::show(ui, state, task);
            });
        }
        MaintenanceSection::Integrity => integrity::show(ui, state, task),
        MaintenanceSection::Metadata => metadata::show(ui, state, task),
        MaintenanceSection::Diagnostics => diagnostics::show(ui, state, task),
    }
}

/// After a task finishes: refreshes the integrity snapshot after Scrub/Repair
/// and the inventory after an inventory rebuild, and sets the section's notice.
/// Called by the app when the task runner reports a finished job.
pub(crate) fn handle_task_completion(state: &mut GuiState, task: &TaskRunner, status: JobStatus) {
    let Some(last) = task.last_task() else {
        return;
    };
    match last.name.as_str() {
        "Scrub" | "Repair" => {
            state.integrity.refresh_snapshot();
            // Task names stay English (they identify the task); show them translated.
            let name = if last.name == "Scrub" {
                tr("Scrub")
            } else {
                tr("Repair")
            };
            state.integrity.notice = Some(match status {
                JobStatus::Completed => trf(
                    "{task} completed; integrity summary refreshed.",
                    &[("task", &name)],
                ),
                JobStatus::Cancelled => trf("{task} was cancelled.", &[("task", &name)]),
                JobStatus::Failed => trf(
                    "{task} finished with errors; the latest available integrity snapshot was loaded.",
                    &[("task", &name)],
                ),
                _ => name.to_string(),
            });
        }
        "Inventory rebuild" => {
            state.inventory.refresh();
            state.manifest.notice = Some(match status {
                JobStatus::Completed => {
                    tr("Inventory rebuild completed and Files was refreshed.").to_string()
                }
                JobStatus::Cancelled => tr("Inventory rebuild was cancelled.").to_string(),
                JobStatus::Failed => {
                    tr("Inventory rebuild failed. See Jobs for details.").to_string()
                }
                _ => tr("Inventory rebuild updated.").to_string(),
            });
        }
        _ => {}
    }
}
