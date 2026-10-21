use std::fmt;
use std::sync::Mutex;
use std::thread::sleep;
use std::time::Duration;

use serde::de::DeserializeOwned;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

use crate::auth::client_id;
use crate::config::Config;

const API_BASE: &str = "https://api-v2.soundcloud.com";
// The web API is the website's own backend; present as an ordinary browser.
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
const MAX_ATTEMPTS: u32 = 4;

#[derive(Debug, Clone)]
pub enum ApiError {
    /// No connection / DNS / timeout / TLS failure.
    Offline(String),
    /// The saved token was rejected even with a fresh client_id.
    Unauthorized,
    /// Still rate limited after backing off.
    RateLimited,
    /// 403: usually geo-blocked or not available to this account.
    Forbidden,
    NotFound,
    Http(u16),
    Other(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Offline(m) => write!(f, "can't reach SoundCloud ({m})"),
            Self::Unauthorized => write!(f, "your SoundCloud login has expired; please sign in again"),
            Self::RateLimited => write!(f, "SoundCloud is rate limiting requests; try again in a bit"),
            Self::Forbidden => write!(f, "not available (blocked in your region or for your account)"),
            Self::NotFound => write!(f, "not found"),
            Self::Http(c) => write!(f, "SoundCloud returned HTTP {c}"),
            Self::Other(m) => write!(f, "{m}"),
        }
    }
}

/// Thin wrapper over a ureq agent using the OS TLS stack.
#[derive(Clone)]
pub struct Agent(ureq::Agent);

impl Agent {
    pub fn new() -> Self {
        let tls = TlsConfig::builder()
            .provider(TlsProvider::NativeTls)
            .root_certs(RootCerts::PlatformVerifier)
            .build();
        let config = ureq::Agent::config_builder()
            .tls_config(tls)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .user_agent(USER_AGENT)
            .build();
        Self(config.into())
    }

    /// One GET returning (status, retry-after seconds, body).
    fn fetch(&self, url: &str, oauth: Option<&str>) -> Result<(u16, Option<u64>, String), ApiError> {
        fn headers<B>(req: ureq::RequestBuilder<B>, oauth: Option<&str>) -> ureq::RequestBuilder<B> {
            let req = req
                .header("Accept", "application/json, text/javascript, */*; q=0.1")
                .header("Origin", "https://soundcloud.com")
                .header("Referer", "https://soundcloud.com/");
            match oauth {
                Some(t) => req.header("Authorization", format!("OAuth {t}")),
                None => req,
            }
        }
        let mut resp = headers(self.0.get(url), oauth).call().map_err(|e| ApiError::Offline(e.to_string()))?;
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse().ok());
        let body = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| ApiError::Offline(e.to_string()))?;
        Ok((status, retry_after, body))
    }

    /// GET a binary resource from the CDN (artwork). Capped at `limit` bytes.
    pub fn get_bytes(&self, url: &str, limit: u64) -> Result<Vec<u8>, ApiError> {
        let mut resp = self.0.get(url).call().map_err(|e| ApiError::Offline(e.to_string()))?;
        match resp.status().as_u16() {
            200..=299 => resp
                .body_mut()
                .with_config()
                .limit(limit)
                .read_to_vec()
                .map_err(|e| ApiError::Offline(e.to_string())),
            404 => Err(ApiError::NotFound),
            code => Err(ApiError::Http(code)),
        }
    }

    /// Streaming GET (progressive audio), optionally from a byte offset.
    /// Returns the body reader and the total resource length if known.
    pub fn get_reader(
        &self,
        url: &str,
        from: u64,
    ) -> Result<(ureq::BodyReader<'static>, Option<u64>), ApiError> {
        let mut req = self.0.get(url);
        if from > 0 {
            req = req.header("Range", format!("bytes={from}-"));
        }
        let resp = req.call().map_err(|e| ApiError::Offline(e.to_string()))?;
        let header = |name: &str| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        let total = match resp.status().as_u16() {
            200 => header("content-length").and_then(|v| v.parse().ok()),
            // "bytes 1000-1999/123456"
            206 => header("content-range").and_then(|v| v.rsplit('/').next()?.parse().ok()),
            403 => return Err(ApiError::Forbidden),
            404 => return Err(ApiError::NotFound),
            code => return Err(ApiError::Http(code)),
        };
        Ok((resp.into_body().into_reader(), total))
    }

    /// Streaming GET of a whole file with no overall time limit (artist
    /// downloads can be large WAVs). Also returns the server's file name.
    pub fn get_file(&self, url: &str) -> Result<(ureq::BodyReader<'static>, Option<String>), ApiError> {
        let resp = self
            .0
            .get(url)
            .config()
            .timeout_global(None)
            .build()
            .call()
            .map_err(|e| ApiError::Offline(e.to_string()))?;
        match resp.status().as_u16() {
            200..=299 => {}
            403 => return Err(ApiError::Forbidden),
            404 => return Err(ApiError::NotFound),
            code => return Err(ApiError::Http(code)),
        }
        let name = resp
            .headers()
            .get("content-disposition")
            .and_then(|v| v.to_str().ok())
            .and_then(disposition_filename);
        Ok((resp.into_body().into_reader(), name))
    }

    /// POST a form; returns (status, body). For scrobbling.
    pub fn post_form(&self, url: &str, form: &[(&str, String)]) -> Result<(u16, String), ApiError> {
        let pairs: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let mut resp = self.0.post(url).send_form(pairs).map_err(|e| ApiError::Offline(e.to_string()))?;
        let status = resp.status().as_u16();
        Ok((status, resp.body_mut().read_to_string().unwrap_or_default()))
    }

    /// POST JSON with one extra header; returns (status, body).
    pub fn post_json(&self, url: &str, header: &str, value: &str, body: &serde_json::Value) -> Result<(u16, String), ApiError> {
        let mut resp = self.0.post(url).header(header, value).send_json(body).map_err(|e| ApiError::Offline(e.to_string()))?;
        let status = resp.status().as_u16();
        Ok((status, resp.body_mut().read_to_string().unwrap_or_default()))
    }

    /// GET with one extra header; returns (status, body).
    pub fn get_with_header(&self, url: &str, header: &str, value: &str) -> Result<(u16, String), ApiError> {
        let mut resp = self.0.get(url).header(header, value).call().map_err(|e| ApiError::Offline(e.to_string()))?;
        let status = resp.status().as_u16();
        Ok((status, resp.body_mut().read_to_string().unwrap_or_default()))
    }

    /// Where `url` ends up after redirects (short on.soundcloud.com links).
    pub fn final_url(&self, url: &str) -> Result<String, ApiError> {
        use ureq::ResponseExt;
        let resp = self.0.get(url).call().map_err(|e| ApiError::Offline(e.to_string()))?;
        Ok(resp.get_uri().to_string())
    }

    /// GET a public page/script (used for client_id discovery).
    pub fn get_text(&self, url: &str) -> Result<String, ApiError> {
        match self.fetch(url, None)? {
            (200..=299, _, body) => Ok(body),
            (code, _, _) => Err(ApiError::Http(code)),
        }
    }
}

pub struct Api {
    agent: Agent,
    client_id: Mutex<Option<String>>,
    token: Mutex<Option<String>>,
}

impl Api {
    pub fn new(cached_client_id: Option<String>) -> Self {
        Self {
            agent: Agent::new(),
            client_id: Mutex::new(cached_client_id),
            token: Mutex::new(None),
        }
    }

    /// The underlying HTTP agent (shared connection pool), for CDN downloads.
    pub fn agent(&self) -> Agent {
        self.agent.clone()
    }

    pub fn set_token(&self, token: Option<String>) {
        *self.token.lock().unwrap() = token;
    }

    fn client_id(&self) -> Result<String, ApiError> {
        if let Some(id) = self.client_id.lock().unwrap().clone() {
            return Ok(id);
        }
        self.refresh_client_id()
    }

    fn refresh_client_id(&self) -> Result<String, ApiError> {
        let id = client_id::discover(&self.agent)?;
        *self.client_id.lock().unwrap() = Some(id.clone());
        let mut cfg = Config::load();
        cfg.client_id = Some(id.clone());
        cfg.save();
        Ok(id)
    }

    /// GET an API path (`/me`) or a full `next_href` URL and decode JSON.
    pub fn get<T: DeserializeOwned>(&self, path_or_url: &str) -> Result<T, ApiError> {
        parse(&self.get_raw(path_or_url)?)
    }

    /// Like `get`, but returns the response body unparsed (for the metadata cache).
    /// Handles client_id rotation, rate limiting and transient server errors.
    pub fn get_raw(&self, path_or_url: &str) -> Result<String, ApiError> {
        let mut client_id = self.client_id()?;
        let mut refreshed_id = false;
        for attempt in 0..MAX_ATTEMPTS {
            let url = with_client_id(path_or_url, &client_id);
            let token = self.token.lock().unwrap().clone();
            let (status, retry_after, body) = self.agent.fetch(&url, token.as_deref())?;
            match status {
                200..=299 => return Ok(body),
                // A rotated client_id and an expired token both show up as a bare 401.
                401 if !refreshed_id => {
                    client_id = self.refresh_client_id()?;
                    refreshed_id = true;
                }
                401 => return Err(ApiError::Unauthorized),
                403 => return Err(ApiError::Forbidden),
                404 => return Err(ApiError::NotFound),
                429 => {
                    if attempt + 1 == MAX_ATTEMPTS {
                        return Err(ApiError::RateLimited);
                    }
                    let backoff = 2u64.pow(attempt + 1);
                    sleep(Duration::from_secs(retry_after.unwrap_or(backoff).min(60)));
                }
                500..=599 if attempt + 1 < MAX_ATTEMPTS => {
                    sleep(Duration::from_secs(2u64.pow(attempt)));
                }
                code => return Err(ApiError::Http(code)),
            }
        }
        Err(ApiError::Unauthorized)
    }
}

pub fn parse<T: DeserializeOwned>(body: &str) -> Result<T, ApiError> {
    serde_json::from_str(body).map_err(|e| ApiError::Other(format!("unexpected response from SoundCloud: {e}")))
}

/// Builds the full URL and sets `client_id`, replacing any stale one
/// (pagination links sometimes carry their own).
fn with_client_id(path_or_url: &str, client_id: &str) -> String {
    let base = if path_or_url.starts_with("http") {
        path_or_url.to_string()
    } else {
        format!("{API_BASE}{path_or_url}")
    };
    let (head, query) = match base.split_once('?') {
        Some((h, q)) => (h.to_string(), q.to_string()),
        None => (base, String::new()),
    };
    let mut params: Vec<&str> = query
        .split('&')
        .filter(|p| !p.is_empty() && !p.starts_with("client_id="))
        .collect();
    let cid = format!("client_id={client_id}");
    params.push(&cid);
    format!("{head}?{}", params.join("&"))
}

#[cfg(test)]
mod tests {
    use super::{Api, with_client_id};

    /// Live check: a stale client_id is replaced via rediscovery and the call succeeds.
    /// Run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "hits the network"]
    fn recovers_from_rotated_client_id() {
        let api = Api::new(Some("0123456789abcdefghijklmnopqrstuv".into()));
        let v: serde_json::Value = api.get("/search/tracks?q=lofi&limit=1").unwrap();
        assert!(v["collection"].as_array().is_some_and(|c| !c.is_empty()));
        assert_ne!(api.client_id().unwrap(), "0123456789abcdefghijklmnopqrstuv");
    }

    #[test]
    fn client_id_param() {
        assert_eq!(with_client_id("/me", "X"), "https://api-v2.soundcloud.com/me?client_id=X");
        assert_eq!(
            with_client_id("https://api-v2.soundcloud.com/a?offset=3&client_id=OLD&limit=2", "X"),
            "https://api-v2.soundcloud.com/a?offset=3&limit=2&client_id=X"
        );
    }
}

/// `attachment; filename="Song.wav"` -> `Song.wav`.
fn disposition_filename(v: &str) -> Option<String> {
    let rest = v.split(';').map(str::trim).find_map(|p| p.strip_prefix("filename="))?;
    let name = rest.trim_matches('"').trim();
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
mod disposition_tests {
    #[test]
    fn filename() {
        assert_eq!(super::disposition_filename(r#"attachment; filename="My Song.wav""#).as_deref(), Some("My Song.wav"));
        assert_eq!(super::disposition_filename("attachment;filename=a.mp3").as_deref(), Some("a.mp3"));
        assert_eq!(super::disposition_filename("inline"), None);
    }
}
