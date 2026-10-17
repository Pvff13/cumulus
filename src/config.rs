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
    /// Path of the build to update from; defaults to this project's
    /// `target/release/cumulus.exe` (set at compile time).
    pub update_source: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self { client_id: None, art_cache_mb: 100, volume: 0.8, close_to_tray: true, autoplay: true, popout: false, popout_on_top: true, popout_controls: true, update_source: None }
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
