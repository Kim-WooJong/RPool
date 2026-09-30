//! Visual tokens and small layout helpers shared by every screen: palette,
//! spacing, cards, page headers, tabs, primary/danger buttons and the
//! responsive breakpoints.
use eframe::egui;
use egui::Color32;

pub(crate) const NAVIGATION_WIDTH: f32 = 196.0;
pub(crate) const NAVIGATION_COMPACT_WIDTH: f32 = 60.0;
pub(crate) const TASK_CONSOLE_HEIGHT: f32 = 180.0;
pub(crate) const TASK_CONSOLE_MINI_HEIGHT: f32 = 40.0;
pub(crate) const CONTROL_HEIGHT: f32 = 30.0;
pub(crate) const ROW_HEIGHT: f32 = 32.0;
pub(crate) const SECTION_GAP: f32 = 16.0;
pub(crate) const SUBSECTION_GAP: f32 = 8.0;
pub(crate) const CONTENT_MARGIN: i8 = 16;
pub(crate) const CORNER_RADIUS: u8 = 8;
pub(crate) const CARD_RADIUS: u8 = 12;
pub(crate) const CAPACITY_BAR_HEIGHT: f32 = 18.0;
pub(crate) const SECTION_TITLE_SIZE: f32 = 22.0;
pub(crate) const CARD_TITLE_SIZE: f32 = 16.0;
pub(crate) const STATUS_TEXT_SIZE: f32 = 11.0;
pub(crate) const PROGRESS_BAR_HEIGHT: f32 = 22.0;

/// Window narrower than this: the navigation shows icons only.
pub(crate) const NAV_COMPACT_BELOW: f32 = 960.0;
/// Content at least this wide: cards sit in two columns.
pub(crate) const TWO_COLUMN_MIN: f32 = 1060.0;
/// Window lower than this: the operation console starts collapsed.
pub(crate) const CONSOLE_MINI_BELOW: f32 = 860.0;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Palette {
    pub bg: Color32,
    pub nav: Color32,
    pub surface: Color32,
    pub surface_alt: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub accent_fg: Color32,
    pub danger: Color32,
    pub shadow: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub(crate) fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            bg: rgb(17, 21, 30),
            nav: rgb(13, 17, 25),
            surface: rgb(25, 31, 43),
            surface_alt: rgb(32, 40, 55),
            border: rgb(45, 55, 73),
            text: rgb(226, 231, 240),
            muted: rgb(142, 153, 172),
            accent: rgb(76, 141, 246),
            accent_soft: rgb(33, 55, 92),
            accent_fg: Color32::WHITE,
            danger: rgb(214, 84, 84),
            shadow: Color32::from_black_alpha(80),
        }
    } else {
        Palette {
            bg: rgb(243, 245, 250),
            nav: rgb(233, 237, 245),
            surface: Color32::WHITE,
            surface_alt: rgb(238, 242, 248),
            border: rgb(218, 224, 234),
            text: rgb(28, 34, 46),
            muted: rgb(96, 106, 124),
            accent: rgb(37, 99, 235),
            accent_soft: rgb(219, 232, 254),
            accent_fg: Color32::WHITE,
            danger: rgb(190, 45, 45),
            shadow: Color32::from_black_alpha(20),
        }
    }
}

pub(crate) fn pal(ui: &egui::Ui) -> Palette {
    palette(ui.visuals().dark_mode)
}

pub(crate) fn apply(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        let dark = style.visuals.dark_mode;
        let p = palette(dark);
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
        style
            .text_styles
            .insert(egui::TextStyle::Heading, egui::FontId::proportional(20.0));
        style.spacing.button_padding = egui::vec2(12.0, 5.0);
        style.spacing.interact_size.y = CONTROL_HEIGHT;
        style.spacing.indent = 16.0;
        style.spacing.window_margin = egui::Margin::same(CONTENT_MARGIN);
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating = true;

        let v = &mut style.visuals;
        v.panel_fill = p.bg;
        v.window_fill = p.surface;
        v.extreme_bg_color = p.surface_alt;
        v.faint_bg_color = p.surface_alt;
        v.override_text_color = None;
        v.hyperlink_color = p.accent;
        v.selection.bg_fill = p.accent_soft;
        v.selection.stroke = egui::Stroke::new(1.0, p.accent);
        v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, p.border);
        v.widgets.noninteractive.fg_stroke.color = p.text;
        v.widgets.inactive.weak_bg_fill = p.surface_alt;
        v.widgets.inactive.bg_fill = p.surface_alt;
        v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, p.border);
        v.widgets.hovered.weak_bg_fill = p.accent_soft;
        v.widgets.hovered.bg_fill = p.accent_soft;
        v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, p.accent);
        v.widgets.active.bg_stroke = egui::Stroke::new(1.0, p.accent);
        v.widgets.open.bg_stroke = egui::Stroke::new(1.0, p.border);
        let radius = egui::CornerRadius::same(CORNER_RADIUS);
        v.widgets.noninteractive.corner_radius = radius;
        v.widgets.inactive.corner_radius = radius;
        v.widgets.hovered.corner_radius = radius;
        v.widgets.active.corner_radius = radius;
        v.widgets.open.corner_radius = radius;
        v.window_corner_radius = egui::CornerRadius::same(CARD_RADIUS);
        v.menu_corner_radius = radius;
        v.window_shadow = egui::epaint::Shadow {
            offset: [0, 4],
            blur: 16,
            spread: 0,
            color: p.shadow,
        };
        v.popup_shadow = v.window_shadow;
    });
}

pub(crate) fn success_colors(dark: bool) -> (Color32, Color32) {
    if dark {
        (rgb(30, 60, 42), rgb(134, 214, 156))
    } else {
        (rgb(222, 244, 229), rgb(28, 110, 56))
    }
}
pub(crate) fn warning_colors(dark: bool) -> (Color32, Color32) {
    if dark {
        (rgb(66, 52, 26), rgb(236, 190, 100))
    } else {
        (rgb(252, 240, 214), rgb(140, 90, 10))
    }
}
pub(crate) fn error_colors(dark: bool) -> (Color32, Color32) {
    if dark {
        (rgb(72, 34, 38), rgb(240, 150, 150))
    } else {
        (rgb(252, 228, 228), rgb(160, 40, 40))
    }
}
pub(crate) fn neutral_colors(dark: bool) -> (Color32, Color32) {
    if dark {
        (rgb(40, 47, 62), rgb(186, 194, 210))
    } else {
        (rgb(232, 236, 243), rgb(78, 88, 106))
    }
}
pub(crate) fn info_colors(dark: bool) -> (Color32, Color32) {
    let p = palette(dark);
    (
        p.accent_soft,
        if dark { rgb(160, 196, 255) } else { p.accent },
    )
}

/// The card frame. Prefer [`card_section`], which also draws the header.
pub(crate) fn card(ui: &egui::Ui) -> egui::Frame {
    let p = pal(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(CARD_RADIUS)
        .inner_margin(egui::Margin::same(16))
        .shadow(egui::epaint::Shadow {
            offset: [0, 1],
            blur: 6,
            spread: 0,
            color: p.shadow,
        })
}

/// A titled card filling the available width. `actions` sit at the right of
/// the header and wrap below the title when space is short.
pub(crate) fn card_section<R>(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: Option<&str>,
    actions: impl FnOnce(&mut egui::Ui),
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let inner = card(ui)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(title)
                        .size(CARD_TITLE_SIZE)
                        .strong()
                        .color(pal(ui).text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), actions);
            });
            if let Some(subtitle) = subtitle {
                ui.label(egui::RichText::new(subtitle).small().color(pal(ui).muted));
            }
            ui.add_space(SUBSECTION_GAP);
            body(ui)
        })
        .inner;
    ui.add_space(SUBSECTION_GAP + 4.0);
    inner
}

/// Page title with a muted description.
pub(crate) fn page_header(ui: &mut egui::Ui, title: &str, description: Option<&str>) {
    ui.label(
        egui::RichText::new(title)
            .size(SECTION_TITLE_SIZE)
            .strong()
            .color(pal(ui).text),
    );
    if let Some(description) = description {
        ui.label(egui::RichText::new(description).color(pal(ui).muted));
    }
    ui.add_space(SUBSECTION_GAP);
}

/// Segmented sub-page tabs that wrap on narrow windows.
pub(crate) fn tabs<T: PartialEq + Copy>(ui: &mut egui::Ui, current: &mut T, items: &[(T, &str)]) {
    let p = pal(ui);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for &(value, label) in items {
            let selected = *current == value;
            let text =
                egui::RichText::new(label).color(if selected { p.accent_fg } else { p.text });
            let button = egui::Button::new(text)
                .fill(if selected {
                    p.accent
                } else {
                    Color32::TRANSPARENT
                })
                .stroke(egui::Stroke::new(
                    1.0,
                    if selected { p.accent } else { p.border },
                ))
                .corner_radius(egui::CornerRadius::same(255))
                .min_size(egui::vec2(0.0, 28.0));
            if ui.add(button).clicked() {
                *current = value;
            }
        }
    });
    ui.add_space(SUBSECTION_GAP);
}

/// The one main action of a card.
pub(crate) fn primary_button(ui: &mut egui::Ui, enabled: bool, label: &str) -> egui::Response {
    let p = pal(ui);
    ui.add_enabled(
        enabled,
        egui::Button::new(egui::RichText::new(label).strong().color(p.accent_fg))
            .fill(p.accent)
            .stroke(egui::Stroke::NONE)
            .min_size(egui::vec2(88.0, CONTROL_HEIGHT)),
    )
}

/// Destructive action.
pub(crate) fn danger_button(ui: &mut egui::Ui, enabled: bool, label: &str) -> egui::Response {
    let p = pal(ui);
    ui.add_enabled(
        enabled,
        egui::Button::new(egui::RichText::new(label).strong().color(Color32::WHITE))
            .fill(p.danger)
            .stroke(egui::Stroke::NONE)
            .min_size(egui::vec2(0.0, CONTROL_HEIGHT)),
    )
}

/// Muted helper text.
pub(crate) fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).small().color(pal(ui).muted));
}

/// Two cards side by side when there is room, stacked otherwise. The state
/// is passed through so both closures may borrow it mutably.
pub(crate) fn two_up<S>(
    ui: &mut egui::Ui,
    state: &mut S,
    left: impl FnOnce(&mut egui::Ui, &mut S),
    right: impl FnOnce(&mut egui::Ui, &mut S),
) {
    if ui.available_width() >= TWO_COLUMN_MIN {
        ui.columns(2, |columns| {
            // `columns` justifies its children; keep buttons at natural width.
            let [a, b] = columns else { unreachable!() };
            a.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                left(ui, state)
            });
            b.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                right(ui, state)
            });
        });
    } else {
        left(ui, state);
        right(ui, state);
    }
}

/// Height for a bounded list inside a card: a share of the page body,
/// clamped so lists stay usable on small and large windows.
pub(crate) fn list_height(body_height: f32) -> f32 {
    (body_height * 0.4).clamp(120.0, 380.0)
}

/// Page body: one vertical scroll with the page id, never horizontal. Lists
/// inside use their own bounded scroll areas.
pub(crate) fn page_body<R>(
    ui: &mut egui::Ui,
    id: &str,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::ScrollArea::vertical()
        .id_salt(("page", id))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_max_width(ui.available_width());
            body(ui)
        })
        .inner
}

/// Height for side-by-side scroll panes: most of the window, bounded so
/// the page's own header and actions stay visible.
pub(crate) fn pane_height(ui: &egui::Ui) -> f32 {
    (ui.ctx().content_rect().height() - 320.0).clamp(240.0, 760.0)
}

/// Two independently scrolling panes: side by side when wide, stacked when
/// narrow. `pane(ui, 0)` draws the left/top pane, `pane(ui, 1)` the other.
pub(crate) fn split_panes(
    ui: &mut egui::Ui,
    id: &str,
    height: f32,
    mut pane: impl FnMut(&mut egui::Ui, usize),
) {
    let mut scroll = |ui: &mut egui::Ui, side: usize, height: f32, fill: bool| {
        card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .id_salt((id, side))
                .max_height(height)
                .auto_shrink([false, !fill])
                .show(ui, |ui| pane(ui, side));
        });
    };
    if ui.available_width() >= TWO_COLUMN_MIN {
        ui.columns(2, |columns| {
            for (side, column) in columns.iter_mut().enumerate() {
                column.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    scroll(ui, side, height, true)
                });
            }
        });
    } else {
        scroll(ui, 0, height * 0.7, false);
        ui.add_space(SUBSECTION_GAP);
        scroll(ui, 1, height * 0.7, false);
    }
    ui.add_space(SUBSECTION_GAP);
}

/// A card of exactly `size` with its own vertical scroll: growing content
/// never pushes a neighbouring pane away. Wide rows are clipped, so content
/// should wrap; tables that must stay wide scroll horizontally themselves.
pub(crate) fn fixed_pane(
    ui: &mut egui::Ui,
    id: &str,
    size: egui::Vec2,
    content: impl FnOnce(&mut egui::Ui),
) {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(CARD_RADIUS),
        p.surface,
        egui::Stroke::new(1.0, p.border),
        egui::StrokeKind::Inside,
    );
    let inner = rect.shrink(12.0);
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id)
            .max_rect(inner)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(inner.intersect(ui.clip_rect()));
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .max_height(inner.height())
        .show(&mut child, content);
}

/// Whether panes should sit side by side at this content width.
pub(crate) fn wide(ui: &egui::Ui) -> bool {
    ui.available_width() >= 900.0
}
