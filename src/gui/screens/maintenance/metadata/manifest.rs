use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::gui::widgets::{local_file_field, output_file_field, remote_selector};
use eframe::egui;
use std::ffi::OsString;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    theme::card_section(ui, "Where the manifest replicas live", Some("Used by all three actions below: a pool, or explicit crypt remotes. With neither, the manifest's own providers are used."), |_| {}, |ui| {
        pool_selector(ui, &state.pool_names, &mut state.manifest.pool_name);
        if state.manifest.pool_name.is_empty() {
            remote_selector(
                ui,
                &mut state.manifest.recovery_remotes,
                &state.crypt_remotes,
                &state.settings.default_remote_path,
                &state.remote_roots,
                &mut state.manifest.manual_remote,
            );
        }
    });
    theme::two_up(
        ui,
        &mut (&mut *state, &mut *task),
        |ui, s| check_card(ui, s.0, s.1),
        |ui, s| recover_card(ui, s.0, s.1),
    );
}

fn check_card(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    theme::card_section(
        ui,
        "Check replicas",
        Some(
            "Compare every replica with a reference manifest, and rewrite missing or damaged ones.",
        ),
        |_| {},
        |ui| {
            local_file_field(
                ui,
                "Reference manifest",
                &mut state.manifest.reference,
                Some("rpool manifest"),
                &["json"],
            );
            ui.horizontal_wrapped(|ui| {
                if theme::primary_button(ui, !task.is_running(), "Verify replicas").clicked() {
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
        },
    );
}

fn recover_card(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    theme::card_section(
        ui,
        "Recover a lost manifest",
        Some("Finds an archive's manifest on its providers by archive ID."),
        |_| {},
        |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Archive ID");
                ui.add(
                    egui::TextEdit::singleline(&mut state.manifest.recovery_archive_id)
                        .desired_width(ui.available_width().min(320.0)),
                );
            });
            output_file_field(
                ui,
                "Output manifest (optional)",
                &mut state.manifest.recovery_output,
            );
            if theme::primary_button(ui, !task.is_running(), "Recover manifest").clicked() {
                state.manifest.error = start_recovery(state, task).err();
                state.manifest.notice = None;
            }
        },
    );
}

fn pool_selector(ui: &mut egui::Ui, pool_names: &[String], selected: &mut String) {
    ui.horizontal_wrapped(|ui| {
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
    task.start_rpool(
        "Manifest",
        &state.settings.rclone,
        reference_args(state, action)?,
    )
}

fn reference_args(state: &GuiState, action: &str) -> Result<Vec<OsString>, String> {
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
    } else {
        for remote in &state.manifest.recovery_remotes {
            if !remote.trim().is_empty() {
                args.push(OsString::from("--remote"));
                args.push(OsString::from(remote.trim()));
            }
        }
    }
    if action == "replicate" {
        args.push(OsString::from("--retries"));
        args.push(OsString::from(state.settings.retries.to_string()));
    }
    Ok(args)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replica_actions_use_the_pool_or_explicit_remotes() {
        use clap::Parser;
        let mut state = GuiState::new(Default::default(), Default::default(), Default::default());
        assert!(reference_args(&state, "verify").is_err());
        state.manifest.reference = "a.json".into();
        state.manifest.recovery_remotes = vec!["x_crypt:rpool".into()];
        for action in ["verify", "replicate"] {
            let args = reference_args(&state, action).unwrap();
            assert!(args.iter().any(|a| a == "--remote"), "{action}");
            let argv = std::iter::once(OsString::from("rpool")).chain(args);
            assert!(crate::cli::Cli::try_parse_from(argv).is_ok(), "{action}");
        }
        state.manifest.pool_name = "family".into();
        let args = reference_args(&state, "verify").unwrap();
        assert!(args.iter().any(|a| a == "--pool") && !args.iter().any(|a| a == "--remote"));
    }
}
