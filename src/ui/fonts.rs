//! Fallback fonts for the decorative Unicode that is common in track and
//! artist names (CJK, symbols, math letters, emoji...). egui's built-in
//! fonts only cover Latin/Greek/Cyrillic.
//!
//! The OS font files are memory-mapped rather than read, so only the parts
//! actually used for rendering are paged into RAM.

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

#[cfg(windows)]
const FALLBACKS: &[(&str, u32)] = &[
    ("segoeui.ttf", 0),  // extended Latin, general punctuation
    ("seguisym.ttf", 0), // arrows, math, dingbats, box drawing
    ("msyh.ttc", 0),     // Chinese
    ("YuGothM.ttc", 0),  // Japanese
    ("malgun.ttf", 0),   // Korean
    ("Nirmala.ttc", 0),  // Indic scripts
    ("LeelawUI.ttf", 0), // Thai, Lao, Khmer
    ("ebrima.ttf", 0),   // African scripts
    ("seguihis.ttf", 0), // historic scripts / runes some names use
    ("seguiemj.ttf", 0), // emoji (drawn monochrome)
];

#[cfg(not(windows))]
const FALLBACKS: &[(&str, u32)] = &[];

/// Main fonts to choose from: (label, file in the Windows Fonts folder).
/// "Default" is egui's built-in font.
pub const CHOICES: [(&str, &str); 11] = [
    ("Default", ""),
    ("Segoe UI", "segoeui.ttf"),
    ("Arial", "arial.ttf"),
    ("Calibri", "calibri.ttf"),
    ("Verdana", "verdana.ttf"),
    ("Tahoma", "tahoma.ttf"),
    ("Trebuchet MS", "trebuc.ttf"),
    ("Georgia", "georgia.ttf"),
    ("Bahnschrift", "bahnschrift.ttf"),
    ("Consolas", "consola.ttf"),
    ("Comic Sans MS", "comic.ttf"),
];

pub fn install(ctx: &egui::Context, main: &str) {
    let mut defs = FontDefinitions::default();
    let dir = std::env::var_os("WINDIR")
        .map(|w| std::path::PathBuf::from(w).join("Fonts"))
        .unwrap_or_default();
    // The chosen font goes first; the built-in one stays as a fallback.
    if let Some((_, file)) = CHOICES.iter().find(|(name, file)| *name == main && !file.is_empty()) {
        if let Some(bytes) = map(&dir.join(file)) {
            defs.font_data.insert(format!("main:{file}"), FontData::from_static(bytes).into());
            defs.families.entry(FontFamily::Proportional).or_default().insert(0, format!("main:{file}"));
        }
    }
    for &(file, index) in FALLBACKS {
        let Some(bytes) = map(&dir.join(file)) else { continue };
        let mut data = FontData::from_static(bytes);
        data.index = index;
        defs.font_data.insert(file.to_string(), data.into());
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            defs.families.entry(family).or_default().push(file.to_string());
        }
    }
    ctx.set_fonts(defs);
}

/// Maps a font file for the lifetime of the process (fonts are never unloaded).
fn map(path: &std::path::Path) -> Option<&'static [u8]> {
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: system font files are not modified while the app runs; the
    // mapping is read-only and intentionally leaked to get a 'static slice.
    let mmap = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    Some(&**Box::leak(Box::new(mmap)))
}
