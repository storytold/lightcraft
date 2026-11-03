use super::encoding::srgb8_in;
use super::{DeepImage, DeepSamples, Deserialize, OutputSpace, Plane, Rgba8, Serialize};

/// Where a watermark sits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    #[default]
    BottomRight,
}

/// A text watermark, sized relative to the image so every export size looks the same.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Watermark {
    pub text: String,
    /// Basic Japanese vertical lettering: upright glyphs in columns from right to left.
    pub vertical: bool,
    /// Text height as a fraction of the image's short edge.
    pub size: f32,
    /// 0..1.
    pub opacity: f32,
    pub anchor: Anchor,
    /// Margin from the left or right edge, as a fraction of the short edge (unused when the anchor
    /// is centred across: top, center, bottom).
    pub inset_x: f32,
    /// Margin from the top or bottom edge, as a fraction of the short edge (unused when the anchor
    /// is centred down: left, center, right).
    pub inset_y: f32,
    /// Degrees clockwise: 0, 90, 180 or 270 (anything else draws unrotated). The watermark is
    /// sized before it is turned, and the insets are measured to the turned watermark.
    pub rotation: u16,
    /// sRGB colour.
    pub color: [u8; 3],
    /// Soft dark drop shadow for legibility on bright areas.
    pub shadow: bool,
    /// A graphic (PNG with transparency, JPEG, …) drawn instead of the text (empty = text).
    pub image: String,
    /// The graphic's width as a fraction of the photo's width.
    pub image_width: f32,
    /// The colour space the photo is in (set by the encoder; the graphic is converted into it).
    #[serde(skip)]
    pub target: Option<OutputSpace>,
}

impl Default for Watermark {
    fn default() -> Self {
        Self {
            text: String::new(),
            vertical: false,
            size: 0.035,
            opacity: 0.7,
            anchor: Anchor::BottomRight,
            inset_x: 0.025,
            inset_y: 0.025,
            rotation: 0,
            color: [255; 3],
            shadow: true,
            image: String::new(),
            image_width: 0.2,
            target: None,
        }
    }
}

impl Watermark {
    /// `rotation` in clockwise quarter turns.
    fn quarter_turns(&self) -> u8 {
        match self.rotation {
            90 => 1,
            180 => 2,
            270 => 3,
            _ => 0,
        }
    }
}

/// Where the point `(x, y)` of a `w × h` box lands when the box is turned `turns` quarter turns
/// clockwise (for pixel coordinates: the turned box is `h × w` for odd turns).
fn turn(turns: u8, x: f32, y: f32, w: f32, h: f32) -> (f32, f32) {
    match turns {
        1 => (h - 1.0 - y, x),
        2 => (w - 1.0 - x, h - 1.0 - y),
        3 => (y, w - 1.0 - x),
        _ => (x, y),
    }
}

/// Draw `wm` onto `img` (straight alpha blending of the encoded values).
pub fn draw_watermark(img: &mut Rgba8, wm: &Watermark) {
    blend_watermark(img, wm, 1.0);
}

/// Draw `wm` onto `img`, a preview of an export whose short edge is `export_short_edge` pixels.
/// The smallest text size and shadow offset scale with the preview, so it looks like the file.
pub fn draw_watermark_preview(img: &mut Rgba8, wm: &Watermark, export_short_edge: usize) {
    let short = img.width.min(img.height);
    let scale = if export_short_edge == 0 { 1.0 } else { short as f32 / export_short_edge as f32 };
    blend_watermark(img, wm, scale);
}

fn blend_watermark(img: &mut Rgba8, wm: &Watermark, floor_scale: f32) {
    let width = img.width;
    watermark_coverage(img.width, img.height, wm, crate::fonts::CRAFT_FONTS, floor_scale, |x, y, k, col| {
        let p = &mut img.data[y * width + x];
        if p[3] == 255 {
            for c in 0..3 {
                p[c] = (p[c] as f32 + (col[c] as f32 - p[c] as f32) * k).round() as u8;
            }
            return;
        }
        // over a transparent pixel (an export that keeps transparency): the mark composites over
        // it and raises its alpha, so it shows where the photo is transparent
        let a = p[3] as f32 / 255.0;
        let out = k + a * (1.0 - k);
        if out <= 0.0 {
            return;
        }
        for c in 0..3 {
            p[c] = ((col[c] as f32 * k + p[c] as f32 * a * (1.0 - k)) / out).round().clamp(0.0, 255.0) as u8;
        }
        p[3] = (out * 255.0).round().clamp(0.0, 255.0) as u8;
    });
}

/// Draw `wm` into the `w × h` area of `img` whose top-left corner is (`x0`, `y0`), sized and
/// anchored as on an image of that size (a slideshow slide's photo). The area is clipped to `img`.
pub fn draw_watermark_in(img: &mut Rgba8, wm: &Watermark, x0: usize, y0: usize, w: usize, h: usize) {
    let (width, height) = (img.width, img.height);
    watermark_coverage(w, h, wm, crate::fonts::CRAFT_FONTS, 1.0, |x, y, k, col| {
        let (px, py) = (x0.saturating_add(x), y0.saturating_add(y));
        if px >= width || py >= height {
            return;
        }
        if let Some(p) = img.data.get_mut(py * width + px) {
            for c in 0..3 {
                p[c] = (p[c] as f32 + (col[c] as f32 - p[c] as f32) * k).round() as u8;
            }
        }
    });
}

/// Draw `wm` onto a high-bit-depth image (`wm.color` is in the image's space, 8-bit encoded; float
/// images are linear and blend in linear light).
pub fn draw_watermark_deep(img: &mut DeepImage, wm: &Watermark) {
    draw_watermark_deep_over(img, None, wm);
}

/// [`draw_watermark_deep`] onto an image with straight alpha `alpha` (its size): the mark
/// composites over transparent pixels and raises their alpha, as [`draw_watermark`] does.
pub(super) fn draw_watermark_deep_over(img: &mut DeepImage, mut alpha: Option<&mut Plane>, wm: &Watermark) {
    let (width, trc) = (img.width, img.space.trc());
    // over a transparent pixel `i`: the weights of the mark and of the pixel, its alpha raised
    // (`None`: an opaque pixel, blended as without alpha)
    let mut over = move |i: usize, k: f32| -> Option<(f32, f32)> {
        let a = alpha.as_deref_mut().and_then(|a| a.data.get_mut(i))?;
        let before = if a.is_finite() { a.clamp(0.0, 1.0) } else { 1.0 };
        if before >= 1.0 {
            return None;
        }
        let out = k + before * (1.0 - k);
        *a = out;
        Some(if out <= 0.0 { (0.0, 1.0) } else { (k / out, before * (1.0 - k) / out) })
    };
    match &mut img.samples {
        DeepSamples::U16(v) => watermark_coverage(img.width, img.height, wm, crate::fonts::CRAFT_FONTS, 1.0, |x, y, k, col| {
            let w = over(y * width + x, k);
            let i = (y * width + x) * 3;
            for c in 0..3 {
                let p = v[i + c] as f32;
                let t = col[c] as f32 * 257.0;
                let blended = match w {
                    Some((wk, wp)) => p * wp + t * wk,
                    None => p + (t - p) * k,
                };
                v[i + c] = blended.round().clamp(0.0, 65535.0) as u16;
            }
        }),
        DeepSamples::F32(v) => watermark_coverage(img.width, img.height, wm, crate::fonts::CRAFT_FONTS, 1.0, |x, y, k, col| {
            let w = over(y * width + x, k);
            let i = (y * width + x) * 3;
            for c in 0..3 {
                let target = trc.decode(col[c] as f32 / 255.0);
                v[i + c] = match w {
                    Some((wk, wp)) => v[i + c] * wp + target * wk,
                    None => v[i + c] + (target - v[i + c]) * k,
                };
            }
        }),
    }
}

/// Lay out `wm` on a `width × height` image and call `blend(x, y, coverage × opacity, colour)` for
/// every covered pixel (shadow pass first). `craft` is [`crate::fonts::CRAFT_FONTS`] (a parameter
/// so tests can render without it). `floor_scale` is this image's size over the exported file's
/// (1 when drawing the file itself): the 6 px minimum text size is a minimum in the file.
pub(super) fn watermark_coverage(
    width: usize,
    height: usize,
    wm: &Watermark,
    craft: &'static [crate::fonts::CraftFont],
    floor_scale: f32,
    mut blend_px: impl FnMut(usize, usize, f32, [u8; 3]),
) {
    if !wm.image.trim().is_empty() {
        logo_coverage(width, height, wm, blend_px);
        return;
    }
    let text = wm.text.trim();
    if text.is_empty() || width == 0 || height == 0 {
        return;
    }
    let short = width.min(height) as f32;
    let px = (wm.size.clamp(0.005, 0.5) * short).max(6.0 * floor_scale);
    let Some(layout) = crate::text::TextLayout::new(text, px, wm.vertical, craft) else { return };
    let (tw, th) = (layout.width, layout.height);
    // laid out unturned in a tw × th box, placed by the box it becomes
    let turns = wm.quarter_turns();
    let (bw, bh) = if turns % 2 == 1 { (th, tw) } else { (tw, th) };
    let (ix, iy) = (wm.inset_x.clamp(0.0, 0.4) * short, wm.inset_y.clamp(0.0, 0.4) * short);
    let (w, h) = (width as f32, height as f32);
    use Anchor::*;
    let ox = match wm.anchor {
        TopLeft | Left | BottomLeft => ix,
        Top | Center | Bottom => (w - bw) / 2.0,
        TopRight | Right | BottomRight => w - ix - bw,
    };
    let oy = match wm.anchor {
        TopLeft | Top | TopRight => iy,
        Left | Center | Right => (h - bh) / 2.0,
        BottomLeft | Bottom | BottomRight => h - iy - bh,
    };
    let alpha = wm.opacity.clamp(0.0, 1.0);
    let passes: &[(f32, [u8; 3], f32)] =
        if wm.shadow { &[((px * 0.05).max(floor_scale), [0, 0, 0], 0.45), (0.0, wm.color, 1.0)] } else { &[(0.0, wm.color, 1.0)] };
    for &(off, col, a) in passes {
        let mut put = |gx: i32, gy: i32, cov: f32| {
            if gx < 0 || gy < 0 || gx >= width as i32 || gy >= height as i32 {
                return;
            }
            blend_px(gx as usize, gy as usize, (cov * a * alpha).clamp(0.0, 1.0), col);
        };
        if turns == 0 {
            layout.draw_in(ox + off, oy + off, w, h, &mut put);
        } else {
            // turned with the box; the shadow still falls down and to the right
            layout.draw_points(|x, y, cov| {
                let (x, y) = turn(turns, x, y, tw, th);
                put((ox + off + x) as i32, (oy + off + y) as i32, cov);
            });
        }
    }
}

/// A watermark graphic, decoded once per file (and again when the file changes).
pub(crate) fn watermark_logo(path: &str) -> Option<std::sync::Arc<Rgba8>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    static CACHE: Mutex<Option<HashMap<String, (std::time::SystemTime, Arc<Rgba8>)>>> = Mutex::new(None);
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = c.get_or_insert_with(HashMap::new);
    if let Some((m, img)) = map.get(path)
        && *m == modified
    {
        return Some(img.clone());
    }
    let img = Arc::new(read_graphic(path).ok()?);
    map.insert(path.to_string(), (modified, img.clone()));
    Some(img)
}

/// A graphic file (a PNG with transparency, typically) as 8-bit sRGB with its alpha: the
/// watermark logo, the loupe's layout image. Applies EXIF orientation before either consumer sizes it.
/// The error says why it couldn't be read or decoded.
pub fn read_graphic(path: &str) -> Result<Rgba8, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("can't read {path}: {e}"))?;
    let decoded = lightcraft_codecs::decode(&bytes, Default::default()).map_err(|e| format!("can't decode {path}: {e}"))?;
    Ok(decoded.to_srgb8().into_oriented(lightcraft_geom::Orientation::from_exif(decoded.orientation)))
}

/// The graphic watermark: scaled (premultiplied, so transparent edges stay clean) to
/// `image_width` of the photo, placed like the text, blended with its alpha × opacity.
fn logo_coverage(width: usize, height: usize, wm: &Watermark, mut blend_px: impl FnMut(usize, usize, f32, [u8; 3])) {
    let Some(logo) = watermark_logo(wm.image.trim()) else {
        log::warn!("watermark: can't read {}", wm.image);
        return;
    };
    if width == 0 || height == 0 || logo.width == 0 || logo.height == 0 {
        return;
    }
    let turns = wm.quarter_turns();
    // Reserve the inset on anchored axes before fitting the turned box. Centered axes
    // ignore insets, just as placement below does. One scale preserves the graphic's shape.
    let short = width.min(height) as f32;
    let (ix, iy) = (wm.inset_x.clamp(0.0, 0.4) * short, wm.inset_y.clamp(0.0, 0.4) * short);
    use Anchor::*;
    let available_w = width as f64 - if matches!(wm.anchor, Top | Center | Bottom) { 0.0 } else { ix as f64 };
    let available_h = height as f64 - if matches!(wm.anchor, Left | Center | Right) { 0.0 } else { iy as f64 };
    let (max_w, max_h) = if turns % 2 == 1 { (available_h, available_w) } else { (available_w, available_h) };
    let scale =
        (wm.image_width.clamp(0.01, 1.0) as f64 * width as f64 / logo.width as f64).min(max_w / logo.width as f64).min(max_h / logo.height as f64);
    let lw = (logo.width as f64 * scale).round().max(1.0) as usize;
    let lh = (logo.height as f64 * scale).round().max(1.0) as usize;
    let pre: lightcraft_raster::Image<[f32; 4]> = lightcraft_raster::Image::from_fn(logo.width, logo.height, |x, y| {
        let p = logo.get(x, y);
        let a = p[3] as f32 / 255.0;
        [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, a]
    });
    let scaled = lightcraft_raster::resample::resize(&pre, lw, lh, lightcraft_raster::resample::Filter::Mitchell);
    let (bw, bh) = if turns % 2 == 1 { (lh, lw) } else { (lw, lh) };
    let (w, h, lwf, lhf) = (width as f32, height as f32, bw as f32, bh as f32);
    let ox = match wm.anchor {
        TopLeft | Left | BottomLeft => ix,
        Top | Center | Bottom => (w - lwf) / 2.0,
        TopRight | Right | BottomRight => w - ix - lwf,
    }
    .max(0.0) as usize;
    let oy = match wm.anchor {
        TopLeft | Top | TopRight => iy,
        Left | Center | Right => (h - lhf) / 2.0,
        BottomLeft | Bottom | BottomRight => h - iy - lhf,
    }
    .max(0.0) as usize;
    let space = wm.target.unwrap_or(OutputSpace::Srgb);
    let op = wm.opacity.clamp(0.0, 1.0);
    for y in 0..lh {
        for x in 0..lw {
            let [r, g, b, a] = scaled.get(x, y);
            let a = a.clamp(0.0, 1.0);
            let (tx, ty) = turn(turns, x as f32, y as f32, lw as f32, lh as f32);
            let (px, py) = (ox + tx as usize, oy + ty as usize);
            if a <= 1e-4 || px >= width || py >= height {
                continue;
            }
            let c = [r, g, b].map(|v| (v / a).round().clamp(0.0, 255.0) as u8);
            blend_px(px, py, a * op, srgb8_in(space, c));
        }
    }
}
