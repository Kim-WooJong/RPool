use eframe::egui;

pub(crate) const NAVIGATION_WIDTH: f32 = 165.0;
pub(crate) const TASK_CONSOLE_HEIGHT: f32 = 220.0;
pub(crate) const CONTROL_HEIGHT: f32 = 26.0;
pub(crate) const ROW_HEIGHT: f32 = 28.0;
pub(crate) const SECTION_GAP: f32 = 12.0;
pub(crate) const SUBSECTION_GAP: f32 = 8.0;
pub(crate) const CONTENT_MARGIN: i8 = 8;
pub(crate) const CORNER_RADIUS: u8 = 2;
pub(crate) const CAPACITY_BAR_HEIGHT: f32 = 18.0;
pub(crate) const SECTION_TITLE_SIZE: f32 = 18.0;
pub(crate) const STATUS_TEXT_SIZE: f32 = 11.0;

pub(crate) fn apply(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
        style.spacing.interact_size.y = CONTROL_HEIGHT;
        style.spacing.indent = 16.0;
        style.spacing.window_margin = egui::Margin::same(CONTENT_MARGIN);

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
