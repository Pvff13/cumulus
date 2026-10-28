//! A track's own page, like the one on soundcloud.com: details, related
//! tracks, and the albums and playlists it's in.

use std::sync::Arc;

use eframe::egui::{self, RichText};

use super::rows::{self, RowCtx, TrackAction, compact};
use super::theme::{self, accent, muted};
use super::{App, ArtistInfo, Msg, Page};
use crate::api::ApiError;
use crate::api::models::{Availability, Download, Playlist, Track};
use crate::player::QueueItem;

/// Playlists to load at first, and per "Show more".
const PLAYLISTS: usize = 20;
/// Descriptions longer than this are cut short until "Show more".
const DESCRIPTION_PREVIEW: usize = 320;

#[derive(Clone)]
pub enum Section<T> {
    Loading,
    Loaded(Vec<T>),
    Failed(String),
}

pub struct TrackView {
    pub track: Arc<Track>,
    related: Section<Arc<Track>>,
    albums: Section<Arc<Playlist>>,
    playlists: Section<Arc<Playlist>>,
    /// Where the next page of playlists starts (None = no more).
    playlists_next: Option<String>,
    loading_more: bool,
    show_full_description: bool,
}

impl TrackView {
    fn new(track: Arc<Track>) -> Self {
        Self {
            track,
            related: Section::Loading,
            albums: Section::Loading,
            playlists: Section::Loading,
            playlists_next: None,
            loading_more: false,
            show_full_description: false,
        }
    }
}

impl TrackView {
    /// Drops hidden tracks/playlists from the lists.
    pub fn hide(&mut self, hidden: &super::hide::Hidden) {
        if let Section::Loaded(v) = &mut self.related {
            v.retain(|t| !hidden.track(t));
        }
        if let Section::Loaded(v) = &mut self.playlists {
            v.retain(|p| !hidden.playlist(p));
        }
        if let Section::Loaded(v) = &mut self.albums {
            v.retain(|p| !hidden.playlist(p));
        }
    }
}

/// One part of a track page, fetched separately so each shows when ready.
pub enum Part {
    Details(Result<Track, ApiError>),
    Related(Result<Vec<Track>, ApiError>),
    Albums(Result<Vec<Playlist>, ApiError>),
    /// A batch of playlists and where the next one starts; `more` = appended
    /// by "Show more".
    Playlists { result: Result<(Vec<Playlist>, Option<String>), ApiError>, more: bool },
}

fn section<T, U>(result: Result<Vec<T>, ApiError>, wrap: impl Fn(T) -> U) -> Section<U> {
    match result {
        Ok(items) => Section::Loaded(items.into_iter().map(wrap).collect()),
        Err(e) => Section::Failed(e.to_string()),
    }
}

/// "2022-12-05T18:03:59Z" -> "5 Dec 2022".
pub fn format_date(iso: &str) -> Option<String> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let mut parts = iso.get(..10)?.split('-');
    let (y, m, d) = (parts.next()?, parts.next()?.parse::<usize>().ok()?, parts.next()?.parse::<u32>().ok()?);
    Some(format!("{d} {} {y}", MONTHS.get(m.checked_sub(1)?)?))
}

impl App {
    /// Opens `track`'s page and starts loading its details, related tracks,
    /// albums and playlists.
    pub(super) fn open_track(&mut self, track: Arc<Track>) {
        if track.local_path.is_some() {
            return; // a file on this PC has no SoundCloud page
        }
        let id = track.id;
        self.push_page(Page::Track(TrackView::new(track)));
        self.spawn(move |api, send| send(Msg::TrackPage { id, part: Part::Details(api.track(id)) }));
        self.spawn(move |api, send| send(Msg::TrackPage { id, part: Part::Related(api.related(id)) }));
        self.spawn(move |api, send| send(Msg::TrackPage { id, part: Part::Albums(api.track_albums(id)) }));
        self.spawn(move |api, send| {
            send(Msg::TrackPage { id, part: Part::Playlists { result: api.track_playlists(id, None, PLAYLISTS), more: false } })
        });
    }

    fn more_playlists(&mut self) {
        let Some(Page::Track(view)) = self.pages.last_mut() else { return };
        let Some(next) = view.playlists_next.clone().filter(|_| !view.loading_more) else { return };
        view.loading_more = true;
        let id = view.track.id;
        self.spawn(move |api, send| {
            let result = api.track_playlists(id, Some(next), PLAYLISTS);
            send(Msg::TrackPage { id, part: Part::Playlists { result, more: true } })
        });
    }

    pub(super) fn apply_track_page(&mut self, id: u64, part: Part) {
        for page in &mut self.pages {
            let Page::Track(view) = page else { continue };
            if view.track.id != id {
                continue;
            }
            match &part {
                Part::Details(Ok(t)) => view.track = Arc::new(t.clone()),
                Part::Details(Err(_)) => {} // keep what we had
                Part::Related(r) => {
                    view.related = section(clone_result(r), Arc::new);
                    // All of them (SoundCloud sends 30), so what plays from
                    // here matches what's listed.
                    if let Section::Loaded(v) = &mut view.related {
                        v.retain(|t| t.id != id);
                    }
                }
                Part::Albums(r) => view.albums = section(clone_result(r), Arc::new),
                Part::Playlists { result, more } => {
                    view.loading_more = false;
                    match result {
                        Ok((found, next)) => {
                            #[cfg(debug_assertions)]
                            eprintln!("track {id}: {} playlists, more: {}", found.len(), next.is_some());
                            view.playlists_next = next.clone();
                            let found = found.iter().cloned().map(Arc::new);
                            match &mut view.playlists {
                                Section::Loaded(list) if *more => {
                                    // Pages can overlap when playlists change; skip repeats.
                                    for p in found {
                                        if !list.iter().any(|q| q.id == p.id) {
                                            list.push(p);
                                        }
                                    }
                                }
                                _ => view.playlists = Section::Loaded(found.collect()),
                            }
                        }
                        // Keep what's shown; "Show more" can be tried again.
                        Err(_) if *more => {}
                        Err(e) => view.playlists = Section::Failed(e.to_string()),
                    }
                }
            }
        }
    }

    pub(super) fn track_ui(&mut self, ui: &mut egui::Ui) {
        if ui.button("← Back").clicked() {
            self.go_back();
            return;
        }
        ui.add_space(8.0);
        let Some(Page::Track(view)) = self.pages.last() else { return };
        let track = view.track.clone();
        let (related, albums, playlists) = (view.related.clone(), view.albums.clone(), view.playlists.clone());
        let (has_more_playlists, loading_more) = (view.playlists_next.is_some(), view.loading_more);
        let mut show_more = false;
        let full_description = view.show_full_description;

        let mut action: Option<(TrackAction, Arc<Track>)> = None;
        let mut play_related: Option<usize> = None;
        let mut open_playlist: Option<Arc<Playlist>> = None;
        let mut toggle_description = false;

        egui::ScrollArea::vertical().id_salt(("track-page", track.id)).auto_shrink(false).show(ui, |ui| {
            // Header: artwork, title, artist, numbers, buttons.
            ui.horizontal_top(|ui| {
                let _ = rows::artwork(ui, &self.art, track.artwork(), 160.0, "t300x300");
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new("Track").small().color(muted()));
                    ui.add(egui::Label::new(RichText::new(&track.title).heading()).wrap());
                    let artist = ui
                        .add(egui::Label::new(RichText::new(&track.user.username).size(15.0).color(muted())).sense(egui::Sense::click()))
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text("Show this artist's tracks");
                    if artist.clicked() {
                        action = Some((TrackAction::OpenArtist, track.clone()));
                    }
                    ui.add_space(4.0);
                    ui.label(RichText::new(stats_line(&track)).small().color(muted()));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let drm = track.availability() == Availability::Drm;
                        let play = egui::Button::new(
                            RichText::new(if drm { "↗  Play on SoundCloud" } else { "▶  Play" }).strong().color(theme::on_accent()),
                        )
                        .fill(accent());
                        if ui.add_enabled(track.availability() != Availability::Blocked, play).clicked() {
                            action = Some((if drm { TrackAction::OpenOnSoundCloud } else { TrackAction::Play }, track.clone()));
                        }
                        let liked = self.liked.contains(&track.id);
                        let like = if liked { "♥  Liked" } else { "♡  Like" };
                        if ui.button(like).on_hover_text("Opens a SoundCloud window to (un)like it there").clicked() {
                            action = Some((TrackAction::LikeOnSoundCloud, track.clone()));
                        }
                        if ui.button("Add to queue").clicked() {
                            action = Some((TrackAction::AddToQueue, track.clone()));
                        }
                        match track.download() {
                            Some(Download::File) => {
                                if ui.button("⬇  Download").clicked() {
                                    action = Some((TrackAction::Download, track.clone()));
                                }
                            }
                            Some(Download::Link { label, .. }) => {
                                if ui.button(format!("⬇  {label} ↗")).clicked() {
                                    action = Some((TrackAction::Download, track.clone()));
                                }
                            }
                            None => {}
                        }
                        if !track.permalink_url.is_empty() {
                            ui.hyperlink_to("Open on SoundCloud ↗", &track.permalink_url);
                        }
                    });
                });
            });

            // Description, cut short when long.
            if let Some(desc) = track.description.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
                ui.add_space(12.0);
                let long = desc.chars().count() > DESCRIPTION_PREVIEW;
                let shown: String = if long && !full_description {
                    format!("{}…", desc.chars().take(DESCRIPTION_PREVIEW).collect::<String>().trim_end())
                } else {
                    desc.to_string()
                };
                ui.add(egui::Label::new(RichText::new(shown)).wrap());
                if long && ui.link(if full_description { "Show less" } else { "Show more" }).clicked() {
                    toggle_description = true;
                }
            }

            let cx = RowCtx { art: &self.art, liked: &self.liked, current: self.queue.current_id(), local_list: false };

            ui.add_space(16.0);
            heading(ui, "Related tracks", &related);
            if let Section::Loaded(list) = &related {
                ui.spacing_mut().item_spacing.y = 2.0;
                for (i, t) in list.iter().enumerate() {
                    match rows::track_row(ui, &cx, t, None) {
                        Some(TrackAction::Play) => play_related = Some(i),
                        Some(a) => action = Some((a, t.clone())),
                        None => {}
                    }
                }
            }

            // Albums only appear when it's on one (most tracks aren't).
            if !matches!(&albums, Section::Loaded(v) if v.is_empty()) {
                ui.add_space(16.0);
                heading(ui, "In albums", &albums);
                if let Section::Loaded(list) = &albums {
                    for p in list {
                        if rows::playlist_row(ui, cx.art, p, None).clicked() {
                            open_playlist = Some(p.clone());
                        }
                    }
                }
            }

            ui.add_space(16.0);
            heading(ui, "In playlists", &playlists);
            if let Section::Loaded(list) = &playlists {
                if list.is_empty() {
                    ui.label(RichText::new("Not in any public playlists yet.").color(muted()));
                }
                for p in list {
                    if rows::playlist_row(ui, cx.art, p, None).clicked() {
                        open_playlist = Some(p.clone());
                    }
                }
                if has_more_playlists {
                    ui.add_space(4.0);
                    if loading_more {
                        ui.spinner();
                    } else if ui.button("Show more").clicked() {
                        show_more = true;
                    }
                }
            }
            ui.add_space(12.0);
        });

        if show_more {
            self.more_playlists();
        }
        if toggle_description {
            if let Some(Page::Track(view)) = self.pages.last_mut() {
                view.show_full_description = !view.show_full_description;
            }
        }
        if let Some(i) = play_related {
            if let Section::Loaded(list) = &related {
                let items = list.iter().map(|t| QueueItem { id: t.id, track: Some(t.clone()) }).collect();
                self.play_list(items, i);
            }
        }
        if let Some(p) = open_playlist {
            self.open_playlist(p);
        }
        if let Some((a, t)) = action {
            match a {
                TrackAction::OpenArtist => self.open_artist(ArtistInfo {
                    id: t.user.id,
                    username: t.user.username.clone(),
                    permalink_url: t.user.permalink_url.clone(),
                    avatar_url: t.user.avatar_url.clone(),
                }),
                a => self.track_action(a, t),
            }
        }
    }
}

/// "▶ 19.2K plays · ♥ 508 · ↻ 12 · 💬 72 · 5 Dec 2022 · #genre"
fn stats_line(t: &Track) -> String {
    let mut parts = Vec::new();
    if let Some(n) = t.playback_count {
        parts.push(format!("▶ {} plays", compact(n)));
    }
    if let Some(n) = t.likes_count {
        parts.push(format!("♥ {}", compact(n)));
    }
    if let Some(n) = t.reposts_count {
        parts.push(format!("↻ {} reposts", compact(n)));
    }
    if let Some(n) = t.comment_count {
        parts.push(format!("{} comments", compact(n)));
    }
    parts.push(rows::format_duration(t.duration));
    if let Some(d) = t.display_date.as_deref().or(t.created_at.as_deref()).and_then(format_date) {
        parts.push(d);
    }
    if let Some(g) = t.genre.as_deref().map(str::trim).filter(|g| !g.is_empty()) {
        parts.push(format!("#{g}"));
    }
    parts.join("  ·  ")
}

fn heading<T>(ui: &mut egui::Ui, title: &str, section: &Section<T>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(18.0).strong());
        if matches!(section, Section::Loading) {
            ui.spinner();
        }
    });
    if let Section::Failed(e) = section {
        ui.label(RichText::new(format!("Couldn't load: {e}")).color(theme::error()));
    }
    ui.add_space(4.0);
}

/// `ApiError` isn't `Clone`; each page part keeps its own copy.
fn clone_result<T: Clone>(r: &Result<Vec<T>, ApiError>) -> Result<Vec<T>, ApiError> {
    match r {
        Ok(v) => Ok(v.clone()),
        Err(e) => Err(ApiError::Offline(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(format_date("2022-12-05T18:03:59Z").as_deref(), Some("5 Dec 2022"));
        assert_eq!(format_date("2024-01-31").as_deref(), Some("31 Jan 2024"));
        assert_eq!(format_date("nope"), None);
        assert_eq!(format_date("2024-13-01"), None);
    }
}
