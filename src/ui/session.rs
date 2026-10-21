//! "Resume where you left off": the queue (in play order), the current
//! entry and position are saved now and then, and restored paused at start.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::queue::{Queue, Repeat};

/// Saves at most this many queue entries around the current one.
const MAX_SAVED: usize = 1000;

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    /// Track ids in play order.
    pub ids: Vec<u64>,
    /// Index of the current entry in `ids`.
    pub current: usize,
    /// Seconds into the current track.
    pub position: f64,
    /// 0 off, 1 all, 2 one.
    pub repeat: u8,
}

fn file() -> PathBuf {
    crate::paths::data_dir().join("session.json")
}

impl Saved {
    /// Snapshot of `queue` at `position` seconds into the current track.
    pub fn of(queue: &Queue, position: f64) -> Option<Self> {
        if queue.is_empty() {
            return None;
        }
        let len = queue.len();
        let pos = queue.position();
        // Keep a window around the current entry for very long queues.
        let start = pos.saturating_sub(MAX_SAVED / 4).min(len.saturating_sub(MAX_SAVED));
        let end = (start + MAX_SAVED).min(len);
        let ids = (start..end).filter_map(|p| queue.at(p).map(|i| i.id)).collect();
        let repeat = match queue.repeat {
            Repeat::Off => 0,
            Repeat::All => 1,
            Repeat::One => 2,
        };
        Some(Self { ids, current: pos - start, position: (position * 10.0).round() / 10.0, repeat })
    }

    pub fn repeat(&self) -> Repeat {
        match self.repeat {
            1 => Repeat::All,
            2 => Repeat::One,
            _ => Repeat::Off,
        }
    }

    pub fn load() -> Option<Self> {
        let text = std::fs::read_to_string(file()).ok()?;
        let saved: Self = serde_json::from_str(&text).ok()?;
        (!saved.ids.is_empty() && saved.current < saved.ids.len()).then_some(saved)
    }

    pub fn save(&self) {
        if let Ok(text) = serde_json::to_string(self) {
            let tmp = file().with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, file());
            }
        }
    }

    /// Forgets the saved session (signing out).
    pub fn clear() {
        let _ = std::fs::remove_file(file());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::QueueItem;

    fn queue(n: u64, start: usize) -> Queue {
        let mut q = Queue::default();
        q.set((1..=n).map(|id| QueueItem { id, track: None }).collect(), start);
        q
    }

    #[test]
    fn snapshot() {
        assert_eq!(Saved::of(&Queue::default(), 0.0), None);
        let s = Saved::of(&queue(5, 2), 61.23).unwrap();
        assert_eq!((s.ids, s.current, s.position), (vec![1, 2, 3, 4, 5], 2, 61.2));
        // A long queue keeps a window that still contains the current entry.
        let s = Saved::of(&queue(5000, 4000), 0.0).unwrap();
        assert_eq!(s.ids.len(), MAX_SAVED);
        assert_eq!(s.ids[s.current], 4001);
    }
}
