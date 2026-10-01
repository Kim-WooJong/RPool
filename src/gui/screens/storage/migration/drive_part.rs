//! The drive part of the wizard's review and run steps: what the plan does
//! with the drive's files, and how far the run got. Mirrors the drive lines
//! of `pool migrate plan` / `status`.
use super::review_step::quota_verdict;
use super::state::format_eta;
use crate::gui::i18n::{tr, trf};
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use crate::migration::drive_model::{DrivePlan, DriveStatus, GenerationRef};
use crate::presentation::format_bytes;
use eframe::egui;

/// "original layout" / "layout 0123456789ab" in the GUI language.
pub(crate) fn generation_text(generation: &GenerationRef) -> String {
    match &generation.epoch {
        None => tr("original layout").into(),
        Some(epoch) => trf(
            "layout {epoch}",
            &[("epoch", &&epoch[..epoch.len().min(12)])],
        ),
    }
}

/// Label and tone of a drive part's state.
pub(crate) fn drive_state(status: &DriveStatus) -> (&'static str, StatusTone) {
    if status.adopted.is_some() {
        (tr("Drive adopted"), StatusTone::Success)
    } else if status.ready {
        (tr("Drive ready to adopt"), StatusTone::Info)
    } else if status.frozen {
        (tr("Drive moving (frozen)"), StatusTone::Warning)
    } else {
        (tr("Drive not started"), StatusTone::Neutral)
    }
}

pub(super) fn review(ui: &mut egui::Ui, drive: &DrivePlan) {
    ui.separator();
    ui.label(
        egui::RichText::new(trf(
            "Drive: {files} files ({source})",
            &[
                ("files", &drive.entries.len()),
                ("source", &generation_text(&drive.source)),
            ],
        ))
        .strong(),
    );
    let c = &drive.counts;
    ui.horizontal_wrapped(|ui| {
        status_badge(
            ui,
            &trf("{n} kept", &[("n", &c.unaffected)]),
            StatusTone::Neutral,
        );
        status_badge(
            ui,
            &trf("{n} relocate", &[("n", &c.relocate)]),
            StatusTone::Info,
        );
        status_badge(
            ui,
            &trf("{n} re-encode", &[("n", &c.reencode)]),
            StatusTone::Info,
        );
        status_badge(
            ui,
            &trf("{n} lost", &[("n", &c.lost)]),
            if c.lost > 0 {
                StatusTone::Error
            } else {
                StatusTone::Neutral
            },
        );
        if c.unknown > 0 {
            status_badge(
                ui,
                &trf("{n} unknown", &[("n", &c.unknown)]),
                StatusTone::Warning,
            );
        }
    });
    egui::Grid::new("migration-drive-review-grid")
        .num_columns(2)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            ui.label(tr("Download"));
            ui.label(format_bytes(drive.download_bytes));
            ui.end_row();
            ui.label(tr("Upload"));
            ui.label(format_bytes(drive.upload_bytes));
            ui.end_row();
            ui.label(tr("New cloud space"));
            ui.label(format_bytes(drive.new_storage_bytes));
            ui.end_row();
            ui.label(tr("Estimated time"));
            ui.label(format_eta(drive.estimated_seconds));
            ui.end_row();
            ui.label(tr("Quota (archives and drive)"));
            let (label, tone) = quota_verdict(drive.quota_ok);
            status_badge(ui, label, tone);
            ui.end_row();
        });
    if !drive.bootstrap_ok {
        ui.colored_label(
            ui.visuals().error_fg_color,
            tr("The drive has too many files for a new PC to open in one go; its adoption will be refused. Archives still migrate."),
        );
    }
    theme::hint(ui, tr("The drive switches only when you adopt it after the run. Until then every PC keeps the current drive; once the drive part starts, PCs on it keep their changes locally instead of uploading them."));
    for note in &drive.notes {
        theme::hint(ui, &format!("• {note}"));
    }
}

pub(super) fn progress(ui: &mut egui::Ui, status: &DriveStatus) {
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.strong(tr("Drive"));
        ui.label(generation_text(&status.source));
        let (label, tone) = drive_state(status);
        status_badge(ui, label, tone);
    });
    let fraction = if status.to_move == 0 {
        1.0
    } else {
        status.switched as f32 / status.to_move as f32
    };
    ui.add(
        egui::ProgressBar::new(fraction)
            .desired_height(theme::PROGRESS_BAR_HEIGHT)
            .text(trf(
                "{done}/{total} drive files ready",
                &[("done", &status.switched), ("total", &status.to_move)],
            )),
    );
    ui.horizontal_wrapped(|ui| {
        if !status.lost.is_empty() {
            status_badge(
                ui,
                &trf("{n} lost", &[("n", &status.lost.len())]),
                StatusTone::Error,
            );
        }
        if status.failed_unknown > 0 {
            status_badge(
                ui,
                &trf("{n} unknown (retry)", &[("n", &status.failed_unknown)]),
                StatusTone::Warning,
            );
        }
    });
}
