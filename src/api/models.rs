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
    fn art_sizes() {
        assert_eq!(
            sized_art("https://i1.sndcdn.com/artworks-abc-large.jpg", "t67x67"),
            "https://i1.sndcdn.com/artworks-abc-t67x67.jpg"
        );
        assert_eq!(sized_art("https://x/y.png", "t67x67"), "https://x/y.png");
    }
}
