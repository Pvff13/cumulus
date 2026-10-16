//! Row widgets for track and playlist lists. Every row credits the uploader
//! (linked to their SoundCloud profile) and links to the item's own page.

use eframe::egui::{
    self, Align, CursorIcon, Layout, OpenUrl, Rect, Response, RichText, Sense, Stroke, UiBuilder, vec2,
};

use std::collections::HashSet;

use super::theme::{ACCENT, HOVER, MUTED, SURFACE, TAG};
use crate::api::models::{Availability, Playlist, Track, User, sized_art};
use crate::cache::art::ArtCache;

pub const ROW_HEIGHT: f32 = 52.0;
const ART: f32 = 40.0;

/// Allocates a full-width row and returns (response, inner rect).
fn row_frame(ui: &mut egui::Ui) -> (Response, Rect) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, HOVER);
    }
    (resp, rect.shrink2(vec2(10.0, 6.0)))
}

pub fn artwork(ui: &mut egui::Ui, art: &ArtCache, url: Option<&str>, size: f32, variant: &str) -> Response {
    let (r, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    match url.and_then(|u| art.get(&sized_art(u, variant))) {
        Some(t) => egui::Image::new(&t).corner_radius(4).paint_at(ui, r),
        None => {
            ui.painter().rect_filled(r, 4.0, SURFACE);
        }
    }
    resp
}

/// Artwork with a ▶ overlay while the row is hovered. Returns true if clicked.
fn play_artwork(ui: &mut egui::Ui, art: &ArtCache, url: Option<&str>, row_hovered: bool) -> bool {
    let resp = artwork(ui, art, url, ART, "t67x67");
    if row_hovered {
        let r = resp.rect;
        ui.painter().rect_filled(r, 4.0, egui::Color32::from_black_alpha(140));
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "▶", egui::FontId::proportional(18.0), egui::Color32::WHITE);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

/// A muted label that opens `url` in the browser when clicked.
pub fn link_label(ui: &mut egui::Ui, text: &str, url: &str) {
    if url.is_empty() {
        ui.label(RichText::new(text).color(MUTED));
        return;
    }
    let resp = ui
        .add(egui::Label::new(RichText::new(text).color(MUTED)).sense(Sense::click()).truncate())
        .on_hover_cursor(CursorIcon::PointingHand);
    if resp.hovered() {
        let r = resp.rect;
        ui.painter().hline(r.x_range(), r.bottom(), Stroke::new(1.0, MUTED));
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
        .add(egui::Label::new(RichText::new("↗").color(MUTED).size(15.0)).sense(Sense::click()))
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text("Open on SoundCloud");
    if resp.clicked() {
        ui.ctx().open_url(OpenUrl::new_tab(url));
    }
}

fn tag(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(TAG));
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
        if let Some(a) = track_menu(ui, track, liked) {
            action = Some(a);
        }
    });
    let mut child = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::right_to_left(Align::Center)));
    let ui = &mut child;
    open_on_soundcloud(ui, &track.permalink_url);
    ui.add_space(6.0);
    ui.label(RichText::new(format_duration(track.duration)).color(MUTED).monospace());
    ui.add_space(4.0);
    if (hovered || liked) && heart(ui, liked).clicked() {
        action = Some(TrackAction::LikeOnSoundCloud);
    }
    match availability {
        Availability::Blocked => tag(ui, "Not available"),
        Availability::Preview => tag(ui, "Preview"),
        Availability::Playable => {}
    }
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        if play_artwork(ui, cx.art, track.artwork(), hovered && availability != Availability::Blocked) {
            action = Some(TrackAction::Play);
        }
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.add_space(1.0);
            let mut title = RichText::new(&track.title).strong();
            if cx.current == Some(track.id) {
                title = title.color(ACCENT);
            }
            if availability == Availability::Blocked {
                title = title.color(MUTED).strikethrough();
            }
            ui.add(egui::Label::new(title).truncate().selectable(false));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if clickable_label(ui, &track.user.username, "Show this artist's tracks") {
                    action = Some(TrackAction::OpenArtist);
                }
                if let Some(n) = note {
                    ui.label(RichText::new(format!("· {n}")).color(MUTED).small());
                }
            });
        });
    });
    action
}

/// Right-click menu for a track.
pub fn track_menu(ui: &mut egui::Ui, track: &Track, liked: bool) -> Option<TrackAction> {
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
    ui.separator();
    item(ui, &format!("Go to {}", track.user.username), TrackAction::OpenArtist);
    if ui.button("Open on SoundCloud ↗").clicked() {
        ui.ctx().open_url(OpenUrl::new_tab(&track.permalink_url));
        ui.close();
    }
    action
}

/// ♥ (liked) / ♡. Clicking opens the track on SoundCloud to (un)like it there.
pub fn heart(ui: &mut egui::Ui, liked: bool) -> Response {
    let (glyph, color, tip) = if liked {
        ("♥", ACCENT, "Liked. Opens SoundCloud to unlike")
    } else {
        ("♡", MUTED, "Opens SoundCloud to like")
    };
    ui.add(egui::Label::new(RichText::new(glyph).color(color).size(16.0)).sense(Sense::click()))
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(tip)
}

/// A muted label that underlines on hover; returns true when clicked.
fn clickable_label(ui: &mut egui::Ui, text: &str, tip: &str) -> bool {
    let resp = ui
        .add(egui::Label::new(RichText::new(text).color(MUTED)).sense(Sense::click()).truncate())
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(tip);
    if resp.hovered() {
        ui.painter().hline(resp.rect.x_range(), resp.rect.bottom(), Stroke::new(1.0, MUTED));
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
    ui.label(RichText::new(stats.join(" · ")).color(MUTED));
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        let _ = artwork(ui, art, user.avatar_url.as_deref(), ART, "t67x67");
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
    ui.label(RichText::new(format!("{} tracks", playlist.track_count)).color(MUTED));
    if playlist.is_album {
        tag(ui, "Album");
    }
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        artwork(ui, art, playlist.artwork(), ART, "t67x67");
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.add_space(1.0);
            ui.add(egui::Label::new(RichText::new(&playlist.title).strong()).truncate().selectable(false));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                link_label(ui, &playlist.user.username, &playlist.user.permalink_url);
                if let Some(n) = note {
                    ui.label(RichText::new(format!("· {n}")).color(MUTED).small());
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
    let (r, _) = child.allocate_exact_size(vec2(ART, ART), Sense::hover());
    child.painter().rect_filled(r, 4.0, SURFACE);
    child.add_space(4.0);
    child.label(RichText::new(label).color(MUTED));
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
