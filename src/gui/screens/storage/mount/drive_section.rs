//! The settings every mount needs: pool, mode, sync, workspace and mountpoint.
use super::form::MountForm;
use crate::gui::state::GuiState;
use crate::gui::theme;
use crate::gui::widgets::section_header;
use eframe::egui;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sync {
    Pool,
    Local,
    SharedRoot,
}

fn sync_mode(form: &MountForm) -> Sync {
    if form.virtual_drive && form.pool_sync {
        Sync::Pool
    } else if form.bounded_shared
        || !form.shared_root.trim().is_empty()
        || !form.worker_name.trim().is_empty()
    {
        Sync::SharedRoot
    } else {
        Sync::Local
    }
}

fn set_sync(form: &mut MountForm, mode: Sync) {
    match mode {
        Sync::Pool => form.pool_sync = true,
        Sync::Local => {
            form.pool_sync = false;
            form.bounded_shared = false;
            form.shared_root.clear();
            form.worker_name.clear();
        }
        Sync::SharedRoot => form.pool_sync = false,
    }
}

pub(super) fn directory_field(ui: &mut egui::Ui, value: &mut String, hint: &str) {
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(360.0)
                .hint_text(hint),
        );
        if ui.button("Choose…").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                *value = path.display().to_string();
            }
        }
    });
}

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let form = &mut state.mount;
    section_header(ui, "Drive", None);
    ui.add_enabled_ui(!form.runner.is_running(), |ui| {
        egui::Grid::new("mount-drive-grid")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                ui.label("Pool");
                let mut selected = form.pool.clone();
                egui::ComboBox::from_id_salt("mount-pool")
                    .width(240.0)
                    .selected_text(if form.pool.is_empty() { "Select pool" } else { &form.pool })
                    .show_ui(ui, |ui| {
                        for name in &state.pool_names {
                            ui.selectable_value(&mut selected, name.clone(), name);
                        }
                    });
                form.select_pool(selected, &mut state.settings);
                ui.end_row();

                ui.label("Mode");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut form.virtual_drive, true, "Online drive (recommended)")
                        .on_hover_text("All files are visible; contents download on demand and clean cache is trimmed automatically. Cloud files are never deleted by trimming.");
                    ui.radio_value(&mut form.virtual_drive, false, "Full local replica")
                        .on_hover_text("Keeps a complete plaintext copy on this PC. Needs disk space for everything.");
                });
                ui.end_row();

                ui.label("Sync");
                let mut mode = sync_mode(form);
                ui.horizontal(|ui| {
                    if form.virtual_drive {
                        ui.radio_value(&mut mode, Sync::Pool, "Automatic pool sync")
                            .on_hover_text("Sync metadata lives in every pool destination. Use the same pool on each PC; no coordinator needed.");
                    }
                    ui.radio_value(&mut mode, Sync::Local, "This PC only");
                    ui.radio_value(&mut mode, Sync::SharedRoot, "Shared root (legacy)")
                        .on_hover_text("Older shared mode through one encrypted shared root and a worker name per PC.");
                });
                if mode != sync_mode(form) {
                    set_sync(form, mode);
                }
                ui.end_row();

                ui.label("Workspace");
                directory_field(ui, &mut form.workspace, "Empty folder on this PC, e.g. C:\\RPool\\work");
                ui.end_row();

                ui.label(if cfg!(windows) { "Drive letter" } else { "Mount folder" });
                ui.add(egui::TextEdit::singleline(&mut form.mountpoint).desired_width(120.0).hint_text(if cfg!(windows) { "R:" } else { "/mnt/rpool" }))
                    .on_hover_text(if cfg!(windows) { "An unused drive letter. Requires WinFsp." } else { "An existing empty directory. Requires FUSE." });
                ui.end_row();

                match mode {
                    Sync::Pool => {
                        ui.label("PC name");
                        ui.add(egui::TextEdit::singleline(&mut form.worker_name).desired_width(200.0).hint_text("optional; generated if empty"))
                            .on_hover_text("Shown in conflict file names.");
                        ui.end_row();
                    }
                    Sync::SharedRoot => {
                        ui.label("Shared root");
                        ui.add(egui::TextEdit::singleline(&mut form.shared_root).desired_width(240.0).hint_text("crypt:teamspace"));
                        ui.end_row();
                        ui.label("Worker name");
                        ui.add(egui::TextEdit::singleline(&mut form.worker_name).desired_width(200.0).hint_text("required, unique per PC"));
                        ui.end_row();
                        if form.virtual_drive {
                            ui.label("");
                            ui.vertical(|ui| {
                                ui.checkbox(&mut form.bounded_shared, "Bounded shared history (latest cloud state wins)");
                                if form.bounded_shared {
                                    ui.checkbox(&mut form.shared_coordinator, "This PC is the one coordinator");
                                    ui.horizontal(|ui| {
                                        ui.label("Previous versions");
                                        ui.add(egui::DragValue::new(&mut form.shared_keep_previous).range(0..=100));
                                    });
                                }
                            });
                            ui.end_row();
                        }
                    }
                    Sync::Local => {}
                }
            });
    });
    ui.add_space(theme::SUBSECTION_GAP);
    ui.small(match (form.virtual_drive, sync_mode(form)) {
        (true, Sync::Pool) => "Use a NEW workspace when switching from a full replica. Saves stay local until the cloud copy is verified.",
        (true, _) => "Served files keep their revision for this mount; incoming changes appear as named copies. Empty folders stay local.",
        (false, _) => "Incoming shared changes are applied when unmounted or at the next start. Protect the local plaintext copy.",
    });
}
