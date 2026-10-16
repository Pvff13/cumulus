//! Home: SoundCloud's personalised home page (your mixes, Daily Drops,
//! stations, recommended albums...) as rows of tiles, in SoundCloud's order.

use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, Align, Align2, Color32, CursorIcon, FontId, Layout, RichText, vec2};

use super::library::PlaylistView;
use super::rows;
use super::theme::{HOVER, MUTED};
use super::{App, Msg, Page, status_line};
use crate::api::models::{HomeItem, Page as ApiPage, Playlist, Selection, SystemPlaylist};
use crate::api::{Api, ApiError, client};
use crate::cache::meta;
use crate::player::QueueItem;

const TILE: f32 = 150.0;

#[derive(Default)]
pub struct HomeState {
    pub sections: Vec<Selection>,
    pub loading: bool,
    pub error: Option<String>,
    pub started: bool,
    pub from_cache: bool,
}

enum HomeAction {
    OpenMix(SystemPlaylist),
    PlayMix(SystemPlaylist),
    OpenPlaylist(Arc<Playlist>),
}

/// Your personal mixes and "Made for you" (Daily Drops) first; everything
/// else keeps SoundCloud's order.
fn parse(body: &str) -> Result<Vec<Selection>, ApiError> {
    let page: ApiPage<Selection> = client::parse(body)?;
    let mut sections = page.collection;
    let rank = |s: &Selection| match () {
        _ if s.urn.ends_with(":your-moods") => 0,
        _ if s.urn.ends_with(":made-for-you") => 1,
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
        ui.horizontal(|ui| {
            ui.heading("Home");
            if self.home.loading {
                ui.spinner();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                refresh = ui.add_enabled(!self.home.loading, egui::Button::new("↻")).on_hover_text("Refresh").clicked();
            });
        });
        status_line(ui, self.home.error.as_deref(), self.home.from_cache && self.home.loading);
        ui.add_space(6.0);

        let mut action = None;
        egui::ScrollArea::vertical().id_salt("home").auto_shrink(false).show(ui, |ui| {
            for (i, section) in self.home.sections.iter().enumerate() {
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
            None => {}
        }
        if refresh {
            self.load_home();
        }
    }
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
            ui.painter().rect_filled(img.rect, 4.0, HOVER.gamma_multiply(0.35));
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
        ui.add(egui::Label::new(RichText::new(subtitle).small().color(MUTED)).truncate().selectable(false));
    });
    action
}
