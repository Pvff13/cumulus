//! First page of each list, saved as the raw API JSON so the app can show
//! your library instantly on start-up and while offline.

use std::fs;
use std::path::PathBuf;

use crate::paths;

const LIMIT: u64 = 20 * 1024 * 1024;

fn path(key: &str) -> PathBuf {
    paths::cache_dir("meta").join(format!("{}.json", super::key_hash(key)))
}

pub fn load(key: &str) -> Option<String> {
    let p = path(key);
    let s = fs::read_to_string(&p).ok()?;
    super::touch(&p);
    Some(s)
}

pub fn save(key: &str, body: &str) {
    if fs::write(path(key), body).is_ok() {
        let dir = paths::cache_dir("meta");
        if super::dir_size(&dir) > LIMIT {
            super::trim(&dir, LIMIT * 8 / 10);
        }
    }
}
