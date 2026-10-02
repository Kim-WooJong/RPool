//! The Files page: Library (drive explorer and uploaded archives), Upload and
//! Restore tabs. Verify and Status forms live here too but are shown from
//! Maintenance › Archive. Entry point `show`, called by the app for `Page::Files`.

/// Files › Library: drive explorer, trash/versions/rollback and archive list.
#[path = "inventory/mod.rs"]
pub(crate) mod inventory;
/// Files › Restore: `rpool get` from a manifest.
pub(crate) mod restore;
/// Archive status form (`rpool status`), shown under Maintenance.
pub(crate) mod status;
/// Files › Upload: file queue, target pool, options and `rpool put`.
#[path = "upload/mod.rs"]
pub(crate) mod upload;
/// Archive verify form (`rpool verify`), shown under Maintenance.
pub(crate) mod verify;

pub(crate) use inventory::InventoryForm;
pub(crate) use restore::RestoreForm;
pub(crate) use status::StatusForm;
pub(crate) use upload::UploadForm;
pub(crate) use verify::VerifyForm;

use crate::gui::i18n::tr;
use crate::gui::state::{FilesSection, GuiState};
use crate::gui::task::TaskRunner;
use eframe::egui;

/// Draws the Files tabs and the selected section (`state.files_section`).
pub(crate) fn show(ui: &mut egui::Ui, state: &mut GuiState, task: &mut TaskRunner) {
    crate::gui::theme::tabs(
        ui,
        &mut state.files_section,
        &[
            (FilesSection::Inventory, tr("Library")),
            (FilesSection::Upload, tr("Upload")),
            (FilesSection::Restore, tr("Restore")),
        ],
    );
    match state.files_section {
        FilesSection::Inventory => inventory::show(ui, state, task),
        FilesSection::Upload => upload::show(ui, state, task),
        FilesSection::Restore => restore::show(ui, state, task),
    }
}

#[cfg(test)]
mod i18n_tests {
    use crate::gui::i18n::{tr_in, Language};

    #[test]
    fn files_and_health_strings_are_translated() {
        assert_eq!(tr_in(Language::Korean, "Library"), "라이브러리");
        assert_eq!(tr_in(Language::Korean, "Start upload"), "업로드 시작");
        assert_eq!(tr_in(Language::Japanese, "Restore"), "復元");
        assert_eq!(tr_in(Language::Chinese, "Scrub"), "巡检");
        assert_eq!(
            tr_in(Language::Korean, "Uploading {position} / {total}"),
            "업로드 중 {position} / {total}"
        );
        // Technical tokens stay inside the translated text.
        assert!(tr_in(Language::Korean, "RS {k}+{m} · {mib} MiB shards").starts_with("RS {k}+{m}"));
        assert_eq!(tr_in(Language::English, "Integrity"), "Integrity");
    }
}
