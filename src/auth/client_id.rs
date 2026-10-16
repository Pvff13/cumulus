//! The website's public `client_id`. SoundCloud embeds it in one of the JS
//! bundles linked from the homepage and rotates it occasionally, so we
//! rediscover it whenever API calls start failing with 401.

use crate::api::client::{Agent, ApiError};

const HOME: &str = "https://soundcloud.com/";
const ASSET_PREFIX: &str = "https://a-v2.sndcdn.com/assets/";

pub fn discover(agent: &Agent) -> Result<String, ApiError> {
    let html = agent.get_text(HOME)?;
    let scripts = script_urls(&html);
    // The client_id has always lived in one of the last bundles; check those first.
    for url in scripts.iter().rev() {
        if let Ok(js) = agent.get_text(url) {
            if let Some(id) = find_client_id(&js) {
                return Ok(id);
            }
        }
    }
    Err(ApiError::Other(
        "could not find SoundCloud's web client_id (the website layout may have changed)".into(),
    ))
}

fn script_urls(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find(ASSET_PREFIX) {
        let tail = &rest[i..];
        let end = tail.find(['"', '\'']).unwrap_or(tail.len());
        let url = &tail[..end];
        if url.ends_with(".js") && !out.iter().any(|u| u == url) {
            out.push(url.to_string());
        }
        rest = &tail[end..];
    }
    out
}

fn find_client_id(js: &str) -> Option<String> {
    for pat in ["client_id:\"", "client_id=\"", "client_id=", "\"client_id\":\""] {
        let mut rest = js;
        while let Some(i) = rest.find(pat) {
            let cand: String = rest[i + pat.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if cand.len() == 32 {
                return Some(cand);
            }
            rest = &rest[i + pat.len()..];
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_scripts_and_id() {
        let html = r#"<script crossorigin src="https://a-v2.sndcdn.com/assets/0-abc.js"></script>
            <script src="https://a-v2.sndcdn.com/assets/55-7f9.js"></script>
            <link href="https://a-v2.sndcdn.com/assets/x.css">"#;
        assert_eq!(
            script_urls(html),
            ["https://a-v2.sndcdn.com/assets/0-abc.js", "https://a-v2.sndcdn.com/assets/55-7f9.js"]
        );
        let js = r#"foo({client_id:"V7f0p2q7JD4voO1Esqg6bwwtNYcYAtLC",env:"production"})"#;
        assert_eq!(find_client_id(js).as_deref(), Some("V7f0p2q7JD4voO1Esqg6bwwtNYcYAtLC"));
        assert_eq!(find_client_id("client_id:\"short\""), None);
    }
}
