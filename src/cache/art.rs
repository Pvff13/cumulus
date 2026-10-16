//! Artwork loader: disk cache (size-capped, oldest evicted first) plus a
//! small in-memory texture cache. Requests are served newest-first so the
//! rows currently on screen load before ones that were scrolled past.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

use crate::api::client::Agent;
use crate::paths;

const WORKERS: usize = 4;
/// Pending requests beyond this are dropped (oldest first); they were for
/// rows that have long since scrolled away and get re-requested if needed.
const MAX_QUEUE: usize = 96;
/// In-memory texture budget, in pixels (about 16 MB of RGBA).
const MEM_PIXELS: usize = 4_000_000;
const MAX_DOWNLOAD: u64 = 4 * 1024 * 1024;
/// Images are stored in memory at most this many pixels wide/high.
const MAX_DIM: u32 = 300;
/// A failed image (offline, 404) is retried after this long.
const RETRY_AFTER: Duration = Duration::from_secs(60);

struct Entry {
    tex: TextureHandle,
    pixels: usize,
    last_used: u64,
}

#[derive(Default)]
struct State {
    loaded: HashMap<String, Entry>,
    queue: Vec<String>,
    in_flight: HashSet<String>,
    failed: HashMap<String, Instant>,
    pixels: usize,
    frame: u64,
}

struct Inner {
    ctx: egui::Context,
    agent: Agent,
    state: Mutex<State>,
    wake: Condvar,
    dir: PathBuf,
    disk_limit: u64,
    disk_used: AtomicU64,
    trim_lock: Mutex<()>,
}

#[derive(Clone)]
pub struct ArtCache(Arc<Inner>);

impl ArtCache {
    pub fn new(ctx: egui::Context, agent: Agent, disk_limit_mb: u64) -> Self {
        let dir = paths::cache_dir("art");
        let inner = Arc::new(Inner {
            ctx,
            agent,
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            disk_used: AtomicU64::new(super::dir_size(&dir)),
            dir,
            disk_limit: disk_limit_mb.max(10) * 1024 * 1024,
            trim_lock: Mutex::new(()),
        });
        for _ in 0..WORKERS {
            let inner = inner.clone();
            thread::spawn(move || worker(inner));
        }
        Self(inner)
    }

    /// The texture for `url` if it is loaded; otherwise queues it and returns None.
    pub fn get(&self, url: &str) -> Option<TextureHandle> {
        let mut st = self.0.state.lock().unwrap();
        let frame = st.frame;
        if let Some(e) = st.loaded.get_mut(url) {
            e.last_used = frame;
            return Some(e.tex.clone());
        }
        let recently_failed = st.failed.get(url).is_some_and(|t| t.elapsed() < RETRY_AFTER);
        if !st.in_flight.contains(url) && !recently_failed {
            st.queue.retain(|u| u != url);
            st.queue.push(url.to_string());
            if st.queue.len() > MAX_QUEUE {
                let excess = st.queue.len() - MAX_QUEUE;
                st.queue.drain(..excess);
            }
            self.0.wake.notify_one();
        }
        None
    }

    /// Call once per frame: advances the LRU clock and evicts textures that
    /// haven't been drawn recently once over the memory budget.
    pub fn end_frame(&self) {
        let mut st = self.0.state.lock().unwrap();
        st.frame += 1;
        if st.pixels <= MEM_PIXELS {
            return;
        }
        let frame = st.frame;
        let mut by_age: Vec<(u64, String)> = st
            .loaded
            .iter()
            .filter(|(_, e)| e.last_used + 2 < frame)
            .map(|(k, e)| (e.last_used, k.clone()))
            .collect();
        by_age.sort();
        for (_, key) in by_age {
            if st.pixels <= MEM_PIXELS * 3 / 4 {
                break;
            }
            if let Some(e) = st.loaded.remove(&key) {
                st.pixels -= e.pixels;
            }
        }
    }
}

fn worker(inner: Arc<Inner>) {
    loop {
        let url = {
            let mut st = inner.state.lock().unwrap();
            loop {
                if let Some(u) = st.queue.pop() {
                    st.in_flight.insert(u.clone());
                    break u;
                }
                st = inner.wake.wait(st).unwrap();
            }
        };
        let result = load(&inner, &url);
        let mut st = inner.state.lock().unwrap();
        st.in_flight.remove(&url);
        match result {
            Some(img) => {
                let pixels = img.width() * img.height();
                let tex = inner.ctx.load_texture(&url, img, TextureOptions::LINEAR);
                let frame = st.frame;
                st.pixels += pixels;
                st.loaded.insert(url, Entry { tex, pixels, last_used: frame });
            }
            None => {
                st.failed.insert(url, Instant::now());
            }
        }
        drop(st);
        inner.ctx.request_repaint();
    }
}

fn load(inner: &Inner, url: &str) -> Option<ColorImage> {
    let path = inner.dir.join(super::key_hash(url));
    let bytes = match fs::read(&path) {
        Ok(b) => {
            super::touch(&path);
            b
        }
        Err(_) => {
            let b = inner.agent.get_bytes(url, MAX_DOWNLOAD).ok()?;
            if fs::write(&path, &b).is_ok() {
                let used = inner.disk_used.fetch_add(b.len() as u64, Ordering::Relaxed) + b.len() as u64;
                if used > inner.disk_limit {
                    if let Ok(_guard) = inner.trim_lock.try_lock() {
                        let left = super::trim(&inner.dir, inner.disk_limit * 8 / 10);
                        inner.disk_used.store(left, Ordering::Relaxed);
                    }
                }
            }
            b
        }
    };
    let rgba = match image::load_from_memory(&bytes) {
        // Big artwork (500 px mix tiles) is shrunk on load; nothing is shown
        // larger than this, and it keeps texture memory down.
        Ok(img) if img.width().max(img.height()) > MAX_DIM => img.thumbnail(MAX_DIM, MAX_DIM).to_rgba8(),
        Ok(img) => img.to_rgba8(),
        Err(_) => {
            // Corrupt or truncated file: drop it so it is fetched again next time.
            let _ = fs::remove_file(&path);
            return None;
        }
    };
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}
