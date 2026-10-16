//! Self-update from the local build folder.
//!
//! New versions are built on this machine (`cargo build --release` writes
//! `target/release/cumulus.exe`). An installed copy notices a newer build
//! there and, when the user agrees, swaps itself: Windows lets a running .exe
//! be renamed, so the old file is moved aside, the new one copied into place
//! and started, and the old file is deleted on the next start.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

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
    let src = PathBuf::from(configured.unwrap_or_else(|| env!("CUMULUS_UPDATE_SOURCE").to_string()));
    let me = std::env::current_exe().ok()?.canonicalize().ok()?;
    let src_real = src.canonicalize().ok()?;
    (src_real != me).then_some(src_real)
}

/// Remembers the source file's timestamp so the (cheap) check only asks a
/// build for its number when the file actually changed.
#[derive(Default)]
pub struct Checker {
    seen: Option<SystemTime>,
}

impl Checker {
    /// The source exe, if it holds a newer build than this one.
    pub fn newer_build(&mut self) -> Option<PathBuf> {
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
