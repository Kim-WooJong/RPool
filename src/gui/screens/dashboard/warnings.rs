//! Attention pane of the overview page: capacity refresh errors, load
//! warnings, pools without usable remotes and provider errors.
use crate::gui::i18n::{tr, trf};
use crate::gui::screens::dashboard::health::{pool_health, PoolHealth};
use crate::gui::screens::dashboard::DashboardData;
use crate::gui::theme;
use crate::models::QuotaReport;
use eframe::egui;

/// Draw the attention list; nothing when there is nothing to report.
pub(crate) fn show(
    ui: &mut egui::Ui,
    data: &DashboardData,
    reports: &[QuotaReport],
    usage_error: Option<&str>,
    crypt_remotes: &[String],
) {
    let provider_errors: Vec<&QuotaReport> = reports
        .iter()
        .filter(|report| report.error.is_some())
        .collect();

    let pool_warnings: Vec<_> = data
        .pools
        .iter()
        .filter(|pool| {
            matches!(
                pool_health(pool, crypt_remotes),
                PoolHealth::Empty | PoolHealth::MissingRemote
            )
        })
        .collect();

    if data.warnings.is_empty()
        && provider_errors.is_empty()
        && pool_warnings.is_empty()
        && usage_error.is_none()
    {
        return;
    }

    ui.label(
        egui::RichText::new(tr("Attention"))
            .size(theme::SECTION_TITLE_SIZE)
            .strong(),
    );
    ui.add_space(theme::SUBSECTION_GAP);

    if let Some(error) = usage_error {
        warning_line(
            ui,
            &trf("Capacity refresh failed: {error}", &[("error", &error)]),
        );
    }
    for warning in &data.warnings {
        warning_line(ui, warning);
    }
    for pool in pool_warnings {
        let name: &dyn std::fmt::Display = &pool.name;
        let text = match pool_health(pool, crypt_remotes) {
            PoolHealth::Empty => trf("Pool {name} has no storage remotes.", &[("name", name)]),
            PoolHealth::MissingRemote => trf(
                "Pool {name} references an unavailable crypt remote.",
                &[("name", name)],
            ),
            PoolHealth::Ready | PoolHealth::Checking => continue,
        };
        warning_line(ui, &text);
    }
    for report in provider_errors {
        let detail = report.error.as_deref().unwrap_or(tr("unknown error"));
        warning_line(ui, &format!("{}: {detail}", report.remote));
    }

    ui.add_space(theme::SECTION_GAP);
    ui.separator();
}

/// One warning line in the theme's warning color.
fn warning_line(ui: &mut egui::Ui, text: &str) {
    let (_, foreground) = theme::warning_colors(ui.visuals().dark_mode);
    ui.colored_label(foreground, text);
}
