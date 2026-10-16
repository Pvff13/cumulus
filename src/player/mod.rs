//! Playback: a decode/engine thread feeding an audio output thread.
//!
//! The UI owns the queue (order, shuffle, repeat) and tells the engine what
//! to play now and what comes next; the engine decodes ahead, opens the next
//! track early and joins it on without a gap. What is *heard* (track +
//! position) is read back via `Player::now`.

mod fmp4;
mod hls;
mod output;
mod source;

use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::sync::{Arc, MutexGuard};
use std::thread;
use std::time::Duration;

use output::{Output, SampleQueue, Shared};
pub use source::QueueItem;
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
}

impl Player {
    pub fn new(api: Arc<Api>, volume: f32, on_event: impl Fn(Event) + Send + 'static) -> Self {
        let shared = Arc::new(Shared::new(volume));
        let output = Output::start(shared.clone());
        let (tx, rx) = channel();
        let engine = Engine {
            api,
            shared: shared.clone(),
            output,
            events: Box::new(on_event),
            rx,
            current: None,
            opening: None,
            next_item: None,
            next: Next::None,
            waiting_for_next: false,
            paused: true,
            buf: Vec::new(),
        };
        thread::Builder::new()
            .name("audio-engine".into())
            .spawn(move || engine.run())
            .expect("spawn audio engine");
        Self { tx, shared }
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
            if self.waiting_for_next {
                self.join_next();
            }
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
                self.current = None;
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
                self.next_item = None;
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
        self.buf.clear();
        match src.decode_next(&mut self.buf) {
            Ok(Step::Produced) => {
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
