//! Library › Drive files: a read-only explorer of the selected pool's
//! drive, listed from the pool's cloud metadata on a background thread.

use super::drive_state::{DriveForm, DriveLoad, DriveTree};
use super::explorer::{self, files_label, folders_label, ExplorerState, ViewMode};
use crate::gui::i18n::{tr, trf};
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use crate::presentation::format_bytes;
use eframe::egui;

/// Toolbar items after the pool picker: Refresh, search and status.
pub(crate) fn toolbar(ui: &mut egui::Ui, form: &mut DriveForm, rclone: &str) {
    if form.poll() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
    if form.needs_load() {
        form.start(rclone);
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
    let loading = form.is_loading();
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
            status_badge(ui, &tree.mode, StatusTone::Success);
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

/// The explorer (or the loading / error / empty state) below the toolbar.
pub(crate) fn body(ui: &mut egui::Ui, form: &mut DriveForm, rclone: &str) {
    if form.pool.is_empty() {
        theme::hint(
            ui,
            tr("Pick a pool to browse the folders and files on its drive."),
        );
        return;
    }
    let mut retry = false;
    let loading = form.is_loading();
    let (pool, load, explorer, query) = form.parts();
    match load {
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
        Some(DriveLoad::Ready(tree)) => drive_view(ui, pool, tree, explorer, query),
    }
    if retry {
        form.start(rclone);
    }
}

fn drive_view(
    ui: &mut egui::Ui,
    pool: &str,
    tree: &DriveTree,
    explorer: &mut ExplorerState,
    query: &mut String,
) {
    if tree.mode == "none" {
        theme::hint(
            ui,
            tr("This pool has no drive yet (mount it once from Drive)."),
        );
        return;
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
    explorer::show(ui, pool, tree, explorer, query);
}
