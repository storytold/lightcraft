//! The identity plate's graphic: an SVG (rasterised with resvg, pure Rust) or a PNG / JPEG file,
//! fitted to the module bar's height. Loaded once per path and kept as a texture; a file that
//! can't be read or decoded is an error the command reports, never a panic.

/// The largest plate file read (a logo is kilobytes; this bounds a hostile choice).
const MAX_FILE: u64 = 8 << 20;
/// The plate's pixel height before display scaling (the bar is 44 px; 2× for sharp HiDPI).
pub const PIXEL_HEIGHT: u32 = 64;
/// The widest plate, in pixels (a banner logo is clipped to this).
const MAX_WIDTH: u32 = 1024;

/// Decode `path` into premultiplied RGBA pixels `PIXEL_HEIGHT` tall (or smaller), keeping its aspect.
pub fn load(path: &str) -> Result<egui::ColorImage, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path;
        Err("an identity plate image needs the desktop app".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let len = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?.len();
        if len > MAX_FILE {
            return Err(format!("{path} is too large for an identity plate ({len} bytes)"));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        let svg = std::path::Path::new(path).extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("svg"));
        if svg { rasterise_svg(&bytes) } else { decode_raster(&bytes) }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn rasterise_svg(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default()).map_err(|e| format!("not a usable SVG: {e}"))?;
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return Err("the SVG has no size".into());
    }
    let scale = PIXEL_HEIGHT as f32 / h;
    let pw = ((w * scale).round() as u32).clamp(1, MAX_WIDTH);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pw, PIXEL_HEIGHT).ok_or("can't allocate the plate")?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    Ok(egui::ColorImage::from_rgba_premultiplied([pw as usize, PIXEL_HEIGHT as usize], pixmap.data()))
}

#[cfg(not(target_arch = "wasm32"))]
fn decode_raster(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    // fitted into a box twice the plate's height wide-ish: a logo is short and wide
    let t = dac_codecs::decode_thumbnail(bytes, MAX_WIDTH).map_err(|e| format!("not a usable image: {e}"))?;
    let img = t.image;
    if img.width == 0 || img.height == 0 || img.data.len() != img.width.saturating_mul(img.height) {
        return Err("the image is empty".into());
    }
    let flat: Vec<u8> = img.data.iter().flatten().copied().collect();
    let full = egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &flat);
    // scale to the plate's height (nearest: cheap, once per choice)
    let h = (PIXEL_HEIGHT as usize).min(img.height);
    let w = (img.width.saturating_mul(h) / img.height).clamp(1, MAX_WIDTH as usize);
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        let sy = (y * img.height / h).min(img.height - 1);
        for x in 0..w {
            let sx = (x * img.width / w).min(img.width - 1);
            pixels.push(full.pixels.get(sy * img.width + sx).copied().unwrap_or_default());
        }
    }
    Ok(egui::ColorImage { size: [w, h], source_size: egui::vec2(w as f32, h as f32), pixels })
}

/// The plate's texture for `path`, loaded on first use and cached in egui's memory by path.
/// `None` when it can't be loaded (the command that chose it already said why).
pub fn texture(ctx: &egui::Context, path: &str) -> Option<egui::TextureHandle> {
    let key = egui::Id::new(("identity-plate-image", path.to_string()));
    if let Some(cached) = ctx.data(|d| d.get_temp::<Option<egui::TextureHandle>>(key)) {
        return cached;
    }
    let tex = load(path).ok().map(|img| ctx.load_texture("identity-plate", img, egui::TextureOptions::LINEAR));
    ctx.data_mut(|d| d.insert_temp(key, tex.clone()));
    tex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_svg_rasterises_to_the_plate_height_keeping_its_aspect() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="50"><rect width="200" height="50" fill="red"/></svg>"#;
        let img = rasterise_svg(svg).unwrap();
        assert_eq!(img.size, [256, PIXEL_HEIGHT as usize]);
        assert!(img.pixels.iter().any(|p| p.r() > 200));
    }

    #[test]
    fn bad_files_are_errors_not_panics() {
        assert!(rasterise_svg(b"<svg").is_err());
        assert!(rasterise_svg(br#"<svg xmlns="http://www.w3.org/2000/svg" width="0" height="0"/>"#).is_err());
        assert!(decode_raster(b"\x89PNG garbage").is_err());
        assert!(load("/no/such/plate.svg").is_err());
    }
}
