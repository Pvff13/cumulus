//! The persistent bottom bar: now playing (with links back to SoundCloud),
//! transport controls, seek bar and volume. Also the keyboard shortcuts.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Key, Layout, Modifiers, RichText, Sense, vec2};

use super::queue::Repeat;
use super::rows::{self, format_duration, link_label};
use super::theme::{accent, muted, sidebar};
use super::App;

pub const HEIGHT: f32 = 84.0;
const SEEK_STEP: f64 = 5.0;
const VOLUME_STEP: f32 = 0.05;

impl App {
    /// Global shortcuts (ignored while typing in a text field).
    pub(super) fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (slash, space, left, right, up, down) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Slash),
                i.consume_key(Modifiers::NONE, Key::Space),
                i.consume_key(Modifiers::NONE, Key::ArrowLeft),
                i.consume_key(Modifiers::NONE, Key::ArrowRight),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
            )
        });
        if slash {
            self.focus_search();
        }
        if space {
            self.toggle_play();
        }
        if left || right {
            if let Some((_, pos)) = self.player.now() {
                self.seek_to(pos + if right { SEEK_STEP } else { -SEEK_STEP });
            }
        }
        if up || down {
            self.set_volume(self.volume + if up { VOLUME_STEP } else { -VOLUME_STEP }, true);
        }
    }

    pub(super) fn toggle_play(&mut self) {
        if !self.player.is_paused() {
            self.player.pause();
        } else if self.player.now().is_some() {
            self.player.resume();
        } else if let Some(item) = self.queue.current().cloned() {
            // Finished or stopped: start the current track again.
            self.player.play(item, 0.0);
            self.player.set_next(self.queue.peek_next());
        }
    }

    pub(super) fn seek_to(&mut self, to: f64) {
        let to = to.max(0.0);
        self.player.seek(to);
        self.pending_seek = Some((to, Instant::now()));
        self.desktop.position_changed();
    }

    pub(super) fn set_volume(&mut self, v: f32, save: bool) {
        self.volume = v.clamp(0.0, 1.0);
        self.player.set_volume(self.volume);
        if save {
            // Load fresh: the API layer may have saved a new client_id meanwhile.
            let mut config = crate::config::Config::load();
            config.volume = self.volume;
            config.save();
        }
    }

    pub(super) fn next(&mut self) {
        if let Some(item) = self.queue.advance() {
            self.player.play(item, 0.0);
            self.player.set_next(self.queue.peek_next());
        }
    }

    pub(super) fn previous(&mut self) {
        // Like every player: restart the track unless we're right at its start.
        if self.player.now().is_some_and(|(_, pos)| pos > 3.0) {
            self.seek_to(0.0);
        } else if let Some(item) = self.queue.back() {
            self.player.play(item, 0.0);
            self.player.set_next(self.queue.peek_next());
        }
    }

    pub(super) fn player_bar(&mut self, ui: &mut egui::Ui) {
        let Some(item) = self.queue.current().cloned() else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Pick a track to start playing  ·  Space play/pause  ·  ←/→ seek  ·  ↑/↓ volume  ·  / search").color(muted()));
            });
            return;
        };
        let now = self.player.now().filter(|(id, _)| *id == item.id);
        let playing = !self.player.is_paused();
        // Show the requested position right after a seek until playback catches up.
        let mut pos = now.map_or(0.0, |(_, p)| p);
        if let Some((target, at)) = self.pending_seek {
            if at.elapsed() < Duration::from_millis(800) && (pos - target).abs() > 1.0 {
                pos = target;
            } else {
                self.pending_seek = None;
            }
        }
        let duration = item.track.as_ref().map_or(0.0, |t| t.duration as f64 / 1000.0);

        let mut like = None;
        let full = ui.available_width();
        let side = (full * 0.28).clamp(180.0, 340.0);
        ui.horizontal(|ui| {
            // Now playing.
            ui.allocate_ui_with_layout(vec2(side, HEIGHT - 16.0), Layout::left_to_right(Align::Center), |ui| {
                let art = item.track.as_ref().and_then(|t| t.artwork().map(str::to_string));
                let _ = rows::artwork(ui, &self.art, art.as_deref(), 56.0, "large");
                ui.vertical(|ui| {
                    ui.add_space(6.0);
                    match &item.track {
                        Some(t) => {
                            let title = ui
                                .add(
                                    egui::Label::new(RichText::new(&t.title).strong())
                                        .truncate()
                                        .sense(Sense::click()),
                                )
                                .on_hover_text("Open on SoundCloud")
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if title.clicked() && !t.permalink_url.is_empty() {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(&t.permalink_url));
                            }
                            ui.horizontal(|ui| {
                                link_label(ui, &t.user.username, &t.user.permalink_url);
                                if rows::heart(ui, self.liked.contains(&t.id)).clicked() {
                                    like = Some(t.clone());
                                }
                            });
                        }
                        None => {
                            ui.label(RichText::new("Loading...").color(muted()));
                        }
                    }
                });
            });

            // Transport + seek bar.
            let center = (ui.available_width() - side).max(200.0);
            ui.allocate_ui_with_layout(vec2(center, HEIGHT - 16.0), Layout::top_down(Align::Center), |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let controls = 6.0 * 34.0;
                    ui.add_space(((ui.available_width() - controls) / 2.0).max(0.0));
                    if icon(ui, "🔀", self.queue.shuffle, "Shuffle").clicked() {
                        self.queue.set_shuffle(!self.queue.shuffle);
                        self.player.set_next(self.queue.peek_next());
                    }
                    if icon(ui, "⏮", false, "Previous").clicked() {
                        self.previous();
                    }
                    let main = if playing { "⏸" } else { "▶" };
                    if ui
                        .add(egui::Button::new(RichText::new(main).size(20.0)).corner_radius(16).min_size(vec2(36.0, 32.0)))
                        .on_hover_text(if playing { "Pause (Space)" } else { "Play (Space)" })
                        .clicked()
                    {
                        self.toggle_play();
                    }
                    if icon(ui, "⏭", false, "Next").clicked() {
                        self.next();
                    }
                    let (glyph, tip) = match self.queue.repeat {
                        Repeat::Off => ("🔁", "Repeat: off"),
                        Repeat::All => ("🔁", "Repeat: all"),
                        Repeat::One => ("🔂", "Repeat: one"),
                    };
                    if icon(ui, glyph, self.queue.repeat != Repeat::Off, tip).clicked() {
                        self.queue.repeat = self.queue.repeat.cycle();
                        self.player.set_next(self.queue.peek_next());
                    }
                    let tip = if self.autoplay {
                        "Autoplay on: similar tracks play when the queue runs out"
                    } else {
                        "Autoplay off: stop at the end of the queue"
                    };
                    if icon(ui, "∞", self.autoplay, tip).clicked() {
                        self.set_autoplay(!self.autoplay);
                    }
                });
                ui.horizontal(|ui| {
                    let time_w = 52.0;
                    ui.add_sized([time_w, 18.0], egui::Label::new(RichText::new(format_duration((pos * 1000.0) as u64)).small().color(muted())));
                    let mut value = self.seek_drag.unwrap_or(pos);
                    ui.spacing_mut().slider_width = (ui.available_width() - time_w - 8.0).max(60.0);
                    let resp = ui.add(
                        egui::Slider::new(&mut value, 0.0..=duration.max(1.0)).show_value(false).trailing_fill(true),
                    );
                    if resp.dragged() {
                        self.seek_drag = Some(value);
                    }
                    if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                        self.seek_drag = None;
                        self.seek_to(value);
                    }
                    ui.label(RichText::new(format_duration((duration * 1000.0) as u64)).small().color(muted()));
                });
            });

            // Queue toggle + volume.
            ui.allocate_ui_with_layout(vec2(ui.available_width(), HEIGHT - 16.0), Layout::right_to_left(Align::Center), |ui| {
                if icon(ui, "☰", self.show_queue, "Queue").clicked() {
                    self.show_queue = !self.show_queue;
                }
                ui.add_space(6.0);
                let mut v = self.volume;
                ui.spacing_mut().slider_width = 90.0;
                let resp = ui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(false).trailing_fill(true));
                if resp.changed() {
                    self.set_volume(v, false);
                }
                if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                    self.set_volume(v, true);
                }
                let glyph = if self.volume == 0.0 { "🔇" } else { "🔊" };
                ui.label(RichText::new(glyph).color(muted())).on_hover_text("Volume (↑/↓)");
            });
        });
        if let Some(track) = like {
            self.track_action(rows::TrackAction::LikeOnSoundCloud, track);
        }
    }
}

fn icon(ui: &mut egui::Ui, glyph: &str, active: bool, tip: &str) -> egui::Response {
    let text = RichText::new(glyph).size(16.0).color(if active { accent() } else { muted() });
    ui.add(egui::Button::new(text).frame(false).min_size(vec2(30.0, 30.0))).on_hover_text(tip)
}

pub fn bar_frame() -> egui::Frame {
    egui::Frame::NONE.fill(sidebar()).inner_margin(egui::Margin::symmetric(14, 8))
}
