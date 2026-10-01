//! The versions side panel of one file: a newest-first timeline (time,
//! size, PC, kind, "Current") with "Restore this version" and "Restore as a
//! copy". Versions whose data is gone are greyed with the reason.

use super::state::{Change, HistoryForm};
use super::{args, badges, changes, clock, format, paths};
use crate::drive_history::model::{VersionEntry, VersionKind};
use crate::gui::i18n::{tr, trf};
use crate::gui::task::TaskRunner;
use crate::gui::theme;
use crate::presentation::format_bytes;
use eframe::egui;

/// Why a version cannot be restored (`None`: it can).
pub(crate) fn unavailable(version: &VersionEntry) -> Option<&'static str> {
    if version.restorable {
        None
    } else if version.kind == VersionKind::Deleted {
        Some(tr("Marks the deletion: there is no content to restore."))
    } else {
        Some(tr("Its data has expired or was deleted permanently."))
    }
}

/// `(restore, restore as copy)` allowed for the selected version.
pub(crate) fn allowed(version: Option<&VersionEntry>) -> (bool, bool) {
    match version {
        Some(v) if v.restorable => (!v.current, true),
        _ => (false, false),
    }
}

pub(crate) fn show(
    ui: &mut egui::Ui,
    history: &mut HistoryForm,
    task: &mut TaskRunner,
    rclone: &str,
    narrow: bool,
) {
    let Some(panel) = history.versions.as_mut() else {
        return;
    };
    let (folder, name) = paths::split(&panel.path);
    let mut close = false;
    let mut restore = None;
    theme::card(ui).inner_margin(12).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            if narrow {
                if ui
                    .button(format!("⏴ {}", tr("Back to the files")))
                    .clicked()
                {
                    close = true;
                }
            } else {
                ui.label(
                    egui::RichText::new(tr("Versions"))
                        .size(theme::CARD_TITLE_SIZE)
                        .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(tr("Close")).clicked() {
                        close = true;
                    }
                });
            }
        });
        ui.add(egui::Label::new(egui::RichText::new(format!("📄 {name}")).strong()).truncate());
        ui.add(egui::Label::new(egui::RichText::new(&folder).weak()).truncate());
        theme::hint(
            ui,
            tr("Restoring keeps the current content as a version, so nothing is lost."),
        );
        ui.add_space(theme::SUBSECTION_GAP);
        let versions = match &panel.fetch.value {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    theme::hint(ui, tr("Reading the versions…"));
                });
                return;
            }
            Some(Err(error)) => {
                ui.colored_label(
                    theme::error_colors(ui.visuals().dark_mode).1,
                    trf(
                        "The versions could not be read: {error}",
                        &[("error", error)],
                    ),
                );
                if ui.button(tr("Retry")).clicked() {
                    panel.fetch.stale = true;
                }
                return;
            }
            Some(Ok(versions)) => versions,
        };
        if versions.is_empty() {
            ui.weak(tr("No earlier versions of this file are recorded."));
            return;
        }
        if panel
            .selected
            .as_ref()
            .is_some_and(|id| !versions.iter().any(|v| &v.id == id))
        {
            panel.selected = None;
        }
        let now = crate::utils::now_unix();
        let offset = clock::offset();
        // The actions sit above the list so they stay in view however
        // long the list is.
        let chosen = panel
            .selected
            .as_deref()
            .and_then(|id| versions.iter().find(|v| v.id == id));
        let (can_restore, can_copy) = allowed(chosen);
        let idle = !task.is_running();
        ui.horizontal_wrapped(|ui| {
            if theme::primary_button(ui, idle && can_restore, tr("Restore this version"))
                .on_hover_text(tr("Make the selected version the current one."))
                .clicked()
            {
                restore = chosen.map(|v| (v.id.clone(), false));
            }
            if ui
                .add_enabled(idle && can_copy, egui::Button::new(tr("Restore as a copy")))
                .on_hover_text(tr(
                    "Save the selected version as a new file next to this one.",
                ))
                .clicked()
            {
                restore = chosen.map(|v| (v.id.clone(), true));
            }
            if panel.fetch.is_loading() {
                ui.spinner();
            }
        });
        match chosen {
            None => theme::hint(ui, tr("Select a version to restore it.")),
            Some(v) if v.current && v.restorable => {
                theme::hint(ui, tr("This is the current version."));
            }
            Some(v) => {
                if let Some(reason) = unavailable(v) {
                    theme::hint(ui, reason);
                }
            }
        }
        ui.add_space(theme::SUBSECTION_GAP);
        let mut clicked = None;
        egui::ScrollArea::vertical()
            .id_salt("library-versions")
            .max_height((ui.available_height() - 16.0).max(120.0))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for version in versions {
                    let selected = panel.selected.as_deref() == Some(version.id.as_str());
                    if entry(ui, version, selected, now, offset).clicked() {
                        clicked = Some(version.id.clone());
                    }
                }
            });
        if clicked.is_some() {
            panel.selected = clicked;
        }
    });
    let pool = panel.pool.clone();
    let path = panel.path.clone();
    if close {
        history.versions = None;
    } else if let Some((id, as_copy)) = restore {
        let change = Change::Version {
            name: name.clone(),
            as_copy,
        };
        let args = args::versions_restore(&pool, &path, &id, as_copy);
        changes::start(history, task, rclone, &pool, change, args);
    }
}

/// One timeline entry; greyed when its data is gone.
fn entry(
    ui: &mut egui::Ui,
    version: &VersionEntry,
    selected: bool,
    now: u64,
    offset: i64,
) -> egui::Response {
    let p = theme::pal(ui);
    let gone = unavailable(version);
    let frame = egui::Frame::new()
        .fill(if selected {
            p.accent_soft
        } else {
            egui::Color32::TRANSPARENT
        })
        .stroke(egui::Stroke::new(
            1.0,
            if selected { p.accent } else { p.border },
        ))
        .corner_radius(theme::CORNER_RADIUS)
        .inner_margin(egui::Margin::symmetric(10, 6));
    let response = frame
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if gone.is_some() {
                ui.multiply_opacity(0.55);
            }
            ui.horizontal_wrapped(|ui| {
                badges::kind(ui, version.kind);
                if version.current {
                    badges::current(ui);
                }
                let (age, at) = format::when(now, version.time_unix, offset);
                ui.label(egui::RichText::new(age).strong())
                    .on_hover_text(at);
            });
            let (_, at) = format::when(now, version.time_unix, offset);
            let mut line = vec![];
            if !at.is_empty() {
                line.push(at);
            }
            if version.kind != VersionKind::Deleted {
                line.push(format_bytes(version.size));
            }
            if let Some(author) = &version.author {
                line.push(author.clone());
            }
            ui.weak(line.join(" · "));
            if let Some(reason) = gone {
                ui.small(reason);
            }
        })
        .response;
    ui.add_space(4.0);
    ui.interact(
        response.rect,
        ui.id().with(("version", &version.id)),
        egui::Sense::click(),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_rules() {
        let versions = super::super::sample::versions("/a.txt", 1_000_000_000);
        let current = &versions[0];
        assert!(current.current);
        assert_eq!(allowed(Some(current)), (false, true));
        assert_eq!(allowed(Some(&versions[1])), (true, true));
        let deleted = versions
            .iter()
            .find(|v| v.kind == VersionKind::Deleted)
            .unwrap();
        assert_eq!(allowed(Some(deleted)), (false, false));
        assert!(unavailable(deleted).unwrap().contains("deletion"));
        let expired = versions.last().unwrap();
        assert!(unavailable(expired).unwrap().contains("expired"));
        assert_eq!(unavailable(current), None);
        assert_eq!(allowed(None), (false, false));
    }
}
