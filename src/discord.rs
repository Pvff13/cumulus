//! "Listening to …" on Discord, through the Discord desktop app's local
//! RPC pipe. Only runs when switched on; nothing leaves the PC except
//! through the user's own Discord app.

use std::io::{Read, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Cumulus's application on Discord's developer portal (public, not a
/// secret: it names the app in "Listening to …").
pub const APP_ID: &str = "1562382731055341628";

/// What to show; None clears the status.
#[derive(Clone, Debug, PartialEq)]
pub struct Activity {
    pub title: String,
    pub artist: String,
    pub artwork: Option<String>,
    pub url: String,
    /// Unix ms when the track (at this speed) started and will end.
    pub start_ms: u64,
    pub end_ms: u64,
}

impl Activity {
    /// For a track `position` s into `duration` s, playing at `speed`.
    pub fn new(title: &str, artist: &str, artwork: Option<String>, url: &str, position: f64, duration: f64, speed: f32) -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let speed = f64::from(speed.max(0.1));
        let start_ms = now.saturating_sub((position / speed * 1000.0) as u64);
        let end_ms = start_ms + (duration / speed * 1000.0) as u64;
        Self { title: title.to_string(), artist: artist.to_string(), artwork, url: url.to_string(), start_ms, end_ms }
    }

    fn to_json(&self) -> Value {
        // Discord wants 2-128 characters in these fields.
        let field = |s: &str| {
            let s: String = s.chars().take(120).collect();
            if s.chars().count() < 2 { format!("{s}  ") } else { s }
        };
        let mut activity = json!({
            "type": 2, // "Listening to"
            "details": field(&self.title),
            "state": field(&format!("by {}", self.artist)),
            "timestamps": { "start": self.start_ms, "end": self.end_ms },
            "assets": { "large_text": field(&self.title) },
        });
        if let Some(art) = &self.artwork {
            activity["assets"]["large_image"] = json!(art);
        }
        if self.url.starts_with("https://") {
            activity["buttons"] = json!([{ "label": "Listen on SoundCloud", "url": self.url }]);
        }
        activity
    }
}

/// Talks to Discord on a background thread. Send `Some(activity)` to show
/// it, `None` to clear it; reconnects when Discord (re)starts.
pub fn start(app_id: String) -> Sender<Option<Activity>> {
    let (tx, rx) = channel();
    thread::Builder::new()
        .name("discord".into())
        .spawn(move || run(&app_id, rx))
        .expect("spawn discord thread");
    tx
}

fn run(app_id: &str, rx: Receiver<Option<Activity>>) {
    let mut pipe: Option<Pipe> = None;
    let mut wanted: Option<Activity> = None;
    let mut dirty = false;
    loop {
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(a) => {
                // Only the newest state matters.
                wanted = rx.try_iter().last().unwrap_or(a);
                dirty = true;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                if let Some(p) = &mut pipe {
                    let _ = p.set_activity(None);
                }
                return;
            }
        }
        if pipe.is_none() && wanted.is_some() {
            pipe = Pipe::connect(app_id).ok();
            dirty = true;
        }
        if dirty {
            if let Some(p) = &mut pipe {
                let result = p.set_activity(wanted.as_ref());
                #[cfg(debug_assertions)]
                eprintln!("discord: {} -> {result:?}", wanted.as_ref().map_or("clear", |a| a.title.as_str()));
                if result.is_err() {
                    pipe = None; // Discord closed; retry later
                }
            }
            dirty = pipe.is_none() && wanted.is_some();
        }
    }
}

struct Pipe {
    file: std::fs::File,
    nonce: u64,
}

impl Pipe {
    fn connect(app_id: &str) -> Result<Self, String> {
        let mut last = String::from("Discord isn't running");
        for i in 0..10 {
            match open_ipc(i) {
                Ok(file) => {
                    let mut p = Self { file, nonce: 0 };
                    p.send(0, &json!({ "v": 1, "client_id": app_id }))?;
                    let (op, reply) = p.recv()?;
                    if op == 1 && reply["evt"] == "READY" {
                        return Ok(p);
                    }
                    // Discord answered but said no (e.g. unknown app id).
                    return Err(reply["message"].as_str().unwrap_or("handshake failed").to_string());
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn set_activity(&mut self, activity: Option<&Activity>) -> Result<(), String> {
        self.nonce += 1;
        let args = json!({ "pid": std::process::id(), "activity": activity.map(Activity::to_json) });
        self.send(1, &json!({ "cmd": "SET_ACTIVITY", "args": args, "nonce": self.nonce.to_string() }))?;
        let (_, reply) = self.recv()?;
        match reply["evt"].as_str() {
            Some("ERROR") => Err(reply["data"]["message"].as_str().unwrap_or("error").to_string()),
            _ => Ok(()),
        }
    }

    /// One frame: opcode and length (little-endian u32s), then JSON.
    fn send(&mut self, op: u32, body: &Value) -> Result<(), String> {
        let body = body.to_string();
        let mut frame = Vec::with_capacity(8 + body.len());
        frame.extend_from_slice(&op.to_le_bytes());
        frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
        frame.extend_from_slice(body.as_bytes());
        self.file.write_all(&frame).map_err(|e| e.to_string())
    }

    fn recv(&mut self) -> Result<(u32, Value), String> {
        let mut head = [0u8; 8];
        self.file.read_exact(&mut head).map_err(|e| e.to_string())?;
        let op = u32::from_le_bytes(head[..4].try_into().unwrap());
        let len = u32::from_le_bytes(head[4..].try_into().unwrap()) as usize;
        if len > 1 << 20 {
            return Err("oversized frame".into());
        }
        let mut body = vec![0u8; len];
        self.file.read_exact(&mut body).map_err(|e| e.to_string())?;
        Ok((op, serde_json::from_slice(&body).unwrap_or(Value::Null)))
    }
}

#[cfg(windows)]
fn open_ipc(i: u32) -> Result<std::fs::File, String> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(format!(r"\\.\pipe\discord-ipc-{i}"))
        .map_err(|e| e.to_string())
}

#[cfg(not(windows))]
fn open_ipc(_i: u32) -> Result<std::fs::File, String> {
    Err("Discord status is Windows-only for now".into())
}

/// Debug helper: tries a handshake with `app_id` and reports what Discord said.
#[cfg(debug_assertions)]
pub fn probe(app_id: &str) -> String {
    match Pipe::connect(app_id) {
        Ok(_) => "connected: READY".to_string(),
        Err(e) => format!("Discord replied: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_json() {
        let a = Activity::new("t", "artist", Some("https://i1.sndcdn.com/a.jpg".into()), "https://soundcloud.com/x/y", 30.0, 120.0, 1.0);
        assert_eq!(a.end_ms - a.start_ms, 120_000);
        let j = a.to_json();
        assert_eq!(j["type"], 2);
        assert_eq!(j["details"], "t  "); // padded to Discord's 2-char minimum
        assert_eq!(j["state"], "by artist");
        assert_eq!(j["buttons"][0]["url"], "https://soundcloud.com/x/y");
        // At 1.25× a 120 s track takes 96 s.
        let fast = Activity::new("tt", "a", None, "", 0.0, 120.0, 1.25);
        assert_eq!(fast.end_ms - fast.start_ms, 96_000);
        assert!(fast.to_json().get("buttons").is_none());
    }
}
