//! The main content area: library lists, playlist and artist pages, search,
//! and what happens when a row's action is used.

use std::sync::Arc;

use eframe::egui::{self, Align, Key, Layout, RichText};

use super::library::{Entry, FeedKind, Row};
use super::rows::{self, ROW_HEIGHT, RowCtx, TrackAction};
use super::theme::muted;
use super::{App, ArtistInfo, Msg, Nav, Page, status_line};
use crate::api::models::{Playlist, Track};
use crate::api::SearchKind;
use crate::player::QueueItem;

impl App {
    pub(super) fn content_ui(&mut self, ui: &mut egui::Ui) {
        match self.pages.last() {
            Some(Page::Playlist(_)) => self.playlist_ui(ui),
            Some(Page::Artist(a)) => {
                let a = a.clone();
                self.artist_ui(ui, &a);
            }
            None => match (self.nav, self.nav.feed()) {
                (_, Some(kind)) => {
                    let nav = self.nav;
                    self.feed_ui(ui, kind, Some(nav.label()), nav.empty_text());
                }
                (Nav::Home, None) => self.home_ui(ui),
                (_, None) => self.search_ui(ui),
            },
        }
    }

    fn back_button(&mut self, ui: &mut egui::Ui) {
        if ui.button("← Back").clicked() {
            self.pages.pop();
        }
        ui.add_space(8.0);
    }

    /// A paginated list (library, search results or an artist's tracks).
    fn feed_ui(&mut self, ui: &mut egui::Ui, kind: FeedKind, title: Option<&str>, empty: &str) {
        if !self.feeds.get(&kind).is_some_and(|f| f.started) {
            self.load_feed(kind, true);
        }
        let mut menu = super::overlay_ui::MenuResult::default();
        let feed = &self.feeds[&kind];
        let mut refresh = false;
        ui.horizontal(|ui| {
            if let Some(t) = title {
                ui.heading(t);
            }
            if feed.loading {
                ui.spinner();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                refresh = ui.add_enabled(!feed.loading, egui::Button::new("↻")).on_hover_text("Refresh").clicked();
                menu = super::overlay_ui::overlay_menu(ui, &mut self.overlay_menu);
            });
        });
        status_line(ui, feed.error.as_deref(), feed.from_cache && feed.loading);
        ui.add_space(6.0);

        if feed.rows.is_empty() {
            if !feed.loading && feed.error.is_none() {
                ui.label(RichText::new(empty).color(muted()));
            }
        } else {
            let cx = RowCtx { art: &self.art, liked: &self.liked, current: self.queue.current_id() };
            let mut open: Option<Arc<Playlist>> = None;
            let mut artist: Option<ArtistInfo> = None;
            let mut action: Option<(usize, TrackAction)> = None;
            let mut more = false;
            ui.spacing_mut().item_spacing.y = 2.0;
            egui::ScrollArea::vertical().id_salt(kind).auto_shrink(false).show_rows(
                ui,
                ROW_HEIGHT,
                feed.rows.len(),
                |ui, range| {
                    for (i, row) in feed.rows[range.clone()].iter().enumerate() {
                        match row {
                            Row::Track { track, note } => {
                                if let Some(a) = rows::track_row(ui, &cx, track, note.as_deref()) {
                                    action = Some((range.start + i, a));
                                }
                            }
                            Row::Playlist { playlist, note } => {
                                if rows::playlist_row(ui, cx.art, playlist, note.as_deref()).clicked() {
                                    open = Some(playlist.clone());
                                }
                            }
                            Row::User(u) => {
                                if rows::user_row(ui, cx.art, u) {
                                    artist = Some(ArtistInfo {
                                        id: u.id,
                                        username: u.username.clone(),
                                        permalink_url: u.permalink_url.clone(),
                                        avatar_url: u.avatar_url.clone(),
                                    });
                                }
                            }
                        }
                    }
                    if range.end == feed.rows.len() && feed.loading {
                        ui.horizontal(|ui| ui.spinner());
                    }
                    more = feed.wants_more(range.end);
                },
            );
            #[cfg(debug_assertions)]
            dev_actions(feed, &mut open, &mut action);

            if let Some((clicked, a)) = action {
                let feed = &self.feeds[&kind];
                if let Row::Track { track, .. } = &feed.rows[clicked] {
                    let track = track.clone();
                    if a == TrackAction::Play {
                        // Queue every track in the list, starting at the clicked one.
                        let start = feed.rows[..clicked].iter().filter(|r| matches!(r, Row::Track { .. })).count();
                        let items = feed
                            .rows
                            .iter()
                            .filter_map(|r| match r {
                                Row::Track { track, .. } => Some(QueueItem { id: track.id, track: Some(track.clone()) }),
                                _ => None,
                            })
                            .collect();
                        self.play_list(items, start);
                    } else {
                        self.track_action(a, track);
                    }
                }
            }
            if let Some(p) = open {
                self.open_playlist(p);
            }
            if let Some(a) = artist {
                self.open_artist(a);
            }
            if more {
                self.load_feed(kind, false);
            }
        }
        if refresh {
            self.load_feed(kind, true);
        }
        self.handle_menu(menu);
    }

    fn playlist_ui(&mut self, ui: &mut egui::Ui) {
        self.back_button(ui);
        let art = self.art.clone();
        let current = self.queue.current_id();
        let Some(Page::Playlist(view)) = self.pages.last_mut() else { return };
        let p = view.playlist.clone();
        ui.horizontal(|ui| {
            let _ = rows::artwork(ui, &art, p.artwork(), 120.0, "t300x300");
            ui.add_space(8.0);
            ui.vertical(|ui| {
                // Mixes get a synthetic id with the top bit set (see SystemPlaylist::local_id).
                let kind = if p.id >= 1 << 63 { "Mix" } else if p.is_album { "Album" } else { "Playlist" };
                ui.label(RichText::new(kind).small().color(muted()));
                ui.add(egui::Label::new(RichText::new(&p.title).heading()).truncate());
                rows::link_label(ui, &p.user.username, &p.user.permalink_url);
                ui.label(RichText::new(format!("{} tracks", p.track_count)).color(muted()));
                if !p.permalink_url.is_empty() {
                    ui.hyperlink_to("Open on SoundCloud ↗", &p.permalink_url);
                }
            });
        });
        ui.add_space(8.0);
        status_line(ui, view.error.as_deref(), false);
        if view.loading {
            ui.spinner();
        }

        let cx = RowCtx { art: &art, liked: &self.liked, current };
        let mut visible = 0..0;
        let mut action: Option<(usize, TrackAction)> = None;
        ui.spacing_mut().item_spacing.y = 2.0;
        egui::ScrollArea::vertical()
            .id_salt(("playlist", p.id))
            .auto_shrink(false)
            .show_rows(ui, ROW_HEIGHT, view.entries.len(), |ui, range| {
                for (i, entry) in view.entries[range.clone()].iter().enumerate() {
                    match entry {
                        Entry::Loaded(t) => {
                            if let Some(a) = rows::track_row(ui, &cx, t, None) {
                                action = Some((range.start + i, a));
                            }
                        }
                        Entry::Pending(_) => rows::pending_row(ui, "Loading..."),
                        Entry::Missing => rows::pending_row(ui, "Track unavailable"),
                    }
                }
                visible = range;
            });

        let playlist_id = p.id;
        let missing = view.take_missing(visible);
        // Queue the whole playlist; tracks not fetched yet are filled in when they come up.
        let to_do = action.and_then(|(clicked, a)| {
            let Entry::Loaded(track) = &view.entries[clicked] else { return None };
            let track = track.clone();
            if a != TrackAction::Play {
                return Some((a, track, Vec::new(), 0));
            }
            let start = view.entries[..clicked].iter().filter(|e| !matches!(e, Entry::Missing)).count();
            let items = view
                .entries
                .iter()
                .filter_map(|e| match e {
                    Entry::Loaded(t) => Some(QueueItem { id: t.id, track: Some(t.clone()) }),
                    Entry::Pending(id) => Some(QueueItem { id: *id, track: None }),
                    Entry::Missing => None,
                })
                .collect();
            Some((a, track, items, start))
        });
        for asked in missing {
            self.spawn(move |api, send| {
                let result = api.tracks(&asked);
                send(Msg::Tracks { playlist_id, asked, result });
            });
        }
        match to_do {
            Some((TrackAction::Play, _, items, start)) => {
                self.play_list(items, start);
                self.playing_list = Some(playlist_id);
            }
            Some((a, track, _, _)) => self.track_action(a, track),
            None => {}
        }
    }

    fn artist_ui(&mut self, ui: &mut egui::Ui, artist: &ArtistInfo) {
        self.back_button(ui);
        ui.horizontal(|ui| {
            let _ = rows::artwork(ui, &self.art, artist.avatar_url.as_deref(), 64.0, "t300x300");
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("Artist").small().color(muted()));
                ui.heading(&artist.username);
                if !artist.permalink_url.is_empty() {
                    ui.hyperlink_to("Open on SoundCloud ↗", &artist.permalink_url);
                }
            });
        });
        ui.add_space(8.0);
        self.feed_ui(ui, FeedKind::Artist(artist.id), Some("Tracks"), "No public tracks.");
    }

    fn search_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Search");
        ui.add_space(6.0);
        let resp = ui.add(
            egui::TextEdit::singleline(&mut self.search_text)
                .hint_text("Search tracks, people, playlists...   ( / )")
                .desired_width(f32::INFINITY),
        );
        if self.focus_search {
            resp.request_focus();
            self.focus_search = false;
        }
        if resp.changed() {
            self.search_edited = Some(std::time::Instant::now());
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            self.run_search();
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for (kind, label) in [
                (SearchKind::Tracks, "Tracks"),
                (SearchKind::People, "People"),
                (SearchKind::Playlists, "Playlists"),
                (SearchKind::Albums, "Albums"),
            ] {
                if ui.selectable_label(self.search_kind == kind, label).clicked() {
                    self.search_kind = kind;
                }
            }
        });
        ui.add_space(4.0);
        if self.search_query.is_empty() {
            ui.label(RichText::new("Type to search SoundCloud.").color(muted()));
            return;
        }
        let kind = FeedKind::Search(self.search_kind);
        self.feed_ui(ui, kind, None, "No results.");
    }

    /// Everything a row can ask for except Play (which needs the whole list).
    pub(super) fn track_action(&mut self, action: TrackAction, track: Arc<Track>) {
        let title = track.title.clone();
        match action {
            TrackAction::Play => self.play_list(vec![QueueItem { id: track.id, track: Some(track) }], 0),
            TrackAction::LikeOnSoundCloud | TrackAction::AddToPlaylistOnSoundCloud => {
                // SoundCloud puts likes and playlist edits behind a browser
                // check, so these are done on soundcloud.com itself. Likes
                // are re-read when you come back to this window.
                self.ctx.open_url(egui::OpenUrl::new_tab(&track.permalink_url));
                let what = if action == TrackAction::LikeOnSoundCloud { "like" } else { "add it to a playlist" };
                self.info(format!("Opened “{title}” on SoundCloud to {what} there"));
                self.recheck_on_focus = Some(action);
            }
            TrackAction::PlayNext | TrackAction::AddToQueue => {
                let was_empty = self.queue.is_empty();
                let item = QueueItem { id: track.id, track: Some(track) };
                if action == TrackAction::PlayNext {
                    self.queue.play_next(item);
                    self.info(format!("Playing next: {title}"));
                } else {
                    self.queue.add_to_end(item);
                    self.info(format!("Added to queue: {title}"));
                }
                if was_empty {
                    self.play_queue_at(0);
                } else {
                    self.player.set_next(self.queue.peek_next());
                }
            }
            TrackAction::OpenOnSoundCloud => {
                self.ctx.open_url(egui::OpenUrl::new_tab(&track.permalink_url));
                self.info(format!("“{title}” is DRM-protected, so it only plays on SoundCloud. Opened it in your browser."));
            }
            TrackAction::Download => match track.download() {
                Some(crate::api::models::Download::File) => {
                    if !self.downloading.insert(track.id) {
                        return;
                    }
                    self.info(format!("Downloading “{title}”…"));
                    let id = track.id;
                    self.spawn(move |api, send| {
                        let result = crate::download::save(api, &track);
                        send(Msg::Downloaded { id, title, result });
                    });
                }
                Some(crate::api::models::Download::Link { url, label }) => {
                    self.ctx.open_url(egui::OpenUrl::new_tab(url));
                    self.info(format!("Opened the artist's “{label}” link for “{title}” in your browser"));
                }
                None => {}
            },
            TrackAction::OpenArtist => self.open_artist(ArtistInfo {
                id: track.user.id,
                username: track.user.username.clone(),
                permalink_url: track.user.permalink_url.clone(),
                avatar_url: track.user.avatar_url.clone(),
            }),
        }
    }

    /// Global shortcut target for "/".
    pub(super) fn focus_search(&mut self) {
        self.nav = Nav::Search;
        self.pages.clear();
        self.focus_search = true;
    }
}

/// Debug builds: CUMULUS_DEV_OPEN=1 opens the first playlist and
/// CUMULUS_DEV_PLAY=1 plays the first track, once each, for testing.
#[cfg(debug_assertions)]
fn dev_actions(
    feed: &super::library::Feed,
    open: &mut Option<Arc<Playlist>>,
    action: &mut Option<(usize, TrackAction)>,
) {
    let take = |name: &str| {
        let set = std::env::var_os(name).is_some();
        // SAFETY: only the UI thread reads these dev-only flags.
        unsafe { std::env::remove_var(name) };
        set
    };
    if open.is_none() && take("CUMULUS_DEV_OPEN") {
        *open = feed.rows.iter().find_map(|r| match r {
            Row::Playlist { playlist, .. } => Some(playlist.clone()),
            _ => None,
        });
    }
    if action.is_none() && take("CUMULUS_DEV_PLAY") {
        *action = feed
            .rows
            .iter()
            .position(|r| matches!(r, Row::Track { .. }))
            .map(|i| (i, TrackAction::Play));
    }
}
