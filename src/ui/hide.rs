//! Things the user never wants to see: tracks marked "Not interested",
//! blocked artists and hidden keywords. Applied to every list, the home page,
//! related tracks and autoplay.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::library::Row;
use crate::api::models::{Playlist, Track};
use crate::config::Config;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedArtist {
    pub id: u64,
    pub name: String,
}

#[derive(Default)]
pub struct Hidden {
    pub tracks: Vec<(u64, String)>,
    pub artists: Vec<BlockedArtist>,
    pub keywords: Vec<String>,
    track_ids: HashSet<u64>,
    artist_ids: HashSet<u64>,
    /// Lower-cased keywords.
    words: Vec<String>,
}

impl Hidden {
    pub fn load(c: &Config) -> Self {
        let mut h = Self { tracks: c.hidden_tracks.clone(), artists: c.blocked_artists.clone(), keywords: c.hidden_keywords.clone(), ..Self::default() };
        h.reindex();
        h
    }

    fn reindex(&mut self) {
        self.track_ids = self.tracks.iter().map(|(id, _)| *id).collect();
        self.artist_ids = self.artists.iter().map(|a| a.id).collect();
        self.words = self.keywords.iter().map(|k| k.trim().to_lowercase()).filter(|k| !k.is_empty()).collect();
    }

    pub fn save(&self) {
        let mut c = Config::load();
        c.hidden_tracks = self.tracks.clone();
        c.blocked_artists = self.artists.clone();
        c.hidden_keywords = self.keywords.clone();
        c.save();
    }

    fn has_word(&self, text: &str) -> bool {
        if self.words.is_empty() {
            return false;
        }
        let text = text.to_lowercase();
        self.words.iter().any(|w| text.contains(w.as_str()))
    }

    pub fn track(&self, t: &Track) -> bool {
        self.track_ids.contains(&t.id)
            || self.artist_ids.contains(&t.user.id)
            || self.has_word(&t.title)
            || self.has_word(&t.user.username)
    }

    pub fn playlist(&self, p: &Playlist) -> bool {
        self.artist_ids.contains(&p.user.id) || self.has_word(&p.title)
    }

    pub fn row(&self, r: &Row) -> bool {
        match r {
            Row::Track { track, .. } => self.track(track),
            Row::Playlist { playlist, .. } => self.playlist(playlist),
            Row::User(u) => self.artist_ids.contains(&u.id) || self.has_word(&u.username),
        }
    }

    pub fn hide_track(&mut self, t: &Track) {
        if !self.track_ids.contains(&t.id) {
            self.tracks.push((t.id, format!("{} – {}", t.user.username, t.title)));
            self.reindex();
        }
    }

    pub fn block_artist(&mut self, id: u64, name: &str) {
        if id != 0 && !self.artist_ids.contains(&id) {
            self.artists.push(BlockedArtist { id, name: name.to_string() });
            self.reindex();
        }
    }

    pub fn add_keyword(&mut self, word: &str) {
        let word = word.trim();
        if !word.is_empty() && !self.keywords.iter().any(|k| k.eq_ignore_ascii_case(word)) {
            self.keywords.push(word.to_string());
            self.reindex();
        }
    }

    pub fn unhide_track(&mut self, i: usize) {
        if i < self.tracks.len() {
            self.tracks.remove(i);
            self.reindex();
        }
    }

    pub fn unblock_artist(&mut self, i: usize) {
        if i < self.artists.len() {
            self.artists.remove(i);
            self.reindex();
        }
    }

    pub fn remove_keyword(&mut self, i: usize) {
        if i < self.keywords.len() {
            self.keywords.remove(i);
            self.reindex();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: u64, title: &str, user_id: u64, user: &str) -> Track {
        serde_json::from_str(&format!(r#"{{"id":{id},"title":"{title}","user":{{"id":{user_id},"username":"{user}"}}}}"#)).unwrap()
    }

    #[test]
    fn hides_by_track_artist_and_keyword() {
        let mut h = Hidden::default();
        let a = track(1, "Song", 10, "Alice");
        let b = track(2, "Nightcore Remix", 20, "Bob");
        assert!(!h.track(&a) && !h.track(&b));
        h.hide_track(&a);
        assert!(h.track(&a));
        h.unhide_track(0);
        h.block_artist(10, "Alice");
        assert!(h.track(&a) && !h.track(&b));
        h.add_keyword("  NIGHTCORE ");
        assert!(h.track(&b));
        h.add_keyword("nightcore"); // duplicates ignored
        assert_eq!(h.keywords.len(), 1);
        h.remove_keyword(0);
        assert!(!h.track(&b));
    }
}
