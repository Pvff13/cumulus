//! Minimal HLS media-playlist parser: the init segment (`EXT-X-MAP`) and the
//! list of media segments with their durations. Encrypted playlists
//! (`EXT-X-KEY` with a method other than NONE) are rejected, because those
//! are DRM-protected and not ours to play.

#[derive(Debug, Clone)]
pub struct Segment {
    pub url: String,
    /// Seconds.
    pub duration: f64,
    /// Start time within the track, seconds.
    pub start: f64,
}

#[derive(Debug, Clone)]
pub struct MediaPlaylist {
    pub init_url: Option<String>,
    pub segments: Vec<Segment>,
}

impl MediaPlaylist {
    pub fn duration(&self) -> f64 {
        self.segments.last().map_or(0.0, |s| s.start + s.duration)
    }

    /// Index of the segment containing `t` seconds.
    pub fn segment_at(&self, t: f64) -> usize {
        self.segments
            .iter()
            .rposition(|s| s.start <= t)
            .unwrap_or(0)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum HlsError {
    Encrypted,
    Invalid,
}

pub fn parse(text: &str) -> Result<MediaPlaylist, HlsError> {
    if !text.trim_start().starts_with("#EXTM3U") {
        return Err(HlsError::Invalid);
    }
    let mut init_url = None;
    let mut segments = Vec::new();
    let mut pending: Option<f64> = None;
    let mut start = 0.0;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            if !rest.contains("METHOD=NONE") {
                return Err(HlsError::Encrypted);
            }
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            init_url = attr(rest, "URI");
        } else if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let secs = rest.split(',').next().unwrap_or("0").trim();
            pending = Some(secs.parse().map_err(|_| HlsError::Invalid)?);
        } else if !line.starts_with('#') {
            let duration = pending.take().ok_or(HlsError::Invalid)?;
            segments.push(Segment { url: line.to_string(), duration, start });
            start += duration;
        }
    }
    if segments.is_empty() {
        return Err(HlsError::Invalid);
    }
    Ok(MediaPlaylist { init_url, segments })
}

fn attr(list: &str, name: &str) -> Option<String> {
    let key = format!("{name}=\"");
    let i = list.find(&key)? + key.len();
    let end = list[i..].find('"')?;
    Some(list[i..i + end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:10\n#EXT-X-PLAYLIST-TYPE:VOD\n\
        #EXT-X-MAP:URI=\"https://cdn/init.mp4?expires=1&Policy=a,b\"\n\
        #EXTINF:10.0078,\nhttps://cdn/data000.m4s?x=1\n#EXTINF:9.98458,\nhttps://cdn/data001.m4s?x=1\n\
        #EXTINF:4.5,\nhttps://cdn/data002.m4s?x=1\n#EXT-X-ENDLIST\n";

    #[test]
    fn parses_fmp4_playlist() {
        let p = parse(SAMPLE).unwrap();
        assert_eq!(p.init_url.as_deref(), Some("https://cdn/init.mp4?expires=1&Policy=a,b"));
        assert_eq!(p.segments.len(), 3);
        assert!((p.segments[2].start - 19.99238).abs() < 1e-6);
        assert!((p.duration() - 24.49238).abs() < 1e-6);
        assert_eq!(p.segment_at(0.0), 0);
        assert_eq!(p.segment_at(10.5), 1);
        assert_eq!(p.segment_at(99.0), 2);
    }

    #[test]
    fn rejects_encrypted() {
        let enc = SAMPLE.replace("#EXT-X-VERSION:7", "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\"");
        assert_eq!(parse(&enc).unwrap_err(), HlsError::Encrypted);
    }
}
