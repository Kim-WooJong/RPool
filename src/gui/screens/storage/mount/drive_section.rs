//! The settings every mount needs: pool, mode, sync, workspace and mountpoint.
use super::form::MountForm;
use crate::gui::i18n::tr;
use crate::gui::state::GuiState;
use crate::gui::theme;
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
                .desired_width((ui.available_width() - 90.0).clamp(140.0, 360.0))
                .hint_text(hint),
        );
        if ui.button(tr("Choose…")).clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                *value = path.display().to_string();
            }
        }
    });
}

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let form = &mut state.mount;
    let pool_names = &state.pool_names;
    let settings = &mut state.settings;
    theme::card_section(
        ui,
        tr("Connection"),
        Some(tr(
            "Which pool, how it syncs, and where it appears on this PC.",
        )),
        |_| {},
        |ui| {
            // Pools switch while one runs; the running pool's settings are locked.
            ui.scope(|ui| {
        egui::Grid::new("mount-drive-grid")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                ui.label(tr("Pool"));
                let mut selected = form.pool.clone();
                egui::ComboBox::from_id_salt("mount-pool")
                    .width(240.0)
                    .selected_text(if form.pool.is_empty() { tr("Select pool") } else { &form.pool })
                    .show_ui(ui, |ui| {
                        for name in pool_names {
                            ui.selectable_value(&mut selected, name.clone(), name);
                        }
                    });
                form.select_pool(selected, settings);
                ui.end_row();
                if form.session.is_running() {
                    ui.disable();
                }

                ui.label(tr("Mode"));
                ui.horizontal_wrapped(|ui| {
                    ui.radio_value(&mut form.virtual_drive, true, tr("Online drive (recommended)"))
                        .on_hover_text(tr("All files are visible; contents download on demand and clean cache is trimmed automatically. Cloud files are never deleted by trimming."));
                    ui.radio_value(&mut form.virtual_drive, false, tr("Full local replica"))
                        .on_hover_text(tr("Keeps a complete plaintext copy on this PC. Needs disk space for everything."));
                });
                ui.end_row();

                ui.label(tr("Sync"));
                let mut mode = sync_mode(form);
                ui.horizontal_wrapped(|ui| {
                    if form.virtual_drive {
                        ui.radio_value(&mut mode, Sync::Pool, tr("Automatic pool sync"))
                            .on_hover_text(tr("Sync metadata lives in every pool destination. Use the same pool on each PC; no coordinator needed."));
                    }
                    ui.radio_value(&mut mode, Sync::Local, tr("This PC only"));
                    ui.radio_value(&mut mode, Sync::SharedRoot, tr("Shared root (legacy)"))
                        .on_hover_text(tr("Older shared mode through one encrypted shared root and a worker name per PC."));
                });
                if mode != sync_mode(form) {
                    set_sync(form, mode);
                }
                ui.end_row();

                ui.label(tr("Workspace"));
                directory_field(ui, &mut form.workspace, tr("Empty folder on this PC, e.g. C:\\RPool\\work"));
                ui.end_row();

                ui.label(if cfg!(windows) { tr("Drive letter") } else { tr("Mount folder") });
                ui.add(egui::TextEdit::singleline(&mut form.mountpoint).desired_width(120.0).hint_text(if cfg!(windows) { "R:" } else { "/mnt/rpool" }))
                    .on_hover_text(if cfg!(windows) { tr("An unused drive letter. Requires WinFsp.") } else { tr("An existing empty directory. Requires FUSE.") });
                ui.end_row();

                match mode {
                    Sync::Pool => {
                        ui.label(tr("PC name"));
                        ui.add(egui::TextEdit::singleline(&mut form.worker_name).desired_width(200.0).hint_text(tr("optional; generated if empty")))
                            .on_hover_text(tr("Shown in conflict file names."));
                        ui.end_row();
                    }
                    Sync::SharedRoot => {
                        ui.label(tr("Shared root"));
                        ui.add(egui::TextEdit::singleline(&mut form.shared_root).desired_width(240.0).hint_text("crypt:teamspace"));
                        ui.end_row();
                        ui.label(tr("Worker name"));
                        ui.add(egui::TextEdit::singleline(&mut form.worker_name).desired_width(200.0).hint_text(tr("required, unique per PC")));
                        ui.end_row();
                        if form.virtual_drive {
                            ui.label("");
                            ui.vertical(|ui| {
                                ui.checkbox(&mut form.bounded_shared, tr("Bounded shared history (latest cloud state wins)"));
                                if form.bounded_shared {
                                    ui.checkbox(&mut form.shared_coordinator, tr("This PC is the one coordinator"));
                                    ui.horizontal(|ui| {
                                        ui.label(tr("Previous versions"));
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
            theme::hint(ui, match (form.virtual_drive, sync_mode(form)) {
        (true, Sync::Pool) => tr("Use a NEW workspace when switching from a full replica. Saves stay local until the cloud copy is verified."),
        (true, _) => tr("Served files keep their revision for this mount; incoming changes appear as named copies. Empty folders stay local."),
        (false, _) => tr("Incoming shared changes are applied when unmounted or at the next start. Protect the local plaintext copy."),
    });
        },
    );
}
