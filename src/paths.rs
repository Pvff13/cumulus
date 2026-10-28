use std::path::PathBuf;

/// Per-user data directory, e.g. `%LOCALAPPDATA%\Cumulus` on Windows.
pub fn data_dir() -> PathBuf {
    // Debug builds: CUMULUS_DEV_DATA points at a throwaway folder for tests.
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os("CUMULUS_DEV_DATA") {
        let dir = PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    let base = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir);
    let dir = base.join("Cumulus");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn config_file() -> PathBuf {
    data_dir().join("config.json")
}

/// Size-capped caches (artwork, list metadata). Safe to delete at any time.
pub fn cache_dir(kind: &str) -> PathBuf {
    let dir = data_dir().join("cache").join(kind);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// WebView2 profile used only by the sign-in window.
pub fn webview_dir() -> PathBuf {
    data_dir().join("webview")
}
