//! The history part of a mount card: range picker, one bar chart per
//! account and the totals of the range.
use super::chart;
use super::format;
use super::history::{HistoryState, Range};
use crate::gui::i18n::{tr, trf};
use crate::gui::theme;
use eframe::egui;

/// Translated name of a range in the picker ("1 hour", "24 hours", …).
pub(crate) fn range_label(range: Range) -> &'static str {
    match range {
        Range::Hour => tr("1 hour"),
        Range::Day => tr("24 hours"),
        Range::Week => tr("7 days"),
        Range::Month => tr("30 days"),
    }
}

/// Translated bar size of a range, shown with the chart.
fn bucket_label(range: Range) -> &'static str {
    match range {
        Range::Hour => tr("one bar per minute"),
        Range::Day => tr("one bar per 15 minutes"),
        Range::Week => tr("one bar per hour"),
        Range::Month => tr("one bar per 6 hours"),
    }
}

/// Draws the history; returns the range the user picked.
pub(crate) fn show(ui: &mut egui::Ui, range: Range, history: &HistoryState) -> Range {
    let mut picked = range;
    ui.horizontal_wrapped(|ui| {
        for candidate in Range::ALL {
            ui.selectable_value(&mut picked, candidate, range_label(candidate));
        }
        if history.loading() {
            ui.spinner();
        }
    });
    let (up_color, down_color) = chart::colors(ui);
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(tr("↑ upload")).color(up_color).small());
        ui.label(
            egui::RichText::new(tr("↓ download"))
                .color(down_color)
                .small(),
        );
        theme::hint(ui, bucket_label(range));
    });
    ui.add_space(4.0);
    let Some(chart) = history.chart.as_ref().filter(|chart| chart.range == range) else {
        theme::hint(ui, tr("Loading history…"));
        return picked;
    };
    if chart.remotes.is_empty() {
        theme::hint(ui, tr("No traffic was recorded in this range."));
        return picked;
    }
    for remote in &chart.remotes {
        ui.add(egui::Label::new(egui::RichText::new(&remote.remote).strong()).wrap());
        chart::history_bars(ui, remote);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!("↑ {}", format::bytes(remote.total_up)))
                    .color(up_color),
            );
            ui.label(
                egui::RichText::new(format!("↓ {}", format::bytes(remote.total_down)))
                    .color(down_color),
            );
            theme::hint(
                ui,
                &trf(
                    "{ok} OK · {failed} failed",
                    &[("ok", &remote.ok_ops), ("failed", &remote.failed_ops)],
                ),
            );
        });
        ui.add_space(theme::SUBSECTION_GAP);
    }
    picked
}
