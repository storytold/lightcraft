use lightcraft_raster::Rgba8;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub(super) struct Region {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

fn valid(img: &Rgba8) -> bool {
    img.width > 0 && img.height > 0 && img.width.checked_mul(img.height) == Some(img.data.len())
}

/// Bound the selection in the *rendered* frame, with context for the diffusion model.
pub(super) fn region(mask: &Rgba8) -> Result<Region, String> {
    if !valid(mask) {
        return Err("invalid generation mask dimensions".into());
    }
    let (mut x0, mut y0, mut x1, mut y1) = (mask.width, mask.height, 0, 0);
    for (i, _) in mask.data.iter().enumerate().filter(|(_, p)| p[0] > 0) {
        let (x, y) = (i % mask.width, i / mask.width);
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x + 1);
        y1 = y1.max(y + 1);
    }
    if x1 <= x0 || y1 <= y0 {
        return Err("the selected mask is empty in the cropped photo".into());
    }
    let pad = ((x1 - x0).max(y1 - y0) / 8).max(32);
    x0 = x0.saturating_sub(pad);
    y0 = y0.saturating_sub(pad);
    x1 = x1.saturating_add(pad).min(mask.width);
    y1 = y1.saturating_add(pad).min(mask.height);
    Ok(Region { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
}

/// ComfyUI's VAE needs multiples of 8. Size is bounded regardless of the source resolution.
pub(super) fn network_size(r: Region) -> (usize, usize) {
    let scale = 1024.0 / r.w.max(r.h).max(1) as f64;
    let side = |v: usize| ((v as f64 * scale / 8.0).round() as usize * 8).clamp(64, 1024);
    (side(r.w), side(r.h))
}

fn sample(img: &Rgba8, x: f64, y: f64) -> [f32; 4] {
    let x = x.clamp(0.0, img.width.saturating_sub(1) as f64);
    let y = y.clamp(0.0, img.height.saturating_sub(1) as f64);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(img.width - 1), (y0 + 1).min(img.height - 1));
    let (dx, dy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
    let pixel = |x: usize, y: usize| img.data.get(y.saturating_mul(img.width).saturating_add(x)).copied().unwrap_or([0; 4]);
    let (a, b, c, d) = (pixel(x0, y0), pixel(x1, y0), pixel(x0, y1), pixel(x1, y1));
    std::array::from_fn(|k| {
        let top = a[k] as f32 + (b[k] as f32 - a[k] as f32) * dx;
        let bottom = c[k] as f32 + (d[k] as f32 - c[k] as f32) * dx;
        top + (bottom - top) * dy
    })
}

pub(super) fn crop(img: &Rgba8, r: Region) -> Result<Rgba8, String> {
    if !valid(img) || r.w == 0 || r.h == 0 || r.x.saturating_add(r.w) > img.width || r.y.saturating_add(r.h) > img.height {
        return Err("invalid generation region".into());
    }
    let (w, h) = network_size(r);
    let mut out = Rgba8::new(w, h);
    for (i, p) in out.data.iter_mut().enumerate() {
        let x = r.x as f64 + (i % w) as f64 * r.w as f64 / w as f64;
        let y = r.y as f64 + (i / w) as f64 * r.h as f64 / h as f64;
        *p = sample(img, x, y).map(|v| v.round().clamp(0.0, 255.0) as u8);
    }
    Ok(out)
}

/// Blend once, in linear light, through the full-resolution mask. No resampled mask or VAE
/// pixel can change an unselected pixel, and the photo's full dimensions are retained.
pub(super) fn composite(mut original: Rgba8, mask: &Rgba8, generated: &Rgba8, r: Region) -> Result<Rgba8, String> {
    if !valid(&original)
        || !valid(mask)
        || !valid(generated)
        || (original.width, original.height) != (mask.width, mask.height)
        || network_size(r) != (generated.width, generated.height)
        || r.x.saturating_add(r.w) > original.width
        || r.y.saturating_add(r.h) > original.height
    {
        return Err("ComfyUI returned unexpected image dimensions".into());
    }
    use lightcraft_color::transfer::{encode_srgb8, srgb_to_linear};
    let width = original.width;
    for (i, (p, m)) in original.data.iter_mut().zip(&mask.data).enumerate() {
        if m[0] == 0 {
            continue;
        }
        let (x, y) = (i % width, i / width);
        if x < r.x || y < r.y || x >= r.x + r.w || y >= r.y + r.h {
            continue;
        }
        let g = sample(generated, (x - r.x) as f64 * generated.width as f64 / r.w as f64, (y - r.y) as f64 * generated.height as f64 / r.h as f64);
        let alpha = f32::from(m[0]) / 255.0;
        for (p, v) in p.iter_mut().take(3).zip(g) {
            let a = srgb_to_linear(f32::from(*p) / 255.0);
            let b = srgb_to_linear(v / 255.0);
            *p = encode_srgb8(a + (b - a) * alpha);
        }
    }
    Ok(original)
}

pub(super) fn encode(img: &Rgba8) -> Result<Vec<u8>, String> {
    lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_bounds_and_composite_keep_full_resolution_and_unselected_pixels() {
        let mut original = Rgba8::new(200, 120);
        original.data.fill([100, 50, 10, 255]);
        let mut mask = Rgba8::new(200, 120);
        mask.data[60 * 200 + 100] = [255; 4];
        mask.data[60 * 200 + 101] = [128, 128, 128, 255];
        let r = region(&mask).unwrap();
        assert!(r.w < 200 && r.x < 100 && r.x + r.w > 101);
        let (w, h) = network_size(r);
        assert!(w <= 1024 && h <= 1024 && w % 8 == 0 && h % 8 == 0);
        let mut generated = Rgba8::new(w, h);
        generated.data.fill([10, 20, 240, 255]);
        let out = composite(original.clone(), &mask, &generated, r).unwrap();
        assert_eq!((out.width, out.height), (200, 120));
        for (i, (a, b)) in original.data.iter().zip(&out.data).enumerate() {
            if mask.data[i][0] == 0 {
                assert_eq!(a, b);
            }
        }
        assert_eq!(out.data[60 * 200 + 100], [10, 20, 240, 255]);
        assert_ne!(out.data[60 * 200 + 101], original.data[60 * 200 + 101]);
        assert!(region(&Rgba8::new(20, 20)).is_err());
        assert!(composite(original, &mask, &Rgba8::new(0, 0), r).is_err());
    }
}
