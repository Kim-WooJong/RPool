//! The live part of a mount card: summary line, queue, alert banners and one
//! block per account with rates, sparkline, totals and errors.
use super::chart;
use super::ring::Ring;
use super::view_model::{activity_label, Activity, LiveView, RemoteView};
use crate::gui::i18n::tr;
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use crate::monitor::model::AlertKind;
use eframe::egui;
use std::collections::BTreeMap;

/// "Uploading" (green, with a pulsing dot), "Downloading" (blue) or "Idle"
/// (grey).
pub(crate) fn activity_badge(ui: &mut egui::Ui, activity: Activity) {
    let tone = match activity {
        Activity::Uploading => StatusTone::Success,
        Activity::Downloading => StatusTone::Info,
        Activity::Idle => StatusTone::Neutral,
    };
    // The dot sits left of the badge also in right-to-left header rows.
    let right_to_left = ui.layout().prefer_right_to_left();
    if right_to_left {
        status_badge(ui, activity_label(activity), tone);
    }
    if activity == Activity::Uploading {
        let (_, color) = theme::success_colors(ui.visuals().dark_mode);
        let time = ui.input(|i| i.time) as f32;
        let alpha = 0.45 + 0.55 * (0.5 + 0.5 * (time * 4.0).sin());
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), 4.0, color.gamma_multiply(alpha));
    }
    if !right_to_left {
        status_badge(ui, activity_label(activity), tone);
    }
}

/// A coloured banner: warning for a stalled queue or an upload limit, error
/// otherwise.
fn alert_banner(ui: &mut egui::Ui, kind: AlertKind, text: &str) {
    let dark = ui.visuals().dark_mode;
    let (fill, foreground) = match kind {
        AlertKind::Stalled | AlertKind::MetadataGrowing | AlertKind::UploadLimit => {
            theme::warning_colors(dark)
        }
        AlertKind::Errors | AlertKind::Unreachable => theme::error_colors(dark),
    };
    egui::Frame::new()
        .fill(fill)
        .corner_radius(egui::CornerRadius::same(theme::CORNER_RADIUS))
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(format!("⚠ {text}")).color(foreground));
        });
    ui.add_space(4.0);
}

/// Summary lines of a mount card: uptime, last sync, upload queue and alert
/// banners. Called by the Monitoring page's `mount_card`.
pub(crate) fn summary(ui: &mut egui::Ui, live: &LiveView) {
    ui.horizontal_wrapped(|ui| {
        ui.label(&live.uptime);
        ui.label("·");
        ui.label(&live.last_sync);
    });
    ui.horizontal_wrapped(|ui| {
        let p = theme::pal(ui);
        let color = if live.queue_waiting { p.text } else { p.muted };
        ui.label(egui::RichText::new(&live.queue).color(color));
        if let Some(oldest) = &live.queue_oldest {
            theme::hint(ui, oldest);
        }
    });
    if !live.alerts.is_empty() {
        ui.add_space(4.0);
        for (kind, text) in &live.alerts {
            alert_banner(ui, *kind, text);
        }
    }
}

/// One block per account (two per row when wide enough) with its live
/// rates, sparkline from `rings` and totals. Called for the Live tab.
pub(crate) fn remotes(
    ui: &mut egui::Ui,
    live: &LiveView,
    rings: &BTreeMap<String, Ring>,
    now: u64,
) {
    if live.remotes.is_empty() {
        theme::hint(ui, tr("No account has moved data yet."));
        return;
    }
    let pairs = ui.available_width() >= theme::TWO_COLUMN_MIN;
    for chunk in live.remotes.chunks(if pairs { 2 } else { 1 }) {
        if pairs {
            ui.columns(2, |columns| {
                for (column, remote) in columns.iter_mut().zip(chunk) {
                    column.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        remote_block(ui, remote, rings.get(&remote.remote), now)
                    });
                }
            });
        } else {
            remote_block(ui, &chunk[0], rings.get(&chunk[0].remote), now);
        }
        ui.add_space(theme::SUBSECTION_GAP);
    }
}

/// One account block: name, backend and activity badges, upload-limit badge,
/// 10 s rates, sparkline, byte totals, transfer/op counts and the last error.
fn remote_block(ui: &mut egui::Ui, remote: &RemoteView, ring: Option<&Ring>, now: u64) {
    let p = theme::pal(ui);
    let (up_color, down_color) = chart::colors(ui);
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(egui::CornerRadius::same(theme::CORNER_RADIUS))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::Label::new(egui::RichText::new(&remote.remote).strong()).wrap());
                if let Some(backend) = &remote.backend {
                    status_badge(ui, backend, StatusTone::Neutral);
                }
                activity_badge(ui, remote.activity);
                if let Some(detail) = &remote.upload_limit {
                    ui.scope(|ui| status_badge(ui, tr("Upload limit"), StatusTone::Warning))
                        .response
                        .on_hover_text(detail);
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(format!("↑ {}", remote.upload_rate))
                        .strong()
                        .color(up_color),
                );
                ui.label(
                    egui::RichText::new(format!("↓ {}", remote.download_rate))
                        .strong()
                        .color(down_color),
                );
                theme::hint(ui, tr("10 s average · last 10 minutes"));
            });
            chart::sparkline(ui, ring, now);
            ui.add(egui::Label::new(egui::RichText::new(&remote.totals).small()).wrap());
            ui.horizontal_wrapped(|ui| {
                theme::hint(ui, &remote.transfers);
                theme::hint(ui, "·");
                theme::hint(ui, &remote.ops);
                theme::hint(ui, "·");
                theme::hint(ui, &remote.last_ok);
            });
            if let Some(error) = &remote.last_error {
                ui.add(egui::Label::new(egui::RichText::new(error).small().color(p.danger)).wrap());
            }
        });
}
