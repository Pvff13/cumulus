//! Disk + memory caches. Nothing here is required for correctness; any of it
//! can be deleted and is rebuilt on demand. Audio is never cached.

pub mod art;
pub mod meta;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Stable 64-bit FNV-1a hash, used for cache file names.
pub fn key_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// Marks a cache file as recently used (for oldest-first eviction).
pub fn touch(path: &Path) {
    if let Ok(f) = fs::File::options().append(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// Total size of the files in `dir`.
pub fn dir_size(dir: &Path) -> u64 {
    files(dir).iter().map(|(_, len, _)| len).sum()
}

/// Deletes least-recently-used files until `dir` is below `target` bytes.
/// Returns the new total size.
pub fn trim(dir: &Path, target: u64) -> u64 {
    let mut entries = files(dir);
    let mut total: u64 = entries.iter().map(|(_, len, _)| len).sum();
    entries.sort_by_key(|(_, _, modified)| *modified);
    for (path, len, _) in entries {
        if total <= target {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
    total
}

fn files(dir: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    rd.filter_map(Result::ok)
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file()
                .then(|| (e.path(), m.len(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_removes_oldest_first() {
        let dir = std::env::temp_dir().join(format!("cumulus-trim-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (i, name) in ["old", "mid", "new"].iter().enumerate() {
            let p = dir.join(name);
            fs::write(&p, vec![0u8; 100]).unwrap();
            let f = fs::File::options().append(true).open(&p).unwrap();
            f.set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1000 * (i as u64 + 1)))
                .unwrap();
        }
        assert_eq!(trim(&dir, 150), 100);
        assert!(!dir.join("old").exists() && !dir.join("mid").exists() && dir.join("new").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
