//! Turning a track into decoded audio: pick a stream, resolve its URL, then
//! decode HLS/AAC (preferred) or progressive MP3. Streams are only ever held
//! in memory; nothing is written to disk.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::thread;
use std::time::Duration as StdDuration;

use serde::Deserialize;
use symphonia_bundle_mp3::{MpaDecoder, MpaReader};
use symphonia_codec_aac::AacDecoder;
use symphonia_core::codecs::CodecParameters;
use symphonia_core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia_core::errors::Error as SymError;
use symphonia_core::formats::{FormatOptions, FormatReader};
use symphonia_core::io::{MediaSourceStream, ReadOnlySource};
use symphonia_core::packet::PacketRef;
use symphonia_core::units::{Duration, Timestamp};

use super::fmp4::{self, InitInfo, Sample};
use super::hls::{self, HlsError, MediaPlaylist};
use crate::api::client::Agent;
use crate::api::models::{Availability, Track, Transcoding};
use crate::api::{Api, ApiError};

/// Failure reason for DRM-only tracks (the UI skips these quietly).
pub const DRM_REASON: &str = "DRM-protected: only plays on SoundCloud's own site and apps";

/// What the engine is asked to play: a track id, plus the full track if we
/// already have it (playlist entries may still be stubs).
#[derive(Debug, Clone)]
pub struct QueueItem {
    pub id: u64,
    pub track: Option<Arc<Track>>,
}

pub enum Step {
    /// Appended audio to the output buffer.
    Produced,
    /// Waiting on the network; try again shortly.
    Waiting,
    Ended,
}

pub trait Source: Send {
    fn track_id(&self) -> u64;
    fn rate(&self) -> u32;
    /// Seconds of the track decoded so far (including the start offset).
    fn position(&self) -> f64;
    fn duration(&self) -> f64;
    /// Decodes the next chunk and appends interleaved stereo f32 to `out`.
    fn decode_next(&mut self, out: &mut Vec<f32>) -> Result<Step, String>;
    fn seek(&mut self, to: f64) -> Result<(), String>;
}

#[derive(Deserialize)]
struct StreamUrl {
    url: String,
}

/// Resolves and opens `item` for playback starting at `start` seconds.
pub fn open(api: &Api, item: &QueueItem, start: f64) -> Result<Box<dyn Source>, String> {
    // The user's own files play straight from disk.
    if let Some(path) = item.track.as_ref().and_then(|t| t.local_path.clone()) {
        return Ok(Box::new(super::local::LocalSource::open(item.id, &path, start)?));
    }
    let mut track = match &item.track {
        Some(t) => t.clone(),
        None => fetch_track(api, item.id)?,
    };
    match track.availability() {
        Availability::Blocked => return Err("not available in your region".into()),
        Availability::Preview => return Err("preview only (needs SoundCloud Go+)".into()),
        Availability::Drm => return Err(DRM_REASON.into()),
        Availability::Playable => {}
    }
    let url = match resolve(api, &track) {
        // A saved/old track object can carry an expired authorization; refetch once.
        Err(ApiError::Forbidden | ApiError::Unauthorized | ApiError::NotFound) if item.track.is_some() => {
            track = fetch_track(api, item.id)?;
            resolve(api, &track)
        }
        r => r,
    }
    .map_err(|e| match e {
        ApiError::Forbidden | ApiError::NotFound => "not available (blocked in your region or removed)".to_string(),
        e => e.to_string(),
    })?;

    let t = pick(&track)?;
    let duration = track.duration as f64 / 1000.0;
    if t.format.protocol == "hls" {
        HlsSource::open(api.agent(), track.id, &url, start).map(|s| Box::new(s) as Box<dyn Source>)
    } else {
        Mp3Source::open(api.agent(), track.id, url, duration, start).map(|s| Box::new(s) as Box<dyn Source>)
    }
}

fn fetch_track(api: &Api, id: u64) -> Result<Arc<Track>, String> {
    let mut found = api.tracks(&[id]).map_err(|e| e.to_string())?;
    found.remove(&id).map(Arc::new).ok_or_else(|| "track no longer exists".into())
}

/// Preferred stream: plain HLS AAC (160k, else 96k), then progressive MP3.
/// Encrypted (DRM) streams are never used.
/// Stream quality: true prefers 160 kbps AAC (when the account gets it),
/// false the lighter 96-128 kbps streams.
pub static HIGH_QUALITY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

fn pick(track: &Track) -> Result<&Transcoding, String> {
    let all = track.media.as_ref().map(|m| m.transcodings.as_slice()).unwrap_or_default();
    let usable = |t: &&Transcoding| !t.snipped;
    let aac = |t: &&Transcoding| t.format.protocol == "hls" && t.format.mime_type.starts_with("audio/mp4");
    let high = HIGH_QUALITY.load(Ordering::Relaxed);
    let best = all
        .iter()
        .filter(usable)
        .filter(aac)
        .find(|t| t.preset.starts_with("aac_160") == high)
        .or_else(|| all.iter().filter(usable).find(aac))
        .or_else(|| {
            all.iter()
                .filter(usable)
                .find(|t| t.format.protocol == "progressive" && t.format.mime_type.starts_with("audio/mpeg"))
        });
    match best {
        Some(t) => Ok(t),
        None if all.iter().any(|t| t.format.protocol.contains("encrypted")) => Err(DRM_REASON.into()),
        None => Err("no supported stream for this track".into()),
    }
}

fn resolve(api: &Api, track: &Track) -> Result<String, ApiError> {
    let t = pick(track).map_err(ApiError::Other)?;
    let auth = track.track_authorization.as_deref().unwrap_or_default();
    let s: StreamUrl = api.get(&format!("{}?track_authorization={auth}", t.url))?;
    Ok(s.url)
}

/// Downloads segments in order on its own thread, at most 2 ahead.
fn spawn_fetcher(agent: Agent, urls: Vec<String>, cancel: Arc<AtomicBool>) -> Receiver<Result<Vec<u8>, String>> {
    let (tx, rx): (SyncSender<_>, _) = sync_channel(2);
    thread::spawn(move || {
        for url in urls {
            let mut result = Err(String::new());
            for attempt in 0..4u32 {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                match agent.get_bytes(&url, 32 * 1024 * 1024) {
                    Ok(b) => {
                        result = Ok(b);
                        break;
                    }
                    Err(e) => {
                        result = Err(format!("stream interrupted ({e})"));
                        thread::sleep(StdDuration::from_millis(500 * 2u64.pow(attempt)));
                    }
                }
            }
            let failed = result.is_err();
            if tx.send(result).is_err() || failed {
                return;
            }
        }
    });
    rx
}

struct Current {
    data: Vec<u8>,
    samples: Vec<Sample>,
    next: usize,
}

pub struct HlsSource {
    agent: Agent,
    track_id: u64,
    playlist: MediaPlaylist,
    init: InitInfo,
    decoder: AacDecoder,
    rate: u32,
    rx: Receiver<Result<Vec<u8>, String>>,
    cancel: Arc<AtomicBool>,
    current: Option<Current>,
    /// Frames still to drop (encoder priming at the start, or seek offset).
    skip: u64,
    start: f64,
    decoded: u64,
    scratch: Vec<f32>,
}

impl HlsSource {
    fn open(agent: Agent, track_id: u64, playlist_url: &str, start: f64) -> Result<Self, String> {
        let text = agent.get_text(playlist_url).map_err(|e| e.to_string())?;
        let playlist = hls::parse(&text).map_err(|e| match e {
            HlsError::Encrypted => DRM_REASON.to_string(),
            HlsError::Invalid => "unexpected stream format".to_string(),
        })?;
        let init_url = playlist.init_url.clone().ok_or("unexpected stream format (no init segment)")?;
        let init_bytes = agent.get_bytes(&init_url, 1024 * 1024).map_err(|e| e.to_string())?;
        let init = fmp4::parse_init(&init_bytes).ok_or("unexpected stream format (init segment)")?;
        let mut params = AudioCodecParameters::new();
        params.for_codec(CODEC_ID_AAC).with_extra_data(init.asc.clone().into_boxed_slice());
        let decoder = AacDecoder::try_new(&params, &AudioDecoderOptions::default())
            .map_err(|e| format!("unsupported audio format ({e})"))?;
        let rate = decoder.codec_params().sample_rate.unwrap_or(init.timescale);
        let cancel = Arc::new(AtomicBool::new(false));
        let mut s = Self {
            rx: spawn_fetcher(agent.clone(), Vec::new(), cancel.clone()),
            agent,
            track_id,
            playlist,
            init,
            decoder,
            rate,
            cancel,
            current: None,
            skip: 0,
            start: 0.0,
            decoded: 0,
            scratch: Vec::new(),
        };
        s.seek(start)?;
        Ok(s)
    }
}

impl Drop for HlsSource {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Source for HlsSource {
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
        self.playlist.duration()
    }

    fn seek(&mut self, to: f64) -> Result<(), String> {
        let to = to.clamp(0.0, (self.duration() - 0.5).max(0.0));
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        let first = self.playlist.segment_at(to);
        let urls = self.playlist.segments[first..].iter().map(|s| s.url.clone()).collect();
        self.rx = spawn_fetcher(self.agent.clone(), urls, self.cancel.clone());
        self.current = None;
        self.decoder.reset();
        let seg_start = self.playlist.segments[first].start;
        let into = ((to - seg_start).max(0.0) * f64::from(self.rate)) as u64;
        self.skip = into + if first == 0 { self.init.priming } else { 0 };
        self.start = to;
        self.decoded = 0;
        Ok(())
    }

    fn decode_next(&mut self, out: &mut Vec<f32>) -> Result<Step, String> {
        if self.current.as_ref().is_none_or(|c| c.next >= c.samples.len()) {
            match self.rx.recv_timeout(StdDuration::from_millis(100)) {
                Ok(Ok(data)) => {
                    let frag = fmp4::parse_fragment(&data, &self.init).ok_or("corrupt audio segment")?;
                    self.current = Some(Current { data, samples: frag.samples, next: 0 });
                }
                Ok(Err(e)) => return Err(e),
                Err(RecvTimeoutError::Timeout) => return Ok(Step::Waiting),
                Err(RecvTimeoutError::Disconnected) => return Ok(Step::Ended),
            }
        }
        let cur = self.current.as_mut().expect("segment loaded above");
        let s = cur.samples[cur.next];
        cur.next += 1;
        let packet = PacketRef::new(
            0,
            Timestamp::new(0),
            Duration::new(u64::from(s.duration)),
            &cur.data[s.offset..s.offset + s.size],
        );
        let channels = match self.decoder.decode_ref(&packet) {
            Ok(buf) => {
                let ch = buf.spec().channels().count();
                buf.copy_to_vec_interleaved(&mut self.scratch);
                ch
            }
            // A single bad frame: skip it, as players do.
            Err(SymError::DecodeError(_)) => return Ok(Step::Produced),
            Err(e) => return Err(format!("audio decode failed ({e})")),
        };
        append_stereo(&self.scratch, channels, &mut self.skip, &mut self.decoded, out);
        Ok(Step::Produced)
    }
}

/// Appends `samples` (interleaved, `channels` wide) as stereo, dropping the
/// first `skip` frames.
pub(super) fn append_stereo(samples: &[f32], channels: usize, skip: &mut u64, decoded: &mut u64, out: &mut Vec<f32>) {
    if channels == 0 {
        return;
    }
    let frames = samples.len() / channels;
    let drop = (*skip).min(frames as u64) as usize;
    *skip -= drop as u64;
    for f in samples[drop * channels..frames * channels].chunks_exact(channels) {
        let (l, r) = if channels == 1 { (f[0], f[0]) } else { (f[0], f[1]) };
        out.push(l);
        out.push(r);
    }
    *decoded += (frames - drop) as u64;
}

pub struct Mp3Source {
    agent: Agent,
    track_id: u64,
    url: String,
    duration: f64,
    reader: MpaReader<'static>,
    decoder: MpaDecoder,
    rate: u32,
    start: f64,
    decoded: u64,
    skip: u64,
    scratch: Vec<f32>,
}

impl Mp3Source {
    fn open(agent: Agent, track_id: u64, url: String, duration: f64, start: f64) -> Result<Self, String> {
        let (reader, decoder, rate) = Self::connect(&agent, &url, start)?;
        Ok(Self { agent, track_id, url, duration, reader, decoder, rate, start, decoded: 0, skip: 0, scratch: Vec::new() })
    }

    /// Opens the stream at (approximately) `start` seconds with a byte-range
    /// request: SoundCloud's MP3s are 128 kbps CBR (16000 bytes/s), and MP3
    /// frames resynchronise by themselves.
    fn connect(agent: &Agent, url: &str, start: f64) -> Result<(MpaReader<'static>, MpaDecoder, u32), String> {
        let from = (start.max(0.0) * 16_000.0) as u64;
        let (body, _) = agent.get_reader(url, from).map_err(|e| e.to_string())?;
        let mss = MediaSourceStream::new(Box::new(ReadOnlySource::new(body)), Default::default());
        let reader = MpaReader::try_new(mss, FormatOptions::default()).map_err(|e| format!("unexpected stream format ({e})"))?;
        let params = match reader.tracks().first().and_then(|t| t.codec_params.clone()) {
            Some(CodecParameters::Audio(p)) => p,
            _ => return Err("unexpected stream format".into()),
        };
        let decoder = MpaDecoder::try_new(&params, &AudioDecoderOptions::default())
            .map_err(|e| format!("unsupported audio format ({e})"))?;
        let rate = params.sample_rate.unwrap_or(44_100);
        Ok((reader, decoder, rate))
    }
}

impl Source for Mp3Source {
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
        let (reader, decoder, rate) = Self::connect(&self.agent, &self.url, to)?;
        (self.reader, self.decoder, self.rate) = (reader, decoder, rate);
        self.start = to;
        self.decoded = 0;
        Ok(())
    }

    fn decode_next(&mut self, out: &mut Vec<f32>) -> Result<Step, String> {
        let packet = match self.reader.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => return Ok(Step::Ended),
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(Step::Ended),
            Err(e) => return Err(format!("stream interrupted ({e})")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::models::{Page, TrackLike};

    /// Live check against the signed-in account: decode a few seconds of an
    /// HLS/AAC track and of the MP3-only one (if any), plus a seek.
    /// Run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "hits the network and needs a saved login"]
    fn decodes_real_streams() {
        let api = Api::new(crate::config::Config::load().client_id);
        api.set_token(crate::auth::load_token().unwrap());
        let me = api.me().unwrap();
        let likes: Page<TrackLike> = api.get(&Api::likes_url(me.id)).unwrap();
        let tracks: Vec<Track> = likes.collection.into_iter().filter_map(|l| l.track).collect();
        let has = |t: &Track, proto: &str| {
            t.media.as_ref().is_some_and(|m| m.transcodings.iter().any(|x| x.format.protocol == proto))
        };
        let aac = tracks.iter().find(|t| has(t, "hls") && t.availability() == Availability::Playable).unwrap();
        let mp3 = tracks.iter().find(|t| has(t, "progressive"));
        for (label, track) in [("aac", Some(aac)), ("mp3", mp3)] {
            let Some(track) = track else { continue };
            let item = QueueItem { id: track.id, track: Some(Arc::new(track.clone())) };
            let mut src = open(&api, &item, 0.0).unwrap_or_else(|e| panic!("{label}: {e}"));
            let mut out = Vec::new();
            while out.len() < (src.rate() * 2 * 3) as usize {
                if let Step::Ended = src.decode_next(&mut out).unwrap() {
                    break;
                }
            }
            let peak = out.iter().fold(0f32, |m, s| m.max(s.abs()));
            println!("{label}: {} Hz, {:.2}s decoded, peak {peak:.3}, duration {:.1}s", src.rate(), src.position(), src.duration());
            assert!(peak > 0.01, "{label}: decoded audio is silent");
            src.seek(30.0).unwrap();
            out.clear();
            while out.is_empty() {
                src.decode_next(&mut out).unwrap();
            }
            println!("{label}: after seek position {:.2}s", src.position());
            assert!((src.position() - 30.0).abs() < 1.0);
        }
    }
}
