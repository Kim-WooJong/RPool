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
    let unavailable = reports.iter().filter(|report| report.error.is_some()).count();
    let pool_attention = pools_needing_attention(&data.pools, crypt_remotes);

    if ui.available_width() < 650.0 {
        metric(ui, "Reported capacity", if known_total > 0 {
            format!("{} / {}", format_bytes(known_used), format_bytes(known_total))
        } else { "Unavailable".into() }, None);
        metric(ui, "Files", data.file_count.to_string(),
            Some(format!("{} logical data", format_bytes(data.logical_bytes))));
        health_card(ui, reports, refreshing, usage_error, unavailable, pool_attention, data);
        return;
    }

    ui.columns(3, |columns| {
        metric(
            &mut columns[0],
            "Reported capacity",
            if known_total > 0 {
                format!("{} / {}", format_bytes(known_used), format_bytes(known_total))
            } else {
                "Unavailable".to_string()
            },
            if known_total > 0 {
                Some(format!("{:.1}% used", known_used as f64 / known_total as f64 * 100.0))
            } else {
                None
            },
        );

        metric(
            &mut columns[1],
            "Files",
            data.file_count.to_string(),
            Some(format!("{} logical data", format_bytes(data.logical_bytes))),
        );

        health_card(&mut columns[2], reports, refreshing, usage_error, unavailable, pool_attention, data);
    });

    ui.add_space(theme::SECTION_GAP);
    ui.separator();
}

fn health_card(ui: &mut egui::Ui, reports: &[QuotaReport], refreshing: bool,
    usage_error: Option<&str>, unavailable: usize, pool_attention: usize, data: &DashboardData) {
    theme::card(ui).show(ui, |ui| {
            ui.set_min_height(92.0);
        ui.label(egui::RichText::new("Health").weak());
        ui.add_space(4.0);
        if refreshing && reports.is_empty() {
            status_badge(ui, "Refreshing", StatusTone::Neutral);
        } else if usage_error.is_some()
            || unavailable > 0
            || pool_attention > 0
            || !data.warnings.is_empty()
        {
            status_badge(ui, "Attention", StatusTone::Warning);
        } else if reports.is_empty() {
            status_badge(ui, "Unknown", StatusTone::Neutral);
        } else {
            status_badge(ui, "Healthy", StatusTone::Success);
        }
        ui.add_space(4.0);
        ui.small(format!(
            "{} provider{} reporting",
            reports.len().saturating_sub(unavailable),
            if reports.len().saturating_sub(unavailable) == 1 { "" } else { "s" }
        ));
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
        .filter_map(|report| match (report.used, report.total, report.error.as_ref()) {
            (Some(used), Some(total), None) => Some((used, total)),
            _ => None,
        })
        .fold((0_u64, 0_u64), |(used_acc, total_acc), (used, total)| {
            (used_acc.saturating_add(used), total_acc.saturating_add(total))
        })
}
