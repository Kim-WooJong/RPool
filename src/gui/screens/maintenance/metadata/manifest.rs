use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::widgets::{local_file_field, output_file_field, remote_selector};
use eframe::egui;
use std::ffi::OsString;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    ui.label(egui::RichText::new("Manifest replicas").strong());
    ui.label(
        egui::RichText::new(
            "Verify, replicate, or recover the metadata required to reconstruct an archive.",
        )
        .weak(),
    );

    local_file_field(
        ui,
        "Reference manifest",
        &mut state.manifest.reference,
        Some("rpool manifest"),
        &["json"],
    );
    pool_selector(ui, &state.pool_names, &mut state.manifest.pool_name);

    ui.horizontal(|ui| {
        if ui
            .add_enabled(!task.is_running(), egui::Button::new("Verify replicas"))
            .clicked()
        {
            state.manifest.error = start_reference_action(state, task, "verify").err();
            state.manifest.notice = None;
        }
        if ui
            .add_enabled(!task.is_running(), egui::Button::new("Replicate / repair"))
            .clicked()
        {
            state.manifest.error = start_reference_action(state, task, "replicate").err();
            state.manifest.notice = None;
        }
    });

    ui.collapsing("Recover manifest", |ui| {
        ui.horizontal(|ui| {
            ui.label("Archive ID");
            ui.add(
                egui::TextEdit::singleline(&mut state.manifest.recovery_archive_id)
                    .desired_width(360.0),
            );
        });
        output_file_field(
            ui,
            "Output manifest (optional)",
            &mut state.manifest.recovery_output,
        );

        if state.manifest.pool_name.is_empty() {
            remote_selector(
                ui,
                &mut state.manifest.recovery_remotes,
                &state.crypt_remotes,
                &state.settings.default_remote_path,
                &state.remote_roots,
                &mut state.manifest.manual_remote,
            );
        } else {
            ui.label(format!(
                "Recovery will search pool '{}'.",
                state.manifest.pool_name
            ));
        }

        if ui
            .add_enabled(!task.is_running(), egui::Button::new("Recover manifest"))
            .clicked()
        {
            state.manifest.error = start_recovery(state, task).err();
            state.manifest.notice = None;
        }
    });
}

fn pool_selector(ui: &mut egui::Ui, pool_names: &[String], selected: &mut String) {
    ui.horizontal(|ui| {
        ui.label("Target pool (optional)");
        egui::ComboBox::from_id_salt("manifest-pool")
            .selected_text(if selected.is_empty() {
                "Derive from manifest"
            } else {
                selected.as_str()
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(selected, String::new(), "Derive from manifest");
                for name in pool_names {
                    ui.selectable_value(selected, name.clone(), name.as_str());
                }
            });
    });
}

fn start_reference_action(
    state: &GuiState,
    task: &mut TaskRunner,
    action: &str,
) -> Result<(), String> {
    let reference = state.manifest.reference.trim();
    if reference.is_empty() {
        return Err("Select or enter a reference manifest first.".to_string());
    }

    let mut args = vec![
        OsString::from("manifest"),
        OsString::from(action),
        OsString::from(reference),
    ];
    if !state.manifest.pool_name.is_empty() {
        args.push(OsString::from("--pool"));
        args.push(OsString::from(state.manifest.pool_name.trim()));
    }
    if action == "replicate" {
        args.push(OsString::from("--retries"));
        args.push(OsString::from(state.settings.retries.to_string()));
    }
    task.start_rpool("Manifest", &state.settings.rclone, args)
}

fn start_recovery(state: &GuiState, task: &mut TaskRunner) -> Result<(), String> {
    let archive_id = state.manifest.recovery_archive_id.trim();
    if archive_id.is_empty() {
        return Err("Enter an archive ID first.".to_string());
    }
    if state.manifest.pool_name.is_empty() && state.manifest.recovery_remotes.is_empty() {
        return Err("Select a pool or add at least one recovery remote.".to_string());
    }

    let mut args = vec![
        OsString::from("manifest"),
        OsString::from("recover"),
        OsString::from(archive_id),
    ];
    if !state.manifest.pool_name.is_empty() {
        args.push(OsString::from("--pool"));
        args.push(OsString::from(state.manifest.pool_name.trim()));
    } else {
        for remote in &state.manifest.recovery_remotes {
            if !remote.trim().is_empty() {
                args.push(OsString::from("--remote"));
                args.push(OsString::from(remote.trim()));
            }
        }
    }
    if !state.manifest.recovery_output.trim().is_empty() {
        args.push(OsString::from("--output"));
        args.push(OsString::from(state.manifest.recovery_output.trim()));
    }
    task.start_rpool("Manifest recovery", &state.settings.rclone, args)
}
