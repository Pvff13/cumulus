//! A small log of things that went wrong (audio device trouble, tracks that
//! got stuck), in cumulus.log next to config.json, so problems that only
//! happen now and then can be looked into afterwards. No listening history
//! or account details go in it. Kept under ~256 KB (the older half moves to
//! cumulus.old.log).

use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 256 * 1024;

static LOCK: Mutex<()> = Mutex::new(());

/// Appends one line, with the date and time (UTC).
pub fn line(msg: &str) {
    #[cfg(debug_assertions)]
    eprintln!("log: {msg}");
    let _guard = LOCK.lock();
    let path = crate::paths::data_dir().join("cumulus.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::rename(&path, crate::paths::data_dir().join("cumulus.old.log"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} {msg}", timestamp());
    }
}

/// "2026-11-10 14:36:59" (UTC), without pulling in a date library.
fn timestamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year}-{month:02}-{day:02} {:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    #[test]
    fn timestamp_looks_right() {
        let t = super::timestamp();
        assert_eq!(t.len(), 19);
        assert!(t.starts_with("20"));
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[13..14], ":");
    }
}
