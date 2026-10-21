//! Appearance settings: theme gallery and editor, UI scale, font, layout and
//! wallpapers (a picture, a slideshow folder or the playing artwork).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, TextureHandle};

use super::theme::{self, CustomTheme, muted};
use super::{App, Msg};
use crate::config::Config;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum WallMode {
    #[default]
    Off,
    Picture,
    Slideshow,
    Artwork,
}

impl WallMode {
    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Picture => "picture",
            Self::Slideshow => "slideshow",
            Self::Artwork => "artwork",
        }
    }

    pub fn from_key(k: &str) -> Self {
        [Self::Off, Self::Picture, Self::Slideshow, Self::Artwork].into_iter().find(|m| m.key() == k).unwrap_or_default()
    }
}

/// Appearance settings mirrored from config.
pub struct Looks {
    pub ui_scale: f32,
    pub font: String,
    pub player_on_top: bool,
    pub sidebar_right: bool,
    pub compact: bool,
    pub custom: Option<CustomTheme>,
    pub wall_mode: WallMode,
    pub wall_path: Option<String>,
    pub wall_minutes: u32,
    pub wall_dim: f32,
    /// The theme being edited, and text pasted to import one.
    draft: Option<CustomTheme>,
    import: String,
}

impl Looks {
    pub fn load(c: &Config) -> Self {
        Self {
            ui_scale: c.ui_scale,
            font: c.font.clone(),
            player_on_top: c.player_on_top,
            sidebar_right: c.sidebar_right,
            compact: c.compact,
            custom: c.custom_theme.clone(),
            wall_mode: WallMode::from_key(&c.wallpaper_mode),
            wall_path: c.wallpaper_path.clone(),
            wall_minutes: c.wallpaper_minutes,
            wall_dim: c.wallpaper_dim,
            draft: None,
            import: String::new(),
        }
    }

    fn save(&self) {
        let mut c = Config::load();
        c.ui_scale = self.ui_scale;
        c.font = self.font.clone();
        c.player_on_top = self.player_on_top;
        c.sidebar_right = self.sidebar_right;
        c.compact = self.compact;
        c.custom_theme = self.custom.clone();
        c.wallpaper_mode = self.wall_mode.key().into();
        c.wallpaper_path = self.wall_path.clone();
        c.wallpaper_minutes = self.wall_minutes;
        c.wallpaper_dim = self.wall_dim;
        c.save();
    }
}

/// The loaded wallpaper and slideshow position.
#[derive(Default)]
pub struct Wallpaper {
    tex: Option<TextureHandle>,
    /// What `tex` shows (a file path), or what's being loaded.
    key: String,
    slideshow: Vec<PathBuf>,
    index: usize,
    next_at: Option<Instant>,
}

/// Pictures in a folder, sorted by name.
fn pictures_in(dir: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png"))
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// Decodes a picture, shrunk to at most 1920×1200 (wallpapers don't need more).
pub fn load_picture(path: &str) -> Result<egui::ColorImage, String> {
    let img = image::open(path).map_err(|e| e.to_string())?;
    let img = if img.width() > 1920 || img.height() > 1200 { img.thumbnail(1920, 1200) } else { img };
    let rgba = img.to_rgba8();
    Ok(egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}

impl App {
    /// Applies scale, font, layout and theme at start-up.
    pub(super) fn apply_looks(&mut self) {
        self.ctx.set_zoom_factor(self.looks.ui_scale.clamp(0.7, 1.5));
        super::rows::COMPACT.store(self.looks.compact, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = &self.looks.custom {
            theme::set_custom(&self.ctx, t);
        }
    }

    /// Draws the wallpaper (if any) behind the page content, dimmed.
    pub(super) fn paint_wallpaper(&mut self, ui: &mut egui::Ui) {
        let tex = match self.looks.wall_mode {
            WallMode::Off => return,
            WallMode::Artwork => {
                let url = self.queue.current().and_then(|i| i.track.as_ref()).and_then(|t| t.artwork().map(str::to_string));
                url.and_then(|u| self.art.get(&crate::api::models::sized_art(&u, "t500x500")))
            }
            WallMode::Picture | WallMode::Slideshow => {
                self.tick_wallpaper();
                self.wall.tex.clone()
            }
        };
        let Some(tex) = tex else { return };
        let rect = ui.max_rect().expand(20.0); // under the panel's margins too
        // Cover the area, cropping the picture's longer side.
        let [w, h] = tex.size().map(|v| v as f32);
        let (rw, rh) = (rect.width(), rect.height());
        let scale = (rw / w).max(rh / h);
        let (cw, ch) = (rw / scale / w, rh / scale / h);
        let uv = egui::Rect::from_center_size(egui::pos2(0.5, 0.5), egui::vec2(cw, ch));
        let painter = ui.painter();
        painter.image(tex.id(), rect, uv, Color32::WHITE);
        let dim = (self.looks.wall_dim.clamp(0.0, 0.95) * 255.0) as u8;
        let bg = theme::bg();
        painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(bg.r(), bg.g(), bg.b(), dim));
    }

    /// Loads the picture / moves the slideshow along when it's time.
    fn tick_wallpaper(&mut self) {
        let Some(path) = self.looks.wall_path.clone() else { return };
        let wanted = match self.looks.wall_mode {
            WallMode::Picture => path,
            WallMode::Slideshow => {
                if self.wall.slideshow.is_empty() {
                    self.wall.slideshow = pictures_in(&path);
                    self.wall.index = 0;
                }
                if self.wall.slideshow.is_empty() {
                    return;
                }
                let due = self.wall.next_at.is_some_and(|t| Instant::now() >= t);
                if due {
                    self.wall.index = (self.wall.index + 1) % self.wall.slideshow.len();
                }
                if self.wall.next_at.is_none() || due {
                    self.wall.next_at = Some(Instant::now() + Duration::from_secs(u64::from(self.looks.wall_minutes.max(1)) * 60));
                }
                if let Some(t) = self.wall.next_at {
                    self.ctx.request_repaint_after(t.saturating_duration_since(Instant::now()));
                }
                self.wall.slideshow[self.wall.index].display().to_string()
            }
            _ => return,
        };
        if self.wall.key != wanted {
            self.wall.key = wanted.clone();
            self.spawn(move |_, send| send(Msg::Wallpaper { key: wanted.clone(), image: load_picture(&wanted) }));
        }
    }

    pub(super) fn wallpaper_loaded(&mut self, key: String, image: Result<egui::ColorImage, String>) {
        if key != self.wall.key {
            return;
        }
        match image {
            Ok(img) => self.wall.tex = Some(self.ctx.load_texture("wallpaper", img, egui::TextureOptions::LINEAR)),
            Err(e) => self.error(format!("Couldn't load the wallpaper: {e}")),
        }
    }

    pub(super) fn appearance_settings(&mut self, ui: &mut egui::Ui) {
        let mut save = false;

        // Theme gallery.
        ui.label(RichText::new("Theme gallery").strong());
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
            for t in theme::gallery() {
                if theme_card(ui, &t, self.looks.custom.as_ref() == Some(&t)).clicked() {
                    theme::set_custom(&self.ctx, &t);
                    self.looks.custom = Some(t);
                    save = true;
                }
            }
        });
        ui.label(
            RichText::new("Or use 🎨 Appearance in the sidebar for Dark / Grey / Light with an accent colour.").small().color(muted()),
        );

        // Theme editor.
        ui.add_space(6.0);
        egui::CollapsingHeader::new(RichText::new("Make your own theme").strong()).show(ui, |ui| {
            let draft = self.looks.draft.get_or_insert_with(|| self.looks.custom.clone().unwrap_or_else(|| CustomTheme::from_current("My theme")));
            let mut live = false;
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut draft.name);
                live |= ui.checkbox(&mut draft.light, "Light theme").changed();
            });
            egui::Grid::new("theme-editor").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                for (label, field) in [
                    ("Background", &mut draft.bg),
                    ("Sidebar & player bar", &mut draft.sidebar),
                    ("Buttons & cards", &mut draft.surface),
                    ("Text", &mut draft.text),
                    ("Dim text", &mut draft.muted),
                    ("Accent", &mut draft.accent),
                ] {
                    ui.label(label);
                    let mut c = theme::parse_hex(field).unwrap_or(Color32::GRAY);
                    if egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::Opaque).changed() {
                        *field = theme::to_hex(c);
                        live = true;
                    }
                    ui.end_row();
                }
            });
            if live {
                theme::set_custom(&self.ctx, draft);
            }
            ui.horizontal(|ui| {
                if ui.button("Save theme").clicked() {
                    self.looks.custom = Some(draft.clone());
                    save = true;
                }
                if ui.button("Copy as text").on_hover_text("Share it: anyone can paste it below").clicked() {
                    if let Ok(json) = serde_json::to_string(draft) {
                        ui.ctx().copy_text(json);
                    }
                }
            });
            ui.add_space(4.0);
            ui.label(RichText::new("Paste a theme someone shared").small().color(muted()));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.looks.import).hint_text("{\"name\": ...}").desired_width(320.0));
                if ui.button("Use it").clicked() {
                    match serde_json::from_str::<CustomTheme>(self.looks.import.trim()) {
                        Ok(t) if theme::set_custom(&self.ctx, &t) => {
                            self.looks.custom = Some(t.clone());
                            self.looks.draft = Some(t);
                            self.looks.import.clear();
                            save = true;
                        }
                        _ => self.error("That isn't a Cumulus theme (copy it with \"Copy as text\")."),
                    }
                }
            });
        });

        // Scale and font.
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("UI scale");
            let mut pct = (self.looks.ui_scale * 100.0).round();
            let r = ui.add(egui::Slider::new(&mut pct, 70.0..=150.0).step_by(5.0).suffix("%"));
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                self.looks.ui_scale = pct / 100.0;
                self.ctx.set_zoom_factor(self.looks.ui_scale);
                save = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("Font");
            egui::ComboBox::from_id_salt("font").selected_text(&self.looks.font).show_ui(ui, |ui| {
                for (name, _) in super::fonts::CHOICES {
                    if ui.selectable_label(self.looks.font == name, name).clicked() {
                        self.looks.font = name.to_string();
                        super::fonts::install(&self.ctx, name);
                        save = true;
                    }
                }
            });
        });

        // Layout.
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Player bar");
            save |= ui.radio_value(&mut self.looks.player_on_top, false, "Bottom").changed();
            save |= ui.radio_value(&mut self.looks.player_on_top, true, "Top").changed();
        });
        ui.horizontal(|ui| {
            ui.label("Sidebar");
            save |= ui.radio_value(&mut self.looks.sidebar_right, false, "Left").changed();
            save |= ui.radio_value(&mut self.looks.sidebar_right, true, "Right").changed();
        });
        if ui.checkbox(&mut self.looks.compact, "Compact lists (smaller rows)").changed() {
            super::rows::COMPACT.store(self.looks.compact, std::sync::atomic::Ordering::Relaxed);
            save = true;
        }

        // Visualizer.
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Visualizer");
            for style in super::visualizer::Style::ALL {
                if ui.radio_value(&mut self.viz_style, style, style.label()).changed() {
                    self.player.set_visualizer(style != super::visualizer::Style::Off);
                    let mut c = Config::load();
                    c.visualizer = style.key().into();
                    c.save();
                }
            }
        });
        ui.label(RichText::new("Behind the player bar. Uses a little CPU while music plays.").small().color(muted()));

        // Wallpaper.
        ui.add_space(8.0);
        ui.label(RichText::new("Wallpaper").strong());
        ui.horizontal(|ui| {
            for (m, label) in [
                (WallMode::Off, "Off"),
                (WallMode::Picture, "Picture"),
                (WallMode::Slideshow, "Slideshow"),
                (WallMode::Artwork, "Playing artwork"),
            ] {
                if ui.radio_value(&mut self.looks.wall_mode, m, label).changed() {
                    self.wall = Wallpaper::default();
                    save = true;
                }
            }
        });
        match self.looks.wall_mode {
            WallMode::Picture | WallMode::Slideshow => {
                let folder = self.looks.wall_mode == WallMode::Slideshow;
                ui.horizontal(|ui| {
                    let shown = self.looks.wall_path.clone().unwrap_or_else(|| "Nothing chosen".into());
                    ui.add(egui::Label::new(RichText::new(shown).small().color(muted())).truncate());
                    if ui.button(if folder { "Choose folder…" } else { "Choose picture…" }).clicked() {
                        let picked = if folder {
                            crate::dialogs::pick_folder("Folder of wallpapers")
                        } else {
                            crate::dialogs::pick_file("Wallpaper", "Pictures", &["jpg", "jpeg", "png"])
                        };
                        if let Some(p) = picked {
                            self.looks.wall_path = Some(p.display().to_string());
                            self.wall = Wallpaper::default();
                            save = true;
                        }
                    }
                });
                if folder {
                    ui.horizontal(|ui| {
                        ui.label("Change every");
                        let r = ui.add(egui::Slider::new(&mut self.looks.wall_minutes, 1..=60).suffix(" min"));
                        save |= r.drag_stopped() || (r.changed() && !r.dragged());
                    });
                }
            }
            WallMode::Artwork => {
                ui.label(RichText::new("The playing song's cover, behind everything.").small().color(muted()));
            }
            WallMode::Off => {}
        }
        if self.looks.wall_mode != WallMode::Off {
            ui.horizontal(|ui| {
                ui.label("Dimming");
                let mut pct = (self.looks.wall_dim * 100.0).round();
                let r = ui.add(egui::Slider::new(&mut pct, 0.0..=95.0).step_by(5.0).suffix("%"));
                if r.changed() {
                    self.looks.wall_dim = pct / 100.0;
                }
                save |= r.drag_stopped() || (r.changed() && !r.dragged());
            });
        }

        if save {
            self.looks.save();
        }
    }
}

/// A clickable preview of a gallery theme.
fn theme_card(ui: &mut egui::Ui, t: &CustomTheme, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(104.0, 74.0), egui::Sense::click());
    let c = |h: &str| theme::parse_hex(h).unwrap_or(Color32::GRAY);
    let p = ui.painter();
    let body = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), 52.0));
    p.rect_filled(body, 6.0, c(&t.bg));
    p.rect_filled(egui::Rect::from_min_size(body.min, egui::vec2(26.0, body.height())), 6.0, c(&t.sidebar));
    p.rect_filled(egui::Rect::from_min_size(body.min + egui::vec2(34.0, 10.0), egui::vec2(60.0, 8.0)), 3.0, c(&t.text));
    p.rect_filled(egui::Rect::from_min_size(body.min + egui::vec2(34.0, 24.0), egui::vec2(44.0, 6.0)), 3.0, c(&t.muted));
    p.circle_filled(body.min + egui::vec2(86.0, 40.0), 6.0, c(&t.accent));
    let stroke = if selected { egui::Stroke::new(2.0, theme::accent()) } else if resp.hovered() { egui::Stroke::new(1.0, muted()) } else { egui::Stroke::NONE };
    p.rect_stroke(body, 6.0, stroke, egui::StrokeKind::Outside);
    p.text(
        egui::pos2(rect.center().x, rect.bottom() - 9.0),
        egui::Align2::CENTER_CENTER,
        &t.name,
        egui::FontId::proportional(12.0),
        theme::text(),
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}
