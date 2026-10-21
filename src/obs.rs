//! Now-playing output for streaming: a text file OBS's Text source can read,
//! and a small local web page for a Browser source. Both are off unless
//! switched on; the web page only listens on this PC (127.0.0.1).

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::Serialize;

/// What the overlay page shows.
#[derive(Clone, Default, Serialize, PartialEq)]
pub struct Now {
    pub title: String,
    pub artist: String,
    pub artwork: Option<String>,
    pub playing: bool,
    pub position: f64,
    pub duration: f64,
    /// Accent colour ("#8b6cff") so the card matches the app.
    pub accent: String,
}

/// The browser overlay's server.
pub struct Overlay {
    now: Arc<Mutex<Now>>,
    pub port: u16,
}

impl Overlay {
    pub fn start(port: u16) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("port {port} is busy ({e})"))?;
        let now = Arc::new(Mutex::new(Now::default()));
        let shared = now.clone();
        thread::Builder::new()
            .name("obs-overlay".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let now = shared.clone();
                    thread::spawn(move || {
                        let _ = serve(stream, &now);
                    });
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { now, port })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    pub fn set(&self, now: Now) {
        if let Ok(mut n) = self.now.lock() {
            *n = now;
        }
    }
}

fn serve(stream: TcpStream, now: &Mutex<Now>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    let path = request.split_whitespace().nth(1).unwrap_or("/");
    let (status, kind, body) = match path {
        "/" | "/index.html" => ("200 OK", "text/html; charset=utf-8", PAGE.to_string()),
        p if p.starts_with("/now.json") => {
            let n = now.lock().map(|n| n.clone()).unwrap_or_default();
            ("200 OK", "application/json; charset=utf-8", serde_json::to_string(&n).unwrap_or_default())
        }
        _ => ("404 Not Found", "text/plain", "not found".to_string()),
    };
    let mut out = stream;
    write!(
        out,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    out.flush()
}

/// Fills `{title}`, `{artist}` in the user's text-file format.
pub fn format_text(format: &str, title: &str, artist: &str) -> String {
    format.replace("{title}", title).replace("{artist}", artist)
}

/// Writes the text file only when its content changes.
#[derive(Default)]
pub struct TextFile {
    last: Option<String>,
}

impl TextFile {
    pub fn write(&mut self, path: &str, text: &str) -> Result<(), String> {
        if self.last.as_deref() == Some(text) {
            return Ok(());
        }
        std::fs::write(path, text).map_err(|e| e.to_string())?;
        self.last = Some(text.to_string());
        Ok(())
    }
}

/// Where the text file goes unless the user picks somewhere else.
pub fn default_text_path() -> String {
    crate::paths::data_dir().join("now-playing.txt").display().to_string()
}

const PAGE: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Cumulus - Now playing</title>
<style>
  html, body { margin: 0; background: transparent; font-family: "Segoe UI", system-ui, sans-serif; overflow: hidden; }
  #card { display: flex; align-items: center; gap: 14px; padding: 12px 16px 16px; margin: 8px;
          background: rgba(18, 18, 21, .82); border-radius: 12px; color: #f2f2f5; position: relative;
          width: max-content; max-width: calc(100vw - 48px); transition: opacity .4s; }
  #card.hidden { opacity: 0; }
  #art { width: 64px; height: 64px; border-radius: 8px; object-fit: cover; background: #2a2a30; flex: none; }
  #text { min-width: 0; }
  #title { font-weight: 700; font-size: 20px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  #artist { font-size: 16px; opacity: .75; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  #bar { position: absolute; left: 16px; right: 16px; bottom: 7px; height: 3px; border-radius: 2px; background: rgba(255,255,255,.15); }
  #fill { height: 100%; width: 0; border-radius: 2px; background: #8b6cff; }
</style></head>
<body>
<div id="card" class="hidden"><img id="art" alt=""><div id="text"><div id="title"></div><div id="artist"></div></div>
<div id="bar"><div id="fill"></div></div></div>
<script>
  // Polls Cumulus on this PC once a second. ?hidepaused=1 hides the card while paused.
  const hidePaused = new URLSearchParams(location.search).get("hidepaused") === "1";
  async function tick() {
    try {
      const n = await (await fetch("now.json", { cache: "no-store" })).json();
      const card = document.getElementById("card");
      card.classList.toggle("hidden", !n.title || (hidePaused && !n.playing));
      document.getElementById("title").textContent = n.title;
      document.getElementById("artist").textContent = n.artist;
      const art = document.getElementById("art");
      if (n.artwork && art.src !== n.artwork) art.src = n.artwork;
      art.style.visibility = n.artwork ? "visible" : "hidden";
      const fill = document.getElementById("fill");
      fill.style.width = n.duration > 0 ? (100 * n.position / n.duration) + "%" : "0";
      if (n.accent) fill.style.background = n.accent;
    } catch (e) { /* Cumulus closed: keep the last card */ }
  }
  tick(); setInterval(tick, 1000);
</script></body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_format() {
        assert_eq!(format_text("{artist} - {title}", "Song", "Band"), "Band - Song");
        assert_eq!(format_text("♪ {title}", "Song", "Band"), "♪ Song");
    }

    #[test]
    fn serves_page_and_json() {
        use std::io::Read;
        // Find a free port, then serve on it.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let o = Overlay::start(port).unwrap();
        o.set(Now { title: "Song".into(), artist: "Band".into(), playing: true, ..Now::default() });
        let get = |path: &str| {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let mut body = String::new();
            s.read_to_string(&mut body).unwrap();
            body
        };
        assert!(get("/").contains("Now playing"));
        assert!(get("/now.json").contains(r#""title":"Song""#));
        assert!(get("/nope").starts_with("HTTP/1.1 404"));
    }
}
