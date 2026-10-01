mod data;
mod details;
mod drive;
mod drive_state;
mod explorer;
mod filters;
pub(crate) mod history;
mod rebuild;
mod state;
mod table;

use crate::gui::i18n::{tr, trf};
#[cfg(any(test, debug_assertions))]
pub(crate) use explorer::sample as drive_sample;
pub(crate) use state::InventoryForm;
use state::LibraryView;

use crate::gui::state::{FilesSection, GuiState};
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::gui::widgets::{section_header, toolbar};
use crate::presentation::format_bytes;
use details::InventoryAction;
use eframe::egui;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    section_header(
        ui,
        tr("Library"),
        Some(tr(
            "Browse the folders and files on a pool's drive, or the archives uploaded to it.",
        )),
    );
    theme::tabs(
        ui,
        &mut state.inventory.view,
        &[
            (LibraryView::Drive, tr("Drive files")),
            (LibraryView::Archives, tr("Uploaded archives")),
        ],
    );
    state.inventory.drive.sync_pools(&state.pool_names);

    let view = state.inventory.view;
    let mut switch_to_upload = false;
    toolbar(ui, |ui| {
        pool_picker(ui, &state.pool_names, &mut state.inventory.drive);
        match view {
            LibraryView::Drive => {
                drive::toolbar(ui, &mut state.inventory.drive, &state.settings.rclone)
            }
            LibraryView::Archives => {
                if ui.button(tr("Refresh")).clicked() {
                    state.inventory.refresh();
                }
                if ui.button(tr("Upload")).clicked() {
                    switch_to_upload = true;
                }
                if ui.button(tr("Rebuild…")).clicked() {
                    state.inventory.show_rebuild = !state.inventory.show_rebuild;
                }
            }
        }
    });
    if switch_to_upload {
        state.files_section = FilesSection::Upload;
        return;
    }

    ui.add_space(theme::SUBSECTION_GAP);
    if state.pool_names.is_empty() {
        theme::hint(ui, tr("No pools yet: create one in Storage › Pools first."));
        return;
    }
    match view {
        LibraryView::Drive => {
            drive::body(ui, &mut state.inventory.drive, task, &state.settings.rclone);
        }
        LibraryView::Archives => archives(ui, state, task),
    }
}

/// Required pool picker: no "All pools"; the only pool is preselected.
fn pool_picker(ui: &mut egui::Ui, pools: &[String], form: &mut drive_state::DriveForm) {
    let mut selected = form.pool.clone();
    egui::ComboBox::from_id_salt("library-pool")
        .selected_text(if selected.is_empty() {
            tr("Choose a pool…")
        } else {
            selected.as_str()
        })
        .show_ui(ui, |ui| {
            for pool in pools {
                ui.selectable_value(&mut selected, pool.clone(), pool);
            }
        });
    form.select(selected);
}

/// The uploaded-archives inventory of the selected pool.
fn archives(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    if !state.inventory.loaded {
        state.inventory.refresh();
    }
    if state.inventory.show_rebuild {
        rebuild::show(ui, &mut state.inventory, task, &state.settings.rclone);
        ui.add_space(theme::SUBSECTION_GAP);
    }
    if state.inventory.drive.pool.is_empty() {
        theme::hint(ui, tr("Pick a pool to list the archives uploaded to it."));
        return;
    }
    state.inventory.pool_filter = state.inventory.drive.pool.clone();

    filters::show(ui, &mut state.inventory);
    let outside = state.inventory.outside_pool();
    if outside > 0 {
        theme::hint(
            ui,
            &if outside == 1 {
                trf(
                    "{n} archive of other pools (or matching no pool) not shown.",
                    &[("n", &outside)],
                )
            } else {
                trf(
                    "{n} archives of other pools (or matching no pool) not shown.",
                    &[("n", &outside)],
                )
            },
        );
    }

    if let Some(error) = &state.inventory.error {
        ui.add_space(theme::SUBSECTION_GAP);
        ui.colored_label(theme::error_colors(ui.visuals().dark_mode).1, error);
    }

    let rows = state.inventory.visible_rows();
    let visible_bytes = rows.iter().fold(0_u64, |total, row| {
        total.saturating_add(row.entry.original_size)
    });
    ui.add_space(theme::SUBSECTION_GAP);
    ui.label(
        egui::RichText::new({
            let (n, size) = (rows.len(), format_bytes(visible_bytes));
            if n == 1 {
                trf("{n} file · {size}", &[("n", &n), ("size", &size)])
            } else {
                trf("{n} files · {size}", &[("n", &n), ("size", &size)])
            }
        })
        .weak(),
    );

    ui.add_space(theme::SUBSECTION_GAP);
    let selected = state
        .inventory
        .selected_archive_id
        .as_deref()
        .and_then(|selected| rows.iter().find(|row| row.entry.archive_id == selected))
        .cloned();
    let mut action = None;
    let total_width = ui.available_width();
    if total_width < 760.0 {
        // Narrow: the table on top in its own scroll, details below.
        let height = (ui.available_height() * 0.55).max(200.0);
        ui.allocate_ui(egui::vec2(total_width, height), |ui| {
            ui.set_max_height(height);
            table::show(ui, &rows, &mut state.inventory);
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("inventory-details-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                action = details::show(ui, selected.as_ref());
            });
    } else {
        let detail_width = 300.0_f32.min((total_width * 0.38).max(250.0));
        let table_width = (total_width - detail_width - theme::SECTION_GAP).max(360.0);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width(table_width);
                table::show(ui, &rows, &mut state.inventory);
            });
            ui.separator();
            ui.vertical(|ui| {
                ui.set_width(detail_width);
                egui::ScrollArea::vertical()
                    .id_salt("inventory-details-scroll")
                    .show(ui, |ui| {
                        action = details::show(ui, selected.as_ref());
                    });
            });
        });
    }

    if let (Some(action), Some(row)) = (action, selected) {
        match action {
            InventoryAction::Restore => {
                state.restore.manifest = row.entry.manifest_source;
                state.files_section = FilesSection::Restore;
            }
            InventoryAction::Verify => {
                state.verify.manifest = row.entry.manifest_source;
                state.page = crate::gui::state::Page::Maintenance;
                state.maintenance_section = crate::gui::state::MaintenanceSection::Archive;
            }
            InventoryAction::Status => {
                state.status.manifest = row.entry.manifest_source;
                state.page = crate::gui::state::Page::Maintenance;
                state.maintenance_section = crate::gui::state::MaintenanceSection::Archive;
            }
        }
    }
}
