//! One compact provider card: name and backend type, usage bar, capacity
//! line, encryption status with its crypt remotes, manual setup, and the
//! account limits (24 h upload budget, pause, last activity, editor and
//! keep-alive).

use super::limits_model::{card_limits, Tone};
use super::model::ProviderCard;
use crate::gui::i18n::{tr, trf};
use crate::gui::theme;
use crate::gui::widgets::{capacity_bar_colored, capacity_bar_sized, status_badge};
use crate::presentation::format_bytes;
use eframe::egui;

/// A button the user clicked on a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardAction {
    SetupEncryption,
    EditLimits,
    KeepAlive,
}

fn tone_color(ui: &egui::Ui, tone: Tone) -> egui::Color32 {
    let p = theme::pal(ui);
    match tone {
        Tone::Muted => p.muted,
        Tone::Warning => theme::warning_colors(ui.visuals().dark_mode).1,
        Tone::Danger => p.danger,
    }
}

/// The account-limit lines of a card.
fn limits_section(ui: &mut egui::Ui, card: &ProviderCard<'_>) {
    let Some(status) = card.limits else {
        return;
    };
    let view = card_limits(status, crate::utils::now_unix());
    ui.add_space(4.0);
    if let Some(budget) = &view.budget {
        let paused = view
            .state
            .as_ref()
            .is_some_and(|(_, tone)| *tone == Tone::Warning);
        if paused {
            let (fill, text) = theme::warning_colors(ui.visuals().dark_mode);
            capacity_bar_colored(
                ui,
                Some(budget.ratio),
                &budget.bar,
                ui.available_width(),
                text,
                fill,
            );
        } else {
            capacity_bar_sized(ui, Some(budget.ratio), &budget.bar, ui.available_width());
        }
        ui.add(
            egui::Label::new(
                egui::RichText::new(&budget.line)
                    .small()
                    .color(theme::pal(ui).muted),
            )
            .truncate(),
        );
    }
    if let Some((text, tone)) = &view.state {
        let color = tone_color(ui, *tone);
        ui.add(egui::Label::new(egui::RichText::new(text).small().color(color)).wrap());
    }
    let color = tone_color(ui, view.activity_tone);
    ui.add(egui::Label::new(egui::RichText::new(&view.activity).small().color(color)).wrap())
        .on_hover_text(tr("Measured from this computer's successful calls. Providers decide what counts as activity; RPool cannot guarantee that API access keeps an account."));
}

/// Draws the card; returns the clicked action.
pub(crate) fn show(ui: &mut egui::Ui, card: &ProviderCard<'_>, idle: bool) -> Option<CardAction> {
    let p = theme::pal(ui);
    let mut action = None;
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
                if card.missing
                    && ui
                        .add_enabled(idle, egui::Button::new(tr("Set up encryption…")).small())
                        .clicked()
                {
                    action = Some(CardAction::SetupEncryption);
                }
            });
            limits_section(ui, card);
            ui.horizontal_wrapped(|ui| {
                if ui.add(egui::Button::new(tr("Limits…")).small()).clicked() {
                    action = Some(CardAction::EditLimits);
                }
                if ui
                    .add_enabled(idle, egui::Button::new(tr("Keep alive now")).small())
                    .on_hover_text(tr("One cheap authenticated call to the account, recorded as activity."))
                    .clicked()
                {
                    action = Some(CardAction::KeepAlive);
                }
            });
        });
    action
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
