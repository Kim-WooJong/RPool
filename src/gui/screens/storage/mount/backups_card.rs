//! Drive › Maintenance › "Workspace backups": the folders workspace switches
//! left next to the selected workspace (`mount::workspace_backups`), with
//! their size and exported local-only writes, and deletion of one at a time.
//! Listing and deletion run on a background thread (sizes can take a while).
use super::form::MountForm;
use crate::gui::i18n::{tr, trf};
use crate::gui::theme;
use crate::mount::workspace_backups::{list, remove, BackupKind, WorkspaceBackup};
use crate::presentation::format_bytes;
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};

/// State of the card, held in `MountForm::backups`.
#[derive(Default)]
pub(crate) struct BackupsView {
    /// Workspace the shown list belongs to.
    workspace: String,
    /// Last listing, or its error.
    found: Option<Result<Vec<WorkspaceBackup>, String>>,
    /// A listing or deletion running in the background.
    pending: Option<Receiver<Outcome>>,
    /// Backup whose deletion waits for the second click.
    confirm: Option<PathBuf>,
    /// Also delete exported local-only writes of `confirm`.
    include_recovered: bool,
    /// Result of the last deletion.
    notice: Option<String>,
}

/// What a background job reports.
enum Outcome {
    /// A listing.
    Listed(Result<Vec<WorkspaceBackup>, String>),
    /// A deletion: freed bytes, then the new listing.
    Removed(Result<u64, String>, Result<Vec<WorkspaceBackup>, String>),
}

impl BackupsView {
    /// Lists the backups of `workspace` in the background.
    fn refresh(&mut self, workspace: &str) {
        let path = PathBuf::from(workspace);
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(Outcome::Listed(list(&path).map_err(|e| format!("{e:#}"))));
        });
        self.workspace = workspace.to_owned();
        self.pending = Some(rx);
    }
    /// Deletes `backup` in the background, then lists again.
    fn delete(&mut self, workspace: &str, backup: PathBuf, include_recovered: bool) {
        let path = PathBuf::from(workspace);
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let removed = remove(&path, &backup, include_recovered).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Outcome::Removed(
                removed,
                list(&path).map_err(|e| format!("{e:#}")),
            ));
        });
        self.pending = Some(rx);
        self.confirm = None;
    }
    /// Takes a finished background result.
    fn poll(&mut self) {
        let Some(rx) = &self.pending else {
            return;
        };
        let Ok(outcome) = rx.try_recv() else {
            return;
        };
        self.pending = None;
        match outcome {
            Outcome::Listed(found) => self.found = Some(found),
            Outcome::Removed(removed, found) => {
                self.notice = Some(match removed {
                    Ok(bytes) => trf(
                        "Backup deleted; {size} freed.",
                        &[("size", &format_bytes(bytes))],
                    ),
                    Err(error) => error,
                });
                self.found = Some(found);
            }
        }
    }
}

/// The card; lists automatically when the workspace changes.
pub(super) fn show(ui: &mut egui::Ui, form: &mut MountForm) {
    let workspace = form.workspace.trim().to_owned();
    let view = &mut form.backups;
    view.poll();
    if view.pending.is_some() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(200));
    }
    if !workspace.is_empty() && view.workspace != workspace && view.pending.is_none() {
        view.found = None;
        view.notice = None;
        view.refresh(&workspace);
    }
    let busy = view.pending.is_some();
    let mut refresh = false;
    theme::card_section(
        ui,
        tr("Workspace backups"),
        Some(tr("Folders the workspace switches after pool changes left next to this workspace. The drive's data is in the cloud; a backup only keeps the old local state. Delete them once the drive works.")),
        |ui| {
            refresh = ui
                .add_enabled(!busy && !workspace.is_empty(), egui::Button::new(tr("Refresh")))
                .clicked();
        },
        |ui| {
            if view.pending.is_some() {
                ui.spinner();
                return;
            }
            if let Some(notice) = &view.notice {
                ui.label(notice);
            }
            let found = match &view.found {
                None => return,
                Some(Err(error)) => {
                    ui.label(error);
                    return;
                }
                Some(Ok(found)) => found.clone(),
            };
            if found.is_empty() {
                theme::hint(ui, tr("No backups next to this workspace."));
                return;
            }
            let total: u64 = found.iter().map(|b| b.bytes).sum();
            ui.label(trf("{count} backup(s), {size} in all", &[("count", &found.len()), ("size", &format_bytes(total))]));
            let mut delete = None;
            for backup in &found {
                ui.separator();
                let name = backup
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ui.horizontal_wrapped(|ui| {
                    ui.strong(name);
                    ui.label(match backup.kind {
                        BackupKind::Migration => tr("before a pool migration"),
                        BackupKind::Transition => tr("before Apply pool changes"),
                    });
                    ui.label(format_bytes(backup.bytes));
                });
                if backup.recovered_files > 0 {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        trf("{n} local-only file(s) in recovered-writes/ may exist nowhere else; copy what you need into the drive first.", &[("n", &backup.recovered_files)]),
                    );
                }
                ui.horizontal_wrapped(|ui| {
                    if view.confirm.as_ref() == Some(&backup.path) {
                        if backup.recovered_files > 0 {
                            ui.checkbox(&mut view.include_recovered, tr("Also delete the exported local-only files"));
                        }
                        let allowed = backup.recovered_files == 0 || view.include_recovered;
                        if ui.add_enabled(allowed, egui::Button::new(tr("Delete permanently"))).clicked() {
                            delete = Some(backup.path.clone());
                        }
                        if ui.button(tr("Cancel")).clicked() {
                            view.confirm = None;
                        }
                    } else if ui.button(tr("Delete…")).clicked() {
                        view.confirm = Some(backup.path.clone());
                        view.include_recovered = false;
                    }
                });
            }
            if let Some(path) = delete {
                let include = view.include_recovered;
                view.delete(&workspace, path, include);
            }
        },
    );
    if refresh {
        view.refresh(&workspace);
    }
}
