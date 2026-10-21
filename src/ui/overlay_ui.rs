//! The "▣ Overlay" menu next to the refresh button, and the pop-out
//! now-playing window. The pop-out is a real second window: it keeps
//! updating while the main window is minimised or hidden in the tray, and
//! OBS can capture it with a Window Capture source.

use std::time::Duration;

use eframe::egui::{
    self, Align, CornerRadius, Layout, RichText, Sense, ViewportBuilder, ViewportId, WindowLevel, vec2,
};

use super::App;
use super::rows;
use super::theme::{accent, bg, muted};
use crate::config::Config;

/// What the pop-out shows.
#[derive(Default, Clone, PartialEq)]
pub struct NowPlaying {
    pub track_id: u64,
    pub title: String,
    pub artist: String,
    pub artwork: Option<String>,
    pub playing: bool,
    /// Seconds.
    pub position: f64,
    pub duration: f64,
}

/// Pop-out settings, remembered in config.json.
pub struct OverlayMenu {
    pub popout: bool,
    pub on_top: bool,
    pub controls: bool,
}

impl OverlayMenu {
    pub fn load(c: &Config) -> Self {
        Self { popout: c.popout, on_top: c.popout_on_top, controls: c.popout_controls }
    }

    fn save(&self) {
        let mut c = Config::load();
        (c.popout, c.popout_on_top, c.popout_controls) = (self.popout, self.on_top, self.controls);
        c.save();
    }
}

/// Settings toggled in a menu this frame (saved afterwards).
#[derive(Default)]
pub struct MenuResult {
    pub changed: bool,
}

/// The pop-out's toggles; shared by the ▣ menu and the pop-out's own right-click menu.
fn toggles(ui: &mut egui::Ui, menu: &mut OverlayMenu) -> bool {
    let mut changed = false;
    changed |= ui
        .checkbox(&mut menu.controls, "Show controls")
        .on_hover_text("Previous, play/pause, next and volume on the pop-out")
        .changed();
    changed |= ui
        .checkbox(&mut menu.on_top, "Always on top")
        .on_hover_text("Keep the pop-out above other windows")
        .changed();
    changed
}

/// The "▣ Overlay" button and its menu. Takes only the bits it needs so it
/// can sit in headers that are drawn while other app state is borrowed.
pub fn overlay_menu(ui: &mut egui::Ui, menu: &mut OverlayMenu) -> MenuResult {
    let mut out = MenuResult::default();
    let label = RichText::new("▣ Overlay").color(if menu.popout { accent() } else { ui.visuals().text_color() });
    // Stays open while ticking options; closes on a click outside it.
    let stay_open = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    egui::containers::menu::MenuButton::new(label).config(stay_open).ui(ui, |ui| {
        ui.set_min_width(270.0);
        ui.label(RichText::new("Now-playing pop-out").strong());
        ui.label(RichText::new("A small window with the current song's cover, title and artist.").small().color(muted()));
        ui.add_space(6.0);
        out.changed |= ui.checkbox(&mut menu.popout, "Show pop-out window").changed();
        ui.add_enabled_ui(menu.popout, |ui| {
            ui.indent("popout-options", |ui| out.changed |= toggles(ui, menu));
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "For your stream: in OBS add a Window Capture source and pick \"Cumulus - Now playing\". \
                 Right-click the pop-out for these options too.",
            )
            .small()
            .color(muted()),
        );
    });
    out
}

impl App {
    /// What's playing, for the pop-out.
    pub(super) fn now_playing(&self) -> NowPlaying {
        let now = self.player.now();
        let Some(t) = self.queue.current().and_then(|i| i.track.as_ref()) else {
            return NowPlaying::default();
        };
        NowPlaying {
            track_id: t.id,
            title: t.title.clone(),
            artist: t.user.username.clone(),
            artwork: t.artwork().map(|u| crate::api::models::sized_art(u, "t300x300")),
            playing: !self.player.is_paused() && now.is_some(),
            position: now.filter(|(id, _)| *id == t.id).map_or(0.0, |(_, p)| p),
            duration: t.duration as f64 / 1000.0,
        }
    }

    /// Saves pop-out settings changed in a menu.
    pub(super) fn handle_menu(&mut self, result: MenuResult) {
        if result.changed {
            self.overlay_menu.save();
        }
    }

    /// Draws the pop-out window (while it's switched on).
    pub(super) fn popout(&mut self, ctx: &egui::Context) {
        if !self.overlay_menu.popout {
            return;
        }
        let np = self.now_playing();
        let art = self.art.clone();
        let mut volume = self.volume;
        let level = if self.overlay_menu.on_top { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal };
        let builder = ViewportBuilder::default()
            .with_title("Cumulus - Now playing")
            .with_inner_size([440.0, 100.0])
            .with_min_inner_size([300.0, 72.0])
            .with_window_level(level)
            .with_icon(std::sync::Arc::new(super::window_icon()));
        let mut closed = false;
        let mut action = None;
        let mut settings_changed = false;
        let mut expand = false;
        let mini = self.mini.is_some();
        let menu = &mut self.overlay_menu;
        ctx.show_viewport_immediate(ViewportId::from_hash_of("cumulus-now-playing"), builder, |ui, _class| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                closed = true;
            }
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(bg()).inner_margin(12.0)).show(ui, |ui| {
                // Right-click anywhere on the pop-out for its options.
                let bg = ui.interact(ui.max_rect(), ui.scope_id().with("popout-bg"), Sense::click());
                egui::Popup::context_menu(&bg).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                    if mini && ui.button("⛶  Back to the full window").clicked() {
                        expand = true;
                        ui.close();
                    }
                    settings_changed |= toggles(ui, menu);
                    ui.separator();
                    if ui.button("Close pop-out").clicked() {
                        closed = true;
                        ui.close();
                    }
                });
                let controls = menu.controls.then_some(&mut volume);
                let full = ui.max_rect();
                action = now_playing_card(ui, &art, &np, controls);
                // Mini player: a way back to the full window.
                if mini {
                    let r = egui::Rect::from_min_size(full.right_top() + vec2(-14.0, -10.0), vec2(22.0, 18.0));
                    let b = egui::Button::new(RichText::new("⛶").size(13.0).color(muted())).frame(false);
                    if ui.put(r, b).on_hover_text("Back to the full window").clicked() {
                        expand = true;
                    }
                }
            });
            if np.playing {
                ui.ctx().request_repaint_after(Duration::from_millis(500));
            }
        });
        match action {
            Some(PopAction::Previous) => self.previous(),
            Some(PopAction::PlayPause) => self.toggle_play(),
            Some(PopAction::Next) => self.next(),
            Some(PopAction::Volume { save }) => self.set_volume(volume, save),
            None => {}
        }
        // Leaving the mini player restores the pop-out as it was.
        if (closed || expand) && self.mini.is_some() {
            self.set_mini(false);
            return;
        }
        if closed {
            self.overlay_menu.popout = false;
            settings_changed = true;
        }
        if settings_changed && self.mini.is_none() {
            self.overlay_menu.save();
        }
    }
}

enum PopAction {
    Previous,
    PlayPause,
    Next,
    /// The slider moved; `save` once it's released.
    Volume { save: bool },
}

/// Cover, title and artist, with a progress bar along the bottom. With
/// `volume` given (controls switched on), previous / play-pause / next and a
/// volume slider are shown on the right.
fn now_playing_card(
    ui: &mut egui::Ui,
    art: &crate::cache::art::ArtCache,
    np: &NowPlaying,
    volume: Option<&mut f32>,
) -> Option<PopAction> {
    let full = ui.max_rect();
    let mut action = None;
    let size = (full.height() - 6.0).clamp(40.0, 160.0);
    // Fixed column for the controls on the right; cover and text get the rest.
    const CONTROLS_W: f32 = 132.0;
    let controls_w = if volume.is_some() { CONTROLS_W } else { 0.0 };
    let text_rect = egui::Rect::from_min_max(full.min, egui::pos2(full.right() - controls_w, full.bottom()));
    let mut left = ui.new_child(egui::UiBuilder::new().max_rect(text_rect).layout(Layout::left_to_right(Align::Center)));
    if np.track_id == 0 {
        left.label(RichText::new("Nothing playing").color(muted()));
    } else {
        let _ = rows::artwork(&mut left, art, np.artwork.as_deref(), size, "t300x300");
        left.add_space(6.0);
        left.vertical(|ui| {
            ui.add_space((size - 44.0).max(0.0) / 2.0);
            ui.add(egui::Label::new(RichText::new(&np.title).strong().size(17.0).color(super::theme::text())).truncate());
            ui.add(egui::Label::new(RichText::new(&np.artist).size(14.0).color(muted())).truncate());
            if !np.playing {
                ui.label(RichText::new("Paused").small().color(muted()));
            }
        });
    }
    if let Some(volume) = volume {
        let rect = egui::Rect::from_min_max(egui::pos2(full.right() - CONTROLS_W, full.top()), full.max);
        let mut right = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Center)));
        right.add_space(((rect.height() - 58.0) / 2.0).max(0.0));
        right.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.add_space(((CONTROLS_W - 3.0 * 34.0) / 2.0).max(0.0));
            let button = |ui: &mut egui::Ui, glyph: &str, tip: &str| {
                let text = RichText::new(glyph).size(17.0).color(super::theme::text());
                ui.add(egui::Button::new(text).frame(false).min_size(vec2(32.0, 30.0))).on_hover_text(tip).clicked()
            };
            if button(ui, "⏮", "Previous") {
                action = Some(PopAction::Previous);
            }
            let tip = if np.playing { "Pause" } else { "Play" };
            let main = ui.add(egui::Button::new("").frame(false).min_size(vec2(32.0, 30.0))).on_hover_text(tip);
            rows::play_pause_icon(ui.painter(), main.rect.center(), np.playing, 0.95, super::theme::text());
            if main.clicked() {
                action = Some(PopAction::PlayPause);
            }
            if button(ui, "⏭", "Next") {
                action = Some(PopAction::Next);
            }
        });
        right.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new(if *volume == 0.0 { "🔇" } else { "🔊" }).color(muted()));
            ui.spacing_mut().slider_width = CONTROLS_W - 34.0;
            let r = ui.add(egui::Slider::new(volume, 0.0..=1.0).show_value(false).trailing_fill(true));
            if r.changed() || r.drag_stopped() {
                action = Some(PopAction::Volume { save: r.drag_stopped() || (r.changed() && !r.dragged()) });
            }
        });
    }
    if np.track_id != 0 && np.duration > 0.0 {
        let frac = (np.position / np.duration).clamp(0.0, 1.0) as f32;
        let bar = egui::Rect::from_min_size(egui::pos2(full.left(), full.bottom() + 9.0), vec2(full.width() * frac, 3.0));
        ui.painter().rect_filled(bar, CornerRadius::same(1), accent());
    }
    action
}
