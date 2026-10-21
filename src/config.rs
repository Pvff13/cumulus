use serde::{Deserialize, Serialize};

use crate::paths;

/// Small non-secret settings persisted as JSON. The OAuth token is never
/// stored here; it lives in the OS credential store (see `auth`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Last known public web client_id, re-discovered when it stops working.
    pub client_id: Option<String>,
    /// Disk limit for cached artwork, in MB.
    pub art_cache_mb: u64,
    /// 0.0..=1.0
    pub volume: f32,
    /// Closing the window hides it to the tray and keeps playing.
    pub close_to_tray: bool,
    /// At the end of the queue, keep going with similar tracks.
    pub autoplay: bool,
    /// The now-playing pop-out window: open, always on top, with controls.
    pub popout: bool,
    pub popout_on_top: bool,
    pub popout_controls: bool,
    /// Colours: "dark", "grey" or "light", and an accent name (see ui::theme).
    pub theme: String,
    pub accent: String,
    /// Seconds to crossfade between tracks (0 = off, gapless).
    pub crossfade_secs: f32,
    /// Crossfade style ("smooth", "linear", "dip", "filter", "echo").
    pub transition: String,
    /// Skip silence at the ends/starts of tracks.
    pub trim_silence: bool,
    /// Even out loudness between tracks.
    pub match_volume: bool,
    /// Equalizer on/off and gains in dB (preamp, 60, 230, 910, 3.6k, 14k).
    pub eq_on: bool,
    pub eq_gains: [f32; 6],
    /// Playback speed (1.0 = normal).
    pub speed: f32,
    /// Show "Listening to …" on Discord.
    pub discord_status: bool,
    /// Discord application id, if not the built-in one.
    pub discord_app_id: Option<String>,
    /// Appearance: zoom, main font, layout, custom theme and wallpaper.
    pub ui_scale: f32,
    pub font: String,
    pub player_on_top: bool,
    pub sidebar_right: bool,
    pub compact: bool,
    pub custom_theme: Option<crate::ui::CustomTheme>,
    /// "off", "picture", "slideshow" or "artwork".
    pub wallpaper_mode: String,
    pub wallpaper_path: Option<String>,
    pub wallpaper_minutes: u32,
    pub wallpaper_dim: f32,
    /// Changed in-app shortcuts and global hotkeys: action -> "Ctrl+Alt+P".
    pub hotkeys: std::collections::BTreeMap<String, String>,
    pub global_hotkeys: std::collections::BTreeMap<String, String>,
    /// Visualizer style ("off", "bars", "mirrored", "line").
    pub visualizer: String,
    /// OBS: now-playing text file (path, format) and the browser overlay (port).
    pub obs_text: bool,
    pub obs_text_path: Option<String>,
    pub obs_text_format: String,
    pub obs_overlay: bool,
    pub obs_port: u16,
    /// Scrobbling: which plays to send, and user names to show (the logins
    /// themselves are in the OS credential store).
    pub scrobble_local: bool,
    pub lastfm_user: Option<String>,
    pub listenbrainz_user: Option<String>,
    /// A Last.fm API key/secret of your own (if this build has none).
    pub lastfm_api_key: Option<String>,
    pub lastfm_api_secret: Option<String>,
    /// Output device name (None = system default).
    pub audio_device: Option<String>,
    /// Prefer 160 kbps AAC streams.
    pub high_quality: bool,
    /// Skip a track that hasn't moved for this many seconds (0 = off).
    pub skip_stuck_secs: u32,
    /// Sidebar sections, top to bottom (empty = the default order).
    pub sidebar_order: Vec<String>,
    /// Hiding: "Not interested" tracks (id, label), blocked artists, keywords.
    pub hidden_tracks: Vec<(u64, String)>,
    pub blocked_artists: Vec<crate::ui::BlockedArtist>,
    pub hidden_keywords: Vec<String>,
    /// Path of the build to update from; defaults to this project's
    /// `target/release/cumulus.exe` (set at compile time).
    pub update_source: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self { client_id: None, art_cache_mb: 100, volume: 0.8, close_to_tray: true, autoplay: true, popout: false, popout_on_top: true, popout_controls: true, theme: "dark".into(), accent: "purple".into(), crossfade_secs: 0.0, transition: "smooth".into(), trim_silence: true, match_volume: false, eq_on: false, eq_gains: [0.0; 6], speed: 1.0, discord_status: false, discord_app_id: None, sidebar_order: Vec::new(), hidden_tracks: Vec::new(), blocked_artists: Vec::new(), hidden_keywords: Vec::new(), scrobble_local: true, lastfm_user: None, listenbrainz_user: None, lastfm_api_key: None, lastfm_api_secret: None, visualizer: "off".into(), obs_text: false, obs_text_path: None, obs_text_format: "{artist} - {title}".into(), obs_overlay: false, obs_port: 8337, hotkeys: Default::default(), global_hotkeys: Default::default(), ui_scale: 1.0, font: "Default".into(), player_on_top: false, sidebar_right: false, compact: false, custom_theme: None, wallpaper_mode: "off".into(), wallpaper_path: None, wallpaper_minutes: 10, wallpaper_dim: 0.65, audio_device: None, high_quality: true, skip_stuck_secs: 0, update_source: None }
    }
}

impl Config {
    pub fn load() -> Self {
        std::fs::read_to_string(paths::config_file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(paths::config_file(), s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn popout_off_by_default_and_remembered() {
        // New installs (no config.json, or one without these keys) start with it off.
        let fresh: Config = serde_json::from_str("{}").unwrap();
        assert!(!fresh.popout && fresh.popout_on_top && fresh.popout_controls);
        // Turned on, it round-trips through config.json, so it reopens next start.
        let mut c = Config::default();
        c.popout = true;
        c.popout_controls = false;
        let back: Config = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert!(back.popout && !back.popout_controls);
    }
}
