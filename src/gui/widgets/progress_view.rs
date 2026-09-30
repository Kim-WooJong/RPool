use crate::gui::i18n::trf;
use crate::gui::task::TaskProgress;
use crate::gui::theme;
use crate::presentation::format_bytes;
use eframe::egui;

pub(crate) fn progress_view(ui: &mut egui::Ui, progress: &TaskProgress) {
    let Some(ratio) = progress.fraction().map(|value| value.clamp(0.0, 1.0)) else {
        return;
    };

    let percent = ratio * 100.0;
    let width = ui.available_width().min(720.0);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(width, theme::PROGRESS_BAR_HEIGHT),
        egui::Sense::hover(),
    );
    let radius = egui::CornerRadius::same(theme::CORNER_RADIUS);
    let painter = ui.painter();
    painter.rect_filled(rect, radius, ui.visuals().widgets.inactive.bg_fill);

    if ratio > 0.0 {
        let filled = egui::Rect::from_min_max(
            rect.min,
            egui::pos2(rect.left() + rect.width() * ratio, rect.bottom()),
        );
        painter.rect_filled(filled, radius, ui.visuals().selection.bg_fill);
    }

    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        format!("{percent:.0}%"),
        egui::FontId::proportional(12.0),
        ui.visuals().text_color(),
    );

    if let (Some(current), Some(total)) = (progress.current_item, progress.total_items) {
        ui.small(trf(
            "Items {current} / {total}",
            &[("current", &current), ("total", &total)],
        ));
        return;
    }

    let Some(total_bytes) = progress.total_bytes else {
        return;
    };
    let completed = progress.completed_bytes.unwrap_or(0).min(total_bytes);
    ui.horizontal_wrapped(|ui| {
        ui.small(trf(
            "Progress {done} / {total}",
            &[
                ("done", &format_bytes(completed)),
                ("total", &format_bytes(total_bytes)),
            ],
        ));

        if let Some(transferred) = progress.transferred_bytes {
            if transferred != completed {
                ui.separator();
                ui.small(trf(
                    "Network {bytes}",
                    &[("bytes", &format_bytes(transferred))],
                ));
            }
        }

        if let Some(rate) = progress.bytes_per_second {
            ui.separator();
            ui.small(format!("{}/s", format_bytes(rate.max(0.0) as u64)));
        }

        if let Some(eta) = progress.eta_seconds {
            ui.separator();
            ui.small(trf(
                "ETA {minutes}m {seconds}s",
                &[
                    ("minutes", &(eta / 60)),
                    ("seconds", &format!("{:02}", eta % 60)),
                ],
            ));
        }
    });
}
