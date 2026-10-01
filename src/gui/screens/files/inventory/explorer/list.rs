//! The List view: a header with sortable columns and one row per entry.

use super::action::Action;
use super::columns::Columns;
use super::context;
use super::paint::cell;
use super::scroll::reveal_offset;
use super::sort::SortKey;
use super::state::ExplorerState;
use super::summary::{icon, items_label, type_label};
use crate::gui::i18n::tr;
use crate::gui::screens::files::inventory::drive_state::DriveTree;
use crate::gui::theme;
use crate::presentation::format_bytes;
use eframe::egui;

pub(crate) const ROW: f32 = 26.0;

pub(crate) fn show(
    ui: &mut egui::Ui,
    tree: &DriveTree,
    rows: &[usize],
    state: &mut ExplorerState,
    global: bool,
    height: f32,
) -> Option<Action> {
    let mut action = None;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let cols = Columns::for_width(ui.available_width(), global);
        header(ui, &cols, state, &mut action);
        ui.separator();
        let selected = state.selected.as_deref();
        let mut area = egui::ScrollArea::vertical()
            .id_salt("library-explorer-list")
            .max_height(height)
            .auto_shrink([false, false]);
        if std::mem::take(&mut state.reveal) {
            if let Some(index) =
                selected.and_then(|path| rows.iter().position(|&id| tree.nodes[id].path == path))
            {
                area = area.vertical_scroll_offset(reveal_offset(index, ROW, state.scroll));
            }
        }
        let output = area.show_rows(ui, ROW, rows.len(), |ui, range| {
            for &id in &rows[range] {
                let is_selected = selected == Some(tree.nodes[id].path.as_str());
                if let Some(clicked) = row(ui, tree, id, &cols, is_selected) {
                    action = Some(clicked);
                }
            }
        });
        state.scroll.offset = output.state.offset.y;
        state.scroll.view = output.inner_rect.height();
    });
    action
}

fn header(ui: &mut egui::Ui, cols: &Columns, state: &ExplorerState, action: &mut Option<Action>) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW), egui::Sense::hover());
    let mut left = rect.left();
    let mut column = |ui: &mut egui::Ui, width: f32, label: &str, key: Option<SortKey>, right| {
        let cell_rect = egui::Rect::from_x_y_ranges(left..=left + width, rect.y_range());
        let text = match key {
            Some(key) => format!("{label}{}", state.sort.arrow(key)),
            None => label.to_string(),
        };
        let mut rich = egui::RichText::new(text).strong();
        if let Some(key) = key {
            let response = ui
                .interact(
                    cell_rect,
                    ui.id().with(("explorer-sort", label)),
                    egui::Sense::click(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if response.hovered() {
                rich = rich.color(theme::pal(ui).accent);
            }
            if response.clicked() {
                *action = Some(Action::Sort(key));
            }
        }
        cell(ui, rect, left, width, rich, right);
        left += width;
    };
    column(ui, cols.name, tr("Name"), Some(SortKey::Name), false);
    if let Some(width) = cols.folder {
        column(ui, width, tr("Folder"), None, false);
    }
    column(ui, cols.size, tr("Size"), Some(SortKey::Size), true);
    if let Some(width) = cols.kind {
        column(ui, width, tr("Type"), Some(SortKey::Type), false);
    }
}

/// One entry; returns what a click on it asked for.
fn row(
    ui: &mut egui::Ui,
    tree: &DriveTree,
    id: usize,
    cols: &Columns,
    selected: bool,
) -> Option<Action> {
    let node = &tree.nodes[id];
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW), egui::Sense::click());
    let p = theme::pal(ui);
    if selected {
        ui.painter().rect_filled(rect, 4.0, p.accent_soft);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 4.0, p.surface_alt);
    }
    let mut left = rect.left();
    let name = format!("{} {}", icon(node.is_dir), node.name);
    cell(
        ui,
        rect,
        left,
        cols.name,
        // Folders in bold so they stand apart from files at a glance.
        if node.is_dir {
            egui::RichText::new(name).color(p.text).strong()
        } else {
            egui::RichText::new(name).color(p.muted)
        },
        false,
    );
    left += cols.name;
    let mut folder_rect = None;
    if let Some(width) = cols.folder {
        let folder = node.folder_path();
        let label = if folder.is_empty() {
            tr("(drive root)")
        } else {
            folder
        };
        let link = egui::RichText::new(label).color(p.accent);
        let hovered_cell = egui::Rect::from_x_y_ranges(left..=left + width, rect.y_range());
        let over = response
            .hover_pos()
            .is_some_and(|pos| hovered_cell.contains(pos));
        cell(
            ui,
            rect,
            left,
            width,
            if over { link.underline() } else { link },
            false,
        );
        folder_rect = Some(hovered_cell);
        left += width;
    }
    let size = if node.is_dir {
        egui::RichText::new(items_label(node.children.len())).color(p.muted)
    } else {
        egui::RichText::new(format_bytes(node.size))
            .monospace()
            .color(p.text)
    };
    cell(ui, rect, left, cols.size, size, true);
    left += cols.size;
    if let Some(width) = cols.kind {
        let kind = egui::RichText::new(type_label(&node.name, node.is_dir)).color(p.muted);
        cell(ui, rect, left, width, kind, false);
    }
    let response = response.on_hover_text(&node.path);
    let mut action = if response.double_clicked() && node.is_dir {
        Some(Action::Open(node.path.clone()))
    } else if response.clicked() {
        let on_folder = folder_rect
            .zip(response.interact_pointer_pos())
            .is_some_and(|(folder, pos)| folder.contains(pos));
        Some(if on_folder {
            Action::Reveal(node.path.clone())
        } else {
            Action::Select(node.path.clone())
        })
    } else {
        None
    };
    context::menu(&response, node, &mut action);
    action
}
