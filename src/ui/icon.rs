//! The app icon, drawn in code (a white cloud on the accent colour) so the
//! window, tray and installer all use the same image without asset files.

/// Square RGBA image of `size` × `size` pixels.
pub fn rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let (r, g, b) = (0x8b, 0x6c, 0xff);
    let radius = s * 0.22;
    // Cloud = three circles over a rounded base.
    let circles = [(0.36, 0.56, 0.17), (0.56, 0.47, 0.22), (0.72, 0.60, 0.13)];
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            // Rounded-square background with a 1px soft edge.
            let dx = (px - s / 2.0).abs() - (s / 2.0 - radius);
            let dy = (py - s / 2.0).abs() - (s / 2.0 - radius);
            // Rounded-box signed distance: negative inside, positive outside.
            let outside = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - radius;
            let bg_alpha = (0.5 - outside).clamp(0.0, 1.0);
            // Cloud coverage (signed distance, antialiased).
            let (u, v) = (px / s, py / s);
            let mut d = f32::MAX;
            for (cx, cy, cr) in circles {
                d = d.min(((u - cx).hypot(v - cy) - cr) * s);
            }
            let base = {
                let bx = (u - 0.54).abs() - 0.22;
                let by = (v - 0.66).abs() - 0.07;
                (bx.max(by)) * s
            };
            d = d.min(base);
            let white = (0.5 - d).clamp(0.0, 1.0);
            let mix = |c: u8| (f32::from(c) * (1.0 - white) + 255.0 * white) as u8;
            out.extend_from_slice(&[mix(r), mix(g), mix(b), (bg_alpha * 255.0) as u8]);
        }
    }
    out
}

/// Debug builds: writes `icon.png` (256 px) and a multi-size `icon.ico` for
/// the exe resource and installer (`cargo run -- --dev-icon assets`). Each
/// size is drawn natively so small taskbar icons stay sharp; entries are
/// PNG-compressed, which Windows supports since Vista.
#[cfg(debug_assertions)]
pub fn write_files(dir: &std::path::Path) {
    const SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];
    let png = |size: u32| {
        let img = image::RgbaImage::from_raw(size, size, rgba(size)).expect("icon buffer size");
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).expect("encode png");
        out
    };
    let images: Vec<(u32, Vec<u8>)> = SIZES.iter().map(|&s| (s, png(s))).collect();
    let mut ico = vec![0, 0, 1, 0];
    ico.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, data) in &images {
        let dim = if *size >= 256 { 0 } else { *size as u8 }; // 0 means 256
        ico.extend_from_slice(&[dim, dim, 0, 0, 1, 0, 32, 0]); // no palette, 1 plane, 32 bpp
        ico.extend_from_slice(&(data.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in &images {
        ico.extend_from_slice(data);
    }
    std::fs::create_dir_all(dir).expect("create icon dir");
    std::fs::write(dir.join("icon.png"), &images.last().expect("256 px image").1).expect("write icon.png");
    std::fs::write(dir.join("icon.ico"), &ico).expect("write icon.ico");
    println!("wrote {}/icon.png and icon.ico ({} sizes)", dir.display(), images.len());
}

#[cfg(test)]
mod tests {
    #[test]
    fn icon_shape() {
        let px = super::rgba(64);
        assert_eq!(px.len(), 64 * 64 * 4);
        let at = |x: usize, y: usize| &px[(y * 64 + x) * 4..][..4];
        assert_eq!(at(0, 0)[3], 0, "rounded corner is transparent");
        assert_eq!(at(36, 40), &[255, 255, 255, 255], "cloud is white");
        assert_eq!(at(32, 8), &[0x8b, 0x6c, 0xff, 255], "background is solid accent");
        assert_eq!(at(3, 32), &[0x8b, 0x6c, 0xff, 255], "solid right up to the straight edges");
    }
}
