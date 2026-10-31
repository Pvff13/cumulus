//! Listen along: one Cumulus hosts, friends follow what it plays. The two
//! talk through a small relay (server/listen-along, a Cloudflare Worker):
//! the host sends the track id, its position and whether it's playing, and
//! the relay passes that on. No audio goes through the relay; each person
//! streams from SoundCloud themselves. If the host allows it, listeners can
//! also ask for songs to be queued; the host's Cumulus answers each request.

use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// What the host is doing, as sent to listeners.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct State {
    /// None: nothing playing.
    pub track: Option<u64>,
    /// Seconds into the track when this was sent.
    pub position: f64,
    pub playing: bool,
    /// Listeners may ask for songs.
    pub requests: bool,
}

/// How the host answered a song request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Added,
    /// Waiting for the host to approve it.
    Waiting,
    Declined,
    /// That person already has as many songs waiting as the host allows.
    Limit,
    /// The host doesn't take requests (now).
    Closed,
    /// It can't play (DRM, or not found).
    Unavailable,
}

impl Answer {
    fn key(self) -> &'static str {
        match self {
            Answer::Added => "added",
            Answer::Waiting => "waiting",
            Answer::Declined => "declined",
            Answer::Limit => "limit",
            Answer::Closed => "closed",
            Answer::Unavailable => "unavailable",
        }
    }

    fn from_key(s: &str) -> Option<Self> {
        [Answer::Added, Answer::Waiting, Answer::Declined, Answer::Limit, Answer::Closed, Answer::Unavailable]
            .into_iter()
            .find(|a| a.key() == s)
    }
}

/// From the UI to a session's thread.
enum Out {
    State(State),
    /// Listener: ask the host to queue a song.
    Request { track: u64, name: String },
    /// Host: answer someone's request.
    Reply { to: String, track: u64, answer: Answer },
    Stop,
}

/// What a session's background thread reports.
#[derive(Debug)]
pub enum Event {
    /// (Re)connected to the relay.
    Connected,
    /// Host: how many people are listening along.
    Listeners(usize),
    /// Listener: what the host is playing now (received at that instant).
    State(State, Instant),
    /// Host: a listener (`from`, a short id from the relay) asks for a song.
    Request { from: String, track: u64, name: String },
    /// Listener: the host's answer to your request.
    Reply { track: u64, answer: Answer },
    /// The session is over (the host ended it, or it timed out).
    Ended,
    /// Couldn't connect, or gave up reconnecting.
    Failed(String),
}

/// A new session: (room code, host key). Both are 128-bit random hex; the
/// room code is what friends need to join, the key never leaves this PC.
pub fn new_room() -> (String, String) {
    let hex = || {
        let mut b = [0u8; 16];
        getrandom::fill(&mut b).expect("random bytes");
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    };
    (hex(), hex())
}

/// Accepts a pasted code (spaces and case don't matter).
pub fn parse_code(s: &str) -> Option<String> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    let s = s.strip_prefix("cumulus:").unwrap_or(&s).to_string();
    (s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit())).then_some(s)
}

fn room_url(server: &str, room: &str, query: &str) -> String {
    format!("{}/v1/rooms/{room}?{query}", server.trim_end_matches('/'))
}

/// Hosting a session. Send states with `send`; dropping it ends the session.
pub struct Host {
    tx: Sender<Out>,
}

impl Host {
    pub fn start(server: &str, room: &str, key: &str, events: impl Fn(Event) + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let url = room_url(server, room, &format!("role=host&key={key}"));
        thread::Builder::new()
            .name("listen-host".into())
            .spawn(move || run(&url, Some(rx), &events))
            .expect("spawn listen thread");
        Self { tx }
    }

    pub fn send(&self, state: State) {
        let _ = self.tx.send(Out::State(state));
    }

    pub fn answer(&self, to: &str, track: u64, answer: Answer) {
        let _ = self.tx.send(Out::Reply { to: to.to_string(), track, answer });
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Tells the thread to end the session for everyone.
        let _ = self.tx.send(Out::Stop);
    }
}

/// Following someone's session. Dropping it leaves.
pub struct Listener {
    tx: Sender<Out>,
}

impl Listener {
    pub fn join(server: &str, room: &str, events: impl Fn(Event) + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let url = room_url(server, room, "role=listen");
        thread::Builder::new()
            .name("listen-follow".into())
            .spawn(move || run(&url, Some(rx), &events))
            .expect("spawn listen thread");
        Self { tx }
    }

    /// Asks the host to queue a song; `name` is shown to the host.
    pub fn request(&self, track: u64, name: &str) {
        let _ = self.tx.send(Out::Request { track, name: name.to_string() });
    }
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

/// How the connection ended.
enum Exit {
    /// We're done (left, or ended the session).
    Done,
    /// The relay said the session is over.
    Ended,
    /// Dropped: try again.
    Lost(String),
}

/// Connects, and reconnects for a while if the connection drops. `rx`
/// carries what to send; it closing, or `Out::Stop`, means stop.
fn run(url: &str, rx: Option<Receiver<Out>>, events: &dyn Fn(Event)) {
    let rx = rx.expect("control channel");
    let is_host = url.contains("role=host");
    let mut last: Option<State> = None;
    let mut failures = 0u32;
    loop {
        match connect(url) {
            Ok(mut ws) => {
                failures = 0;
                events(Event::Connected);
                // A reconnecting host re-sends where it is.
                if let Some(s) = last {
                    let _ = send_state(&mut ws, s);
                }
                match session(&mut ws, &rx, &mut last, is_host, events) {
                    Exit::Done => {
                        let _ = ws.close(None);
                        let _ = ws.flush();
                        return;
                    }
                    Exit::Ended => return events(Event::Ended),
                    Exit::Lost(why) => {
                        #[cfg(debug_assertions)]
                        eprintln!("listen: connection lost ({why}), retrying");
                        let _ = why;
                    }
                }
            }
            Err(Refused::Gone) => return events(Event::Ended),
            Err(Refused::Full) => return events(Event::Failed("that session is full".into())),
            Err(Refused::Other(e)) if failures == 0 && last.is_none() && !is_host => {
                return events(Event::Failed(e));
            }
            Err(Refused::Other(_)) => {}
        }
        failures += 1;
        // About two minutes of retries (the relay keeps a room for a minute
        // after its host drops).
        if failures > 12 {
            return events(Event::Failed("lost the connection to the listen-along server".into()));
        }
        // Wait, but stop at once if told to.
        let wait = Duration::from_secs(u64::from(failures.min(5)) * 2);
        let until = Instant::now() + wait;
        while Instant::now() < until {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Out::State(s)) => last = Some(s),
                // Requests and answers made while disconnected are dropped.
                Ok(Out::Request { .. } | Out::Reply { .. }) => {}
                Ok(Out::Stop) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

enum Refused {
    /// No such session (wrong code, or it ended).
    Gone,
    Full,
    Other(String),
}

fn connect(url: &str) -> Result<Socket, Refused> {
    match tungstenite::connect(url) {
        Ok((ws, _)) => {
            // Short reads, so the loop can also send and notice "stop".
            let timeout = Some(Duration::from_millis(200));
            match ws.get_ref() {
                MaybeTlsStream::Plain(s) => {
                    let _ = s.set_read_timeout(timeout);
                }
                MaybeTlsStream::NativeTls(s) => {
                    let _ = s.get_ref().set_read_timeout(timeout);
                }
                _ => {}
            }
            Ok(ws)
        }
        Err(tungstenite::Error::Http(resp)) => match resp.status().as_u16() {
            404 => Err(Refused::Gone),
            409 => Err(Refused::Full),
            code => Err(Refused::Other(format!("the listen-along server said {code}"))),
        },
        Err(e) => Err(Refused::Other(format!("couldn't reach the listen-along server ({e})"))),
    }
}

fn send_state(ws: &mut Socket, s: State) -> tungstenite::Result<()> {
    let m = json!({
        "type": "state",
        "track": s.track,
        "position": (s.position * 10.0).round() / 10.0,
        "playing": s.playing,
        "requests": s.requests,
    });
    ws.send(Message::text(m.to_string()))
}

fn session(
    ws: &mut Socket,
    rx: &Receiver<Out>,
    last: &mut Option<State>,
    is_host: bool,
    events: &dyn Fn(Event),
) -> Exit {
    let mut pinged = Instant::now();
    loop {
        // Outgoing: the newest state only, and every request / answer.
        let mut newest = None;
        loop {
            let message = match rx.try_recv() {
                Ok(Out::State(s)) => {
                    newest = Some(s);
                    continue;
                }
                Ok(Out::Request { track, name }) => json!({ "type": "request", "track": track, "name": name }),
                Ok(Out::Reply { to, track, answer }) => json!({ "type": "reply", "to": to, "track": track, "status": answer.key() }),
                Ok(Out::Stop) | Err(TryRecvError::Disconnected) => return Exit::Done,
                Err(TryRecvError::Empty) => break,
            };
            if let Err(e) = ws.send(Message::text(message.to_string())) {
                return Exit::Lost(e.to_string());
            }
        }
        if let Some(s) = newest {
            *last = Some(s);
            if is_host && let Err(e) = send_state(ws, s) {
                return Exit::Lost(e.to_string());
            }
        }
        // Keep idle connections alive (Cloudflare drops ones silent for
        // about 100 s), without waking the relay more than needed.
        if pinged.elapsed() > Duration::from_secs(60) {
            pinged = Instant::now();
            if ws.send(Message::Ping(Vec::new().into())).is_err() {
                return Exit::Lost("ping failed".into());
            }
        }
        match ws.read() {
            Ok(Message::Text(text)) => {
                let Ok(m) = serde_json::from_str::<Value>(&text) else { continue };
                match m["type"].as_str() {
                    Some("listeners") => events(Event::Listeners(m["count"].as_u64().unwrap_or(0) as usize)),
                    Some("ended") => return Exit::Ended,
                    Some("state") if !is_host => {
                        let playing = m["playing"].as_bool().unwrap_or(false);
                        // A late joiner's first state can be a few seconds old.
                        let age = if playing { m["age"].as_f64().unwrap_or(0.0).clamp(0.0, 60_000.0) / 1000.0 } else { 0.0 };
                        let s = State {
                            track: m["track"].as_u64(),
                            position: m["position"].as_f64().unwrap_or(0.0) + age,
                            playing,
                            requests: m["requests"].as_bool().unwrap_or(false),
                        };
                        *last = Some(s);
                        events(Event::State(s, Instant::now()));
                    }
                    Some("request") if is_host => {
                        if let (Some(from), Some(track)) = (m["from"].as_str(), m["track"].as_u64()) {
                            let name = m["name"].as_str().unwrap_or("A listener").to_string();
                            events(Event::Request { from: from.to_string(), track, name });
                        }
                    }
                    Some("reply") if !is_host => {
                        if let (Some(track), Some(answer)) = (m["track"].as_u64(), m["status"].as_str().and_then(Answer::from_key)) {
                            events(Event::Reply { track, answer });
                        }
                    }
                    _ => {}
                }
            }
            Ok(Message::Close(frame)) => {
                // 1000 = the host ended it; anything else, try again.
                return match frame {
                    Some(f) if u16::from(f.code) == 1000 => Exit::Ended,
                    Some(f) if u16::from(f.code) == 4000 => Exit::Done,
                    _ => Exit::Lost("closed".into()),
                };
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Exit::Lost(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        let (room, key) = new_room();
        assert_eq!(room.len(), 32);
        assert_ne!(room, key);
        assert_eq!(parse_code(&format!("  {} ", room.to_uppercase())), Some(room.clone()));
        assert_eq!(parse_code(&format!("cumulus:{room}")), Some(room));
        assert_eq!(parse_code("not a code"), None);
    }
}
