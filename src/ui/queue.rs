//! Play queue: the list you started playing from, the play order (shuffled
//! or not) and repeat mode. The audio engine only ever sees "now" and "next".

use std::collections::HashSet;
use std::sync::Arc;

use crate::api::models::Track;
use crate::player::QueueItem;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Repeat {
    Off,
    All,
    One,
}

impl Repeat {
    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

pub struct Queue {
    items: Vec<QueueItem>,
    /// Indexes into `items` in play order.
    order: Vec<usize>,
    pos: usize,
    pub shuffle: bool,
    pub repeat: Repeat,
    /// Tracks that failed to play this session; skipped when picking "next".
    unplayable: HashSet<u64>,
    rng: u64,
}

impl Default for Queue {
    fn default() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e37_79b9, |d| d.as_nanos() as u64);
        Self {
            items: Vec::new(),
            order: Vec::new(),
            pos: 0,
            shuffle: false,
            repeat: Repeat::Off,
            unplayable: HashSet::new(),
            rng: seed | 1,
        }
    }
}

impl Queue {
    /// Replaces the queue, starting at `items[start]`.
    pub fn set(&mut self, items: Vec<QueueItem>, start: usize) {
        self.items = items;
        self.rebuild_order(start.min(self.items.len().saturating_sub(1)));
    }

    pub fn current(&self) -> Option<&QueueItem> {
        self.order.get(self.pos).map(|&i| &self.items[i])
    }

    pub fn current_id(&self) -> Option<u64> {
        self.current().map(|i| i.id)
    }

    /// What should play after the current track ends on its own.
    pub fn peek_next(&self) -> Option<QueueItem> {
        if self.repeat == Repeat::One {
            return self.current().cloned();
        }
        self.following(self.pos).map(|p| self.items[self.order[p]].clone())
    }

    /// Next button: always moves on (even in repeat-one).
    pub fn advance(&mut self) -> Option<QueueItem> {
        let p = self.following(self.pos)?;
        self.pos = p;
        self.current().cloned()
    }

    /// Previous button.
    pub fn back(&mut self) -> Option<QueueItem> {
        if self.pos > 0 {
            self.pos -= 1;
        } else if self.repeat == Repeat::All && !self.order.is_empty() {
            self.pos = self.order.len() - 1;
        }
        self.current().cloned()
    }

    /// The engine moved on to `id` by itself (gapless): follow it.
    pub fn sync_to(&mut self, id: u64) {
        if self.current_id() == Some(id) {
            return;
        }
        if let Some(p) = self.following(self.pos).filter(|&p| self.items[self.order[p]].id == id) {
            self.pos = p;
        } else if let Some(p) = self.order.iter().position(|&i| self.items[i].id == id) {
            self.pos = p;
        }
    }

    pub fn mark_unplayable(&mut self, id: u64) {
        self.unplayable.insert(id);
    }

    pub fn set_shuffle(&mut self, on: bool) {
        self.shuffle = on;
        if let Some(&cur) = self.order.get(self.pos) {
            self.rebuild_order(cur);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Play-order position of the current track.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Number of entries in play order.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// The entry at a play-order position.
    pub fn at(&self, p: usize) -> Option<&QueueItem> {
        self.order.get(p).map(|&i| &self.items[i])
    }

    pub fn is_unplayable(&self, id: u64) -> bool {
        self.unplayable.contains(&id)
    }

    /// Jumps to a play-order position (clicking an entry in the queue view).
    pub fn jump_to(&mut self, p: usize) -> Option<QueueItem> {
        if p >= self.order.len() {
            return None;
        }
        self.pos = p;
        self.current().cloned()
    }

    /// Moves an upcoming entry (drag-and-drop). Both positions must be after
    /// the current track; `to` is the position it should end up at.
    pub fn move_entry(&mut self, from: usize, to: usize) {
        let n = self.order.len();
        if from <= self.pos || to <= self.pos || from >= n || to >= n || from == to {
            return;
        }
        let entry = self.order.remove(from);
        self.order.insert(to, entry);
    }

    /// Removes an entry from the queue (not the current one).
    pub fn remove_at(&mut self, p: usize) {
        if p == self.pos || p >= self.order.len() {
            return;
        }
        self.order.remove(p);
        if p < self.pos {
            self.pos -= 1;
        }
    }

    /// Queues `item` to play right after the current track.
    pub fn play_next(&mut self, item: QueueItem) {
        self.items.push(item);
        let idx = self.items.len() - 1;
        if self.order.is_empty() {
            self.order.push(idx);
            self.pos = 0;
        } else {
            self.order.insert(self.pos + 1, idx);
        }
    }

    /// Whether `id` is anywhere in the queue.
    pub fn contains(&self, id: u64) -> bool {
        self.items.iter().any(|i| i.id == id)
    }

    /// Queues `item` at the end.
    pub fn add_to_end(&mut self, item: QueueItem) {
        self.items.push(item);
        self.order.push(self.items.len() - 1);
    }

    /// Fills in a track that was only known by id.
    pub fn update_track(&mut self, track: Arc<Track>) {
        for item in self.items.iter_mut().filter(|i| i.id == track.id) {
            item.track = Some(track.clone());
        }
    }

    /// Position after `from` in play order, skipping unplayable tracks and
    /// wrapping when repeating all.
    fn following(&self, from: usize) -> Option<usize> {
        let n = self.order.len();
        for step in 1..=n {
            let p = from + step;
            let p = if p < n {
                p
            } else if self.repeat == Repeat::All {
                p % n
            } else {
                return None;
            };
            if !self.unplayable.contains(&self.items[self.order[p]].id) {
                return Some(p);
            }
        }
        None
    }

    /// Play order with `current` first when shuffling, list order otherwise.
    fn rebuild_order(&mut self, current: usize) {
        let n = self.items.len();
        self.order = (0..n).collect();
        if self.shuffle && n > 1 {
            self.order.swap(0, current);
            for i in (2..n).rev() {
                let j = 1 + (self.next_rand() % i as u64) as usize;
                self.order.swap(i, j);
            }
            self.pos = 0;
        } else {
            self.pos = current;
        }
    }

    fn next_rand(&mut self) -> u64 {
        // xorshift64: plenty for shuffling a playlist.
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(n: u64) -> Vec<QueueItem> {
        (1..=n).map(|id| QueueItem { id, track: None }).collect()
    }

    #[test]
    fn order_repeat_and_skips() {
        let mut q = Queue::default();
        q.set(items(3), 1);
        assert_eq!(q.current_id(), Some(2));
        assert_eq!(q.peek_next().map(|i| i.id), Some(3));
        q.advance();
        assert_eq!(q.peek_next().map(|i| i.id), None);
        q.repeat = Repeat::All;
        assert_eq!(q.peek_next().map(|i| i.id), Some(1));
        q.repeat = Repeat::One;
        assert_eq!(q.peek_next().map(|i| i.id), Some(3));
        q.repeat = Repeat::All;
        q.mark_unplayable(1);
        assert_eq!(q.peek_next().map(|i| i.id), Some(2));
        q.sync_to(2);
        assert_eq!(q.current_id(), Some(2));
        assert_eq!(q.back().map(|i| i.id), Some(1));
    }

    #[test]
    fn editing_the_queue() {
        let mut q = Queue::default();
        q.set(items(5), 1); // current = 2; upcoming 3,4,5
        q.move_entry(4, 2); // 5 moves up to right after current
        let ids = |q: &Queue| (0..q.len()).map(|p| q.at(p).unwrap().id).collect::<Vec<_>>();
        assert_eq!(ids(&q), [1, 2, 5, 3, 4]);
        q.move_entry(0, 3); // can't move history/current
        assert_eq!(ids(&q), [1, 2, 5, 3, 4]);
        q.remove_at(0);
        assert_eq!((ids(&q), q.position()), (vec![2, 5, 3, 4], 0));
        q.play_next(QueueItem { id: 9, track: None });
        assert_eq!(q.peek_next().unwrap().id, 9);
        q.add_to_end(QueueItem { id: 10, track: None });
        assert_eq!(ids(&q), [2, 9, 5, 3, 4, 10]);
        assert_eq!(q.jump_to(3).unwrap().id, 3);
        assert_eq!(q.peek_next().unwrap().id, 4);
    }

    #[test]
    fn shuffle_keeps_current_first_and_covers_all() {
        let mut q = Queue::default();
        q.shuffle = true;
        q.set(items(50), 7);
        assert_eq!(q.current_id(), Some(8));
        let mut seen = vec![q.current_id().unwrap()];
        while let Some(i) = q.advance() {
            seen.push(i.id);
        }
        seen.sort();
        assert_eq!(seen, (1..=50).collect::<Vec<_>>());
    }
}
