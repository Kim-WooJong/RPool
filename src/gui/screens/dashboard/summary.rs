use crate::gui::i18n::{tr, trf};
use crate::gui::screens::dashboard::health::pools_needing_attention;
use crate::gui::screens::dashboard::DashboardData;
use crate::gui::theme;
use crate::gui::widgets::{status_badge, StatusTone};
use crate::models::QuotaReport;
use crate::presentation::format_bytes;
use eframe::egui;

pub(crate) fn show(
    ui: &mut egui::Ui,
    data: &DashboardData,
    reports: &[QuotaReport],
    usage_error: Option<&str>,
    crypt_remotes: &[String],
    refreshing: bool,
) {
    let (known_used, known_total) = aggregate_capacity(reports);
    let unavailable = reports
        .iter()
        .filter(|report| report.error.is_some())
        .count();
    let pool_attention = pools_needing_attention(&data.pools, crypt_remotes);

    // Keep Health next to capacity even inside the compact overview pane.
    // The enclosing overview scroll area provides horizontal overflow if needed.
    ui.set_min_width(320.0);
    ui.columns(2, |columns| {
        metric(
            &mut columns[0],
            tr("Reported capacity"),
            if known_total > 0 {
                format!(
                    "{} / {}",
                    format_bytes(known_used),
                    format_bytes(known_total)
                )
            } else {
                tr("Unavailable").to_string()
            },
            if known_total > 0 {
                Some(trf(
                    "{percent}% used",
                    &[(
                        "percent",
                        &format!("{:.1}", known_used as f64 / known_total as f64 * 100.0),
                    )],
                ))
            } else {
                None
            },
        );
        health_card(
            &mut columns[1],
            reports,
            refreshing,
            usage_error,
            unavailable,
            pool_attention,
            data,
        );
    });
    ui.add_space(4.0);
    metric(
        ui,
        tr("Files"),
        data.file_count.to_string(),
        Some(trf(
            "{size} logical data",
            &[("size", &format_bytes(data.logical_bytes))],
        )),
    );

    ui.add_space(theme::SECTION_GAP);
    ui.separator();
}

fn health_card(
    ui: &mut egui::Ui,
    reports: &[QuotaReport],
    refreshing: bool,
    usage_error: Option<&str>,
    unavailable: usize,
    pool_attention: usize,
    data: &DashboardData,
) {
    theme::card(ui).show(ui, |ui| {
        ui.set_min_height(92.0);
        ui.label(egui::RichText::new(tr("Health")).weak());
        ui.add_space(4.0);
        if refreshing && reports.is_empty() {
            status_badge(ui, tr("Refreshing"), StatusTone::Neutral);
        } else if usage_error.is_some()
            || unavailable > 0
            || pool_attention > 0
            || !data.warnings.is_empty()
        {
            status_badge(ui, tr("Attention"), StatusTone::Warning);
        } else if reports.is_empty() {
            status_badge(ui, tr("Unknown"), StatusTone::Neutral);
        } else {
            status_badge(ui, tr("Healthy"), StatusTone::Success);
        }
        ui.add_space(4.0);
        let reporting = reports.len().saturating_sub(unavailable);
        ui.small(if reporting == 1 {
            trf("{n} provider reporting", &[("n", &reporting)])
        } else {
            trf("{n} providers reporting", &[("n", &reporting)])
        });
    });
}

fn metric(ui: &mut egui::Ui, label: &str, value: String, detail: Option<String>) {
    theme::card(ui).show(ui, |ui| {
        ui.set_min_height(92.0);
        ui.label(egui::RichText::new(label).weak());
        ui.add_space(2.0);
        ui.label(egui::RichText::new(value).size(20.0).strong());
        if let Some(detail) = detail {
            ui.small(detail);
        }
    });
}

fn aggregate_capacity(reports: &[QuotaReport]) -> (u64, u64) {
    reports
        .iter()
        .filter_map(
            |report| match (report.used, report.total, report.error.as_ref()) {
                (Some(used), Some(total), None) => Some((used, total)),
                _ => None,
            },
        )
        .fold((0_u64, 0_u64), |(used_acc, total_acc), (used, total)| {
            (
                used_acc.saturating_add(used),
                total_acc.saturating_add(total),
            )
        })
}
