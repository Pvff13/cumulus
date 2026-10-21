//! Colours. The theme (dark / grey / light) and accent colour can be changed
//! while running, so colours are read through functions, not constants.

use std::sync::RwLock;

use eframe::egui::{self, Color32, CornerRadius, FontId, Stroke, TextStyle, Theme};

/// Background shades.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Base {
    Dark,
    Grey,
    Light,
}

impl Base {
    pub const ALL: [Base; 3] = [Base::Dark, Base::Grey, Base::Light];

    pub fn label(self) -> &'static str {
        match self {
            Base::Dark => "Dark",
            Base::Grey => "Grey",
            Base::Light => "Light",
        }
    }

    /// Name stored in config.json.
    pub fn key(self) -> &'static str {
        match self {
            Base::Dark => "dark",
            Base::Grey => "grey",
            Base::Light => "light",
        }
    }

    pub fn from_key(key: &str) -> Base {
        Base::ALL.into_iter().find(|b| b.key() == key).unwrap_or(Base::Dark)
    }
}

/// Accent colours to pick from: (config name, label, colour).
pub const ACCENTS: [(&str, &str, Color32); 12] = [
    ("purple", "Purple", Color32::from_rgb(0x8b, 0x6c, 0xff)),
    ("blue", "Blue", Color32::from_rgb(0x4c, 0x8d, 0xff)),
    ("sky", "Sky", Color32::from_rgb(0x38, 0xbd, 0xf8)),
    ("teal", "Teal", Color32::from_rgb(0x2d, 0xd4, 0xbf)),
    ("green", "Green", Color32::from_rgb(0x4a, 0xde, 0x80)),
    ("lime", "Lime", Color32::from_rgb(0xa3, 0xe6, 0x35)),
    ("yellow", "Yellow", Color32::from_rgb(0xfa, 0xcc, 0x15)),
    ("orange", "Orange", Color32::from_rgb(0xff, 0x77, 0x22)),
    ("red", "Red", Color32::from_rgb(0xf4, 0x3f, 0x5e)),
    ("pink", "Pink", Color32::from_rgb(0xf4, 0x72, 0xb6)),
    ("magenta", "Magenta", Color32::from_rgb(0xd9, 0x46, 0xef)),
    ("silver", "Silver", Color32::from_rgb(0xb4, 0xb8, 0xc4)),
];

#[derive(Clone, Copy)]
struct Palette {
    base: Base,
    accent_key: &'static str,
    accent: Color32,
    bg: Color32,
    sidebar: Color32,
    surface: Color32,
    extreme: Color32,
    text: Color32,
    muted: Color32,
    error: Color32,
    hover: Color32,
    tag: Color32,
}

const DARK: Palette = Palette {
    base: Base::Dark,
    accent_key: "purple",
    accent: ACCENTS[0].2,
    bg: Color32::from_rgb(0x12, 0x12, 0x15),
    sidebar: Color32::from_rgb(0x0b, 0x0b, 0x0d),
    surface: Color32::from_rgb(0x1c, 0x1c, 0x21),
    extreme: Color32::from_rgb(0x0e, 0x0e, 0x11),
    text: Color32::from_rgb(0xe8, 0xe8, 0xec),
    muted: Color32::from_rgb(0x9a, 0x9a, 0xa6),
    error: Color32::from_rgb(0xff, 0x6b, 0x6b),
    hover: Color32::from_rgb(0x22, 0x22, 0x28),
    tag: Color32::from_rgb(0xc9, 0xa2, 0x5a),
};

const GREY: Palette = Palette {
    base: Base::Grey,
    bg: Color32::from_rgb(0x2b, 0x2c, 0x31),
    sidebar: Color32::from_rgb(0x22, 0x23, 0x27),
    surface: Color32::from_rgb(0x38, 0x39, 0x3f),
    extreme: Color32::from_rgb(0x1e, 0x1f, 0x22),
    text: Color32::from_rgb(0xee, 0xee, 0xf2),
    muted: Color32::from_rgb(0xa8, 0xa9, 0xb3),
    error: Color32::from_rgb(0xff, 0x7b, 0x7b),
    hover: Color32::from_rgb(0x3e, 0x3f, 0x46),
    tag: Color32::from_rgb(0xd8, 0xb4, 0x6c),
    ..DARK
};

const LIGHT: Palette = Palette {
    base: Base::Light,
    bg: Color32::from_rgb(0xf7, 0xf7, 0xf9),
    sidebar: Color32::from_rgb(0xea, 0xea, 0xef),
    surface: Color32::from_rgb(0xe2, 0xe2, 0xe8),
    extreme: Color32::from_rgb(0xff, 0xff, 0xff),
    text: Color32::from_rgb(0x1b, 0x1b, 0x21),
    muted: Color32::from_rgb(0x66, 0x66, 0x73),
    error: Color32::from_rgb(0xc8, 0x2e, 0x2e),
    hover: Color32::from_rgb(0xe6, 0xe6, 0xec),
    tag: Color32::from_rgb(0x95, 0x64, 0x0c),
    ..DARK
};

static PALETTE: RwLock<Palette> = RwLock::new(DARK);

fn get() -> Palette {
    *PALETTE.read().unwrap_or_else(|e| e.into_inner())
}

pub fn accent() -> Color32 {
    get().accent
}
pub fn bg() -> Color32 {
    get().bg
}
pub fn sidebar() -> Color32 {
    get().sidebar
}
pub fn surface() -> Color32 {
    get().surface
}
pub fn text() -> Color32 {
    get().text
}
pub fn muted() -> Color32 {
    get().muted
}
pub fn error() -> Color32 {
    get().error
}
pub fn hover() -> Color32 {
    get().hover
}
pub fn tag() -> Color32 {
    get().tag
}
pub fn base() -> Base {
    get().base
}
pub fn accent_key() -> &'static str {
    get().accent_key
}

/// Text colour that reads on top of an accent-filled button.
pub fn on_accent() -> Color32 {
    if luminance(accent()) > 0.45 { Color32::from_rgb(0x14, 0x14, 0x18) } else { Color32::WHITE }
}

/// Relative luminance (0 = black, 1 = white).
fn luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// On the light theme bright accents (yellow, lime...) are darkened until
/// they're readable as text on the pale background.
fn readable_on_light(mut c: Color32) -> Color32 {
    while luminance(c) > 0.28 {
        let f = |v: u8| (v as f32 * 0.9) as u8;
        c = Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()));
    }
    c
}

/// A theme made of the user's own colours (or one from the gallery).
/// Shared as JSON text: {"name": ..., "light": false, "bg": "#121215", ...}.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CustomTheme {
    pub name: String,
    #[serde(default)]
    pub light: bool,
    pub bg: String,
    pub sidebar: String,
    pub surface: String,
    pub text: String,
    pub muted: String,
    pub accent: String,
}

/// "#1a2b3c" -> colour (None if malformed).
pub fn parse_hex(s: &str) -> Option<Color32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

pub fn to_hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

impl CustomTheme {
    #[allow(clippy::too_many_arguments)]
    fn new(name: &str, light: bool, bg: &str, sidebar: &str, surface: &str, text: &str, muted: &str, accent: &str) -> Self {
        let s = |v: &str| v.to_string();
        Self { name: s(name), light, bg: s(bg), sidebar: s(sidebar), surface: s(surface), text: s(text), muted: s(muted), accent: s(accent) }
    }

    /// The current colours as a custom theme (starting point for the editor).
    pub fn from_current(name: &str) -> Self {
        let p = get();
        Self {
            name: name.to_string(),
            light: p.base == Base::Light,
            bg: to_hex(p.bg),
            sidebar: to_hex(p.sidebar),
            surface: to_hex(p.surface),
            text: to_hex(p.text),
            muted: to_hex(p.muted),
            accent: to_hex(p.accent),
        }
    }

    fn palette(&self) -> Option<Palette> {
        let base = if self.light { LIGHT } else { DARK };
        let (bg, text) = (parse_hex(&self.bg)?, parse_hex(&self.text)?);
        Some(Palette {
            base: base.base,
            accent_key: "custom",
            accent: parse_hex(&self.accent)?,
            bg,
            sidebar: parse_hex(&self.sidebar)?,
            surface: parse_hex(&self.surface)?,
            extreme: if self.light { Color32::WHITE } else { mix(bg, Color32::BLACK, 0.25) },
            text,
            muted: parse_hex(&self.muted)?,
            error: base.error,
            hover: mix(bg, text, 0.07),
            tag: base.tag,
        })
    }
}

/// Ready-made themes for the gallery.
pub fn gallery() -> Vec<CustomTheme> {
    vec![
        CustomTheme::new("SoundCloud", false, "#121212", "#0a0a0a", "#1f1f1f", "#f2f2f2", "#999999", "#ff5500"),
        CustomTheme::new("Midnight", false, "#0f1626", "#0a0f1c", "#1a2438", "#e6ecf5", "#8b99b3", "#5b8cff"),
        CustomTheme::new("Synthwave", false, "#1a1033", "#120a26", "#2a1a4d", "#f5e6ff", "#b39bd1", "#ff3ea5"),
        CustomTheme::new("Forest", false, "#121a14", "#0c120e", "#1c2820", "#e3efe5", "#93a897", "#4ade80"),
        CustomTheme::new("OLED", false, "#000000", "#000000", "#141414", "#ffffff", "#8a8a8a", "#8b6cff"),
        CustomTheme::new("Mono", false, "#111111", "#000000", "#1e1e1e", "#f2f2f2", "#9a9a9a", "#e6e6e6"),
        CustomTheme::new("Nord", false, "#2e3440", "#272c36", "#3b4252", "#eceff4", "#a3abb9", "#88c0d0"),
        CustomTheme::new("Dracula", false, "#282a36", "#21222c", "#343746", "#f8f8f2", "#a4a7c0", "#bd93f9"),
        CustomTheme::new("Solarized", false, "#002b36", "#00212b", "#073642", "#eee8d5", "#93a1a1", "#b58900"),
        CustomTheme::new("Coffee", false, "#1e1714", "#16110e", "#2b211c", "#f0e6dc", "#b09a8a", "#d4a373"),
        CustomTheme::new("Sakura", true, "#fff5f8", "#fbe7ee", "#f6dbe5", "#2a1a20", "#7a5a66", "#d6457a"),
        CustomTheme::new("Paper", true, "#f5f1e8", "#ebe5d8", "#e2dacb", "#2b2620", "#6e665a", "#b5542b"),
    ]
}

/// Switches to a custom theme. Returns false (and changes nothing) if a
/// colour is malformed.
pub fn set_custom(ctx: &egui::Context, theme: &CustomTheme) -> bool {
    let Some(p) = theme.palette() else { return false };
    *PALETTE.write().unwrap_or_else(|e| e.into_inner()) = p;
    apply(ctx);
    true
}

/// Switches theme and accent (unknown names fall back to dark / purple).
pub fn set(ctx: &egui::Context, base: &str, accent: &str) {
    let base = Base::from_key(base);
    let (accent_key, _, colour) = ACCENTS.iter().copied().find(|(k, ..)| *k == accent).unwrap_or(ACCENTS[0]);
    let mut p = match base {
        Base::Dark => DARK,
        Base::Grey => GREY,
        Base::Light => LIGHT,
    };
    p.accent_key = accent_key;
    p.accent = if base == Base::Light { readable_on_light(colour) } else { colour };
    *PALETTE.write().unwrap_or_else(|e| e.into_inner()) = p;
    apply(ctx);
}

pub fn apply(ctx: &egui::Context) {
    let p = get();
    let light = p.base == Base::Light;
    let mut v = if light { egui::Visuals::light() } else { egui::Visuals::dark() };
    v.panel_fill = p.bg;
    // Menus and pop-ups use the page colour so buttons, sliders and
    // checkboxes (drawn in `surface`) stand out inside them.
    v.window_fill = if light { p.extreme } else { p.bg };
    v.extreme_bg_color = p.extreme;
    v.faint_bg_color = p.surface;
    v.override_text_color = Some(p.text);
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent.linear_multiply(if light { 0.3 } else { 0.45 });
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.window_corner_radius = CornerRadius::same(8);
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(6);
    }
    v.widgets.inactive.weak_bg_fill = p.surface;
    v.widgets.inactive.bg_fill = p.surface;
    // One set of visuals for whichever theme egui thinks it's in.
    ctx.set_visuals_of(Theme::Dark, v.clone());
    ctx.set_visuals_of(Theme::Light, v);
    ctx.set_theme(if light { Theme::Light } else { Theme::Dark });

    ctx.all_styles_mut(|style| {
        style.text_styles.insert(TextStyle::Body, FontId::proportional(14.5));
        style.text_styles.insert(TextStyle::Button, FontId::proportional(14.5));
        style.text_styles.insert(TextStyle::Heading, FontId::proportional(22.0));
        style.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
    });
    ctx.request_repaint();
}

/// The "Appearance" button and its menu. Returns the new (theme, accent)
/// names when one is picked.
pub fn appearance_menu(ui: &mut egui::Ui) -> Option<(&'static str, &'static str)> {
    let mut picked = None;
    let (current_base, current_accent) = (base(), accent_key());
    let stay_open = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    let button = egui::containers::menu::MenuButton::new(egui::RichText::new("🎨 Appearance").small());
    button.config(stay_open).ui(ui, |ui| {
        ui.set_min_width(236.0);
        ui.label(egui::RichText::new("Theme").strong());
        ui.horizontal(|ui| {
            for b in Base::ALL {
                if ui.selectable_label(b == current_base, b.label()).clicked() {
                    picked = Some((b.key(), current_accent));
                }
            }
        });
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Accent colour").strong());
        ui.add_space(2.0);
        let light = current_base == Base::Light;
        for row in ACCENTS.chunks(6) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for &(key, label, colour) in row {
                    let colour = if light { readable_on_light(colour) } else { colour };
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::click());
                    let painter = ui.painter();
                    if key == current_accent {
                        painter.circle_stroke(rect.center(), 14.0, Stroke::new(2.0, text()));
                    } else if resp.hovered() {
                        painter.circle_stroke(rect.center(), 14.0, Stroke::new(1.0, muted()));
                    }
                    painter.circle_filled(rect.center(), 11.0, colour);
                    if resp.on_hover_text(label).clicked() {
                        picked = Some((current_base.key(), key));
                    }
                }
            });
        }
    });
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_accents_are_readable() {
        for (_, name, c) in ACCENTS {
            assert!(luminance(readable_on_light(c)) <= 0.28, "{name}");
        }
        // Already-dark colours are left alone.
        assert_eq!(readable_on_light(Color32::from_rgb(0x30, 0x30, 0x90)), Color32::from_rgb(0x30, 0x30, 0x90));
    }

    #[test]
    fn unknown_names_fall_back() {
        assert_eq!(Base::from_key("neon"), Base::Dark);
        assert_eq!(Base::from_key("grey"), Base::Grey);
    }
}
