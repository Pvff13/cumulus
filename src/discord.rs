//! "Listening to …" on Discord, through the Discord desktop app's local
//! RPC pipe. Only runs when switched on; nothing leaves the PC except
//! through the user's own Discord app.
//!
//! While you host a listen-along session the status also carries its code
//! as a "join secret", so friends can Ask to Join from your profile; their
//! Cumulus gets the code from their Discord.

use std::io::{Read, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Cumulus's application on Discord's developer portal (public, not a
/// secret: it names the app in "Listening to …").
pub const APP_ID: &str = "1562382731055341628";

/// A listen-along session friends can join from Discord.
#[derive(Clone, Debug, PartialEq)]
pub struct Join {
    /// The session code (only given to people Discord lets join).
    pub secret: String,
    /// A public id for the session (not the code).
    pub party: String,
    /// People in it, host included, and the most allowed.
    pub size: usize,
    pub max: usize,
}

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
    pub join: Option<Join>,
}

impl Activity {
    /// For a track `position` s into `duration` s, playing at `speed`.
    pub fn new(title: &str, artist: &str, artwork: Option<String>, url: &str, position: f64, duration: f64, speed: f32) -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let speed = f64::from(speed.max(0.1));
        let start_ms = now.saturating_sub((position / speed * 1000.0) as u64);
        let end_ms = start_ms + (duration / speed * 1000.0) as u64;
        Self { title: title.to_string(), artist: artist.to_string(), artwork, url: url.to_string(), start_ms, end_ms, join: None }
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
        if let Some(j) = &self.join {
            // Discord doesn't allow buttons together with joining.
            activity["party"] = json!({ "id": j.party, "size": [j.size.max(1), j.max] });
            activity["secrets"] = json!({ "join": j.secret });
        } else if self.url.starts_with("https://") {
            activity["buttons"] = json!([{ "label": "Listen on SoundCloud", "url": self.url }]);
        }
        activity
    }
}

/// From the UI to Discord.
pub enum Request {
    /// Show this status (None clears it).
    Activity(Option<Activity>),
    /// Answer someone's Ask to Join.
    Answer { user_id: String, accept: bool },
}

/// From Discord to the UI.
#[derive(Debug)]
pub enum Incoming {
    /// You clicked Join on a friend's status: here's their session code.
    Join(String),
    /// Someone asked to join your session.
    JoinRequest { user_id: String, name: String },
}

/// Talks to Discord on a background thread; reconnects when Discord
/// (re)starts. Events from Discord go to `incoming`.
pub fn start(app_id: String, incoming: impl Fn(Incoming) + Send + 'static) -> Sender<Request> {
    let (tx, rx) = channel();
    thread::Builder::new()
        .name("discord".into())
        .spawn(move || run(&app_id, rx, &incoming))
        .expect("spawn discord thread");
    tx
}

fn run(app_id: &str, rx: Receiver<Request>, incoming: &dyn Fn(Incoming)) {
    let mut pipe: Option<Pipe> = None;
    let mut wanted: Option<Activity> = None;
    let mut dirty = false;
    let mut next_try = Instant::now();
    loop {
        // Short waits, so events from Discord are picked up quickly.
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Request::Activity(a)) => {
                wanted = a;
                dirty = true;
            }
            Ok(Request::Answer { user_id, accept }) => {
                if let Some(p) = &mut pipe {
                    let cmd = if accept { "SEND_ACTIVITY_JOIN_INVITE" } else { "CLOSE_ACTIVITY_REQUEST" };
                    if p.request(cmd, json!({ "user_id": user_id }), None, incoming).is_err() {
                        pipe = None;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                if let Some(p) = &mut pipe {
                    let _ = p.set_activity(None, incoming);
                }
                return;
            }
        }
        // Stay connected even with nothing to show: a friend's Join can
        // arrive at any time.
        if pipe.is_none() && Instant::now() >= next_try {
            next_try = Instant::now() + Duration::from_secs(15);
            pipe = Pipe::connect(app_id).ok();
            dirty = true;
        }
        let Some(p) = &mut pipe else { continue };
        if dirty {
            let result = p.set_activity(wanted.as_ref(), incoming);
            #[cfg(debug_assertions)]
            eprintln!("discord: {} -> {result:?}", wanted.as_ref().map_or("clear", |a| a.title.as_str()));
            dirty = false;
            if result.is_err() {
                pipe = None; // Discord closed; retry later
                continue;
            }
        }
        if p.poll(incoming).is_err() {
            pipe = None;
        }
    }
}

struct Pipe {
    conn: Conn,
    nonce: u64,
}

impl Pipe {
    fn connect(app_id: &str) -> Result<Self, String> {
        let mut last = String::from("Discord isn't running");
        for i in 0..10 {
            match Conn::open(i) {
                Ok(conn) => {
                    let mut p = Self { conn, nonce: 0 };
                    p.send(0, &json!({ "v": 1, "client_id": app_id }))?;
                    let (op, reply) = p.recv()?;
                    if op == 1 && reply["evt"] == "READY" {
                        // Hear about joins (you clicked Join) and join requests.
                        for evt in ["ACTIVITY_JOIN", "ACTIVITY_JOIN_REQUEST"] {
                            let _ = p.request("SUBSCRIBE", json!({}), Some(evt), &|_| {});
                        }
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

    fn set_activity(&mut self, activity: Option<&Activity>, incoming: &dyn Fn(Incoming)) -> Result<(), String> {
        let args = json!({ "pid": std::process::id(), "activity": activity.map(Activity::to_json) });
        self.request("SET_ACTIVITY", args, None, incoming)
    }

    /// Sends a command and waits for its reply, handing on any events that
    /// arrive first.
    fn request(&mut self, cmd: &str, args: Value, evt: Option<&str>, incoming: &dyn Fn(Incoming)) -> Result<(), String> {
        self.nonce += 1;
        let nonce = self.nonce.to_string();
        let mut body = json!({ "cmd": cmd, "args": args, "nonce": nonce });
        if let Some(e) = evt {
            body["evt"] = json!(e);
        }
        self.send(1, &body)?;
        loop {
            let (op, reply) = self.recv()?;
            if op == 2 {
                return Err("Discord closed the connection".into());
            }
            if reply["nonce"].as_str() == Some(nonce.as_str()) {
                return match reply["evt"].as_str() {
                    Some("ERROR") => Err(reply["data"]["message"].as_str().unwrap_or("error").to_string()),
                    _ => Ok(()),
                };
            }
            dispatch(&reply, incoming);
        }
    }

    /// Reads any events waiting on the pipe.
    fn poll(&mut self, incoming: &dyn Fn(Incoming)) -> Result<(), String> {
        while self.conn.pending()? {
            let (op, msg) = self.recv()?;
            match op {
                1 => dispatch(&msg, incoming),
                // Discord pings now and then; answer so it keeps the connection.
                3 => self.send(4, &msg)?,
                2 => return Err("Discord closed the connection".into()),
                _ => {}
            }
        }
        Ok(())
    }

    /// One frame: opcode and length (little-endian u32s), then JSON.
    fn send(&mut self, op: u32, body: &Value) -> Result<(), String> {
        let body = body.to_string();
        let mut frame = Vec::with_capacity(8 + body.len());
        frame.extend_from_slice(&op.to_le_bytes());
        frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
        frame.extend_from_slice(body.as_bytes());
        self.conn.write_all(&frame).map_err(|e| e.to_string())
    }

    fn recv(&mut self) -> Result<(u32, Value), String> {
        let mut head = [0u8; 8];
        self.conn.read_exact(&mut head).map_err(|e| e.to_string())?;
        let op = u32::from_le_bytes(head[..4].try_into().unwrap());
        let len = u32::from_le_bytes(head[4..].try_into().unwrap()) as usize;
        if len > 1 << 20 {
            return Err("oversized frame".into());
        }
        let mut body = vec![0u8; len];
        self.conn.read_exact(&mut body).map_err(|e| e.to_string())?;
        Ok((op, serde_json::from_slice(&body).unwrap_or(Value::Null)))
    }
}

/// Passes a Discord event on to the UI.
fn dispatch(msg: &Value, incoming: &dyn Fn(Incoming)) {
    let data = &msg["data"];
    match msg["evt"].as_str() {
        Some("ACTIVITY_JOIN") => {
            if let Some(secret) = data["secret"].as_str() {
                incoming(Incoming::Join(secret.to_string()));
            }
        }
        Some("ACTIVITY_JOIN_REQUEST") => {
            let user = &data["user"];
            if let Some(id) = user["id"].as_str() {
                let name = user["global_name"].as_str().or(user["username"].as_str()).unwrap_or("Someone");
                incoming(Incoming::JoinRequest { user_id: id.to_string(), name: name.to_string() });
            }
        }
        _ => {}
    }
}

/// The pipe to the Discord app.
struct Conn {
    #[cfg(windows)]
    file: std::fs::File,
    #[cfg(unix)]
    sock: std::os::unix::net::UnixStream,
    /// Bytes read ahead while checking for pending data.
    #[cfg(unix)]
    ahead: Vec<u8>,
}

#[cfg(windows)]
impl Conn {
    fn open(i: u32) -> Result<Self, String> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!(r"\\.\pipe\discord-ipc-{i}"))
            .map_err(|e| e.to_string())?;
        Ok(Self { file })
    }

    /// Whether there's something to read (without blocking).
    fn pending(&mut self) -> Result<bool, String> {
        use std::os::windows::io::AsRawHandle;
        let mut avail = 0u32;
        // SAFETY: a valid pipe handle; only the byte count is written.
        let ok = unsafe {
            windows_sys::Win32::System::Pipes::PeekNamedPipe(
                self.file.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut avail,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 { Err("Discord closed the pipe".into()) } else { Ok(avail > 0) }
    }
}

/// Linux: a Unix socket in the runtime dir (or Flatpak/Snap Discord's).
#[cfg(unix)]
impl Conn {
    fn open(i: u32) -> Result<Self, String> {
        let base = std::env::var("XDG_RUNTIME_DIR").or_else(|_| std::env::var("TMPDIR")).unwrap_or_else(|_| "/tmp".into());
        let dirs = [base.clone(), format!("{base}/app/com.discordapp.Discord"), format!("{base}/snap.discord")];
        let mut last = String::from("Discord isn't running");
        for d in dirs {
            match std::os::unix::net::UnixStream::connect(format!("{d}/discord-ipc-{i}")) {
                Ok(sock) => return Ok(Self { sock, ahead: Vec::new() }),
                Err(e) => last = e.to_string(),
            }
        }
        Err(last)
    }

    /// Whether there's something to read: reads what's there without
    /// blocking and keeps it for the next `read`.
    fn pending(&mut self) -> Result<bool, String> {
        if !self.ahead.is_empty() {
            return Ok(true);
        }
        self.sock.set_nonblocking(true).map_err(|e| e.to_string())?;
        let mut b = [0u8; 4096];
        let r = self.sock.read(&mut b);
        self.sock.set_nonblocking(false).map_err(|e| e.to_string())?;
        match r {
            Ok(0) => Err("Discord closed the socket".into()),
            Ok(n) => {
                self.ahead.extend_from_slice(&b[..n]);
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        #[cfg(windows)]
        return self.file.read(buf);
        #[cfg(unix)]
        {
            if !self.ahead.is_empty() {
                let n = buf.len().min(self.ahead.len());
                buf[..n].copy_from_slice(&self.ahead[..n]);
                self.ahead.drain(..n);
                return Ok(n);
            }
            self.sock.read(buf)
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        #[cfg(windows)]
        return self.file.write(buf);
        #[cfg(unix)]
        return self.sock.write(buf);
    }

    fn flush(&mut self) -> std::io::Result<()> {
        #[cfg(windows)]
        return self.file.flush();
        #[cfg(unix)]
        return self.sock.flush();
    }
}

/// Debug helper: tries a handshake with `app_id` and reports what Discord said.
#[cfg(debug_assertions)]
pub fn probe(app_id: &str) -> String {
    match Pipe::connect(app_id) {
        Ok(mut p) => {
            for evt in ["ACTIVITY_JOIN", "ACTIVITY_JOIN_REQUEST"] {
                println!("subscribe {evt}: {:?}", p.request("SUBSCRIBE", json!({}), Some(evt), &|_| {}));
            }
            // Also check that waiting for events works (and changes nothing).
            for _ in 0..12 {
                if let Err(e) = p.poll(&|e| println!("event: {e:?}")) {
                    return format!("connected, then polling failed: {e}");
                }
                thread::sleep(Duration::from_millis(250));
            }
            "connected: READY, subscribed, polled 3 s".to_string()
        }
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

    #[test]
    fn joinable_activity_has_no_buttons() {
        let mut a = Activity::new("tt", "a", None, "https://soundcloud.com/x/y", 0.0, 60.0, 1.0);
        a.join = Some(Join { secret: "s".repeat(32), party: "p1".into(), size: 2, max: 21 });
        let j = a.to_json();
        assert!(j.get("buttons").is_none());
        assert_eq!(j["secrets"]["join"], "s".repeat(32));
        assert_eq!(j["party"]["size"], json!([2, 21]));
    }

    /// The whole conversation with a stand-in for the Discord app.
    #[cfg(unix)]
    #[test]
    fn talks_to_discord() {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("cumulus-fake-discord-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: the only test that reads this variable.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", &dir) };
        let listener = UnixListener::bind(dir.join("discord-ipc-0")).unwrap();
        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let read = |s: &mut std::os::unix::net::UnixStream| {
                let mut head = [0u8; 8];
                s.read_exact(&mut head).unwrap();
                let len = u32::from_le_bytes(head[4..].try_into().unwrap()) as usize;
                let mut body = vec![0u8; len];
                s.read_exact(&mut body).unwrap();
                (u32::from_le_bytes(head[..4].try_into().unwrap()), serde_json::from_slice::<Value>(&body).unwrap())
            };
            let write = |s: &mut std::os::unix::net::UnixStream, op: u32, v: Value| {
                let b = v.to_string();
                s.write_all(&op.to_le_bytes()).unwrap();
                s.write_all(&(b.len() as u32).to_le_bytes()).unwrap();
                s.write_all(b.as_bytes()).unwrap();
            };
            let (op, hello) = read(&mut s);
            assert_eq!((op, hello["client_id"].as_str()), (0, Some("123")));
            write(&mut s, 1, json!({ "cmd": "DISPATCH", "evt": "READY" }));
            let mut subscribed = Vec::new();
            for _ in 0..2 {
                let (_, m) = read(&mut s);
                subscribed.push(m["evt"].as_str().unwrap().to_string());
                write(&mut s, 1, json!({ "cmd": "SUBSCRIBE", "nonce": m["nonce"] }));
            }
            let (_, m) = read(&mut s);
            assert_eq!(m["cmd"], "SET_ACTIVITY");
            assert_eq!(m["args"]["activity"]["secrets"]["join"], "c".repeat(32));
            // An event arrives before the reply.
            write(&mut s, 1, json!({ "cmd": "DISPATCH", "evt": "ACTIVITY_JOIN_REQUEST", "data": { "user": { "id": "7", "global_name": "Pal" } } }));
            write(&mut s, 1, json!({ "cmd": "SET_ACTIVITY", "nonce": m["nonce"] }));
            // Then, while idle: a ping and a Join.
            write(&mut s, 3, json!({ "p": 1 }));
            write(&mut s, 1, json!({ "cmd": "DISPATCH", "evt": "ACTIVITY_JOIN", "data": { "secret": "abc" } }));
            let (op, _) = read(&mut s);
            (subscribed, op)
        });
        let mut p = Pipe::connect("123").unwrap();
        let got = std::cell::RefCell::new(Vec::new());
        let sink = |e: Incoming| got.borrow_mut().push(format!("{e:?}"));
        let mut a = Activity::new("tt", "a", None, "", 0.0, 60.0, 1.0);
        a.join = Some(Join { secret: "c".repeat(32), party: "p".into(), size: 1, max: 21 });
        p.set_activity(Some(&a), &sink).unwrap();
        for _ in 0..100 {
            p.poll(&sink).unwrap();
            if got.borrow().len() >= 2 {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let (subscribed, answered_ping) = server.join().unwrap();
        assert_eq!(subscribed, ["ACTIVITY_JOIN", "ACTIVITY_JOIN_REQUEST"]);
        assert_eq!(answered_ping, 4);
        assert_eq!(got.into_inner(), vec![r#"JoinRequest { user_id: "7", name: "Pal" }"#, r#"Join("abc")"#]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dispatches_join_events() {
        let got = std::cell::RefCell::new(Vec::new());
        let sink = |e: Incoming| got.borrow_mut().push(format!("{e:?}"));
        dispatch(&json!({ "cmd": "DISPATCH", "evt": "ACTIVITY_JOIN", "data": { "secret": "abc" } }), &sink);
        dispatch(
            &json!({ "cmd": "DISPATCH", "evt": "ACTIVITY_JOIN_REQUEST", "data": { "user": { "id": "42", "username": "pal" } } }),
            &sink,
        );
        dispatch(&json!({ "cmd": "DISPATCH", "evt": "READY" }), &sink);
        assert_eq!(got.into_inner(), vec![r#"Join("abc")"#, r#"JoinRequest { user_id: "42", name: "pal" }"#]);
    }
}
