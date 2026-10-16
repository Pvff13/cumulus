//! Just enough fragmented-MP4 (CMAF) parsing to get AAC frames out of
//! SoundCloud's HLS streams: the init segment gives the decoder config,
//! each media segment is `moof` (sample table) + `mdat` (sample data).

#[derive(Debug, Clone)]
pub struct InitInfo {
    /// MPEG-4 AudioSpecificConfig, passed to the AAC decoder as extra data.
    pub asc: Vec<u8>,
    pub timescale: u32,
    /// Encoder priming samples to drop from the start of the track (edit list).
    pub priming: u64,
    /// `trex` defaults, used when a fragment doesn't specify its own.
    default_duration: u32,
    default_size: u32,
}

/// One coded sample (AAC frame) inside a media segment.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub offset: usize,
    pub size: usize,
    pub duration: u32,
}

#[derive(Debug)]
pub struct Fragment {
    /// Decode time of the first sample, in timescale units (kept for
    /// diagnostics; playback positions come from the playlist timeline).
    #[allow(dead_code)]
    pub base_time: u64,
    pub samples: Vec<Sample>,
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn be64(b: &[u8], at: usize) -> Option<u64> {
    let hi = be32(b, at)? as u64;
    Some(hi << 32 | be32(b, at + 4)? as u64)
}

/// Iterates the boxes in `b[start..end]` as (type, box_start, payload_start, box_end).
fn boxes(b: &[u8], start: usize, end: usize) -> impl Iterator<Item = ([u8; 4], usize, usize, usize)> + '_ {
    let mut off = start;
    std::iter::from_fn(move || {
        if off + 8 > end {
            return None;
        }
        let size32 = be32(b, off)? as usize;
        let kind: [u8; 4] = b[off + 4..off + 8].try_into().ok()?;
        let (size, header) = match size32 {
            1 => (be64(b, off + 8)? as usize, 16),
            0 => (end - off, 8),
            n => (n, 8),
        };
        if size < header || off + size > end {
            return None;
        }
        let item = (kind, off, off + header, off + size);
        off += size;
        Some(item)
    })
}

fn find(b: &[u8], start: usize, end: usize, path: &[&[u8; 4]]) -> Option<(usize, usize, usize)> {
    let (first, rest) = path.split_first()?;
    let (_, bstart, payload, bend) = boxes(b, start, end).find(|(k, ..)| k == *first)?;
    if rest.is_empty() { Some((bstart, payload, bend)) } else { find(b, payload, bend, rest) }
}

pub fn parse_init(b: &[u8]) -> Option<InitInfo> {
    let (_, moov, moov_end) = find(b, 0, b.len(), &[b"moov"])?;
    let (_, trak, trak_end) = find(b, moov, moov_end, &[b"trak"])?;

    let (_, mdhd, _) = find(b, trak, trak_end, &[b"mdia", b"mdhd"])?;
    let timescale = if b[mdhd] == 1 { be32(b, mdhd + 20)? } else { be32(b, mdhd + 12)? };

    // stsd (full box) -> entry count -> mp4a sample entry (28 bytes of fields) -> esds
    let (_, stsd, stsd_end) = find(b, trak, trak_end, &[b"mdia", b"minf", b"stbl", b"stsd"])?;
    let (kind, _, mp4a, mp4a_end) = boxes(b, stsd + 8, stsd_end).next()?;
    if &kind != b"mp4a" {
        return None;
    }
    let (_, esds, esds_end) = find(b, mp4a + 28, mp4a_end, &[b"esds"])?;
    let asc = decoder_specific_info(&b[esds + 4..esds_end])?;

    // Edit list media_time = priming samples to skip.
    let priming = find(b, trak, trak_end, &[b"edts", b"elst"])
        .and_then(|(_, elst, _)| {
            let version = b[elst];
            let count = be32(b, elst + 4)?;
            if count == 0 {
                return None;
            }
            let t = if version == 1 { be64(b, elst + 16)? as i64 } else { be32(b, elst + 12)? as i32 as i64 };
            (t > 0).then_some(t as u64)
        })
        .unwrap_or(0);

    let (default_duration, default_size) = find(b, moov, moov_end, &[b"mvex", b"trex"])
        .and_then(|(_, trex, _)| Some((be32(b, trex + 12)?, be32(b, trex + 16)?)))
        .unwrap_or((1024, 0));

    Some(InitInfo { asc, timescale, priming, default_duration, default_size })
}

/// Walks the ES_Descriptor in an `esds` box to the DecoderSpecificInfo (tag 5).
fn decoder_specific_info(d: &[u8]) -> Option<Vec<u8>> {
    fn descriptor(d: &[u8], at: usize) -> Option<(u8, usize, usize)> {
        let tag = *d.get(at)?;
        let mut len = 0usize;
        let mut i = at + 1;
        for _ in 0..4 {
            let byte = *d.get(i)?;
            i += 1;
            len = len << 7 | (byte & 0x7f) as usize;
            if byte & 0x80 == 0 {
                break;
            }
        }
        Some((tag, i, i + len))
    }
    let (tag, mut p, _) = descriptor(d, 0)?;
    if tag != 0x03 {
        return None;
    }
    let flags = *d.get(p + 2)?;
    p += 3;
    if flags & 0x80 != 0 {
        p += 2;
    }
    if flags & 0x40 != 0 {
        p += 1 + *d.get(p)? as usize;
    }
    if flags & 0x20 != 0 {
        p += 2;
    }
    let (tag, p, _) = descriptor(d, p)?; // DecoderConfigDescriptor
    if tag != 0x04 {
        return None;
    }
    let (tag, start, end) = descriptor(d, p + 13)?;
    (tag == 0x05).then(|| d.get(start..end).map(<[u8]>::to_vec))?
}

/// Parses one media segment (`styp`/`sidx`/`moof`/`mdat`). Sample offsets are
/// absolute positions in `b`.
pub fn parse_fragment(b: &[u8], init: &InitInfo) -> Option<Fragment> {
    let (moof_start, moof, moof_end) = find(b, 0, b.len(), &[b"moof"])?;
    let (_, traf, traf_end) = find(b, moof, moof_end, &[b"traf"])?;
    let (_, tfhd, _) = find(b, traf, traf_end, &[b"tfhd"])?;

    let flags = be32(b, tfhd)? & 0x00ff_ffff;
    let mut p = tfhd + 8; // version/flags + track_ID
    let mut base_offset = moof_start as u64;
    if flags & 0x01 != 0 {
        base_offset = be64(b, p)?;
        p += 8;
    }
    if flags & 0x02 != 0 {
        p += 4;
    }
    let mut default_duration = init.default_duration;
    if flags & 0x08 != 0 {
        default_duration = be32(b, p)?;
        p += 4;
    }
    let mut default_size = init.default_size;
    if flags & 0x10 != 0 {
        default_size = be32(b, p)?;
    }

    let base_time = find(b, traf, traf_end, &[b"tfdt"])
        .and_then(|(_, tfdt, _)| if b[tfdt] == 1 { be64(b, tfdt + 4) } else { be32(b, tfdt + 4).map(u64::from) })
        .unwrap_or(0);

    let mut samples = Vec::new();
    for (kind, _, trun, _) in boxes(b, traf, traf_end) {
        if &kind != b"trun" {
            continue;
        }
        let tflags = be32(b, trun)? & 0x00ff_ffff;
        let count = be32(b, trun + 4)? as usize;
        let mut p = trun + 8;
        let mut offset = base_offset as usize;
        if tflags & 0x001 != 0 {
            offset = (base_offset as i64 + be32(b, p)? as i32 as i64) as usize;
            p += 4;
        }
        if tflags & 0x004 != 0 {
            p += 4;
        }
        for _ in 0..count {
            let mut duration = default_duration;
            let mut size = default_size;
            if tflags & 0x100 != 0 {
                duration = be32(b, p)?;
                p += 4;
            }
            if tflags & 0x200 != 0 {
                size = be32(b, p)?;
                p += 4;
            }
            if tflags & 0x400 != 0 {
                p += 4;
            }
            if tflags & 0x800 != 0 {
                p += 4;
            }
            if offset + size as usize > b.len() {
                return None;
            }
            samples.push(Sample { offset, size: size as usize, duration });
            offset += size as usize;
        }
    }
    Some(Fragment { base_time, samples })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real 761-byte init segment of a SoundCloud aac_160k stream.
    const INIT_HEX: &str = "0000001c6674797069736f350000020069736f3569736f366d703431000002dd6d6f6f760000006c6d76686400000000\n        0000000000000000000003e8000000000001000001000000000000000000000000010000000000000000000000000000\n        000100000000000000000000000000004000000000000000000000000000000000000000000000000000000000000002\n        000001e07472616b0000005c746b68640000000300000000000000000000000100000000000000000000000000000000\n        000000010100000000010000000000000000000000000000000100000000000000000000000000004000000000000000\n        0000000000000024656474730000001c656c73740000000000000001000000000000000000010000000001586d646961\n        000000206d6468640000000000000000000000000000ac440000000055c400000000002d68646c720000000000000000\n        736f756e000000000000000000000000536f756e6448616e646c657200000001036d696e6600000010736d6864000000\n        00000000000000002464696e660000001c6472656600000000000000010000000c75726c2000000001000000c7737462\n        6c0000007b7374736400000000000000010000006b6d7034610000000000000001000000000000000000020010000000\n        00ac44000000000033657364730000000003808080220001000480808014401500000000027100000271000580808002\n        121006808080010200000014627472740000000000027100000271000000001073747473000000000000000000000010\n        737473630000000000000000000000147374737a000000000000000000000000000000107374636f0000000000000000\n        000000286d76657800000020747265780000000000000001000000010000000000000000000000000000006175647461\n        000000596d657461000000000000002168646c7200000000000000006d6469726170706c000000000000000000000000\n        2c696c737400000024a9746f6f0000001c6461746100000001000000004c61766636312e372e313030";

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace() && *c != '\\').collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn esds_to_audio_specific_config() {
        let esds = hex("038080802200010004808080144015000000000271000002710005808080021210068080800102");
        assert_eq!(decoder_specific_info(&esds), Some(vec![0x12, 0x10]));
    }

    #[test]
    fn init_segment() {
        let init = parse_init(&hex(INIT_HEX)).expect("parses");
        assert_eq!(init.asc, vec![0x12, 0x10]); // AAC-LC, 44.1 kHz, stereo
        assert_eq!(init.timescale, 44100);
        assert_eq!(init.priming, 0); // SoundCloud's edit list has media_time 0
    }

    #[test]
    fn fragment_with_moof_relative_offsets() {
        // moof { mfhd, traf { tfhd(default-base-is-moof, default dur 1024), tfdt v1, trun(data offset + sizes) } } mdat
        let mut moof = Vec::new();
        let mut push_box = |out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]| {
            out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(body);
        };
        let mut traf = Vec::new();
        push_box(&mut traf, b"tfhd", &[0, 0x02, 0, 0x08, 0, 0, 0, 1, 0, 0, 4, 0]);
        push_box(&mut traf, b"tfdt", &[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10, 0]);
        // trun: flags 0x201, 2 samples, data offset (patched below), sizes 3 and 5
        let trun_body = [0, 0, 0x02, 0x01, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 5];
        push_box(&mut traf, b"trun", &trun_body);
        let mut moof_body = Vec::new();
        push_box(&mut moof_body, b"mfhd", &[0, 0, 0, 0, 0, 0, 0, 1]);
        push_box(&mut moof_body, b"traf", &traf);
        push_box(&mut moof, b"moof", &moof_body);
        let data_offset = (moof.len() + 8) as u32;
        let pos = moof.windows(4).position(|w| w == b"trun").unwrap() + 4 + 8;
        moof[pos..pos + 4].copy_from_slice(&data_offset.to_be_bytes());
        push_box(&mut moof, b"mdat", &[1, 2, 3, 4, 5, 6, 7, 8]);

        let init = InitInfo { asc: vec![], timescale: 44100, priming: 0, default_duration: 0, default_size: 0 };
        let f = parse_fragment(&moof, &init).unwrap();
        assert_eq!(f.base_time, 4096);
        assert_eq!(f.samples.len(), 2);
        assert_eq!(&moof[f.samples[0].offset..][..3], &[1, 2, 3]);
        assert_eq!(&moof[f.samples[1].offset..][..5], &[4, 5, 6, 7, 8]);
        assert_eq!(f.samples[1].duration, 1024);
    }
}
