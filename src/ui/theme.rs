use eframe::egui::{self, Color32, CornerRadius, FontId, Stroke, TextStyle, Theme};

pub const ACCENT: Color32 = Color32::from_rgb(0x8b, 0x6c, 0xff);
pub const BG: Color32 = Color32::from_rgb(0x12, 0x12, 0x15);
pub const SIDEBAR: Color32 = Color32::from_rgb(0x0b, 0x0b, 0x0d);
pub const SURFACE: Color32 = Color32::from_rgb(0x1c, 0x1c, 0x21);
pub const TEXT: Color32 = Color32::from_rgb(0xe8, 0xe8, 0xec);
pub const MUTED: Color32 = Color32::from_rgb(0x9a, 0x9a, 0xa6);
pub const ERROR: Color32 = Color32::from_rgb(0xff, 0x6b, 0x6b);
pub const HOVER: Color32 = Color32::from_rgb(0x22, 0x22, 0x28);
pub const TAG: Color32 = Color32::from_rgb(0xc9, 0xa2, 0x5a);

pub fn apply(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = SURFACE;
    v.extreme_bg_color = Color32::from_rgb(0x0e, 0x0e, 0x11);
    v.faint_bg_color = SURFACE;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT.linear_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.window_corner_radius = CornerRadius::same(8);
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(6);
    }
    v.widgets.inactive.weak_bg_fill = SURFACE;
    v.widgets.inactive.bg_fill = SURFACE;
    ctx.set_visuals_of(Theme::Dark, v);
    ctx.set_theme(Theme::Dark);

    ctx.all_styles_mut(|style| {
        style.text_styles.insert(TextStyle::Body, FontId::proportional(14.5));
        style.text_styles.insert(TextStyle::Button, FontId::proportional(14.5));
        style.text_styles.insert(TextStyle::Heading, FontId::proportional(22.0));
        style.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
    });
}
