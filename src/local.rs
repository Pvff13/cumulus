//! The user's own music: folders to scan, the scanned library (cached so
//! start-up is instant) and local playlists, which can mix local files with
//! SoundCloud tracks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::api::models::Track;
use crate::player::local::{EXTENSIONS, read_tags};

/// Local tracks get ids with this bit set (and the top bit clear, which
/// marks mixes), so they never clash with SoundCloud's.
pub const LOCAL_BIT: u64 = 1 << 62;
/// Stop scanning after this many files.
const MAX_FILES: usize = 50_000;

pub fn is_local(id: u64) -> bool {
    id & LOCAL_BIT != 0 && id & (1 << 63) == 0
}

/// Stable id for a file path.
pub fn id_for(path: &str) -> u64 {
    // FNV-1a over the lower-cased path.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.to_lowercase().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h & !(1 << 63)) | LOCAL_BIT
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LocalTrack {
    pub path: String,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub album: String,
    pub duration_ms: u64,
    /// Cover picture file (extracted from the tags, or cover.jpg next to it).
    #[serde(default)]
    pub art: Option<String>,
    /// File modification time (s), to skip re-reading unchanged files.
    #[serde(default)]
    pub modified: u64,
}

impl LocalTrack {
    pub fn id(&self) -> u64 {
        id_for(&self.path)
    }

    /// As a `Track`, so lists, the queue and the player treat it like any other.
    pub fn to_track(&self) -> Track {
        let art = self.art.as_ref().map(|p| format!("file:///{}", p.replace('\\', "/")));
        let json = serde_json::json!({
            "id": self.id(),
            "title": self.title,
            "duration": self.duration_ms,
            "artwork_url": art,
            "user": { "id": 0, "username": self.artist },
            "policy": "ALLOW",
        });
        let mut t: Track = serde_json::from_value(json).expect("valid track json");
        t.local_path = Some(self.path.clone());
        t
    }
}

/// Folders and loose files the user added, and what scanning found.
#[derive(Default, Serialize, Deserialize)]
pub struct Library {
    pub folders: Vec<String>,
    pub files: Vec<String>,
    pub tracks: Vec<LocalTrack>,
    /// Debug builds: a throwaway library that's never written to disk.
    #[serde(skip)]
    pub no_save: bool,
}

fn library_file() -> PathBuf {
    crate::paths::data_dir().join("local-library.json")
}

fn playlists_file() -> PathBuf {
    crate::paths::data_dir().join("local-playlists.json")
}

fn write_json<T: Serialize>(path: &Path, value: &T) {
    if let Ok(text) = serde_json::to_string(value) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

impl Library {
    pub fn load() -> Self {
        std::fs::read_to_string(library_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if self.no_save {
            return;
        }
        write_json(&library_file(), self);
    }

    pub fn find(&self, id: u64) -> Option<&LocalTrack> {
        self.tracks.iter().find(|t| t.id() == id)
    }
}

fn is_music(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn modified(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

fn walk(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if depth > 12 || out.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, depth + 1, out);
        } else if is_music(&p) {
            out.push(p);
        }
    }
}

/// A picture to use as the cover: the embedded one (saved to the cache), or
/// cover/folder/front.jpg|png next to the file.
fn cover_for(path: &Path, picture: Option<Vec<u8>>) -> Option<String> {
    if let Some(bytes) = picture.filter(|b| b.len() > 64) {
        let ext = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) { "png" } else { "jpg" };
        let dest = crate::paths::cache_dir("local-art").join(format!("{:x}.{ext}", id_for(&path.display().to_string())));
        if dest.exists() || std::fs::write(&dest, &bytes).is_ok() {
            return Some(dest.display().to_string());
        }
    }
    let dir = path.parent()?;
    ["cover", "folder", "front", "Cover", "Folder", "Front"]
        .iter()
        .flat_map(|n| ["jpg", "jpeg", "png"].map(|e| dir.join(format!("{n}.{e}"))))
        .find(|p| p.exists())
        .map(|p| p.display().to_string())
}

/// Reads one file's tags into a library entry (falls back to the file name).
pub fn describe(path: &Path) -> Option<LocalTrack> {
    let p = path.display().to_string();
    let tags = read_tags(&p).ok()?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Unknown").to_string();
    // "Artist - Title.mp3" when there are no tags.
    let (guess_artist, guess_title) = match stem.split_once(" - ") {
        Some((a, t)) => (a.trim().to_string(), t.trim().to_string()),
        None => (String::new(), stem.clone()),
    };
    let nonempty = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    Some(LocalTrack {
        title: nonempty(tags.title).unwrap_or(guess_title),
        artist: nonempty(tags.artist).or_else(|| (!guess_artist.is_empty()).then_some(guess_artist)).unwrap_or_else(|| "Unknown artist".into()),
        album: nonempty(tags.album).unwrap_or_default(),
        duration_ms: tags.duration_ms,
        art: cover_for(path, tags.picture),
        modified: modified(path),
        path: p,
    })
}

/// Scans the folders and loose files. Unchanged files keep their old entry
/// (only new or edited files are read again).
pub fn scan(folders: &[String], files: &[String], old: &[LocalTrack]) -> Vec<LocalTrack> {
    let mut paths = Vec::new();
    for f in folders {
        walk(Path::new(f), 0, &mut paths);
    }
    paths.extend(files.iter().map(PathBuf::from).filter(|p| p.is_file()));
    let known: HashMap<&str, &LocalTrack> = old.iter().map(|t| (t.path.as_str(), t)).collect();
    let mut seen = std::collections::HashSet::new();
    paths
        .into_iter()
        .filter(|p| seen.insert(p.display().to_string().to_lowercase()))
        .filter_map(|p| {
            let key = p.display().to_string();
            match known.get(key.as_str()) {
                Some(t) if t.modified == modified(&p) => Some((*t).clone()),
                _ => describe(&p),
            }
        })
        .collect()
}

/// An entry in a local playlist.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Entry {
    /// A file on this PC.
    Local { path: String },
    /// A SoundCloud track (shown from these details; played by id).
    Cloud { id: u64, title: String, artist: String, duration_ms: u64, artwork: Option<String>, permalink: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LocalPlaylist {
    pub name: String,
    pub entries: Vec<Entry>,
}

pub fn load_playlists() -> Vec<LocalPlaylist> {
    std::fs::read_to_string(playlists_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn save_playlists(lists: &[LocalPlaylist]) {
    write_json(&playlists_file(), &lists);
}

impl Entry {
    pub fn of(track: &Track) -> Self {
        match &track.local_path {
            Some(path) => Self::Local { path: path.clone() },
            None => Self::Cloud {
                id: track.id,
                title: track.title.clone(),
                artist: track.user.username.clone(),
                duration_ms: track.duration,
                artwork: track.artwork_url.clone(),
                permalink: track.permalink_url.clone(),
            },
        }
    }

    /// The track to show and play. SoundCloud entries are stand-ins until
    /// the queue fetches their streams (`full` is false for them).
    pub fn track(&self, library: &Library) -> Option<(Arc<Track>, bool)> {
        match self {
            Self::Local { path } => {
                let lt = library.find(id_for(path)).cloned().or_else(|| describe(Path::new(path)))?;
                Some((Arc::new(lt.to_track()), true))
            }
            Self::Cloud { id, title, artist, duration_ms, artwork, permalink } => {
                let json = serde_json::json!({
                    "id": id, "title": title, "duration": duration_ms, "artwork_url": artwork,
                    "permalink_url": permalink, "user": { "id": 0, "username": artist },
                });
                serde_json::from_value(json).ok().map(|t| (Arc::new(t), false))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        let id = id_for(r"C:\Music\a.mp3");
        assert!(is_local(id));
        assert_eq!(id, id_for(r"c:\music\A.mp3")); // Windows paths ignore case
        assert_ne!(id, id_for(r"C:\Music\b.mp3"));
        assert!(!is_local(123));
        assert!(!is_local(1 << 63 | 5)); // mixes
    }

    #[test]
    fn local_track_round_trip() {
        let lt = LocalTrack {
            path: r"C:\Music\Band - Song.flac".into(),
            title: "Song".into(),
            artist: "Band".into(),
            album: String::new(),
            duration_ms: 1000,
            art: None,
            modified: 0,
        };
        let t = lt.to_track();
        assert_eq!((t.title.as_str(), t.user.username.as_str(), t.duration), ("Song", "Band", 1000));
        assert_eq!(t.local_path.as_deref(), Some(lt.path.as_str()));
        assert!(is_local(t.id));
        assert_eq!(Entry::of(&t), Entry::Local { path: lt.path.clone() });
    }
}

#[cfg(test)]
mod scan_tests {
    use super::*;

    #[test]
    fn scans_a_folder() {
        let dir = std::env::temp_dir().join(format!("cumulus-scan-{}", std::process::id()));
        let sub = dir.join("Album");
        std::fs::create_dir_all(&sub).unwrap();
        // A tiny valid WAV (0.1 s of silence).
        let wav = |path: &Path| {
            let data = vec![0u8; 4410 * 4];
            let mut f = Vec::new();
            f.extend_from_slice(b"RIFF");
            f.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
            f.extend_from_slice(b"WAVEfmt ");
            for v in [16u32] {
                f.extend_from_slice(&v.to_le_bytes());
            }
            for v in [1u16, 2] {
                f.extend_from_slice(&v.to_le_bytes());
            }
            for v in [44_100u32, 176_400] {
                f.extend_from_slice(&v.to_le_bytes());
            }
            for v in [4u16, 16] {
                f.extend_from_slice(&v.to_le_bytes());
            }
            f.extend_from_slice(b"data");
            f.extend_from_slice(&(data.len() as u32).to_le_bytes());
            f.extend_from_slice(&data);
            std::fs::write(path, f).unwrap();
        };
        wav(&dir.join("Band - Song.wav"));
        wav(&sub.join("track.wav"));
        std::fs::write(sub.join("cover.jpg"), b"not really a jpeg").unwrap();
        std::fs::write(dir.join("notes.txt"), b"ignore me").unwrap();

        let found = scan(&[dir.display().to_string()], &[], &[]);
        assert_eq!(found.len(), 2);
        let song = found.iter().find(|t| t.title == "Song").unwrap();
        assert_eq!(song.artist, "Band"); // from "Artist - Title" file name
        assert!((90..=110).contains(&song.duration_ms));
        let track = found.iter().find(|t| t.title == "track").unwrap();
        assert!(track.art.as_deref().is_some_and(|a| a.ends_with("cover.jpg")));
        // A second scan reuses unchanged entries.
        assert_eq!(scan(&[dir.display().to_string()], &[], &found), found);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
