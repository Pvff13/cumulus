//! Listen along: host a session friends can join (with a code, or from your
//! Discord status), or follow someone else's. See crate::listen for the
//! network side.

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use super::theme::{accent, muted};
use super::{App, Msg};
use crate::api::models::{Availability, Track};
use crate::api::ApiError;
use crate::listen::{self, Event, State};
use crate::player::QueueItem;

/// The relay Cumulus uses unless config.json's `listen_server` says
/// otherwise (server/listen-along, on Cloudflare).
const DEFAULT_SERVER: Option<&str> = Some("wss://cumulus-listen.cumulus-app.workers.dev");

/// Most people in a session, host included (the relay allows 20 listeners).
pub(super) const MAX_PEOPLE: usize = 21;

/// A public id for a session on Discord, which doesn't give away its code.
pub(super) fn party_id(room: &str) -> String {
    use md5::{Digest, Md5};
    format!("{:x}", Md5::digest(format!("cumulus-party:{room}").as_bytes()))
}

/// How far a listener may drift from the host before it seeks.
const MAX_DRIFT_SECS: f64 = 2.5;
/// A host re-sends where it is this often even when nothing changed.
/// Changes (skips, pauses, seeks, stalls) go out at once anyway, and
/// listeners check themselves every second, so this can be long: the
/// relay's rooms can sleep in between, which keeps Cloudflare usage down.
const HEARTBEAT: Duration = Duration::from_secs(30);

pub(super) enum Along {
    Off,
    Hosting {
        host: listen::Host,
        room: String,
        listeners: usize,
        connected: bool,
        /// What was sent last, and when.
        sent: Option<(State, Instant)>,
    },
    Following {
        _listener: listen::Listener,
        connected: bool,
        /// The host's latest state and when it arrived.
        target: Option<(State, Instant)>,
        /// A track being looked up to play.
        loading: Option<u64>,
        /// A track that couldn't be played (DRM, failed): not retried.
        skip: Option<u64>,
        /// Last drift check.
        checked: Instant,
    },
}

impl Along {
    pub(super) fn room(&self) -> Option<&str> {
        match self {
            Along::Hosting { room, .. } => Some(room),
            _ => None,
        }
    }

    pub(super) fn listeners(&self) -> usize {
        match self {
            Along::Hosting { listeners, .. } => *listeners,
            _ => 0,
        }
    }
}

/// Where the host's position is now, given what it sent and when.
fn position_now(s: State, at: Instant) -> f64 {
    if s.playing { s.position + at.elapsed().as_secs_f64() } else { s.position }
}

pub(super) fn server() -> Option<String> {
    #[cfg(debug_assertions)]
    if let Ok(s) = std::env::var("CUMULUS_DEV_LISTEN_SERVER") {
        return Some(s);
    }
    crate::config::Config::load()
        .listen_server
        .filter(|s| s.starts_with("wss://") || s.starts_with("ws://"))
        .or_else(|| DEFAULT_SERVER.map(String::from))
}

impl App {
    fn along_sender(&self) -> impl Fn(Event) + Send + 'static {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        move |e| {
            let _ = tx.send(Msg::Along(e));
            ctx.request_repaint();
        }
    }

    pub(super) fn start_hosting(&mut self) {
        let Some(server) = server() else { return self.error("Listen along isn't set up: there's no server.") };
        let (room, key) = listen::new_room();
        let host = listen::Host::start(&server, &room, &key, self.along_sender());
        self.along = Along::Hosting { host, room, listeners: 0, connected: false, sent: None };
    }

    pub(super) fn join_session(&mut self, code: &str) {
        let Some(room) = listen::parse_code(code) else {
            return self.error("That doesn't look like a listen-along code.");
        };
        let Some(server) = server() else { return self.error("Listen along isn't set up: there's no server.") };
        if self.along.room() == Some(room.as_str()) {
            return self.info("That's your own session.");
        }
        let listener = listen::Listener::join(&server, &room, self.along_sender());
        self.along = Along::Following { _listener: listener, connected: false, target: None, loading: None, skip: None, checked: Instant::now() };
        self.info("Joining the listening session…");
    }

    /// Ends hosting (for everyone) or stops following.
    pub(super) fn stop_along(&mut self) {
        let was = std::mem::replace(&mut self.along, Along::Off);
        match was {
            Along::Hosting { .. } => self.info("Listening session ended."),
            Along::Following { .. } => self.info("Stopped listening along."),
            Along::Off => {}
        }
    }

    pub(super) fn along_event(&mut self, e: Event) {
        match (&mut self.along, e) {
            (Along::Hosting { listeners, .. }, Event::Listeners(n)) => {
                let joined = n > *listeners;
                *listeners = n;
                if joined {
                    self.info("Someone joined your listening session.");
                }
            }
            (Along::Hosting { connected, sent, .. }, Event::Connected) => {
                *connected = true;
                *sent = None; // re-send where we are
            }
            (Along::Following { connected, .. }, Event::Connected) => *connected = true,
            (Along::Following { target, .. }, Event::State(s, at)) => {
                *target = Some((s, at));
                self.follow();
            }
            (Along::Off, _) => {}
            (Along::Following { .. }, Event::Ended) => {
                self.along = Along::Off;
                self.info("The listening session has ended.");
            }
            (_, Event::Ended) => self.along = Along::Off,
            (_, Event::Failed(why)) => {
                self.along = Along::Off;
                self.error(format!("Listen along stopped: {why}"));
            }
            _ => {}
        }
    }

    /// Every frame: a host sends its playback when it changes (and now and
    /// then anyway); a listener needs nothing (it follows each update).
    pub(super) fn along_tick(&mut self) {
        #[cfg(debug_assertions)]
        self.along_dev();
        // Listener: re-check every second, so a track that took a moment
        // to start is brought in step straight away.
        if let Along::Following { checked, .. } = &mut self.along {
            if checked.elapsed() >= Duration::from_secs(1) {
                *checked = Instant::now();
                self.follow();
            }
            self.ctx.request_repaint_after(Duration::from_secs(1));
            return;
        }
        let now = self.player.now();
        let local = self.queue.current().and_then(|i| i.track.as_ref()).is_some_and(|t| t.local_path.is_some());
        let paused = self.player.is_paused();
        let Along::Hosting { host, sent, connected: true, .. } = &mut self.along else { return };
        // Your own files can't be shared: listeners see "nothing playing".
        let state = State {
            track: now.map(|n| n.0).filter(|_| !local),
            position: now.map_or(0.0, |n| n.1),
            playing: now.is_some() && !paused && !local,
        };
        let changed = match *sent {
            None => true,
            Some((s, at)) => {
                s.track != state.track
                    || s.playing != state.playing
                    || (position_now(s, at) - state.position).abs() > MAX_DRIFT_SECS
                    || at.elapsed() > HEARTBEAT
            }
        };
        if changed {
            host.send(state);
            *sent = Some((state, Instant::now()));
        }
    }

    /// Debug builds, for tests: CUMULUS_DEV_ALONG=host (writes the code to
    /// CUMULUS_DEV_ALONG_FILE) or join:<code>, once signed in.
    #[cfg(debug_assertions)]
    fn along_dev(&mut self) {
        // CUMULUS_DEV_ALONG_JUMP=<at>:<to>[:pause]: once the host reaches
        // <at> s, seek to <to> (and pause), to test that listeners follow.
        if let (Along::Hosting { .. }, Ok(jump)) = (&self.along, std::env::var("CUMULUS_DEV_ALONG_JUMP")) {
            let parts: Vec<&str> = jump.split(':').collect();
            if let (Some(at), Some(to)) = (parts.first().and_then(|s| s.parse::<f64>().ok()), parts.get(1).and_then(|s| s.parse().ok())) {
                if self.player.now().is_some_and(|(_, pos)| pos >= at) {
                    // SAFETY: only read back on this (UI) thread.
                    unsafe { std::env::remove_var("CUMULUS_DEV_ALONG_JUMP") };
                    eprintln!("along-dev: jump to {to}");
                    self.seek_to(to);
                    if parts.get(2) == Some(&"pause") {
                        self.player.pause();
                    }
                }
            }
        }
        if self.me == 0 || !matches!(self.along, Along::Off) {
            return;
        }
        let Some(how) = std::env::var("CUMULUS_DEV_ALONG").ok() else { return };
        // SAFETY: only read back on this (UI) thread.
        unsafe { std::env::remove_var("CUMULUS_DEV_ALONG") };
        if how == "host" {
            self.start_hosting();
            if let (Some(room), Ok(file)) = (self.along.room(), std::env::var("CUMULUS_DEV_ALONG_FILE")) {
                let _ = std::fs::write(file, room);
            }
        } else if let Some(code) = how.strip_prefix("join:") {
            self.join_session(code);
        }
    }

    /// Listener: make this player match the host.
    fn follow(&mut self) {
        let Along::Following { target: Some((s, at)), loading, skip, .. } = &mut self.along else { return };
        let (s, at) = (*s, *at);
        if s.track.is_some() && s.track == *skip {
            return;
        }
        let Some(id) = s.track else {
            if !self.player.is_paused() {
                self.player.pause();
            }
            return;
        };
        let now = self.player.now();
        if now.map(|n| n.0) != Some(id) {
            if *loading != Some(id) {
                *loading = Some(id);
                self.spawn(move |api, send| send(Msg::AlongTrack(api.track(id))));
            }
            return;
        }
        if s.playing && self.player.is_paused() {
            self.player.resume();
        } else if !s.playing && !self.player.is_paused() {
            self.player.pause();
        }
        let want = position_now(s, at);
        if now.is_some_and(|(_, pos)| (pos - want).abs() > MAX_DRIFT_SECS) {
            self.seek_to(want);
        }
    }

    pub(super) fn along_track(&mut self, result: Result<Track, ApiError>) {
        let Along::Following { target: Some((s, at)), loading, skip, .. } = &mut self.along else { return };
        let (s, at) = (*s, *at);
        let wanted = loading.take();
        let track = match result {
            Ok(t) if Some(t.id) == wanted && s.track == wanted => t,
            Ok(_) => return self.follow(), // the host moved on meanwhile
            Err(e) => {
                *skip = wanted;
                return self.error(format!("Couldn't load the track your friend is playing: {e}"));
            }
        };
        if track.availability() == Availability::Drm {
            *skip = wanted;
            self.player.pause();
            return self.error(format!("“{}” is DRM-protected, so it can't play here.", track.title));
        }
        let item = QueueItem { id: track.id, track: Some(Arc::new(track)) };
        self.queue.set(vec![item.clone()], 0);
        self.player.play(item, position_now(s, at));
        self.player.set_next(None);
        if !s.playing {
            self.player.pause();
        }
    }

    pub(super) fn discord_event(&mut self, e: crate::discord::Incoming) {
        use crate::discord::Incoming;
        match e {
            // You clicked Join on a friend's Discord status.
            Incoming::Join(code) => self.join_session(&code),
            Incoming::JoinRequest { user_id, name } => {
                if self.along.room().is_some() && !self.join_requests.iter().any(|(id, _)| *id == user_id) {
                    self.join_requests.push((user_id, name));
                }
            }
        }
    }

    /// "<name> wants to listen along" with Accept / Decline, while hosting.
    pub(super) fn join_requests_ui(&mut self, ui: &mut egui::Ui) {
        if self.along.room().is_none() {
            self.join_requests.clear();
            return;
        }
        let mut answer = None;
        for (i, (_, name)) in self.join_requests.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("👥 {name} wants to listen along")).color(accent()));
                if ui.small_button("Accept").clicked() {
                    answer = Some((i, true));
                }
                if ui.small_button("Decline").clicked() {
                    answer = Some((i, false));
                }
            });
        }
        if let Some((i, accept)) = answer {
            let (user_id, _) = self.join_requests.remove(i);
            if let Some(tx) = &self.discord {
                let _ = tx.send(crate::discord::Request::Answer { user_id, accept });
            }
        }
    }

    /// The 👥 button in the player bar and its menu.
    pub(super) fn along_button(&mut self, ui: &mut egui::Ui) {
        let active = !matches!(self.along, Along::Off);
        let label = match &self.along {
            Along::Hosting { listeners, .. } if *listeners > 0 => format!("👥 {listeners}"),
            _ => "👥".to_string(),
        };
        let text = RichText::new(label).size(15.0).color(if active { accent() } else { muted() });
        let stay_open = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        let mut action: Option<AlongAction> = None;
        egui::containers::menu::MenuButton::new(text)
            .config(stay_open)
            .ui(ui, |ui| {
                ui.set_min_width(300.0);
                ui.label(RichText::new("Listen along").strong());
                if server().is_none() {
                    ui.label(RichText::new("Not set up: there's no listen-along server.").small().color(muted()));
                    return;
                }
                match &self.along {
                    Along::Off => {
                        ui.label(
                            RichText::new("Friends hear what you play, in step with you. They need Cumulus too.")
                                .small()
                                .color(muted()),
                        );
                        if ui.button("Start a listening session").clicked() {
                            action = Some(AlongAction::Host);
                        }
                        ui.separator();
                        ui.label("Join a friend's session:");
                        ui.horizontal(|ui| {
                            let resp = ui.add(egui::TextEdit::singleline(&mut self.along_code).hint_text("paste their code").desired_width(200.0));
                            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if (ui.button("Join").clicked() || enter) && !self.along_code.trim().is_empty() {
                                action = Some(AlongAction::Join);
                            }
                        });
                    }
                    Along::Hosting { room, listeners, connected, .. } => {
                        if !connected {
                            ui.label(RichText::new("Connecting…").small().color(muted()));
                        }
                        ui.label("Your code (send it to a friend):");
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(room).monospace().small());
                            if ui.small_button("Copy").clicked() {
                                ui.ctx().copy_text(room.clone());
                                action = Some(AlongAction::Copied);
                            }
                        });
                        let who = match listeners {
                            0 => "Nobody's listening yet.".to_string(),
                            1 => "1 person listening along.".to_string(),
                            n => format!("{n} people listening along."),
                        };
                        ui.label(RichText::new(who).color(muted()));
                        if self.discord_on {
                            ui.label(RichText::new("Friends can also join from your Discord status.").small().color(muted()));
                        }
                        if ui.button("End session").clicked() {
                            action = Some(AlongAction::Stop);
                        }
                    }
                    Along::Following { connected, .. } => {
                        let what = if *connected { "You're listening along." } else { "Connecting…" };
                        ui.label(RichText::new(what).color(muted()));
                        if ui.button("Leave").clicked() {
                            action = Some(AlongAction::Stop);
                        }
                    }
                }
            })
            .0
            .on_hover_text("Listen along with friends");
        match action {
            Some(AlongAction::Host) => self.start_hosting(),
            Some(AlongAction::Join) => {
                let code = std::mem::take(&mut self.along_code);
                self.join_session(&code);
            }
            Some(AlongAction::Stop) => self.stop_along(),
            Some(AlongAction::Copied) => self.info("Code copied."),
            None => {}
        }
    }

    /// A line at the top while following someone.
    pub(super) fn along_banner(&mut self, ui: &mut egui::Ui) {
        if !matches!(self.along, Along::Following { .. }) {
            return;
        }
        let mut leave = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new("👥 Listening along with a friend").color(accent()));
            leave = ui.small_button("Leave").clicked();
        });
        if leave {
            self.stop_along();
        }
    }
}

enum AlongAction {
    Host,
    Join,
    Stop,
    Copied,
}
