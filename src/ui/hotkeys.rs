//! Rebindable keyboard shortcuts, plus optional global (system-wide)
//! hotkeys for the media actions.

use std::collections::BTreeMap;

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};

/// Everything a key can do.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Action {
    PlayPause,
    Next,
    Previous,
    SeekForward,
    SeekBack,
    VolumeUp,
    VolumeDown,
    Mute,
    Shuffle,
    Repeat,
    Like,
    Search,
    Back,
    Forward,
    Slower,
    Faster,
}

impl Action {
    pub const ALL: [Action; 16] = [
        Self::PlayPause,
        Self::Next,
        Self::Previous,
        Self::SeekForward,
        Self::SeekBack,
        Self::VolumeUp,
        Self::VolumeDown,
        Self::Mute,
        Self::Shuffle,
        Self::Repeat,
        Self::Like,
        Self::Search,
        Self::Back,
        Self::Forward,
        Self::Slower,
        Self::Faster,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::PlayPause => "Play / pause",
            Self::Next => "Next track",
            Self::Previous => "Previous track",
            Self::SeekForward => "Seek forward 5 s",
            Self::SeekBack => "Seek back 5 s",
            Self::VolumeUp => "Volume up",
            Self::VolumeDown => "Volume down",
            Self::Mute => "Mute",
            Self::Shuffle => "Shuffle",
            Self::Repeat => "Repeat",
            Self::Like => "Like (on SoundCloud)",
            Self::Search => "Search",
            Self::Back => "Back",
            Self::Forward => "Forward",
            Self::Slower => "Slower",
            Self::Faster => "Faster",
        }
    }

    /// Name in config.json.
    pub fn key(self) -> String {
        format!("{self:?}")
    }

    /// Only these make sense system-wide (while another app is in front).
    pub fn can_be_global(self) -> bool {
        matches!(self, Self::PlayPause | Self::Next | Self::Previous | Self::VolumeUp | Self::VolumeDown | Self::Mute | Self::Like)
    }

    /// The built-in in-app shortcut.
    pub fn default_key(self) -> Option<&'static str> {
        Some(match self {
            Self::PlayPause => "Space",
            Self::Next => "Ctrl+Right",
            Self::Previous => "Ctrl+Left",
            Self::SeekForward => "Right",
            Self::SeekBack => "Left",
            Self::VolumeUp => "Up",
            Self::VolumeDown => "Down",
            Self::Mute => "M",
            Self::Shuffle => "S",
            Self::Repeat => "R",
            Self::Like => "L",
            Self::Search => "Ctrl+F",
            Self::Back => "Alt+Left",
            Self::Forward => "Alt+Right",
            Self::Slower => "Shift+Comma",
            Self::Faster => "Shift+Period",
        })
    }
}

/// "Ctrl+Shift+ArrowRight" -> shortcut (egui key names).
pub fn parse(s: &str) -> Option<KeyboardShortcut> {
    let mut mods = Modifiers::NONE;
    let mut key = None;
    for part in s.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods = mods | Modifiers::COMMAND,
            "shift" => mods = mods | Modifiers::SHIFT,
            "alt" => mods = mods | Modifiers::ALT,
            _ => key = Some(Key::from_name(part)?),
        }
    }
    Some(KeyboardShortcut::new(mods, key?))
}

/// Shortcut -> "Ctrl+Shift+ArrowRight".
pub fn format(s: &KeyboardShortcut) -> String {
    let mut parts = Vec::new();
    if s.modifiers.command || s.modifiers.ctrl {
        parts.push("Ctrl");
    }
    if s.modifiers.alt {
        parts.push("Alt");
    }
    if s.modifiers.shift {
        parts.push("Shift");
    }
    parts.push(s.logical_key.name());
    parts.join("+")
}

/// Friendlier text for a binding ("Ctrl+→").
pub fn pretty(s: &str) -> String {
    s.split('+')
        .map(|p| match p {
            "Right" => "→",
            "Left" => "←",
            "Up" => "↑",
            "Down" => "↓",
            "Comma" => ",",
            "Period" => ".",
            other => other,
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// egui key name -> the code name the global-hotkey crate parses.
fn global_code(key: Key) -> Option<String> {
    let name = key.name();
    Some(match key {
        Key::A | Key::B | Key::C | Key::D | Key::E | Key::F | Key::G | Key::H | Key::I | Key::J | Key::K | Key::L | Key::M | Key::N | Key::O | Key::P | Key::Q | Key::R | Key::S | Key::T | Key::U | Key::V | Key::W | Key::X | Key::Y | Key::Z => format!("Key{name}"),
        Key::Num0 | Key::Num1 | Key::Num2 | Key::Num3 | Key::Num4 | Key::Num5 | Key::Num6 | Key::Num7 | Key::Num8 | Key::Num9 => format!("Digit{name}"),
        Key::ArrowLeft => "ArrowLeft".into(),
        Key::ArrowRight => "ArrowRight".into(),
        Key::ArrowUp => "ArrowUp".into(),
        Key::ArrowDown => "ArrowDown".into(),
        Key::Space | Key::Home | Key::End | Key::PageUp | Key::PageDown | Key::Insert | Key::Delete | Key::Comma | Key::Period | Key::Minus | Key::Slash | Key::Semicolon | Key::Backslash => name.to_string(),
        Key::Equals => "Equal".into(),
        Key::OpenBracket => "BracketLeft".into(),
        Key::CloseBracket => "BracketRight".into(),
        k if name.starts_with('F') && name[1..].parse::<u8>().is_ok() => {
            let _ = k;
            name.to_string()
        }
        _ => return None,
    })
}

/// A shortcut as a global hotkey, if it can be one.
pub fn to_global(s: &KeyboardShortcut) -> Option<global_hotkey::hotkey::HotKey> {
    use global_hotkey::hotkey::{HotKey, Modifiers as M};
    let code = global_code(s.logical_key)?;
    let mut mods = M::empty();
    if s.modifiers.command || s.modifiers.ctrl {
        mods |= M::CONTROL;
    }
    if s.modifiers.alt {
        mods |= M::ALT;
    }
    if s.modifiers.shift {
        mods |= M::SHIFT;
    }
    let code: global_hotkey::hotkey::Code = code.parse().ok()?;
    Some(HotKey::new(Some(mods), code))
}

/// The user's bindings: in-app (overrides of the defaults) and global.
#[derive(Default)]
pub struct Bindings {
    /// action -> shortcut text ("" = no key).
    pub local: BTreeMap<Action, String>,
    pub global: BTreeMap<Action, String>,
}

impl Bindings {
    pub fn load(c: &crate::config::Config) -> Self {
        let find = |name: &str| Action::ALL.into_iter().find(|a| a.key() == name);
        let mut b = Self::default();
        for a in Action::ALL {
            b.local.insert(a, a.default_key().unwrap_or_default().to_string());
        }
        for (name, key) in &c.hotkeys {
            if let Some(a) = find(name) {
                b.local.insert(a, key.clone());
            }
        }
        for (name, key) in &c.global_hotkeys {
            if let Some(a) = find(name).filter(|a| a.can_be_global()) {
                b.global.insert(a, key.clone());
            }
        }
        b
    }

    pub fn save(&self) {
        let mut c = crate::config::Config::load();
        c.hotkeys = self
            .local
            .iter()
            .filter(|(a, k)| a.default_key() != Some(k.as_str()))
            .map(|(a, k)| (a.key(), k.clone()))
            .collect();
        c.global_hotkeys = self.global.iter().filter(|(_, k)| !k.is_empty()).map(|(a, k)| (a.key(), k.clone())).collect();
        c.save();
    }

    /// In-app shortcuts, most modifiers first (so Ctrl+→ wins over →).
    pub fn local_shortcuts(&self) -> Vec<(KeyboardShortcut, Action)> {
        let mut v: Vec<_> = self.local.iter().filter_map(|(a, k)| parse(k).map(|s| (s, *a))).collect();
        let count = |m: &Modifiers| u8::from(m.command || m.ctrl) + u8::from(m.alt) + u8::from(m.shift);
        v.sort_by_key(|(s, _)| std::cmp::Reverse(count(&s.modifiers)));
        v
    }
}

/// Registers global hotkeys and turns their presses into actions.
pub struct Global {
    manager: Option<global_hotkey::GlobalHotKeyManager>,
    registered: Vec<(u32, Action, global_hotkey::hotkey::HotKey)>,
}

impl Global {
    /// `on_press` runs on the event loop when a hotkey fires.
    pub fn new(on_press: impl Fn(u32) + Send + Sync + 'static) -> Self {
        global_hotkey::GlobalHotKeyEvent::set_event_handler(Some(move |e: global_hotkey::GlobalHotKeyEvent| {
            if e.state == global_hotkey::HotKeyState::Pressed {
                on_press(e.id);
            }
        }));
        Self { manager: global_hotkey::GlobalHotKeyManager::new().ok(), registered: Vec::new() }
    }

    /// Replaces the registered set. Returns combos Windows refused (taken by
    /// another app).
    pub fn set(&mut self, bindings: &Bindings) -> Vec<String> {
        let Some(m) = &self.manager else { return vec!["global hotkeys aren't available".into()] };
        for (_, _, hk) in self.registered.drain(..) {
            let _ = m.unregister(hk);
        }
        let mut failed = Vec::new();
        for (a, k) in &bindings.global {
            let Some(hk) = parse(k).and_then(|s| to_global(&s)) else { continue };
            match m.register(hk) {
                Ok(()) => self.registered.push((hk.id(), *a, hk)),
                Err(_) => failed.push(pretty(k)),
            }
        }
        failed
    }

    pub fn action(&self, id: u32) -> Option<Action> {
        self.registered.iter().find(|(i, ..)| *i == id).map(|(_, a, _)| *a)
    }
}

/// Reads the next key press for the recorder: Some(Some(text)) for a key,
/// Some(None) to clear (Backspace/Delete), None while waiting. Esc cancels
/// via `cancel`.
pub fn record(ctx: &egui::Context, cancel: &mut bool) -> Option<Option<String>> {
    ctx.input_mut(|i| {
        let mut out = None;
        i.events.retain(|e| match e {
            egui::Event::Key { key, pressed: true, modifiers, .. } if out.is_none() && !*cancel => {
                match key {
                    Key::Escape => *cancel = true,
                    Key::Backspace | Key::Delete if modifiers.is_none() => out = Some(None),
                    k => out = Some(Some(format(&KeyboardShortcut::new(*modifiers, *k)))),
                }
                false
            }
            _ => true,
        });
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for a in Action::ALL {
            let k = a.default_key().unwrap();
            let s = parse(k).unwrap_or_else(|| panic!("{k}"));
            assert_eq!(format(&s), k, "{a:?}");
        }
        assert!(parse("Ctrl+Nonsense").is_none());
        assert_eq!(pretty("Ctrl+Right"), "Ctrl+→");
    }

    #[test]
    fn globals_convert() {
        assert!(to_global(&parse("Ctrl+Alt+P").unwrap()).is_some());
        assert!(to_global(&parse("Ctrl+Shift+Right").unwrap()).is_some());
        assert!(to_global(&parse("F9").unwrap()).is_some());
    }

    #[test]
    fn modifiers_first() {
        let b = Bindings::load(&crate::config::Config::default());
        let order = b.local_shortcuts();
        let next = order.iter().position(|(_, a)| *a == Action::Next).unwrap();
        let seek = order.iter().position(|(_, a)| *a == Action::SeekForward).unwrap();
        assert!(next < seek);
    }
}
