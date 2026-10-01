//! Left navigation: full labels on wide windows, icons with hover text on
//! narrow ones. The selected page gets an accent bar.
use crate::gui::i18n::tr;
use crate::gui::state::Page;
use crate::gui::theme;
use eframe::egui;

const ITEMS: &[(Page, &str, &str)] = &[
    (Page::Dashboard, "🏠", "Overview"),
    (Page::Drive, "💾", "Drive"),
    (Page::Files, "📁", "Files"),
    (Page::Storage, "☁", "Storage"),
    (Page::Monitoring, "📈", "Monitoring"),
    (Page::Maintenance, "✔", "Health"),
    (Page::Jobs, "📋", "Activity"),
    (Page::Settings, "⚙", "Settings"),
];

pub(crate) fn show(ui: &mut egui::Ui, page: &mut Page, compact: bool) {
    let p = theme::pal(ui);
    ui.add_space(14.0);
    if compact {
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("R").size(22.0).strong().color(p.accent));
        });
    } else {
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("RPool")
                    .size(22.0)
                    .strong()
                    .color(p.text),
            );
        });
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            theme::hint(ui, tr("Sharded cloud storage"));
        });
    }
    ui.add_space(18.0);
    for (index, &(target, icon, label)) in ITEMS.iter().enumerate() {
        if index == 6 {
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(4.0);
        }
        // The labels are the page titles, translated in `app::page_title`.
        nav_button(ui, page, target, icon, tr(label), compact);
    }
}

fn nav_button(
    ui: &mut egui::Ui,
    page: &mut Page,
    target: Page,
    icon: &str,
    label: &str,
    compact: bool,
) {
    let p = theme::pal(ui);
    let selected = *page == target;
    let text = if compact {
        egui::RichText::new(icon).size(18.0)
    } else {
        egui::RichText::new(format!("{icon}   {label}")).size(14.5)
    };
    let text = text.color(if selected { p.text } else { p.muted });
    let button = egui::Button::new(text)
        .fill(if selected {
            p.accent_soft
        } else {
            egui::Color32::TRANSPARENT
        })
        .stroke(egui::Stroke::NONE)
        .corner_radius(egui::CornerRadius::same(theme::CORNER_RADIUS));
    let width = ui.available_width();
    let response = ui.add_sized([width, 38.0], button);
    let response = if compact {
        response.on_hover_text(label)
    } else {
        response
    };
    if selected {
        let rect = response.rect;
        ui.painter().rect_filled(
            egui::Rect::from_min_size(
                rect.left_top() + egui::vec2(0.0, 8.0),
                egui::vec2(3.0, rect.height() - 16.0),
            ),
            egui::CornerRadius::same(2),
            p.accent,
        );
    }
    if response.clicked() {
        *page = target;
    }
}

#[cfg(test)]
mod tests {
    use super::ITEMS;
    use crate::gui::i18n::{tr_in, Language};

    /// Checked with `tr_in` so the process-wide language (shared with tests
    /// that render in English) is never changed.
    #[test]
    fn navigation_labels_have_korean_japanese_and_chinese_text() {
        for &(_, _, label) in ITEMS {
            for language in [Language::Korean, Language::Japanese, Language::Chinese] {
                let text = tr_in(language, label);
                assert_ne!(text, label, "{label} is untranslated for {language:?}");
            }
        }
        assert_eq!(tr_in(Language::Korean, "Overview"), "개요");
        assert_eq!(tr_in(Language::English, "Overview"), "Overview");
    }
}
