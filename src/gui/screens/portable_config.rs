//! Settings › Portable configuration: move RPool settings between PCs and see
//! where the active settings files live. Package export/import carries crypt
//! secrets inside an age-encrypted vault.
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::gui::widgets::section_header;
use eframe::egui;
use std::ffi::OsString;

/// Inputs of the Portable configuration tab, held in `GuiState::portable`;
/// turned into `rpool export` / `rpool import` arguments for the task runner.
#[derive(Debug)]
pub(crate) struct PortableForm {
    /// Package folder to export to or import from (`ROOT` argument).
    package_root: String,
    /// age public key (`age1…`) that encrypts crypt secrets on export; optional on import.
    age_recipient: String,
    /// Private age identity file used to decrypt the package on import.
    age_identity: String,
    /// `age` executable name or path (`--age`).
    age: String,
    /// `age-keygen` executable name or path (`--age-keygen`, import only).
    age_keygen: String,
    /// Optional rclone.conf path; empty = rclone's active config.
    rclone_config: String,
    /// Confirmation checkbox that enables the destructive Import button; reset after each import.
    import_confirmed: bool,
    /// Argument-building error from the last button press, shown under the buttons.
    error: Option<String>,
}
impl Default for PortableForm {
    fn default() -> Self {
        Self {
            package_root: String::new(),
            age_recipient: String::new(),
            age_identity: String::new(),
            age: "age".into(),
            age_keygen: "age-keygen".into(),
            rclone_config: String::new(),
            import_confirmed: false,
            error: None,
        }
    }
}

/// `missing` is the (translated) message shown when `value` is empty.
fn required<'a>(value: &'a str, missing: &str) -> Result<&'a str, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(missing.to_string());
    }
    Ok(value)
}

impl PortableForm {
    /// Appends the options shared by export and import (`--rclone-config`, `--age`) when set.
    fn common(&self, args: &mut Vec<OsString>) {
        if !self.rclone_config.trim().is_empty() {
            args.extend(["--rclone-config".into(), self.rclone_config.trim().into()]);
        }
        if !self.age.trim().is_empty() {
            args.extend(["--age".into(), self.age.trim().into()]);
        }
    }
    /// `rpool export ROOT [--age-recipient R]`: crypt secrets need a recipient.
    pub(crate) fn export_args(&self) -> Result<Vec<OsString>, String> {
        let root = required(&self.package_root, tr("Enter a package folder first."))?;
        let mut args: Vec<OsString> = vec!["export".into(), root.into()];
        if !self.age_recipient.trim().is_empty() {
            args.extend(["--age-recipient".into(), self.age_recipient.trim().into()]);
        }
        self.common(&mut args);
        Ok(args)
    }
    /// `rpool import ROOT --age-identity FILE [--dry-run]`.
    pub(crate) fn import_args(&self, dry_run: bool) -> Result<Vec<OsString>, String> {
        let root = required(&self.package_root, tr("Enter a package folder first."))?;
        let mut args: Vec<OsString> = vec!["import".into(), root.into()];
        if !self.age_identity.trim().is_empty() {
            args.extend(["--age-identity".into(), self.age_identity.trim().into()]);
        }
        if !self.age_recipient.trim().is_empty() {
            args.extend(["--age-recipient".into(), self.age_recipient.trim().into()]);
        }
        if !self.age_keygen.trim().is_empty() {
            args.extend(["--age-keygen".into(), self.age_keygen.trim().into()]);
        }
        self.common(&mut args);
        if dry_run {
            args.push("--dry-run".into());
        }
        Ok(args)
    }
}

/// One labelled text field row of the package grid; `pick` adds a Choose…
/// button: `Some(true)` picks a folder, `Some(false)` a file, `None` none.
fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str, pick: Option<bool>) {
    ui.label(label);
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(340.0)
                .hint_text(hint),
        );
        match pick {
            Some(true) if ui.button(tr("Choose…")).clicked() => {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    *value = path.display().to_string();
                }
            }
            Some(false) if ui.button(tr("Choose…")).clicked() => {
                if let Some(path) = rfd::FileDialog::new().pick_file() {
                    *value = path.display().to_string();
                }
            }
            _ => {}
        }
    });
    ui.end_row();
}

/// Starts `rpool <args>` as a background task named `name`; returns the
/// argument or start error to show, `None` on success.
fn start(
    task: &mut TaskRunner,
    rclone: &str,
    name: &str,
    args: Result<Vec<OsString>, String>,
) -> Option<String> {
    args.and_then(|args| task.start_rpool(name, rclone, args))
        .err()
}

/// Grid of the active settings file paths (same as `rpool config paths`),
/// each with a Copy button; contents are never read.
fn paths(ui: &mut egui::Ui) {
    let rows: [(&str, anyhow::Result<std::path::PathBuf>); 5] = [
        (tr("Settings folder"), crate::config::app_config_dir()),
        (tr("GUI settings"), crate::config::gui_settings_path()),
        (tr("Pools"), crate::config::pools_path()),
        (
            tr("Remote default paths"),
            crate::config::remote_roots_path(),
        ),
        (
            tr("Account identities"),
            crate::config::app_config_dir().map(|d| d.join("provider_domains.json")),
        ),
    ];
    egui::Grid::new("portable-paths")
        .num_columns(3)
        .striped(true)
        .show(ui, |ui| {
            for (label, path) in rows {
                ui.label(label);
                match path {
                    Ok(path) => {
                        let text = path.display().to_string();
                        ui.monospace(&text);
                        if ui.small_button(tr("Copy")).clicked() {
                            ui.ctx().copy_text(text);
                        }
                    }
                    Err(error) => {
                        ui.label(trf("unavailable: {error}", &[("error", &error)]));
                        ui.label("");
                    }
                }
                ui.end_row();
            }
        });
    ui.small(tr(
        "Same as `rpool config paths`. File contents are never shown here.",
    ));
}

/// Renders the Portable configuration tab; called by `screens::settings::show`.
/// Buttons stay disabled while another task runs.
pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    let (form, rclone) = (&mut state.portable, state.settings.rclone.clone());
    let busy = task.is_running();
    section_header(ui, tr("Portable configuration"), Some(tr("Move pools, per-remote paths and GUI defaults between PCs. Machine-local paths such as the rclone executable are kept.")));

    ui.strong(tr("Package (with crypt secrets, age-encrypted)"));
    egui::Grid::new("portable-package")
        .num_columns(2)
        .spacing([14.0, 8.0])
        .show(ui, |ui| {
            field(
                ui,
                tr("Package folder"),
                &mut form.package_root,
                tr("e.g. D:\\sync\\rpool"),
                Some(true),
            );
            field(
                ui,
                tr("age recipient"),
                &mut form.age_recipient,
                tr("age1… (export; optional for import)"),
                None,
            );
            field(
                ui,
                tr("age identity file"),
                &mut form.age_identity,
                tr("private key file outside the package (import)"),
                Some(false),
            );
            field(
                ui,
                "rclone.conf",
                &mut form.rclone_config,
                tr("optional; rclone's active config if empty"),
                Some(false),
            );
            field(ui, "age / age-keygen", &mut form.age, "age", None);
            ui.label("");
            ui.add(egui::TextEdit::singleline(&mut form.age_keygen).desired_width(160.0));
            ui.end_row();
        });
    ui.add_enabled_ui(!busy, |ui| {
        ui.horizontal_wrapped(|ui| {
            if ui.button(tr("Export package")).on_hover_text(tr("Writes config/portable-config.json and, when crypt remotes exist, secrets/rclone.age.")).clicked() {
                form.error = start(task, &rclone, "Export portable package", form.export_args());
            }
            if ui.button(tr("Validate import (dry run)")).on_hover_text(tr("Checks the package and this PC's configuration without changing anything.")).clicked() {
                form.error = start(task, &rclone, "Validate portable package", form.import_args(true));
            }
            ui.checkbox(&mut form.import_confirmed, tr("Replace local pools and settings (a rollback snapshot is kept)"));
            if ui.add_enabled(form.import_confirmed, egui::Button::new(tr("Import package"))).clicked() {
                form.error = start(task, &rclone, "Import portable package", form.import_args(false));
                form.import_confirmed = false;
            }
        });
    });
    ui.add_space(theme::SECTION_GAP);

    if let Some(error) = &form.error {
        let (_, color) = theme::error_colors(ui.visuals().dark_mode);
        ui.label(egui::RichText::new(error).color(color));
    }
    ui.small(tr("Results appear in the task console below. Restart the GUI after an import to reload pools and settings."));
    ui.add_space(theme::SECTION_GAP);

    ui.strong(tr("Active settings files"));
    paths(ui);
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(args: Vec<OsString>) -> crate::cli::Cli {
        crate::cli::Cli::try_parse_from(std::iter::once(OsString::from("rpool")).chain(args))
            .unwrap()
    }

    #[test]
    fn package_actions_build_valid_cli_commands() {
        let mut form = PortableForm::default();
        assert!(form.export_args().is_err());
        form.package_root = "/sync/rpool pkg".into();
        form.age_recipient = "age1example".into();
        form.age_identity = "/keys/id.txt".into();
        let Some(crate::cli::Commands::Export(export)) = parse(form.export_args().unwrap()).command
        else {
            panic!()
        };
        assert_eq!(export.age_recipient.as_deref(), Some("age1example"));
        assert_eq!(
            export.artifact_root,
            std::path::PathBuf::from("/sync/rpool pkg")
        );
        let Some(crate::cli::Commands::Import(dry)) =
            parse(form.import_args(true).unwrap()).command
        else {
            panic!()
        };
        assert!(dry.dry_run);
        assert_eq!(dry.age_identity, Some("/keys/id.txt".into()));
        let Some(crate::cli::Commands::Import(real)) =
            parse(form.import_args(false).unwrap()).command
        else {
            panic!()
        };
        assert!(!real.dry_run);
    }

    #[test]
    fn portable_screen_renders_headless() {
        let mut state = crate::gui::state::GuiState::new(
            crate::gui::settings::GuiSettings::default(),
            Default::default(),
            Default::default(),
        );
        let mut task = TaskRunner::default();
        let ctx = egui::Context::default();
        for _ in 0..2 {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                show(ui, &mut state, &mut task)
            });
            output.textures_delta.clear();
        }
        assert!(!task.is_running(), "rendering starts nothing");
    }
}
