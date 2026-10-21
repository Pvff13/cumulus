//! Playback: a decode/engine thread feeding an audio output thread.
//!
//! The UI owns the queue (order, shuffle, repeat) and tells the engine what
//! to play now and what comes next; the engine decodes ahead, opens the next
//! track early and joins it on without a gap, or crossfades into it when a
//! crossfade is set. What is *heard* (track + position) is read back via
//! `Player::now`.

mod fmp4;
pub mod eq;
mod hls;
pub mod local;
mod output;
mod source;
mod transitions;

use std::sync::atomic::Ordering;

pub use transitions::Style;
use transitions::{Fade, Scan};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::sync::{Arc, MutexGuard};
use std::thread;
use std::time::Duration;

use output::{Output, SampleQueue, Shared};
pub use source::{DRM_REASON, QueueItem};
use source::{Source, Step};

use crate::api::Api;

/// How much decoded audio to keep queued ahead of the device.
const BUFFER_SECS: f64 = 4.0;
/// Start opening the next track this long before the current one ends.
const PREPARE_NEXT_SECS: f64 = 30.0;

pub enum Event {
    /// A track couldn't be played. `upcoming` means it was the prepared
    /// next track (the current one keeps playing).
    Failed { id: u64, reason: String, upcoming: bool },
    /// The last track finished and nothing was queued after it.
    Ended,
}

enum Cmd {
    Play { item: QueueItem, at: f64 },
    SetNext(Option<QueueItem>),
    Pause,
    Resume,
    Seek(f64),
    Stop,
}

pub struct Player {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
    output: Output,
}

impl Player {
    pub fn new(api: Arc<Api>, volume: f32, on_event: impl Fn(Event) + Send + 'static) -> Self {
        let shared = Arc::new(Shared::new(volume));
        let output = Output::start(shared.clone());
        let (tx, rx) = channel();
        let engine = Engine {
            api,
            shared: shared.clone(),
            output: output.clone(),
            events: Box::new(on_event),
            rx,
            current: None,
            opening: None,
            next_item: None,
            next: Next::None,
            waiting_for_next: false,
            paused: true,
            buf: Vec::new(),
            fading: None,
            current_item: None,
            next_scan: None,
            scan: None,
            scanned: None,
            gain: 1.0,
            trim_lead: false,
        };
        thread::Builder::new()
            .name("audio-engine".into())
            .spawn(move || engine.run())
            .expect("spawn audio engine");
        Self { tx, shared, output }
    }

    /// Equalizer on/off and gains (preamp, then 5 bands, in dB).
    pub fn set_eq(&self, on: bool, gains: [f32; 6]) {
        self.shared.eq_on.store(on, Ordering::Relaxed);
        for (slot, g) in self.shared.eq_gains.iter().zip(gains) {
            slot.store(g.to_bits(), Ordering::Relaxed);
        }
        self.shared.eq_version.fetch_add(1, Ordering::Relaxed);
    }

    /// Output device by name (None = system default). Reopens the stream.
    pub fn set_device(&self, name: Option<String>) {
        if let Ok(mut d) = self.shared.device.lock() {
            *d = name;
        }
        self.output.reopen();
    }

    /// Stream quality for tracks opened from now on.
    pub fn set_high_quality(&self, high: bool) {
        source::HIGH_QUALITY.store(high, Ordering::Relaxed);
    }

    /// Turns the visualizer feed on/off.
    pub fn set_visualizer(&self, on: bool) {
        self.shared.viz_on.store(on, Ordering::Relaxed);
        if !on {
            if let Ok(mut v) = self.shared.viz.lock() {
                v.clear();
                v.shrink_to_fit();
            }
        }
    }

    /// Latest mono output samples (oldest first) and their sample rate.
    pub fn visualizer_samples(&self) -> (Vec<f32>, f32) {
        let v = self.shared.viz.lock().map(|v| v.clone()).unwrap_or_default();
        (v, self.shared.viz_rate.load(Ordering::Relaxed) as f32)
    }

    /// Output devices to choose from.
    pub fn devices() -> Vec<String> {
        output::device_names()
    }

    /// Playback speed, 0.5-2.0 (pitch follows, like a record).
    pub fn set_speed(&self, speed: f32) {
        let speed = speed.clamp(0.5, 2.0);
        let old = f32::from_bits(self.shared.speed.swap(speed.to_bits(), Ordering::Relaxed));
        if (old - speed).abs() > f32::EPSILON {
            self.output.reopen();
        }
    }

    /// Plays `item` from `at` seconds, replacing whatever is playing.
    pub fn play(&self, item: QueueItem, at: f64) {
        self.shared.paused.store(false, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Play { item, at });
    }

    /// What to play after the current track (None = stop after it).
    pub fn set_next(&self, item: Option<QueueItem>) {
        let _ = self.tx.send(Cmd::SetNext(item));
    }

    pub fn pause(&self) {
        self.shared.paused.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Pause);
    }

    pub fn resume(&self) {
        self.shared.paused.store(false, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Resume);
    }

    pub fn seek(&self, to: f64) {
        let _ = self.tx.send(Cmd::Seek(to));
    }

    pub fn stop(&self) {
        self.shared.paused.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Stop);
    }

    pub fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::Relaxed)
    }

    /// Crossfade length between tracks that follow on by themselves
    /// (0 = none, plain gapless).
    pub fn set_crossfade(&self, secs: f32) {
        let ms = (secs.clamp(0.0, 12.0) * 1000.0) as u32;
        self.shared.crossfade_ms.store(ms, Ordering::Relaxed);
    }

    /// How crossfades sound.
    pub fn set_style(&self, style: Style) {
        self.shared.style.store(style.index(), Ordering::Relaxed);
    }

    /// Skip silence at the ends and starts of tracks.
    pub fn set_trim_silence(&self, on: bool) {
        self.shared.trim_silence.store(on, Ordering::Relaxed);
    }

    /// Even out loudness between tracks.
    pub fn set_match_volume(&self, on: bool) {
        self.shared.match_volume.store(on, Ordering::Relaxed);
    }

    pub fn set_volume(&self, v: f32) {
        self.shared.volume.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// The track being heard and its position in seconds.
    pub fn now(&self) -> Option<(u64, f64)> {
        self.shared.queue.lock().ok()?.now()
    }
}

enum Next {
    None,
    Opening(u64, Receiver<Result<Box<dyn Source>, String>>),
    Ready(Box<dyn Source>),
}

struct Engine {
    api: Arc<Api>,
    shared: Arc<Shared>,
    output: Output,
    events: Box<dyn Fn(Event) + Send>,
    rx: Receiver<Cmd>,
    current: Option<Box<dyn Source>>,
    /// The current track while it is being resolved/opened.
    opening: Option<(u64, Receiver<Result<Box<dyn Source>, String>>)>,
    next_item: Option<QueueItem>,
    next: Next,
    /// Current track finished decoding; join the next one when it's ready.
    waiting_for_next: bool,
    paused: bool,
    buf: Vec<f32>,
    /// The previous track fading out under the current one.
    fading: Option<Fade>,
    /// What the current track is (for scanning it).
    current_item: Option<QueueItem>,
    /// Background scans (trailing silence, loudness) of the next and current tracks.
    next_scan: Option<(u64, Receiver<Scan>)>,
    scan: Option<(u64, Receiver<Scan>)>,
    scanned: Option<(u64, Scan)>,
    /// Volume-matching gain applied to the current track.
    gain: f32,
    /// Still skipping the current track's leading silence.
    trim_lead: bool,
}

fn spawn_open(api: Arc<Api>, item: QueueItem, at: f64) -> Receiver<Result<Box<dyn Source>, String>> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let _ = tx.send(source::open(&api, &item, at));
    });
    rx
}

impl Engine {
    fn queue(&self) -> MutexGuard<'_, SampleQueue> {
        self.shared.queue.lock().unwrap()
    }

    fn run(mut self) {
        loop {
            // Block entirely when there is nothing to do (idle or paused).
            let busy = self.opening.is_some()
                || matches!(self.next, Next::Opening(..))
                || self.waiting_for_next
                || (!self.paused && self.current.is_some());
            let cmd = if busy {
                match self.rx.try_recv() {
                    Ok(c) => Some(c),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return,
                }
            } else {
                match self.rx.recv() {
                    Ok(c) => Some(c),
                    Err(_) => return,
                }
            };
            if let Some(cmd) = cmd {
                self.handle(cmd);
                continue;
            }
            self.poll_opening();
            self.poll_next();
            self.poll_scan();
            if self.waiting_for_next {
                self.join_next();
            }
            self.maybe_start_fade();
            if !self.decode_some() {
                // Buffer is full (or waiting on the network): wait a bit, but
                // wake immediately for commands.
                match self.rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(cmd) => self.handle(cmd),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Play { item, at } => {
                self.fading = None;
                self.current = None;
                self.current_item = Some(item.clone());
                self.next = Next::None;
                self.next_item = None;
                self.waiting_for_next = false;
                self.queue().flush();
                self.opening = Some((item.id, spawn_open(self.api.clone(), item, at)));
                self.paused = false;
            }
            Cmd::SetNext(item) => {
                let same = match (&self.next_item, &item) {
                    (Some(a), Some(b)) => a.id == b.id,
                    (None, None) => true,
                    _ => false,
                };
                if !same {
                    self.next = Next::None;
                    self.next_scan = None;
                    self.next_item = item;
                }
            }
            Cmd::Pause => {
                self.paused = true;
                self.output.pause();
            }
            Cmd::Resume => {
                self.paused = false;
                if self.current.is_some() || self.queue().buffered_frames() > 0 {
                    self.output.play();
                }
            }
            Cmd::Seek(to) => {
                self.fading = None;
                self.trim_lead = false;
                if let Some(src) = self.current.as_mut() {
                    let result = src.seek(to);
                    let (id, rate, pos) = (src.track_id(), src.rate(), src.position());
                    let mut q = self.shared.queue.lock().unwrap();
                    q.flush();
                    q.mark(id, pos, rate);
                    drop(q);
                    // The prepared next track stays prepared, but its start
                    // must be re-joined after this one ends.
                    self.waiting_for_next = false;
                    if let Err(reason) = result {
                        self.current = None;
                        (self.events)(Event::Failed { id, reason, upcoming: false });
                    }
                }
            }
            Cmd::Stop => {
                self.fading = None;
                self.current = None;
                self.opening = None;
                self.next = Next::None;
                self.next_item = None;
                self.waiting_for_next = false;
                self.queue().flush();
                self.paused = true;
                self.output.pause();
            }
        }
    }

    /// Starts `src` as the current track: right away if the output is idle or
    /// at the same sample rate (gapless), otherwise once the buffer drains.
    fn start(&mut self, src: Box<dyn Source>) {
        let id = src.track_id();
        let wants_scan = self.shared.trim_silence.load(Ordering::Relaxed) || self.shared.match_volume.load(Ordering::Relaxed);
        self.scanned = None;
        self.scan = match self.next_scan.take() {
            Some((scan_id, rx)) if scan_id == id => Some((scan_id, rx)),
            _ => match &self.current_item {
                Some(item) if wants_scan && item.id == id => {
                    Some((id, transitions::spawn_scan(self.api.clone(), item.clone(), src.duration())))
                }
                _ => None,
            },
        };
        self.poll_scan();
        // A scan done ahead of time sets the level straight away; otherwise it
        // eases in when the scan finishes.
        self.gain = self.target_gain(id);
        self.trim_lead = self.shared.trim_silence.load(Ordering::Relaxed) && src.position() < 0.5;
        self.output.configure(src.rate());
        self.queue().mark(src.track_id(), src.position(), src.rate());
        self.current = Some(src);
        if !self.paused {
            self.output.play();
        }
    }

    fn poll_opening(&mut self) {
        let Some((id, rx)) = &self.opening else { return };
        let id = *id;
        match rx.try_recv() {
            Ok(Ok(src)) => {
                self.opening = None;
                self.start(src);
            }
            Ok(Err(reason)) => {
                self.opening = None;
                (self.events)(Event::Failed { id, reason, upcoming: false });
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.opening = None,
        }
    }

    fn poll_next(&mut self) {
        // Begin opening the next track shortly before the current one ends.
        if matches!(self.next, Next::None) {
            let near_end = match &self.current {
                Some(c) => c.duration() - c.position() < PREPARE_NEXT_SECS,
                None => self.waiting_for_next,
            };
            if near_end {
                if let Some(item) = self.next_item.clone() {
                    let wants_scan = self.shared.trim_silence.load(Ordering::Relaxed)
                        || self.shared.match_volume.load(Ordering::Relaxed);
                    let duration = item.track.as_ref().map(|t| t.duration as f64 / 1000.0).filter(|d| *d > 0.0);
                    if let (true, Some(duration)) = (wants_scan, duration) {
                        self.next_scan = Some((item.id, transitions::spawn_scan(self.api.clone(), item.clone(), duration)));
                    }
                    self.next = Next::Opening(item.id, spawn_open(self.api.clone(), item, 0.0));
                }
            }
        }
        if let Next::Opening(id, rx) = &self.next {
            let id = *id;
            match rx.try_recv() {
                Ok(Ok(src)) => self.next = Next::Ready(src),
                Ok(Err(reason)) => {
                    self.next = Next::None;
                    self.next_item = None;
                    (self.events)(Event::Failed { id, reason, upcoming: true });
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.next = Next::None,
            }
        }
    }

    /// With a crossfade set, starts the next track that many seconds before
    /// the current one ends, fading the current one out underneath it.
    fn maybe_start_fade(&mut self) {
        let fade_secs = f64::from(self.shared.crossfade_ms.load(Ordering::Relaxed)) / 1000.0;
        if fade_secs <= 0.0 || self.fading.is_some() {
            return;
        }
        let (Some(cur), Next::Ready(next)) = (&self.current, &self.next) else { return };
        let remaining = self.music_end(cur.track_id()).unwrap_or(cur.duration()) - cur.position();
        // Same sample rate only (one device stream), and never on very short tracks.
        if remaining > fade_secs || remaining <= 0.5 || cur.duration() < fade_secs * 3.0 || next.rate() != cur.rate() {
            return;
        }
        let Next::Ready(next) = std::mem::replace(&mut self.next, Next::None) else { return };
        let old = self.current.take().expect("checked above");
        let total = (remaining * f64::from(old.rate())) as u64;
        let style = Style::from_index(self.shared.style.load(Ordering::Relaxed));
        self.fading = Some(Fade::new(old, style, self.gain, total));
        self.current_item = self.next_item.take();
        #[cfg(debug_assertions)]
        eprintln!("crossfade ({style:?}): {} -> {} over {remaining:.1}s", self.fading.as_ref().map_or(0, |f| f.track_id()), next.track_id());
        self.start(next);
    }

    /// Picks up a finished background scan of the current track.
    fn poll_scan(&mut self) {
        let Some((id, rx)) = &self.scan else { return };
        match rx.try_recv() {
            Ok(scan) => {
                #[cfg(debug_assertions)]
                eprintln!("scan {id}: music ends {:?}, gain {:?}", scan.music_end, scan.gain);
                self.scanned = Some((*id, scan));
                self.scan = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.scan = None,
        }
    }

    /// Where `id`'s music ends before trailing silence (silence trimming on).
    fn music_end(&self, id: u64) -> Option<f64> {
        if !self.shared.trim_silence.load(Ordering::Relaxed) {
            return None;
        }
        self.scanned.as_ref().filter(|(i, _)| *i == id).and_then(|(_, s)| s.music_end)
    }

    /// The volume-matching gain for `id` (1.0 when off or not known yet).
    fn target_gain(&self, id: u64) -> f32 {
        if !self.shared.match_volume.load(Ordering::Relaxed) {
            return 1.0;
        }
        self.scanned.as_ref().filter(|(i, _)| *i == id).and_then(|(_, s)| s.gain).unwrap_or(1.0)
    }

    /// The current track has been fully decoded: continue with the next.
    fn join_next(&mut self) {
        match std::mem::replace(&mut self.next, Next::None) {
            Next::Ready(src) => {
                // A different sample rate needs the device stream reopened,
                // which can only happen once the old audio has played out.
                if src.rate() != self.output.rate() && self.queue().buffered_frames() > 0 {
                    self.next = Next::Ready(src);
                    return;
                }
                self.waiting_for_next = false;
                self.current_item = self.next_item.take();
                self.start(src);
            }
            other @ Next::Opening(..) => self.next = other,
            Next::None if self.next_item.is_some() => {} // poll_next opens it
            Next::None => {
                // Nothing queued: finish once the last audio has played.
                if self.queue().buffered_frames() == 0 {
                    self.waiting_for_next = false;
                    self.output.pause();
                    (self.events)(Event::Ended);
                }
            }
        }
    }

    /// Applies the volume-matching gain to `buf`, easing towards `target`.
    fn apply_gain(&mut self, target: f32) {
        if self.gain == 1.0 && target == 1.0 {
            return;
        }
        let frames = (self.buf.len() / output::CHANNELS).max(1);
        // At most 0.05 per chunk: a full change takes about half a second.
        let end = self.gain + (target - self.gain).clamp(-0.05, 0.05);
        for (f, frame) in self.buf.chunks_exact_mut(output::CHANNELS).enumerate() {
            let g = self.gain + (end - self.gain) * f as f32 / frames as f32;
            for s in frame {
                *s = transitions::soft_clip(*s * g);
            }
        }
        self.gain = end;
    }

    /// Decodes one chunk if the buffer has room. Returns false if it should wait.
    fn decode_some(&mut self) -> bool {
        if self.paused {
            return false;
        }
        let Some(src) = self.current.as_mut() else { return false };
        let target = (BUFFER_SECS * f64::from(src.rate())) as u64;
        if self.shared.queue.lock().unwrap().buffered_frames() >= target {
            return false;
        }
        let id = src.track_id();
        let music_end = self.music_end(id);
        let target_gain = self.target_gain(id);
        let Some(src) = self.current.as_mut() else { return false };
        let step = if music_end.is_some_and(|end| src.position() >= end) {
            Ok(Step::Ended) // only silence is left
        } else {
            self.buf.clear();
            src.decode_next(&mut self.buf)
        };
        match step {
            Ok(Step::Produced) => {
                if self.trim_lead {
                    let silent = transitions::leading_silence(&self.buf);
                    if silent == self.buf.len() && src.position() < transitions::MAX_LEADING_TRIM {
                        self.buf.clear();
                    } else {
                        self.buf.drain(..silent);
                        self.trim_lead = false;
                        let frames = (self.buf.len() / output::CHANNELS) as f64;
                        let start = src.position() - frames / f64::from(src.rate());
                        self.shared.queue.lock().unwrap().retime(id, start);
                    }
                }
                self.apply_gain(target_gain);
                if let Some(fade) = self.fading.as_mut() {
                    if !fade.mix_into(&mut self.buf) {
                        #[cfg(debug_assertions)]
                        eprintln!("crossfade done: {} of {} frames mixed, outgoing energy {:.1}", fade.done, fade.total, fade.energy);
                        self.fading = None;
                    }
                }
                if !self.buf.is_empty() {
                    self.shared.queue.lock().unwrap().push(&self.buf);
                }
                true
            }
            Ok(Step::Waiting) => true,
            Ok(Step::Ended) => {
                self.current = None;
                self.waiting_for_next = true;
                true
            }
            Err(reason) => {
                let id = src.track_id();
                self.current = None;
                (self.events)(Event::Failed { id, reason, upcoming: false });
                true
            }
        }
    }
}

