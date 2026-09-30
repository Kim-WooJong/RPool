//! Account-change steps that act on a drive workspace: apply a changed pool
//! in place, or recover into a new pool after losing an account. Shown in
//! Storage › Account changes next to Drain and Reprocess.
use super::drive_section::directory_field;
use super::form::MountForm;
use super::status_bar::run;
use crate::gui::i18n::tr;
use crate::gui::state::{GuiState, Page};
use crate::gui::theme;
use eframe::egui;

fn reprocess_plan(ui: &mut egui::Ui, form: &mut MountForm) {
    ui.horizontal_wrapped(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut form.recovery_reprocess_plan)
                .desired_width(ui.available_width().min(320.0))
                .hint_text(tr("optional completed plan.json")),
        );
        if ui.button(tr("Choose plan…")).clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Reprocess plan", &["json"])
                .pick_file()
            {
                form.recovery_reprocess_plan = path.display().to_string();
            }
        }
    });
}

/// Which drive the steps below act on, with a way to change it.
fn target(ui: &mut egui::Ui, state: &mut GuiState) {
    let form = &state.mount;
    let pool = if form.pool.is_empty() {
        tr("no pool selected")
    } else {
        &form.pool
    };
    let workspace = if form.workspace.trim().is_empty() {
        tr("no workspace")
    } else {
        form.workspace.trim()
    };
    let mut open = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(tr("Drive:"));
        ui.strong(format!("{pool} · {workspace}"));
        open = ui.small_button(tr("Change in Drive")).clicked();
    });
    if open {
        state.page = Page::Drive;
    }
}

pub(crate) fn apply_card(ui: &mut egui::Ui, state: &mut GuiState) {
    theme::card_section(ui, tr("Apply a changed pool to a drive"), Some(tr("After adding or removing accounts in Pools: unmount, then apply. The pool name and workspace stay; current files, conflicts and sealed writes are verified in a new metadata generation first.")), |_| {}, |ui| {
        target(ui, state);
        let (form, settings) = (&mut state.mount, &mut state.settings);
        if !(form.virtual_drive && form.pool_sync) {
            theme::hint(ui, tr("Needs an online drive with Automatic pool sync."));
            return;
        }
        ui.add_enabled_ui(!form.runner.is_running(), |ui| {
            ui.label(tr("Completed Reprocess plan (optional):"));
            reprocess_plan(ui, form);
            if theme::primary_button(ui, true, tr("Apply pool changes")).clicked() {
                run(form, settings, |form, rclone| form.start_action(rclone, 6));
            }
            theme::hint(ui, tr("Old history stays in a sibling backup. Other PCs keep the old generation until they apply too. This can take time and space."));
        });
    });
}

pub(crate) fn recover_card(ui: &mut egui::Ui, state: &mut GuiState) {
    theme::card_section(ui, tr("Recover after losing an account"), Some(tr("Copies the verified contents of an old workspace into a NEW pool and workspace. The original is kept.")), |_| {}, |ui| {
        ui.label(tr("1. In Pools, save the remaining accounts as a NEW pool. In Drive, select it with a NEW empty workspace, Automatic pool sync, and history deletion off."));
        target(ui, state);
        let (form, settings) = (&mut state.mount, &mut state.settings);
        ui.add_enabled_ui(!form.runner.is_running(), |ui| {
            ui.label(tr("2. The original workspace (unmount it first):"));
            directory_field(ui, &mut form.recovery_source, tr("original workspace"));
            ui.label(tr("Unavailable remote aliases to skip, one per line (no colon or path):"));
            ui.add(egui::TextEdit::multiline(&mut form.recovery_skip_remotes).desired_rows(2).desired_width(ui.available_width().min(320.0)));
            ui.label(tr("Completed Reprocess plan (optional, reuses verified replacement archives):"));
            reprocess_plan(ui, form);
            if theme::primary_button(ui, true, tr("Recover files into the selected pool")).clicked() {
                let result = form
                    .save_mount_settings(settings)
                    .and_then(|()| form.start_account_recovery(&settings.rclone));
                if let Err(error) = result {
                    form.notice = Some(error);
                }
            }
            theme::hint(ui, tr("3. Check the recovery report, then mount the new pool normally. Only locally known files, sealed writes and conflicts can be recovered; missing data is reported per file."));
            if let Some(notice) = &form.notice {
                ui.label(notice);
            }
        });
    });
}
