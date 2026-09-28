use eframe::egui;

pub(crate) const NAVIGATION_WIDTH: f32 = 190.0;
pub(crate) const TASK_CONSOLE_HEIGHT: f32 = 180.0;
pub(crate) const CONTROL_HEIGHT: f32 = 32.0;
pub(crate) const ROW_HEIGHT: f32 = 34.0;
pub(crate) const SECTION_GAP: f32 = 18.0;
pub(crate) const SUBSECTION_GAP: f32 = 8.0;
pub(crate) const CONTENT_MARGIN: i8 = 16;
pub(crate) const CORNER_RADIUS: u8 = 8;
pub(crate) const CAPACITY_BAR_HEIGHT: f32 = 18.0;
pub(crate) const SECTION_TITLE_SIZE: f32 = 25.0;
pub(crate) const STATUS_TEXT_SIZE: f32 = 11.0;

pub(crate) const PROGRESS_BAR_HEIGHT: f32 = 22.0;

pub(crate) fn apply(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
        style.spacing.interact_size.y = CONTROL_HEIGHT;
        style.spacing.indent = 16.0;
        style.spacing.window_margin = egui::Margin::same(CONTENT_MARGIN);

        let dark = style.visuals.dark_mode;
        style.visuals.panel_fill = if dark {
            egui::Color32::from_rgb(22, 28, 39)
        } else {
            egui::Color32::from_rgb(247, 249, 252)
        };
        style.visuals.extreme_bg_color = if dark {
            egui::Color32::from_rgb(15, 20, 29)
        } else {
            egui::Color32::from_rgb(237, 241, 247)
        };
        style.visuals.selection.bg_fill = if dark {
            egui::Color32::from_rgb(36, 79, 114)
        } else {
            egui::Color32::from_rgb(202, 225, 245)
        };
        style.visuals.selection.stroke = egui::Stroke::new(
            1.0,
            if dark {
                egui::Color32::from_rgb(204, 232, 255)
            } else {
                egui::Color32::from_rgb(23, 63, 99)
            },
        );
        let radius = egui::CornerRadius::same(CORNER_RADIUS);
        style.visuals.widgets.noninteractive.corner_radius = radius;
        style.visuals.widgets.inactive.corner_radius = radius;
        style.visuals.widgets.hovered.corner_radius = radius;
        style.visuals.widgets.active.corner_radius = radius;
        style.visuals.widgets.open.corner_radius = radius;
        style.visuals.window_corner_radius = radius;
        style.visuals.menu_corner_radius = radius;

        let border = style.visuals.widgets.noninteractive.bg_stroke;
        style.visuals.widgets.inactive.bg_stroke = border;
        style.visuals.widgets.active.bg_stroke = border;
        style.visuals.widgets.open.bg_stroke = border;
    });
}

pub(crate) fn success_colors(dark: bool) -> (egui::Color32, egui::Color32) {
    if dark {
        (
            egui::Color32::from_rgb(42, 61, 45),
            egui::Color32::from_rgb(151, 201, 158),
        )
    } else {
        (
            egui::Color32::from_rgb(229, 240, 231),
            egui::Color32::from_rgb(42, 104, 54),
        )
    }
}

pub(crate) fn warning_colors(dark: bool) -> (egui::Color32, egui::Color32) {
    if dark {
        (
            egui::Color32::from_rgb(65, 55, 36),
            egui::Color32::from_rgb(218, 181, 105),
        )
    } else {
        (
            egui::Color32::from_rgb(246, 238, 219),
            egui::Color32::from_rgb(132, 91, 20),
        )
    }
}

pub(crate) fn error_colors(dark: bool) -> (egui::Color32, egui::Color32) {
    if dark {
        (
            egui::Color32::from_rgb(66, 42, 42),
            egui::Color32::from_rgb(224, 153, 153),
        )
    } else {
        (
            egui::Color32::from_rgb(246, 228, 228),
            egui::Color32::from_rgb(145, 48, 48),
        )
    }
}

pub(crate) fn neutral_colors(dark: bool) -> (egui::Color32, egui::Color32) {
    if dark {
        (
            egui::Color32::from_rgb(47, 47, 47),
            egui::Color32::from_rgb(190, 190, 190),
        )
    } else {
        (
            egui::Color32::from_rgb(238, 238, 238),
            egui::Color32::from_rgb(83, 83, 83),
        )
    }
}

pub(crate) fn card(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::new()
        .fill(if ui.visuals().dark_mode {
            egui::Color32::from_rgb(29, 37, 50)
        } else {
            egui::Color32::WHITE
        })
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(CORNER_RADIUS)
        .inner_margin(14)
}
