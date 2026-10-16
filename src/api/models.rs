use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // full_name is kept for a later profile view
pub struct User {
    pub id: u64,
    pub username: String,
    #[serde(default)]
    pub full_name: Option<String>,
    #[serde(default)]
    pub permalink_url: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub likes_count: Option<u64>,
    #[serde(default)]
    pub playlist_count: Option<u64>,
    #[serde(default)]
    pub followings_count: Option<u64>,
    #[serde(default)]
    pub followers_count: Option<u64>,
    #[serde(default)]
    pub track_count: Option<u64>,
}

/// The cut-down user object embedded in tracks, playlists and activities.
#[derive(Debug, Clone, Deserialize)]
pub struct MiniUser {
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub permalink_url: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // media/track_authorization/waveform_url are used from milestone 3
pub struct Track {
    pub id: u64,
    pub title: String,
    /// Milliseconds.
    #[serde(default)]
    pub duration: u64,
    #[serde(default)]
    pub permalink_url: String,
    #[serde(default)]
    pub artwork_url: Option<String>,
    pub user: MiniUser,
    /// ALLOW / MONETIZE play fully, SNIP is a 30 s preview (Go+), BLOCK is unavailable.
    #[serde(default)]
    pub policy: Option<String>,
    #[serde(default)]
    pub streamable: Option<bool>,
    #[serde(default)]
    pub media: Option<Media>,
    #[serde(default)]
    pub track_authorization: Option<String>,
    #[serde(default)]
    pub waveform_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Playable,
    /// Only a preview clip is streamable (Go+ content).
    Preview,
    /// Geo-blocked, removed or otherwise not streamable.
    Blocked,
}

impl Track {
    pub fn availability(&self) -> Availability {
        match self.policy.as_deref() {
            Some("BLOCK") => Availability::Blocked,
            Some("SNIP") => Availability::Preview,
            _ if self.streamable == Some(false) => Availability::Blocked,
            _ => Availability::Playable,
        }
    }

    /// Track artwork, falling back to the uploader's avatar like soundcloud.com does.
    pub fn artwork(&self) -> Option<&str> {
        self.artwork_url.as_deref().or(self.user.avatar_url.as_deref())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct Media {
    #[serde(default)]
    pub transcodings: Vec<Transcoding>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct Transcoding {
    pub url: String,
    #[serde(default)]
    pub preset: String,
    #[serde(default)]
    pub snipped: bool,
    pub format: TranscodingFormat,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct TranscodingFormat {
    pub protocol: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Playlist {
    pub id: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub permalink_url: String,
    #[serde(default)]
    pub artwork_url: Option<String>,
    pub user: MiniUser,
    #[serde(default)]
    pub track_count: u32,
    #[serde(default)]
    pub is_album: bool,
    /// Only present when fetching a single playlist.
    #[serde(default)]
    pub tracks: Vec<PlaylistEntry>,
}

impl Playlist {
    pub fn artwork(&self) -> Option<&str> {
        self.artwork_url.as_deref().or(self.user.avatar_url.as_deref())
    }
}

/// A single playlist response includes only its first few tracks in full;
/// the rest are `{id, ...}` stubs that are filled in via `/tracks?ids=`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PlaylistEntry {
    Full(Track),
    Stub { id: u64 },
}

/// One entry of the like list.
#[derive(Debug, Clone, Deserialize)]
pub struct TrackLike {
    pub track: Option<Track>,
}

/// Stream, reposts and library entries share this shape.
#[derive(Debug, Clone, Deserialize)]
pub struct Activity {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub track: Option<Track>,
    #[serde(default)]
    pub playlist: Option<Playlist>,
    #[serde(default)]
    pub user: Option<MiniUser>,
}

/// A page of a paginated collection.
#[derive(Debug, Clone, Deserialize)]
pub struct Page<T> {
    #[serde(default = "Vec::new")]
    pub collection: Vec<T>,
    #[serde(default)]
    pub next_href: Option<String>,
}

/// One row of the SoundCloud home page ("Mixed for you", "Made for you", ...).
#[derive(Debug, Clone, Deserialize)]
pub struct Selection {
    #[serde(default)]
    pub title: String,
    /// e.g. `soundcloud:selections:your-moods`
    #[serde(default)]
    pub urn: String,
    pub items: Page<HomeItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind")]
pub enum HomeItem {
    /// A generated mix / station ("Your Mix 1", "Daily Drops", ...).
    #[serde(rename = "system-playlist")]
    Mix(SystemPlaylist),
    #[serde(rename = "playlist")]
    Playlist(Playlist),
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemPlaylist {
    pub urn: String,
    #[serde(default)]
    pub title: String,
    /// Usually the artists in the mix.
    #[serde(default)]
    pub short_description: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub artwork_url: Option<String>,
    #[serde(default)]
    pub calculated_artwork_url: Option<String>,
    #[serde(default)]
    pub permalink_url: String,
    /// Track id stubs; fetched in full as they come into view.
    #[serde(default)]
    pub tracks: Vec<PlaylistEntry>,
}

impl SystemPlaylist {
    pub fn artwork(&self) -> Option<&str> {
        self.calculated_artwork_url.as_deref().or(self.artwork_url.as_deref())
    }

    pub fn subtitle(&self) -> &str {
        self.short_description.as_deref().or(self.description.as_deref()).unwrap_or_default()
    }

    pub fn track_ids(&self) -> Vec<u64> {
        self.tracks
            .iter()
            .map(|e| match e {
                PlaylistEntry::Full(t) => t.id,
                PlaylistEntry::Stub { id } => *id,
            })
            .collect()
    }

    /// Mixes have no numeric id; derive a stable one from the URN so a mix
    /// can be shown with the playlist view.
    pub fn local_id(&self) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in self.urn.bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
        }
        h | 1 << 63 // never collides with real (small) playlist ids
    }

    /// As a playlist header (title, artwork, link back to SoundCloud).
    pub fn as_playlist(&self) -> Playlist {
        Playlist {
            id: self.local_id(),
            title: self.title.clone(),
            permalink_url: self.permalink_url.clone(),
            artwork_url: self.artwork().map(str::to_string),
            user: MiniUser {
                id: 0,
                username: "SoundCloud".into(),
                permalink_url: "https://soundcloud.com/discover".into(),
                avatar_url: None,
            },
            track_count: self.tracks.len() as u32,
            is_album: false,
            tracks: Vec::new(),
        }
    }
}

/// Rewrites a SoundCloud artwork/avatar URL to a given size variant,
/// e.g. `t67x67` or `t300x300` (the API returns the 100 px `large` one).
pub fn sized_art(url: &str, size: &str) -> String {
    match url.rfind("-large.") {
        Some(i) => format!("{}-{size}.{}", &url[..i], &url[i + "-large.".len()..]),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_entries_full_and_stub() {
        let json = r#"{"id":1,"title":"p","user":{"id":2,"username":"u"},"track_count":2,"tracks":[
            {"id":10,"title":"t","user":{"id":3,"username":"a"},"policy":"MONETIZE"},
            {"id":11,"kind":"track","monetization_model":"BLACKBOX","policy":"SNIP"}]}"#;
        let p: Playlist = serde_json::from_str(json).unwrap();
        assert!(matches!(p.tracks[0], PlaylistEntry::Full(_)));
        assert!(matches!(p.tracks[1], PlaylistEntry::Stub { id: 11 }));
    }

    #[test]
    fn home_items() {
        let json = r#"{"collection":[{"title":"Mixed for you","items":{"collection":[
            {"kind":"system-playlist","urn":"soundcloud:system-playlists:your-moods:1:1","title":"Your Mix 1",
             "short_description":"a, b","tracks":[{"id":5,"kind":"track","policy":"ALLOW"}]},
            {"kind":"playlist","id":7,"title":"p","user":{"username":"u"},"track_count":2},
            {"kind":"something-new"}]}}]}"#;
        let page: Page<Selection> = serde_json::from_str(json).unwrap();
        let items = &page.collection[0].items.collection;
        let HomeItem::Mix(m) = &items[0] else { panic!("mix") };
        assert_eq!((m.track_ids(), m.subtitle()), (vec![5], "a, b"));
        assert!(m.local_id() > 1 << 62);
        assert!(matches!(items[1], HomeItem::Playlist(_)));
        assert!(matches!(items[2], HomeItem::Other));
    }

    #[test]
    fn art_sizes() {
        assert_eq!(
            sized_art("https://i1.sndcdn.com/artworks-abc-large.jpg", "t67x67"),
            "https://i1.sndcdn.com/artworks-abc-t67x67.jpg"
        );
        assert_eq!(sized_art("https://x/y.png", "t67x67"), "https://x/y.png");
    }
}
