//! The persistent bottom bar: now playing (with links back to SoundCloud),
//! transport controls, seek bar and volume. Also the keyboard shortcuts.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Key, Layout, Modifiers, RichText, Sense, vec2};

use super::queue::Repeat;
use super::rows::{self, format_duration, link_label};
use super::theme::{accent, muted, sidebar};
use super::App;
use super::hotkeys::Action;

pub const HEIGHT: f32 = 84.0;
const SPEED_STEP: f32 = 0.05;
/// Speed presets: (label, speed).
const SPEEDS: [(&str, f32); 3] = [("Slowed", 0.85), ("Normal", 1.0), ("Nightcore", 1.25)];

/// 1.0 -> "1×", 1.25 -> "1.25×".
fn speed_label(speed: f32) -> String {
    let s = format!("{speed:.2}");
    format!("{}×", s.trim_end_matches('0').trim_end_matches('.'))
}
/// Every keyboard shortcut (shown as a tooltip).
pub const SHORTCUTS: &str = "Space  play / pause\n\
←/→  seek 5 s    Ctrl+←/→  previous / next track\n\
↑/↓  volume    M  mute\n\
S  shuffle    R  repeat    L  like (on SoundCloud)\n\
Shift+<  /  Shift+>  slower / faster\n\
/  or  Ctrl+F  or  Ctrl+K  search\n\
Alt+←/→  back / forward";
/// The sleep timer fades out over this long before pausing.
const SLEEP_FADE: Duration = Duration::from_secs(10);

/// When the sleep timer stops playback.
#[derive(Clone, Copy, PartialEq)]
pub enum Sleep {
    At(Instant),
    /// At the end of this track.
    EndOfTrack(u64),
}
const SEEK_STEP: f64 = 5.0;
const VOLUME_STEP: f32 = 0.05;

impl App {
    /// Keyboard shortcuts (ignored while typing in a text field or while
    /// Settings is recording a new key).
    pub(super) fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || self.recording.is_some() {
            return;
        }
        let mut fired = Vec::new();
        ctx.input_mut(|i| {
            for (shortcut, action) in self.bindings.local_shortcuts() {
                if i.consume_shortcut(&shortcut) {
                    fired.push(action);
                }
            }
            // "/" always opens search too.
            if i.consume_key(Modifiers::NONE, Key::Slash) {
                fired.push(Action::Search);
            }
        });
        for a in fired {
            self.run_action(a);
        }
    }

    /// Does what a shortcut / global hotkey asks.
    pub(super) fn run_action(&mut self, action: Action) {
        match action {
            Action::PlayPause => self.toggle_play(),
            Action::Next => self.next(),
            Action::Previous => self.previous(),
            Action::SeekForward | Action::SeekBack => {
                if let Some((_, pos)) = self.player.now() {
                    self.seek_to(pos + if action == Action::SeekForward { SEEK_STEP } else { -SEEK_STEP });
                }
            }
            Action::VolumeUp => self.set_volume(self.volume + VOLUME_STEP, true),
            Action::VolumeDown => self.set_volume(self.volume - VOLUME_STEP, true),
            Action::Mute => match self.muted_from.take() {
                Some(v) if self.volume == 0.0 => self.set_volume(v, true),
                _ if self.volume == 0.0 => self.set_volume(0.5, true),
                _ => {
                    self.muted_from = Some(self.volume);
                    self.set_volume(0.0, true);
                }
            },
            Action::Shuffle => {
                self.queue.set_shuffle(!self.queue.shuffle);
                self.player.set_next(self.queue.peek_next());
                self.info(if self.queue.shuffle { "Shuffle on" } else { "Shuffle off" });
            }
            Action::Repeat => {
                self.queue.repeat = self.queue.repeat.cycle();
                self.player.set_next(self.queue.peek_next());
                self.info(match self.queue.repeat {
                    Repeat::Off => "Repeat off",
                    Repeat::All => "Repeat all",
                    Repeat::One => "Repeat one",
                });
            }
            Action::Like => {
                if let Some(t) = self.queue.current().and_then(|i| i.track.clone()) {
                    self.track_action(rows::TrackAction::LikeOnSoundCloud, t);
                }
            }
            Action::Search => self.focus_search(),
            Action::Back => self.go_back(),
            Action::Forward => self.go_forward(),
            Action::Slower | Action::Faster => {
                let step = if action == Action::Faster { SPEED_STEP } else { -SPEED_STEP };
                let speed = ((self.speed + step) * 20.0).round() / 20.0;
                self.set_speed(speed, true);
                self.info(format!("Speed {}", speed_label(self.speed)));
            }
        }
    }

    /// Runs every frame: fades out and pauses when the sleep timer is up.
    pub(super) fn sleep_tick(&mut self, ctx: &egui::Context) {
        #[cfg(debug_assertions)]
        if std::env::var_os("CUMULUS_DEV_POSLOG").is_some() {
            if let Some((id, pos)) = self.player.now() {
                eprintln!("poslog {:.2} {id} {pos:.2}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64());
            }
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        let Some(sleep) = self.sleep else { return };
        ctx.request_repaint_after(Duration::from_millis(250));
        if self.player.is_paused() {
            return; // the countdown keeps going, but there's nothing to fade
        }
        let left = match sleep {
            Sleep::At(end) => end.saturating_duration_since(Instant::now()),
            Sleep::EndOfTrack(id) => match (self.player.now(), self.queue.current()) {
                (Some((now_id, pos)), Some(item)) if now_id == id => {
                    let duration = item.track.as_ref().map_or(f64::MAX, |t| t.duration as f64 / 1000.0);
                    Duration::from_secs_f64((duration - pos).max(0.0))
                }
                // The next track has started (or playback ended).
                _ => Duration::ZERO,
            },
        };
        if left.is_zero() {
            self.player.pause();
            self.player.set_volume(self.volume);
            self.sleep = None;
            self.info("Sleep timer: paused. Goodnight!");
        } else if left < SLEEP_FADE {
            let t = left.as_secs_f32() / SLEEP_FADE.as_secs_f32();
            self.player.set_volume(self.volume * t);
        }
    }

    fn set_sleep(&mut self, sleep: Option<Sleep>) {
        self.sleep = sleep;
        self.player.set_volume(self.volume); // undo any fade in progress
        self.info(match sleep {
            None => "Sleep timer off".to_string(),
            Some(Sleep::EndOfTrack(_)) => "Sleep timer: pausing at the end of this track".to_string(),
            Some(Sleep::At(end)) => {
                format!("Sleep timer: pausing in {} min", (end.saturating_duration_since(Instant::now()).as_secs() + 30) / 60)
            }
        });
    }

    /// The ☾ button and its menu.
    fn sleep_button(&mut self, ui: &mut egui::Ui) {
        let label = match self.sleep {
            Some(Sleep::At(end)) => {
                let s = end.saturating_duration_since(Instant::now()).as_secs();
                format!("☾ {}:{:02}", s / 60, s % 60)
            }
            Some(Sleep::EndOfTrack(_)) => "☾ end".to_string(),
            None => "☾".to_string(),
        };
        let text = RichText::new(label).size(15.0).color(if self.sleep.is_some() { accent() } else { muted() });
        let mut pick: Option<Option<Sleep>> = None;
        egui::containers::menu::MenuButton::new(text).ui(ui, |ui| {
            ui.label(RichText::new("Sleep timer").strong());
            ui.label(RichText::new("Fades out, then pauses.").small().color(muted()));
            ui.add_space(4.0);
            for mins in [15u64, 30, 45, 60, 90] {
                if ui.button(format!("{mins} minutes")).clicked() {
                    pick = Some(Some(Sleep::At(Instant::now() + Duration::from_secs(mins * 60))));
                }
            }
            if let Some(id) = self.queue.current_id() {
                if ui.button("End of this track").clicked() {
                    pick = Some(Some(Sleep::EndOfTrack(id)));
                }
            }
            if self.sleep.is_some() {
                ui.separator();
                if ui.button("Turn off").clicked() {
                    pick = Some(None);
                }
            }
        })
        .0
        .on_hover_text("Sleep timer");
        if let Some(sleep) = pick {
            self.set_sleep(sleep);
        }
    }

    pub(super) fn set_speed(&mut self, speed: f32, save: bool) {
        self.speed = speed.clamp(0.5, 2.0);
        self.player.set_speed(self.speed);
        if save {
            let mut config = crate::config::Config::load();
            config.speed = self.speed;
            config.save();
        }
    }

    fn apply_eq(&mut self, save: bool) {
        self.player.set_eq(self.eq_on, self.eq_gains);
        if save {
            let mut config = crate::config::Config::load();
            config.eq_on = self.eq_on;
            config.eq_gains = self.eq_gains;
            config.save();
        }
    }

    /// Speed button ("1×") and its menu.
    fn speed_button(&mut self, ui: &mut egui::Ui) {
        let changed = (self.speed - 1.0).abs() > 0.001;
        let text = RichText::new(speed_label(self.speed)).size(13.0).color(if changed { accent() } else { muted() });
        let stay_open = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        let mut pick = None;
        egui::containers::menu::MenuButton::new(text)
            .config(stay_open)
            .ui(ui, |ui| {
                ui.set_min_width(230.0);
                ui.label(RichText::new("Playback speed").strong());
                ui.label(RichText::new("Pitch follows speed, like a record (Shift+< / Shift+>).").small().color(muted()));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    for (name, s) in SPEEDS {
                        if ui.selectable_label((self.speed - s).abs() < 0.001, name).clicked() {
                            pick = Some(s);
                        }
                    }
                });
                ui.add_space(4.0);
                let mut v = self.speed;
                ui.spacing_mut().slider_width = 160.0;
                let r = ui.add(
                    egui::Slider::new(&mut v, 0.5..=2.0)
                        .step_by(f64::from(SPEED_STEP))
                        .trailing_fill(true)
                        .custom_formatter(|v, _| speed_label(v as f32)),
                );
                // Changing speed reopens the audio stream: only on release.
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    pick = Some(v);
                } else if r.dragged() {
                    self.speed = v; // shown while dragging, applied on release
                }
            })
            .0
            .on_hover_text("Playback speed");
        if let Some(s) = pick {
            self.set_speed(s, true);
        }
    }

    /// "EQ" button and the equalizer menu.
    fn eq_button(&mut self, ui: &mut egui::Ui) {
        use crate::player::eq::{LABELS, PRESETS, RANGE, preset_name};
        let text = RichText::new("EQ").size(13.0).color(if self.eq_on { accent() } else { muted() });
        let stay_open = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        let (mut changed, mut save) = (false, false);
        egui::containers::menu::MenuButton::new(text)
            .config(stay_open)
            .ui(ui, |ui| {
                ui.set_min_width(300.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Equalizer").strong());
                    if ui.checkbox(&mut self.eq_on, "On").changed() {
                        changed = true;
                        save = true;
                    }
                });
                ui.add_space(4.0);
                // Presets (picking one also turns the EQ on).
                let current = preset_name(&self.eq_gains);
                ui.horizontal_wrapped(|ui| {
                    for (name, gains) in PRESETS {
                        if ui.selectable_label(current == Some(name), name).clicked() {
                            self.eq_gains = gains;
                            self.eq_on = true;
                            changed = true;
                            save = true;
                        }
                    }
                });
                ui.add_space(6.0);
                ui.add_enabled_ui(self.eq_on, |ui| {
                    ui.horizontal(|ui| {
                        for (i, label) in LABELS.iter().enumerate() {
                            ui.vertical(|ui| {
                                ui.set_width(42.0);
                                ui.spacing_mut().slider_width = 110.0;
                                let r = ui.add(
                                    egui::Slider::new(&mut self.eq_gains[i], -RANGE..=RANGE)
                                        .vertical()
                                        .step_by(0.5)
                                        .trailing_fill(true)
                                        .show_value(false),
                                );
                                if r.changed() {
                                    changed = true;
                                }
                                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                                    save = true;
                                }
                                let g = self.eq_gains[i];
                                ui.label(RichText::new(format!("{g:+.1}")).small());
                                ui.label(RichText::new(*label).small().color(muted()));
                            });
                        }
                    });
                });
                ui.label(
                    RichText::new(if current.is_none() { "Custom" } else { "Drag a slider to make your own." })
                        .small()
                        .color(muted()),
                );
            })
            .0
            .on_hover_text("Equalizer");
        if changed || save {
            self.apply_eq(save);
        }
    }

    pub(super) fn toggle_play(&mut self) {
        if !self.player.is_paused() {
            self.player.pause();
        } else if self.player.now().is_some() {
            self.player.resume();
        } else if let Some(item) = self.queue.current().cloned() {
            // Finished or stopped: start the current track again (or where
            // the last run left off).
            self.player.play(item, self.resume_at.take().unwrap_or(0.0));
            self.player.set_next(self.queue.peek_next());
        }
    }

    pub(super) fn seek_to(&mut self, to: f64) {
        let to = to.max(0.0);
        // Not started since launch: just move the resume point.
        if self.resume_at.is_some() && self.player.now().is_none() {
            self.resume_at = Some(to);
            return;
        }
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
        self.resume_at = None;
        if let Some(item) = self.queue.advance() {
            self.player.play(item, 0.0);
            self.player.set_next(self.queue.peek_next());
        }
    }

    pub(super) fn previous(&mut self) {
        self.resume_at = None;
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
                ui.label(RichText::new("Pick a track to start playing  ·  Space play/pause  ·  ←/→ seek  ·  ↑/↓ volume  ·  / search").color(muted()))
                    .on_hover_text(SHORTCUTS);
            });
            return;
        };
        let now = self.player.now().filter(|(id, _)| *id == item.id);
        let playing = !self.player.is_paused();
        // Visualizer behind the bar.
        if self.viz_style != super::visualizer::Style::Off {
            if playing {
                let (samples, rate) = self.player.visualizer_samples();
                self.viz.update(&samples, rate);
            } else {
                self.viz.update(&[], 0.0); // fall to rest
            }
            ui.ctx().request_repaint_after(Duration::from_millis(33));
            let rect = ui.max_rect().shrink2(vec2(0.0, 2.0));
            self.viz.draw(ui.painter(), rect, self.viz_style, accent().gamma_multiply(0.22));
        }
        // Show the requested position right after a seek until playback catches up.
        let mut pos = now.map_or(self.resume_at.unwrap_or(0.0), |(_, p)| p);
        if let Some((target, at)) = self.pending_seek {
            if at.elapsed() < Duration::from_millis(800) && (pos - target).abs() > 1.0 {
                pos = target;
            } else {
                self.pending_seek = None;
            }
        }
        let duration = item.track.as_ref().map_or(0.0, |t| t.duration as f64 / 1000.0);

        let mut like = None;
        let mut open_track = None;
        let full = ui.available_width();
        let side = (full * 0.28).clamp(180.0, 340.0);
        ui.horizontal(|ui| {
            // Now playing.
            ui.allocate_ui_with_layout(vec2(side, HEIGHT - 16.0), Layout::left_to_right(Align::Center), |ui| {
                // Always the full width, so the controls don't shift with
                // the title's length.
                ui.set_width(side);
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
                                .on_hover_text("Track details, related tracks and playlists")
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if title.clicked() {
                                open_track = Some(t.clone());
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
                ui.set_width(center);
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
                    let main = ui
                        .add(egui::Button::new("").corner_radius(16).min_size(vec2(36.0, 32.0)))
                        .on_hover_text(if playing { "Pause (Space)" } else { "Play (Space)" });
                    let color = ui.visuals().text_color();
                    super::rows::play_pause_icon(ui.painter(), main.rect.center(), playing, 0.85, color);
                    if main.clicked() {
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
                if ui
                    .add(egui::Button::new(RichText::new("Mini").size(12.0).color(muted())))
                    .on_hover_text("Mini player: a small always-on-top window")
                    .clicked()
                {
                    self.set_mini(true);
                }
                self.sleep_button(ui);
                self.eq_button(ui);
                self.speed_button(ui);
                ui.add_space(4.0);
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
        if let Some(track) = open_track {
            self.track_action(rows::TrackAction::OpenTrack, track);
        }
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
