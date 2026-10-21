//! Fallback for logins when there's no OS keyring (WSL, minimal Linux
//! desktops): one JSON file only this user can read (mode 0600). On Windows
//! the Credential Manager is always there, so this is never used.

use std::collections::BTreeMap;
use std::path::PathBuf;

fn path() -> PathBuf {
    crate::paths::data_dir().join("secrets.json")
}

fn read() -> BTreeMap<String, String> {
    std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn get(name: &str) -> Option<String> {
    read().remove(name).filter(|v| !v.is_empty())
}

/// Stores (or with None, removes) a secret.
pub fn set(name: &str, value: Option<&str>) -> Result<(), String> {
    let mut all = read();
    match value {
        Some(v) => {
            all.insert(name.to_string(), v.to_string());
        }
        None => {
            if all.remove(name).is_none() {
                return Ok(());
            }
        }
    }
    if all.is_empty() {
        let _ = std::fs::remove_file(path());
        return Ok(());
    }
    let text = serde_json::to_string(&all).map_err(|e| e.to_string())?;
    write_private(&path(), &text).map_err(|e| format!("could not save login: {e}"))
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(text.as_bytes())?;
    std::fs::rename(&tmp, path)
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    std::fs::write(path, text)
}
