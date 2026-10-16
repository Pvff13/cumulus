//! OS integration: the system media controls (Windows SMTC overlay and
//! hardware media keys, MPRIS on Linux, Now Playing on macOS) and the tray icon.

use std::time::{Duration, Instant};

use eframe::egui;
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::api::models::{Track, sized_art};

pub enum DesktopEvent {
    Media(MediaControlEvent),
    ToggleWindow,
    PlayPause,
    Next,
    Previous,
    Quit,
}

struct TrayIds {
    show: MenuId,
    play: MenuId,
    next: MenuId,
    prev: MenuId,
    quit: MenuId,
}

pub struct Desktop {
    media: Option<MediaControls>,
    tray: Option<TrayIcon>,
    play_item: Option<MenuItem>,
    shown_track: Option<u64>,
    shown_playing: Option<bool>,
    last_progress: Instant,
}

impl Desktop {
    /// Sets up media controls and the tray. Either may be unavailable (e.g.
    /// no tray on some Linux desktops); the app works without them.
    pub fn new(cc: &eframe::CreationContext<'_>, send: impl Fn(DesktopEvent) + Send + Sync + Clone + 'static) -> Self {
        let media = media_controls(cc, send.clone());
        let (tray, play_item) = match tray(send) {
            Some((t, p)) => (Some(t), Some(p)),
            None => (None, None),
        };
        Self { media, tray, play_item, shown_track: None, shown_playing: None, last_progress: Instant::now() }
    }

    /// Keeps the OS controls in step with playback. Cheap to call every tick:
    /// it only talks to the OS when something changed (and every few seconds
    /// while playing, to keep the overlay's timeline accurate).
    pub fn update(&mut self, track: Option<&Track>, playing: bool, position: Option<f64>) {
        let track_id = track.map(|t| t.id);
        if track_id != self.shown_track {
            self.shown_track = track_id;
            self.shown_playing = None;
            if let Some(m) = self.media.as_mut() {
                let cover = track.and_then(|t| t.artwork()).map(|u| sized_art(u, "t300x300"));
                let _ = m.set_metadata(MediaMetadata {
                    title: track.map(|t| t.title.as_str()),
                    artist: track.map(|t| t.user.username.as_str()),
                    album: Some("SoundCloud"),
                    cover_url: cover.as_deref(),
                    duration: track.map(|t| Duration::from_millis(t.duration)),
                });
            }
            if let Some(tray) = &self.tray {
                let tip = match track {
                    Some(t) => truncate(&format!("{} · {}", t.title, t.user.username), 120),
                    None => "Cumulus".to_string(),
                };
                let _ = tray.set_tooltip(Some(tip));
            }
        }
        let refresh_progress = playing && self.last_progress.elapsed() > Duration::from_secs(5);
        if self.shown_playing != Some(playing) || refresh_progress {
            self.shown_playing = Some(playing);
            self.last_progress = Instant::now();
            if let Some(item) = &self.play_item {
                item.set_text(if playing { "Pause" } else { "Play" });
            }
            if let Some(m) = self.media.as_mut() {
                let progress = position.map(|p| MediaPosition(Duration::from_secs_f64(p.max(0.0))));
                let state = match (track_id, playing) {
                    (None, _) => MediaPlayback::Stopped,
                    (Some(_), true) => MediaPlayback::Playing { progress },
                    (Some(_), false) => MediaPlayback::Paused { progress },
                };
                let _ = m.set_playback(state);
            }
        }
    }

    pub fn has_tray(&self) -> bool {
        self.tray.is_some()
    }

    /// Call after a seek so the overlay's timeline jumps too.
    pub fn position_changed(&mut self) {
        self.shown_playing = None;
    }
}

fn media_controls(
    cc: &eframe::CreationContext<'_>,
    send: impl Fn(DesktopEvent) + Send + Sync + 'static,
) -> Option<MediaControls> {
    #[cfg(windows)]
    let hwnd = {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match cc.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get() as *mut std::ffi::c_void),
            _ => None,
        }
    };
    #[cfg(not(windows))]
    let hwnd = {
        let _ = cc;
        None
    };
    let config = PlatformConfig { display_name: "Cumulus", dbus_name: "cumulus_player", hwnd };
    let mut controls = MediaControls::new(config).ok()?;
    controls.attach(move |e| send(DesktopEvent::Media(e))).ok()?;
    Some(controls)
}

fn tray(send: impl Fn(DesktopEvent) + Send + Sync + Clone + 'static) -> Option<(TrayIcon, MenuItem)> {
    let show = MenuItem::new("Show / hide Cumulus", true, None);
    let play = MenuItem::new("Play", true, None);
    let next = MenuItem::new("Next", true, None);
    let prev = MenuItem::new("Previous", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    for item in [&show as &dyn tray_icon::menu::IsMenuItem, &PredefinedMenuItem::separator(), &play, &next, &prev] {
        menu.append(item).ok()?;
    }
    menu.append(&PredefinedMenuItem::separator()).ok()?;
    menu.append(&quit).ok()?;
    let ids = TrayIds {
        show: show.id().clone(),
        play: play.id().clone(),
        next: next.id().clone(),
        prev: prev.id().clone(),
        quit: quit.id().clone(),
    };

    let size = 32;
    let icon = tray_icon::Icon::from_rgba(super::icon::rgba(size), size, size).ok()?;
    let tray = TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("Cumulus")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .ok()?;

    let on_click = send.clone();
    TrayIconEvent::set_event_handler(Some(move |e| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            on_click(DesktopEvent::ToggleWindow);
        }
    }));
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let event = match e.id {
            id if id == ids.show => DesktopEvent::ToggleWindow,
            id if id == ids.play => DesktopEvent::PlayPause,
            id if id == ids.next => DesktopEvent::Next,
            id if id == ids.prev => DesktopEvent::Previous,
            id if id == ids.quit => DesktopEvent::Quit,
            _ => return,
        };
        send(event);
    }));
    Some((tray, play))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max - 1).chain(std::iter::once('…')).collect()
    }
}

/// Window icon for the title bar / taskbar.
pub fn window_icon() -> egui::IconData {
    let size = 64;
    egui::IconData { rgba: super::icon::rgba(size), width: size, height: size }
}
