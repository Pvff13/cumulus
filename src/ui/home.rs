//! Home: SoundCloud's personalised home page (your mixes, Daily Drops,
//! stations, recommended albums...) as rows of tiles, in SoundCloud's order.

use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, Align, Align2, Color32, CursorIcon, FontId, Layout, RichText, vec2};

use super::library::PlaylistView;
use super::rows::{self, TrackAction};
use super::theme::{hover, muted};
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
    pub loading: bool,
    pub error: Option<String>,
    pub started: bool,
    pub from_cache: bool,
}

enum HomeAction {
    OpenMix(SystemPlaylist),
    PlayMix(SystemPlaylist),
    OpenPlaylist(Arc<Playlist>),
    /// Play the recently-played list from this position.
    PlayHistory(usize),
    Track(TrackAction, Arc<Track>),
}

/// Your personal mixes and "Made for you" (Daily Drops) first; everything
/// else keeps SoundCloud's order.
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
        egui::ScrollArea::vertical().id_salt("home").auto_shrink(false).show(ui, |ui| {
            for (i, section) in self.home.sections.iter().enumerate() {
                // "Recently Played": the songs themselves, each with its own cover.
                if section.urn.contains(":recently-played") && !self.home.history.is_empty() {
                    ui.label(RichText::new(&section.title).size(18.0).strong());
                    ui.add_space(4.0);
                    egui::ScrollArea::horizontal().id_salt(("home-row", i)).show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 14.0;
                            for (n, t) in self.home.history.iter().enumerate() {
                                if let Some(a) = track_tile(ui, &self.art, liked, t, n) {
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
                            if let Some(a) = tile(ui, &self.art, item) {
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
                self.pages.push(Page::Playlist(view));
            }
            Some(HomeAction::PlayMix(m)) => {
                let items = m.track_ids().into_iter().map(|id| QueueItem { id, track: None }).collect();
                self.play_list(items, 0);
                self.status = Some(super::Status { text: format!("Playing {}", m.title), at: Instant::now(), error: false });
            }
            Some(HomeAction::OpenPlaylist(p)) => self.open_playlist(p),
            Some(HomeAction::PlayHistory(n)) => {
                let items = self.home.history.iter().map(|t| QueueItem { id: t.id, track: Some(t.clone()) }).collect();
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
) -> Option<HomeAction> {
    let mut action = None;
    ui.allocate_ui_with_layout(vec2(TILE, TILE + 46.0), Layout::top_down(Align::Min), |ui| {
        ui.set_width(TILE);
        let img = rows::artwork(ui, art, track.artwork(), TILE, "t300x300")
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("Play");
        if ui.rect_contains_pointer(img.rect) {
            let center = img.rect.right_bottom() - vec2(24.0, 24.0);
            ui.painter().rect_filled(img.rect, 4.0, hover().gamma_multiply(0.35));
            ui.painter().circle_filled(center, 18.0, Color32::from_black_alpha(200));
            ui.painter().text(center, Align2::CENTER_CENTER, "▶", FontId::proportional(16.0), Color32::WHITE);
        }
        if img.clicked() {
            action = Some(if track.availability() == crate::api::models::Availability::Drm {
                HomeAction::Track(TrackAction::OpenOnSoundCloud, track.clone())
            } else {
                HomeAction::PlayHistory(index)
            });
        }
        img.context_menu(|ui| {
            if let Some(a) = rows::track_menu(ui, track, liked.contains(&track.id)) {
                action = Some(if a == TrackAction::Play { HomeAction::PlayHistory(index) } else { HomeAction::Track(a, track.clone()) });
            }
        });
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(&track.title).strong()).truncate().selectable(false));
        ui.add(egui::Label::new(RichText::new(&track.user.username).small().color(muted())).truncate().selectable(false));
    });
    action
}

/// One tile: artwork, title, subtitle. Mixes get a ▶ while hovered.
fn tile(ui: &mut egui::Ui, art: &crate::cache::art::ArtCache, item: &HomeItem) -> Option<HomeAction> {
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
        let play_center = img.rect.right_bottom() - vec2(24.0, 24.0);
        let on_play = mix.is_some()
            && hovered
            && ui.pointer_hover_pos().is_some_and(|p| p.distance(play_center) <= 18.0);
        if hovered {
            ui.painter().rect_filled(img.rect, 4.0, hover().gamma_multiply(0.35));
            if mix.is_some() {
                ui.painter().circle_filled(play_center, 18.0, Color32::from_black_alpha(if on_play { 230 } else { 170 }));
                ui.painter().text(play_center, Align2::CENTER_CENTER, "▶", FontId::proportional(16.0), Color32::WHITE);
            }
        }
        if img.clicked() {
            action = Some(match (mix, item) {
                (Some(m), _) if on_play => HomeAction::PlayMix(m.clone()),
                (Some(m), _) => HomeAction::OpenMix(m.clone()),
                (None, HomeItem::Playlist(p)) => HomeAction::OpenPlaylist(Arc::new(p.clone())),
                _ => return,
            });
        }
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(title).strong()).truncate().selectable(false));
        ui.add(egui::Label::new(RichText::new(subtitle).small().color(muted())).truncate().selectable(false));
    });
    action
}
