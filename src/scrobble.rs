//! Scrobbling what you play to Last.fm and/or ListenBrainz. Off until the
//! user connects an account; logins live in the OS credential store.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};
use serde_json::{Value, json};

use crate::api::client::Agent;

/// Cumulus's Last.fm API account (https://www.last.fm/api/account/create).
/// Desktop apps ship these; Last.fm only uses them to name the app.
pub const LASTFM_KEY: &str = "";
pub const LASTFM_SECRET: &str = "";
const LASTFM_API: &str = "https://ws.audioscrobbler.com/2.0/";
const LISTENBRAINZ_API: &str = "https://api.listenbrainz.org/1";

/// A play to report.
#[derive(Clone, Debug, PartialEq)]
pub struct Play {
    pub artist: String,
    pub title: String,
    pub duration_secs: u64,
    /// Unix time it started.
    pub started: u64,
    pub url: String,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Last.fm/ListenBrainz want the real artist, but many SoundCloud uploads
/// are "Artist - Title" posted by a label or reposter: use that when present.
pub fn split_title(uploader: &str, title: &str) -> (String, String) {
    match title.split_once(" - ") {
        Some((a, t)) if !a.trim().is_empty() && !t.trim().is_empty() => (a.trim().to_string(), t.trim().to_string()),
        _ => (uploader.to_string(), title.trim().to_string()),
    }
}

/// Scrobble when at least half the track (or 4 minutes) has been heard, for
/// tracks over 30 seconds (Last.fm's rule; ListenBrainz uses the same).
pub fn heard_enough(heard_secs: f64, duration_secs: u64) -> bool {
    duration_secs > 30 && heard_secs >= (duration_secs as f64 / 2.0).min(240.0)
}

// ---- credentials (OS credential store) ----

fn entry(name: &str) -> Option<keyring::Entry> {
    keyring::Entry::new("Cumulus", name).ok()
}

pub fn load_secret(name: &str) -> Option<String> {
    entry(name)?.get_password().ok().filter(|s| !s.is_empty())
}

pub fn save_secret(name: &str, value: Option<&str>) {
    if let Some(e) = entry(name) {
        let _ = match value {
            Some(v) => e.set_password(v),
            None => e.delete_credential(),
        };
    }
}

// ---- Last.fm ----

/// Last.fm's request signature: md5 of the sorted params + the secret.
fn lastfm_sig(params: &[(&str, String)], secret: &str) -> String {
    let mut sorted: Vec<&(&str, String)> = params.iter().filter(|(k, _)| *k != "format").collect();
    sorted.sort_by_key(|(k, _)| *k);
    let mut s = String::new();
    for (k, v) in sorted {
        s.push_str(k);
        s.push_str(v);
    }
    s.push_str(secret);
    format!("{:x}", Md5::digest(s.as_bytes()))
}

fn lastfm_call(agent: &Agent, key: &str, secret: &str, mut params: Vec<(&str, String)>) -> Result<Value, String> {
    params.push(("api_key", key.to_string()));
    let sig = lastfm_sig(&params, secret);
    params.push(("api_sig", sig));
    params.push(("format", "json".into()));
    let (status, body) = agent.post_form(LASTFM_API, &params).map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&body).map_err(|_| format!("Last.fm returned HTTP {status}"))?;
    match v.get("message").and_then(Value::as_str) {
        Some(msg) if v.get("error").is_some() => Err(msg.to_string()),
        _ => Ok(v),
    }
}

/// Step 1 of connecting: a token to approve in the browser, and the page to open.
pub fn lastfm_start(agent: &Agent, key: &str, secret: &str) -> Result<(String, String), String> {
    let v = lastfm_call(agent, key, secret, vec![("method", "auth.getToken".into())])?;
    let token = v["token"].as_str().ok_or("no token from Last.fm")?.to_string();
    let url = format!("https://www.last.fm/api/auth/?api_key={key}&token={token}");
    Ok((token, url))
}

/// Step 2: after approving, swap the token for a session. Returns (session key, user name).
pub fn lastfm_finish(agent: &Agent, key: &str, secret: &str, token: &str) -> Result<(String, String), String> {
    let v = lastfm_call(agent, key, secret, vec![("method", "auth.getSession".into()), ("token", token.into())])?;
    let s = &v["session"];
    Ok((s["key"].as_str().ok_or("Last.fm didn't confirm it yet")?.to_string(), s["name"].as_str().unwrap_or("").to_string()))
}

// ---- ListenBrainz ----

/// Checks a ListenBrainz user token; returns the user name.
pub fn listenbrainz_check(agent: &Agent, token: &str) -> Result<String, String> {
    let (_, body) = agent
        .get_with_header(&format!("{LISTENBRAINZ_API}/validate-token"), "Authorization", &format!("Token {}", token.trim()))
        .map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    if v["valid"].as_bool() == Some(true) {
        Ok(v["user_name"].as_str().unwrap_or("").to_string())
    } else {
        Err("ListenBrainz didn't accept that token".into())
    }
}

fn listenbrainz_submit(agent: &Agent, token: &str, kind: &str, play: &Play) -> Result<(), String> {
    let mut listen = json!({
        "track_metadata": {
            "artist_name": play.artist,
            "track_name": play.title,
            "additional_info": {
                "duration_ms": play.duration_secs * 1000,
                "submission_client": "Cumulus",
                "submission_client_version": env!("CARGO_PKG_VERSION"),
                "music_service": "soundcloud.com",
                "origin_url": play.url,
            }
        }
    });
    if kind == "single" {
        listen["listened_at"] = json!(play.started);
    }
    if play.url.is_empty() {
        listen["track_metadata"]["additional_info"].as_object_mut().map(|m| {
            m.remove("music_service");
            m.remove("origin_url")
        });
    }
    let body = json!({ "listen_type": kind, "payload": [listen] });
    let (status, text) = agent
        .post_json(&format!("{LISTENBRAINZ_API}/submit-listens"), "Authorization", &format!("Token {token}"), &body)
        .map_err(|e| e.to_string())?;
    if (200..300).contains(&status) { Ok(()) } else { Err(format!("ListenBrainz: HTTP {status} {text}")) }
}

// ---- background sender ----

/// Who to send to (filled from the credential store).
#[derive(Clone, Default)]
pub struct Accounts {
    pub lastfm: Option<(String, String, String)>, // (key, secret, session)
    pub listenbrainz: Option<String>,
}

pub enum Job {
    NowPlaying(Play),
    Scrobble(Play),
    Accounts(Accounts),
}

/// Sends plays on a background thread, retrying failed scrobbles a few
/// times. Returns the channel to post jobs to.
pub fn start(agent: Agent) -> Sender<Job> {
    let (tx, rx) = channel();
    thread::Builder::new().name("scrobbler".into()).spawn(move || run(&agent, rx)).expect("spawn scrobbler");
    tx
}

fn run(agent: &Agent, rx: Receiver<Job>) {
    let mut accounts = Accounts::default();
    let mut retry: Vec<(Play, u32)> = Vec::new();
    loop {
        let job = match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(j) => Some(j),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(_) => return,
        };
        match job {
            Some(Job::Accounts(a)) => accounts = a,
            Some(Job::NowPlaying(p)) => send(agent, &accounts, &p, false),
            Some(Job::Scrobble(p)) => {
                if !send_ok(agent, &accounts, &p) {
                    retry.push((p, 0));
                }
            }
            None => {}
        }
        // Retry failed scrobbles (offline etc.) a few times.
        let pending = std::mem::take(&mut retry);
        for (p, tries) in pending {
            if !send_ok(agent, &accounts, &p) && tries < 5 {
                retry.push((p, tries + 1));
            }
        }
    }
}

fn send(agent: &Agent, accounts: &Accounts, play: &Play, scrobble: bool) {
    let _ = send_each(agent, accounts, play, scrobble);
}

fn send_ok(agent: &Agent, accounts: &Accounts, play: &Play) -> bool {
    send_each(agent, accounts, play, true).iter().all(Result::is_ok)
}

fn send_each(agent: &Agent, accounts: &Accounts, play: &Play, scrobble: bool) -> Vec<Result<(), String>> {
    let mut results = Vec::new();
    if let Some((key, secret, session)) = &accounts.lastfm {
        let mut params = vec![
            ("method", if scrobble { "track.scrobble" } else { "track.updateNowPlaying" }.to_string()),
            ("artist", play.artist.clone()),
            ("track", play.title.clone()),
            ("sk", session.clone()),
        ];
        if play.duration_secs > 0 {
            params.push(("duration", play.duration_secs.to_string()));
        }
        if scrobble {
            params.push(("timestamp", play.started.to_string()));
        }
        results.push(lastfm_call(agent, key, secret, params).map(|_| ()));
    }
    if let Some(token) = &accounts.listenbrainz {
        results.push(listenbrainz_submit(agent, token, if scrobble { "single" } else { "playing_now" }, play));
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_lastfm_docs_scheme() {
        // md5("api_keyKmethodauth.getTokenS") - sorted params, then the secret.
        let sig = lastfm_sig(&[("method", "auth.getToken".into()), ("api_key", "K".into()), ("format", "json".into())], "S");
        assert_eq!(sig, format!("{:x}", Md5::digest(b"api_keyKmethodauth.getTokenS")));
    }

    #[test]
    fn rules() {
        assert!(!heard_enough(100.0, 25)); // too short to count
        assert!(!heard_enough(59.0, 120));
        assert!(heard_enough(60.0, 120)); // half
        assert!(heard_enough(240.0, 3600)); // or 4 minutes
        assert_eq!(split_title("label", "Band - Song"), ("Band".into(), "Song".into()));
        assert_eq!(split_title("me", "just a song"), ("me".into(), "just a song".into()));
    }
}
