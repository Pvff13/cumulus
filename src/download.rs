//! Saving files that artists offer for download on SoundCloud. Only tracks
//! the artist marked downloadable are fetched, from SoundCloud's own download
//! link; streams are never saved.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use crate::api::Api;
use crate::api::models::Track;

/// Downloads the artist's file into the Downloads folder; returns where it went.
pub fn save(api: &Api, track: &Track) -> Result<PathBuf, String> {
    let link = api.download_link(track.id).map_err(|e| e.to_string())?;
    let (mut body, server_name) = api.agent().get_file(&link).map_err(|e| e.to_string())?;

    let dir = dirs::download_dir().unwrap_or_else(crate::paths::data_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("can't open {}: {e}", dir.display()))?;
    let part = dir.join(format!("cumulus-{}.part", track.id));
    let result = (|| {
        let mut out = BufWriter::new(File::create(&part)?);
        let mut head = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = body.read(&mut buf)?;
            if n == 0 {
                break;
            }
            if head.len() < 16 {
                head.extend_from_slice(&buf[..n.min(16 - head.len())]);
            }
            out.write_all(&buf[..n])?;
        }
        out.flush()?;
        Ok::<_, std::io::Error>(head)
    })();
    let head = match result {
        Ok(head) => head,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(format!("download failed: {e}"));
        }
    };

    let ext = server_name
        .as_deref()
        .and_then(|n| Path::new(n).extension())
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .or_else(|| sniff(&head).map(str::to_string))
        .unwrap_or_else(|| "mp3".to_string());
    let stem = sanitize(&format!("{} - {}", track.user.username, track.title));
    let path = unique(&dir, &stem, &ext);
    std::fs::rename(&part, &path).map_err(|e| format!("can't save {}: {e}", path.display()))?;
    Ok(path)
}

/// Audio type from the first bytes, when the server doesn't name the file.
fn sniff(head: &[u8]) -> Option<&'static str> {
    Some(match head {
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'A', b'V', b'E', ..] => "wav",
        [b'f', b'L', b'a', b'C', ..] => "flac",
        [b'F', b'O', b'R', b'M', _, _, _, _, b'A', b'I', b'F', ..] => "aiff",
        [b'O', b'g', b'g', b'S', ..] => "ogg",
        [_, _, _, _, b'f', b't', b'y', b'p', ..] => "m4a",
        [b'I', b'D', b'3', ..] => "mp3",
        [0xFF, b, ..] if b & 0xE0 == 0xE0 => "mp3",
        _ => return None,
    })
}

/// Makes a title safe as a Windows file name.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']);
    let short: String = trimmed.chars().take(150).collect();
    if short.is_empty() { "SoundCloud download".to_string() } else { short }
}

/// `dir/stem.ext`, or `dir/stem (2).ext` and so on if that's taken.
fn unique(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let mut path = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem} ({n}).{ext}"));
        n += 1;
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names() {
        assert_eq!(sanitize("DJ / X - What? <Remix>."), "DJ _ X - What_ _Remix_");
        assert_eq!(sanitize("  ...  "), "SoundCloud download");
        assert_eq!(sniff(b"RIFF\0\0\0\0WAVEfmt "), Some("wav"));
        assert_eq!(sniff(b"ID3\x04"), Some("mp3"));
        assert_eq!(sniff(b"\0\0\0\x20ftypM4A "), Some("m4a"));
        assert_eq!(sniff(b"hello"), None);
    }
}
