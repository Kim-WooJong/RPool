//! Destructive management of independent new uploads in a stopped workspace.
//! The backend rechecks exact IDs and holds `virtual.lock`; GUI snapshots never
//! authorize deletion by themselves.
use super::form::MountForm;
use crate::gui::i18n::{tr, trf};
use crate::gui::theme;
use crate::monitor::model::QueueOperation;
use crate::mount::virtual_drive::pending_cancel::{self, CancelReport, PendingList};
use crate::presentation::format_bytes;
use eframe::egui;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};

#[derive(Default)]
pub(super) struct PendingQueueView {
    workspace: String,
    found: Option<Result<PendingList, String>>,
    selected: BTreeSet<String>,
    pending: Option<Receiver<Outcome>>,
    confirm: bool,
    acknowledged: bool,
    notice: Option<String>,
}

enum Outcome {
    Listed(Result<PendingList, String>),
    Cancelled(Result<CancelReport, String>, Result<PendingList, String>),
}

impl PendingQueueView {
    fn refresh(&mut self, workspace: &str) {
        let path = PathBuf::from(workspace);
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(Outcome::Listed(
                pending_cancel::list(&path).map_err(|error| format!("{error:#}")),
            ));
        });
        self.workspace = workspace.to_owned();
        self.pending = Some(rx);
    }

    fn cancel(&mut self, workspace: &str, generation: u64) {
        let path = PathBuf::from(workspace);
        let selected: Vec<String> = self.selected.iter().cloned().collect();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let result = pending_cancel::cancel(&path, generation, &selected)
                .map_err(|error| format!("{error:#}"));
            let listed = pending_cancel::list(&path).map_err(|error| format!("{error:#}"));
            let _ = tx.send(Outcome::Cancelled(result, listed));
        });
        self.pending = Some(rx);
        self.confirm = false;
        self.acknowledged = false;
    }

    fn poll(&mut self) {
        let Some(rx) = &self.pending else { return };
        let Ok(outcome) = rx.try_recv() else { return };
        self.pending = None;
        match outcome {
            Outcome::Listed(found) => self.found = Some(found),
            Outcome::Cancelled(result, found) => {
                self.notice = Some(match result {
                    Ok(report) => trf(
                        "Cancelled {count} upload(s) and deleted their local copies ({bytes} logical bytes).",
                        &[("count", &report.count), ("bytes", &format_bytes(report.bytes))],
                    ),
                    Err(error) => trf("Cancellation failed or local cleanup is incomplete: {error}", &[("error", &error)]),
                });
                self.found = Some(found);
                self.selected.clear();
            }
        }
        if let Some(Ok(found)) = &self.found {
            self.selected.retain(|id| {
                found
                    .rows
                    .iter()
                    .any(|row| &row.id == id && row.blocked.is_none())
            });
        }
    }
}

fn block_text(reason: &str) -> &'static str {
    match reason {
        "deletion" => tr("Deletion entries cannot be cancelled here."),
        "pending deletion or move" => tr("A pending deletion or move makes cancellation unsafe."),
        "not an independent new file" => tr("Only independent new files can be cancelled."),
        _ => tr("Another pending change overlaps this file."),
    }
}

pub(super) fn show(ui: &mut egui::Ui, form: &mut MountForm) {
    let workspace = form.workspace.trim().to_owned();
    let running = form.session.is_running()
        || crate::monitor::active_mounts()
            .iter()
            .any(|entry| entry.workspace == workspace);
    let view = &mut form.pending_queue;
    view.poll();
    if view.pending.is_some() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(200));
    }
    if view.workspace != workspace && view.pending.is_none() {
        view.workspace = workspace.clone();
        view.found = None;
        view.selected.clear();
        view.confirm = false;
        view.notice = None;
    }
    if running {
        view.found = None;
        view.selected.clear();
        view.confirm = false;
    } else if !workspace.is_empty() && view.found.is_none() && view.pending.is_none() {
        view.refresh(&workspace);
    }
    let busy = view.pending.is_some();
    let mut refresh = false;
    let mut do_cancel = None;
    theme::card_section(
        ui,
        tr("Pending uploads"),
        Some(tr("Review pending changes after unmounting. Cancelling permanently removes selected unsent new files from this PC; it does not delete cloud files.")),
        |ui| {
            refresh = ui
                .add_enabled(!busy && !running && !workspace.is_empty(), egui::Button::new(tr("Refresh")))
                .clicked();
        },
        |ui| {
            if running {
                theme::hint(ui, tr("Unmount the drive before cancelling pending uploads. Wait until it has fully stopped, then refresh this list."));
                return;
            }
            if busy {
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
            if found.rows.is_empty() {
                theme::hint(ui, tr("No pending drive changes."));
                return;
            }
            let eligible = found.rows.iter().filter(|row| row.blocked.is_none()).count();
            ui.label(trf("{count} pending change(s); {eligible} new upload(s) can be cancelled.", &[("count", &found.rows.len()), ("eligible", &eligible)]));
            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                for row in &found.rows {
                    ui.horizontal_wrapped(|ui| {
                        let mut checked = view.selected.contains(&row.id);
                        let label = format!("{} · {}", row.path, format_bytes(row.size));
                        if ui.add_enabled(row.blocked.is_none(), egui::Checkbox::new(&mut checked, label)).changed() {
                            if checked { view.selected.insert(row.id.clone()); }
                            else { view.selected.remove(&row.id); }
                            view.confirm = false;
                        }
                        if row.operation == QueueOperation::Delete {
                            theme::hint(ui, tr("Delete"));
                        }
                        if let Some(reason) = row.blocked {
                            theme::hint(ui, block_text(reason));
                        }
                    });
                }
            });
            if !view.confirm {
                if ui.add_enabled(!view.selected.is_empty(), egui::Button::new(tr("Cancel selected uploads and delete local copies…"))).clicked() {
                    view.confirm = true;
                    view.acknowledged = false;
                }
            } else {
                let count = view.selected.len();
                let bytes = found.rows.iter().filter(|row| view.selected.contains(&row.id)).fold(0u64, |sum, row| sum.saturating_add(row.size));
                ui.colored_label(ui.visuals().warn_fg_color, trf("Permanently discard {count} selected new file(s), {bytes} of local data? They will disappear from this drive. Already sent cloud fragments may remain until cleanup.", &[("count", &count), ("bytes", &format_bytes(bytes))]));
                ui.checkbox(&mut view.acknowledged, tr("I understand these local files cannot be recovered."));
                ui.horizontal(|ui| {
                    if ui.add_enabled(view.acknowledged, egui::Button::new(tr("Delete selected permanently"))).clicked() {
                        do_cancel = Some(found.generation);
                    }
                    if ui.button(tr("Keep files")).clicked() {
                        view.confirm = false;
                        view.acknowledged = false;
                    }
                });
            }
        },
    );
    if refresh {
        view.refresh(&workspace);
    }
    if let Some(generation) = do_cancel {
        view.cancel(&workspace, generation);
    }
}
