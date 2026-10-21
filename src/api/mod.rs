//! Everything that talks to SoundCloud's (private, website) API lives under
//! here, so when they change something there is one place to fix.

pub mod client;
pub mod models;

use std::collections::{HashMap, HashSet};

pub use client::{Api, ApiError};
use models::{Page, Playlist, Track, User};

const PAGE: u32 = 50;
/// `/tracks?ids=` accepts at most this many ids.
pub const IDS_PER_REQUEST: usize = 50;

impl Api {
    /// The signed-in account.
    pub fn me(&self) -> Result<User, ApiError> {
        self.get("/me")
    }

    /// Short-lived link to the original file of a track the artist made
    /// downloadable (needs a signed-in account).
    pub fn download_link(&self, track_id: u64) -> Result<String, ApiError> {
        #[derive(serde::Deserialize)]
        struct Link {
            #[serde(rename = "redirectUri")]
            redirect_uri: String,
        }
        Ok(self.get::<Link>(&format!("/tracks/{track_id}/download"))?.redirect_uri)
    }

    /// First page of the user's liked tracks.
    pub fn likes_url(user_id: u64) -> String {
        format!("/users/{user_id}/track_likes?limit={PAGE}")
    }

    /// The Stream feed (tracks and playlists posted or reposted by people you follow).
    pub fn stream_url() -> String {
        format!("/stream?limit={PAGE}")
    }

    /// Tracks and playlists the user has reposted.
    pub fn reposts_url(user_id: u64) -> String {
        format!("/stream/users/{user_id}/reposts?limit={PAGE}")
    }

    /// The user's own and liked playlists/albums.
    pub fn library_url() -> String {
        format!("/me/library/all?limit={PAGE}")
    }

    /// Search results for `query` (first page).
    pub fn search_url(kind: SearchKind, query: &str) -> String {
        let what = match kind {
            SearchKind::Tracks => "tracks",
            SearchKind::People => "users",
            SearchKind::Playlists => "playlists_without_albums",
            SearchKind::Albums => "albums",
        };
        format!("/search/{what}?q={}&limit={PAGE}", encode(query))
    }

    /// The personalised home page: mixes, stations, recommendations.
    pub fn home_url() -> String {
        "/mixed-selections?limit=20".to_string()
    }

    /// Tracks similar to `track_id` (what SoundCloud's autoplay plays next).
    pub fn related(&self, track_id: u64) -> Result<Vec<Track>, ApiError> {
        let page: Page<Track> = self.get(&format!("/tracks/{track_id}/related?limit=30"))?;
        Ok(page.collection)
    }

    /// One track in full (counts, description...).
    pub fn track(&self, track_id: u64) -> Result<Track, ApiError> {
        self.get(&format!("/tracks/{track_id}"))
    }

    /// Public playlists that include the track, from `url` (None = the
    /// first page). SoundCloud drops private/deleted playlists from each page
    /// after picking it, so pages come back short: this keeps following
    /// `next_href` until it has at least `want` (or runs out). Returns the
    /// playlists and where to continue from.
    pub fn track_playlists(
        &self,
        track_id: u64,
        url: Option<String>,
        want: usize,
    ) -> Result<(Vec<Playlist>, Option<String>), ApiError> {
        let mut url = Some(url.unwrap_or_else(|| format!("/tracks/{track_id}/playlists_without_albums?limit=50")));
        let mut found = Vec::new();
        for _ in 0..6 {
            let Some(u) = url.take() else { break };
            let page: Page<Playlist> = self.get(&u)?;
            found.extend(page.collection);
            url = page.next_href;
            if found.len() >= want {
                break;
            }
        }
        Ok((found, url))
    }

    /// Albums/EPs the track is on.
    pub fn track_albums(&self, track_id: u64) -> Result<Vec<Playlist>, ApiError> {
        let page: Page<Playlist> = self.get(&format!("/tracks/{track_id}/albums?limit=10"))?;
        Ok(page.collection)
    }

    /// Tracks the user played recently, newest first.
    pub fn play_history_url() -> String {
        "/me/play-history/tracks?limit=50".to_string()
    }

    /// An artist's own uploads.
    pub fn user_tracks_url(user_id: u64) -> String {
        format!("/users/{user_id}/tracks?limit={PAGE}")
    }

    /// Ids of every track the user has liked (for the hearts).
    pub fn liked_ids(&self) -> Result<HashSet<u64>, ApiError> {
        let mut ids = HashSet::new();
        let mut url = Some("/me/track_likes/ids?limit=5000".to_string());
        while let Some(u) = url {
            let page: Page<u64> = self.get(&u)?;
            ids.extend(page.collection);
            url = page.next_href;
        }
        Ok(ids)
    }

    /// Full track objects for up to `IDS_PER_REQUEST` ids. Tracks that no
    /// longer exist are simply missing from the result.
    pub fn tracks(&self, ids: &[u64]) -> Result<HashMap<u64, Track>, ApiError> {
        let ids: Vec<String> = ids.iter().take(IDS_PER_REQUEST).map(u64::to_string).collect();
        let list: Vec<Track> = self.get(&format!("/tracks?ids={}", ids.join(",")))?;
        Ok(list.into_iter().map(|t| (t.id, t)).collect())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchKind {
    Tracks,
    People,
    Playlists,
    Albums,
}

/// Percent-encodes a query-string value.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn query_encoding() {
        assert_eq!(super::encode("lo fi & chill/ñ"), "lo%20fi%20%26%20chill%2F%C3%B1");
    }
}
