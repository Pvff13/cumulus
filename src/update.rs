//! Self-update, from two places:
//!
//! - **GitHub Releases** (everyone): the latest release's `cumulus.exe` is
//!   downloaded and only kept if its signature checks out (see `signing`).
//! - **The local build folder** (the maintainer's own copy): a newer
//!   `target/release/cumulus.exe` on this machine.
//!
//! Either way the swap is the same: Windows lets a running .exe be renamed,
//! so the old file is moved aside, the new one copied into place and
//! started, and the old file is deleted on the next start. Nothing is
//! installed until the user clicks the button.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;

use crate::api::client::Agent;

/// How often to ask GitHub for a new release.
const REMOTE_EVERY: Duration = Duration::from_secs(6 * 3600);
const MAX_EXE: u64 = 64 * 1024 * 1024;

pub const BUILD_ID_ARG: &str = "--build-id";

/// This build's number (seconds since 1970 when it was compiled).
pub fn build_id() -> u64 {
    env!("CUMULUS_BUILD_ID").parse().unwrap_or(0)
}

fn old_path(exe: &Path) -> PathBuf {
    exe.with_file_name("cumulus.old.exe")
}

/// Removes the previous version left behind by an update. Right after an
/// update the old process may still be closing, so keep trying for a bit.
pub fn cleanup() {
    // Downloaded updates that have been installed (or superseded).
    let _ = std::fs::remove_dir_all(crate::paths::data_dir().join("update"));
    let Ok(exe) = std::env::current_exe() else { return };
    let old = old_path(&exe);
    if !old.exists() {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..20 {
            if std::fs::remove_file(&old).is_ok() || !old.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
}

/// Where newer builds appear, unless that's the running copy itself (e.g.
/// running straight from the build folder). Only release builds update.
fn source() -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        return None;
    }
    let configured = crate::config::Config::load().update_source;
    // Public builds have no built-in source; `update_source` in config.json can still set one.
    let src = PathBuf::from(configured.or_else(|| option_env!("CUMULUS_UPDATE_SOURCE").map(str::to_string))?);
    let me = std::env::current_exe().ok()?.canonicalize().ok()?;
    let src_real = src.canonicalize().ok()?;
    (src_real != me).then_some(src_real)
}

/// An update that is ready to install.
#[derive(Debug, Clone)]
pub struct Ready {
    /// The new exe (a verified download, or the local build).
    pub exe: PathBuf,
    /// "0.2.1" for a release, None for a local build.
    pub version: Option<String>,
}

#[derive(Default)]
pub struct Checker {
    /// Local source timestamp, so the build is only asked for its number
    /// when the file actually changed.
    seen: Option<SystemTime>,
    last_remote: Option<Instant>,
    /// Releases that failed to download or verify; not retried this session.
    rejected: HashSet<String>,
}

/// Result of a check the user asked for.
pub enum Outcome {
    Ready(Ready),
    UpToDate,
    /// Couldn't check, or the new release failed verification.
    Problem(String),
}

impl Checker {
    /// Background check: a newer version, if there is one. The local build
    /// folder first, then GitHub (at most every few hours).
    pub fn check(&mut self, agent: &Agent) -> Option<Ready> {
        if let Some(exe) = self.newer_build() {
            return Some(Ready { exe, version: None });
        }
        if cfg!(debug_assertions) || self.last_remote.is_some_and(|t| t.elapsed() < REMOTE_EVERY) {
            return None;
        }
        match self.remote(agent) {
            Outcome::Ready(r) => Some(r),
            Outcome::UpToDate | Outcome::Problem(_) => None,
        }
    }

    /// "Check for updates" button: checks everything right now and says what it found.
    pub fn check_now(&mut self, agent: &Agent) -> Outcome {
        self.seen = None;
        if let Some(exe) = self.newer_build() {
            return Outcome::Ready(Ready { exe, version: None });
        }
        if cfg!(debug_assertions) {
            return Outcome::Problem("updates are off in debug builds".into());
        }
        self.remote(agent)
    }

    fn remote(&mut self, agent: &Agent) -> Outcome {
        self.last_remote = Some(Instant::now());
        match newer_release(agent, &self.rejected) {
            Ok(Some(r)) => Outcome::Ready(r),
            Ok(None) => Outcome::UpToDate,
            Err(Fail::Unreachable) => Outcome::Problem("couldn't reach GitHub; check your connection".into()),
            Err(Fail::Rejected { version, why }) => {
                eprintln!("cumulus update {version}: {why}");
                self.rejected.insert(version.clone());
                Outcome::Problem(format!("version {version} is out but couldn't be installed: {why}"))
            }
        }
    }

    /// The local source exe, if it holds a newer build than this one.
    fn newer_build(&mut self) -> Option<PathBuf> {
        let src = source()?;
        let modified = std::fs::metadata(&src).and_then(|m| m.modified()).ok()?;
        if self.seen == Some(modified) {
            return None;
        }
        self.seen = Some(modified);
        (read_build_id(&src)? > build_id()).then_some(src)
    }
}

/// Runs `<exe> --build-id`, which prints its number and exits without a window.
fn read_build_id(exe: &Path) -> Option<u64> {
    let mut cmd = Command::new(exe);
    cmd.arg(BUILD_ID_ARG).stdin(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// `https://github.com/owner/repo` (from Cargo.toml) -> the latest-release API URL.
fn latest_release_url() -> Option<String> {
    let repo = env!("CARGO_PKG_REPOSITORY").trim_end_matches('/');
    let path = repo.strip_prefix("https://github.com/")?;
    Some(format!("https://api.github.com/repos/{path}/releases/latest"))
}

/// "v0.10.2" -> (0, 10, 2); anything unparsable counts as 0.
fn parse_version(v: &str) -> (u64, u64, u64) {
    let mut parts = v.trim().trim_start_matches('v').split('.').map(|p| {
        p.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0)
    });
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

enum Fail {
    /// No connection, rate limited, unexpected answer: try again later.
    Unreachable,
    /// This release can't be used (missing files, bad signature): don't retry it.
    Rejected { version: String, why: String },
}

/// Downloads the latest release if it's newer, and keeps it only if the
/// signature verifies.
fn newer_release(agent: &Agent, rejected: &HashSet<String>) -> Result<Option<Ready>, Fail> {
    let Some(url) = latest_release_url() else { return Ok(None) };
    let body = agent.get_bytes(&url, 1024 * 1024).map_err(|_| Fail::Unreachable)?;
    let release = serde_json::from_slice::<Release>(&body).map_err(|_| Fail::Unreachable)?;
    let version = release.tag_name.trim_start_matches('v').to_string();
    if parse_version(&version) <= parse_version(current_version()) {
        return Ok(None);
    }
    let fail = |why: &str| Fail::Rejected { version: version.clone(), why: why.to_string() };
    if rejected.contains(&version) {
        return Err(fail("it failed verification earlier"));
    }
    let asset = |name: &str| release.assets.iter().find(|a| a.name == name).map(|a| a.browser_download_url.clone());
    let exe_url = asset("cumulus.exe").ok_or_else(|| fail("release has no cumulus.exe"))?;
    let sig_url = asset("cumulus.exe.sig").ok_or_else(|| fail("release has no signature"))?;
    let sig = agent.get_bytes(&sig_url, 4096).map_err(|e| fail(&format!("signature download failed ({e})")))?;
    let exe = agent.get_bytes(&exe_url, MAX_EXE).map_err(|e| fail(&format!("download failed ({e})")))?;
    crate::signing::verify(&version, &exe, &String::from_utf8_lossy(&sig)).map_err(|e| fail(&e))?;
    // Verified: only now does it touch the disk.
    let dir = crate::paths::data_dir().join("update");
    std::fs::create_dir_all(&dir).map_err(|e| fail(&e.to_string()))?;
    let path = dir.join(format!("cumulus-{version}.exe"));
    std::fs::write(&path, &exe).map_err(|e| fail(&e.to_string()))?;
    Ok(Some(Ready { exe: path, version: Some(version) }))
}

/// Swaps in the new exe and starts it. The caller then quits.
pub fn apply(new_exe: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = old_path(&exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| format!("couldn't move the running version aside ({e})"))?;
    if let Err(e) = std::fs::copy(new_exe, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("couldn't copy the new version ({e})"));
    }
    Command::new(&exe).spawn().map_err(|e| format!("couldn't start the new version ({e})"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(parse_version("v0.10.0") > parse_version("0.9.9"));
        assert!(parse_version("0.2.0") > parse_version("0.1.0"));
        assert_eq!(parse_version("v1.2.3-beta"), (1, 2, 3));
        assert_eq!(latest_release_url().as_deref(), Some("https://api.github.com/repos/Pvff13/cumulus/releases/latest"));
    }

    /// Live: what "Check for updates" asks GitHub. This build's version
    /// equals the latest release, so it must say "up to date" (not an error).
    #[test]
    #[ignore = "hits GitHub"]
    fn check_reports_up_to_date_for_latest() {
        match newer_release(&Agent::new(), &HashSet::new()) {
            Ok(None) => {}
            Ok(Some(r)) => panic!("unexpected newer release {:?}", r.version),
            Err(Fail::Unreachable) => panic!("GitHub unreachable"),
            Err(Fail::Rejected { version, why }) => panic!("{version} rejected: {why}"),
        }
    }

    /// Live: the published latest release downloads and its signature
    /// verifies against the built-in key; a flipped byte is rejected.
    /// Run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "hits GitHub"]
    fn latest_release_verifies() {
        let agent = Agent::new();
        let release: Release =
            serde_json::from_slice(&agent.get_bytes(&latest_release_url().unwrap(), 1 << 20).unwrap()).unwrap();
        let version = release.tag_name.trim_start_matches('v');
        let get = |name: &str| {
            let a = release.assets.iter().find(|a| a.name == name).expect(name);
            agent.get_bytes(&a.browser_download_url, MAX_EXE).unwrap()
        };
        let (mut exe, sig) = (get("cumulus.exe"), get("cumulus.exe.sig"));
        let sig = String::from_utf8_lossy(&sig).to_string();
        crate::signing::verify(version, &exe, &sig).expect("published release verifies");
        let mid = exe.len() / 2;
        exe[mid] ^= 0x40;
        assert!(crate::signing::verify(version, &exe, &sig).is_err(), "tampered download must be rejected");
        println!("release {version}: {} bytes, signature OK, tamper rejected", exe.len());
    }
}
