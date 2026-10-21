//! Playing the user's own music files (MP3, AAC/M4A, FLAC, WAV/AIFF,
//! OGG Vorbis, ALAC) and reading their tags.

use std::fs::File;
use std::path::Path;

use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia_core::errors::Error as SymError;
use symphonia_core::formats::probe::Hint;
use symphonia_core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia_core::io::MediaSourceStream;
use symphonia_core::meta::{MetadataOptions, StandardTag};
use symphonia_core::units::Time;

use super::source::{Source, Step, append_stereo};

/// File extensions treated as music.
pub const EXTENSIONS: [&str; 10] = ["mp3", "m4a", "aac", "mp4", "flac", "wav", "aif", "aiff", "ogg", "oga"];

fn open_reader(path: &str) -> Result<Box<dyn FormatReader>, String> {
    let file = File::open(path).map_err(|e| format!("can't open the file ({e})"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = Path::new(path).extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| format!("not a playable audio file ({e})"))
}

pub struct LocalSource {
    track_id: u64,
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    stream: u32,
    rate: u32,
    duration: f64,
    start: f64,
    decoded: u64,
    skip: u64,
    scratch: Vec<f32>,
}

impl LocalSource {
    pub fn open(track_id: u64, path: &str, start: f64) -> Result<Self, String> {
        let reader = open_reader(path)?;
        let track = reader.default_track(TrackType::Audio).ok_or("no audio in this file")?;
        let Some(CodecParameters::Audio(params)) = track.codec_params.clone() else {
            return Err("no audio in this file".into());
        };
        let stream = track.id;
        let rate = params.sample_rate.unwrap_or(44_100);
        let duration = track.num_frames.map_or(0.0, |n| n as f64 / f64::from(rate));
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .map_err(|e| format!("unsupported audio format ({e})"))?;
        let mut src = Self { track_id, reader, decoder, stream, rate, duration, start: 0.0, decoded: 0, skip: 0, scratch: Vec::new() };
        if start > 0.5 {
            src.seek(start)?;
        }
        Ok(src)
    }
}

impl Source for LocalSource {
    fn track_id(&self) -> u64 {
        self.track_id
    }

    fn rate(&self) -> u32 {
        self.rate
    }

    fn position(&self) -> f64 {
        self.start + self.decoded as f64 / f64::from(self.rate)
    }

    fn duration(&self) -> f64 {
        self.duration
    }

    fn seek(&mut self, to: f64) -> Result<(), String> {
        let time = Time::try_from_secs_f64(to.max(0.0)).ok_or("bad seek position")?;
        self.reader
            .seek(SeekMode::Coarse, SeekTo::Time { time, track_id: Some(self.stream) })
            .map_err(|e| format!("can't seek in this file ({e})"))?;
        self.decoder.reset();
        self.start = to.max(0.0);
        self.decoded = 0;
        Ok(())
    }

    fn decode_next(&mut self, out: &mut Vec<f32>) -> Result<Step, String> {
        let packet = loop {
            match self.reader.next_packet() {
                Ok(Some(p)) if p.track_id == self.stream => break p,
                Ok(Some(_)) => continue, // another stream (cover art, video)
                Ok(None) => return Ok(Step::Ended),
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(Step::Ended),
                Err(e) => return Err(format!("can't read the file ({e})")),
            }
        };
        let channels = match self.decoder.decode(&packet) {
            Ok(buf) => {
                let ch = buf.spec().channels().count();
                buf.copy_to_vec_interleaved(&mut self.scratch);
                ch
            }
            Err(SymError::DecodeError(_)) => return Ok(Step::Produced),
            Err(e) => return Err(format!("audio decode failed ({e})")),
        };
        append_stereo(&self.scratch, channels, &mut self.skip, &mut self.decoded, out);
        Ok(Step::Produced)
    }
}

/// What a file's tags say.
#[derive(Default, Debug)]
pub struct Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: u64,
    /// Embedded cover picture (JPEG/PNG bytes).
    pub picture: Option<Vec<u8>>,
}

/// Reads tags and length without decoding any audio.
pub fn read_tags(path: &str) -> Result<Tags, String> {
    let mut reader = open_reader(path)?;
    let mut tags = Tags::default();
    if let Some(track) = reader.default_track(TrackType::Audio) {
        if let Some(CodecParameters::Audio(p)) = &track.codec_params {
            let rate = f64::from(p.sample_rate.unwrap_or(44_100));
            tags.duration_ms = track.num_frames.map_or(0, |n| (n as f64 / rate * 1000.0) as u64);
        }
    }
    let mut meta = reader.metadata();
    if let Some(rev) = meta.skip_to_latest() {
        let containers = std::iter::once(&rev.media).chain(rev.per_track.iter().map(|t| &t.metadata));
        for c in containers {
            for tag in &c.tags {
                match &tag.std {
                    Some(StandardTag::TrackTitle(s)) if tags.title.is_none() => tags.title = Some(s.to_string()),
                    Some(StandardTag::Artist(s)) if tags.artist.is_none() => tags.artist = Some(s.to_string()),
                    Some(StandardTag::AlbumArtist(s)) if tags.artist.is_none() => tags.artist = Some(s.to_string()),
                    Some(StandardTag::Album(s)) if tags.album.is_none() => tags.album = Some(s.to_string()),
                    _ => {}
                }
            }
            if tags.picture.is_none() {
                tags.picture = c.visuals.first().map(|v| v.data.to_vec());
            }
        }
    }
    Ok(tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a 1-second 44.1 kHz stereo 16-bit WAV of a 440 Hz tone.
    fn wav(path: &std::path::Path) {
        let rate = 44_100u32;
        let frames = rate;
        let mut data = Vec::with_capacity(frames as usize * 4);
        for i in 0..frames {
            let s = ((2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate as f32).sin() * 12_000.0) as i16;
            data.extend_from_slice(&s.to_le_bytes());
            data.extend_from_slice(&s.to_le_bytes());
        }
        let mut f = Vec::new();
        f.extend_from_slice(b"RIFF");
        f.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        f.extend_from_slice(b"WAVEfmt ");
        f.extend_from_slice(&16u32.to_le_bytes());
        f.extend_from_slice(&1u16.to_le_bytes()); // PCM
        f.extend_from_slice(&2u16.to_le_bytes());
        f.extend_from_slice(&rate.to_le_bytes());
        f.extend_from_slice(&(rate * 4).to_le_bytes());
        f.extend_from_slice(&4u16.to_le_bytes());
        f.extend_from_slice(&16u16.to_le_bytes());
        f.extend_from_slice(b"data");
        f.extend_from_slice(&(data.len() as u32).to_le_bytes());
        f.extend_from_slice(&data);
        std::fs::write(path, f).unwrap();
    }

    #[test]
    fn plays_a_wav_file() {
        let path = std::env::temp_dir().join(format!("cumulus-test-{}.wav", std::process::id()));
        wav(&path);
        let p = path.display().to_string();
        let tags = read_tags(&p).unwrap();
        assert!((990..=1010).contains(&tags.duration_ms), "{}", tags.duration_ms);
        let mut src = LocalSource::open(7, &p, 0.0).unwrap();
        assert_eq!((src.rate(), src.track_id()), (44_100, 7));
        let mut out = Vec::new();
        while let Ok(Step::Produced) = src.decode_next(&mut out) {}
        let seconds = out.len() as f64 / 2.0 / 44_100.0;
        assert!((seconds - 1.0).abs() < 0.02, "{seconds}");
        assert!(out.iter().any(|s| s.abs() > 0.3)); // it's the tone, not silence
        src.seek(0.5).unwrap();
        assert!((src.position() - 0.5).abs() < 0.01);
        let _ = std::fs::remove_file(&path);
    }
}
