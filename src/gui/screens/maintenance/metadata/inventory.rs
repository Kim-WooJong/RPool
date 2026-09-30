use crate::gui::state::GuiState;
use crate::gui::task::TaskRunner;
use eframe::egui;
use std::ffi::OsString;

pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    ui.label(egui::RichText::new("Inventory cache").strong());
    ui.label(egui::RichText::new("Rebuild the local inventory index from manifest files. The index is not a source of truth.").weak());
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut state.manifest.inventory_directory)
                .hint_text("Directory containing manifests")
                .desired_width(360.0),
        );
        if ui.button("Browse…").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                state.manifest.inventory_directory = path.display().to_string();
            }
        }
        if ui
            .add_enabled(!task.is_running(), egui::Button::new("Rebuild inventory"))
            .clicked()
        {
            state.manifest.error = start(state, task).err();
            state.manifest.notice = None;
        }
    });
    ui.add_space(crate::gui::theme::SUBSECTION_GAP);
    ui.label(
        egui::RichText::new(
            "Add one archive to the index from its manifest (local file or remote object).",
        )
        .weak(),
    );
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut state.manifest.inventory_manifest)
                .hint_text("file.rpool.json or crypt:path/manifest.json")
                .desired_width(360.0),
        );
        if ui.button("Browse…").clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Manifest JSON", &["json"])
                .pick_file()
            {
                state.manifest.inventory_manifest = path.display().to_string();
            }
        }
        if ui
            .add_enabled(!task.is_running(), egui::Button::new("Add to inventory"))
            .clicked()
        {
            state.manifest.error = add_args(&state.manifest.inventory_manifest)
                .and_then(|args| task.start_rpool("Inventory add", &state.settings.rclone, args))
                .err();
            state.manifest.notice = None;
        }
    });
}

/// `rpool inventory add <manifest>`.
fn add_args(manifest: &str) -> Result<Vec<OsString>, String> {
    let manifest = manifest.trim();
    if manifest.is_empty() {
        return Err("Choose a manifest to add first.".to_string());
    }
    Ok(vec!["inventory".into(), "add".into(), manifest.into()])
}

fn start(state: &GuiState, task: &mut TaskRunner) -> Result<(), String> {
    let directory = state.manifest.inventory_directory.trim();
    if directory.is_empty() {
        return Err("Choose a directory containing manifests first.".to_string());
    }
    task.start_rpool(
        "Inventory rebuild",
        &state.settings.rclone,
        [
            OsString::from("inventory"),
            OsString::from("rebuild"),
            OsString::from(directory),
        ],
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn single_manifest_add_uses_the_inventory_add_command() {
        use clap::Parser;
        assert!(super::add_args("  ").is_err());
        let args = super::add_args(" crypt:pool/a b.json ").unwrap();
        let cli = crate::cli::Cli::try_parse_from(
            std::iter::once(std::ffi::OsString::from("rpool")).chain(args),
        )
        .unwrap();
        let Some(crate::cli::Commands::Inventory(inventory)) = cli.command else {
            panic!("inventory command expected")
        };
        assert!(format!("{inventory:?}").contains("crypt:pool/a b.json"));
    }
}
