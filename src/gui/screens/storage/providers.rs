use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::widgets::{local_file_field, output_file_field};
use eframe::egui;
use std::ffi::OsString;

#[derive(Debug, Default)]
pub(crate) struct ProviderForm {
    pub(crate) connection: Option<crate::provider::onboarding::ConnectionSetup>,
    pub(crate) missing_encryption: Vec<String>,
    pub(crate) discovery_known: bool,
    pub(crate) encryption_failed: bool,
    pub(crate) refresh_requested: bool,
    pub(crate) setup_open: bool,
    pub(crate) crypt_name: String,
    pub(crate) backing_provider: String,
    pub(crate) crypt_root: String,
    pub(crate) entropy_index: usize,
    pub(crate) filename_index: usize,
    pub(crate) disable_directory_encryption: bool,
    pub(crate) backup_acknowledged: bool,
    pub(crate) setup_notice: Option<String>,
    pub(crate) health_pool: String,
    pub(crate) manifest: String,
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) output: String,
    pub(crate) dry_run: bool,
    pub(crate) delete_source: bool,
    pub(crate) allow_risky: bool,
    pub(crate) error: Option<String>,
}

impl ProviderForm {
    fn apply_defaults(&mut self, defaults: &crate::config_sync::provision::EncryptionDefaults) {
        self.crypt_root = defaults.root.clone();
        self.entropy_index = [256, 128, 512, 1024]
            .iter()
            .position(|v| *v == defaults.entropy_bits)
            .unwrap_or(3);
        self.filename_index = ["standard", "obfuscate", "off"]
            .iter()
            .position(|v| *v == defaults.filename_encryption)
            .unwrap_or(0);
        self.disable_directory_encryption = !defaults.directory_encryption;
        self.backup_acknowledged = false;
    }
}

fn encryption_status(known: bool, missing: bool, running: bool, failed: bool) -> &'static str {
    if !known {
        "Encryption status unknown"
    } else if !missing {
        "Encryption configured"
    } else if running {
        "Setting up encryption…"
    } else if failed {
        "Setup incomplete — retry"
    } else {
        "Encryption setup pending"
    }
}

pub(crate) fn start_automatic_encryption(
    state: &GuiState,
    task: &mut TaskRunner,
) -> Result<(), String> {
    state
        .settings
        .encryption
        .validate()
        .map_err(|e| e.to_string())?;
    let mut args = vec![
        OsString::from("provider"),
        OsString::from("ensure-encryption"),
    ];
    args.extend(state.settings.encryption.ensure_args());
    task.start_rpool(
        crate::gui::app::AUTO_ENCRYPTION_TASK,
        &state.settings.rclone,
        args,
    )
}

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    ui.heading("Providers");
    ui.label("Check provider health and migrate shards away from a provider without changing archive data.");
    ui.horizontal(|ui| {
        if ui.add_enabled(!task.is_running() && state.providers.connection.is_none(), egui::Button::new("+ Connect cloud provider")).clicked() {
            state.providers.setup_notice = Some(match crate::provider::onboarding::open_setup(&state.settings.rclone) {
                Ok(connection) => {
                    state.providers.connection = Some(connection);
                    "rclone setup opened in your terminal. Complete the login, then quit the wizard. Providers will refresh automatically when the wizard closes.".into()
                },
                Err(error) => error.to_string(),
            });
        }
        if ui.add_enabled(!task.is_running() && state.providers.connection.is_none(), egui::Button::new("Set up encryption")).clicked() {
            state.providers.apply_defaults(&state.settings.encryption);
            state.providers.setup_open = true;
        }
        if ui.add_enabled(!task.is_running() && state.providers.connection.is_none(), egui::Button::new("Retry automatic encryption")).clicked() {
            state.providers.encryption_failed = false;
            state.providers.setup_notice = Some(match start_automatic_encryption(state, task) {
                Ok(()) => "Checking encryption for connected providers. See Jobs for progress.".into(),
                Err(error) => error,
            });
        }
        if ui.add_enabled(!task.is_running(), egui::Button::new("Refresh providers")).clicked() {
            state.providers.refresh_requested = true;
        }
    });
    if state.providers.connection.is_some() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Waiting for the connection wizard to close…");
            if ui.small_button("Wizard already closed — refresh").on_hover_text("Use only after closing the external rclone wizard if completion was not detected.").clicked() {
                state.providers.connection = None;
                state.providers.refresh_requested = true;
            }
        });
    }
    if let Some(notice) = &state.providers.setup_notice {
        ui.label(notice);
    }
    ui.small("Encryption is set up automatically for connected base providers. Back up your rclone configuration to preserve generated keys.");
    if !state.backing_remotes.is_empty() {
        egui::ScrollArea::vertical()
            .id_salt("base-provider-list")
            .max_height(120.0)
            .show(ui, |ui| {
                for provider in &state.backing_remotes {
                    ui.horizontal(|ui| {
                        ui.strong(provider);
                        let missing = state.providers.missing_encryption.contains(provider);
                        ui.label(encryption_status(
                            state.providers.discovery_known, missing,
                            task.is_running() && task.task_name() == Some(crate::gui::app::AUTO_ENCRYPTION_TASK),
                            state.providers.encryption_failed,
                        )).on_hover_text("Configuration status only; cloud access and key recovery are not verified by this indicator.");
                    });
                }
            });
    }
    encryption_dialog(ui.ctx(), state, task);
    ui.separator();

    let size = egui::vec2(
        ui.available_width(),
        ((ui.available_height() - ui.spacing().item_spacing.y) / 2.0).max(1.0),
    );
    super::pools::pane(ui, "provider-health-scroll", size, |ui| {
        ui.heading("Health monitor");
        ui.horizontal(|ui| {
            ui.label("Pool (optional)");
            egui::ComboBox::from_id_salt("provider-health-pool")
                .selected_text(if state.providers.health_pool.is_empty() {
                    "All crypt remotes"
                } else {
                    state.providers.health_pool.as_str()
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut state.providers.health_pool,
                        String::new(),
                        "All crypt remotes",
                    );
                    for name in &state.pool_names {
                        ui.selectable_value(
                            &mut state.providers.health_pool,
                            name.clone(),
                            name.as_str(),
                        );
                    }
                });
            if ui
                .add_enabled(!task.is_running(), egui::Button::new("Check health"))
                .clicked()
            {
                state.providers.error = start_health(state, task).err();
            }
        });
    });
    super::pools::pane(ui, "provider-migration-scroll", size, |ui| {
        ui.set_min_width(380.0);
        ui.separator();
        ui.heading("Drain / migrate provider");
        local_file_field(
            ui,
            "Manifest (local file or remote path)",
            &mut state.providers.manifest,
            Some("rpool manifest"),
            &["json"],
        );
        ui.horizontal(|ui| {
            ui.label("From");
            ui.add(
                egui::TextEdit::singleline(&mut state.providers.from)
                    .hint_text("old-crypt:rpool")
                    .desired_width(280.0),
            );
        });
        ui.horizontal(|ui| {
            ui.label("To");
            ui.add(
                egui::TextEdit::singleline(&mut state.providers.to)
                    .hint_text("new-crypt:rpool")
                    .desired_width(280.0),
            );
        });
        output_file_field(
            ui,
            "Output manifest (optional for local input)",
            &mut state.providers.output,
        );
        ui.vertical(|ui| {
            ui.checkbox(&mut state.providers.dry_run, "Dry run");
            ui.checkbox(
                &mut state.providers.delete_source,
                "Delete old shards after verified migration",
            );
            ui.checkbox(
                &mut state.providers.allow_risky,
                "Allow unverified / weaker failure-domain safety",
            );
        });
        if ui
            .add_enabled(!task.is_running(), egui::Button::new("Drain provider"))
            .clicked()
        {
            state.providers.error = start_drain(state, task).err();
        }

        ui.label("The destination must be an rclone crypt remote. Drain copies and fully verifies new shard objects before manifest replacement; source deletion is optional and occurs last.");
        if let Some(error) = &state.providers.error {
            ui.label(error);
        }
    });
}

fn start_health(state: &GuiState, task: &mut TaskRunner) -> Result<(), String> {
    let mut args = vec![
        OsString::from("provider"),
        OsString::from("health"),
        OsString::from("--workers"),
        OsString::from(state.settings.workers.to_string()),
    ];
    if !state.providers.health_pool.is_empty() {
        args.push(OsString::from("--pool"));
        args.push(OsString::from(state.providers.health_pool.trim()));
    }
    task.start_rpool("Provider health", &state.settings.rclone, args)
}

fn start_drain(state: &GuiState, task: &mut TaskRunner) -> Result<(), String> {
    let manifest = state.providers.manifest.trim();
    let from = state.providers.from.trim();
    let to = state.providers.to.trim();
    if manifest.is_empty() || from.is_empty() || to.is_empty() {
        return Err("Manifest, From, and To are required.".to_string());
    }
    let mut args = vec![
        OsString::from("provider"),
        OsString::from("drain"),
        OsString::from(manifest),
        OsString::from("--from"),
        OsString::from(from),
        OsString::from("--to"),
        OsString::from(to),
        OsString::from("--workers"),
        OsString::from(state.settings.workers.to_string()),
        OsString::from("--retries"),
        OsString::from(state.settings.retries.to_string()),
    ];
    if !state.providers.output.trim().is_empty() {
        args.push(OsString::from("--output"));
        args.push(OsString::from(state.providers.output.trim()));
    }
    if state.providers.dry_run {
        args.push(OsString::from("--dry-run"));
    }
    if state.providers.delete_source {
        args.push(OsString::from("--delete-source"));
    }
    if state.providers.allow_risky {
        args.push(OsString::from("--allow-risky"));
    }
    task.start_rpool("Provider drain", &state.settings.rclone, args)
}

fn encryption_dialog(ctx: &egui::Context, state: &mut GuiState, task: &mut TaskRunner) {
    if !state.providers.setup_open {
        return;
    }
    let mut open = true;
    egui::Window::new("New encrypted provider").id(egui::Id::new("provider-encryption-setup"))
        .open(&mut open).resizable(true).default_width(530.0).show(ctx, |ui| {
        egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
            ui.label("Create a new crypt provider around a connected cloud. Existing keys and files are never changed.");
            ui.add_enabled_ui(!task.is_running() && state.providers.connection.is_none(), |ui| {
                ui.label("Backing provider (non-crypt remote name)");
                egui::ComboBox::from_id_salt("crypt-backing-choice")
                    .selected_text(if state.providers.backing_provider.is_empty() { "Choose provider" } else { &state.providers.backing_provider })
                    .show_ui(ui, |ui| {
                        for name in &state.backing_remotes {
                            ui.selectable_value(&mut state.providers.backing_provider, name.clone(), name.as_str());
                        }
                    });
                ui.add(egui::TextEdit::singleline(&mut state.providers.backing_provider).hint_text("e.g. google_1 (no colon)"));
                ui.label("New encrypted provider name");
                ui.add(egui::TextEdit::singleline(&mut state.providers.crypt_name).hint_text("e.g. google_1_crypt"));
                ui.label("Parent folder (optional)");
                ui.add(egui::TextEdit::singleline(&mut state.providers.crypt_root).hint_text("rpool"));
                ui.small("A fresh uniquely named child folder is used. This does not encrypt existing files in place.");
                let strengths = ["256 bits", "128 bits", "512 bits", "1024 bits"];
                egui::ComboBox::from_label("Generated password entropy")
                    .selected_text(strengths[state.providers.entropy_index.min(3)])
                    .show_ui(ui, |ui| { for (i, label) in strengths.iter().enumerate() { ui.selectable_value(&mut state.providers.entropy_index, i, *label); } });
                ui.small("This controls random password strength, not the cipher. File contents always use rclone crypt encryption.");
                let modes = ["Standard — encrypted filenames (recommended)", "Obfuscate — reversible names, not strong filename secrecy", "Off — original filenames visible"];
                egui::ComboBox::from_label("Filename protection")
                    .selected_text(modes[state.providers.filename_index.min(2)])
                    .show_ui(ui, |ui| { for (i, label) in modes.iter().enumerate() { ui.selectable_value(&mut state.providers.filename_index, i, *label); } });
                ui.checkbox(&mut state.providers.disable_directory_encryption, "Leave directory names visible");
                ui.small("Filename Off also leaves directory names visible regardless of the directory option.");
                ui.separator();
                ui.label("Generated keys are saved only in your local rclone configuration. No recovery vault is created automatically. Back up that configuration securely or export an encrypted RPool package before uploading data.");
                ui.checkbox(&mut state.providers.backup_acknowledged, "I understand that losing these keys loses access to my data");
                ui.small("Close other rclone config editors before creating. Encrypted config files require the official rclone config wizard instead.");
                if ui.add_enabled(state.providers.backup_acknowledged, egui::Button::new("Generate keys and create encrypted provider")).clicked() {
                    match start_encryption(state, task) {
                        Ok(()) => state.providers.setup_notice = Some("Creating encrypted provider. See Jobs for the result; refresh providers after completion.".into()),
                        Err(error) => state.providers.setup_notice = Some(error),
                    }
                }
            });
            if let Some(notice) = &state.providers.setup_notice { ui.label(notice); }
        });
    });
    state.providers.setup_open = open;
}

fn start_encryption(state: &GuiState, task: &mut TaskRunner) -> Result<(), String> {
    let form = &state.providers;
    if form.crypt_name.trim().is_empty() || form.backing_provider.trim().is_empty() {
        return Err("Choose a backing provider and enter a new encrypted provider name.".into());
    }
    let bits = [256, 128, 512, 1024][form.entropy_index.min(3)];
    let mode = ["standard", "obfuscate", "off"][form.filename_index.min(2)];
    let args: Vec<OsString> = vec![
        "provider".into(),
        "encrypt".into(),
        format!("--name={}", form.crypt_name.trim()).into(),
        format!("--provider={}", form.backing_provider.trim()).into(),
        format!("--root={}", form.crypt_root.trim()).into(),
        format!("--entropy-bits={bits}").into(),
        format!("--filename-encryption={mode}").into(),
        format!(
            "--directory-encryption={}",
            !form.disable_directory_encryption
        )
        .into(),
    ];
    task.start_rpool("Create encrypted provider", &state.settings.rclone, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_manual_setup_inherits_defaults_without_touching_identity() {
        let mut form = ProviderForm {
            crypt_name: "existing-draft".into(),
            backup_acknowledged: true,
            ..Default::default()
        };
        let defaults = crate::config_sync::provision::EncryptionDefaults::default();
        form.apply_defaults(&defaults);
        assert_eq!([256, 128, 512, 1024][form.entropy_index], 1024);
        assert_eq!(form.crypt_root, "rpool");
        assert_eq!(form.crypt_name, "existing-draft");
        assert!(!form.backup_acknowledged);
        let changed = crate::config_sync::provision::EncryptionDefaults {
            entropy_bits: 128,
            filename_encryption: "off".into(),
            directory_encryption: false,
            root: "custom folder".into(),
        };
        form.apply_defaults(&changed);
        assert_eq!([256, 128, 512, 1024][form.entropy_index], 128);
        assert_eq!(form.filename_index, 2);
        assert!(form.disable_directory_encryption);
        assert_eq!(form.crypt_root, "custom folder");
    }

    #[test]
    fn readiness_does_not_claim_success_on_unknown_or_missing_provider() {
        assert_eq!(
            encryption_status(false, false, false, false),
            "Encryption status unknown"
        );
        assert_eq!(
            encryption_status(true, false, false, true),
            "Encryption configured"
        );
        assert_eq!(
            encryption_status(true, true, false, true),
            "Setup incomplete — retry"
        );
        assert_eq!(
            encryption_status(true, true, true, true),
            "Setting up encryption…"
        );
    }
}
