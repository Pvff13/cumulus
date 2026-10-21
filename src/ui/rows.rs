//! Row widgets for track and playlist lists. Every row credits the uploader
//! (linked to their SoundCloud profile) and links to the item's own page.

use eframe::egui::{
    self, Align, CursorIcon, Layout, OpenUrl, Rect, Response, RichText, Sense, Stroke, UiBuilder, vec2,
};

use std::collections::HashSet;

use super::theme::{accent, hover, muted, surface, tag as tag_colour};
use crate::api::models::{Availability, Download, Playlist, Track, User, sized_art};
use crate::cache::art::ArtCache;

/// Compact layout: smaller rows and artwork.
pub static COMPACT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn is_compact() -> bool {
    COMPACT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Height of a list row.
pub fn row_height() -> f32 {
    if is_compact() { 40.0 } else { 52.0 }
}

/// Artwork size in list rows.
fn art_size() -> f32 {
    if is_compact() { 30.0 } else { 40.0 }
}

/// Allocates a full-width row and returns (response, inner rect).
fn row_frame(ui: &mut egui::Ui) -> (Response, Rect) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_height()), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, hover());
    }
    (resp, rect.shrink2(vec2(10.0, 6.0)))
}

pub fn artwork(ui: &mut egui::Ui, art: &ArtCache, url: Option<&str>, size: f32, variant: &str) -> Response {
    let (r, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    match url.and_then(|u| art.get(&sized_art(u, variant))) {
        Some(t) => egui::Image::new(&t).corner_radius(4).paint_at(ui, r),
        None => {
            ui.painter().rect_filled(r, 4.0, surface());
        }
    }
    resp
}

/// Artwork with a ▶ overlay while the row is hovered. Returns true if clicked.
fn play_artwork(ui: &mut egui::Ui, art: &ArtCache, url: Option<&str>, row_hovered: bool, glyph: &str) -> bool {
    let resp = artwork(ui, art, url, art_size(), "t67x67");
    if row_hovered {
        let r = resp.rect;
        ui.painter().rect_filled(r, 4.0, egui::Color32::from_black_alpha(140));
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, glyph, egui::FontId::proportional(18.0), egui::Color32::WHITE);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

/// A muted label that opens `url` in the browser when clicked.
pub fn link_label(ui: &mut egui::Ui, text: &str, url: &str) {
    if url.is_empty() {
        ui.label(RichText::new(text).color(muted()));
        return;
    }
    let resp = ui
        .add(egui::Label::new(RichText::new(text).color(muted())).sense(Sense::click()).truncate())
        .on_hover_cursor(CursorIcon::PointingHand);
    if resp.hovered() {
        let r = resp.rect;
        ui.painter().hline(r.x_range(), r.bottom(), Stroke::new(1.0, muted()));
    }
    if resp.clicked() {
        ui.ctx().open_url(OpenUrl::new_tab(url));
    }
}

/// The small ↗ that opens the item on soundcloud.com.
fn open_on_soundcloud(ui: &mut egui::Ui, url: &str) {
    if url.is_empty() {
        return;
    }
    let resp = ui
        .add(egui::Label::new(RichText::new("↗").color(muted()).size(15.0)).sense(Sense::click()))
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text("Open on SoundCloud");
    if resp.clicked() {
        ui.ctx().open_url(OpenUrl::new_tab(url));
    }
}

fn tag(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(tag_colour()));
}

pub fn format_duration(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// What a row needs besides the item itself.
pub struct RowCtx<'a> {
    pub art: &'a ArtCache,
    /// Tracks the user has liked (shown as ♥).
    pub liked: &'a HashSet<u64>,
    /// The track currently playing, highlighted.
    pub current: Option<u64>,
    /// Rows of a local playlist (the menu offers "Remove from this playlist").
    pub local_list: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackAction {
    Play,
    /// Liking needs SoundCloud's own site (it puts writes behind a browser
    /// check), so these open the track page there.
    LikeOnSoundCloud,
    AddToPlaylistOnSoundCloud,
    PlayNext,
    AddToQueue,
    OpenArtist,
    /// Play a DRM-only track where it can play: on soundcloud.com.
    OpenOnSoundCloud,
    /// Get the file the artist offers (or open their download link).
    Download,
    /// The track's own page (details, related tracks, playlists).
    OpenTrack,
    /// Hide this track everywhere.
    NotInterested,
    /// Hide everything by this artist.
    BlockArtist,
    /// Add to local playlist n (None = a new one).
    AddToLocal(Option<usize>),
    /// Remove from the local playlist being shown.
    RemoveFromLocal,
}

/// A track row: ▶ on the artwork (or double-click) plays, the heart opens
/// the track on SoundCloud to like it there,
/// right-click opens the full menu, the artist name opens their tracks.
pub fn track_row(ui: &mut egui::Ui, cx: &RowCtx<'_>, track: &Track, note: Option<&str>) -> Option<TrackAction> {
    let (resp, inner) = row_frame(ui);
    let hovered = ui.rect_contains_pointer(resp.rect);
    let mut action = resp.double_clicked().then_some(TrackAction::Play);
    let availability = track.availability();
    let liked = cx.liked.contains(&track.id);
    resp.context_menu(|ui| {
        if let Some(a) = track_menu(ui, track, liked, cx.local_list) {
            action = Some(a);
        }
    });
    let mut child = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::right_to_left(Align::Center)));
    let ui = &mut child;
    open_on_soundcloud(ui, &track.permalink_url);
    if download_button(ui, track) {
        action = Some(TrackAction::Download);
    }
    ui.add_space(4.0);
    ui.label(RichText::new(format_duration(track.duration)).color(muted()).monospace());
    ui.add_space(4.0);
    if (hovered || liked) && heart(ui, liked).clicked() {
        action = Some(TrackAction::LikeOnSoundCloud);
    }
    match availability {
        Availability::Blocked => tag(ui, "Not available"),
        Availability::Preview => tag(ui, "Preview"),
        Availability::Drm => {
            ui.label(RichText::new("DRM").small().color(tag_colour()))
                .on_hover_text("Protected by SoundCloud: it only plays on soundcloud.com or SoundCloud's apps. Click to open it there.");
        }
        Availability::Playable => {}
    }
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        let glyph = if availability == Availability::Drm { "↗" } else { "▶" };
        if play_artwork(ui, cx.art, track.artwork(), hovered && availability != Availability::Blocked, glyph) {
            action = Some(TrackAction::Play);
        }
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.add_space(1.0);
            let mut title = RichText::new(&track.title).strong();
            if cx.current == Some(track.id) {
                title = title.color(accent());
            }
            if availability == Availability::Blocked {
                title = title.color(muted()).strikethrough();
            } else if availability == Availability::Drm {
                title = title.color(muted());
            }
            let name = ui
                .add(egui::Label::new(title).truncate().selectable(false).sense(Sense::click()))
                .on_hover_cursor(CursorIcon::PointingHand)
                .on_hover_text("Track details, related tracks and playlists");
            if name.hovered() {
                ui.painter().hline(name.rect.x_range(), name.rect.bottom(), Stroke::new(1.0, ui.visuals().text_color()));
            }
            if name.clicked() {
                action = Some(TrackAction::OpenTrack);
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if clickable_label(ui, &track.user.username, "Show this artist's tracks") {
                    action = Some(TrackAction::OpenArtist);
                }
                if let Some(n) = note {
                    ui.label(RichText::new(format!("· {n}")).color(muted()).small());
                }
            });
        });
    });
    // DRM-only tracks can't play here: "play" opens them on soundcloud.com.
    if availability == Availability::Drm && action == Some(TrackAction::Play) {
        action = Some(TrackAction::OpenOnSoundCloud);
    }
    action
}

/// ⬇ on every track: in the accent colour when the artist offers a
/// download, faint when they don't (that one just opens the track on
/// SoundCloud). Returns true when an offered download is clicked.
fn download_button(ui: &mut egui::Ui, track: &Track) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), Sense::click());
    if track.local_path.is_some() {
        return false; // already on this PC
    }
    let offer = track.download();
    let color = match (&offer, resp.hovered()) {
        (Some(_), true) => accent().gamma_multiply(1.25),
        (Some(_), false) => accent(),
        (None, true) => muted(),
        (None, false) => muted().gamma_multiply(0.35),
    };
    let size = if offer.is_some() { 18.0 } else { 15.0 };
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "⬇", egui::FontId::proportional(size), color);
    let tip = match offer {
        Some(Download::File) => "Download: the artist offers this file on SoundCloud".to_string(),
        Some(Download::Link { url, label }) => format!("{label}: opens the artist's link ({})", host(url)),
        None => "The artist doesn't offer a download for this track. Click to open it on SoundCloud.".to_string(),
    };
    let clicked = resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip).clicked();
    if clicked && offer.is_none() && !track.permalink_url.is_empty() {
        ui.ctx().open_url(OpenUrl::new_tab(&track.permalink_url));
        return false;
    }
    clicked && offer.is_some()
}

/// `https://www.hypeddit.com/x` -> `hypeddit.com`.
pub fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    host.strip_prefix("www.").unwrap_or(host)
}

/// Right-click menu for a track.
pub fn track_menu(ui: &mut egui::Ui, track: &Track, liked: bool, in_local_list: bool) -> Option<TrackAction> {
    let mut action = None;
    let mut item = |ui: &mut egui::Ui, text: &str, a: TrackAction| {
        if ui.button(text).clicked() {
            action = Some(a);
            ui.close();
        }
    };
    item(ui, "▶  Play", TrackAction::Play);
    item(ui, "Play next", TrackAction::PlayNext);
    item(ui, "Add to queue", TrackAction::AddToQueue);
    ui.separator();
    let like = if liked { "♥  Unlike on SoundCloud ↗" } else { "♡  Like on SoundCloud ↗" };
    item(ui, like, TrackAction::LikeOnSoundCloud);
    item(ui, "Add to playlist on SoundCloud ↗", TrackAction::AddToPlaylistOnSoundCloud);
    match track.download() {
        Some(Download::File) => item(ui, "⬇  Download", TrackAction::Download),
        Some(Download::Link { label, .. }) => item(ui, &format!("⬇  {label} ↗"), TrackAction::Download),
        None => {}
    }
    ui.separator();
    item(ui, "Track details", TrackAction::OpenTrack);
    item(ui, &format!("Go to {}", track.user.username), TrackAction::OpenArtist);
    if ui.button("Open on SoundCloud ↗").clicked() {
        ui.ctx().open_url(OpenUrl::new_tab(&track.permalink_url));
        ui.close();
    }
    ui.separator();
    ui.menu_button("Add to local playlist", |ui| {
        let names = super::local_page::PLAYLIST_NAMES.read().map(|n| n.clone()).unwrap_or_default();
        for (i, name) in names.iter().enumerate() {
            if ui.button(name).clicked() {
                action = Some(TrackAction::AddToLocal(Some(i)));
                ui.close();
            }
        }
        if !names.is_empty() {
            ui.separator();
        }
        if ui.button("New playlist").clicked() {
            action = Some(TrackAction::AddToLocal(None));
            ui.close();
        }
    });
    if in_local_list {
        if ui.button("Remove from this playlist").clicked() {
            action = Some(TrackAction::RemoveFromLocal);
            ui.close();
        }
    }
    if track.local_path.is_none() {
        ui.separator();
        if ui.button("Not interested").clicked() {
            action = Some(TrackAction::NotInterested);
            ui.close();
        }
        if ui.button(format!("Block {}", track.user.username)).clicked() {
            action = Some(TrackAction::BlockArtist);
            ui.close();
        }
    }
    action
}

/// ♥ (liked) / ♡. Clicking opens the track on SoundCloud to (un)like it there.
pub fn heart(ui: &mut egui::Ui, liked: bool) -> Response {
    let (glyph, color, tip) = if liked {
        ("♥", accent(), "Liked. Opens SoundCloud to unlike")
    } else {
        ("♡", muted(), "Opens SoundCloud to like")
    };
    ui.add(egui::Label::new(RichText::new(glyph).color(color).size(16.0)).sense(Sense::click()))
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(tip)
}

/// A muted label that underlines on hover; returns true when clicked.
fn clickable_label(ui: &mut egui::Ui, text: &str, tip: &str) -> bool {
    let resp = ui
        .add(egui::Label::new(RichText::new(text).color(muted())).sense(Sense::click()).truncate())
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(tip);
    if resp.hovered() {
        ui.painter().hline(resp.rect.x_range(), resp.rect.bottom(), Stroke::new(1.0, muted()));
    }
    resp.clicked()
}

/// A person in search results. Returns true when clicked (open their tracks).
pub fn user_row(ui: &mut egui::Ui, art: &ArtCache, user: &User) -> bool {
    let (resp, inner) = row_frame(ui);
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let mut child = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::right_to_left(Align::Center)));
    let ui = &mut child;
    open_on_soundcloud(ui, &user.permalink_url);
    ui.add_space(6.0);
    let mut stats = Vec::new();
    if let Some(n) = user.followers_count {
        stats.push(format!("{} followers", compact(n)));
    }
    if let Some(n) = user.track_count {
        stats.push(format!("{n} tracks"));
    }
    ui.label(RichText::new(stats.join(" · ")).color(muted()));
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        let _ = artwork(ui, art, user.avatar_url.as_deref(), art_size(), "t67x67");
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(&user.username).strong()).truncate().selectable(false));
    });
    resp.clicked()
}

/// 1234 -> "1.2K", 2_500_000 -> "2.5M".
pub fn compact(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}K", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

pub fn playlist_row(ui: &mut egui::Ui, art: &ArtCache, playlist: &Playlist, note: Option<&str>) -> Response {
    let (resp, inner) = row_frame(ui);
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let mut child = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::right_to_left(Align::Center)));
    let ui = &mut child;
    open_on_soundcloud(ui, &playlist.permalink_url);
    ui.add_space(6.0);
    ui.label(RichText::new(format!("{} tracks", playlist.track_count)).color(muted()));
    if playlist.is_album {
        tag(ui, "Album");
    }
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        artwork(ui, art, playlist.artwork(), art_size(), "t67x67");
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.add_space(1.0);
            ui.add(egui::Label::new(RichText::new(&playlist.title).strong()).truncate().selectable(false));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                link_label(ui, &playlist.user.username, &playlist.user.permalink_url);
                if let Some(n) = note {
                    ui.label(RichText::new(format!("· {n}")).color(muted()).small());
                }
            });
        });
    });
    resp
}

/// Placeholder row while a playlist track is still being fetched.
pub fn pending_row(ui: &mut egui::Ui, label: &str) {
    let (_, inner) = row_frame(ui);
    let mut child = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)));
    let (r, _) = child.allocate_exact_size(vec2(art_size(), art_size()), Sense::hover());
    child.painter().rect_filled(r, 4.0, surface());
    child.add_space(4.0);
    child.label(RichText::new(label).color(muted()));
}

#[cfg(test)]
mod tests {
    #[test]
    fn durations() {
        assert_eq!(super::format_duration(105_646), "1:45");
        assert_eq!(super::format_duration(3_725_000), "1:02:05");
        assert_eq!(super::compact(999), "999");
        assert_eq!(super::compact(1_234), "1.2K");
        assert_eq!(super::compact(2_500_000), "2.5M");
    }
}

/// ▶ or ⏸ drawn as shapes, centred exactly on `center` (font glyphs sit
/// off-centre by a few pixels). `scale` 1.0 is about 18 px tall.
pub fn play_pause_icon(painter: &egui::Painter, center: egui::Pos2, pause: bool, scale: f32, color: egui::Color32) {
    let v = |x: f32, y: f32| egui::vec2(x * scale, y * scale);
    if pause {
        // Snapped to whole physical pixels with one shared width: otherwise a
        // centre between pixels anti-aliases one bar thinner than the other.
        let ppp = painter.ctx().pixels_per_point();
        let px = |points: f32| (points * ppp).round().max(1.0);
        let (w, h, gap) = (px(5.0 * scale), px(16.0 * scale), px(4.0 * scale));
        let left = (center.x * ppp - (2.0 * w + gap) / 2.0).round();
        let top = (center.y * ppp - h / 2.0).round();
        for x in [left, left + w + gap] {
            let bar = egui::Rect::from_min_size(egui::pos2(x / ppp, top / ppp), egui::vec2(w / ppp, h / ppp));
            painter.rect_filled(bar, 1.0 * scale, color);
        }
    } else {
        // Spans -6..9, so its box sits 1.5 right of centre: about a tenth of
        // its width, which is what reads as centred for a play triangle.
        let points = vec![center + v(-6.0, -9.0), center + v(-6.0, 9.0), center + v(9.0, 0.0)];
        painter.add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
    }
}
