//! Audio output. A dedicated thread owns the cpal stream (always stereo f32,
//! opened at the track's own sample rate; WASAPI's built-in resampler matches
//! it to the device). The stream callback pulls from `SampleQueue`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub const CHANNELS: usize = 2;

/// Marks where in the sample stream a track (or a seek position) begins,
/// so the UI can show what is actually being heard.
#[derive(Debug, Clone, Copy)]
pub struct Boundary {
    /// Frame index (in `written` units) where this starts.
    pub at: u64,
    pub track_id: u64,
    /// Track position at `at`, seconds.
    pub start: f64,
    pub rate: u32,
}

#[derive(Default)]
pub struct SampleQueue {
    /// Interleaved stereo samples waiting to be played.
    pub samples: VecDeque<f32>,
    /// Frames handed to the device so far.
    pub consumed: u64,
    /// Frames pushed so far.
    pub written: u64,
    pub boundaries: VecDeque<Boundary>,
}

impl SampleQueue {
    pub fn buffered_frames(&self) -> u64 {
        self.written - self.consumed
    }

    pub fn push(&mut self, samples: &[f32]) {
        self.samples.extend(samples);
        self.written += (samples.len() / CHANNELS) as u64;
    }

    /// Drops everything not yet played (for seeks / a new track).
    pub fn flush(&mut self) {
        self.samples.clear();
        self.consumed = self.written;
        self.boundaries.clear();
    }

    pub fn mark(&mut self, track_id: u64, start: f64, rate: u32) {
        let at = self.written;
        self.boundaries.push_back(Boundary { at, track_id, start, rate });
    }

    /// Moves the start position of the newest boundary, if it's `track_id`'s
    /// and nothing has been written after it yet (leading silence skipped).
    pub fn retime(&mut self, track_id: u64, start: f64) {
        let written = self.written;
        if let Some(b) = self.boundaries.back_mut().filter(|b| b.track_id == track_id && b.at == written) {
            b.start = start;
        }
    }

    /// (track id, position in seconds) of what is playing right now.
    pub fn now(&self) -> Option<(u64, f64)> {
        let b = self.boundaries.iter().rev().find(|b| b.at <= self.consumed)?;
        Some((b.track_id, b.start + (self.consumed - b.at) as f64 / f64::from(b.rate)))
    }

    fn prune(&mut self) {
        while self.boundaries.len() > 1 && self.boundaries[1].at <= self.consumed {
            self.boundaries.pop_front();
        }
    }
}

pub struct Shared {
    pub queue: Mutex<SampleQueue>,
    /// f32 bits, 0.0..=1.0 (applied with a squared curve).
    pub volume: AtomicU32,
    pub paused: AtomicBool,
    /// Crossfade between tracks, milliseconds (0 = gapless, no fade).
    pub crossfade_ms: AtomicU32,
    /// Crossfade style (`Style::index`).
    pub style: AtomicU8,
    pub trim_silence: AtomicBool,
    pub match_volume: AtomicBool,
}

impl Shared {
    pub fn new(volume: f32) -> Self {
        Self {
            queue: Mutex::new(SampleQueue::default()),
            volume: AtomicU32::new(volume.to_bits()),
            paused: AtomicBool::new(true),
            crossfade_ms: AtomicU32::new(0),
            style: AtomicU8::new(0),
            trim_silence: AtomicBool::new(false),
            match_volume: AtomicBool::new(false),
        }
    }
}

enum Cmd {
    Configure(u32),
    Play,
    Pause,
}

#[derive(Clone)]
pub struct Output {
    tx: Sender<Cmd>,
    rate: Arc<AtomicU32>,
}

impl Output {
    pub fn start(shared: Arc<Shared>) -> Self {
        let (tx, rx) = channel();
        let rate = Arc::new(AtomicU32::new(0));
        let r = rate.clone();
        thread::Builder::new()
            .name("audio-output".into())
            .spawn(move || run(shared, rx, r))
            .expect("spawn audio output thread");
        Self { tx, rate }
    }

    /// Current stream sample rate (0 before the first track).
    pub fn rate(&self) -> u32 {
        self.rate.load(Ordering::Relaxed)
    }

    /// (Re)opens the stream at `rate` if it isn't already.
    pub fn configure(&self, rate: u32) {
        let _ = self.tx.send(Cmd::Configure(rate));
    }

    pub fn play(&self) {
        let _ = self.tx.send(Cmd::Play);
    }

    pub fn pause(&self) {
        let _ = self.tx.send(Cmd::Pause);
    }
}

fn run(shared: Arc<Shared>, rx: Receiver<Cmd>, current_rate: Arc<AtomicU32>) {
    let broken = Arc::new(AtomicBool::new(false));
    let mut stream: Option<cpal::Stream> = None;
    let mut rate = 0;
    let mut playing = false;
    for cmd in rx {
        match cmd {
            Cmd::Configure(r) => {
                if r == rate && stream.is_some() && !broken.load(Ordering::Relaxed) {
                    continue;
                }
                stream = None;
                rate = r;
                current_rate.store(r, Ordering::Relaxed);
                broken.store(false, Ordering::Relaxed);
                match open(shared.clone(), r, broken.clone()) {
                    Ok(s) => {
                        if playing {
                            let _ = s.play();
                        } else {
                            let _ = s.pause();
                        }
                        stream = Some(s);
                    }
                    Err(e) => eprintln!("cumulus audio: {e}"),
                }
            }
            Cmd::Play => {
                playing = true;
                // Device unplugged / default device changed: reopen.
                if broken.load(Ordering::Relaxed) && rate != 0 {
                    stream = open(shared.clone(), rate, broken.clone()).ok();
                    broken.store(false, Ordering::Relaxed);
                }
                if let Some(s) = &stream {
                    let _ = s.play();
                }
            }
            Cmd::Pause => {
                playing = false;
                if let Some(s) = &stream {
                    let _ = s.pause();
                }
            }
        }
    }
}

fn open(shared: Arc<Shared>, rate: u32, broken: Arc<AtomicBool>) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("no audio output device")?;
    let config = cpal::StreamConfig {
        channels: CHANNELS as u16,
        sample_rate: rate,
        buffer_size: cpal::BufferSize::Default,
    };
    device
        .build_output_stream::<f32, _, _>(
            config,
            move |data, _| fill(&shared, data),
            move |e| {
                eprintln!("cumulus audio stream: {e}");
                broken.store(true, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|e| e.to_string())
}

/// Stream callback: copy queued samples out, apply volume, pad with silence.
fn fill(shared: &Shared, data: &mut [f32]) {
    let gain = {
        let v = f32::from_bits(shared.volume.load(Ordering::Relaxed));
        v * v
    };
    let mut filled = 0;
    // Never block the audio thread; on the rare contention just output silence.
    if let Ok(mut q) = shared.queue.try_lock() {
        let n = data.len().min(q.samples.len()) / CHANNELS * CHANNELS;
        for (d, s) in data[..n].iter_mut().zip(q.samples.drain(..n)) {
            *d = s * gain;
        }
        q.consumed += (n / CHANNELS) as u64;
        q.prune();
        filled = n;
    }
    data[filled..].fill(0.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_follows_boundaries() {
        let mut q = SampleQueue::default();
        q.mark(1, 0.0, 10);
        q.push(&[0.0; 40]); // 20 frames = 2 s of track 1
        q.mark(2, 0.0, 10);
        q.push(&[0.0; 20]);
        q.consumed = 15;
        assert_eq!(q.now(), Some((1, 1.5)));
        q.consumed = 25;
        assert_eq!(q.now(), Some((2, 0.5)));
        q.prune();
        assert_eq!(q.boundaries.len(), 1);
        q.flush();
        assert_eq!(q.buffered_frames(), 0);
        assert_eq!(q.now(), None);
    }
}
