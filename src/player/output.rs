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
    /// Equalizer: on/off, preamp + 5 band gains (dB, f32 bits), and a
    /// counter bumped on every change so the callback rebuilds its filters.
    pub eq_on: AtomicBool,
    pub eq_gains: [AtomicU32; 6],
    pub eq_version: AtomicU32,
    /// Playback speed (f32 bits, 1.0 = normal).
    pub speed: AtomicU32,
    /// Output device by name (None = the system default).
    pub device: Mutex<Option<String>>,
    /// Visualizer feed: the latest mono samples and their rate (only filled
    /// while `viz_on`).
    pub viz_on: AtomicBool,
    pub viz: Mutex<Vec<f32>>,
    pub viz_rate: AtomicU32,
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
            eq_on: AtomicBool::new(false),
            eq_gains: std::array::from_fn(|_| AtomicU32::new(0f32.to_bits())),
            eq_version: AtomicU32::new(0),
            speed: AtomicU32::new(1f32.to_bits()),
            device: Mutex::new(None),
            viz_on: AtomicBool::new(false),
            viz: Mutex::new(Vec::new()),
            viz_rate: AtomicU32::new(0),
        }
    }
}

enum Cmd {
    Configure(u32),
    /// Speed changed: reopen the stream at the new effective rate.
    Reopen,
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

    /// Re-applies `Shared::speed` (reopens the stream).
    pub fn reopen(&self) {
        let _ = self.tx.send(Cmd::Reopen);
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
            Cmd::Reopen if rate != 0 && stream.is_some() => {
                stream = None;
                broken.store(false, Ordering::Relaxed);
                match open(shared.clone(), rate, broken.clone()) {
                    Ok(s) => {
                        let _ = if playing { s.play() } else { s.pause() };
                        stream = Some(s);
                    }
                    Err(e) => eprintln!("cumulus audio: {e}"),
                }
            }
            Cmd::Reopen => {}
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

/// Opens the device stream for audio at the track's `rate`. Playback speed
/// works like a record player: the stream claims a faster/slower rate and
/// Windows' resampler does the rest (pitch follows speed).
fn open(shared: Arc<Shared>, rate: u32, broken: Arc<AtomicBool>) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let wanted = shared.device.lock().ok().and_then(|d| d.clone());
    // The chosen device if it's still there, else the system default.
    let device = wanted
        .and_then(|name| host.output_devices().ok()?.find(|d| device_name(d).as_deref() == Some(name.as_str())))
        .or_else(|| host.default_output_device())
        .ok_or("no audio output device")?;
    let speed = f32::from_bits(shared.speed.load(Ordering::Relaxed)).clamp(0.5, 2.0);
    let rate = (rate as f32 * speed).round() as u32;
    let config = cpal::StreamConfig {
        channels: CHANNELS as u16,
        sample_rate: rate,
        buffer_size: cpal::BufferSize::Default,
    };
    let mut eq = super::eq::Equalizer::new(rate);
    shared.viz_rate.store(rate, Ordering::Relaxed);
    device
        .build_output_stream::<f32, _, _>(
            config,
            move |data, _| fill(&shared, &mut eq, data),
            move |e| {
                eprintln!("cumulus audio stream: {e}");
                broken.store(true, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|e| e.to_string())
}

fn device_name(d: &cpal::Device) -> Option<String> {
    d.description().ok().map(|d| d.name().to_string())
}

/// Names of the output devices, for the Settings picker.
pub fn device_names() -> Vec<String> {
    let host = cpal::default_host();
    let mut names: Vec<String> = host.output_devices().map(|ds| ds.filter_map(|d| device_name(&d)).collect()).unwrap_or_default();
    names.dedup();
    names
}

/// Stream callback: copy queued samples out, equalize, apply volume, pad
/// with silence.
fn fill(shared: &Shared, eq: &mut super::eq::Equalizer, data: &mut [f32]) {
    let gain = {
        let v = f32::from_bits(shared.volume.load(Ordering::Relaxed));
        v * v
    };
    let mut filled = 0;
    // Never block the audio thread; on the rare contention just output silence.
    if let Ok(mut q) = shared.queue.try_lock() {
        let n = data.len().min(q.samples.len()) / CHANNELS * CHANNELS;
        for (d, s) in data[..n].iter_mut().zip(q.samples.drain(..n)) {
            *d = s;
        }
        q.consumed += (n / CHANNELS) as u64;
        q.prune();
        filled = n;
    }
    eq.process(shared, &mut data[..filled]);
    if shared.viz_on.load(Ordering::Relaxed) {
        if let Ok(mut v) = shared.viz.try_lock() {
            v.extend(data[..filled].chunks_exact(CHANNELS).map(|f| (f[0] + f[1]) * 0.5));
            let len = v.len();
            if len > 8192 {
                v.drain(..len - 4096);
            }
        }
    }
    for d in &mut data[..filled] {
        *d *= gain;
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
