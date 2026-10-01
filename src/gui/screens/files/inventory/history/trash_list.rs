//! The trash list: a header with a select-all box and one row per deleted
//! item (check box, name with markers, original folder, size, deleted when
//! and by, expiry). Right-click offers the row's actions.

use super::selection::Selection;
use super::trash_columns::{TrashColumns, CHECK};
use super::trash_rows::TrashRow;
use crate::gui::i18n::{tr, trf};
use crate::gui::screens::files::inventory::explorer::paint::cell;
use crate::gui::theme;
use crate::presentation::format_bytes;
use eframe::egui;

pub(crate) const ROW: f32 = 28.0;

/// What a row's context menu asked for (it selected the row first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowAction {
    Restore,
    RestoreTo,
    Purge,
}

pub(crate) fn show(
    ui: &mut egui::Ui,
    rows: &[TrashRow],
    selection: &mut Selection,
    height: f32,
) -> Option<RowAction> {
    let order: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    let mut action = None;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let cols = TrashColumns::for_width(ui.available_width());
        header(ui, &cols, selection, &order);
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("library-trash-list")
            .max_height(height)
            .auto_shrink([false, false])
            .show_rows(ui, ROW, rows.len(), |ui, range| {
                for row in &rows[range] {
                    if let Some(clicked) = draw_row(ui, row, &cols, selection, &order) {
                        action = Some(clicked);
                    }
                }
            });
    });
    action
}

fn header(ui: &mut egui::Ui, cols: &TrashColumns, selection: &mut Selection, order: &[&str]) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW), egui::Sense::hover());
    let mut all = selection.all(order);
    let check = egui::Rect::from_min_size(rect.min, egui::vec2(CHECK, ROW));
    if ui
        .put(check, egui::Checkbox::without_text(&mut all))
        .on_hover_text(tr("Select all shown items"))
        .changed()
    {
        selection.set_all(order, all);
    }
    let mut left = rect.left() + CHECK;
    let mut column = |width: f32, label: &str, right| {
        cell(
            ui,
            rect,
            left,
            width,
            egui::RichText::new(label).strong(),
            right,
        );
        left += width;
    };
    column(cols.name, tr("Name"), false);
    if let Some(width) = cols.folder {
        column(width, tr("Original folder"), false);
    }
    column(cols.size, tr("Size"), true);
    if let Some(width) = cols.deleted {
        column(width, tr("Deleted"), false);
    }
    if let Some(width) = cols.expires {
        column(width, tr("Leaves the trash"), false);
    }
}

/// Marker after the name: why the row needs attention.
fn marker(row: &TrashRow) -> Option<&'static str> {
    if row.from_rollback {
        Some(tr("from rollback"))
    } else if row.path_taken {
        Some(tr("name in use"))
    } else {
        None
    }
}

fn draw_row(
    ui: &mut egui::Ui,
    row: &TrashRow,
    cols: &TrashColumns,
    selection: &mut Selection,
    order: &[&str],
) -> Option<RowAction> {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW), egui::Sense::click());
    let p = theme::pal(ui);
    let dark = ui.visuals().dark_mode;
    let selected = selection.contains(&row.id);
    if selected {
        ui.painter().rect_filled(rect, 4.0, p.accent_soft);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 4.0, p.surface_alt);
    }
    let mut on = selected;
    let check = egui::Rect::from_min_size(rect.min, egui::vec2(CHECK, ROW));
    if ui
        .put(check, egui::Checkbox::without_text(&mut on))
        .changed()
    {
        selection.toggle(&row.id);
    }

    let mut left = rect.left() + CHECK;
    let icon = if row.is_dir { "📁" } else { "📄" };
    let name = egui::RichText::new(format!("{icon} {}", row.name)).color(p.text);
    match marker(row) {
        Some(text) => {
            // The marker keeps its room; the name is cut before it.
            let tag = (cols.name * 0.45).min(118.0);
            cell(ui, rect, left, cols.name - tag, name, false);
            let color = if row.from_rollback {
                theme::info_colors(dark).1
            } else {
                theme::warning_colors(dark).1
            };
            let tag_text = egui::RichText::new(text).small().color(color);
            cell(ui, rect, left + cols.name - tag, tag, tag_text, true);
        }
        None => cell(ui, rect, left, cols.name, name, false),
    }
    left += cols.name;
    let folder = if row.folder == "/" {
        tr("(drive root)").to_string()
    } else {
        row.folder.clone()
    };
    if let Some(width) = cols.folder {
        cell(
            ui,
            rect,
            left,
            width,
            egui::RichText::new(&folder).color(p.muted),
            false,
        );
        left += width;
    }
    let size = egui::RichText::new(format_bytes(row.size))
        .monospace()
        .color(p.text);
    cell(ui, rect, left, cols.size, size, true);
    left += cols.size;
    if let Some(width) = cols.deleted {
        let text = match &row.deleted_by {
            Some(by) => format!("{} · {by}", row.deleted.0),
            None => row.deleted.0.clone(),
        };
        cell(
            ui,
            rect,
            left,
            width,
            egui::RichText::new(text).color(p.muted),
            false,
        );
        left += width;
    }
    if let Some(width) = cols.expires {
        let color = if row.expired || row.expires_soon {
            theme::warning_colors(dark).1
        } else {
            p.muted
        };
        cell(
            ui,
            rect,
            left,
            width,
            egui::RichText::new(&row.expiry).color(color),
            false,
        );
    }

    let response = response.on_hover_ui(|ui| tooltip(ui, row, &folder));
    if response.clicked() {
        let (toggle, range) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        selection.click(&row.id, order, toggle, range);
    }
    if response.secondary_clicked() && !selected {
        selection.click(&row.id, order, false, false);
    }
    let mut action = None;
    response.context_menu(|ui| {
        for (label, chosen) in [
            (tr("Restore"), RowAction::Restore),
            (tr("Restore to…"), RowAction::RestoreTo),
            (tr("Delete permanently…"), RowAction::Purge),
        ] {
            if ui.button(label).clicked() {
                action = Some(chosen);
                ui.close();
            }
        }
    });
    action
}

fn tooltip(ui: &mut egui::Ui, row: &TrashRow, folder: &str) {
    ui.strong(&row.name);
    ui.label(trf("Was in: {folder}", &[("folder", &folder)]));
    let by = row.deleted_by.as_deref().unwrap_or("?");
    if row.deleted.1.is_empty() {
        ui.label(trf("Deleted by {pc}", &[("pc", &by)]));
    } else {
        ui.label(trf(
            "Deleted {time} by {pc}",
            &[("time", &row.deleted.1), ("pc", &by)],
        ));
    }
    ui.label(&row.expiry);
    if row.from_rollback {
        ui.label(tr(
            "Moved here by a rollback: it was created after the rollback's time.",
        ));
    }
    if row.path_taken {
        ui.label(tr("Another file now has this name; restoring keeps both."));
    }
}
