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

/// Linux: fallbacks found with fontconfig (CJK, symbols...).
#[cfg(not(windows))]
const FALLBACKS: &[(&str, u32)] = &[];

#[cfg(target_os = "linux")]
const LINUX_FALLBACKS: &[&str] = &["Noto Sans CJK JP", "Noto Sans Symbols", "Noto Sans Symbols 2", "DejaVu Sans", "Noto Sans Math"];

/// A font family's file, via `fc-match` (Linux).
#[cfg(target_os = "linux")]
fn fc_file(family: &str) -> Option<std::path::PathBuf> {
    let out = std::process::Command::new("fc-match").args(["-f", "%{file}", family]).output().ok()?;
    let path = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    // fc-match always answers with *something*; only accept the family asked for.
    let check = std::process::Command::new("fc-match").args(["-f", "%{family}", family]).output().ok()?;
    let got = String::from_utf8_lossy(&check.stdout).to_lowercase();
    (path.is_file() && got.contains(&family.to_lowercase().replace(" 2", ""))).then_some(path)
}

/// Main fonts to choose from: (label, file in the Windows Fonts folder).
/// "Default" is egui's built-in font.
#[cfg(windows)]
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

/// Linux: font families (looked up with fontconfig when picked).
#[cfg(not(windows))]
pub const CHOICES: [(&str, &str); 8] = [
    ("Default", ""),
    ("Noto Sans", "Noto Sans"),
    ("DejaVu Sans", "DejaVu Sans"),
    ("Ubuntu", "Ubuntu"),
    ("Liberation Sans", "Liberation Sans"),
    ("Cantarell", "Cantarell"),
    ("Inter", "Inter"),
    ("Noto Sans Mono", "Noto Sans Mono"),
];

/// Where a chosen font's file is.
fn choice_path(dir: &std::path::Path, file: &str) -> Option<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let _ = dir;
        fc_file(file)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Some(dir.join(file))
    }
}

pub fn install(ctx: &egui::Context, main: &str) {
    let mut defs = FontDefinitions::default();
    // egui's monospace font has the arrows and small triangles (← → ▾) the
    // UI uses; let normal text fall back to it so they draw on every system.
    defs.families.entry(FontFamily::Proportional).or_default().push("Hack".to_owned());
    let dir = std::env::var_os("WINDIR")
        .map(|w| std::path::PathBuf::from(w).join("Fonts"))
        .unwrap_or_default();
    // The chosen font goes first; the built-in one stays as a fallback.
    if let Some((_, file)) = CHOICES.iter().find(|(name, file)| *name == main && !file.is_empty()) {
        if let Some(bytes) = choice_path(&dir, file).and_then(|p| map(&p)) {
            defs.font_data.insert(format!("main:{file}"), FontData::from_static(bytes).into());
            defs.families.entry(FontFamily::Proportional).or_default().insert(0, format!("main:{file}"));
        }
    }
    #[cfg(target_os = "linux")]
    for family in LINUX_FALLBACKS {
        let Some(bytes) = fc_file(family).and_then(|p| map(&p)) else { continue };
        let key = format!("fallback:{family}");
        defs.font_data.insert(key.clone(), FontData::from_static(bytes).into());
        for f in [FontFamily::Proportional, FontFamily::Monospace] {
            defs.families.entry(f).or_default().push(key.clone());
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
