//! The Icons view: a grid of tiles with a large icon and a wrapped name.

use super::action::Action;
use super::scroll::reveal_offset;
use super::state::ExplorerState;
use super::summary::icon;
use crate::gui::screens::files::inventory::drive_state::DriveTree;
use crate::gui::theme;
use eframe::egui;

const TILE: egui::Vec2 = egui::vec2(112.0, 96.0);

/// Tiles per row at this width (at least one).
pub(crate) fn columns(width: f32) -> usize {
    ((width / TILE.x).floor() as usize).max(1)
}

pub(crate) fn show(
    ui: &mut egui::Ui,
    tree: &DriveTree,
    rows: &[usize],
    state: &mut ExplorerState,
    height: f32,
) -> Option<Action> {
    let mut action = None;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
        let per_row = columns(ui.available_width());
        let lines = rows.len().div_ceil(per_row);
        let selected = state.selected.as_deref();
        let mut area = egui::ScrollArea::vertical()
            .id_salt("library-explorer-icons")
            .max_height(height)
            .auto_shrink([false, false]);
        if std::mem::take(&mut state.reveal) {
            if let Some(index) =
                selected.and_then(|path| rows.iter().position(|&id| tree.nodes[id].path == path))
            {
                let offset = reveal_offset(index / per_row, TILE.y, state.scroll);
                area = area.vertical_scroll_offset(offset);
            }
        }
        let output = area.show_rows(ui, TILE.y, lines, |ui, range| {
            for line in range {
                ui.horizontal(|ui| {
                    let start = line * per_row;
                    for &id in rows.iter().skip(start).take(per_row) {
                        let is_selected = selected == Some(tree.nodes[id].path.as_str());
                        if let Some(clicked) = tile(ui, tree, id, is_selected) {
                            action = Some(clicked);
                        }
                    }
                });
            }
        });
        state.scroll.offset = output.state.offset.y;
        state.scroll.view = output.inner_rect.height();
    });
    action
}

fn tile(ui: &mut egui::Ui, tree: &DriveTree, id: usize, selected: bool) -> Option<Action> {
    let node = &tree.nodes[id];
    let (rect, response) = ui.allocate_exact_size(TILE, egui::Sense::click());
    let p = theme::pal(ui);
    let body = rect.shrink(3.0);
    if selected {
        ui.painter().rect_filled(body, 6.0, p.accent_soft);
    } else if response.hovered() {
        ui.painter().rect_filled(body, 6.0, p.surface_alt);
    }
    let painter = ui.painter().with_clip_rect(body.intersect(ui.clip_rect()));
    painter.text(
        egui::pos2(body.center().x, body.top() + 6.0),
        egui::Align2::CENTER_TOP,
        icon(node.is_dir),
        egui::FontId::proportional(30.0),
        // Folders in the classic folder colour, files muted.
        if node.is_dir {
            theme::warning_colors(ui.visuals().dark_mode).1
        } else {
            p.muted
        },
    );
    let mut job = egui::text::LayoutJob::simple(
        node.name.clone(),
        egui::FontId::proportional(12.5),
        p.text,
        body.width() - 8.0,
    );
    job.wrap.max_rows = 2;
    job.wrap.break_anywhere = true;
    job.halign = egui::Align::Center;
    let galley = painter.layout_job(job);
    painter.galley(
        egui::pos2(body.center().x, body.top() + 46.0),
        galley,
        p.text,
    );
    let response = response.on_hover_text(&node.path);
    if response.double_clicked() && node.is_dir {
        Some(Action::Open(node.path.clone()))
    } else if response.clicked() {
        Some(Action::Select(node.path.clone()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn grid_fits_whole_tiles() {
        assert_eq!(super::columns(50.0), 1);
        assert_eq!(super::columns(224.0), 2);
        assert_eq!(super::columns(1000.0), 8);
    }
}
