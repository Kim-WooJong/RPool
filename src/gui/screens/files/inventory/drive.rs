//! Library › Drive files: a read-only explorer of the selected pool's
//! drive, listed from the pool's cloud metadata on a background thread.

use super::drive_state::{DriveForm, DriveLoad, DriveTree};
use super::explorer::{self, files_label, folders_label, ExplorerState, ViewMode};
use super::history::{self, HistoryForm};
use crate::gui::i18n::{tr, trf};
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use crate::presentation::format_bytes;
use eframe::egui;

/// Toolbar items after the pool picker: Files/Trash, Refresh, search and
/// status.
pub(crate) fn toolbar(ui: &mut egui::Ui, form: &mut DriveForm, rclone: &str) {
    if form.poll() | history::poll(&mut form.history, &form.pool, rclone) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
    if form.needs_load() || !form.is_loading() && std::mem::take(&mut form.history.refresh_drive) {
        form.start(rclone);
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
    let loading = form.is_loading();
    let trash = &mut form.history.trash_open;
    ui.selectable_value(trash, false, format!("📁 {}", tr("Files")))
        .on_hover_text(tr("The folders and files on the drive."));
    ui.selectable_value(trash, true, format!("🗑 {}", tr("Trash")))
        .on_hover_text(tr("Deleted files and folders, to restore them."));
    let trash = *trash;
    if trash {
        let reading = form
            .history
            .get(&form.pool)
            .is_some_and(|s| s.trash.is_loading());
        if ui
            .add_enabled(
                !form.pool.is_empty() && !reading,
                egui::Button::new(tr("Refresh")),
            )
            .on_hover_text(tr("Read the trash again."))
            .clicked()
        {
            form.history.pool(&form.pool).trash.stale = true;
        }
        ui.add(
            egui::TextEdit::singleline(&mut form.query)
                .hint_text(tr("Search the trash…"))
                .desired_width(200.0),
        );
        if !form.query.is_empty() && ui.button(tr("Clear")).clicked() {
            form.query.clear();
        }
        return;
    }
    if ui
        .add_enabled(
            !form.pool.is_empty() && !loading,
            egui::Button::new(tr("Refresh")),
        )
        .on_hover_text(tr("List the drive again from the pool's cloud metadata."))
        .clicked()
    {
        form.start(rclone);
    }
    let hint = if form.explorer.search_all {
        tr("Search all folders…")
    } else {
        tr("Search this folder…")
    };
    ui.add(
        egui::TextEdit::singleline(&mut form.query)
            .hint_text(hint)
            .desired_width(200.0),
    );
    if !form.query.is_empty() && ui.button(tr("Clear")).clicked() {
        form.query.clear();
    }
    ui.checkbox(&mut form.explorer.search_all, tr("Search all folders"))
        .on_hover_text(tr(
            "Search the whole drive by name instead of the current folder.",
        ));
    let view = &mut form.explorer.view;
    ui.selectable_value(view, ViewMode::List, format!("☰ {}", tr("List")));
    ui.selectable_value(view, ViewMode::Icons, format!("▦ {}", tr("Icons")));
    if loading {
        ui.spinner();
        ui.label(tr("Loading…"));
        return;
    }
    match form.current() {
        Some(DriveLoad::Ready(tree)) if tree.mode == "none" => {
            status_badge(ui, tr("No drive"), StatusTone::Neutral);
        }
        Some(DriveLoad::Ready(tree)) => {
            status_badge(ui, tr("Drive"), StatusTone::Success);
            // Kept on one line: it moves to the next row as a whole.
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!(
                        "{} · {} · {}",
                        folders_label(tree.dirs),
                        files_label(tree.files),
                        format_bytes(tree.bytes)
                    ))
                    .weak(),
                )
                .extend(),
            );
        }
        Some(DriveLoad::Failed(_)) => status_badge(ui, tr("Error"), StatusTone::Error),
        None => {}
    }
}

/// The explorer or the trash (or the loading / error / empty state) below
/// the toolbar, and the history dialogs.
pub(crate) fn body(ui: &mut egui::Ui, form: &mut DriveForm, task: &mut TaskRunner, rclone: &str) {
    if form.pool.is_empty() {
        theme::hint(
            ui,
            tr("Pick a pool to browse the folders and files on its drive."),
        );
        return;
    }
    let mut retry = false;
    let loading = form.is_loading();
    let parts = form.parts();
    let tree = match parts.load {
        Some(DriveLoad::Ready(tree)) => Some(tree),
        _ => None,
    };
    if parts.history.trash_open {
        history::trash(ui, parts.history, parts.pool, parts.query, task, rclone);
    } else {
        match parts.load {
            None => theme::hint(ui, tr("Reading the pool's drive metadata…")),
            Some(DriveLoad::Failed(error)) => {
                let error = trf(
                    "The drive could not be listed: {error}",
                    &[("error", error)],
                );
                ui.colored_label(theme::error_colors(ui.visuals().dark_mode).1, error);
                retry = ui
                    .add_enabled(!loading, egui::Button::new(tr("Retry")))
                    .clicked();
            }
            Some(DriveLoad::Ready(tree)) => drive_view(
                ui,
                parts.pool,
                tree,
                parts.explorer,
                parts.query,
                parts.history,
                task,
                rclone,
            ),
        }
    }
    history::dialogs(ui.ctx(), parts.history, parts.pool, tree, task, rclone);
    if retry {
        form.start(rclone);
    }
}

/// Width of the versions panel next to the explorer.
const VERSIONS_WIDTH: f32 = 340.0;
/// Below this width the versions panel replaces the explorer.
const VERSIONS_BESIDE_MIN: f32 = 860.0;

#[allow(clippy::too_many_arguments)]
fn drive_view(
    ui: &mut egui::Ui,
    pool: &str,
    tree: &DriveTree,
    explorer: &mut ExplorerState,
    query: &mut String,
    history: &mut HistoryForm,
    task: &mut TaskRunner,
    rclone: &str,
) {
    if tree.mode == "none" {
        theme::hint(
            ui,
            tr("This pool has no drive yet (mount it once from Drive)."),
        );
        return;
    }
    if let Some((success, text)) = history
        .get(pool)
        .and_then(|state| state.notice.clone())
        .filter(|_| history.rollback.is_none())
    {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(&text).color(if success {
                theme::success_colors(ui.visuals().dark_mode).1
            } else {
                theme::error_colors(ui.visuals().dark_mode).1
            }));
            if ui.small_button("×").on_hover_text(tr("Close")).clicked() {
                history.pool(pool).notice = None;
            }
        });
    }
    if !tree.notes.is_empty() {
        egui::CollapsingHeader::new(trf("Notes ({n})", &[("n", &tree.notes.len())]))
            .id_salt(("library-drive-notes", pool))
            .show(ui, |ui| {
                for note in &tree.notes {
                    ui.label(egui::RichText::new(note).weak());
                }
            });
    }
    if tree.nodes.is_empty() {
        theme::hint(ui, tr("The drive is empty."));
        return;
    }
    let width = ui.available_width();
    let request = if history.versions.is_none() {
        explorer::show(ui, pool, tree, explorer, query)
    } else if width < VERSIONS_BESIDE_MIN {
        history::versions(ui, history, task, rclone, true);
        None
    } else {
        let height = ui.available_height();
        let gap = theme::SUBSECTION_GAP;
        let side = VERSIONS_WIDTH.min(width * 0.4);
        let mut request = None;
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(width - side - gap, height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(width - side - gap);
                    request = explorer::show(ui, pool, tree, explorer, query);
                },
            );
            ui.allocate_ui_with_layout(
                egui::vec2(side, height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(side);
                    history::versions(ui, history, task, rclone, false);
                },
            );
        });
        request
    };
    if let Some(request) = request {
        history::open(history, pool, request);
    }
}
