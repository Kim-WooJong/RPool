//! The settings every mount needs: pool, workspace, mountpoint and PC name.
use crate::gui::i18n::tr;
use crate::gui::state::GuiState;
use crate::gui::theme;
use eframe::egui;

/// Text field with a "Choose…" folder picker; also used by `transitions`.
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

/// Connection card of the Drive overview: pool, workspace, mountpoint and PC
/// name; the running pool's settings are locked.
pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let form = &mut state.mount;
    let pool_names = &state.pool_names;
    let settings = &mut state.settings;
    theme::card_section(
        ui,
        tr("Connection"),
        Some(tr("Which pool, and where it appears on this PC.")),
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
                            .selected_text(if form.pool.is_empty() {
                                tr("Select pool")
                            } else {
                                &form.pool
                            })
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

                        ui.label(tr("Workspace"));
                        directory_field(
                            ui,
                            &mut form.workspace,
                            tr("Empty folder on this PC, e.g. C:\\RPool\\work"),
                        );
                        ui.end_row();

                        ui.label(if cfg!(windows) {
                            tr("Drive letter")
                        } else {
                            tr("Mount folder")
                        });
                        ui.add(
                            egui::TextEdit::singleline(&mut form.mountpoint)
                                .desired_width(120.0)
                                .hint_text(if cfg!(windows) { "R:" } else { "/mnt/rpool" }),
                        )
                        .on_hover_text(if cfg!(windows) {
                            tr("An unused drive letter. Requires WinFsp.")
                        } else {
                            tr("An existing empty directory. Requires FUSE.")
                        });
                        ui.end_row();

                        ui.label(tr("PC name"));
                        ui.add(
                            egui::TextEdit::singleline(&mut form.pc_name)
                                .desired_width(200.0)
                                .hint_text(tr("optional; generated if empty")),
                        )
                        .on_hover_text(tr("Shown in conflict file names."));
                        ui.end_row();
                    });
            });
            ui.add_space(theme::SUBSECTION_GAP);
            theme::hint(ui, tr("Use the same pool on every PC; sync metadata lives in the pool, no coordinator needed. Saves stay local until the cloud copy is verified."));
        },
    );
}
