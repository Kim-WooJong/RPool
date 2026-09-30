//! Library › Drive files: a read-only folder tree of the selected pool's
//! drive, listed from the pool's cloud metadata on a background thread.

use super::drive_state::{DriveForm, DriveLoad, DriveTree};
use crate::gui::i18n::{tr, trf};
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use crate::presentation::format_bytes;
use eframe::egui;

const INDENT: f32 = 18.0;

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
    ui.add(
        egui::TextEdit::singleline(&mut form.query)
            .hint_text(tr("Search names and paths…"))
            .desired_width(220.0),
    );
    if !form.query.is_empty() && ui.button(tr("Clear")).clicked() {
        form.query.clear();
    }
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
            ui.label(
                egui::RichText::new(format!(
                    "{} · {} · {}",
                    folders_label(tree.dirs),
                    files_label(tree.files),
                    format_bytes(tree.bytes)
                ))
                .weak(),
            );
        }
        Some(DriveLoad::Failed(_)) => status_badge(ui, tr("Error"), StatusTone::Error),
        None => {}
    }
}

/// The tree (or the flat search results) below the toolbar.
pub(crate) fn body(ui: &mut egui::Ui, form: &mut DriveForm, rclone: &str) {
    if form.pool.is_empty() {
        theme::hint(
            ui,
            tr("Pick a pool to browse the folders and files on its drive."),
        );
        return;
    }
    let mut retry = false;
    let mut collapse = false;
    let mut toggle = None;
    match form.current() {
        None => theme::hint(ui, tr("Reading the pool's drive metadata…")),
        Some(DriveLoad::Failed(error)) => {
            let error = trf(
                "The drive could not be listed: {error}",
                &[("error", error)],
            );
            ui.colored_label(theme::error_colors(ui.visuals().dark_mode).1, error);
            retry = ui
                .add_enabled(!form.is_loading(), egui::Button::new(tr("Retry")))
                .clicked();
        }
        Some(DriveLoad::Ready(tree)) => {
            (collapse, toggle) = tree_view(ui, form, tree);
        }
    }
    if retry {
        form.start(rclone);
    }
    if collapse {
        form.expanded.clear();
    }
    if let Some(path) = toggle {
        form.toggle(&path);
    }
}

/// Returns (collapse all, folder to toggle).
fn tree_view(ui: &mut egui::Ui, form: &DriveForm, tree: &DriveTree) -> (bool, Option<String>) {
    if tree.mode == "none" {
        theme::hint(
            ui,
            tr("This pool has no drive yet (mount it once from Drive)."),
        );
        return (false, None);
    }
    if !tree.notes.is_empty() {
        egui::CollapsingHeader::new(trf("Notes ({n})", &[("n", &tree.notes.len())]))
            .id_salt(("library-drive-notes", &form.pool))
            .show(ui, |ui| {
                for note in &tree.notes {
                    ui.label(egui::RichText::new(note).weak());
                }
            });
    }
    if tree.nodes.is_empty() {
        theme::hint(ui, tr("The drive is empty."));
        return (false, None);
    }

    let query = form.query.trim();
    let collapse = query.is_empty()
        && !form.expanded.is_empty()
        && ui.small_button(tr("Collapse all")).clicked();
    let height = (ui.available_height() - 28.0).max(200.0);
    let row_height = ui.spacing().interact_size.y;
    let mut toggle = None;
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let scroll = egui::ScrollArea::both()
            .id_salt(("library-drive", &form.pool))
            .max_height(height)
            .auto_shrink([false, false]);
        if query.is_empty() {
            let rows = tree.visible(&form.expanded);
            scroll.show_rows(ui, row_height, rows.len(), |ui, range| {
                for &(id, depth) in &rows[range] {
                    if tree_row(ui, tree, id, depth, &form.expanded) {
                        toggle = Some(tree.nodes[id].path.clone());
                    }
                }
            });
        } else {
            let found = tree.search(query);
            if found.is_empty() {
                ui.label(egui::RichText::new(tr("No names match the search.")).weak());
                return;
            }
            scroll.show_rows(ui, row_height, found.len(), |ui, range| {
                for &id in &found[range] {
                    search_row(ui, tree, id);
                }
            });
        }
    });
    (collapse, toggle)
}

/// Draws one tree row; returns true when its folder was clicked.
fn tree_row(
    ui: &mut egui::Ui,
    tree: &DriveTree,
    id: usize,
    depth: usize,
    expanded: &std::collections::BTreeSet<String>,
) -> bool {
    let node = &tree.nodes[id];
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * INDENT);
        if node.is_dir {
            let open = expanded.contains(&node.path);
            let arrow = if open { "⏷" } else { "⏵" };
            clicked = ui
                .selectable_label(open, format!("{arrow} {}", node.name))
                .on_hover_text(&node.path)
                .clicked();
            ui.label(
                egui::RichText::new(format!(
                    "{} · {}",
                    items_label(node.children.len()),
                    format_bytes(node.size)
                ))
                .weak(),
            );
        } else {
            ui.add_space(INDENT);
            ui.label(&node.name).on_hover_text(&node.path);
            ui.label(
                egui::RichText::new(format_bytes(node.size))
                    .monospace()
                    .weak(),
            );
        }
    });
    clicked
}

fn search_row(ui: &mut egui::Ui, tree: &DriveTree, id: usize) {
    let node = &tree.nodes[id];
    ui.horizontal(|ui| {
        if node.is_dir {
            ui.label(format!("{}/", node.path));
            ui.label(egui::RichText::new(files_label(node.files)).weak());
        } else {
            ui.label(&node.path);
            ui.label(
                egui::RichText::new(format_bytes(node.size))
                    .monospace()
                    .weak(),
            );
        }
    });
}

fn folders_label(n: usize) -> String {
    if n == 1 {
        trf("{n} folder", &[("n", &n)])
    } else {
        trf("{n} folders", &[("n", &n)])
    }
}

fn files_label(n: usize) -> String {
    if n == 1 {
        trf("{n} file", &[("n", &n)])
    } else {
        trf("{n} files", &[("n", &n)])
    }
}

fn items_label(n: usize) -> String {
    if n == 1 {
        trf("{n} item", &[("n", &n)])
    } else {
        trf("{n} items", &[("n", &n)])
    }
}
