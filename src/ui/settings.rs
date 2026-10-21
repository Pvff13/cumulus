//! The Settings page.

use eframe::egui::{self, RichText};

use super::App;
use super::theme::muted;

impl App {
    pub(super) fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.add_space(8.0);
        egui::ScrollArea::vertical().id_salt("settings").auto_shrink(false).show(ui, |ui| {
            ui.set_max_width(720.0);
            self.settings_sections(ui);
            ui.add_space(24.0);
        });
    }

    fn settings_sections(&mut self, ui: &mut egui::Ui) {
        section(ui, "Appearance", "", |ui| self.appearance_settings(ui));
        section(ui, "Playback", "", |ui| self.playback_settings(ui));
        section(ui, "System", "", |ui| self.system_settings(ui));
        section(ui, "Keyboard", "Click a key to change it, then press the new combination (Backspace clears, Esc cancels).", |ui| {
            self.hotkey_settings(ui)
        });
        section(ui, "Integrations", "", |ui| self.integration_settings(ui));
        section(ui, "Hiding", "Tracks, artists and words you never want to see.", |ui| self.hiding_settings(ui));
    }

    fn playback_settings(&mut self, ui: &mut egui::Ui) {
        if self.audio_devices.is_empty() {
            self.audio_devices = crate::player::Player::devices();
        }
        let mut save = false;
        ui.horizontal(|ui| {
            ui.label("Output device");
            let shown = self.audio_device.clone().unwrap_or_else(|| "System default".into());
            egui::ComboBox::from_id_salt("audio-device").width(300.0).selected_text(shown).show_ui(ui, |ui| {
                if ui.selectable_label(self.audio_device.is_none(), "System default").clicked() {
                    self.audio_device = None;
                    save = true;
                }
                for name in self.audio_devices.clone() {
                    if ui.selectable_label(self.audio_device.as_deref() == Some(&name), &name).clicked() {
                        self.audio_device = Some(name);
                        save = true;
                    }
                }
            });
            if ui.small_button("↻").on_hover_text("Look for devices again").clicked() {
                self.audio_devices = crate::player::Player::devices();
            }
        });
        if save {
            self.player.set_device(self.audio_device.clone());
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Stream quality");
            save |= ui.radio_value(&mut self.high_quality, true, "High (160 kbps AAC when available)").changed();
            save |= ui.radio_value(&mut self.high_quality, false, "Normal (96-128 kbps, less data)").changed();
        });
        self.player.set_high_quality(self.high_quality);
        ui.label(RichText::new("Applies from the next track.").small().color(muted()));

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Skip stuck tracks");
            let r = ui.add(
                egui::Slider::new(&mut self.skip_stuck_secs, 0..=60)
                    .step_by(5.0)
                    .custom_formatter(|v, _| if v == 0.0 { "Off".into() } else { format!("after {v} s") }),
            );
            save |= r.drag_stopped() || (r.changed() && !r.dragged());
        });
        ui.label(RichText::new("If a track stops moving (a stalled stream), move on to the next one.").small().color(muted()));

        if save {
            let mut c = crate::config::Config::load();
            c.audio_device = self.audio_device.clone();
            c.high_quality = self.high_quality;
            c.skip_stuck_secs = self.skip_stuck_secs;
            c.save();
        }
    }

    fn system_settings(&mut self, ui: &mut egui::Ui) {
        if self.desktop.has_tray() && ui.checkbox(&mut self.close_to_tray, "Close to tray, keep playing").changed() {
            let mut config = crate::config::Config::load();
            config.close_to_tray = self.close_to_tray;
            config.save();
        }
        let mut on = self.start_with_windows;
        if ui
            .checkbox(&mut on, "Start with Windows")
            .on_hover_text("Opens hidden in the tray when you sign in to Windows.")
            .changed()
        {
            match crate::autostart::set(on) {
                Ok(()) => self.start_with_windows = on,
                Err(e) => self.error(format!("Couldn't change start-up: {e}")),
            }
        }
        ui.label(RichText::new("Starts hidden in the tray; click the tray icon to open it.").small().color(muted()));
    }

    fn hotkey_settings(&mut self, ui: &mut egui::Ui) {
        use super::hotkeys::{self, Action};
        // A key press while recording becomes the new binding.
        if let Some((action, global)) = self.recording {
            let mut cancel = false;
            if let Some(key) = hotkeys::record(ui.ctx(), &mut cancel) {
                let key = key.unwrap_or_default();
                let map = if global { &mut self.bindings.global } else { &mut self.bindings.local };
                map.insert(action, key);
                self.recording = None;
                self.bindings.save();
                if global {
                    self.start_global_hotkeys();
                }
            } else if cancel {
                self.recording = None;
            }
        }
        egui::Grid::new("hotkeys").num_columns(3).spacing([18.0, 4.0]).striped(true).show(ui, |ui| {
            ui.label(RichText::new("Action").small().color(muted()));
            ui.label(RichText::new("In Cumulus").small().color(muted()));
            ui.label(RichText::new("Global (works in any app)").small().color(muted()));
            ui.end_row();
            for a in Action::ALL {
                ui.label(a.label());
                for global in [false, true] {
                    if global && !a.can_be_global() {
                        ui.label("");
                        continue;
                    }
                    let map = if global { &self.bindings.global } else { &self.bindings.local };
                    let current = map.get(&a).cloned().unwrap_or_default();
                    let text = if self.recording == Some((a, global)) {
                        "Press a key…".to_string()
                    } else if current.is_empty() {
                        "—".to_string()
                    } else {
                        hotkeys::pretty(&current)
                    };
                    if ui.add(egui::Button::new(text).min_size(egui::vec2(140.0, 0.0))).clicked() {
                        self.recording = Some((a, global));
                    }
                }
                ui.end_row();
            }
        });
        if ui.button("Reset to defaults").clicked() {
            self.bindings = super::hotkeys::Bindings::load(&crate::config::Config::default());
            self.bindings.save();
            self.start_global_hotkeys();
        }
    }

    fn integration_settings(&mut self, ui: &mut egui::Ui) {
        self.discord_toggle(ui);
        ui.add_space(8.0);
        self.scrobble_settings(ui);
        ui.add_space(8.0);
        ui.label(RichText::new("OBS / streaming").strong());
        let mut save = false;
        save |= ui.checkbox(&mut self.obs_text_on, "Write the playing song to a text file").changed();
        ui.add_enabled_ui(self.obs_text_on, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(&self.obs_text_path).small().color(muted())).truncate());
                if ui.button("Change…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().set_file_name("now-playing.txt").add_filter("Text", &["txt"]).save_file() {
                        self.obs_text_path = p.display().to_string();
                        save = true;
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Format");
                save |= ui.add(egui::TextEdit::singleline(&mut self.obs_text_format).desired_width(220.0)).lost_focus();
                ui.label(RichText::new("{artist} and {title} are filled in").small().color(muted()));
            });
            ui.label(RichText::new("In OBS: add a Text source, tick \"Read from file\" and pick this file.").small().color(muted()));
        });
        ui.add_space(6.0);
        if ui.checkbox(&mut self.obs_overlay_on, "Now-playing overlay for an OBS Browser source").changed() {
            let on = self.obs_overlay_on;
            self.set_obs_overlay(on);
            save = true;
        }
        if let Some(o) = &self.obs_overlay {
            let url = o.url();
            ui.horizontal(|ui| {
                ui.monospace(&url);
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(url.clone());
                }
                ui.hyperlink_to("Preview ↗", &url);
            });
            ui.label(
                RichText::new("In OBS: add a Browser source with this URL, about 600 × 120. Add ?hidepaused=1 to hide it while paused.")
                    .small()
                    .color(muted()),
            );
        }
        if save {
            let mut c = crate::config::Config::load();
            c.obs_text = self.obs_text_on;
            c.obs_text_path = Some(self.obs_text_path.clone());
            c.obs_text_format = self.obs_text_format.clone();
            c.obs_overlay = self.obs_overlay_on;
            c.obs_port = self.obs_port;
            c.save();
            self.obs_text = crate::obs::TextFile::default(); // rewrite with the new settings
        }
    }

    fn scrobble_settings(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Scrobbling").strong());
        ui.label(RichText::new("Sends what you listen to (after half a track, or 4 minutes) to your profile.").small().color(muted()));

        // Last.fm: approve Cumulus in the browser, then finish here.
        ui.horizontal(|ui| {
            ui.label("Last.fm");
            match (&self.lastfm_user, &self.lastfm_pending) {
                (Some(user), _) => {
                    ui.label(RichText::new(format!("Connected as {user}")).color(super::theme::accent()));
                    if ui.small_button("Disconnect").clicked() {
                        crate::scrobble::save_secret("lastfm-session", None);
                        self.lastfm_user = None;
                        let mut c = crate::config::Config::load();
                        c.lastfm_user = None;
                        c.save();
                        self.restart_scrobbler();
                    }
                }
                (None, Some(token)) => {
                    let token = token.clone();
                    ui.label(RichText::new("Approve Cumulus in your browser, then:").small());
                    if ui.button("Finish connecting").clicked() {
                        if let Some((key, secret)) = self.lastfm_app() {
                            match crate::scrobble::lastfm_finish(&self.api.agent(), &key, &secret, &token) {
                                Ok((session, user)) => {
                                    crate::scrobble::save_secret("lastfm-session", Some(&session));
                                    self.lastfm_user = Some(user.clone());
                                    self.lastfm_pending = None;
                                    let mut c = crate::config::Config::load();
                                    c.lastfm_user = Some(user);
                                    c.save();
                                    self.restart_scrobbler();
                                }
                                Err(e) => self.error(format!("Last.fm: {e}")),
                            }
                        }
                    }
                    if ui.small_button("Cancel").clicked() {
                        self.lastfm_pending = None;
                    }
                }
                (None, None) => {
                    let available = self.lastfm_app().is_some();
                    let r = ui.add_enabled(available, egui::Button::new("Connect…"));
                    if !available {
                        r.on_disabled_hover_text("Not set up in this build yet.");
                    } else if r.clicked() {
                        let (key, secret) = self.lastfm_app().unwrap_or_default();
                        match crate::scrobble::lastfm_start(&self.api.agent(), &key, &secret) {
                            Ok((token, url)) => {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                                self.lastfm_pending = Some(token);
                            }
                            Err(e) => self.error(format!("Last.fm: {e}")),
                        }
                    }
                }
            }
        });

        // ListenBrainz: paste the user token from listenbrainz.org/settings.
        ui.horizontal(|ui| {
            ui.label("ListenBrainz");
            match &self.listenbrainz_user {
                Some(user) => {
                    ui.label(RichText::new(format!("Connected as {user}")).color(super::theme::accent()));
                    if ui.small_button("Disconnect").clicked() {
                        crate::scrobble::save_secret("listenbrainz-token", None);
                        self.listenbrainz_user = None;
                        let mut c = crate::config::Config::load();
                        c.listenbrainz_user = None;
                        c.save();
                        self.restart_scrobbler();
                    }
                }
                None => {
                    ui.add(egui::TextEdit::singleline(&mut self.lb_token_input).password(true).hint_text("User token").desired_width(220.0));
                    if ui.button("Connect").clicked() && !self.lb_token_input.trim().is_empty() {
                        let token = self.lb_token_input.trim().to_string();
                        match crate::scrobble::listenbrainz_check(&self.api.agent(), &token) {
                            Ok(user) => {
                                crate::scrobble::save_secret("listenbrainz-token", Some(&token));
                                self.listenbrainz_user = Some(user.clone());
                                self.lb_token_input.clear();
                                let mut c = crate::config::Config::load();
                                c.listenbrainz_user = Some(user);
                                c.save();
                                self.restart_scrobbler();
                            }
                            Err(e) => self.error(e),
                        }
                    }
                    ui.hyperlink_to(RichText::new("Get your token ↗").small(), "https://listenbrainz.org/settings/");
                }
            }
        });
        if ui.checkbox(&mut self.scrobble_local, "Also scrobble my local files").changed() {
            let mut c = crate::config::Config::load();
            c.scrobble_local = self.scrobble_local;
            c.save();
        }
    }

    fn hiding_settings(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        ui.label(RichText::new("Hidden keywords").strong());
        ui.label(RichText::new("Any track, playlist or artist whose name contains one of these is hidden.").small().color(muted()));
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.keyword_input).hint_text("e.g. nightcore").desired_width(220.0));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.button("Add").clicked() || enter) && !self.keyword_input.trim().is_empty() {
                self.hide_rules.add_keyword(&self.keyword_input);
                self.keyword_input.clear();
                changed = true;
            }
        });
        let mut remove = None;
        ui.horizontal_wrapped(|ui| {
            for (i, k) in self.hide_rules.keywords.iter().enumerate() {
                if ui.button(format!("{k}  ✕")).on_hover_text("Remove").clicked() {
                    remove = Some(i);
                }
            }
        });
        if let Some(i) = remove {
            self.hide_rules.remove_keyword(i);
            changed = true;
        }

        ui.add_space(10.0);
        ui.label(RichText::new("Blocked artists").strong());
        if self.hide_rules.artists.is_empty() {
            ui.label(RichText::new("None. Right-click a track → Block artist.").small().color(muted()));
        }
        let mut unblock = None;
        for (i, a) in self.hide_rules.artists.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(&a.name);
                if ui.small_button("Unblock").clicked() {
                    unblock = Some(i);
                }
            });
        }
        if let Some(i) = unblock {
            self.hide_rules.unblock_artist(i);
            changed = true;
        }

        ui.add_space(10.0);
        ui.label(RichText::new("Not interested").strong());
        if self.hide_rules.tracks.is_empty() {
            ui.label(RichText::new("None. Right-click a track → Not interested.").small().color(muted()));
        }
        let mut unhide = None;
        for (i, (_, name)) in self.hide_rules.tracks.iter().enumerate().rev() {
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(name).truncate());
                if ui.small_button("Show again").clicked() {
                    unhide = Some(i);
                }
            });
        }
        if let Some(i) = unhide {
            self.hide_rules.unhide_track(i);
            changed = true;
        }
        if changed {
            self.hide_rules.save();
            self.rehide();
        }
    }
}

/// A titled settings block.
pub(super) fn section(ui: &mut egui::Ui, title: &str, about: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::CollapsingHeader::new(RichText::new(title).size(17.0).strong()).default_open(true).show(ui, |ui| {
        if !about.is_empty() {
            ui.label(RichText::new(about).small().color(muted()));
            ui.add_space(4.0);
        }
        body(ui);
    });
    ui.add_space(10.0);
}
