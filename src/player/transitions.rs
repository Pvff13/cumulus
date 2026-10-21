//! Transitions between tracks: crossfade styles, silence trimming and
//! volume matching.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::thread;

use super::output::CHANNELS;
use super::source::{self, QueueItem, Source, Step};
use crate::api::Api;

/// How a crossfade sounds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    /// Equal-power curves: even loudness all the way through.
    Smooth,
    /// Straight-line volume ramps (a slight dip in the middle).
    Linear,
    /// The old track fades out, a short pause, then the new one fades in.
    Dip,
    /// DJ-style: the new track comes in without bass, the basslines swap
    /// halfway, and the old track leaves thin.
    FilterSweep,
    /// The old track's last moments repeat as a fading echo.
    EchoOut,
}

impl Style {
    pub const ALL: [Style; 5] = [Style::Smooth, Style::Linear, Style::Dip, Style::FilterSweep, Style::EchoOut];

    pub fn label(self) -> &'static str {
        match self {
            Style::Smooth => "Smooth",
            Style::Linear => "Linear",
            Style::Dip => "Fade out, then in",
            Style::FilterSweep => "Filter sweep (bass swap)",
            Style::EchoOut => "Echo out",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Style::Smooth => "Even loudness all the way through the fade.",
            Style::Linear => "Simple straight fades, with a soft dip in the middle.",
            Style::Dip => "The old track fades away, a short pause, then the new one fades up.",
            Style::FilterSweep => "Like a DJ: the new track comes in without bass and the basslines swap halfway.",
            Style::EchoOut => "The end of the old track trails off as an echo while the new one comes in.",
        }
    }

    /// Name stored in config.json.
    pub fn key(self) -> &'static str {
        match self {
            Style::Smooth => "smooth",
            Style::Linear => "linear",
            Style::Dip => "dip",
            Style::FilterSweep => "filter",
            Style::EchoOut => "echo",
        }
    }

    pub fn from_key(key: &str) -> Style {
        Style::ALL.into_iter().find(|s| s.key() == key).unwrap_or(Style::Smooth)
    }

    pub fn index(self) -> u8 {
        Style::ALL.iter().position(|s| *s == self).unwrap_or(0) as u8
    }

    pub fn from_index(i: u8) -> Style {
        Style::ALL.get(i as usize).copied().unwrap_or(Style::Smooth)
    }
}

/// (fade-in gain, fade-out gain) at `t` = 0..1 through a crossfade.
pub fn gains(style: Style, t: f32) -> (f32, f32) {
    let t = t.clamp(0.0, 1.0);
    match style {
        Style::Smooth => equal_power(t),
        Style::Linear => (t, 1.0 - t),
        Style::Dip => {
            let out = if t < 0.45 { equal_power(t / 0.45).1 } else { 0.0 };
            let inn = if t > 0.55 { equal_power((t - 0.55) / 0.45).0 } else { 0.0 };
            (inn, out)
        }
        // Both tracks are briefly at full level around the bass swap.
        Style::FilterSweep => (equal_power((t / 0.6).min(1.0)).0, equal_power(((t - 0.4) / 0.6).max(0.0)).1),
        // The old track's dry signal stops early; its echo carries on (see Echo).
        Style::EchoOut => {
            let inn = if t > 0.35 { equal_power((t - 0.35) / 0.65).0 } else { 0.0 };
            let dry = if t < 0.3 { 1.0 } else { equal_power(((t - 0.3) / 0.2).min(1.0)).1 };
            (inn, dry)
        }
    }
}

/// Equal-power curves (sin/cos): the combined loudness stays even.
pub fn equal_power(t: f32) -> (f32, f32) {
    let a = t.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
    (a.sin(), a.cos())
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Gently rounds off peaks above 0.9 instead of hard clipping (two tracks
/// mixed, or a boosted quiet track, can go over full scale).
pub fn soft_clip(x: f32) -> f32 {
    let a = x.abs();
    if a <= 0.9 { x } else { x.signum() * (0.9 + 0.1 * ((a - 0.9) / 0.1).tanh()) }
}

/// A second-order high-pass filter (RBJ cookbook), one state per channel.
#[derive(Clone)]
pub struct HighPass {
    b: [f32; 3],
    a: [f32; 2],
    state: [[f32; 4]; CHANNELS],
}

impl HighPass {
    pub fn new(cutoff: f32, rate: u32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * cutoff / rate as f32;
        let alpha = w0.sin() / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let cos = w0.cos();
        let a0 = 1.0 + alpha;
        Self {
            b: [(1.0 + cos) / 2.0 / a0, -(1.0 + cos) / a0, (1.0 + cos) / 2.0 / a0],
            a: [-2.0 * cos / a0, (1.0 - alpha) / a0],
            state: [[0.0; 4]; CHANNELS],
        }
    }

    pub fn process(&mut self, channel: usize, x: f32) -> f32 {
        let s = &mut self.state[channel];
        let y = self.b[0] * x + self.b[1] * s[0] + self.b[2] * s[1] - self.a[0] * s[2] - self.a[1] * s[3];
        *s = [x, s[0], y, s[2]];
        y
    }
}

/// A feedback delay for the echo-out style.
struct Echo {
    line: Vec<f32>,
    pos: usize,
}

const ECHO_SECS: f32 = 0.375;
const ECHO_FEEDBACK: f32 = 0.55;

impl Echo {
    fn new(rate: u32) -> Self {
        let frames = ((ECHO_SECS * rate as f32) as usize).max(1);
        Self { line: vec![0.0; frames * CHANNELS], pos: 0 }
    }

    /// Feeds `send` into the delay and returns the echo coming out of it.
    fn process(&mut self, channel: usize, send: f32) -> f32 {
        let i = self.pos + channel;
        let wet = self.line[i];
        self.line[i] = send + wet * ECHO_FEEDBACK;
        if channel == CHANNELS - 1 {
            self.pos = (self.pos + CHANNELS) % self.line.len();
        }
        wet
    }
}

/// A track fading out while the next one fades in.
pub struct Fade {
    src: Box<dyn Source>,
    style: Style,
    /// Volume-matching gain of the outgoing track.
    gain: f32,
    /// Decoded samples not mixed in yet.
    pending: Vec<f32>,
    pub done: u64,
    pub total: u64,
    ended: bool,
    hp_in: HighPass,
    hp_out: HighPass,
    echo: Option<Echo>,
    /// Debug builds: how much outgoing audio was actually mixed in.
    #[cfg(debug_assertions)]
    pub energy: f32,
}

impl Fade {
    pub fn new(src: Box<dyn Source>, style: Style, gain: f32, total: u64) -> Self {
        let rate = src.rate();
        Self {
            style,
            gain,
            pending: Vec::new(),
            done: 0,
            total: total.max(1),
            ended: false,
            hp_in: HighPass::new(220.0, rate),
            hp_out: HighPass::new(220.0, rate),
            echo: (style == Style::EchoOut).then(|| Echo::new(rate)),
            src,
            #[cfg(debug_assertions)]
            energy: 0.0,
        }
    }

    #[cfg(debug_assertions)]
    pub fn track_id(&self) -> u64 {
        self.src.track_id()
    }

    /// Mixes the next stretch of the outgoing track into `mix` (the incoming
    /// track's audio). Returns false once the fade is over.
    pub fn mix_into(&mut self, mix: &mut [f32]) -> bool {
        let mut tries = 0;
        while !self.ended && self.pending.len() < mix.len() && tries < 8 {
            tries += 1;
            match self.src.decode_next(&mut self.pending) {
                Ok(Step::Produced) => {}
                Ok(Step::Waiting) => break,
                Ok(Step::Ended) | Err(_) => self.ended = true,
            }
        }
        let frames = mix.len() / CHANNELS;
        let have = self.pending.len().min(mix.len());
        for f in 0..frames {
            let t = ((self.done + f as u64) as f32 / self.total as f32).min(1.0);
            let (gain_in, gain_out) = gains(self.style, t);
            // Filter sweep: blend from bass-cut to full (incoming) and from
            // full to bass-cut (outgoing) around the middle.
            let swap = smoothstep(0.4, 0.6, t);
            let echo_wet = smoothstep(0.15, 0.3, t) * (1.0 - smoothstep(0.85, 1.0, t)) * 0.7;
            for c in 0..CHANNELS {
                let i = f * CHANNELS + c;
                let mut new = mix[i];
                let mut old = if i < have { self.pending[i] * self.gain } else { 0.0 };
                match self.style {
                    Style::FilterSweep => {
                        let new_hp = self.hp_in.process(c, new);
                        let old_hp = self.hp_out.process(c, old);
                        new = new_hp + (new - new_hp) * swap;
                        old = old + (old_hp - old) * swap;
                        old *= gain_out;
                    }
                    Style::EchoOut => {
                        let dry = old * gain_out;
                        let wet = self.echo.as_mut().map_or(0.0, |e| e.process(c, dry));
                        old = dry + wet * echo_wet;
                    }
                    _ => old *= gain_out,
                }
                #[cfg(debug_assertions)]
                {
                    self.energy += old.abs() / 1000.0;
                }
                mix[i] = soft_clip(new * gain_in + old);
            }
        }
        self.pending.drain(..have);
        self.done += frames as u64;
        // An echo keeps ringing after the old track's audio runs out.
        let out_of_audio = self.ended && self.pending.is_empty() && self.style != Style::EchoOut;
        self.done < self.total && !out_of_audio
    }
}

/// Below this level (about -48 dBFS) audio counts as silence.
pub const SILENCE: f32 = 0.004;
/// Trailing silence shorter than this is left alone (natural endings).
const MIN_TRAILING_SILENCE: f64 = 1.0;
/// Leading silence is skipped up to this much.
pub const MAX_LEADING_TRIM: f64 = 8.0;
/// Target loudness for volume matching (RMS, about -13 dBFS).
const TARGET_RMS: f32 = 0.22;

/// What a background scan found out about a track.
#[derive(Debug, Default, Clone, Copy)]
pub struct Scan {
    /// Where the music really ends (before trailing silence), seconds.
    pub music_end: Option<f64>,
    /// Gain that brings the track to the target loudness.
    pub gain: Option<f32>,
}

/// Opens a second copy of `item` in the background to find its trailing
/// silence and loudness, so transitions can plan ahead. Decodes about 40 s.
pub fn spawn_scan(api: Arc<Api>, item: QueueItem, duration: f64) -> Receiver<Scan> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let mut scan = Scan::default();
        // Loudness: 15 s from the middle (intros and outros are quieter).
        if let Ok(mut src) = source::open(&api, &item, (duration / 2.0 - 7.5).max(0.0)) {
            let samples = decode_for(&mut *src, 15.0);
            scan.gain = loudness_gain(&samples);
        }
        // Trailing silence: the last 25 s.
        let tail_from = (duration - 25.0).max(0.0);
        if let Ok(mut src) = source::open(&api, &item, tail_from) {
            let start = src.position();
            let rate = src.rate();
            let samples = decode_for(&mut *src, f64::INFINITY);
            scan.music_end = music_end(&samples, rate, start);
        }
        let _ = tx.send(scan);
    });
    rx
}

fn decode_for(src: &mut dyn Source, secs: f64) -> Vec<f32> {
    let until = src.position() + secs;
    let mut out = Vec::new();
    let mut waits = 0;
    while src.position() < until && waits < 200 {
        match src.decode_next(&mut out) {
            Ok(Step::Produced) => {}
            Ok(Step::Waiting) => {
                waits += 1;
                thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok(Step::Ended) | Err(_) => break,
        }
    }
    out
}

/// Gain to reach the target loudness, kept within ±6 dB.
pub fn loudness_gain(samples: &[f32]) -> Option<f32> {
    if samples.len() < 44_100 {
        return None;
    }
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    if rms < 0.01 {
        return None;
    }
    Some((TARGET_RMS / rms).clamp(0.5, 2.0))
}

/// End of the music in `samples` (which start at `start` seconds), if it's
/// followed by at least a second of silence.
pub fn music_end(samples: &[f32], rate: u32, start: f64) -> Option<f64> {
    let frames = samples.len() / CHANNELS;
    let last_loud = samples.chunks_exact(CHANNELS).rposition(|f| f.iter().any(|s| s.abs() > SILENCE));
    let end_frame = last_loud.map_or(0, |f| f + 1);
    let silence = (frames - end_frame) as f64 / f64::from(rate);
    // A little room after the last audible sample so endings aren't clipped.
    (silence >= MIN_TRAILING_SILENCE).then(|| start + end_frame as f64 / f64::from(rate) + 0.25)
}

/// Number of samples of leading silence at the start of `samples`.
pub fn leading_silence(samples: &[f32]) -> usize {
    samples
        .chunks_exact(CHANNELS)
        .position(|f| f.iter().any(|s| s.abs() > SILENCE))
        .map_or(samples.len(), |f| f * CHANNELS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_power_curve() {
        assert_eq!(equal_power(0.0), (0.0, 1.0));
        let (i, o) = equal_power(1.0);
        assert!((i - 1.0).abs() < 1e-6 && o.abs() < 1e-6);
        for t in [0.1f32, 0.25, 0.5, 0.75, 0.9] {
            let (i, o) = equal_power(t);
            assert!((i * i + o * o - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn every_style_starts_old_and_ends_new() {
        for style in Style::ALL {
            let (i0, o0) = gains(style, 0.0);
            let (i1, o1) = gains(style, 1.0);
            assert!(i0 < 0.01 && o0 > 0.99, "{style:?} start");
            assert!(i1 > 0.99 && o1 < 0.01, "{style:?} end");
            assert_eq!(Style::from_key(style.key()), style);
            assert_eq!(Style::from_index(style.index()), style);
        }
        // The dip really has a silent moment in the middle.
        assert_eq!(gains(Style::Dip, 0.5), (0.0, 0.0));
    }

    #[test]
    fn high_pass_removes_bass() {
        let rate = 44_100;
        let mut hp = HighPass::new(220.0, rate);
        let energy = |hp: &mut HighPass, freq: f32| {
            (0..rate)
                .map(|n| hp.process(0, (2.0 * std::f32::consts::PI * freq * n as f32 / rate as f32).sin()).powi(2))
                .skip(4410)
                .sum::<f32>()
        };
        let bass = energy(&mut hp, 50.0);
        let mut hp = HighPass::new(220.0, rate);
        let mids = energy(&mut hp, 2000.0);
        assert!(bass < mids * 0.1, "bass {bass} mids {mids}");
    }

    #[test]
    fn silence_detection() {
        let rate = 1000;
        // 2 s of sound then 3 s of silence (stereo).
        let mut s = vec![0.5f32; 2 * rate as usize * CHANNELS];
        s.extend(vec![0.0f32; 3 * rate as usize * CHANNELS]);
        let end = music_end(&s, rate, 100.0).unwrap();
        assert!((end - 102.25).abs() < 0.01, "{end}");
        // Half a second of silence is a natural ending: leave it.
        let mut short = vec![0.5f32; 2 * rate as usize * CHANNELS];
        short.extend(vec![0.0f32; rate as usize / 2 * CHANNELS]);
        assert_eq!(music_end(&short, rate, 0.0), None);
        let mut lead = vec![0.0f32; 10 * CHANNELS];
        lead.extend([0.2, 0.2]);
        assert_eq!(leading_silence(&lead), 10 * CHANNELS);
    }

    #[test]
    fn loudness_matching() {
        let quiet = vec![0.04f32; 100_000];
        assert_eq!(loudness_gain(&quiet), Some(2.0)); // capped at +6 dB
        let loud = vec![0.44f32; 100_000];
        assert!((loudness_gain(&loud).unwrap() - 0.5).abs() < 1e-3);
        assert_eq!(loudness_gain(&[0.0; 100_000]), None);
        assert!(soft_clip(1.5) < 1.0 && soft_clip(-1.5) > -1.0 && soft_clip(0.5) == 0.5);
    }
}
