pub mod client_id;
pub mod login;

use std::process::{Command, Stdio};

const SERVICE: &str = "Cumulus";
const ACCOUNT: &str = "soundcloud-oauth-token";

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| format!("credential store unavailable: {e}"))
}

/// The saved OAuth token, if any.
pub fn load_token() -> Result<Option<String>, String> {
    let from_keyring = entry().and_then(|e| match e.get_password() {
        Ok(t) if !t.trim().is_empty() => Ok(Some(t)),
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("could not read saved login: {e}")),
    });
    match from_keyring {
        Ok(Some(t)) => Ok(Some(t)),
        // No keyring here (or nothing in it): the private-file fallback.
        Ok(None) | Err(_) if cfg!(unix) => Ok(crate::secret_file::get(ACCOUNT)),
        other => other,
    }
}

pub fn save_token(token: &str) -> Result<(), String> {
    match entry().and_then(|e| e.set_password(token.trim()).map_err(|e| format!("could not save login: {e}"))) {
        Ok(()) => Ok(()),
        Err(_) if cfg!(unix) => crate::secret_file::set(ACCOUNT, Some(token.trim())),
        Err(e) => Err(e),
    }
}

/// Forget the token and the sign-in window's browser profile (site cookies).
pub fn sign_out() {
    if let Ok(e) = entry() {
        let _ = e.delete_credential();
    }
    let _ = crate::secret_file::set(ACCOUNT, None);
    let _ = std::fs::remove_dir_all(crate::paths::webview_dir());
}

/// Accepts either a bare token or the "OAuth 2-xxxx" header form someone
/// might copy out of browser dev tools.
pub fn normalize_pasted_token(s: &str) -> Option<String> {
    let t = s.trim().trim_start_matches("OAuth ").trim().trim_matches('"');
    (t.len() >= 16 && !t.contains(char::is_whitespace)).then(|| t.to_string())
}

/// Runs the sign-in window as a child process and blocks until it closes.
/// Returns Ok(true) if a token was captured and saved.
pub fn run_login_process() -> Result<bool, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .arg(login::LOGIN_ARG)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not open sign-in window: {e}"))?;
    // Hold the child's stdin open while it runs. If this process dies, the
    // pipe closes and the sign-in window shuts itself down (see login.rs).
    // `wait` would close stdin first, so keep our own handle.
    let _lifeline = child.stdin.take();
    let status = child.wait().map_err(|e| e.to_string())?;
    match status.code() {
        Some(0) => Ok(true),
        Some(login::EXIT_CANCELLED) => Ok(false),
        _ => Err("the sign-in window failed to start (is the WebView2 runtime installed?)".into()),
    }
}
