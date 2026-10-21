//! Home: SoundCloud's personalised home page (your mixes, Daily Drops,
//! stations, recommended albums...) as rows of tiles, in SoundCloud's order.

use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, Align, Color32, CursorIcon, Layout, RichText, Sense, Stroke, vec2};

use super::library::PlaylistView;
use super::rows::{self, TrackAction};
use super::theme::{accent, hover, muted};
use super::{App, Msg, Page, status_line};
use crate::api::models::{HomeItem, Page as ApiPage, Playlist, Selection, SystemPlaylist, Track, TrackLike};
use crate::api::{Api, ApiError, client};
use crate::cache::meta;
use crate::player::QueueItem;

const TILE: f32 = 150.0;

#[derive(Default)]
pub struct HomeState {
    pub sections: Vec<Selection>,
    /// What you actually played recently (shown as "Recently Played").
    pub history: Vec<Arc<Track>>,
    /// Playlists, albums and mixes you played recently.
    pub continue_items: Vec<HomeItem>,
    /// New uploads (not reposts) from people you follow, newest first.
    pub fresh: Vec<Arc<Track>>,
    pub loading: bool,
    pub error: Option<String>,
    pub started: bool,
    pub from_cache: bool,
}

enum HomeAction {
    OpenMix(SystemPlaylist),
    PlayMix(SystemPlaylist),
    /// Pause/resume the mix that's already playing.
    TogglePlay,
    OpenPlaylist(Arc<Playlist>),
    /// Play the recently-played list from this position.
    PlayHistory(usize),
    /// Play the fresh-drops list from this position.
    PlayFresh(usize),
    Track(TrackAction, Arc<Track>),
}

/// Your personal mixes and "Made for you" (Daily Drops) first; everything
/// else keeps SoundCloud's order.
/// Playlists/albums/mixes played recently ("Continue listening").
fn fetch_continue(api: &Api) -> Result<Vec<HomeItem>, ApiError> {
    /// Each entry wraps the playlist or mix that was played.
    #[derive(serde::Deserialize)]
    struct Context {
        #[serde(default)]
        playlist: Option<Playlist>,
        #[serde(default)]
        system_playlist: Option<SystemPlaylist>,
    }
    let page: ApiPage<Context> = api.get("/me/play-history/contexts?limit=12")?;
    let mut seen = std::collections::HashSet::new();
    Ok(page
        .collection
        .into_iter()
        .filter_map(|c| c.system_playlist.map(HomeItem::Mix).or(c.playlist.map(HomeItem::Playlist)))
        .filter(|it| match it {
            HomeItem::Mix(m) => seen.insert(m.urn.clone()),
            HomeItem::Playlist(p) => seen.insert(p.id.to_string()),
            HomeItem::Other => false,
        })
        .collect())
}

/// Days since 1970-01-01 for an ISO date ("2026-10-21T08:30:54Z").
fn days_from_iso(iso: &str) -> Option<i64> {
    let mut p = iso.get(..10)?.split('-');
    let (y, m, d): (i64, i64, i64) = (p.next()?.parse().ok()?, p.next()?.parse().ok()?, p.next()?.parse().ok()?);
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// "Fresh drops": tracks people you follow uploaded in the last two weeks.
fn fetch_fresh(api: &Api) -> Result<Vec<Arc<Track>>, ApiError> {
    let page: ApiPage<crate::api::models::Activity> = api.get("/stream?limit=100")?;
    let today = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64 / 86_400);
    let mut seen = std::collections::HashSet::new();
    Ok(page
        .collection
        .into_iter()
        .filter(|a| a.kind == "track") // not "track-repost"
        .filter_map(|a| a.track)
        .filter(|t| {
            let date = t.created_at.as_deref().or(t.display_date.as_deref());
            date.and_then(days_from_iso).is_some_and(|d| today - d <= 14)
        })
        .filter(|t| seen.insert(t.id))
        .take(20)
        .map(Arc::new)
        .collect())
}

/// Recently played tracks, newest first, each listed once.
fn fetch_history(api: &Api) -> Result<Vec<Arc<Track>>, ApiError> {
    let page: ApiPage<TrackLike> = api.get(&Api::play_history_url())?;
    let mut seen = std::collections::HashSet::new();
    Ok(page
        .collection
        .into_iter()
        .filter_map(|h| h.track)
        .filter(|t| seen.insert(t.id))
        .map(Arc::new)
        .collect())
}

fn parse(body: &str) -> Result<Vec<Selection>, ApiError> {
    let page: ApiPage<Selection> = client::parse(body)?;
    let mut sections = page.collection;
    // SoundCloud sometimes repeats the same tile many times in a row.
    for s in &mut sections {
        let mut seen = std::collections::HashSet::new();
        s.items.collection.retain(|it| match it {
            HomeItem::Mix(m) => seen.insert(m.urn.clone()),
            HomeItem::Playlist(p) => seen.insert(p.id.to_string()),
            HomeItem::Other => false,
        });
    }
    let rank = |s: &Selection| match () {
        _ if s.urn.contains(":your-moods") => 0,
        _ if s.urn.contains(":made-for-you") => 1,
        _ => 2,
    };
    sections.sort_by_key(rank);
    Ok(sections)
}

impl App {
    fn load_home(&mut self) {
        let key = format!("home-{}", self.me);
        let show_saved = self.home.sections.is_empty();
        self.home.started = true;
        self.home.loading = true;
        self.spawn(move |api, send| {
            if show_saved {
                if let Some(sections) = meta::load(&key).and_then(|b| parse(&b).ok()) {
                    send(Msg::Home { cached: true, result: Ok(sections) });
                }
            }
            let result = api.get_raw(&Api::home_url()).and_then(|body| {
                let sections = parse(&body)?;
                meta::save(&key, &body);
                Ok(sections)
            });
            send(Msg::Home { cached: false, result });
            send(Msg::History(fetch_history(api)));
            send(Msg::HomeExtras { continue_items: fetch_continue(api).unwrap_or_default(), fresh: fetch_fresh(api).unwrap_or_default() });
        });
    }

    pub(super) fn home_loaded(&mut self, cached: bool, result: Result<Vec<Selection>, ApiError>) {
        let home = &mut self.home;
        match result {
            Ok(sections) => {
                home.sections = sections;
                home.from_cache = cached;
                home.error = None;
                if !cached {
                    home.loading = false;
                }
            }
            Err(e) => {
                home.loading = false;
                home.error = Some(match (&e, home.sections.is_empty()) {
                    (ApiError::Offline(_), false) => "Offline - showing your saved home page".into(),
                    _ => e.to_string(),
                });
            }
        }
    }

    pub(super) fn home_ui(&mut self, ui: &mut egui::Ui) {
        if !self.home.started {
            self.load_home();
        }
        let mut refresh = false;
        let mut menu = super::overlay_ui::MenuResult::default();
        ui.horizontal(|ui| {
            ui.heading("Home");
            if self.home.loading {
                ui.spinner();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                refresh = ui.add_enabled(!self.home.loading, egui::Button::new("↻")).on_hover_text("Refresh").clicked();
                menu = super::overlay_ui::overlay_menu(ui, &mut self.overlay_menu);
            });
        });
        status_line(ui, self.home.error.as_deref(), self.home.from_cache && self.home.loading);
        ui.add_space(6.0);

        let mut action = None;
        let liked = &self.liked;
        // The mix the queue came from, and whether it's playing (not paused).
        let playing = self.playing_list.map(|id| (id, !self.player.is_paused() && self.player.now().is_some()));
        egui::ScrollArea::vertical().id_salt("home").auto_shrink(false).show(ui, |ui| {
            if !self.home.continue_items.is_empty() {
                ui.label(RichText::new("Continue listening").size(18.0).strong());
                ui.add_space(4.0);
                egui::ScrollArea::horizontal().id_salt("home-continue").show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        for item in &self.home.continue_items {
                            if let Some(a) = tile(ui, &self.art, item, playing) {
                                action = Some(a);
                            }
                        }
                    });
                });
                ui.add_space(16.0);
            }
            if !self.home.fresh.is_empty() {
                ui.label(RichText::new("Fresh drops").size(18.0).strong())
                    .on_hover_text("New uploads from people you follow, from the last two weeks");
                ui.add_space(4.0);
                egui::ScrollArea::horizontal().id_salt("home-fresh").show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        for (n, t) in self.home.fresh.iter().enumerate() {
                            if let Some(a) = track_tile(ui, &self.art, liked, t, n, HomeAction::PlayFresh) {
                                action = Some(a);
                            }
                        }
                    });
                });
                ui.add_space(16.0);
            }
            for (i, section) in self.home.sections.iter().enumerate() {
                // "Recently Played": the songs themselves, each with its own cover.
                if section.urn.contains(":recently-played") && !self.home.history.is_empty() {
                    ui.label(RichText::new(&section.title).size(18.0).strong());
                    ui.add_space(4.0);
                    egui::ScrollArea::horizontal().id_salt(("home-row", i)).show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 14.0;
                            for (n, t) in self.home.history.iter().enumerate() {
                                if let Some(a) = track_tile(ui, &self.art, liked, t, n, HomeAction::PlayHistory) {
                                    action = Some(a);
                                }
                            }
                        });
                    });
                    ui.add_space(16.0);
                    continue;
                }
                let items: Vec<&HomeItem> =
                    section.items.collection.iter().filter(|it| !matches!(it, HomeItem::Other)).collect();
                if items.is_empty() {
                    continue;
                }
                ui.label(RichText::new(&section.title).size(18.0).strong());
                ui.add_space(4.0);
                egui::ScrollArea::horizontal().id_salt(("home-row", i)).show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        for item in items {
                            if let Some(a) = tile(ui, &self.art, item, playing) {
                                action = Some(a);
                            }
                        }
                    });
                });
                ui.add_space(16.0);
            }
        });

        // Debug builds: CUMULUS_DEV_OPEN=1 opens the first mix once, for screenshots.
        #[cfg(debug_assertions)]
        if action.is_none() && std::env::var_os("CUMULUS_DEV_OPEN").is_some() {
            if let Some(m) = self.home.sections.iter().flat_map(|s| &s.items.collection).find_map(|it| match it {
                HomeItem::Mix(m) => Some(m.clone()),
                _ => None,
            }) {
                // SAFETY: only the UI thread reads this dev-only flag.
                unsafe { std::env::remove_var("CUMULUS_DEV_OPEN") };
                action = Some(HomeAction::OpenMix(m));
            }
        }
        match action {
            Some(HomeAction::OpenMix(m)) => {
                let view = PlaylistView::from_ids(Arc::new(m.as_playlist()), m.track_ids());
                self.push_page(Page::Playlist(view));
            }
            Some(HomeAction::PlayMix(m)) => {
                let items = m.track_ids().into_iter().map(|id| QueueItem { id, track: None }).collect();
                self.play_list(items, 0);
                self.playing_list = Some(m.local_id());
                self.status = Some(super::Status { text: format!("Playing {}", m.title), at: Instant::now(), error: false });
            }
            Some(HomeAction::TogglePlay) => self.toggle_play(),
            Some(HomeAction::OpenPlaylist(p)) => self.open_playlist(p),
            Some(HomeAction::PlayHistory(n)) => {
                let items = self.home.history.iter().map(|t| QueueItem { id: t.id, track: Some(t.clone()) }).collect();
                self.play_list(items, n);
            }
            Some(HomeAction::PlayFresh(n)) => {
                let items = self.home.fresh.iter().map(|t| QueueItem { id: t.id, track: Some(t.clone()) }).collect();
                self.play_list(items, n);
            }
            Some(HomeAction::Track(a, t)) => self.track_action(a, t),
            None => {}
        }
        self.handle_menu(menu);
        if refresh {
            self.load_home();
        }
    }
}

/// A song tile (Recently Played): its own artwork; click plays it,
/// right-click gives the usual track menu.
fn track_tile(
    ui: &mut egui::Ui,
    art: &crate::cache::art::ArtCache,
    liked: &std::collections::HashSet<u64>,
    track: &Arc<Track>,
    index: usize,
    play: fn(usize) -> HomeAction,
) -> Option<HomeAction> {
    let mut action = None;
    ui.allocate_ui_with_layout(vec2(TILE, TILE + 46.0), Layout::top_down(Align::Min), |ui| {
        ui.set_width(TILE);
        let img = rows::artwork(ui, art, track.artwork(), TILE, "t300x300")
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("Play");
        if ui.rect_contains_pointer(img.rect) {
            let center = img.rect.center();
            ui.painter().rect_filled(img.rect, 4.0, hover().gamma_multiply(0.35));
            ui.painter().circle_filled(center, 24.0, Color32::from_black_alpha(200));
            rows::play_pause_icon(ui.painter(), center, false, 1.0, Color32::WHITE);
        }
        if img.clicked() {
            action = Some(if track.availability() == crate::api::models::Availability::Drm {
                HomeAction::Track(TrackAction::OpenOnSoundCloud, track.clone())
            } else {
                play(index)
            });
        }
        img.context_menu(|ui| {
            if let Some(a) = rows::track_menu(ui, track, liked.contains(&track.id), false) {
                action = Some(if a == TrackAction::Play { play(index) } else { HomeAction::Track(a, track.clone()) });
            }
        });
        ui.add_space(4.0);
        let name = ui
            .add(egui::Label::new(RichText::new(&track.title).strong()).truncate().selectable(false).sense(Sense::click()))
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("Track details, related tracks and playlists");
        if name.hovered() {
            ui.painter().hline(name.rect.x_range(), name.rect.bottom(), Stroke::new(1.0, ui.visuals().text_color()));
        }
        if name.clicked() {
            action = Some(HomeAction::Track(TrackAction::OpenTrack, track.clone()));
        }
        ui.add(egui::Label::new(RichText::new(&track.user.username).small().color(muted())).truncate().selectable(false));
    });
    action
}

/// One tile: artwork, title, subtitle. Mixes get a ▶ while hovered; the
/// mix that's playing keeps a ⏸ (▶ while paused) that pauses/resumes it.
fn tile(
    ui: &mut egui::Ui,
    art: &crate::cache::art::ArtCache,
    item: &HomeItem,
    playing: Option<(u64, bool)>,
) -> Option<HomeAction> {
    let (title, subtitle, artwork, mix) = match item {
        HomeItem::Mix(m) => (m.title.as_str(), m.subtitle().to_string(), m.artwork(), Some(m)),
        HomeItem::Playlist(p) => (p.title.as_str(), p.user.username.clone(), p.artwork(), None),
        HomeItem::Other => return None,
    };
    let mut action = None;
    ui.allocate_ui_with_layout(vec2(TILE, TILE + 46.0), Layout::top_down(Align::Min), |ui| {
        ui.set_width(TILE);
        let img = rows::artwork(ui, art, artwork, TILE, "t300x300").on_hover_cursor(CursorIcon::PointingHand);
        let hovered = ui.rect_contains_pointer(img.rect);
        let play_center = img.rect.center();
        let on_play = mix.is_some()
            && hovered
            && ui.pointer_hover_pos().is_some_and(|p| p.distance(play_center) <= 24.0);
        // Some(is playing) when this tile is the mix the queue came from.
        let active = mix.and_then(|m| playing.filter(|(id, _)| *id == m.local_id()).map(|(_, p)| p));
        if hovered {
            ui.painter().rect_filled(img.rect, 4.0, hover().gamma_multiply(0.35));
        }
        if mix.is_some() && (hovered || active.is_some()) {

            let fill = if active.is_some() { accent() } else { Color32::from_black_alpha(if on_play { 230 } else { 170 }) };
            let fill = if active.is_some() && on_play { fill.gamma_multiply(1.2) } else { fill };
            ui.painter().circle_filled(play_center, 24.0, fill);
            rows::play_pause_icon(ui.painter(), play_center, active == Some(true), 1.0, Color32::WHITE);
        }
        let open = || match item {
            HomeItem::Mix(m) => Some(HomeAction::OpenMix(m.clone())),
            HomeItem::Playlist(p) => Some(HomeAction::OpenPlaylist(Arc::new(p.clone()))),
            HomeItem::Other => None,
        };
        if img.clicked() {
            action = match mix {
                Some(_) if on_play && active.is_some() => Some(HomeAction::TogglePlay),
                Some(m) if on_play => Some(HomeAction::PlayMix(m.clone())),
                _ => open(),
            };
        }
        ui.add_space(4.0);
        // The title opens the mix/playlist too (underlined on hover).
        let name = ui
            .add(egui::Label::new(RichText::new(title).strong()).truncate().selectable(false).sense(Sense::click()))
            .on_hover_cursor(CursorIcon::PointingHand);
        if name.hovered() {
            ui.painter().hline(name.rect.x_range(), name.rect.bottom(), Stroke::new(1.0, ui.visuals().text_color()));
        }
        if name.clicked() {
            action = open();
        }
        ui.add(egui::Label::new(RichText::new(subtitle).small().color(muted())).truncate().selectable(false));
    });
    action
}

