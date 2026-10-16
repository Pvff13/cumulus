//! List state for the library views: paginated feeds (Likes, Stream,
//! Reposts, Playlists) and a single opened playlist.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::api::models::{Activity, Page, Playlist, PlaylistEntry, Track, TrackLike, User};
use crate::api::{ApiError, SearchKind, client};
use crate::cache::meta;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FeedKind {
    Likes,
    Stream,
    Reposts,
    Library,
    Search(SearchKind),
    /// An artist's uploads.
    Artist(u64),
}

impl FeedKind {
    /// Only the sidebar lists are saved for offline / instant start-up.
    fn cache_key(self, user_id: u64) -> Option<String> {
        match self {
            Self::Likes | Self::Stream | Self::Reposts | Self::Library => Some(format!("{self:?}-{user_id}")),
            Self::Search(_) | Self::Artist(_) => None,
        }
    }
}

pub enum Row {
    Track { track: Arc<Track>, note: Option<String> },
    Playlist { playlist: Arc<Playlist>, note: Option<String> },
    User(Arc<User>),
}

/// One page converted to rows.
pub struct RowPage {
    pub rows: Vec<Row>,
    pub next: Option<String>,
}

#[derive(Default)]
pub struct Feed {
    pub rows: Vec<Row>,
    pub next: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    /// Currently showing the saved copy (offline or still refreshing).
    pub from_cache: bool,
    pub started: bool,
    /// Bumped on refresh so late responses from an older load are ignored.
    pub generation: u32,
}

impl Feed {
    /// True when the list is scrolled near its end and more can be loaded.
    pub fn wants_more(&self, visible_end: usize) -> bool {
        !self.loading && self.next.is_some() && visible_end + 15 >= self.rows.len()
    }

    pub fn apply(&mut self, first: bool, cached: bool, result: Result<RowPage, ApiError>) {
        match result {
            Ok(page) => {
                if first {
                    self.rows = page.rows;
                } else {
                    self.rows.extend(page.rows);
                }
                self.next = page.next;
                self.from_cache = cached;
                self.error = None;
                if !cached {
                    self.loading = false;
                }
            }
            Err(e) => {
                self.loading = false;
                self.error = Some(match (&e, self.rows.is_empty()) {
                    (ApiError::Offline(_), false) => "Offline - showing your saved library".into(),
                    _ => e.to_string(),
                });
            }
        }
    }
}

/// Fetches one page of a feed. For the first page, the raw response is
/// also saved so it can be shown instantly next time (see `cached_first_page`).
pub fn fetch_page(
    api: &crate::api::Api,
    kind: FeedKind,
    user_id: u64,
    url: &str,
    first: bool,
) -> Result<RowPage, ApiError> {
    let body = api.get_raw(url)?;
    let page = parse_page(kind, &body)?;
    if let Some(key) = kind.cache_key(user_id).filter(|_| first) {
        meta::save(&key, &body);
    }
    Ok(page)
}

pub fn cached_first_page(kind: FeedKind, user_id: u64) -> Option<RowPage> {
    let body = meta::load(&kind.cache_key(user_id)?)?;
    parse_page(kind, &body).ok()
}

fn parse_page(kind: FeedKind, body: &str) -> Result<RowPage, ApiError> {
    match kind {
        FeedKind::Search(SearchKind::Tracks) | FeedKind::Artist(_) => {
            let page: Page<Track> = client::parse(body)?;
            let rows = page.collection.into_iter().map(|t| Row::Track { track: Arc::new(t), note: None }).collect();
            return Ok(RowPage { rows, next: page.next_href });
        }
        FeedKind::Search(SearchKind::People) => {
            let page: Page<User> = client::parse(body)?;
            let rows = page.collection.into_iter().map(|u| Row::User(Arc::new(u))).collect();
            return Ok(RowPage { rows, next: page.next_href });
        }
        FeedKind::Search(SearchKind::Playlists | SearchKind::Albums) => {
            let page: Page<Playlist> = client::parse(body)?;
            let rows = page
                .collection
                .into_iter()
                .map(|p| Row::Playlist { playlist: Arc::new(p), note: None })
                .collect();
            return Ok(RowPage { rows, next: page.next_href });
        }
        FeedKind::Likes | FeedKind::Stream | FeedKind::Reposts | FeedKind::Library => {}
    }
    if kind == FeedKind::Likes {
        let page: Page<TrackLike> = client::parse(body)?;
        let rows = page
            .collection
            .into_iter()
            .filter_map(|l| l.track)
            .map(|t| Row::Track { track: Arc::new(t), note: None })
            .collect();
        return Ok(RowPage { rows, next: page.next_href });
    }
    let page: Page<Activity> = client::parse(body)?;
    let rows = page.collection.into_iter().filter_map(|a| activity_row(kind, a)).collect();
    Ok(RowPage { rows, next: page.next_href })
}

fn activity_row(kind: FeedKind, a: Activity) -> Option<Row> {
    let by = a.user.as_ref().map(|u| u.username.clone()).unwrap_or_default();
    let reposted = a.kind.ends_with("-repost") && kind == FeedKind::Stream;
    let note = match a.kind.as_str() {
        _ if reposted => Some(format!("reposted by {by}")),
        "playlist-like" | "album-like" | "system-playlist-like" => Some("liked".into()),
        _ => None,
    };
    if let Some(track) = a.track {
        return Some(Row::Track { track: Arc::new(track), note });
    }
    a.playlist.map(|p| Row::Playlist { playlist: Arc::new(p), note })
}

/// A single playlist with its track list; falls back to the saved copy when offline.
pub fn fetch_playlist(api: &crate::api::Api, id: u64) -> Result<Playlist, ApiError> {
    let key = format!("playlist-{id}");
    match api.get_raw(&format!("/playlists/{id}")) {
        Ok(body) => {
            let p = client::parse(&body)?;
            meta::save(&key, &body);
            Ok(p)
        }
        Err(e @ ApiError::Offline(_)) => match meta::load(&key) {
            Some(body) => client::parse(&body),
            None => Err(e),
        },
        Err(e) => Err(e),
    }
}

pub enum Entry {
    Loaded(Arc<Track>),
    Pending(u64),
    /// Deleted or private tracks that no longer resolve.
    Missing,
}

/// An opened playlist. Only the first few tracks arrive in full; the rest are
/// filled in batches as they scroll into view.
pub struct PlaylistView {
    pub playlist: Arc<Playlist>,
    pub entries: Vec<Entry>,
    pub loading: bool,
    pub error: Option<String>,
    requested: HashSet<u64>,
    /// After a failed batch, wait before asking again.
    retry_at: Option<Instant>,
}

impl PlaylistView {
    pub fn new(playlist: Arc<Playlist>) -> Self {
        Self { playlist, entries: Vec::new(), loading: true, error: None, requested: HashSet::new(), retry_at: None }
    }

    pub fn apply_playlist(&mut self, result: Result<Playlist, ApiError>) {
        self.loading = false;
        match result {
            Ok(mut p) => {
                self.entries = p
                    .tracks
                    .drain(..)
                    .map(|e| match e {
                        PlaylistEntry::Full(t) => Entry::Loaded(Arc::new(t)),
                        PlaylistEntry::Stub { id } => Entry::Pending(id),
                    })
                    .collect();
                self.playlist = Arc::new(p);
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Ids in `range` (plus a look-ahead) that still need fetching, in
    /// request-sized chunks. Marks them as requested.
    pub fn take_missing(&mut self, range: std::ops::Range<usize>) -> Vec<Vec<u64>> {
        if self.retry_at.is_some_and(|t| Instant::now() < t) {
            return Vec::new();
        }
        let end = (range.end + 40).min(self.entries.len());
        let ids: Vec<u64> = self.entries[range.start.min(end)..end]
            .iter()
            .filter_map(|e| match e {
                Entry::Pending(id) if !self.requested.contains(id) => Some(*id),
                _ => None,
            })
            .collect();
        self.requested.extend(&ids);
        ids.chunks(crate::api::IDS_PER_REQUEST).map(<[u64]>::to_vec).collect()
    }

    pub fn apply_tracks(&mut self, asked: &[u64], result: Result<HashMap<u64, Track>, ApiError>) {
        match result {
            Ok(mut found) => {
                for e in &mut self.entries {
                    if let Entry::Pending(id) = *e {
                        if let Some(t) = found.remove(&id) {
                            *e = Entry::Loaded(Arc::new(t));
                        } else if asked.contains(&id) {
                            *e = Entry::Missing;
                        }
                    }
                }
            }
            // Let them be requested again next time they're on screen.
            Err(_) => {
                self.retry_at = Some(Instant::now() + Duration::from_secs(10));
                for id in asked {
                    self.requested.remove(id);
                }
            }
        }
    }
}
