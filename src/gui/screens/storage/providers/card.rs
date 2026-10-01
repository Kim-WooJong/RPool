//! One compact provider card: name and backend type, usage bar, capacity
//! line, encryption status with its crypt remotes, and manual setup.

use super::model::ProviderCard;
use crate::gui::i18n::{tr, trf};
use crate::gui::theme;
use crate::gui::widgets::{capacity_bar_sized, status_badge};
use crate::presentation::format_bytes;
use eframe::egui;

/// Draws the card; returns true when "Set up encryption…" was clicked.
pub(crate) fn show(ui: &mut egui::Ui, card: &ProviderCard<'_>, idle: bool) -> bool {
    let p = theme::pal(ui);
    let mut setup = false;
    egui::Frame::new()
        // The page card's fill: the usage bar's track stays visible.
        .fill(p.surface)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(theme::CORNER_RADIUS)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(egui::RichText::new(card.name).strong()).truncate());
                if let Some(kind) = card.kind {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(kind).small().monospace().color(p.muted));
                    });
                }
            });
            ui.add_space(4.0);
            let text = match card.report.and_then(|r| r.used_percent) {
                Some(percent) => format!("{percent:.0}%"),
                None => tr("n/a").to_string(),
            };
            capacity_bar_sized(ui, card.ratio(), &text, ui.available_width());
            ui.add(egui::Label::new(egui::RichText::new(capacity_line(card)).small().color(p.muted)).truncate());
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.scope(|ui| status_badge(ui, card.status, card.tone))
                    .response
                    .on_hover_text(tr("Configuration status only; cloud access and key recovery are not verified by this indicator."));
                for crypt in card.crypts {
                    ui.label(egui::RichText::new(crypt).small().monospace().color(p.muted));
                }
                if card.missing {
                    setup = ui
                        .add_enabled(idle, egui::Button::new(tr("Set up encryption…")).small())
                        .clicked();
                }
            });
        });
    setup
}

/// "4.0 GiB used of 15.0 GiB · 11.0 GiB free", or why it is unknown.
fn capacity_line(card: &ProviderCard<'_>) -> String {
    let Some(report) = card.report else {
        return tr("Capacity not reported yet").to_string();
    };
    if report.error.is_some() {
        return tr("Capacity unavailable").to_string();
    }
    match (report.used, report.total, report.free) {
        (Some(used), Some(total), free) => {
            let line = trf(
                "{used} used of {total}",
                &[
                    ("used", &format_bytes(used)),
                    ("total", &format_bytes(total)),
                ],
            );
            match free {
                Some(free) => format!(
                    "{line} · {}",
                    trf("{free} free", &[("free", &format_bytes(free))])
                ),
                None => line,
            }
        }
        (Some(used), None, _) => trf("{used} used", &[("used", &format_bytes(used))]),
        _ => tr("Capacity unavailable").to_string(),
    }
}
