//! Spectrum visualizer drawn behind the player bar. Fed by the audio
//! output (after the EQ), only while switched on.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Style {
    #[default]
    Off,
    Bars,
    Mirrored,
    Line,
}

impl Style {
    pub const ALL: [Style; 4] = [Self::Off, Self::Bars, Self::Mirrored, Self::Line];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Bars => "Bars",
            Self::Mirrored => "Mirrored",
            Self::Line => "Line",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Bars => "bars",
            Self::Mirrored => "mirrored",
            Self::Line => "line",
        }
    }

    pub fn from_key(k: &str) -> Self {
        Self::ALL.into_iter().find(|s| s.key() == k).unwrap_or_default()
    }
}

const BANDS: usize = 48;
const FFT: usize = 2048;

#[derive(Default)]
pub struct Visualizer {
    /// Smoothed band levels, 0..1.
    levels: Vec<f32>,
}

impl Visualizer {
    /// Updates the bands from the latest mono samples at `rate` Hz.
    pub fn update(&mut self, samples: &[f32], rate: f32) {
        self.levels.resize(BANDS, 0.0);
        if samples.len() < FFT || rate <= 0.0 {
            for l in &mut self.levels {
                *l *= 0.85;
            }
            return;
        }
        let tail = &samples[samples.len() - FFT..];
        // Hann window, then the spectrum.
        let mut re: Vec<f32> = tail
            .iter()
            .enumerate()
            .map(|(i, s)| s * (0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT - 1) as f32).cos()))
            .collect();
        let mut im = vec![0.0; FFT];
        fft(&mut re, &mut im);
        let mag = |bin: usize| (re[bin] * re[bin] + im[bin] * im[bin]).sqrt() / (FFT as f32 / 4.0);
        // Log-spaced bands from 40 Hz to 16 kHz.
        let (lo, hi) = (40f32.ln(), 16_000f32.min(rate / 2.0).ln());
        for (b, level) in self.levels.iter_mut().enumerate() {
            let f0 = (lo + (hi - lo) * b as f32 / BANDS as f32).exp();
            let f1 = (lo + (hi - lo) * (b + 1) as f32 / BANDS as f32).exp();
            let (i0, i1) = ((f0 * FFT as f32 / rate) as usize, ((f1 * FFT as f32 / rate) as usize).max(1));
            let peak = (i0.max(1)..=i1.min(FFT / 2 - 1).max(i0.max(1))).map(mag).fold(0.0f32, f32::max);
            // -60 dB..0 dB -> 0..1, with a little tilt so highs aren't flat.
            let db = 20.0 * (peak.max(1e-6)).log10() + 6.0 * (b as f32 / BANDS as f32);
            let target = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
            *level = if target > *level { target } else { *level * 0.82 + target * 0.18 };
        }
    }

    pub fn draw(&self, painter: &egui::Painter, rect: Rect, style: Style, color: Color32) {
        if self.levels.is_empty() || style == Style::Off {
            return;
        }
        let n = self.levels.len() as f32;
        let w = rect.width() / n;
        match style {
            Style::Bars | Style::Mirrored => {
                for (i, l) in self.levels.iter().enumerate() {
                    let h = l * rect.height();
                    let x = rect.left() + i as f32 * w;
                    let bar = if style == Style::Bars {
                        Rect::from_min_max(egui::pos2(x + 1.0, rect.bottom() - h), egui::pos2(x + w - 1.0, rect.bottom()))
                    } else {
                        let c = rect.center().y;
                        Rect::from_min_max(egui::pos2(x + 1.0, c - h / 2.0), egui::pos2(x + w - 1.0, c + h / 2.0))
                    };
                    painter.rect_filled(bar, 1.0, color);
                }
            }
            Style::Line => {
                let points: Vec<Pos2> = self
                    .levels
                    .iter()
                    .enumerate()
                    .map(|(i, l)| egui::pos2(rect.left() + (i as f32 + 0.5) * w, rect.bottom() - l * rect.height()))
                    .collect();
                painter.add(egui::Shape::line(points, Stroke::new(2.0, color)));
            }
            Style::Off => {}
        }
    }
}

/// In-place radix-2 FFT (length must be a power of two).
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f32::consts::PI / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                (cr, ci) = (cr * wr - ci * wi, cr * wi + ci * wr);
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_finds_a_sine() {
        let n = 1024;
        let mut re: Vec<f32> = (0..n).map(|i| (2.0 * std::f32::consts::PI * 64.0 * i as f32 / n as f32).sin()).collect();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im);
        let mag: Vec<f32> = (0..n / 2).map(|i| (re[i] * re[i] + im[i] * im[i]).sqrt()).collect();
        let peak = mag.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        assert_eq!(peak, 64);
    }

    #[test]
    fn a_tone_lights_the_right_band() {
        let rate = 44_100.0;
        let samples: Vec<f32> = (0..FFT * 2).map(|i| 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / rate).sin()).collect();
        let mut v = Visualizer::default();
        v.update(&samples, rate);
        let loudest = v.levels.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        // 1 kHz on a 40 Hz-16 kHz log scale of 48 bands is band ~25.
        assert!((23..=27).contains(&loudest), "{loudest}");
    }
}
