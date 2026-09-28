use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::widgets::{local_file_field, output_file_field};
use eframe::egui;
use std::ffi::OsString;

#[derive(Debug, Default)]
pub(crate) struct ProviderForm {
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

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    ui.heading("Providers");
    ui.label("Check provider health and migrate shards away from a provider without changing archive data.");
    ui.separator();

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
    ui.horizontal(|ui| {
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
