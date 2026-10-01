//! The results list of a speed test: one block per remote, bottlenecks
//! highlighted, the pool estimate and folders left behind.
use super::view::{Bar, RemoteRow, ReportView};
use crate::gui::i18n::{tr, trf};
use crate::gui::theme;
use crate::gui::widgets::{capacity_bar_sized, status_badge, StatusTone};
use eframe::egui;

/// Width of the "Upload" / "Download" labels in front of the bars.
const LABEL_WIDTH: f32 = 96.0;
const MAX_BAR_WIDTH: f32 = 420.0;

pub(crate) fn show(ui: &mut egui::Ui, view: &ReportView) {
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.strong(tr("Last result"));
        status_badge(
            ui,
            &trf("{mode} per account", &[("mode", &view.mode)]),
            StatusTone::Neutral,
        );
    });
    if view.newer_version {
        theme::hint(
            ui,
            tr("This result comes from a newer RPool; some details may be missing."),
        );
    }
    estimate(ui, view);
    for row in &view.rows {
        remote_row(ui, row);
    }
    leftovers(ui, &view.leftovers);
}

fn tinted(ui: &mut egui::Ui, colors: (egui::Color32, egui::Color32), text: &str) {
    egui::Frame::new()
        .fill(colors.0)
        .corner_radius(theme::CORNER_RADIUS)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.colored_label(colors.1, text);
        });
}

fn estimate(ui: &mut egui::Ui, view: &ReportView) {
    let Some(estimate) = &view.estimate else {
        return;
    };
    let speeds = [
        ("upload", &estimate.upload as &dyn std::fmt::Display),
        ("download", &estimate.download),
    ];
    let text = match (&view.bottleneck_upload, &view.bottleneck_download) {
        (Some(up), Some(down)) if up == down => trf(
            "Expected pool speed: upload {upload}, download {download} — limited by {remote}",
            &[speeds[0], speeds[1], ("remote", up)],
        ),
        (Some(up), Some(down)) => trf(
            "Expected pool speed: upload {upload} (limited by {up}), download {download} (limited by {down})",
            &[speeds[0], speeds[1], ("up", up), ("down", down)],
        ),
        _ => trf(
            "Expected pool speed: upload {upload}, download {download}",
            &speeds,
        ),
    };
    tinted(ui, theme::info_colors(ui.visuals().dark_mode), &text);
}

/// From this width upload and download bars share one line.
const SIDE_BY_SIDE_WIDTH: f32 = 760.0;
const BAR_GAP: f32 = 16.0;

/// The label, then a bar of `width(remaining width after the label)`.
fn speed_bar(ui: &mut egui::Ui, label: &str, bar: Option<&Bar>, width: impl Fn(f32) -> f32) {
    ui.add_sized(
        [LABEL_WIDTH, theme::CAPACITY_BAR_HEIGHT],
        egui::Label::new(egui::RichText::new(label).small()).truncate(),
    );
    let width = width(ui.available_width()).clamp(60.0, MAX_BAR_WIDTH);
    match bar {
        Some(bar) => capacity_bar_sized(ui, Some(bar.ratio), &bar.text, width),
        None => {
            ui.label("—");
        }
    }
}

fn speed_bars(ui: &mut egui::Ui, row: &RemoteRow) {
    if ui.available_width() >= SIDE_BY_SIDE_WIDTH {
        ui.horizontal(|ui| {
            // Half of what is left after both labels and the gap.
            let gap = ui.spacing().item_spacing.x;
            let half = move |rest: f32| (rest - BAR_GAP - LABEL_WIDTH - 3.0 * gap) / 2.0;
            speed_bar(ui, tr("Upload"), row.upload.as_ref(), half);
            ui.add_space(BAR_GAP);
            let rest = |rest: f32| rest;
            speed_bar(ui, tr("Download"), row.download.as_ref(), rest);
        });
    } else {
        let all = |rest: f32| rest;
        ui.horizontal(|ui| speed_bar(ui, tr("Upload"), row.upload.as_ref(), all));
        ui.horizontal(|ui| speed_bar(ui, tr("Download"), row.download.as_ref(), all));
    }
}

fn remote_row(ui: &mut egui::Ui, row: &RemoteRow) {
    let p = theme::pal(ui);
    let dark = ui.visuals().dark_mode;
    let stroke = if row.is_bottleneck() {
        egui::Stroke::new(2.0, theme::warning_colors(dark).1)
    } else {
        egui::Stroke::new(1.0, p.border)
    };
    egui::Frame::new()
        .fill(p.surface_alt)
        .stroke(stroke)
        .corner_radius(theme::CORNER_RADIUS)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            // Name, badges and timings on one wrapping line; a name too long
            // for it (no spaces to wrap at) gets a line of its own.
            let name = egui::RichText::new(&row.remote).monospace().strong();
            let name_width = egui::WidgetText::from(name.clone())
                .into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::TextStyle::Body,
                )
                .size()
                .x;
            let inline = name_width < ui.available_width() / 2.0;
            if !inline {
                ui.add(egui::Label::new(name.clone()).wrap());
            }
            ui.horizontal_wrapped(|ui| {
                if inline {
                    ui.label(name);
                }
                if let Some(backend) = &row.backend {
                    status_badge(ui, backend, StatusTone::Neutral);
                }
                if row.ok {
                    status_badge(ui, tr("OK"), StatusTone::Success);
                } else {
                    status_badge(ui, tr("Failed"), StatusTone::Error);
                }
                if row.slowest_upload {
                    status_badge(ui, tr("Slowest for uploads"), StatusTone::Warning);
                }
                if row.slowest_download {
                    status_badge(ui, tr("Slowest for downloads"), StatusTone::Warning);
                }
                if row.slow_start {
                    status_badge(ui, tr("Slow start"), StatusTone::Warning);
                }
                if let Some(first) = &row.first_op {
                    ui.small(trf("First operation: {time}", &[("time", first)]))
                        .on_hover_text(tr(
                            "The first request after start; includes the provider's cold start.",
                        ));
                }
                if let Some(latency) = &row.latency {
                    ui.small(trf("Latency: {time}", &[("time", latency)]));
                }
            });
            if row.upload.is_some() || row.download.is_some() || row.ok {
                speed_bars(ui, row);
            }
            if let Some(error) = &row.error {
                ui.colored_label(theme::error_colors(dark).1, error);
            }
        });
    ui.add_space(4.0);
}

fn leftovers(ui: &mut egui::Ui, leftovers: &[String]) {
    if leftovers.is_empty() {
        return;
    }
    let (fill, fg) = theme::warning_colors(ui.visuals().dark_mode);
    egui::Frame::new()
        .fill(fill)
        .corner_radius(theme::CORNER_RADIUS)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.colored_label(
                fg,
                tr("These test folders could not be deleted. You can delete them yourself; they only hold test data."),
            );
            for folder in leftovers {
                ui.add(egui::Label::new(egui::RichText::new(folder).monospace().color(fg)).wrap());
            }
        });
}
