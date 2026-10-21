//! "Start with Windows": a per-user Run entry (no admin needed). Started
//! that way, Cumulus opens hidden in the tray.

/// Passed by the Run entry so the window starts hidden.
pub const MINIMIZED_ARG: &str = "--minimized";

#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

#[cfg(windows)]
fn reg(args: &[&str]) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("reg")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .is_ok_and(|s| s.success())
}

/// Whether Cumulus is set to start with Windows.
#[cfg(windows)]
pub fn enabled() -> bool {
    reg(&["query", RUN_KEY, "/v", "Cumulus"])
}

/// Adds or removes the Run entry for this exe.
#[cfg(windows)]
pub fn set(on: bool) -> Result<(), String> {
    let ok = if on {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let command = format!("\"{}\" {MINIMIZED_ARG}", exe.display());
        reg(&["add", RUN_KEY, "/v", "Cumulus", "/t", "REG_SZ", "/d", &command, "/f"])
    } else {
        reg(&["delete", RUN_KEY, "/v", "Cumulus", "/f"]) || !enabled()
    };
    if ok { Ok(()) } else { Err("Windows refused the change".into()) }
}

/// Linux: ~/.config/autostart/cumulus.desktop.
#[cfg(not(windows))]
fn desktop_file() -> Option<std::path::PathBuf> {
    Some(dirs::config_dir()?.join("autostart").join("cumulus.desktop"))
}

#[cfg(not(windows))]
pub fn enabled() -> bool {
    desktop_file().is_some_and(|p| p.exists())
}

#[cfg(not(windows))]
pub fn set(on: bool) -> Result<(), String> {
    let path = desktop_file().ok_or("no config folder")?;
    if !on {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    // Inside an AppImage, current_exe is a temporary mount: start the AppImage itself.
    let exe = std::env::var("APPIMAGE").map(std::path::PathBuf::from).or_else(|_| std::env::current_exe()).map_err(|e| e.to_string())?;
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Cumulus\nComment=SoundCloud player\nExec=\"{}\" {MINIMIZED_ARG}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        exe.display()
    );
    std::fs::create_dir_all(path.parent().ok_or("bad path")?).map_err(|e| e.to_string())?;
    std::fs::write(&path, entry).map_err(|e| e.to_string())
}

/// Started by the Run entry (open hidden in the tray).
pub fn started_minimized() -> bool {
    std::env::args().any(|a| a == MINIMIZED_ARG)
}
