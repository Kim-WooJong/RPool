//! Step 4: the files that cannot be recovered (list only; `pool migrate lost`).
use super::state::{short_id, Step};
use crate::gui::i18n::{tr, trf};
use crate::gui::state::GuiState;
use crate::gui::theme;
use crate::migration::model::{GroupLoss, LostFile, MissingReason};
use crate::presentation::format_bytes;
use eframe::egui;

pub(crate) fn reason_label(reason: MissingReason) -> &'static str {
    match reason {
        MissingReason::RemoteRemoved => "account removed",
        MissingReason::Missing => "missing",
        MissingReason::BadSize => "bad size",
        MissingReason::Corrupt => "corrupt",
        MissingReason::ProviderError => "provider error",
    }
}

/// `reason_label` in the GUI language (the copied list stays English).
fn reason_display(reason: MissingReason) -> &'static str {
    match reason {
        MissingReason::RemoteRemoved => tr("account removed"),
        MissingReason::Missing => tr("missing"),
        MissingReason::BadSize => tr("bad size"),
        MissingReason::Corrupt => tr("corrupt"),
        MissingReason::ProviderError => tr("provider error"),
    }
}

/// "g3: 1/3 available (K=2)" per short group, joined with "; ".
/// `localized` for the on-screen table; English for the clipboard.
fn groups_line(groups: &[GroupLoss], localized: bool) -> String {
    groups
        .iter()
        .map(|g| {
            let total = g.available + g.missing.len();
            if localized {
                trf(
                    "g{group}: {available}/{total} available (K={k})",
                    &[
                        ("group", &g.group),
                        ("available", &g.available),
                        ("total", &total),
                        ("k", &g.required_k),
                    ],
                )
            } else {
                format!(
                    "g{}: {}/{} available (K={})",
                    g.group, g.available, total, g.required_k
                )
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// "remote #index (reason)" of every missing shard, joined with ", ".
fn missing_line(groups: &[GroupLoss], localized: bool) -> String {
    groups
        .iter()
        .flat_map(|g| &g.missing)
        .map(|m| {
            let reason = if localized {
                reason_display(m.reason)
            } else {
                reason_label(m.reason)
            };
            format!("{} #{} ({reason})", m.remote, m.index)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// English group summary (clipboard, tests).
pub(crate) fn groups_text(groups: &[GroupLoss]) -> String {
    groups_line(groups, false)
}

/// English missing-shard summary (clipboard, tests).
pub(crate) fn missing_text(groups: &[GroupLoss]) -> String {
    missing_line(groups, false)
}

/// Tab-separated list with a header, for the clipboard.
pub(crate) fn copy_text(files: &[LostFile]) -> String {
    let mut out = String::from("name\tsize\tarchive_id\tgroups\tmissing shards\n");
    for file in files {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            file.original_name.replace(['\t', '\n'], " "),
            file.size,
            file.archive_id,
            groups_text(&file.groups),
            missing_text(&file.groups)
        ));
    }
    out
}

pub(super) fn show(ui: &mut egui::Ui, state: &mut GuiState) {
    let files = state.migration.active_lost();
    let id = state.migration.active_id.clone().unwrap_or_default();
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(trf(
                "{n} unrecoverable file(s) in migration {id}",
                &[("n", &files.len()), ("id", &short_id(&id))],
            ))
            .strong(),
        );
        if ui
            .add_enabled(!files.is_empty(), egui::Button::new(tr("Copy list")))
            .clicked()
        {
            ui.ctx().copy_text(copy_text(&files));
            state.migration.notice = Some(tr("Lost file list copied.").into());
        }
        if ui.button(tr("Back")).clicked() {
            state.migration.step =
                if state.migration.plan.is_some() && state.migration.active_status().is_none() {
                    Step::Review
                } else {
                    Step::Run
                };
        }
    });
    theme::hint(ui, tr("These files have a group with fewer than K readable shards. The migration skips them; the originals stay where they are. Reattaching a removed account and planning again may bring them back."));
    if files.is_empty() {
        theme::hint(ui, tr("No lost files."));
        return;
    }
    let height = theme::list_height(ui.ctx().content_rect().height());
    let size = egui::vec2(ui.available_width(), height);
    theme::fixed_pane_wide(ui, "migration-lost", size, |ui| {
        egui::Grid::new("migration-lost-grid")
            .striped(true)
            .num_columns(5)
            .show(ui, |ui| {
                for head in [
                    tr("Name"),
                    tr("Size"),
                    tr("Archive"),
                    tr("Groups"),
                    tr("Missing shards"),
                ] {
                    ui.strong(head);
                }
                ui.end_row();
                for file in &files {
                    ui.label(&file.original_name);
                    ui.label(format_bytes(file.size));
                    ui.monospace(short_id(&file.archive_id))
                        .on_hover_text(&file.archive_id);
                    ui.label(groups_line(&file.groups, true));
                    ui.label(missing_line(&file.groups, true));
                    ui.end_row();
                }
            });
    });
}
