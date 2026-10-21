//! OS integration: the system media controls (Windows SMTC overlay and
//! hardware media keys, MPRIS on Linux, Now Playing on macOS) and the tray icon.

use std::time::{Duration, Instant};

use eframe::egui;
use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig};
#[cfg(not(target_os = "linux"))]
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
#[cfg(not(target_os = "linux"))]
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::api::models::{Track, sized_art};

pub enum DesktopEvent {
    Media(MediaControlEvent),
    ToggleWindow,
    /// Bring the window back (Linux tray click; never hides, so a
    /// double-click doesn't open and close it again).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    ShowWindow,
    PlayPause,
    Next,
    Previous,
    Quit,
}

#[cfg(not(target_os = "linux"))]
struct TrayIds {
    show: MenuId,
    play: MenuId,
    next: MenuId,
    prev: MenuId,
    quit: MenuId,
}

/// The tray icon and its Play/Pause item.
#[cfg(not(target_os = "linux"))]
struct Tray {
    icon: TrayIcon,
    play: MenuItem,
}

#[cfg(not(target_os = "linux"))]
impl Tray {
    fn new(send: impl Fn(DesktopEvent) + Send + Sync + Clone + 'static) -> Option<Self> {
        tray(send).map(|(icon, play)| Self { icon, play })
    }

    fn set_tooltip(&self, tip: String) {
        let _ = self.icon.set_tooltip(Some(tip));
    }

    fn set_play_text(&self, text: &'static str) {
        self.play.set_text(text);
    }
}

/// Linux: a StatusNotifierItem (KDE's tray protocol, also used by GNOME's
/// AppIndicator extension). Unlike AppIndicator it reports clicks, so a left
/// click shows/hides Cumulus and right click opens the menu.
#[cfg(target_os = "linux")]
struct Tray(ksni::blocking::Handle<Sni>);

#[cfg(target_os = "linux")]
struct Sni {
    send: Box<dyn Fn(DesktopEvent) + Send + Sync>,
    playing: bool,
    tip: String,
    icon: Vec<ksni::Icon>,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for Sni {
    fn id(&self) -> String {
        "cumulus".into()
    }

    fn title(&self) -> String {
        "Cumulus".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icon.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: self.tip.clone(), ..Default::default() }
    }

    /// Left click (or double-click): open Cumulus, like other KDE tray apps.
    fn activate(&mut self, _x: i32, _y: i32) {
        (self.send)(DesktopEvent::ShowWindow);
    }

    /// Middle click: play/pause.
    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        (self.send)(DesktopEvent::PlayPause);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        let item = |label: &str, event: fn() -> DesktopEvent| -> ksni::MenuItem<Self> {
            StandardItem { label: label.into(), activate: Box::new(move |t: &mut Self| (t.send)(event())), ..Default::default() }.into()
        };
        vec![
            item("Show / hide Cumulus", || DesktopEvent::ToggleWindow),
            ksni::MenuItem::Separator,
            item(if self.playing { "Pause" } else { "Play" }, || DesktopEvent::PlayPause),
            item("Next", || DesktopEvent::Next),
            item("Previous", || DesktopEvent::Previous),
            ksni::MenuItem::Separator,
            item("Quit", || DesktopEvent::Quit),
        ]
    }
}

#[cfg(target_os = "linux")]
impl Tray {
    /// None when there's no tray host (GNOME without the extension, WSL): an
    /// icon would be invisible, so Cumulus mustn't hide to it.
    fn new(send: impl Fn(DesktopEvent) + Send + Sync + Clone + 'static) -> Option<Self> {
        use ksni::blocking::TrayMethods;
        let size = 64;
        // RGBA -> ARGB, as the protocol wants.
        let data = super::icon::rgba(size).chunks_exact(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
        let sni = Sni {
            send: Box::new(send),
            playing: false,
            tip: "Cumulus".into(),
            icon: vec![ksni::Icon { width: size as i32, height: size as i32, data }],
        };
        sni.spawn().ok().map(Self)
    }

    fn set_tooltip(&self, tip: String) {
        self.0.update(|t| t.tip = tip);
    }

    fn set_play_text(&self, text: &'static str) {
        let playing = text == "Pause";
        self.0.update(|t| t.playing = playing);
    }
}

pub struct Desktop {
    media: Option<MediaControls>,
    tray: Option<Tray>,
    shown_track: Option<u64>,
    shown_playing: Option<bool>,
    last_progress: Instant,
}

impl Desktop {
    /// Sets up media controls and the tray. Either may be unavailable (e.g.
    /// no tray on some Linux desktops); the app works without them.
    pub fn new(cc: &eframe::CreationContext<'_>, send: impl Fn(DesktopEvent) + Send + Sync + Clone + 'static) -> Self {
        let media = media_controls(cc, send.clone());
        let tray = Tray::new(send);
        Self { media, tray, shown_track: None, shown_playing: None, last_progress: Instant::now() }
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
                tray.set_tooltip(tip);
            }
        }
        let refresh_progress = playing && self.last_progress.elapsed() > Duration::from_secs(5);
        if self.shown_playing != Some(playing) || refresh_progress {
            self.shown_playing = Some(playing);
            self.last_progress = Instant::now();
            if let Some(tray) = &self.tray {
                tray.set_play_text(if playing { "Pause" } else { "Play" });
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

    /// Whether the window can hide to the tray: needs a tray, and a display
    /// that lets apps hide their windows (native Wayland doesn't).
    pub fn can_hide_to_tray(&self) -> bool {
        self.has_tray() && !crate::NATIVE_WAYLAND.load(std::sync::atomic::Ordering::Relaxed)
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

#[cfg(not(target_os = "linux"))]
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
