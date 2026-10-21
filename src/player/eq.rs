//! Five-band equalizer with a preamp, run in the audio callback so changes
//! are heard immediately (not after the decode buffer drains).

use std::sync::atomic::Ordering;

use super::output::{CHANNELS, Shared};

/// Centre frequencies: a low shelf, three peaks and a high shelf.
pub const BANDS: [f32; 5] = [60.0, 230.0, 910.0, 3600.0, 14000.0];
/// Labels for the preamp and the bands.
pub const LABELS: [&str; 6] = ["Pre", "60", "230", "910", "3.6k", "14k"];
/// Slider range, dB.
pub const RANGE: f32 = 12.0;

/// Presets: name and (preamp, bands...) in dB.
pub const PRESETS: [(&str, [f32; 6]); 7] = [
    ("Flat", [0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ("Bass boost", [-3.0, 6.0, 3.5, 0.0, 0.0, 0.0]),
    ("Treble boost", [-3.0, 0.0, 0.0, 0.0, 3.5, 6.0]),
    ("Vocal", [-2.0, -2.0, -1.0, 3.0, 4.0, 1.0]),
    ("Electronic", [-3.0, 5.0, 1.0, -1.0, 2.0, 4.0]),
    ("Late night", [0.0, -4.0, -2.0, 1.0, 2.0, -3.0]),
    ("Loudness", [-3.0, 4.0, 1.0, -1.0, 1.0, 4.0]),
];

/// The preset matching `gains`, if any.
pub fn preset_name(gains: &[f32; 6]) -> Option<&'static str> {
    PRESETS.iter().find(|(_, g)| g.iter().zip(gains).all(|(a, b)| (a - b).abs() < 0.01)).map(|(n, _)| *n)
}

#[derive(Clone, Copy, Default)]
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    /// Transposed direct form II state, per channel.
    z: [[f32; 2]; CHANNELS],
}

#[derive(Clone, Copy)]
enum Kind {
    LowShelf,
    Peak,
    HighShelf,
}

impl Biquad {
    /// RBJ audio-EQ-cookbook filters.
    fn new(kind: Kind, freq: f32, gain_db: f32, rate: f32) -> Self {
        let freq = freq.min(rate * 0.45);
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * freq / rate;
        let (sin, cos) = w0.sin_cos();
        let (b, a_) = match kind {
            Kind::Peak => {
                let alpha = sin / (2.0 * 1.0); // Q = 1
                ([1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a], [1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a])
            }
            Kind::LowShelf | Kind::HighShelf => {
                let alpha = sin / 2.0 * std::f32::consts::SQRT_2; // shelf slope 1
                let sq = 2.0 * a.sqrt() * alpha;
                let s = if matches!(kind, Kind::LowShelf) { 1.0 } else { -1.0 };
                (
                    [
                        a * ((a + 1.0) - s * (a - 1.0) * cos + sq),
                        s * 2.0 * a * ((a - 1.0) - s * (a + 1.0) * cos),
                        a * ((a + 1.0) - s * (a - 1.0) * cos - sq),
                    ],
                    [
                        (a + 1.0) + s * (a - 1.0) * cos + sq,
                        -s * 2.0 * ((a - 1.0) + s * (a + 1.0) * cos),
                        (a + 1.0) + s * (a - 1.0) * cos - sq,
                    ],
                )
            }
        };
        let a0 = a_[0];
        Self { b: [b[0] / a0, b[1] / a0, b[2] / a0], a: [a_[1] / a0, a_[2] / a0], z: [[0.0; 2]; CHANNELS] }
    }

    fn process(&mut self, c: usize, x: f32) -> f32 {
        let z = &mut self.z[c];
        let y = self.b[0] * x + z[0];
        z[0] = self.b[1] * x - self.a[0] * y + z[1];
        z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

pub struct Equalizer {
    rate: f32,
    /// `Shared::eq_version` the filters were built for.
    version: u32,
    on: bool,
    preamp: f32,
    filters: [Biquad; 5],
}

impl Equalizer {
    pub fn new(rate: u32) -> Self {
        Self { rate: rate as f32, version: u32::MAX, on: false, preamp: 1.0, filters: [Biquad::default(); 5] }
    }

    fn rebuild(&mut self, gains: [f32; 6]) {
        self.preamp = 10f32.powf(gains[0] / 20.0);
        for (i, f) in self.filters.iter_mut().enumerate() {
            let kind = match i {
                0 => Kind::LowShelf,
                4 => Kind::HighShelf,
                _ => Kind::Peak,
            };
            let state = f.z;
            *f = Biquad::new(kind, BANDS[i], gains[i + 1], self.rate);
            f.z = state; // keep the state so changes don't click
        }
    }

    /// Filters `data` (interleaved stereo) in place, if the EQ is on.
    pub fn process(&mut self, shared: &Shared, data: &mut [f32]) {
        let version = shared.eq_version.load(Ordering::Relaxed);
        if version != self.version {
            self.version = version;
            self.on = shared.eq_on.load(Ordering::Relaxed);
            let gains = std::array::from_fn(|i| f32::from_bits(shared.eq_gains[i].load(Ordering::Relaxed)));
            self.rebuild(gains);
        }
        if !self.on {
            return;
        }
        for frame in data.chunks_exact_mut(CHANNELS) {
            for (c, s) in frame.iter_mut().enumerate() {
                let mut x = *s * self.preamp;
                for f in &mut self.filters {
                    x = f.process(c, x);
                }
                *s = super::transitions::soft_clip(x);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gain (dB) of a filter at `freq`, measured with a sine.
    fn response(kind: Kind, centre: f32, gain: f32, freq: f32) -> f32 {
        let rate = 48_000.0;
        let mut f = Biquad::new(kind, centre, gain, rate);
        let n = 48_000;
        let (mut inp, mut out) = (0.0f32, 0.0f32);
        for i in 0..n {
            let x = (2.0 * std::f32::consts::PI * freq * i as f32 / rate).sin();
            let y = f.process(0, x);
            if i > n / 2 {
                inp += x * x;
                out += y * y;
            }
        }
        10.0 * (out / inp).log10()
    }

    #[test]
    fn bands_boost_where_they_should() {
        // A +6 dB peak at 910 Hz: about +6 there, about 0 far away.
        assert!((response(Kind::Peak, 910.0, 6.0, 910.0) - 6.0).abs() < 0.5);
        assert!(response(Kind::Peak, 910.0, 6.0, 12_000.0).abs() < 0.5);
        // Shelves: full gain well inside, none well outside.
        assert!((response(Kind::LowShelf, 60.0, 6.0, 20.0) - 6.0).abs() < 1.0);
        assert!(response(Kind::LowShelf, 60.0, 6.0, 5000.0).abs() < 0.5);
        assert!((response(Kind::HighShelf, 14_000.0, -6.0, 20_000.0) + 6.0).abs() < 1.5);
        assert!(response(Kind::HighShelf, 14_000.0, -6.0, 500.0).abs() < 0.5);
    }

    #[test]
    fn presets() {
        assert_eq!(preset_name(&[0.0; 6]), Some("Flat"));
        assert_eq!(preset_name(&PRESETS[1].1), Some("Bass boost"));
        assert_eq!(preset_name(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]), None);
    }
}
