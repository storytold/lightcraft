//! Export: encode a rendered image to JPEG / PNG / TIFF / WebP / AVIF / JPEG XL (or copy the original / write a
//! DNG, each with the edits in XMP) in sRGB, Display P3, Adobe RGB
//! (1998) compatible, ProPhoto RGB or Rec. 2020 with an embedded ICC profile we generate from the
//! published primaries and curves, optional output sharpening and a JPEG file-size limit. Pure (bytes in, bytes out) so the desktop
//! app, CLI, MCP and the web build share it; writing the file is the caller's job.

use lightcraft_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, JxlColour, JxlOptions, NamedSpace, Samples, encode, icc};
pub use lightcraft_codecs::{JxlEffort, TiffCompression};
use lightcraft_meta::{DateTime, Gps, Metadata};
pub use lightcraft_pipeline::{DeepImage, DeepSamples, OutputDepth, OutputSpace};
use lightcraft_raster::Rgba8;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    #[default]
    Jpeg,
    Png,
    Tiff,
    /// A flat PSD (8/16-bit, no layers): what an image editor such as PhotoCraft saves back to.
    Psd,
    Webp,
    Avif,
    /// JPEG XL: lossy (VarDCT) or lossless, 8/16-bit, HDR as Rec. 2020 PQ.
    Jxl,
    /// The original file, unchanged, with an XMP sidecar holding the edits.
    Original,
    /// A DNG of the raw data (raw photos only), the edits embedded as XMP.
    Dng,
    /// Render to DNG: a linear DNG (16-bit float) of the rendered photo, the edit baked in.
    RenderedDng,
}

impl ExportFormat {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "jpeg" | "jpg" => Self::Jpeg,
            "png" => Self::Png,
            "tiff" | "tif" => Self::Tiff,
            "psd" => Self::Psd,
            "webp" => Self::Webp,
            "avif" => Self::Avif,
            "jxl" | "jpegxl" | "jpeg-xl" | "jpeg_xl" => Self::Jxl,
            "original" => Self::Original,
            "dng" => Self::Dng,
            "rendereddng" | "renderdng" | "render-dng" | "lineardng" => Self::RenderedDng,
            _ => return None,
        })
    }
    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Tiff => "tif",
            Self::Psd => "psd",
            Self::Webp => "webp",
            Self::Avif => "avif",
            Self::Jxl => "jxl",
            // the original keeps its own extension (see `ExportOptions::file_name_for`)
            Self::Original => "",
            Self::Dng | Self::RenderedDng => "dng",
        }
    }

    /// Formats that render pixels (everything but `Original` and `Dng`).
    pub fn is_rendered(self) -> bool {
        !matches!(self, Self::Original | Self::Dng)
    }
}

/// How the output size is chosen (see [`Resize`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResizeMode {
    /// `value` = long edge in pixels.
    #[default]
    LongEdge,
    /// `value` = short edge in pixels.
    ShortEdge,
    /// `value` = width in pixels.
    Width,
    /// `value` = height in pixels.
    Height,
    /// Fit inside `value` wide × `height` tall as the photo stands: width ≤ `value`, height ≤
    /// `height` (Lightroom's "Width & Height").
    WidthHeight,
    /// Fit inside `value × height` pixels, whichever way round the photo is (long edge ≤ the
    /// larger; Lightroom's "Dimensions").
    Dimensions,
    /// `value` = megapixels.
    Megapixels,
    /// `value` = percent of the full size.
    Percent,
}

/// Output size: a mode, its value(s) and whether smaller photos are left at their own size.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Resize {
    pub mode: ResizeMode,
    pub value: f32,
    /// Second dimension for [`ResizeMode::Dimensions`] and [`ResizeMode::WidthHeight`].
    pub height: u32,
    /// Never upscale past the photo's own (cropped) size.
    pub dont_enlarge: bool,
}

impl Default for Resize {
    fn default() -> Self {
        Self { mode: ResizeMode::LongEdge, value: 2048.0, height: 2048, dont_enlarge: true }
    }
}

/// The most pixels an export enlarged past its photo's own size can have: 512 megapixels, also
/// Lightroom's ceiling. A full-size or smaller export is never held to it.
pub const MAX_ENLARGED_PIXELS: f64 = 512e6;

impl Resize {
    pub fn long_edge(px: u32) -> Self {
        Self { value: px as f32, ..Default::default() }
    }

    /// Scale factor from a `w × h` (cropped, full-resolution) photo to the output.
    pub fn scale(&self, w: f64, h: f64) -> f64 {
        let (w, h) = (w.max(1.0), h.max(1.0));
        let (long, short) = (w.max(h), w.min(h));
        let v = (self.value as f64).max(0.0);
        let k = match self.mode {
            ResizeMode::LongEdge => v / long,
            ResizeMode::ShortEdge => v / short,
            ResizeMode::Width => v / w,
            ResizeMode::Height => v / h,
            ResizeMode::WidthHeight => (v / w).min(self.height as f64 / h),
            ResizeMode::Dimensions => {
                let (a, b) = (v, self.height as f64);
                (a.max(b) / long).min(a.min(b) / short)
            }
            ResizeMode::Megapixels => (v * 1e6 / (w * h)).sqrt(),
            ResizeMode::Percent => v / 100.0,
        };
        let k = if self.dont_enlarge { k.min(1.0) } else { k };
        // an enlargement stops at 512 megapixels (Lightroom's ceiling too): 20000 typed for 2000
        // with Don't Enlarge off asked for gigapixels, more memory than a computer has
        let k = k.min((MAX_ENLARGED_PIXELS / (w * h)).sqrt().max(1.0));
        // at least one pixel, at most the encoders' 65535 limit; the limit wins for a photo more
        // than 65535 times as long as it is short (where `clamp` would panic: min above max)
        k.max(1.0 / short).min(65_535.0 / long)
    }

    /// Output size for a `w × h` (cropped, full-resolution) photo.
    pub fn apply(&self, w: f64, h: f64) -> (usize, usize) {
        let k = self.scale(w, h);
        (((w * k).round() as usize).max(1), ((h * k).round() as usize).max(1))
    }
}

/// What happens when an output file already exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Conflict {
    /// Add `-2`, `-3`, … to the name.
    #[default]
    Unique,
    Overwrite,
    /// Leave the existing file and skip the photo.
    Skip,
}

/// Where a batch's files go (Lightroom Classic's Export To).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DestinationMode {
    /// One folder for the whole batch ([`Destination::dir`]).
    #[default]
    SpecificFolder,
    /// Each photo's own folder: the folder its original is in. A photo without one on disk (a
    /// demo photo, a photo in the browser's storage) or whose folder is gone gets an error.
    SameAsOriginal,
}

impl DestinationMode {
    /// `specificFolder` / `sameAsOriginal` (any case).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "specificfolder" => Some(Self::SpecificFolder),
            "sameasoriginal" => Some(Self::SameAsOriginal),
            _ => None,
        }
    }
}

impl Conflict {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "unique" => Some(Self::Unique),
            "overwrite" => Some(Self::Overwrite),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }
}

/// A named set of `app.export` params.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportPreset {
    pub name: String,
    pub params: serde_json::Value,
}

/// The built-in export presets (File → Export with Preset).
pub fn builtin_presets() -> Vec<ExportPreset> {
    let p = |name: &str, params: serde_json::Value| ExportPreset { name: name.into(), params };
    vec![
        p("JPEG (Small)", serde_json::json!({"format": "jpeg", "quality": 85, "longEdge": 2048, "colorSpace": "srgb"})),
        p("JPEG (Large)", serde_json::json!({"format": "jpeg", "quality": 92, "longEdge": 0, "colorSpace": "srgb"})),
        p("Original + Settings", serde_json::json!({"format": "original"})),
        p("DNG", serde_json::json!({"format": "dng"})),
    ]
}

/// Output sharpening target (applied after resizing, in display-encoded values).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SharpenFor {
    #[default]
    None,
    Screen,
    Matte,
    Glossy,
}

impl SharpenFor {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "screen" => Some(Self::Screen),
            "matte" => Some(Self::Matte),
            "glossy" => Some(Self::Glossy),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SharpenAmount {
    Low,
    #[default]
    Standard,
    High,
}

impl SharpenAmount {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "standard" => Some(Self::Standard),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

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

#[cfg(test)]
use crate::text::{TEXT_FONT as WATERMARK_FONT, cell_glyphs as watermark_cell_glyphs, faces as watermark_fonts};

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
        for c in 0..3 {
            p[c] = (p[c] as f32 + (col[c] as f32 - p[c] as f32) * k).round() as u8;
        }
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
    let (width, trc) = (img.width, img.space.trc());
    match &mut img.samples {
        DeepSamples::U16(v) => watermark_coverage(img.width, img.height, wm, crate::fonts::CRAFT_FONTS, 1.0, |x, y, k, col| {
            let i = (y * width + x) * 3;
            for c in 0..3 {
                let p = v[i + c] as f32;
                v[i + c] = (p + (col[c] as f32 * 257.0 - p) * k).round().clamp(0.0, 65535.0) as u16;
            }
        }),
        DeepSamples::F32(v) => watermark_coverage(img.width, img.height, wm, crate::fonts::CRAFT_FONTS, 1.0, |x, y, k, col| {
            let i = (y * width + x) * 3;
            for c in 0..3 {
                let target = trc.decode(col[c] as f32 / 255.0);
                v[i + c] += (target - v[i + c]) * k;
            }
        }),
    }
}

/// Lay out `wm` on a `width × height` image and call `blend(x, y, coverage × opacity, colour)` for
/// every covered pixel (shadow pass first). `craft` is [`crate::fonts::CRAFT_FONTS`] (a parameter
/// so tests can render without it). `floor_scale` is this image's size over the exported file's
/// (1 when drawing the file itself): the 6 px minimum text size is a minimum in the file.
fn watermark_coverage(
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

/// Which metadata is embedded in exported files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MetadataPolicy {
    /// Everything we know: camera, capture settings, lens, location, title, caption, keywords, copyright.
    #[default]
    All,
    /// Everything except camera/lens make, model and capture settings.
    AllExceptCamera,
    /// Copyright and creator only.
    Copyright,
    /// Nothing (the sRGB profile is still embedded).
    None,
}

impl MetadataPolicy {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "all" => Some(Self::All),
            "allexceptcamera" => Some(Self::AllExceptCamera),
            "copyright" => Some(Self::Copyright),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// Transfer curve of an HDR AVIF export (`hdrTransfer`): PQ (absolute, SDR white at 203 cd/m²) or
/// HLG (relative to the display; BT.2100 with SDR white at signal 0.75).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HdrCurve {
    #[default]
    Pq,
    Hlg,
}

impl HdrCurve {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "pq" => Some(Self::Pq),
            "hlg" => Some(Self::Hlg),
            _ => None,
        }
    }
    fn codec(self) -> lightcraft_codecs::HdrTransfer {
        match self {
            Self::Pq => lightcraft_codecs::HdrTransfer::Pq,
            Self::Hlg => lightcraft_codecs::HdrTransfer::Hlg,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExportOptions {
    pub format: ExportFormat,
    /// 1–100 (JPEG, AVIF, lossy JPEG XL).
    pub quality: u8,
    /// Output size (`None` = full size).
    pub resize: Option<Resize>,
    /// Print resolution written into the file (pixels per inch).
    pub ppi: u16,
    /// JPEG only: largest quality whose file fits in this many KB.
    pub limit_kb: Option<u32>,
    pub sharpen: SharpenFor,
    pub sharpen_amount: SharpenAmount,
    /// File name template with the batch-rename tokens ([`crate::rename::expand_tokens`]): `{name}`
    /// (original stem), `{seq}` (zero-padded to 3, counting from `start_number`; `{seq:N}` for N
    /// digits), `{date}` (capture date, YYYYMMDD; `{date:%Y-%m-%d}`…), `{num}`, `{folder}`,
    /// `{camera}`, `{lens}`, `{iso}`, `{rating}`, `{title}`, `{creator}`, `{ext}`; the output
    /// extension is appended.
    pub naming: String,
    /// First `{seq}` value.
    pub start_number: u32,
    /// A specific folder for the whole batch, or each photo's own folder.
    pub destination_mode: DestinationMode,
    /// Created inside the destination folder (empty = none).
    pub subfolder: String,
    /// When a file of the same name exists in the destination folder.
    pub conflict: Conflict,
    /// TIFF compression.
    pub tiff_compression: TiffCompression,
    /// DNG export: how the raw data is stored.
    pub dng_compression: DngCompression,
    pub metadata: MetadataPolicy,
    /// Strip GPS / location even when the policy would include it.
    pub remove_location: bool,
    /// Text watermark (none when absent or the text is empty).
    pub watermark: Option<Watermark>,
    /// Output colour space (AVIF is always sRGB: its muxer cannot embed a profile; lossy JPEG XL
    /// writes Adobe RGB and ProPhoto as Rec. 2020, see [`ExportOptions::effective_space`]).
    pub color_space: OutputSpace,
    /// Bits per channel: 8, 16 (PNG, TIFF, JPEG XL), 32 (TIFF: float, linear) or 10 (AVIF); `None` = the
    /// format's default (TIFF 16, everything else 8). See [`ExportOptions::effective_depth`].
    pub bit_depth: Option<u8>,
    /// HDR output for photos edited in HDR: JPEG as an ISO 21496-1 gain map JPEG (the SDR
    /// rendition plus a gain map, so the file looks right everywhere; `limit_kb` includes both
    /// images and their metadata), AVIF as 10-bit Rec. 2020 PQ, PNG as 16-bit Rec. 2020 PQ (`cICP`; viewers
    /// without cICP support show it as sRGB, washed out), JPEG XL as 16-bit Rec. 2020 PQ, 32-bit float
    /// TIFF with the highlights above SDR white kept. Other formats, and photos without an HDR
    /// edit, are SDR.
    pub hdr: bool,
    /// JPEG XL: mathematically lossless instead of lossy (`quality` is then unused).
    pub jxl_lossless: bool,
    /// JPEG XL: encoder effort (time spent for a smaller file).
    pub jxl_effort: JxlEffort,
    /// JPEG XL: progressive file (a coarse image first, refined as it loads).
    pub jxl_progressive: bool,
    /// After the export, add the files written to Apple Photos (macOS only; see [`AfterExport`]).
    pub add_to_photos: bool,
    /// The Photos album they go into, made when missing (empty = none).
    pub photos_album: String,
    /// The curve of an HDR AVIF: PQ (default) or HLG.
    pub hdr_transfer: HdrCurve,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            format: ExportFormat::Jpeg,
            quality: 90,
            resize: None,
            ppi: 240,
            limit_kb: None,
            sharpen: SharpenFor::None,
            sharpen_amount: SharpenAmount::Standard,
            naming: "{name}".into(),
            start_number: 1,
            destination_mode: DestinationMode::SpecificFolder,
            subfolder: String::new(),
            conflict: Conflict::Unique,
            tiff_compression: TiffCompression::Deflate,
            dng_compression: DngCompression::Lossless,
            metadata: MetadataPolicy::All,
            remove_location: false,
            watermark: None,
            color_space: OutputSpace::Srgb,
            bit_depth: None,
            hdr: false,
            jxl_lossless: false,
            jxl_effort: JxlEffort::Normal,
            jxl_progressive: false,
            add_to_photos: false,
            photos_album: String::new(),
            hdr_transfer: HdrCurve::Pq,
        }
    }
}

/// The `app.export` params that are not export options: which photos, where to, a preset to start
/// from, and the desktop app's background flag.
pub const TARGET_PARAMS: &[&str] = &["id", "ids", "path", "dir", "preset", "background"];

/// The export option params [`ExportOptions::from_json`] reads (and [`ExportOptions::to_json`] writes).
pub const OPTION_PARAMS: &[&str] = &[
    "format",
    "quality",
    "resize",
    "longEdge",
    "shortEdge",
    "width",
    "height",
    "megapixels",
    "percent",
    "fit",
    "dontEnlarge",
    "ppi",
    "limitKb",
    "sharpen",
    "sharpenAmount",
    "naming",
    "startNumber",
    "destinationMode",
    "subfolder",
    "conflict",
    "tiffCompression",
    "dngCompression",
    "metadata",
    "removeLocation",
    "watermark",
    "colorSpace",
    "bitDepth",
    "hdr",
    "jxlLossless",
    "jxlEffort",
    "jxlProgressive",
    "addToPhotos",
    "photosAlbum",
    "hdrTransfer",
];

/// Why an export can't add its files to Apple Photos on this platform (`None`: it can, when the
/// session has the runner; see [`crate::Session::after_export`]).
pub fn apple_photos_unsupported() -> Option<&'static str> {
    (!cfg!(target_os = "macos")).then_some("Apple Photos is only available on macOS")
}

/// The keys of a `watermark` object ([`Watermark`], camelCase). `inset` sets `insetX` and `insetY`
/// together (what versions before the two margins wrote); either of those, when given, wins.
pub const WATERMARK_PARAMS: &[&str] =
    &["text", "vertical", "size", "opacity", "anchor", "insetX", "insetY", "inset", "rotation", "color", "shadow", "image", "imageWidth"];

/// Text height as a fraction of the short edge: the range the renderer draws without clamping.
pub const WATERMARK_SIZE_RANGE: (f32, f32) = (0.005, 0.5);
/// Margin as a fraction of the short edge.
pub const WATERMARK_INSET_RANGE: (f32, f32) = (0.0, 0.4);
/// A graphic's width as a fraction of the photo's width.
pub const WATERMARK_IMAGE_WIDTH_RANGE: (f32, f32) = (0.01, 1.0);

/// Levenshtein distance, for "did you mean" on a misspelled key (both inputs are short).
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev.get(j).copied().unwrap_or(0) + usize::from(ca != cb);
            let del = prev.get(j + 1).copied().unwrap_or(0) + 1;
            let ins = cur.get(j).copied().unwrap_or(0) + 1;
            if let Some(c) = cur.get_mut(j + 1) {
                *c = sub.min(del).min(ins);
            }
        }
        prev = cur;
    }
    prev.last().copied().unwrap_or(0)
}

/// `unknown parameter \`k\``, naming the closest known key when one is near.
fn unknown_key(what: &str, k: &str, known: &[&str]) -> String {
    let near = known.iter().map(|n| (edit_distance(&k.to_ascii_lowercase(), &n.to_ascii_lowercase()), *n)).filter(|(d, _)| *d <= 2).min();
    match near {
        Some((_, n)) => format!("unknown {what} `{k}` (did you mean `{n}`?)"),
        None => format!("unknown {what} `{k}` (one of {})", known.join(", ")),
    }
}

/// A JSON integer (`92`; never `92.5` or `"92"`), as [`ExportOptions::from_json`] reads them.
fn whole(v: &serde_json::Value) -> Option<i64> {
    v.as_i64()
}

/// A `watermark` object as a [`Watermark`]; a legacy `inset` fills whichever of `insetX` and
/// `insetY` is missing.
fn watermark_from_json(v: &serde_json::Value) -> serde_json::Result<Watermark> {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut()
        && let Some(inset) = o.remove("inset")
    {
        o.entry("insetX").or_insert_with(|| inset.clone());
        o.entry("insetY").or_insert(inset);
    }
    serde_json::from_value(v)
}

impl ExportOptions {
    /// Stored export settings (a saved preset, `prefs.json ▸ lastExport`) with the keys an older or
    /// newer version may have written and this one doesn't know dropped, also inside `watermark`.
    /// [`ExportOptions::validate`] is strict about what a caller types; settings saved on disk are
    /// not the caller's typo, and refusing them would make a preset or Export with Previous fail
    /// for good after an update. A `bitDepth` the stored format doesn't have (written at the
    /// format's own depth before issue #1952) is dropped too.
    pub fn known_keys_only(p: &serde_json::Value) -> serde_json::Value {
        let mut p = Self::known_keys(p);
        Self::drop_unfit_depth(&mut p, ExportFormat::default());
        p
    }

    /// [`ExportOptions::known_keys_only`] keeping `bitDepth`, for a preset whose format may only be
    /// known once the call's own params (`path`) are merged in.
    pub(crate) fn known_keys(p: &serde_json::Value) -> serde_json::Value {
        let mut p = p.clone();
        if let Some(o) = p.as_object_mut() {
            o.retain(|k, _| OPTION_PARAMS.contains(&k.as_str()) || TARGET_PARAMS.contains(&k.as_str()));
            if let Some(w) = o.get_mut("watermark").and_then(serde_json::Value::as_object_mut) {
                w.retain(|k, _| WATERMARK_PARAMS.contains(&k.as_str()));
            }
        }
        p
    }

    /// Drop a stored `bitDepth` the format `p` exports ([`ExportOptions::export_format`] with
    /// `fallback`) doesn't have: it was written at the format's own depth before issue #1952.
    pub(crate) fn drop_unfit_depth(p: &mut serde_json::Value, fallback: ExportFormat) {
        let format = Self::export_format(p, fallback);
        if let Some(o) = p.as_object_mut()
            && let Some(depth) = o.get("bitDepth").and_then(whole)
            && !Self::bit_depths(format).iter().any(|d| i64::from(d.0) == depth)
        {
            o.remove("bitDepth");
        }
    }

    /// Strict [`ExportOptions::from_json`] for a command that is about to export (issue #181): an
    /// unknown parameter or a value that cannot be read as what it is for is an error, never a
    /// silent default. `app.export` targets (`id`, `ids`, `path`, `dir`, `preset`, `background`)
    /// are accepted and left to the caller.
    pub fn from_params(p: &serde_json::Value) -> crate::Result<Self> {
        Self::from_params_or(p, ExportFormat::default())
    }

    /// [`ExportOptions::from_params`] for a caller that writes `fallback` to a `path` naming no
    /// format; the format returned is [`ExportOptions::export_format`], the one written.
    pub fn from_params_or(p: &serde_json::Value, fallback: ExportFormat) -> crate::Result<Self> {
        let format = Self::export_format(p, fallback);
        Self::validate_as("app.export", p, Some(format))?;
        Ok(Self { format, ..Self::from_json(p) })
    }

    /// The format `app.export` writes: `format` (`null`: the default), else the one the extension of
    /// `path` names, else `fallback` for a path that names none (the desktop app writes JPEG there,
    /// the headless backend PNG), else the default.
    pub fn export_format(p: &serde_json::Value, fallback: ExportFormat) -> ExportFormat {
        use serde_json::Value;
        match (p.get("format"), p.get("path").and_then(Value::as_str)) {
            (Some(f), _) => f.as_str().and_then(ExportFormat::parse).unwrap_or_default(),
            (None, Some(path)) => std::path::Path::new(path).extension().and_then(|e| e.to_str()).and_then(ExportFormat::parse).unwrap_or(fallback),
            (None, None) => ExportFormat::default(),
        }
    }

    /// Check `app.export` params for `cmd` (the command named in the error): every key must be an
    /// export option ([`OPTION_PARAMS`]) or a target ([`TARGET_PARAMS`]), every value must be the
    /// kind of thing its key takes, and a `watermark` object must hold only [`WATERMARK_PARAMS`]
    /// with its sizes in range (issue #183). `null` means "not given", as a dropped key does.
    pub fn validate(cmd: &str, p: &serde_json::Value) -> crate::Result<()> {
        Self::validate_as(cmd, p, Some(Self::export_format(p, ExportFormat::default())))
    }

    /// [`ExportOptions::validate`] with `bitDepth` checked against `format`, the one exported
    /// (`None`: not known yet, as for a preset without `format`).
    pub(crate) fn validate_as(cmd: &str, p: &serde_json::Value, format: Option<ExportFormat>) -> crate::Result<()> {
        use serde_json::Value;
        let bad = |msg: String| crate::cmd::bad(cmd, msg);
        let Some(obj) = p.as_object() else { return Err(bad("params must be an object".into())) };
        let known: Vec<&str> = OPTION_PARAMS.iter().chain(TARGET_PARAMS).copied().collect();
        let int = |k: &str, v: &Value, lo: i64, hi: i64| -> crate::Result<i64> {
            whole(v).filter(|i| (lo..=hi).contains(i)).ok_or_else(|| bad(format!("`{k}` must be an integer {lo}..{hi}")))
        };
        let num = |k: &str, v: &Value, lo: f64, hi: f64| -> crate::Result<f64> {
            v.as_f64().filter(|f| f.is_finite() && (lo..=hi).contains(f)).ok_or_else(|| bad(format!("`{k}` must be a number {lo}..{hi}")))
        };
        fn string<'a>(cmd: &str, k: &str, v: &'a Value) -> crate::Result<&'a str> {
            v.as_str().ok_or_else(|| crate::cmd::bad(cmd, format!("`{k}` must be a string")))
        }
        let boolean = |k: &str, v: &Value| -> crate::Result<bool> { v.as_bool().ok_or_else(|| bad(format!("`{k}` must be true or false"))) };
        let one_of = |k: &str, v: &Value, options: &[&str]| -> crate::Result<()> {
            let s = string(cmd, k, v)?;
            if options.iter().any(|o| o.eq_ignore_ascii_case(s)) { Ok(()) } else { Err(bad(format!("`{k}` must be one of {}", options.join("|")))) }
        };
        // a choice `from_json` reads with the same `parse`, so what passes here is what gets exported
        let parses = |k: &str, v: &Value, parse: fn(&str) -> bool, options: &[&str]| -> crate::Result<()> {
            if parse(string(cmd, k, v)?) { Ok(()) } else { Err(bad(format!("`{k}` must be one of {}", options.join("|")))) }
        };
        for (k, v) in obj {
            if v.is_null() {
                continue;
            }
            match k.as_str() {
                "format" => {
                    if ExportFormat::parse(string(cmd, k, v)?).is_none() {
                        return Err(bad(format!("`{k}` must be one of jpeg|png|tiff|webp|avif|jxl|dng|original")));
                    }
                }
                "quality" => {
                    int(k, v, 1, 100)?;
                }
                "ppi" => {
                    int(k, v, 1, u16::MAX as i64)?;
                }
                "limitKb" => {
                    int(k, v, 0, u32::MAX as i64)?;
                }
                "startNumber" => {
                    int(k, v, 0, 999_999)?;
                }
                "bitDepth" => {
                    if !matches!(whole(v), Some(8 | 10 | 16 | 32)) {
                        return Err(bad(format!("`{k}` must be 8, 10, 16 or 32")));
                    }
                }
                "longEdge" | "shortEdge" | "width" | "height" | "megapixels" | "percent" => {
                    num(k, v, 0.0, 1.0e9)?;
                }
                "fit" => one_of(k, v, &["dimensions", "widthHeight"])?,
                "resize" => {
                    let Some(r) = v.as_object() else { return Err(bad(format!("`{k}` must be an object {{mode, value, height?, dontEnlarge?}}"))) };
                    if let Some(x) = r.keys().find(|x| !matches!(x.as_str(), "mode" | "value" | "height" | "dontEnlarge")) {
                        return Err(bad(unknown_key("resize key", x, &["mode", "value", "height", "dontEnlarge"])));
                    }
                    serde_json::from_value::<Resize>(v.clone()).map_err(|e| bad(format!("`{k}`: {e}")))?;
                }
                "dontEnlarge" | "removeLocation" | "background" | "hdr" | "jxlLossless" | "jxlProgressive" => {
                    boolean(k, v)?;
                }
                "addToPhotos" => {
                    if boolean(k, v)?
                        && let Some(why) = apple_photos_unsupported()
                    {
                        return Err(bad(format!("`{k}`: {why}")));
                    }
                }
                "photosAlbum" => {
                    if string(cmd, k, v)?.contains('\0') {
                        return Err(bad(format!("`{k}` can't contain a NUL character")));
                    }
                }
                "naming" | "subfolder" | "path" | "dir" | "preset" => {
                    string(cmd, k, v)?;
                }
                "sharpen" => parses(k, v, |s| SharpenFor::parse(s).is_some(), &["none", "screen", "matte", "glossy"])?,
                "sharpenAmount" => parses(k, v, |s| SharpenAmount::parse(s).is_some(), &["low", "standard", "high"])?,
                "hdrTransfer" => one_of(k, v, &["pq", "hlg"])?,
                "conflict" => parses(k, v, |s| Conflict::parse(s).is_some(), &["unique", "overwrite", "skip"])?,
                "destinationMode" => one_of(k, v, &["specificFolder", "sameAsOriginal"])?,
                "tiffCompression" => one_of(k, v, &["none", "lzw", "zip", "deflate"])?,
                "jxlEffort" => one_of(k, v, &["fast", "normal"])?,
                "dngCompression" => {
                    serde_json::from_value::<DngCompression>(v.clone())
                        .map_err(|_| bad(format!("`{k}` must be one of lossless|deflate|uncompressed|lossy")))?;
                }
                "metadata" => parses(k, v, |s| MetadataPolicy::parse(s).is_some(), &["all", "allExceptCamera", "copyright", "none"])?,
                "colorSpace" => {
                    if OutputSpace::parse(string(cmd, k, v)?).is_none() {
                        return Err(bad(format!("`{k}` must be one of srgb|displayP3|adobeRgb|proPhoto|rec2020")));
                    }
                }
                "watermark" => Self::validate_watermark(cmd, v)?,
                "id" => {
                    int(k, v, 0, i64::MAX)?;
                }
                "ids" => {
                    let ok = v.as_array().is_some_and(|a| a.iter().all(|x| x.is_u64()));
                    if !ok {
                        return Err(bad(format!("`{k}` must be an array of photo ids")));
                    }
                }
                _ => return Err(bad(unknown_key("parameter", k, &known))),
            }
        }
        // issue #1952: a depth the format doesn't have would be written at the format's own depth
        if let (Some(format), Some(depth)) = (format, obj.get("bitDepth").and_then(whole)) {
            let mut depths: Vec<u8> = Self::bit_depths(format).iter().map(|d| d.0).collect();
            if !depths.iter().any(|d| i64::from(*d) == depth) {
                depths.sort_unstable();
                let mut list: Vec<String> = depths.iter().map(u8::to_string).collect();
                let last = list.pop().unwrap_or_default();
                let list = if list.is_empty() { last } else { format!("{} or {last}", list.join(", ")) };
                let name = serde_json::to_value(format).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
                return Err(bad(format!("`bitDepth` must be {list} for {name}")));
            }
        }
        Ok(())
    }

    /// A `watermark` value: text, or an object of [`WATERMARK_PARAMS`] (issue #183).
    pub(crate) fn validate_watermark(cmd: &str, v: &serde_json::Value) -> crate::Result<()> {
        use serde_json::Value;
        let bad = |msg: String| crate::cmd::bad(cmd, msg);
        let obj = match v {
            Value::String(_) => return Ok(()),
            Value::Object(o) => o,
            _ => {
                return Err(bad(
                    "`watermark` must be text or an object {text, vertical, size, opacity, anchor, insetX, insetY, inset, rotation, color, shadow, image, imageWidth}".into(),
                ));
            }
        };
        let frac = |k: &str, v: &Value, (lo, hi): (f32, f32), unit: &str| -> crate::Result<()> {
            match v.as_f64() {
                Some(f) if f.is_finite() && (f64::from(lo)..=f64::from(hi)).contains(&f) => Ok(()),
                _ => Err(bad(format!("`watermark.{k}` must be a number {lo}..{hi} ({unit})"))),
            }
        };
        for (k, v) in obj {
            if v.is_null() {
                continue;
            }
            match k.as_str() {
                "text" | "image" => {
                    v.as_str().ok_or_else(|| bad(format!("`watermark.{k}` must be a string")))?;
                }
                "vertical" | "shadow" => {
                    v.as_bool().ok_or_else(|| bad(format!("`watermark.{k}` must be true or false")))?;
                }
                "size" => frac(k, v, WATERMARK_SIZE_RANGE, "fraction of the short edge")?,
                "opacity" => frac(k, v, (0.0, 1.0), "0 = invisible, 1 = solid")?,
                "insetX" | "insetY" | "inset" => frac(k, v, WATERMARK_INSET_RANGE, "fraction of the short edge")?,
                "imageWidth" => frac(k, v, WATERMARK_IMAGE_WIDTH_RANGE, "fraction of the photo's width")?,
                "rotation" => {
                    if !matches!(whole(v), Some(0 | 90 | 180 | 270)) {
                        return Err(bad("`watermark.rotation` must be 0, 90, 180 or 270 (degrees clockwise)".into()));
                    }
                }
                "anchor" => {
                    serde_json::from_value::<Anchor>(v.clone()).map_err(|_| {
                        bad("`watermark.anchor` must be one of topLeft|top|topRight|left|center|right|bottomLeft|bottom|bottomRight".into())
                    })?;
                }
                "color" => {
                    let ok = v.as_array().is_some_and(|a| a.len() == 3 && a.iter().all(|c| matches!(whole(c), Some(0..=255))));
                    if !ok {
                        return Err(bad("`watermark.color` must be [r, g, b] with 0..255 each (sRGB)".into()));
                    }
                }
                _ => return Err(bad(unknown_key("watermark key", k, WATERMARK_PARAMS))),
            }
        }
        watermark_from_json(v).map(|_| ()).map_err(|e| bad(format!("`watermark`: {e}")))
    }

    /// Read options from command params (`format`, `quality`, `limitKb`, `sharpen`, `sharpenAmount`,
    /// `naming`, `metadata`, `removeLocation`, `watermark`, `colorSpace`, `bitDepth`, `ppi`,
    /// `addToPhotos`, `photosAlbum`) and the
    /// size: one of `longEdge`, `shortEdge`, `width`, `height` (both = fit inside W × H),
    /// `megapixels`, `percent` (absent or 0 = full size), or a [`Resize`] object as `resize`; plus
    /// `dontEnlarge` (default true).
    ///
    /// Lenient: anything unknown or unreadable falls back to the default, which suits stored
    /// prefs and dialog prefill. A command that is about to export uses
    /// [`ExportOptions::from_params`], which validates first.
    pub fn from_json(p: &serde_json::Value) -> Self {
        use serde_json::Value;
        let d = Self::default();
        let s = |k: &str| p.get(k).and_then(Value::as_str);
        let u = |k: &str| p.get(k).and_then(Value::as_u64);
        fn enm<T: serde::de::DeserializeOwned>(p: &Value, k: &str) -> Option<T> {
            p.get(k).cloned().and_then(|v| serde_json::from_value(v).ok())
        }
        Self {
            format: s("format").and_then(ExportFormat::parse).unwrap_or(d.format),
            quality: u("quality").map_or(d.quality, |q| q.clamp(1, 100) as u8),
            resize: Self::resize_from_json(p),
            ppi: u("ppi").filter(|v| *v > 0).map_or(d.ppi, |v| v.min(u16::MAX as u64) as u16),
            limit_kb: u("limitKb").filter(|v| *v > 0).map(|v| v.min(u32::MAX as u64) as u32),
            sharpen: s("sharpen").and_then(SharpenFor::parse).unwrap_or(d.sharpen),
            sharpen_amount: s("sharpenAmount").and_then(SharpenAmount::parse).unwrap_or(d.sharpen_amount),
            naming: s("naming").map_or(d.naming, str::to_string),
            start_number: u("startNumber").map_or(d.start_number, |v| v.min(999_999) as u32),
            destination_mode: s("destinationMode").and_then(DestinationMode::parse).unwrap_or(d.destination_mode),
            subfolder: s("subfolder").map_or(d.subfolder, |v| v.trim().replace(['\\', ':', '\0'], "_")),
            conflict: s("conflict").and_then(Conflict::parse).unwrap_or(d.conflict),
            tiff_compression: match s("tiffCompression").map(str::to_ascii_lowercase).as_deref() {
                Some("none") => TiffCompression::None,
                Some("lzw") => TiffCompression::Lzw,
                Some("zip" | "deflate") => TiffCompression::Deflate,
                _ => d.tiff_compression,
            },
            dng_compression: enm(p, "dngCompression").unwrap_or(d.dng_compression),
            metadata: s("metadata").and_then(MetadataPolicy::parse).unwrap_or(d.metadata),
            remove_location: p.get("removeLocation").and_then(Value::as_bool).unwrap_or(d.remove_location),
            watermark: match p.get("watermark") {
                Some(Value::String(t)) => Some(Watermark { text: t.clone(), ..Default::default() }),
                Some(v @ Value::Object(_)) => watermark_from_json(v).ok(),
                _ => None,
            }
            .filter(|w: &Watermark| !w.text.trim().is_empty() || !w.image.trim().is_empty()),
            color_space: s("colorSpace").and_then(OutputSpace::parse).unwrap_or(d.color_space),
            bit_depth: u("bitDepth").filter(|b| matches!(b, 8 | 10 | 16 | 32)).map(|b| b as u8),
            hdr: p.get("hdr").and_then(Value::as_bool).unwrap_or(d.hdr),
            jxl_lossless: p.get("jxlLossless").and_then(Value::as_bool).unwrap_or(d.jxl_lossless),
            jxl_effort: s("jxlEffort").and_then(JxlEffort::parse).unwrap_or(d.jxl_effort),
            jxl_progressive: p.get("jxlProgressive").and_then(Value::as_bool).unwrap_or(d.jxl_progressive),
            add_to_photos: p.get("addToPhotos").and_then(Value::as_bool).unwrap_or(d.add_to_photos),
            photos_album: s("photosAlbum").map_or(d.photos_album, |a| a.trim().to_string()),
            hdr_transfer: s("hdrTransfer").and_then(HdrCurve::parse).unwrap_or(d.hdr_transfer),
        }
    }

    fn resize_from_json(p: &serde_json::Value) -> Option<Resize> {
        use serde_json::Value;
        let f = |k: &str| p.get(k).and_then(Value::as_f64).filter(|v| *v > 0.0);
        let dont_enlarge = p.get("dontEnlarge").and_then(Value::as_bool).unwrap_or(true);
        if let Some(r) = p.get("resize").filter(|v| v.is_object()) {
            let r: Resize = serde_json::from_value(r.clone()).ok()?;
            return (r.value > 0.0).then_some(Resize { dont_enlarge: p.get("dontEnlarge").and_then(Value::as_bool).unwrap_or(r.dont_enlarge), ..r });
        }
        let r = |mode, value: f64| Resize { mode, value: value.min(65_535.0) as f32, height: 0, dont_enlarge };
        // `fit: widthHeight` binds each edge to its own number; the default fits either orientation
        let bound = p.get("fit").and_then(Value::as_str).is_some_and(|fit| fit.eq_ignore_ascii_case("widthHeight"));
        Some(match (f("width"), f("height")) {
            (Some(w), Some(h)) => {
                Resize { height: h.min(65_535.0) as u32, ..r(if bound { ResizeMode::WidthHeight } else { ResizeMode::Dimensions }, w) }
            }
            (Some(w), None) => r(ResizeMode::Width, w),
            (None, Some(h)) => r(ResizeMode::Height, h),
            (None, None) => {
                if let Some(v) = f("longEdge") {
                    r(ResizeMode::LongEdge, v)
                } else if let Some(v) = f("shortEdge") {
                    r(ResizeMode::ShortEdge, v)
                } else if let Some(v) = f("megapixels") {
                    r(ResizeMode::Megapixels, v)
                } else {
                    let v = f("percent")?;
                    r(ResizeMode::Percent, v)
                }
            }
        })
    }

    /// These options as `app.export` params ([`ExportOptions::from_json`] reads them back
    /// unchanged; full size is written as `longEdge: 0`).
    pub fn to_json(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(o) = v.as_object_mut() {
            o.remove("resize");
            if let Some(sz) = Self::resize_json(self.resize.as_ref()).as_object() {
                o.extend(sz.clone());
            }
            if self.resize.is_none() {
                o.insert("longEdge".into(), 0.into());
            }
            o.retain(|_, v| !v.is_null());
        }
        v
    }

    /// Whether `p` names an output size (any of the size params, even 0 = full size).
    pub fn has_size_param(p: &serde_json::Value) -> bool {
        ["resize", "longEdge", "shortEdge", "width", "height", "megapixels", "percent"].iter().any(|k| p.get(k).is_some())
    }

    /// The size params of [`ExportOptions::from_json`] for `resize` (`{}` = full size).
    pub fn resize_json(resize: Option<&Resize>) -> serde_json::Value {
        match resize {
            None => serde_json::json!({}),
            Some(r) => serde_json::json!({"resize": r, "dontEnlarge": r.dont_enlarge}),
        }
    }

    /// The sample format the file is written with (unsupported requests fall back to the closest).
    pub fn effective_depth(&self) -> OutputDepth {
        match (self.format, self.bit_depth) {
            (ExportFormat::Jpeg | ExportFormat::Webp | ExportFormat::Original | ExportFormat::Dng, _) => OutputDepth::U8,
            (ExportFormat::Png, _) if self.hdr => OutputDepth::F32Hdr,
            (ExportFormat::Png, Some(16 | 32)) => OutputDepth::U16,
            (ExportFormat::Png, _) => OutputDepth::U8,
            (ExportFormat::Tiff, Some(8)) => OutputDepth::U8,
            (ExportFormat::Tiff, Some(32)) if self.hdr => OutputDepth::F32Hdr,
            (ExportFormat::Tiff, Some(32)) => OutputDepth::F32Linear,
            (ExportFormat::Tiff, _) => OutputDepth::U16,
            (ExportFormat::RenderedDng, _) if self.hdr => OutputDepth::F32Hdr,
            (ExportFormat::RenderedDng, _) => OutputDepth::F32Linear,
            (ExportFormat::Psd, Some(8)) => OutputDepth::U8,
            (ExportFormat::Psd, _) => OutputDepth::U16,
            (ExportFormat::Avif, _) if self.hdr => OutputDepth::F32Hdr,
            (ExportFormat::Avif, Some(10 | 16 | 32)) => OutputDepth::U16,
            (ExportFormat::Avif, _) => OutputDepth::U8,
            (ExportFormat::Jxl, _) if self.hdr => OutputDepth::F32Hdr,
            (ExportFormat::Jxl, Some(10 | 16 | 32)) => OutputDepth::U16,
            (ExportFormat::Jxl, _) => OutputDepth::U8,
        }
    }

    /// The bit depths `format` offers (value, label); the first is its default.
    pub fn bit_depths(format: ExportFormat) -> &'static [(u8, &'static str)] {
        match format {
            ExportFormat::Jpeg | ExportFormat::Webp | ExportFormat::Original | ExportFormat::Dng => &[(8, "8-bit")],
            ExportFormat::Png => &[(8, "8-bit"), (16, "16-bit")],
            ExportFormat::Tiff => &[(16, "16-bit"), (8, "8-bit"), (32, "32-bit float")],
            ExportFormat::Psd => &[(16, "16-bit"), (8, "8-bit")],
            ExportFormat::RenderedDng => &[(16, "16-bit float")],
            ExportFormat::Avif => &[(8, "8-bit"), (10, "10-bit")],
            ExportFormat::Jxl => &[(8, "8-bit"), (16, "16-bit")],
        }
    }

    /// Whether these options write HDR files (for photos edited in HDR): JPEG (gain map), AVIF,
    /// PNG or JPEG XL (PQ) or 32-bit float TIFF with [`ExportOptions::hdr`].
    pub fn hdr_output(&self) -> bool {
        self.hdr
            && matches!(
                (self.format, self.bit_depth),
                (ExportFormat::Jpeg | ExportFormat::Avif | ExportFormat::Png | ExportFormat::Jxl, _) | (ExportFormat::Tiff, Some(32))
            )
    }

    /// The colour space the file is actually written in (AVIF: sRGB; AVIF, PNG and JPEG XL HDR:
    /// Rec. 2020; lossy JPEG XL: Adobe RGB and ProPhoto as Rec. 2020, the widest space its encoder
    /// can declare without a profile).
    pub fn effective_space(&self) -> OutputSpace {
        match self.format {
            ExportFormat::Avif | ExportFormat::Png | ExportFormat::Jxl if self.hdr => OutputSpace::Rec2020,
            ExportFormat::Avif => OutputSpace::Srgb,
            ExportFormat::Jxl if !self.jxl_lossless && matches!(self.color_space, OutputSpace::AdobeRgb | OutputSpace::ProPhoto) => {
                OutputSpace::Rec2020
            }
            _ => self.color_space,
        }
    }

    /// The JPEG XL encoder settings for these options, writing samples in `space`.
    fn jxl(&self, space: OutputSpace, hdr: bool) -> JxlOptions {
        let colour = match space {
            _ if hdr => JxlColour::Rec2020Pq,
            OutputSpace::DisplayP3 => JxlColour::DisplayP3,
            OutputSpace::Rec2020 => JxlColour::Rec2020,
            // lossless files carry the ICC profile; lossy ones never get here (`effective_space`)
            OutputSpace::Srgb | OutputSpace::AdobeRgb | OutputSpace::ProPhoto => JxlColour::Srgb,
        };
        JxlOptions { quality: self.quality, lossless: self.jxl_lossless, effort: self.jxl_effort, progressive: self.jxl_progressive, colour }
    }

    /// Output file name for photo `p` at 1-based position `seq` in a batch (the original's
    /// extension is kept for [`ExportFormat::Original`]).
    pub fn file_name_for(&self, p: &lightcraft_catalog::Photo, seq: usize) -> String {
        let base = if self.naming.trim().is_empty() { "{name}" } else { self.naming.as_str() };
        let n = seq + self.start_number.max(1) as usize - 1;
        let name = crate::rename::expand_tokens(base, p, n, 3);
        let mut name: String = name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '\0') { '_' } else { c }).collect();
        let (stem, ext) = p.file_name.rsplit_once('.').unwrap_or((&p.file_name, ""));
        if name.trim().is_empty() {
            // every token came out empty (no title, no capture time…): keep the original name
            name = stem.to_string();
        }
        if self.format == ExportFormat::Original {
            let base = name.trim_end_matches('.');
            if ext.is_empty() { base.to_string() } else { format!("{base}.{ext}") }
        } else {
            format!("{name}.{}", self.format.extension())
        }
    }
}

/// Unsharp mask on an 8-bit image (separable box-blur approximation of a small Gaussian).
pub fn output_sharpen(img: &mut Rgba8, target: SharpenFor, amount: SharpenAmount) {
    let src: Vec<[f32; 3]> = img.data.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
    if let Some(out) = unsharp(img.width, img.height, &src, target, amount) {
        for (p, v) in img.data.iter_mut().zip(out) {
            for c in 0..3 {
                p[c] = v[c].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// [`output_sharpen`] on a high-bit-depth image (float images are sharpened in linear light).
pub fn output_sharpen_deep(img: &mut DeepImage, target: SharpenFor, amount: SharpenAmount) {
    let (w, h) = (img.width, img.height);
    match &mut img.samples {
        DeepSamples::U16(v) => {
            let src: Vec<[f32; 3]> = v.as_chunks::<3>().0.iter().map(|c| [c[0] as f32, c[1] as f32, c[2] as f32]).collect();
            if let Some(out) = unsharp(w, h, &src, target, amount) {
                for (d, s) in v.iter_mut().zip(out.as_flattened()) {
                    *d = s.round().clamp(0.0, 65535.0) as u16;
                }
            }
        }
        DeepSamples::F32(v) => {
            let src: Vec<[f32; 3]> = v.as_chunks::<3>().0.iter().map(|c| [c[0], c[1], c[2]]).collect();
            if let Some(out) = unsharp(w, h, &src, target, amount) {
                for (d, s) in v.iter_mut().zip(out.as_flattened()) {
                    *d = s.max(0.0);
                }
            }
        }
    }
}

/// The sharpened values of `src` (`w × h`), or `None` when nothing is to be done.
fn unsharp(w: usize, h: usize, src: &[[f32; 3]], target: SharpenFor, amount: SharpenAmount) -> Option<Vec<[f32; 3]>> {
    let (radius, base) = match target {
        SharpenFor::None => return None,
        SharpenFor::Screen => (1usize, 0.35f32),
        SharpenFor::Matte => (2, 0.6),
        SharpenFor::Glossy => (1, 0.5),
    };
    let k = base
        * match amount {
            SharpenAmount::Low => 0.6,
            SharpenAmount::Standard => 1.0,
            SharpenAmount::High => 1.5,
        };
    if w < 3 || h < 3 {
        return None;
    }
    let r = radius as isize;
    let box_pass = |inp: &[[f32; 3]], horizontal: bool| -> Vec<[f32; 3]> {
        let mut out = vec![[0.0; 3]; inp.len()];
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0.0f32; 3];
                for d in -r..=r {
                    let (sx, sy) = if horizontal {
                        ((x as isize + d).clamp(0, w as isize - 1) as usize, y)
                    } else {
                        (x, (y as isize + d).clamp(0, h as isize - 1) as usize)
                    };
                    let p = inp[sy * w + sx];
                    acc = [acc[0] + p[0], acc[1] + p[1], acc[2] + p[2]];
                }
                let n = (2 * r + 1) as f32;
                out[y * w + x] = acc.map(|v| v / n);
            }
        }
        out
    };
    let blur = box_pass(&box_pass(src, true), false);
    Some(src.iter().zip(&blur).map(|(s, b)| std::array::from_fn(|c| s[c] + (s[c] - b[c]) * k)).collect())
}

/// Encode a rendered display-referred image according to `o` (its pixels must already be in
/// `o.effective_space()`). Resizing to `long_edge` is the
/// caller's job (render at that size); sharpening is applied here.
pub fn encode_image(img: &Rgba8, o: &ExportOptions) -> Result<Vec<u8>, String> {
    encode_with_metadata(img, o, None)
}

/// Like [`encode_image`], embedding `meta` (already filtered by the policy) as EXIF + XMP.
pub fn encode_with_metadata(img: &Rgba8, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    let img = finish_8bit(img, o);
    let space = o.effective_space();
    let profile = icc::write_named(named_space(space));
    let exif = meta.map(lightcraft_meta::write_exif);
    let xmp = meta.map(|m| lightcraft_meta::write_xmp(m, None));
    let meta = EncodeMeta { icc: Some(&profile), exif: exif.as_deref(), xmp: xmp.as_deref(), ppi: Some(o.ppi) };
    // Opaque photo exports need no alpha channel (#1670). Keep real transparency for
    // callers of this encoder, and leave unrelated formats on their existing path.
    let rgb: Option<Vec<u8>> = (matches!(o.format, ExportFormat::Png | ExportFormat::Tiff | ExportFormat::Webp)
        && img.data.iter().all(|p| p.last() == Some(&255)))
    .then(|| img.data.iter().flat_map(|p| p.iter().take(3).copied()).collect());
    let e = match &rgb {
        Some(data) => EncodeImage::new(img.width as u32, img.height as u32, 3, Samples::U8(data)),
        None => EncodeImage::rgba8(&img),
    };
    let r = match o.format {
        ExportFormat::Jpeg => {
            // 4:4:4 normally; 4:2:0 when targeting a file size (much smaller at equal visual quality).
            let sub = if o.limit_kb.is_some() { ChromaSubsampling::S420 } else { ChromaSubsampling::S444 };
            return encode_jpeg_with_limit(o, |q| encode::encode_jpeg(&e, q, sub, &meta));
        }
        ExportFormat::Png => encode::encode_png(&e, &meta),
        ExportFormat::Tiff => encode::encode_tiff(&e, o.tiff_compression, &meta),
        ExportFormat::Psd => encode::encode_psd(&e, &meta),
        ExportFormat::Webp => encode::encode_webp_lossless(&e, &meta),
        ExportFormat::Avif => encode::encode_avif(&e, o.quality, 8, &meta),
        ExportFormat::Jxl => lightcraft_codecs::encode_jxl(&e, &o.jxl(space, false), &meta),
        f @ (ExportFormat::Original | ExportFormat::Dng) => return Err(format!("{f:?} export does not encode pixels")),
        ExportFormat::RenderedDng => return Err("Render to DNG needs a float render".into()),
    };
    r.map_err(|e| e.to_string())
}

/// Apply the same file-size policy to ordinary JPEGs and complete HDR gain map JPEGs.
fn encode_jpeg_with_limit(o: &ExportOptions, jpeg: impl Fn(u8) -> lightcraft_codecs::Result<Vec<u8>>) -> Result<Vec<u8>, String> {
    let Some(kb) = o.limit_kb else { return jpeg(o.quality).map_err(|e| e.to_string()) };
    let limit = u64::from(kb) * 1024;
    let (mut lo, mut hi) = (1u8, o.quality.min(100));
    let mut best = None;
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let b = jpeg(mid).map_err(|e| e.to_string())?;
        if b.len() as u64 <= limit {
            best = Some(b);
            lo = mid + 1;
        } else if mid == 1 {
            break;
        } else {
            hi = mid - 1;
        }
    }
    best.ok_or_else(|| format!("cannot fit within {kb} KB"))
}

/// Output sharpening and the watermark on an 8-bit render in `o.effective_space()`.
fn finish_8bit(img: &Rgba8, o: &ExportOptions) -> Rgba8 {
    let mut img = img.clone();
    output_sharpen(&mut img, o.sharpen, o.sharpen_amount);
    let space = o.effective_space();
    if let Some(wm) = &o.watermark {
        let wm = Watermark { color: srgb8_in(space, wm.color), target: Some(space), ..wm.clone() };
        draw_watermark(&mut img, &wm);
    }
    img
}

/// Output sharpening and the watermark on a high-bit-depth render.
fn finish_deep(img: &DeepImage, o: &ExportOptions) -> DeepImage {
    let mut img = img.clone();
    output_sharpen_deep(&mut img, o.sharpen, o.sharpen_amount);
    if let Some(wm) = &o.watermark {
        let wm = Watermark { color: srgb8_in(img.space, wm.color), target: Some(img.space), ..wm.clone() };
        draw_watermark_deep(&mut img, &wm);
    }
    img
}

/// Encode an ISO 21496-1 gain map JPEG from the SDR rendition `sdr` (8-bit, in the output space)
/// and the HDR rendition `hdr` (float linear, same space and size). The gain map is measured
/// against the base as written, so its 8-bit rounding is handed back by the map.
pub fn encode_gain_map_jpeg(sdr: &Rgba8, hdr: &DeepImage, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    use lightcraft_codecs::gainmap;
    let base = finish_8bit(sdr, o);
    let hdr = finish_deep(hdr, o);
    let DeepSamples::F32(hv) = &hdr.samples else {
        return Err("HDR export needs a float HDR render".into());
    };
    let (w, h) = (base.width, base.height);
    if (hdr.width, hdr.height) != (w, h) || hv.len() < w * h * 3 {
        return Err("HDR and SDR renders differ in size".into());
    }
    let space = o.effective_space();
    let trc = space.trc();
    let sdr_lin: Vec<[f32; 3]> = base.data.iter().map(|p| [0, 1, 2].map(|c| trc.decode(p[c] as f32 / 255.0))).collect();
    let hdr_lin: &[[f32; 3]] = hv.as_chunks::<3>().0;
    let (map, gm) = gainmap::compute(&sdr_lin, hdr_lin, w, h, space.luma(), &gainmap::GainMapOptions::default()).map_err(|e| e.to_string())?;
    let profile = icc::write_named(named_space(space));
    let exif = meta.map(lightcraft_meta::write_exif);
    let xmp = meta.map(|m| lightcraft_meta::write_xmp(m, None));
    let em = EncodeMeta { icc: Some(&profile), exif: exif.as_deref(), xmp: xmp.as_deref(), ppi: Some(o.ppi) };
    let sub = if o.limit_kb.is_some() { ChromaSubsampling::S420 } else { ChromaSubsampling::S444 };
    encode_jpeg_with_limit(o, |q| gainmap::encode_jpeg(&EncodeImage::rgba8(&base), &map, &gm, q, sub, &em))
}

/// The codecs' name of an output space (for its ICC profile).
pub fn named_space(s: OutputSpace) -> NamedSpace {
    match s {
        OutputSpace::Srgb => NamedSpace::Srgb,
        OutputSpace::DisplayP3 => NamedSpace::DisplayP3,
        OutputSpace::AdobeRgb => NamedSpace::AdobeRgb,
        OutputSpace::ProPhoto => NamedSpace::ProPhoto,
        OutputSpace::Rec2020 => NamedSpace::Rec2020,
    }
}

/// An 8-bit sRGB colour expressed in `space` (encoded with its curve).
pub fn srgb8_in(space: OutputSpace, c: [u8; 3]) -> [u8; 3] {
    if space == OutputSpace::Srgb {
        return c;
    }
    let lin = c.map(lightcraft_color::transfer::decode_srgb8);
    let m = lightcraft_color::SRGB.to_space(&space.rgb_space()).to_f32();
    let t = space.trc();
    std::array::from_fn(|i| {
        let v = m[i][0] * lin[0] + m[i][1] * lin[1] + m[i][2] * lin[2];
        (t.encode(v) * 255.0 + 0.5) as u8
    })
}

/// Encode a render according to `o`: its high-bit-depth samples when it has them (16-bit PNG/TIFF,
/// 10-bit AVIF, 32-bit float linear TIFF), else its 8-bit image.
pub fn encode_rendered(r: &lightcraft_pipeline::Rendered, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    match &r.deep {
        Some(d) if o.effective_depth() != OutputDepth::U8 => encode_deep(d, o, meta),
        _ => encode_with_metadata(&r.image, o, meta),
    }
}

/// Encode a high-bit-depth image (see [`encode_rendered`]).
pub fn encode_deep(img: &DeepImage, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    let meta_src = meta;
    let img = finish_deep(img, o);
    let profile = match img.samples {
        DeepSamples::F32(_) => icc::write_matrix_trc(&img.space.rgb_space(), &lightcraft_codecs::Trc::Linear),
        DeepSamples::U16(_) => icc::write_named(named_space(img.space)),
    };
    let exif = meta.map(lightcraft_meta::write_exif);
    let xmp = meta.map(|m| lightcraft_meta::write_xmp(m, None));
    let meta = EncodeMeta { icc: Some(&profile), exif: exif.as_deref(), xmp: xmp.as_deref(), ppi: Some(o.ppi) };
    let (w, h) = (img.width as u32, img.height as u32);
    let e = match &img.samples {
        DeepSamples::U16(v) => EncodeImage::new(w, h, 3, Samples::U16(v)),
        DeepSamples::F32(v) => EncodeImage::new(w, h, 3, Samples::F32(v)),
    };
    let r = match (o.format, &img.samples) {
        (ExportFormat::Png, DeepSamples::F32(v)) if img.space == OutputSpace::Rec2020 => lightcraft_codecs::encode_png_pq(w, h, v, &meta),
        (ExportFormat::Png, _) => encode::encode_png(&e, &meta),
        (ExportFormat::Tiff, _) => encode::encode_tiff(&e, o.tiff_compression, &meta),
        (ExportFormat::Psd, _) => encode::encode_psd(&e, &meta),
        (ExportFormat::RenderedDng, DeepSamples::F32(v)) => {
            let m = meta_src.cloned().unwrap_or_default();
            return lightcraft_raw::write_rendered_dng(img.width, img.height, v, &img.space.rgb_space(), &m, xmp.clone()).map_err(|e| e.to_string());
        }
        (ExportFormat::Avif, DeepSamples::F32(v)) if img.space == OutputSpace::Rec2020 => {
            lightcraft_codecs::encode_avif_hdr(w, h, v, o.quality, 8, &meta, o.hdr_transfer.codec())
        }
        (ExportFormat::Avif, _) => encode::encode_avif(&e, o.quality, 8, &meta),
        (ExportFormat::Jxl, DeepSamples::F32(_)) if img.space == OutputSpace::Rec2020 => {
            lightcraft_codecs::encode_jxl(&e, &o.jxl(img.space, true), &meta)
        }
        (ExportFormat::Jxl, DeepSamples::U16(_)) => lightcraft_codecs::encode_jxl(&e, &o.jxl(img.space, false), &meta),
        (f, _) => return Err(format!("{f:?} export is 8-bit only")),
    };
    r.map_err(|e| e.to_string())
}

/// Whether the export of `photo` under `o` leaves out its location (GPS and place names).
/// (`photo` and `catalog` let library-level privacy rules add to the export option.)
pub fn removes_location(_photo: &lightcraft_catalog::Photo, _catalog: &lightcraft_catalog::Catalog, o: &ExportOptions) -> bool {
    o.remove_location
}

/// The metadata to embed for `photo` under `o.metadata` / `o.remove_location`. `None` = embed nothing.
/// Keywords follow the catalog's keyword list (`Catalog::export_keywords`).
pub fn export_metadata(photo: &lightcraft_catalog::Photo, catalog: &lightcraft_catalog::Catalog, o: &ExportOptions) -> Option<Metadata> {
    let m = &photo.meta;
    let text = |s: &str| (!s.trim().is_empty()).then(|| s.to_string());
    // copyright info (also under "copyright only"): notice, creator, status, usage terms, info URL
    let mut out = Metadata {
        copyright: text(&m.copyright),
        copyright_marked: m.copyright_status.marked(),
        usage_terms: text(&m.usage_terms),
        copyright_url: text(&m.copyright_url),
        artist: text(&m.creator),
        software: Some("LightCraft".into()),
        ..Default::default()
    };
    match o.metadata {
        MetadataPolicy::None => return None,
        MetadataPolicy::Copyright => return Some(out),
        MetadataPolicy::All | MetadataPolicy::AllExceptCamera => {}
    }
    out.title = text(&m.title);
    out.caption = text(&m.caption);
    out.alt_text = text(&m.alt_text);
    out.extended_description = text(&m.extended_description);
    if !o.remove_location {
        out.sublocation = text(&m.location);
        out.city = text(&m.city);
        out.state = text(&m.state);
        out.country = text(&m.country);
    }
    let keywords = catalog.export_keywords(&m.keywords);
    out.keywords = keywords.flat;
    out.hierarchical_keywords = keywords.hierarchical;
    out.capture_time = photo.captured.as_deref().and_then(DateTime::parse_iso);
    out.rating = (photo.rating > 0).then_some(photo.rating as i8);
    // Pixels are exported upright: orientation is baked in.
    out.orientation = Some(lightcraft_meta::Orientation::Normal);
    if !o.remove_location {
        out.gps = m.gps.map(|(latitude, longitude)| Gps { latitude, longitude, altitude: None });
    }
    if o.metadata == MetadataPolicy::All {
        out.model = text(&m.camera);
        out.lens_model = text(&m.lens);
        out.focal_length = m.focal_mm.map(f64::from);
        out.f_number = m.aperture.map(f64::from);
        out.exposure_time = lightcraft_catalog::parse_shutter_seconds(&m.shutter);
        out.iso = m.iso;
    }
    Some(out)
}

/// One exported file.
pub struct Exported {
    pub file_name: String,
    pub bytes: Vec<u8>,
    pub width: usize,
    pub height: usize,
    /// Files written next to it, named like it with another extension: `(extension, bytes)` (the
    /// XMP sidecar of an `Original` export).
    pub sidecars: Vec<(&'static str, Vec<u8>)>,
    /// Set when the render was tried on the GPU and fell back to the CPU: why (see
    /// `docs/gpu-pipeline.md`). The file holds the CPU render either way.
    pub gpu_fallback: Option<String>,
}

/// Output size of photo `p` under `o` (its cropped full size when `o.resize` is `None`).
pub fn output_size(p: &lightcraft_catalog::Photo, o: &ExportOptions) -> (usize, usize) {
    let (w, h) = lightcraft_pipeline::native_output_size(p.width.max(1) as usize, p.height.max(1) as usize, &p.develop);
    // whole pixels first: a crop's float noise (5999.999… for 6000) must not decide a half-pixel tie
    let (w, h) = (w.round().max(1.0), h.round().max(1.0));
    match &o.resize {
        Some(r) => r.apply(w, h),
        None => (w as usize, h as usize),
    }
}

/// Render photo `id` at the requested size and encode it (or copy / convert its original for
/// [`ExportFormat::Original`] / [`ExportFormat::Dng`]).
pub fn export_photo(session: &mut crate::Session, id: lightcraft_catalog::PhotoId, o: &ExportOptions, seq: usize) -> Result<Exported, String> {
    prepare_export(session, id, o, seq)?.run()
}

/// One photo's export, set up from the session ([`prepare_export`]). [`PreparedExport::run`] does
/// the heavy part (read, decode, render, encode) without the session, so it can run on another
/// thread.
pub struct PreparedExport {
    pub photo: lightcraft_catalog::PhotoId,
    pub file_name: String,
    /// The folder of the photo's original on disk ([`DestinationMode::SameAsOriginal`]); `None`
    /// for a demo photo or one in the browser's storage.
    pub source_dir: Option<String>,
    work: Work,
    /// The library's originals, which [`run_batch`] never writes over (shared by a batch).
    guard: std::sync::Arc<crate::originals::OriginalGuard>,
    /// Estimated working memory of [`Self::run`] (bytes), held from [`crate::memory::export_gate`]
    /// while it runs beside other photos of a batch.
    weight: usize,
}

struct RenderWork {
    job: crate::media::RenderJob,
    /// The HDR rendition of a gain map JPEG (`job` renders its SDR base).
    hdr_job: Option<crate::media::RenderJob>,
    meta: Option<Metadata>,
    opts: ExportOptions,
}

enum Work {
    Render(Box<RenderWork>),
    /// [`ExportFormat::Original`] (the file's bytes + an XMP sidecar) or [`ExportFormat::Dng`] (the
    /// raw data re-encoded as a lossless DNG with the edits in its XMP).
    File {
        path: String,
        read: Option<crate::merge::ByteReader>,
        packet: String,
        dng: Option<DngCompression>,
        /// Lossy DNG: JPEG quality and the long edge to scale to (`None`: full size).
        quality: u8,
        lossy_edge: Option<u32>,
        /// The original must not carry a location ([`removes_location`]).
        remove_location: bool,
        label: String,
        size: (usize, usize),
    },
}

/// Set up the export of photo `id` at 1-based position `seq` of a batch. (For a whole batch use
/// [`prepare_batch`]: it looks at the library's originals once.)
pub fn prepare_export(
    session: &mut crate::Session,
    id: lightcraft_catalog::PhotoId,
    o: &ExportOptions,
    seq: usize,
) -> Result<PreparedExport, String> {
    let guard = std::sync::Arc::new(session.original_guard());
    prepare_guarded(session, id, o, seq, guard)
}

/// [`prepare_export`] for each of `ids` in order (`{seq}` = position, from 1).
pub fn prepare_batch(session: &mut crate::Session, ids: &[lightcraft_catalog::PhotoId], o: &ExportOptions) -> Result<Vec<PreparedExport>, String> {
    let guard = std::sync::Arc::new(session.original_guard());
    ids.iter().enumerate().map(|(i, id)| prepare_guarded(session, *id, o, i + 1, guard.clone())).collect()
}

fn prepare_guarded(
    session: &mut crate::Session,
    id: lightcraft_catalog::PhotoId,
    o: &ExportOptions,
    seq: usize,
    guard: std::sync::Arc<crate::originals::OriginalGuard>,
) -> Result<PreparedExport, String> {
    let p = session.catalog.photo(id).ok_or("no such photo")?;
    let file_name = o.file_name_for(p, seq);
    let source_dir = original_folder(&p.source);
    // HDR output only for photos edited in HDR; the rest of the batch exports as usual
    let sdr_opts;
    let o = if o.hdr && !p.develop.hdr.enabled {
        sdr_opts = ExportOptions { hdr: false, ..o.clone() };
        &sdr_opts
    } else {
        o
    };
    let (full_px, long) = ((p.width as usize).saturating_mul(p.height as usize), p.width.max(p.height) as usize);
    let (work, weight) = if o.format.is_rendered() {
        let (w, h) = output_size(p, o);
        let (nw, nh) = lightcraft_pipeline::native_output_size(p.width.max(1) as usize, p.height.max(1) as usize, &p.develop);
        let native_px = (nw.round().max(1.0) * nh.round().max(1.0)) as usize;
        let label = p.file_name.clone();
        let meta = export_metadata(p, &session.catalog, o);
        let gain_map = o.hdr_output() && o.format == ExportFormat::Jpeg;
        let job = session.export_job(id, w, h, o.effective_space(), o.effective_depth())?;
        let weight = render_weight(full_px, long, job.level, w.saturating_mul(h));
        if let Some(why) = too_big_for_memory((w, h), native_px, weight, crate::memory::total_ram()) {
            return Err(format!("{label}: {why}"));
        }
        let hdr_job = if gain_map { Some(session.export_job(id, w, h, o.effective_space(), OutputDepth::F32Hdr)?) } else { None };
        (Work::Render(Box::new(RenderWork { job, hdr_job, meta, opts: o.clone() })), weight)
    } else {
        let lightcraft_catalog::Source::File { path } = &p.source else {
            return Err(format!("{} is a generated demo photo: it has no original file to export", p.file_name));
        };
        let dng = (o.format == ExportFormat::Dng).then_some(o.dng_compression);
        // the file's bytes; a DNG also decodes the raw data (16-bit samples) and writes it again
        let weight = (p.file_size as usize).saturating_add(if dng.is_some() { full_px.saturating_mul(6) } else { 0 });
        let work = Work::File {
            path: path.clone(),
            read: session.media.file_bytes.clone(),
            packet: crate::sidecar::sidecar_packet_with(p, &session.catalog, removes_location(p, &session.catalog, o)),
            dng,
            quality: o.quality,
            lossy_edge: o.resize.is_some().then(|| {
                let (w, h) = output_size(p, o);
                u32::try_from(w.max(h)).unwrap_or(u32::MAX)
            }),
            remove_location: removes_location(p, &session.catalog, o),
            label: p.file_name.clone(),
            size: (p.width as usize, p.height as usize),
        };
        (work, weight)
    };
    Ok(PreparedExport { photo: id, file_name, source_dir, work, guard, weight })
}

/// The folder a photo's original is in, for [`DestinationMode::SameAsOriginal`]. None for a demo
/// photo, and on the web, where an original lives in the browser's storage, not in a folder.
fn original_folder(source: &lightcraft_catalog::Source) -> Option<String> {
    let lightcraft_catalog::Source::File { path } = source else { return None };
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    let dir = std::path::Path::new(path).parent()?.to_string_lossy().to_string();
    (!dir.is_empty()).then_some(dir)
}

/// Why an export of `out` pixels (`w`, `h`), from a photo of `native_px` pixels at its own size,
/// can't be made on a computer with `ram` bytes, given its working memory `weight`: an export
/// enlarged past the photo's size that needs more memory than the computer has would fail to
/// allocate, which ends the process rather than the export. Full-size and smaller exports, and
/// computers whose memory isn't known, are never refused.
fn too_big_for_memory(out: (usize, usize), native_px: usize, weight: usize, ram: Option<usize>) -> Option<String> {
    let ram = ram?;
    if out.0.saturating_mul(out.1) <= native_px || weight <= ram {
        return None;
    }
    let gb = |b: usize| b as f64 / (1u64 << 30) as f64;
    Some(format!(
        "exporting at {} × {} needs about {:.0} GB of memory, more than this computer has ({:.0} GB); choose a smaller size",
        out.0,
        out.1,
        gb(weight).ceil(),
        gb(ram)
    ))
}

/// Working memory of rendering a photo of `full_px` pixels (long edge `long`) from source level
/// `level` into `out_px` output pixels: the decoded source (linear RGB f32, 12 B/px) plus the
/// pipeline's planes and the encoded output (~36 B per output pixel). Measured: a 24 MP export
/// peaks at ~1.2 GB, one at 2048 px from the 2560 px preview at ~150 MB.
fn render_weight(full_px: usize, long: usize, level: crate::media::SourceLevel, out_px: usize) -> usize {
    let edge = level.max_edge().min(long.max(1));
    let scale = edge as f64 / long.max(1) as f64;
    let src_px = (full_px as f64 * scale * scale).min(usize::MAX as f64 / 64.0) as usize;
    src_px.saturating_mul(12).saturating_add(out_px.saturating_mul(36))
}

impl PreparedExport {
    pub fn run(self) -> Result<Exported, String> {
        let file_name = self.file_name;
        match self.work {
            Work::Render(w) => {
                let RenderWork { job, hdr_job, meta, opts } = *w;
                let result = job.run();
                let r = result.rendered?;
                let gpu_fallback = result.gpu_fallback;
                let bytes = match hdr_job {
                    Some(hj) => {
                        let hr = hj.run().rendered?;
                        let hdr = hr.deep.ok_or("the HDR render has no float samples")?;
                        encode_gain_map_jpeg(&r.image, &hdr, &opts, meta.as_ref())?
                    }
                    None => encode_rendered(&r, &opts, meta.as_ref())?,
                };
                Ok(Exported { file_name, bytes, width: r.image.width, height: r.image.height, sidecars: Vec::new(), gpu_fallback })
            }
            Work::File { path, read, packet, dng, quality, lossy_edge, label, size, remove_location } => {
                let bytes = match &read {
                    Some(r) => r(&path)?,
                    None => std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?,
                };
                let Some(dng) = dng else {
                    // the file is copied byte for byte: refuse rather than leak a location it embeds
                    if remove_location {
                        let metadata = lightcraft_meta::try_extract(&bytes)
                            .map_err(|e| format!("{label}: can't verify location metadata in this original file: {e}"))?;
                        if metadata.gps.is_some() {
                            return Err(format!(
                                "{label}: can't remove the location from an original file that embeds GPS; export as JPEG, TIFF or DNG instead"
                            ));
                        }
                    }
                    return Ok(Exported {
                        file_name,
                        bytes,
                        width: size.0,
                        height: size.1,
                        sidecars: vec![("xmp", packet.into_bytes())],
                        gpu_fallback: None,
                    });
                };
                if lightcraft_raw::probe(&bytes).is_none() {
                    return Err(format!("{label}: DNG export needs a raw photo"));
                }
                let mut raw = lightcraft_raw::decode(&bytes).map_err(|e| format!("{label}: {e}"))?;
                crate::camera_preview::with_starting_colour(&mut raw, &bytes);
                drop(bytes);
                let compression = match dng {
                    DngCompression::Lossless => lightcraft_raw::DngCompression::Lj92 { tile: 256 },
                    DngCompression::Deflate => lightcraft_raw::DngCompression::Deflate { tile: 256, half: false },
                    DngCompression::Uncompressed => lightcraft_raw::DngCompression::Uncompressed,
                    DngCompression::Lossy => {
                        lightcraft_raw::DngCompression::LossyJpeg { tile: 256, quality: quality.clamp(1, 100), long_edge: lossy_edge }
                    }
                };
                let dng = lightcraft_raw::write_dng(&raw, &lightcraft_raw::DngWriteOptions { xmp: Some(packet), compression, ..Default::default() })
                    .map_err(|e| e.to_string())?;
                Ok(Exported { file_name, bytes: dng, width: raw.width, height: raw.height, sidecars: Vec::new(), gpu_fallback: None })
            }
        }
    }
}

/// How an exported DNG stores its raw data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DngCompression {
    /// Lossless JPEG (the usual DNG compression).
    #[default]
    Lossless,
    /// Deflate (zip) tiles with prediction.
    Deflate,
    Uncompressed,
    /// Lossy DNG: demosaiced linear data as JPEG tiles at the export quality, scaled to the resize
    /// setting when there is one (Lightroom's "Use Lossy Compression").
    Lossy,
}

/// The destination of a batch: a folder (with `ExportOptions::subfolder` and the conflict policy
/// applied to each file together with its sidecars; with [`DestinationMode::SameAsOriginal`] each
/// photo's own folder instead of `dir`), or one exact file path (single photo; an ordinary file
/// already there is replaced). Either way a catalogued original (or its sidecar)
/// is never written over: see [`crate::originals`].
#[derive(Clone, Debug, Default)]
pub struct Destination {
    pub dir: String,
    pub exact: Option<String>,
}

/// What happens after an export has written its files (Lightroom's Post-Processing): nothing, or
/// adding them to Apple Photos ([`ExportOptions::add_to_photos`]). Made by
/// [`crate::Session::after_export`] before the export starts, so an export asking for something
/// this session can't do fails before writing anything; for Photos it holds the import slot
/// (a [`crate::apple_photos::Reservation`]) until the files are handed over, and frees it when
/// dropped unused (the export failed or never ran). `Send`: a background export takes it to its
/// worker.
#[derive(Default)]
pub struct AfterExport {
    #[cfg(not(target_arch = "wasm32"))]
    apple_photos: Option<crate::apple_photos::Reservation>,
}

impl AfterExport {
    /// Add the files to Apple Photos with `runner`, into `album` when it isn't blank, reserving
    /// the import slot of `imports` now. (Front ends get this from
    /// [`crate::Session::after_export`], which checks the platform first.)
    #[cfg(not(target_arch = "wasm32"))]
    pub fn apple_photos(runner: crate::apple_photos::Runner, album: &str, imports: &crate::apple_photos::Imports) -> Result<Self, String> {
        Ok(Self { apple_photos: Some(imports.reserve(runner, album)?) })
    }

    /// Nothing to do.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn is_none(&self) -> bool {
        self.apple_photos.is_none()
    }

    /// Nothing to do.
    #[cfg(target_arch = "wasm32")]
    pub fn is_none(&self) -> bool {
        true
    }

    /// Do it now for an export's per-photo results ([`run_batch`]'s: only the files written
    /// count), waiting for Photos: for a worker thread (the app's background export) or a caller
    /// without a window (MCP, lightcraft-cli). `None` when there was nothing to do, else the
    /// outcome to report as the export's `applePhotos`: the finished job (`{job, running: false,
    /// requested, album}` with `{imported, ids, warning?}`, `{error}`, or `{skipped}` when the
    /// export was cancelled or wrote nothing). Never fails the export: its files are written
    /// either way.
    pub fn run(self, files: &[serde_json::Value], cancelled: bool) -> Option<serde_json::Value> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(r) = self.apple_photos {
            return Some(r.run(files, cancelled));
        }
        let _ = (files, cancelled);
        None
    }

    /// Like [`AfterExport::run`], without waiting: Photos works on a worker thread and the
    /// outcome is the running job (`{job, running: true, requested, album}`; its result comes
    /// from `export.photosImports`, and the desktop app announces it). For a caller on the UI
    /// thread, which must not wait minutes for Photos.
    pub fn start(self, files: &[serde_json::Value]) -> Option<serde_json::Value> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(r) = self.apple_photos {
            return Some(r.start(files));
        }
        let _ = files;
        None
    }

    /// [`AfterExport::start`] where the session answers on a UI thread
    /// ([`crate::Session::apple_photos_background`]), else [`AfterExport::run`].
    pub fn finish(self, s: &crate::Session, files: &[serde_json::Value]) -> Option<serde_json::Value> {
        #[cfg(not(target_arch = "wasm32"))]
        if s.apple_photos_background {
            return self.start(files);
        }
        let _ = s;
        self.run(files, false)
    }
}

impl crate::Session {
    /// What an export with `o` does after writing its files; an error, before anything is written,
    /// when it asks for Apple Photos and this platform or session has no way to reach it, or
    /// another import is reserved or running. For Photos the import slot is reserved from now on.
    pub fn after_export(&self, o: &ExportOptions) -> Result<AfterExport, String> {
        if !o.add_to_photos {
            return Ok(AfterExport::default());
        }
        if let Some(why) = apple_photos_unsupported() {
            return Err(why.into());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let runner = self.apple_photos.clone().ok_or_else(|| crate::apple_photos::unavailable().to_string())?;
            let mut after = AfterExport::apple_photos(runner, &o.photos_album, &self.apple_photos_imports)?;
            after.apple_photos = after.apple_photos.map(|r| r.shown_in(self.activity.clone()));
            Ok(after)
        }
        #[cfg(target_arch = "wasm32")]
        Err("Apple Photos is only available on macOS".into())
    }
}

/// Export `ids` in order ([`prepare_batch`] + [`run_batch`]), stopping at the first error.
pub fn export_batch(
    session: &mut crate::Session,
    ids: &[lightcraft_catalog::PhotoId],
    o: &ExportOptions,
    to: &Destination,
    write: &mut dyn FnMut(&str, &[u8]) -> Result<(), String>,
    exists: &dyn Fn(&str) -> bool,
) -> Result<Vec<serde_json::Value>, String> {
    let items = prepare_batch(session, ids, o)?;
    run_batch(items, o, to, write, exists, true, &mut |_, _| true)
}

/// Write an exported or rendered file on disk: its folder is created if needed, and the file is
/// replaced atomically ([`lightcraft_catalog::safe_file::write_atomic_nosync`]: a temp file
/// renamed into place), so a failure part-way leaves any previous file intact and no truncated
/// one. Not synced to disk (issue #134): an export can always be made again from the original.
/// The native writer behind exports, renders and screenshots (app, CLI, MCP).
pub fn write_file(path: &str, bytes: &[u8]) -> Result<(), String> {
    write_with(path, bytes, false)
}

/// [`write_file`], synced to disk: for a written file that becomes a library photo (Edit a Copy)
/// and so can't simply be exported again once the catalog points at it.
pub fn write_file_durable(path: &str, bytes: &[u8]) -> Result<(), String> {
    write_with(path, bytes, true)
}

fn write_with(path: &str, bytes: &[u8], durable: bool) -> Result<(), String> {
    use lightcraft_catalog::safe_file::{write_atomic, write_atomic_nosync};
    let p = std::path::Path::new(path);
    if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    if durable { write_atomic(p, bytes) } else { write_atomic_nosync(p, bytes) }.map_err(|e| format!("{path}: {e}"))
}

/// The path of the sidecar with extension `ext` of the exported file `main`.
fn sidecar_path(main: &str, ext: &str) -> String {
    std::path::Path::new(main).with_extension(ext).to_string_lossy().to_string()
}

/// Run prepared exports in order: pick each one's path, and hand the bytes (and sidecars) to
/// `write`. `exists` tells whether a path is taken. The conflict policy applies to a file and its
/// sidecars as one: with Unique both get the same free name, with Skip the photo is skipped when
/// either is taken. A path that is a catalogued original (or its sidecar) is refused whatever the
/// policy. `progress(done, next file)` is called before each photo; returning false cancels the
/// rest. Returns one JSON object per photo: `{path, width, height, bytes, sidecars}`,
/// `{skipped: path}` or (unless `stop_on_error`) `{photo, file, error}`.
///
/// Several photos render side by side ([`export_parallelism`], within the memory of
/// [`crate::memory::export_gate`]), so one photo's decode, encode or GPU wait overlaps another's
/// render (issue #496). Paths, writes, results and `progress` still follow the batch order, and the
/// files are the same as one at a time.
pub fn run_batch(
    items: Vec<PreparedExport>,
    o: &ExportOptions,
    to: &Destination,
    write: &mut dyn FnMut(&str, &[u8]) -> Result<(), String>,
    exists: &dyn Fn(&str) -> bool,
    stop_on_error: bool,
    progress: &mut dyn FnMut(usize, &str) -> bool,
) -> Result<Vec<serde_json::Value>, String> {
    let lanes = export_parallelism(items.len());
    run_batch_with(items, &mut Placer::new(o, to, write, exists, stop_on_error), progress, lanes)
}

/// How many photos of a batch of `n` render side by side. Each render already spreads its rows
/// over every core; the others fill the cores while it decodes, encodes or waits for the GPU.
/// One on wasm32 (no threads).
pub fn export_parallelism(n: usize) -> usize {
    if cfg!(target_arch = "wasm32") {
        return 1;
    }
    let cores = std::thread::available_parallelism().map_or(1, |c| c.get());
    (cores / 4).clamp(1, 4).min(n.max(1))
}

/// [`run_batch`] with `lanes` photos in flight (the calling thread is one of them).
fn run_batch_with(
    items: Vec<PreparedExport>,
    placer: &mut Placer<'_>,
    progress: &mut dyn FnMut(usize, &str) -> bool,
    lanes: usize,
) -> Result<Vec<serde_json::Value>, String> {
    placer.single = items.len() == 1;
    if lanes <= 1 || items.len() <= 1 {
        for (i, item) in items.into_iter().enumerate() {
            if !progress(i, &item.file_name) {
                break;
            }
            let (photo, name, guard, source_dir) = (item.photo, item.file_name.clone(), item.guard.clone(), item.source_dir.clone());
            placer.place(photo, name, source_dir.as_deref(), &guard, run_gated(item))?;
        }
        return Ok(std::mem::take(&mut placer.out));
    }
    let heads: Vec<_> = items.iter().map(|i| (i.photo, i.file_name.clone(), i.guard.clone(), i.source_dir.clone())).collect();
    let queue = Queue {
        lane: std::sync::Mutex::new(Lane { items: items.into_iter().map(Some).collect(), next: 0, placed: 0, stop: false, done: Default::default() }),
        cv: std::sync::Condvar::new(),
        ahead: lanes,
    };
    std::thread::scope(|sc| {
        // stops the lanes however this thread leaves the scope: a panic in `progress` or the placer
        // would otherwise leave them waiting and the scope's join would never return
        let _stop = StopOnDrop(&queue);
        for k in 1..lanes {
            let q = &queue;
            // a lane that can't start leaves its photos to the others (and to this thread)
            if std::thread::Builder::new().name(format!("export-{k}")).spawn_scoped(sc, move || q.work()).is_err() {
                break;
            }
        }
        let placed = || {
            for (i, (photo, name, guard, source_dir)) in heads.into_iter().enumerate() {
                if !progress(i, &name) {
                    break;
                }
                let e = queue.result(i);
                placer.place(photo, name, source_dir.as_deref(), &guard, e)?;
            }
            Ok::<_, String>(())
        };
        placed()
    })?;
    Ok(std::mem::take(&mut placer.out))
}

/// Stops a [`Queue`] when dropped, also while unwinding from a panic.
struct StopOnDrop<'a>(&'a Queue);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.stop();
    }
}

/// Run one photo's export holding its working memory from [`crate::memory::export_gate`]; a panic
/// becomes that photo's error, not the batch's end.
fn run_gated(item: PreparedExport) -> Result<Exported, String> {
    let _held = crate::memory::export_gate().acquire(item.weight);
    let what = format!("exporting {}", item.file_name);
    crate::guard::catch(&what, || item.run()).and_then(|r| r)
}

/// The photos of a batch shared by its lanes: each lane takes the next one, at most
/// [`Queue::ahead`] past the last placed. That bounds the finished files waiting in memory for
/// their turn too (their working memory is given back to the gate as soon as they are encoded).
struct Queue {
    lane: std::sync::Mutex<Lane>,
    cv: std::sync::Condvar,
    ahead: usize,
}

struct Lane {
    items: Vec<Option<PreparedExport>>,
    /// The next photo to start.
    next: usize,
    /// Photos handed to the placer so far.
    placed: usize,
    stop: bool,
    done: std::collections::HashMap<usize, Result<Exported, String>>,
}

impl Lane {
    fn take_next(&mut self) -> Option<(usize, PreparedExport)> {
        let i = self.next;
        let item = self.items.get_mut(i)?.take()?;
        self.next += 1;
        Some((i, item))
    }
}

impl Queue {
    fn lock(&self) -> std::sync::MutexGuard<'_, Lane> {
        self.lane.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn wait<'a>(&self, g: std::sync::MutexGuard<'a, Lane>) -> std::sync::MutexGuard<'a, Lane> {
        self.cv.wait(g).unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn finish(&self, i: usize, r: Result<Exported, String>) {
        self.lock().done.insert(i, r);
        self.cv.notify_all();
    }

    /// A lane thread: run photos until none is left or the batch stops.
    fn work(&self) {
        loop {
            let mut l = self.lock();
            let (i, item) = loop {
                if l.stop || l.next >= l.items.len() {
                    return;
                }
                if l.next < l.placed.saturating_add(self.ahead) {
                    match l.take_next() {
                        Some(t) => break t,
                        None => return,
                    }
                }
                l = self.wait(l);
            };
            drop(l);
            self.finish(i, run_gated(item));
        }
    }

    /// Photo `i`'s result, waiting for it; run here when no lane has started it yet.
    fn result(&self, i: usize) -> Result<Exported, String> {
        let mut l = self.lock();
        loop {
            if let Some(r) = l.done.remove(&i) {
                l.placed = i + 1;
                drop(l);
                self.cv.notify_all();
                return r;
            }
            if l.next == i {
                let Some((_, item)) = l.take_next() else {
                    return Err("export: photo missing from the batch".into());
                };
                drop(l);
                self.finish(i, run_gated(item));
                l = self.lock();
                continue;
            }
            l = self.wait(l);
        }
    }

    fn stop(&self) {
        self.lock().stop = true;
        self.cv.notify_all();
    }
}

/// Where a batch's files go: each photo's path (destination folder, subfolder, conflict policy,
/// sidecars, never an original) and its write, in batch order.
struct Placer<'a> {
    o: &'a ExportOptions,
    to: &'a Destination,
    single: bool,
    taken: std::collections::HashSet<String>,
    out: Vec<serde_json::Value>,
    write: &'a mut dyn FnMut(&str, &[u8]) -> Result<(), String>,
    exists: &'a dyn Fn(&str) -> bool,
    stop_on_error: bool,
}

fn join_path(a: &str, b: &str) -> String {
    if a.is_empty() { b.to_string() } else { format!("{}/{b}", a.trim_end_matches('/')) }
}

impl<'a> Placer<'a> {
    fn new(
        o: &'a ExportOptions,
        to: &'a Destination,
        write: &'a mut dyn FnMut(&str, &[u8]) -> Result<(), String>,
        exists: &'a dyn Fn(&str) -> bool,
        stop_on_error: bool,
    ) -> Placer<'a> {
        Placer { o, to, single: false, taken: Default::default(), out: Vec::new(), write, exists, stop_on_error }
    }

    /// The folder photo `name` (its original in `source_dir`) is written to: the batch's folder or
    /// the photo's own ([`DestinationMode`]), plus the subfolder. An error for a photo with no
    /// folder of its own, or whose folder is gone: never a file written somewhere else instead.
    fn folder_for(&self, name: &str, source_dir: Option<&str>) -> Result<String, String> {
        let base = match self.o.destination_mode {
            DestinationMode::SpecificFolder => self.to.dir.clone(),
            DestinationMode::SameAsOriginal => {
                let Some(d) = source_dir else {
                    return Err(format!(
                        "{name}: the photo has no folder on disk (a demo photo or one in the browser's storage), so it can't be exported to the same folder as its original; choose a specific folder"
                    ));
                };
                if !(self.exists)(d) {
                    return Err(format!("{name}: the original's folder is missing ({d}); reconnect it or choose a specific folder"));
                }
                d.to_string()
            }
        };
        Ok(if self.o.subfolder.is_empty() { base } else { join_path(&base, &self.o.subfolder) })
    }

    /// Place photo `photo`'s export result (file `name`, its original in `source_dir`): write it
    /// and record the outcome. `Err` only when it failed and `stop_on_error` is set.
    fn place(
        &mut self,
        photo: lightcraft_catalog::PhotoId,
        name: String,
        source_dir: Option<&str>,
        guard: &crate::originals::OriginalGuard,
        e: Result<Exported, String>,
    ) -> Result<(), String> {
        use serde_json::json;
        // an exact file path needs no folder
        let dir = if self.to.exact.is_some() && self.single { Ok(String::new()) } else { self.folder_for(&name, source_dir) };
        let (o, exists) = (self.o, self.exists);
        let (dir, e) = match dir.and_then(|dir| e.map(|e| (dir, e))) {
            Ok(v) => v,
            Err(err) if self.stop_on_error => return Err(err),
            Err(err) => {
                self.out.push(json!({"photo": photo.0, "file": name, "error": err}));
                return Ok(());
            }
        };
        let dir = dir.as_str();
        // the exported file and its sidecars
        let group = |main: &str| std::iter::once(main.to_string()).chain(e.sidecars.iter().map(|(x, _)| sidecar_path(main, x))).collect::<Vec<_>>();
        let path = match self.to.exact.as_deref().filter(|_| self.single) {
            Some(p) => Ok(p.to_string()),
            None => {
                let path = join_path(dir, &e.file_name);
                let taken = &self.taken;
                let busy = |main: &str| group(main).iter().any(|p| taken.contains(p) || exists(p));
                if !busy(&path) {
                    Ok(path)
                } else {
                    match o.conflict {
                        Conflict::Overwrite => Ok(path),
                        Conflict::Skip => {
                            self.out.push(json!({"skipped": path}));
                            return Ok(());
                        }
                        Conflict::Unique => {
                            let (stem, ext) = e.file_name.rsplit_once('.').map_or((e.file_name.as_str(), None), |(a, b)| (a, Some(b)));
                            let name = |n: usize| join_path(dir, &ext.map_or(format!("{stem}-{n}"), |x| format!("{stem}-{n}.{x}")));
                            (2..1_000_000).map(name).find(|p| !busy(p)).ok_or_else(|| format!("{path}: no free file name"))
                        }
                    }
                }
            }
        };
        let file = path.clone().unwrap_or_else(|_| name.clone());
        let write = &mut *self.write;
        let written = path.and_then(|path| {
            let files = group(&path);
            // never over an original, whatever the conflict policy or the exact path said
            for f in &files {
                guard.check(std::path::Path::new(f))?;
            }
            write(&path, &e.bytes)?;
            let mut sidecars = Vec::new();
            for ((_, bytes), sc) in e.sidecars.iter().zip(files.iter().skip(1)) {
                write(sc, bytes)?;
                sidecars.push(sc.clone());
            }
            Ok((path, files, sidecars))
        });
        match written {
            Ok((path, files, sidecars)) => {
                self.taken.extend(files);
                let mut entry = json!({"path": path, "width": e.width, "height": e.height, "bytes": e.bytes.len(), "sidecars": sidecars});
                if let Some(why) = &e.gpu_fallback {
                    entry["gpuFallback"] = json!(why);
                }
                self.out.push(entry);
            }
            Err(err) if self.stop_on_error => return Err(err),
            Err(err) => self.out.push(json!({"photo": photo.0, "file": file, "error": err})),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_image() -> Rgba8 {
        let mut img = Rgba8::new(96, 64);
        for (i, p) in img.data.iter_mut().enumerate() {
            let (x, y) = (i % 96, i / 96);
            *p = [(x * 2) as u8, (y * 3) as u8, ((x * y) % 255) as u8, 255];
        }
        img
    }

    /// Issue #1670: removing a redundant alpha channel preserves samples and metadata.
    #[test]
    fn opaque_eight_bit_exports_are_rgb() {
        let img = test_image();
        let metadata = Metadata { copyright: Some("Synthetic export test".into()), ..Default::default() };
        for format in [ExportFormat::Png, ExportFormat::Tiff, ExportFormat::Webp] {
            for compression in [TiffCompression::None, TiffCompression::Lzw, TiffCompression::Deflate, TiffCompression::PackBits] {
                let options = ExportOptions { format, tiff_compression: compression, ..Default::default() };
                let bytes = encode_with_metadata(&img, &options, Some(&metadata)).unwrap();
                let decoded = lightcraft_codecs::decode(&bytes, Default::default()).unwrap();
                assert!(!decoded.has_alpha, "{format:?}/{compression:?} must be RGB");
                assert_eq!((decoded.width, decoded.height, decoded.bit_depth), (96, 64, 8));
                assert_eq!(decoded.to_srgb8().data, img.data, "{format:?}/{compression:?} pixels");
                assert_eq!(decoded.icc.as_deref(), Some(icc::write_named(NamedSpace::Srgb).as_slice()));
                assert!(decoded.xmp.as_deref().unwrap_or_default().contains("Synthetic export test"));
                if format == ExportFormat::Png {
                    assert_eq!(bytes[25], 2, "PNG IHDR declares RGB");
                }
                if format != ExportFormat::Tiff {
                    break;
                }
            }
        }
    }

    #[test]
    fn eight_bit_exports_preserve_real_alpha() {
        let mut img = test_image();
        img.data[0][3] = 128;
        for format in [ExportFormat::Png, ExportFormat::Tiff, ExportFormat::Webp] {
            let bytes = encode_image(&img, &ExportOptions { format, ..Default::default() }).unwrap();
            let decoded = lightcraft_codecs::decode(&bytes, Default::default()).unwrap();
            assert!(decoded.has_alpha, "{format:?} must retain real alpha");
            assert_eq!(decoded.to_srgb8().data, img.data, "{format:?} samples");
        }
    }

    #[test]
    fn formats_have_magic() {
        let img = test_image();
        for (f, magic) in
            [(ExportFormat::Jpeg, &b"\xFF\xD8"[..]), (ExportFormat::Png, b"\x89PNG"), (ExportFormat::Tiff, b"II"), (ExportFormat::Webp, b"RIFF")]
        {
            let b = encode_image(&img, &ExportOptions { format: f, ..Default::default() }).unwrap();
            assert!(b.starts_with(magic), "{f:?}");
        }
    }

    #[test]
    fn size_limit_respected() {
        let img = test_image();
        let full = encode_image(&img, &ExportOptions { quality: 100, ..Default::default() }).unwrap();
        let kb = (full.len() / 1024 / 2).max(2) as u32;
        let b = encode_image(&img, &ExportOptions { quality: 100, limit_kb: Some(kb), ..Default::default() }).unwrap();
        assert!(b.len() <= kb as usize * 1024);
    }

    #[test]
    fn hdr_jpeg_size_limit_respected() {
        let img = Rgba8::from_fn(256, 192, |x, y| {
            let n = (x as u32).wrapping_mul(73856093) ^ (y as u32).wrapping_mul(19349663);
            [n as u8, (n >> 8) as u8, (n >> 16) as u8, 255]
        });
        let hdr = DeepImage {
            width: img.width,
            height: img.height,
            space: OutputSpace::Srgb,
            samples: DeepSamples::F32(
                img.data.iter().flat_map(|p| p[..3].iter().map(|v| lightcraft_color::transfer::decode_srgb8(*v) * 4.0)).collect(),
            ),
        };
        let opts = ExportOptions { hdr: true, quality: 100, limit_kb: Some(u32::MAX), ..Default::default() };
        let full = encode_gain_map_jpeg(&img, &hdr, &opts, None).unwrap();
        let kb = (full.len() / 2048) as u32;
        let limited = encode_gain_map_jpeg(&img, &hdr, &ExportOptions { limit_kb: Some(kb), ..opts.clone() }, None).unwrap();
        assert!(limited.len() <= kb as usize * 1024, "{} bytes exceeds {kb} KB", limited.len());
        let decoded = lightcraft_codecs::gainmap::read_jpeg(&limited).unwrap();
        assert!(decoded.meta.alternate_headroom > 1.0, "the gain map must survive size limiting");
        let impossible = encode_gain_map_jpeg(&img, &hdr, &ExportOptions { limit_kb: Some(1), ..opts }, None);
        assert!(impossible.is_err_and(|e| e.contains("cannot fit within 1 KB")));
    }

    #[test]
    fn graphic_watermark_honours_exif_orientation() {
        let dir = std::env::temp_dir().join(format!("lc-wm-orientation-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let logo = Rgba8::from_fn(40, 20, |x, _| if x < 20 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
        for orientation in [2, 6] {
            let meta = Metadata { orientation: Some(lightcraft_geom::Orientation::from_exif(orientation)), ..Default::default() };
            let exif = lightcraft_meta::write_exif(&meta);
            let bytes = lightcraft_codecs::encode_jpeg(
                &EncodeImage::rgba8(&logo),
                100,
                ChromaSubsampling::S444,
                &EncodeMeta { exif: Some(&exif), ..Default::default() },
            )
            .unwrap();
            let path = dir.join(format!("oriented-{orientation}.jpg"));
            std::fs::write(&path, bytes).unwrap();
            let wm = Watermark {
                image: path.to_string_lossy().into(),
                image_width: if orientation == 6 { 0.25 } else { 0.5 },
                opacity: 1.0,
                inset_x: 0.0,
                inset_y: 0.0,
                ..Default::default()
            };
            let mut img = Rgba8::from_fn(80, 80, |_, _| [0, 0, 0, 255]);
            draw_watermark(&mut img, &wm);
            if orientation == 6 {
                assert!(img.get(70, 45)[0] > 240 && img.get(70, 75)[2] > 240, "EXIF 6: red above blue");
            } else {
                assert!(img.get(45, 70)[2] > 240 && img.get(75, 70)[0] > 240, "EXIF 2: blue left of red");
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn graphic_watermark_keeps_aspect_when_fitted_and_rotated() {
        let dir = std::env::temp_dir().join(format!("lc-wm-aspect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tall.png");
        let logo = Rgba8::from_fn(40, 160, |_, _| [255, 0, 0, 255]);
        std::fs::write(&path, lightcraft_codecs::encode_png(&EncodeImage::rgba8(&logo), &EncodeMeta::default()).unwrap()).unwrap();
        for (rotation, expected) in [(0, (20, 80)), (90, (120, 30)), (180, (20, 80)), (270, (120, 30))] {
            let wm = Watermark {
                image: path.to_string_lossy().into(),
                image_width: 0.5,
                opacity: 1.0,
                inset_x: 0.0,
                inset_y: 0.0,
                rotation,
                ..Default::default()
            };
            let mut img = Rgba8::from_fn(120, 80, |_, _| [0, 0, 0, 255]);
            draw_watermark(&mut img, &wm);
            let lit: Vec<_> = img.data.iter().enumerate().filter(|(_, p)| p[0] > 200).map(|(i, _)| (i % 120, i / 120)).collect();
            let bounds = (
                lit.iter().map(|p| p.0).max().unwrap() - lit.iter().map(|p| p.0).min().unwrap() + 1,
                lit.iter().map(|p| p.1).max().unwrap() - lit.iter().map(|p| p.1).min().unwrap() + 1,
            );
            assert_eq!(bounds, expected, "{rotation} degrees: fitting must preserve the logo's aspect ratio");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn graphic_watermark_fit_preserves_anchor_insets() {
        let dir = std::env::temp_dir().join(format!("lc-wm-insets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tall.png");
        let logo = Rgba8::from_fn(40, 160, |_, _| [255, 0, 0, 255]);
        std::fs::write(&path, lightcraft_codecs::encode_png(&EncodeImage::rgba8(&logo), &EncodeMeta::default()).unwrap()).unwrap();
        use Anchor::*;
        for (anchor, rotation, expected) in [
            (BottomRight, 90, (0, 44, 111, 71)),
            (TopLeft, 270, (8, 8, 119, 35)),
            (Right, 90, (0, 26, 111, 53)),
            (BottomRight, 0, (94, 0, 111, 71)),
            (TopLeft, 180, (8, 8, 25, 79)),
            (Bottom, 0, (51, 0, 68, 71)),
            // Centered axes ignore their inset rather than shrinking the image unnecessarily.
            (Center, 90, (0, 25, 119, 54)),
            (Top, 90, (0, 8, 119, 37)),
        ] {
            let wm = Watermark {
                image: path.to_string_lossy().into(),
                image_width: 0.5,
                opacity: 1.0,
                inset_x: 0.1,
                inset_y: 0.1,
                rotation,
                anchor,
                ..Default::default()
            };
            let mut img = Rgba8::from_fn(120, 80, |_, _| [0, 0, 0, 255]);
            draw_watermark(&mut img, &wm);
            let lit: Vec<_> = img.data.iter().enumerate().filter(|(_, p)| p[0] > 200).map(|(i, _)| (i % 120, i / 120)).collect();
            let bounds = (
                lit.iter().map(|p| p.0).min().unwrap(),
                lit.iter().map(|p| p.1).min().unwrap(),
                lit.iter().map(|p| p.0).max().unwrap(),
                lit.iter().map(|p| p.1).max().unwrap(),
            );
            assert_eq!(bounds, expected, "{anchor:?}, {rotation} degrees");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sharpen_increases_edge_contrast() {
        let mut img = Rgba8::new(8, 8);
        for (i, p) in img.data.iter_mut().enumerate() {
            let v = if i % 8 < 4 { 80 } else { 170 };
            *p = [v, v, v, 255];
        }
        output_sharpen(&mut img, SharpenFor::Matte, SharpenAmount::High);
        assert!(img.data[3][0] < 80 && img.data[4][0] > 170);
    }

    /// Exported files carry keywords as Lightroom Classic writes them: names flat in `dc:subject`
    /// (with the keywords containing them), full paths in `lr:hierarchicalSubject`, and nothing of
    /// a keyword left out of export. They used to carry the `a|b` paths in `dc:subject`.
    #[test]
    fn exported_keywords_follow_their_options() {
        use lightcraft_catalog::{Catalog, Op, Photo, PhotoId, Source, keywords::KeywordInfo};
        let mut c = Catalog::new();
        c.apply(Op::SetKeyword { path: "Places".into(), info: Some(KeywordInfo { include_on_export: false, ..KeywordInfo::default() }) }).unwrap();
        c.apply(Op::SetKeyword { path: "draft".into(), info: Some(KeywordInfo { include_on_export: false, ..KeywordInfo::default() }) }).unwrap();
        let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.jpg", "jpeg", 10, 10, "2026-09-30T00:00:00");
        p.meta.keywords = vec!["Places|Lisbon".into(), "travel|Italy".into(), "draft".into()];
        let m = export_metadata(&p, &c, &ExportOptions::default()).unwrap();
        assert_eq!(m.keywords, ["Lisbon", "Italy", "travel"]);
        assert_eq!(m.hierarchical_keywords, ["Places|Lisbon", "travel|Italy"]);
        let jpg = encode_with_metadata(&test_image(), &ExportOptions::default(), Some(&m)).unwrap();
        let back = lightcraft_meta::extract(&jpg);
        assert_eq!((back.keywords, back.hierarchical_keywords), (m.keywords.clone(), m.hierarchical_keywords.clone()));
    }

    #[test]
    fn metadata_policies() {
        use lightcraft_catalog::{Catalog, Photo, PhotoId, Source};
        let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.jpg", "jpeg", 10, 10, "2026-09-30T00:00:00");
        p.meta.camera = "Synthetic X2".into();
        p.meta.copyright = "(c) Me".into();
        p.meta.copyright_status = lightcraft_catalog::CopyrightStatus::Copyrighted;
        p.meta.usage_terms = "Editorial use only".into();
        p.meta.copyright_url = "https://example.com/rights".into();
        p.meta.shutter = "1/250".into();
        p.meta.gps = Some((43.0, -110.0));
        let all = export_metadata(&p, &Catalog::new(), &ExportOptions::default()).unwrap();
        assert_eq!(all.model.as_deref(), Some("Synthetic X2"));
        assert!((all.exposure_time.unwrap() - 0.004).abs() < 1e-9);
        assert!(all.gps.is_some());
        let o = ExportOptions { metadata: MetadataPolicy::AllExceptCamera, remove_location: true, ..Default::default() };
        let m = export_metadata(&p, &Catalog::new(), &o).unwrap();
        assert!(m.model.is_none() && m.gps.is_none() && m.copyright.is_some());
        let c = export_metadata(&p, &Catalog::new(), &ExportOptions { metadata: MetadataPolicy::Copyright, ..Default::default() }).unwrap();
        assert!(c.model.is_none() && c.gps.is_none() && c.copyright.as_deref() == Some("(c) Me"));
        // "copyright only" keeps all the copyright info: status, usage terms, info URL
        assert_eq!(
            (c.copyright_marked, c.usage_terms.as_deref(), c.copyright_url.as_deref()),
            (Some(true), Some("Editorial use only"), Some("https://example.com/rights"))
        );
        assert!(export_metadata(&p, &Catalog::new(), &ExportOptions { metadata: MetadataPolicy::None, ..Default::default() }).is_none());
        // embedded and readable back from the JPEG
        let jpg = encode_with_metadata(&test_image(), &ExportOptions::default(), Some(&all)).unwrap();
        let back = lightcraft_meta::extract(&jpg);
        assert_eq!(back.model.as_deref(), Some("Synthetic X2"));
        assert_eq!(back.copyright.as_deref(), Some("(c) Me"));
        assert_eq!((back.copyright_marked, back.usage_terms.as_deref()), (Some(true), Some("Editorial use only")));
        assert_eq!(back.copyright_url.as_deref(), Some("https://example.com/rights"));
        assert!(back.gps.is_some());
    }

    #[test]
    fn watermark_draws_in_the_anchored_corner_only() {
        let mut img = Rgba8::new(400, 300);
        for p in img.data.iter_mut() {
            *p = [0, 0, 0, 255];
        }
        let o = ExportOptions::from_json(&serde_json::json!({"watermark": {"text": "LightCraft", "size": 0.08, "opacity": 1.0, "shadow": false}}));
        let wm = o.watermark.clone().unwrap();
        draw_watermark(&mut img, &wm);
        let lit = |x0: usize, x1: usize, y0: usize, y1: usize| {
            (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| img.get(x, y)[0] > 128).count()
        };
        assert!(lit(200, 400, 225, 300) > 100, "bottom-right has text");
        assert!(lit(0, 200, 0, 150) == 0, "top-left untouched");
        // string shorthand
        assert_eq!(ExportOptions::from_json(&serde_json::json!({"watermark": "© Me"})).watermark.unwrap().text, "© Me");
        assert!(ExportOptions::from_json(&serde_json::json!({"watermark": ""})).watermark.is_none());
    }

    /// Horizontal and vertical margins move a text watermark separately; settings saved with one
    /// `inset` (before the two margins) read as both, and either new key overrides it.
    #[test]
    fn watermark_insets_are_horizontal_and_vertical() {
        // gaps between the drawn text and the right and bottom edges of a 400 × 300 image
        let gaps = |wm: &Watermark| {
            let mut img = Rgba8::new(400, 300);
            draw_watermark(&mut img, wm);
            let lit: Vec<(usize, usize)> = (0..300).flat_map(|y| (0..400).map(move |x| (x, y))).filter(|&(x, y)| img.get(x, y)[0] > 128).collect();
            let right = lit.iter().map(|p| p.0).max().unwrap();
            let bottom = lit.iter().map(|p| p.1).max().unwrap();
            (399 - right as i64, 299 - bottom as i64)
        };
        let base = Watermark { text: "LC".into(), size: 0.1, opacity: 1.0, shadow: false, inset_x: 0.0, inset_y: 0.0, ..Default::default() };
        let (r0, b0) = gaps(&base);
        // 0.2 and 0.1 of the 300 px short edge
        let (r1, b1) = gaps(&Watermark { inset_x: 0.2, ..base.clone() });
        assert!((r1 - r0 - 60).abs() <= 1 && (b1 - b0).abs() <= 1, "horizontal: {r0},{b0} → {r1},{b1}");
        let (r2, b2) = gaps(&Watermark { inset_y: 0.1, ..base.clone() });
        assert!((r2 - r0).abs() <= 1 && (b2 - b0 - 30).abs() <= 1, "vertical: {r0},{b0} → {r2},{b2}");
        // the old single margin
        let wm = |w: serde_json::Value| ExportOptions::from_json(&json!({"watermark": w})).watermark.unwrap();
        let old = wm(json!({"text": "LC", "inset": 0.1}));
        assert_eq!((old.inset_x, old.inset_y), (0.1, 0.1));
        let mixed = wm(json!({"text": "LC", "inset": 0.1, "insetY": 0.3}));
        assert_eq!((mixed.inset_x, mixed.inset_y), (0.1, 0.3));
        assert!(ExportOptions::from_params(&json!({"watermark": {"text": "LC", "inset": 0.1}})).is_ok());
        // saved and read back as the two margins
        let o = ExportOptions { watermark: Some(Watermark { inset_x: 0.05, inset_y: 0.15, ..base }), ..Default::default() };
        let saved = o.to_json();
        assert!(saved["watermark"].get("inset").is_none(), "{saved}");
        assert_eq!(ExportOptions::from_json(&saved).watermark.map(|w| (w.inset_x, w.inset_y)), Some((0.05, 0.15)));
    }

    /// A turned text watermark is placed by its turned box: taller than wide at 90° and 270°, the
    /// same corner and insets, and an unknown angle draws it unturned.
    #[test]
    fn watermark_rotation_turns_the_text_in_its_corner() {
        let lit = |wm: &Watermark| {
            let mut img = Rgba8::new(400, 300);
            draw_watermark(&mut img, wm);
            let on: Vec<(usize, usize)> = (0..300).flat_map(|y| (0..400).map(move |x| (x, y))).filter(|&(x, y)| img.get(x, y)[0] > 128).collect();
            let (x0, x1) = (on.iter().map(|p| p.0).min().unwrap(), on.iter().map(|p| p.0).max().unwrap());
            let (y0, y1) = (on.iter().map(|p| p.1).min().unwrap(), on.iter().map(|p| p.1).max().unwrap());
            (x0, x1, y0, y1, img)
        };
        let base = Watermark { text: "LightCraft".into(), size: 0.08, opacity: 1.0, shadow: false, inset_x: 0.0, inset_y: 0.0, ..Default::default() };
        let (a0, a1, b0, b1, flat) = lit(&base);
        assert!(a1 - a0 > 2 * (b1 - b0), "unturned text is wide");
        for rotation in [90, 270] {
            let (x0, x1, y0, y1, _) = lit(&Watermark { rotation, ..base.clone() });
            assert!(y1 - y0 > 2 * (x1 - x0), "{rotation}°: tall");
            assert!(x1 >= 390 && y1 >= 290, "{rotation}°: still in the bottom-right corner ({x1}, {y1})");
            // 0.1 × 300 px further from the right edge
            let (_, moved, ..) = lit(&Watermark { rotation, inset_x: 0.1, ..base.clone() });
            assert!((x1 as i64 - moved as i64 - 30).abs() <= 1, "{rotation}°: {x1} → {moved}");
        }
        let (x0, x1, y0, y1, _) = lit(&Watermark { rotation: 180, ..base.clone() });
        assert!(((x1 - x0) as i64 - (a1 - a0) as i64).abs() <= 2 && ((y1 - y0) as i64 - (b1 - b0) as i64).abs() <= 2, "180°: same size");
        assert!(x1 >= 390 && y1 >= 290, "180°: same corner");
        let (.., odd) = lit(&Watermark { rotation: 45, ..base.clone() });
        assert_eq!(odd.data, flat.data, "an angle that isn't a quarter turn draws unturned");
        // saved and read back; older settings have none
        let o = ExportOptions { watermark: Some(Watermark { rotation: 270, ..base }), ..Default::default() };
        assert_eq!(ExportOptions::from_json(&o.to_json()).watermark.map(|w| w.rotation), Some(270));
        assert_eq!(ExportOptions::from_json(&json!({"watermark": {"text": "x"}})).watermark.map(|w| w.rotation), Some(0));
    }

    /// A preview keeps the export's 6 px minimum text size as a share of the photo: a watermark
    /// clamped to 6 px in a 150 px export covers the same share of a 600 px preview.
    #[test]
    fn watermark_preview_scales_the_minimum_text_size() {
        let wm = Watermark { text: "LightCraft".into(), size: 0.01, opacity: 1.0, shadow: false, ..Default::default() };
        let rows = |img: &Rgba8| (0..img.height).filter(|&y| (0..img.width).any(|x| img.get(x, y)[0] > 64)).count() as f32;
        let mut export = Rgba8::new(200, 150);
        draw_watermark(&mut export, &wm);
        let mut preview = Rgba8::new(800, 600);
        draw_watermark_preview(&mut preview, &wm, 150);
        let mut unscaled = Rgba8::new(800, 600);
        draw_watermark(&mut unscaled, &wm);
        let (export_share, preview_share) = (rows(&export) / 150.0, rows(&preview) / 600.0);
        assert!((preview_share - export_share).abs() < 0.02, "export {export_share}, preview {preview_share}");
        assert!(rows(&unscaled) < rows(&preview) / 2.0, "the unscaled 6 px floor would draw it too small");
        // drawn at the export's own size, the preview is the export
        let mut same = Rgba8::new(200, 150);
        draw_watermark_preview(&mut same, &wm, 150);
        assert_eq!(same.data, export.data);
    }

    fn vertical_japanese() -> Watermark {
        let options =
            ExportOptions::from_json(&json!({"watermark": {"text": "日本語", "vertical": true, "size": 0.1, "shadow": false, "opacity": 1.0}}));
        options.watermark.unwrap()
    }

    #[test]
    fn japanese_watermark_options_and_legacy_defaults() {
        let old = ExportOptions::from_json(&json!({"watermark": {"text": "日本語"}}));
        assert!(!old.watermark.unwrap().vertical);
        let wm = vertical_japanese();
        assert!(wm.vertical);
        let round: Watermark = serde_json::from_value(serde_json::to_value(&wm).unwrap()).unwrap();
        assert!(round.vertical);
    }

    /// Built without craft-fonts, a Japanese watermark still lays out and draws (Inter's
    /// missing-glyph boxes), and Latin text is unaffected.
    #[test]
    fn watermarks_work_without_craft_fonts() {
        assert_eq!(watermark_fonts(&[]).len(), 1, "Inter only");
        for wm in [vertical_japanese(), Watermark { text: "LightCraft 日本語".into(), ..Watermark::default() }] {
            let mut covered = 0usize;
            watermark_coverage(400, 300, &wm, &[], 1.0, |_, _, k, _| covered += usize::from(k > 0.0));
            assert!(covered > 0, "{:?} draws something", wm.text);
        }
    }

    #[test]
    fn biz_ud_vertical_watermarks_place_punctuation_at_the_top_right() {
        use ab_glyph::Font;
        let Some(entry) = crate::fonts::CRAFT_FONTS.iter().find(|f| f.family == "BIZ UDMincho") else {
            lightcraft_testkit::skip!(Fonts, "built without BIZ UDMincho from craft-fonts");
            return;
        };
        let font = ab_glyph::FontRef::try_from_slice(entry.bytes).unwrap();
        // BIZ UDMincho has no Unicode presentation-form characters: the old fallback drew 、。.
        assert_eq!(font.glyph_id('︑').0, 0);
        assert_eq!(font.glyph_id('︒').0, 0);
        let data = harfrust::ShaperData::new(&harfrust::FontRef::new(entry.bytes).unwrap());
        for ch in ['、', '。'] {
            let glyph = watermark_cell_glyphs(&font, &data, &ch.to_string(), true, 100.0, 0.0, 0.0).unwrap().remove(0);
            assert_ne!(glyph.id, font.glyph_id(ch), "the font's vertical alternate for {ch}");
            let rect = font.outline_glyph(glyph).unwrap().px_bounds();
            assert!(rect.min.x > 50.0 && rect.max.y < 50.0, "{ch} in the upper right: {rect:?}");
        }
        let craft = Box::leak(
            vec![crate::fonts::CraftFont { family: entry.family, style: entry.style, scripts: entry.scripts, bytes: entry.bytes }].into_boxed_slice(),
        );
        let wm = Watermark {
            text: "、。".into(),
            vertical: true,
            size: 0.1,
            anchor: Anchor::TopLeft,
            inset_x: 0.0,
            inset_y: 0.0,
            shadow: false,
            opacity: 1.0,
            ..Default::default()
        };
        let mut pixels = Vec::new();
        watermark_coverage(400, 300, &wm, craft, 1.0, |x, y, k, _| {
            if k > 0.5 {
                pixels.push((x, y));
            }
        });
        assert!(!pixels.is_empty());
        for (x, y) in pixels {
            assert!(x > 15 && y % 30 < 15, "actual export coverage at the upper right of the 30 px cell: {x}, {y}");
        }
    }

    #[test]
    fn vertical_watermarks_keep_combining_marks_in_one_cell() {
        let coverage = |text: &str, craft| {
            let wm = Watermark {
                text: text.into(),
                vertical: true,
                size: 0.1,
                anchor: Anchor::Center,
                inset_x: 0.0,
                inset_y: 0.0,
                shadow: false,
                opacity: 1.0,
                ..Default::default()
            };
            let mut pixels = vec![0.0f32; 400 * 300];
            watermark_coverage(400, 300, &wm, craft, 1.0, |x, y, k, _| pixels[y * 400 + x] = k);
            assert!(pixels.iter().any(|&k| k > 0.0));
            pixels
        };
        let compare = |a: &str, b: &str, craft| {
            let (a_pixels, b_pixels) = (coverage(a, craft), coverage(b, craft));
            let difference = a_pixels.iter().zip(&b_pixels).position(|(a, b)| a != b);
            assert!(difference.is_none(), "{a:?} vs {b:?}: first differing pixel {difference:?}");
        };
        for (composed, decomposed) in [("éA", "e\u{301}A"), ("Å\nA", "A\u{30a}\nA"), ("A\nB", "A\r\nB"), ("日A", "日\u{e0100}A")] {
            compare(composed, decomposed, &[]);
        }
        if crate::fonts::CRAFT_FONTS.iter().any(|f| f.family == "BIZ UDMincho") {
            for (composed, decomposed) in [("が日", "か\u{3099}日"), ("ぱ\n日", "は\u{309a}\n日")] {
                compare(composed, decomposed, crate::fonts::CRAFT_FONTS);
            }
        } else {
            lightcraft_testkit::skip!(Fonts, "Japanese comparisons: built without BIZ UDMincho from craft-fonts");
        }
    }

    #[test]
    fn upright_watermark_cell_retains_multiple_glyphs() {
        use ab_glyph::{Font, ScaleFont};
        let font = ab_glyph::FontRef::try_from_slice(WATERMARK_FONT).unwrap();
        let data = harfrust::ShaperData::new(&harfrust::FontRef::new(WATERMARK_FONT).unwrap());
        // There is no precomposed A with a combining long solidus overlay.
        let glyphs = watermark_cell_glyphs(&font, &data, "A\u{338}", false, 30.0, 0.0, 0.0).unwrap();
        assert_eq!(glyphs.len(), 2, "retain the base and the separately positioned combining glyph");
        let baseline = font.as_scaled(30.0).ascent();
        for glyph in glyphs {
            assert!((glyph.position.y - baseline).abs() < 1.0, "no extra vertical cell for the mark");
            assert!(font.outline_glyph(glyph).is_some(), "both glyphs have ink");
        }
    }

    #[test]
    fn japanese_watermarks_support_vertical_columns() {
        use ab_glyph::Font;
        let fonts = watermark_fonts(crate::fonts::CRAFT_FONTS);
        if fonts.len() < 2 {
            lightcraft_testkit::skip!(Fonts, "built without CRAFT_FONTS_DIR, so there is no Japanese watermark face");
            return;
        }
        for c in "日本語の文字".chars() {
            assert!(fonts[1..].iter().any(|f| f.glyph_id(c).0 != 0), "a craft-fonts face has {c}");
        }
        let wm = vertical_japanese();
        let mut img = Rgba8::new(400, 300);
        img.data.fill([0, 0, 0, 255]);
        draw_watermark(&mut img, &wm);
        let lit: Vec<_> = (0..300).flat_map(|y| (0..400).map(move |x| (x, y))).filter(|&(x, y)| img.get(x, y)[0] > 64).collect();
        assert!(lit.len() > 100);
        let w = lit.iter().map(|p| p.0).max().unwrap() - lit.iter().map(|p| p.0).min().unwrap();
        let h = lit.iter().map(|p| p.1).max().unwrap() - lit.iter().map(|p| p.1).min().unwrap();
        assert!(h > w * 2, "vertical Japanese must extend down the column");
        let two = Watermark { text: "日\n本".into(), anchor: Anchor::TopLeft, inset_x: 0.0, inset_y: 0.0, ..wm };
        img.data.fill([0, 0, 0, 255]);
        draw_watermark(&mut img, &two);
        for (text, dx) in [("本", 0), ("日", 30)] {
            let mut single = Rgba8::new(400, 300);
            single.data.fill([0, 0, 0, 255]);
            draw_watermark(&mut single, &Watermark { text: text.into(), ..two.clone() });
            for y in 0..30 {
                for x in 0..30 {
                    assert_eq!(img.get(x + dx, y), single.get(x, y), "newlines move columns left");
                }
            }
        }
    }

    #[test]
    fn graphic_watermark_is_placed_scaled_and_blended() {
        let dir = std::env::temp_dir().join(format!("lc-wm-logo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let logo_path = dir.join("logo.png");
        // 40×20: left half opaque red, right half fully transparent
        let logo = Rgba8::from_fn(40, 20, |x, _| if x < 20 { [255, 0, 0, 255] } else { [0, 255, 0, 0] });
        let png = lightcraft_codecs::encode_png(&EncodeImage::rgba8(&logo), &EncodeMeta::default()).unwrap();
        std::fs::write(&logo_path, png).unwrap();
        let mut img = Rgba8::new(400, 300);
        img.data.iter_mut().for_each(|p| *p = [0, 0, 0, 255]);
        let wm = Watermark {
            image: logo_path.to_string_lossy().into(),
            image_width: 0.25,
            opacity: 1.0,
            inset_x: 0.0,
            inset_y: 0.0,
            ..Default::default()
        };
        draw_watermark(&mut img, &wm);
        // 100 px wide × 50 px high in the bottom-right corner
        assert!(img.get(310, 280)[0] > 200, "opaque half: {:?}", img.get(310, 280));
        assert_eq!(img.get(390, 280), [0, 0, 0, 255], "transparent half leaves the photo");
        assert_eq!(img.get(310, 240), [0, 0, 0, 255], "above the logo");
        assert_eq!(img.get(10, 10), [0, 0, 0, 255]);
        // 30 px from the right edge and 15 px from the bottom: x 270..370, y 235..285
        let mut img = Rgba8::new(400, 300);
        img.data.iter_mut().for_each(|p| *p = [0, 0, 0, 255]);
        draw_watermark(&mut img, &Watermark { inset_x: 0.1, inset_y: 0.05, ..wm.clone() });
        assert!(img.get(272, 283)[0] > 200, "inside the moved logo: {:?}", img.get(272, 283));
        assert_eq!(img.get(267, 283), [0, 0, 0, 255], "left of the logo");
        assert_eq!(img.get(272, 288), [0, 0, 0, 255], "below the logo");
        // turned: 180° puts the opaque half on the right; 90° clockwise puts it on top of a 50 × 100 box
        let turned = |rotation: u16| {
            let mut img = Rgba8::new(400, 300);
            img.data.iter_mut().for_each(|p| *p = [0, 0, 0, 255]);
            draw_watermark(&mut img, &Watermark { rotation, ..wm.clone() });
            img
        };
        let half = turned(180);
        assert!(half.get(390, 280)[0] > 200 && half.get(310, 280) == [0, 0, 0, 255]);
        let quarter = turned(90);
        assert!(quarter.get(375, 220)[0] > 200, "top half of the turned logo: {:?}", quarter.get(375, 220));
        assert_eq!(quarter.get(375, 280), [0, 0, 0, 255], "its transparent half below");
        assert_eq!(quarter.get(340, 220), [0, 0, 0, 255], "left of the 50 px wide box");
        assert!(quarter.get(352, 202)[0] > 200 && quarter.get(398, 248)[0] > 200, "the whole box is covered");
        // three quarter turns: the opaque half at the bottom
        let three = turned(270);
        assert!(three.get(375, 280)[0] > 200 && three.get(375, 220) == [0, 0, 0, 255]);
        // an image-only watermark survives the params (no text needed)
        let o = ExportOptions::from_json(&json!({"watermark": {"image": logo_path.to_string_lossy(), "imageWidth": 0.1}}));
        assert_eq!(o.watermark.as_ref().map(|w| w.image_width), Some(0.1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn naming_and_params() {
        let o = ExportOptions::from_json(&serde_json::json!({"format": "jpg", "quality": 150, "longEdge": 2048, "naming": "{name}-{seq}"}));
        assert_eq!(o.format, ExportFormat::Jpeg);
        assert_eq!(o.quality, 100);
        assert_eq!(o.resize, Some(Resize { dont_enlarge: true, height: 0, ..Resize::long_edge(2048) }));
        assert_eq!(o.file_name_for(&named("IMG/1.png"), 7), "IMG_1-007.jpg");
        assert_eq!(o.ppi, 240);
        let orig = ExportOptions { format: ExportFormat::Original, naming: "{name}-{seq}".into(), ..Default::default() };
        assert_eq!(orig.file_name_for(&named("DSC_1.NEF"), 2), "DSC_1-002.NEF");
        assert_eq!(orig.file_name_for(&named("noext"), 1), "noext-001");
        assert_eq!(ExportOptions { format: ExportFormat::Dng, ..Default::default() }.file_name_for(&named("a.cr2"), 1), "a.dng");
    }

    fn named(file_name: &str) -> lightcraft_catalog::Photo {
        use lightcraft_catalog::{Photo, PhotoId, Source};
        Photo::new(PhotoId(1), Source::Demo { scene: 0 }, file_name, "", 1, 1, "2026-01-01T00:00:00")
    }

    /// Export naming takes the same tokens as Rename Photos and import renaming.
    #[test]
    fn naming_uses_the_rename_tokens() {
        let mut p = named("DSC_0815.NEF");
        p.captured = Some("2026-09-30T14:05:09".into());
        p.meta.camera = "Model X/2".into();
        p.meta.title = "Harbour".into();
        p.rating = 5;
        let o = |naming: &str| ExportOptions { naming: naming.into(), start_number: 9, ..Default::default() };
        assert_eq!(o("{date:%Y-%m-%d}_{title}_{seq:2}").file_name_for(&p, 1), "2026-09-30_Harbour_09.jpg");
        assert_eq!(o("{camera}-{num}-{rating}").file_name_for(&p, 1), "Model X_2-0815-5.jpg", "path separators become _");
        assert_eq!(o("{name}-{seq}").file_name_for(&p, 3), "DSC_0815-011.jpg", "a bare {{seq}} keeps its 3 digits");
        assert_eq!(o("{ext}_{name}").file_name_for(&p, 1), "NEF_DSC_0815.jpg");
        // nothing left after expanding: the original name
        p.meta.title.clear();
        assert_eq!(o("{title}").file_name_for(&p, 1), "DSC_0815.jpg");
    }

    #[test]
    fn params_round_trip() {
        let o = ExportOptions {
            format: ExportFormat::Tiff,
            quality: 70,
            resize: Some(Resize { mode: ResizeMode::Megapixels, value: 12.5, height: 0, dont_enlarge: false }),
            ppi: 300,
            limit_kb: Some(500),
            sharpen: SharpenFor::Glossy,
            naming: "{date}-{name}-{seq}".into(),
            start_number: 42,
            subfolder: "Client".into(),
            conflict: Conflict::Skip,
            tiff_compression: TiffCompression::Lzw,
            metadata: MetadataPolicy::Copyright,
            remove_location: true,
            watermark: Some(Watermark { text: "© LC".into(), ..Default::default() }),
            color_space: OutputSpace::ProPhoto,
            bit_depth: Some(16),
            ..Default::default()
        };
        assert_eq!(ExportOptions::from_json(&o.to_json()), o);
        let full = ExportOptions::default();
        assert_eq!(ExportOptions::from_json(&full.to_json()), full);
        assert!(ExportOptions::has_size_param(&full.to_json()), "full size is explicit");
        let mut p = named("IMG.png");
        p.captured = Some("2026-09-30T10:00:00".into());
        assert_eq!(o.file_name_for(&p, 2), "20260930-IMG-043.tif");
    }

    #[test]
    fn resize_modes() {
        let size = |p: serde_json::Value, w: f64, h: f64| ExportOptions::from_json(&p).resize.map(|r| r.apply(w, h));
        // a 6000 × 4000 landscape and a 4000 × 6000 portrait
        assert_eq!(size(json!({"longEdge": 1500}), 6000.0, 4000.0), Some((1500, 1000)));
        assert_eq!(size(json!({"longEdge": 1500}), 4000.0, 6000.0), Some((1000, 1500)));
        assert_eq!(size(json!({"shortEdge": 1080}), 6000.0, 4000.0), Some((1620, 1080)));
        assert_eq!(size(json!({"width": 3000}), 4000.0, 6000.0), Some((3000, 4500)));
        assert_eq!(size(json!({"height": 1000}), 6000.0, 4000.0), Some((1500, 1000)));
        // W × H fits either orientation: the long edge gets the larger number
        assert_eq!(size(json!({"width": 1000, "height": 800}), 6000.0, 4000.0), Some((1000, 667)));
        assert_eq!(size(json!({"width": 1000, "height": 800}), 4000.0, 6000.0), Some((667, 1000)));
        assert_eq!(size(json!({"width": 1000, "height": 500}), 6000.0, 4000.0), Some((750, 500)));
        assert_eq!(size(json!({"width": 270, "height": 337, "fit": "dimensions"}), 480.0, 320.0), Some((337, 225)));
        // Width & Height (`fit: widthHeight`) binds each edge to its own number: W limits the width, H the height (#1620)
        assert_eq!(size(json!({"width": 270, "height": 337, "fit": "widthHeight"}), 480.0, 320.0), Some((270, 180)));
        assert_eq!(size(json!({"width": 270, "height": 337, "fit": "widthHeight"}), 320.0, 480.0), Some((225, 337)));
        assert_eq!(size(json!({"width": 240, "height": 300, "fit": "widthHeight"}), 480.0, 320.0), Some((240, 160)));
        // `fit` only matters with both numbers
        assert_eq!(size(json!({"width": 1000, "fit": "widthHeight"}), 6000.0, 4000.0), Some((1000, 667)));
        let (w, h) = size(json!({"megapixels": 6}), 6000.0, 4000.0).unwrap();
        assert!(((w * h) as f64 - 6e6).abs() < 6e6 * 0.002, "{w}×{h}");
        assert_eq!(size(json!({"percent": 25}), 6000.0, 4000.0), Some((1500, 1000)));
        // don't enlarge (default) vs enlarge
        assert_eq!(size(json!({"longEdge": 8000}), 6000.0, 4000.0), Some((6000, 4000)));
        assert_eq!(size(json!({"longEdge": 9000, "dontEnlarge": false}), 6000.0, 4000.0), Some((9000, 6000)));
        // full size / absent
        assert_eq!(size(json!({"longEdge": 0}), 6000.0, 4000.0), None);
        assert_eq!(size(json!({}), 6000.0, 4000.0), None);
        assert!(ExportOptions::has_size_param(&json!({"longEdge": 0})) && !ExportOptions::has_size_param(&json!({"quality": 3})));
        // the `resize` object form round-trips (what the dialog and prefs store)
        let r = Resize { mode: ResizeMode::ShortEdge, value: 720.0, height: 0, dont_enlarge: false };
        let back = ExportOptions::from_json(&ExportOptions::resize_json(Some(&r))).resize;
        assert_eq!(back, Some(r));
        let r = Resize { mode: ResizeMode::WidthHeight, value: 270.0, height: 337, dont_enlarge: true };
        assert_eq!(ExportOptions::from_json(&ExportOptions::resize_json(Some(&r))).resize, Some(r));
        assert_eq!(ExportOptions::from_json(&ExportOptions::resize_json(None)).resize, None);
        assert_eq!(ExportOptions::from_json(&json!({"ppi": 300})).ppi, 300);
        // never larger than the encoders' limit, never empty
        assert_eq!(size(json!({"percent": 10000, "dontEnlarge": false}), 6000.0, 4000.0), Some((27713, 18475)), "512 MP at most");
        assert_eq!(size(json!({"width": 70000, "dontEnlarge": false}), 60000.0, 100.0), Some((65535, 109)), "65535 px an edge");
        assert_eq!(size(json!({"width": 1}), 6000.0, 40.0).map(|s| s.1), Some(1));
    }

    /// Typing 20000 for 2000 (or percent 10000) with Don't Enlarge off asked for gigapixels: the
    /// render's first plane alone (34 GB at 65535 × 43690) failed to allocate, which aborts the
    /// process. An enlargement stops at 512 megapixels; full-size and smaller exports of photos
    /// larger than that are untouched.
    #[test]
    fn enlargements_stop_at_512_megapixels() {
        let size = |p: serde_json::Value, w: f64, h: f64| ExportOptions::from_json(&p).resize.map(|r| r.apply(w, h)).unwrap();
        for p in [json!({"longEdge": 60000, "dontEnlarge": false}), json!({"megapixels": 5000}), json!({"percent": 10000, "dontEnlarge": false})] {
            let (w, h) = size(p.clone(), 6000.0, 4000.0);
            assert!((w * h) as f64 <= MAX_ENLARGED_PIXELS * 1.001, "{p}: {w}×{h}");
            assert!((w as f64 / h as f64 - 1.5).abs() < 0.001, "{p}: keeps the shape");
        }
        // below the ceiling an enlargement is as asked
        assert_eq!(size(json!({"longEdge": 12000, "dontEnlarge": false}), 6000.0, 4000.0), (12000, 8000));
        // a 600 MP panorama exports at full size, and smaller, as before; it is never enlarged
        assert_eq!(size(json!({"percent": 100, "dontEnlarge": false}), 30000.0, 20000.0), (30000, 20000));
        assert_eq!(size(json!({"percent": 50}), 30000.0, 20000.0), (15000, 10000));
        assert_eq!(size(json!({"percent": 200, "dontEnlarge": false}), 30000.0, 20000.0), (30000, 20000));
    }

    /// An enlarged export that needs more memory than the computer has is refused with a message,
    /// before it starts; everything else goes ahead.
    #[test]
    fn enlarged_exports_must_fit_the_computers_memory() {
        let gb = 1usize << 30;
        let why = too_big_for_memory((27713, 18475), 24_000_000, 20 * gb, Some(16 * gb)).unwrap();
        assert!(why.contains("27713 × 18475") && why.contains("20 GB") && why.contains("16 GB") && why.contains("smaller size"), "{why}");
        assert_eq!(too_big_for_memory((27713, 18475), 24_000_000, 20 * gb, Some(32 * gb)), None, "fits");
        assert_eq!(too_big_for_memory((6000, 4000), 24_000_000, 20 * gb, Some(gb)), None, "full size is never refused");
        assert_eq!(too_big_for_memory((3000, 2000), 24_000_000, 20 * gb, Some(gb)), None, "nor a smaller export");
        assert_eq!(too_big_for_memory((27713, 18475), 24_000_000, 20 * gb, None), None, "unknown memory");
    }

    #[test]
    fn resize_size_survives_the_render() {
        // the render fits the photo into the size `apply` gives; that must not cost a pixel (#1312)
        let cropped = |r: Resize, w: u32, h: u32, x1: f64| {
            let mut p = named("IMG.ARW");
            (p.width, p.height) = (w, h);
            std::sync::Arc::make_mut(&mut p.develop).crop.geometry.rect = lightcraft_geom::Rect::new(0.0, 0.0, x1, 1.0);
            let (bw, bh) = output_size(&p, &ExportOptions { resize: Some(r), ..Default::default() });
            lightcraft_pipeline::output_size(w as usize, h as usize, &p.develop, &lightcraft_pipeline::RenderRequest::fit(bw, bh))
        };
        let rendered = |r: Resize, w: u32, h: u32| cropped(r, w, h, 1.0);
        let mode = |mode: ResizeMode, value: f32, height: u32| Resize { mode, value, height, dont_enlarge: true };
        // a frame a hair narrower than 3:2 (a raw's default crop in floats) puts 1333 × 1.5 just under 1999.5
        let hair = 1.0 - 1e-9;
        assert_eq!(cropped(Resize::long_edge(2000), 6000, 4000, hair), (2000, 1333));
        assert_eq!(cropped(Resize::long_edge(2048), 6000, 4000, hair), (2048, 1365));
        assert_eq!(cropped(mode(ResizeMode::Width, 2000.0, 0), 6000, 4000, hair), (2000, 1333));
        assert_eq!(cropped(mode(ResizeMode::ShortEdge, 1333.0, 0), 6000, 4000, hair), (2000, 1333));
        assert_eq!(cropped(mode(ResizeMode::Height, 1333.0, 0), 6000, 4000, hair), (2000, 1333));
        assert_eq!(rendered(Resize::long_edge(2000), 4000, 6000), (1333, 2000));
        // 3:2, 4:3, 16:9, 1:1 and their portraits, every mode: the render keeps what `apply` says
        for (w, h) in [(6000u32, 4000u32), (6240, 4160), (4032, 3024), (5184, 3888), (3840, 2160), (6000, 3375), (4000, 4000)] {
            for (w, h) in [(w, h), (h, w)] {
                for value in [1000.0, 1080.0, 1333.0, 2000.0, 2048.0, 2500.0, 3000.0] {
                    for r in [
                        Resize::long_edge(value as u32),
                        mode(ResizeMode::ShortEdge, value, 0),
                        mode(ResizeMode::Width, value, 0),
                        mode(ResizeMode::Height, value, 0),
                        mode(ResizeMode::Dimensions, value, 1365),
                        mode(ResizeMode::WidthHeight, value, 1365),
                        mode(ResizeMode::Megapixels, value / 1000.0, 0),
                        mode(ResizeMode::Percent, value / 100.0, 0),
                    ] {
                        let want = r.apply(w.into(), h.into());
                        assert_eq!(rendered(r, w, h), want, "{r:?} on {w}×{h}");
                    }
                }
            }
        }
    }

    /// A photo more than 65535 times as long as it is short (a 70000 × 1 strip) made `scale`
    /// clamp with its lower bound above its upper one, which panics, in the export and in the
    /// Export dialog's preview on the UI thread. The encoders' limit wins, the short edge keeps a
    /// pixel.
    #[test]
    fn resize_survives_photos_too_long_for_the_encoders() {
        let modes = [
            json!({"longEdge": 2000}),
            json!({"shortEdge": 1}),
            json!({"width": 500, "height": 500}),
            json!({"megapixels": 1}),
            json!({"percent": 50}),
            json!({"percent": 1000, "dontEnlarge": false}),
        ];
        for p in modes {
            for (w, h) in [(70_000.0, 1.0), (1.0, 70_000.0), (1e9, 3.0), (65_536.0, 1.0)] {
                let r = ExportOptions::from_json(&p).resize.unwrap();
                let (ow, oh) = r.apply(w, h);
                assert!((1..=65_535).contains(&ow) && (1..=65_535).contains(&oh), "{p} on {w}×{h}: {ow}×{oh}");
                assert!(r.scale(w, h).is_finite());
            }
        }
        // (a short edge under a pixel would be scaled up to one, which the limit then caps)
        assert_eq!(Resize::long_edge(2000).apply(70_000.0, 1.0), (65_535, 1));
        assert_eq!(Resize::long_edge(2000).apply(1.0, 70_000.0), (1, 65_535));
    }

    /// A flat field of a saturated green inside Display P3 but outside sRGB, rendered into `space`.
    fn p3_green(space: OutputSpace) -> Rgba8 {
        use lightcraft_color::{DISPLAY_P3, REC2020};
        let c = DISPLAY_P3.to_space(&REC2020).apply_f32([0.04, 0.45, 0.04]);
        let src = lightcraft_raster::Rgb32f::filled(16, 16, c);
        let req = lightcraft_pipeline::RenderRequest { space, ..lightcraft_pipeline::RenderRequest::fit(16, 16) };
        lightcraft_pipeline::render(&src, &Default::default(), &Default::default(), &req).image
    }

    /// Decode `bytes` and return the centre pixel in linear sRGB primaries (unclamped), plus the
    /// recognised space of the embedded profile.
    fn decoded_in_srgb(bytes: &[u8]) -> ([f32; 3], Option<NamedSpace>) {
        let d = lightcraft_codecs::decode(bytes, Default::default()).expect("decodes");
        let m = d.space.to_space(&lightcraft_color::SRGB);
        (m.apply_f32(d.image.get(8, 8)), d.space.named)
    }

    #[test]
    fn p3_colour_survives_a_p3_export_and_is_clipped_in_srgb() {
        let o = |space| ExportOptions { format: ExportFormat::Png, color_space: space, ..Default::default() };
        let (p3, named) = decoded_in_srgb(&encode_image(&p3_green(OutputSpace::DisplayP3), &o(OutputSpace::DisplayP3)).unwrap());
        assert_eq!(named, Some(NamedSpace::DisplayP3));
        assert!(p3[0] < -0.03 || p3[2] < -0.03, "outside sRGB after a P3 round trip: {p3:?}");
        let (s, named) = decoded_in_srgb(&encode_image(&p3_green(OutputSpace::Srgb), &o(OutputSpace::Srgb)).unwrap());
        assert_eq!(named, Some(NamedSpace::Srgb));
        assert!(s.iter().all(|v| *v > -0.002), "{s:?}");
        // the green itself is about the same brightness either way
        assert!((p3[1] - s[1]).abs() < 0.15, "{p3:?} {s:?}");
    }

    #[test]
    fn every_space_embeds_its_own_profile_and_round_trips() {
        let grey = {
            let src = lightcraft_raster::Rgb32f::filled(16, 16, [0.18; 3]);
            let base = lightcraft_pipeline::RenderRequest::fit(16, 16);
            move |space| {
                lightcraft_pipeline::render(&src, &Default::default(), &Default::default(), &lightcraft_pipeline::RenderRequest { space, ..base })
                    .image
            }
        };
        let reference =
            decoded_in_srgb(&encode_image(&grey(OutputSpace::Srgb), &ExportOptions { format: ExportFormat::Tiff, ..Default::default() }).unwrap()).0;
        for space in OutputSpace::ALL {
            for format in [ExportFormat::Jpeg, ExportFormat::Png, ExportFormat::Tiff, ExportFormat::Webp] {
                let o = ExportOptions { format, color_space: space, ..Default::default() };
                let bytes = encode_image(&grey(space), &o).unwrap();
                let (px, named) = decoded_in_srgb(&bytes);
                assert_eq!(named, Some(named_space(space)), "{space:?} {format:?}");
                // a neutral grey decodes to the same linear value whatever the output space
                for c in 0..3 {
                    assert!((px[c] - reference[c]).abs() < 0.01, "{space:?} {format:?}: {px:?} vs {reference:?}");
                }
            }
            let icc = icc::write_named(named_space(space));
            let info = icc::parse(&icc).expect("our profile parses");
            assert_eq!(info.named, Some(named_space(space)));
            assert!(info.description.as_deref().is_some_and(|d| !d.is_empty()));
        }
        let o = ExportOptions { format: ExportFormat::Avif, color_space: OutputSpace::ProPhoto, ..Default::default() };
        assert_eq!(o.effective_space(), OutputSpace::Srgb);
        let o = ExportOptions::from_json(&serde_json::json!({"colorSpace": "displayP3"}));
        assert_eq!(o.color_space, OutputSpace::DisplayP3);
        assert_eq!(ExportOptions::from_json(&serde_json::json!({"colorSpace": "Adobe RGB (1998) compatible"})).color_space, OutputSpace::AdobeRgb);
    }

    #[test]
    fn srgb_watermark_colour_is_converted() {
        assert_eq!(srgb8_in(OutputSpace::Srgb, [10, 200, 30]), [10, 200, 30]);
        for s in OutputSpace::ALL {
            assert_eq!(srgb8_in(s, [255, 255, 255]), [255, 255, 255], "{s:?}");
            assert_eq!(srgb8_in(s, [0, 0, 0]), [0, 0, 0], "{s:?}");
        }
        let p3 = srgb8_in(OutputSpace::DisplayP3, [255, 0, 0]);
        assert!(p3[0] < 255 && p3[1] > 0, "{p3:?}");
    }

    /// A shallow horizontal grey ramp (few 8-bit levels), rendered at `depth`.
    fn ramp(depth: OutputDepth, space: OutputSpace) -> lightcraft_pipeline::Rendered {
        let src = lightcraft_raster::Rgb32f::from_fn(1024, 4, |x, _| [0.10 + 0.03 * x as f32 / 1023.0; 3]);
        let req = lightcraft_pipeline::RenderRequest { depth, space, ..lightcraft_pipeline::RenderRequest::fit(1024, 4) };
        lightcraft_pipeline::render(&src, &Default::default(), &Default::default(), &req)
    }

    fn distinct_levels(bytes: &[u8]) -> (usize, lightcraft_codecs::Decoded) {
        let d = lightcraft_codecs::decode(bytes, Default::default()).expect("decodes");
        let mut v: Vec<u32> = (0..d.image.width).map(|x| d.image.get(x, 1)[1].to_bits()).collect();
        v.dedup();
        (v.len(), d)
    }

    #[test]
    fn sixteen_bit_tiff_and_png_have_no_banding() {
        let tiff = |bit_depth| ExportOptions { format: ExportFormat::Tiff, bit_depth, ..Default::default() };
        assert_eq!(tiff(None).effective_depth(), OutputDepth::U16, "TIFF defaults to 16-bit");
        let r16 = ramp(OutputDepth::U16, OutputSpace::Srgb);
        assert!(matches!(r16.deep.as_ref().unwrap().samples, DeepSamples::U16(_)));
        let (levels16, d16) = distinct_levels(&encode_rendered(&r16, &tiff(None), None).unwrap());
        let r8 = ramp(OutputDepth::U8, OutputSpace::Srgb);
        let (levels8, _) = distinct_levels(&encode_rendered(&r8, &tiff(Some(8)), None).unwrap());
        eprintln!("gradient levels: 16-bit TIFF {levels16}, 8-bit TIFF {levels8}");
        assert!(levels8 < 40, "{levels8}");
        assert!(levels16 > 10 * levels8, "16-bit: {levels16} levels vs 8-bit: {levels8}");
        assert_eq!(d16.space.named, Some(NamedSpace::Srgb));
        // the 8-bit preview of a deep render matches the 8-bit render
        for (a, b) in r16.image.data.iter().zip(&r8.image.data) {
            assert!(a[1].abs_diff(b[1]) <= 1, "{a:?} {b:?}");
        }
        let png = ExportOptions { format: ExportFormat::Png, bit_depth: Some(16), color_space: OutputSpace::DisplayP3, ..Default::default() };
        let rp = ramp(OutputDepth::U16, OutputSpace::DisplayP3);
        let bytes = encode_rendered(&rp, &png, None).unwrap();
        assert_eq!(bytes[24], 16, "PNG IHDR bit depth");
        let (levels, d) = distinct_levels(&bytes);
        assert!(levels > 10 * levels8, "{levels}");
        assert_eq!(d.space.named, Some(NamedSpace::DisplayP3));
        // sharpening and watermarking work on deep images too
        let o = ExportOptions {
            sharpen: SharpenFor::Matte,
            watermark: Some(Watermark { text: "LC".into(), size: 0.5, ..Default::default() }),
            ..tiff(None)
        };
        assert!(encode_rendered(&r16, &o, None).is_ok());
    }

    #[test]
    fn float_tiff_is_linear_with_a_linear_profile() {
        let o = ExportOptions { format: ExportFormat::Tiff, bit_depth: Some(32), color_space: OutputSpace::ProPhoto, ..Default::default() };
        assert_eq!(o.effective_depth(), OutputDepth::F32Linear);
        let rf = ramp(OutputDepth::F32Linear, OutputSpace::ProPhoto);
        let (levels, df) = distinct_levels(&encode_rendered(&rf, &o, None).unwrap());
        assert!(levels > 500, "{levels}");
        assert_eq!(df.space.named, Some(NamedSpace::ProPhoto));
        assert!(df.space.trc.as_ref().is_some_and(|t| t[0].is_linear()), "{:?}", df.space.trc);
        // decodes to the same light as the 16-bit gamma-encoded export
        let o16 = ExportOptions { bit_depth: Some(16), ..o.clone() };
        let (_, d16) = distinct_levels(&encode_rendered(&ramp(OutputDepth::U16, OutputSpace::ProPhoto), &o16, None).unwrap());
        for x in [0, 300, 700, 1023] {
            let (a, b) = (df.image.get(x, 1)[1], d16.image.get(x, 1)[1]);
            assert!((a - b).abs() < 2e-4, "{x}: {a} vs {b}");
        }
    }

    #[test]
    fn jxl_options() {
        assert_eq!(ExportFormat::parse("JPEG-XL"), Some(ExportFormat::Jxl));
        let o = ExportOptions::from_params(&json!({"format": "jxl", "jxlLossless": true, "jxlEffort": "fast", "jxlProgressive": true})).unwrap();
        assert_eq!((o.format, o.jxl_lossless, o.jxl_effort, o.jxl_progressive), (ExportFormat::Jxl, true, JxlEffort::Fast, true));
        assert_eq!(ExportOptions::from_json(&o.to_json()), o, "saved presets keep the JPEG XL settings");
        assert!(ExportOptions::from_params(&json!({"jxlEffort": "best"})).is_err());
        assert!(ExportOptions::from_params(&json!({"jxlLossless": "yes"})).is_err());
        let space = |lossless, color_space, hdr| {
            ExportOptions { format: ExportFormat::Jxl, jxl_lossless: lossless, color_space, hdr, ..Default::default() }.effective_space()
        };
        assert_eq!(space(false, OutputSpace::DisplayP3, false), OutputSpace::DisplayP3);
        assert_eq!(space(false, OutputSpace::ProPhoto, false), OutputSpace::Rec2020, "lossy has no ProPhoto enum");
        assert_eq!(space(true, OutputSpace::ProPhoto, false), OutputSpace::ProPhoto, "lossless keeps the profile");
        assert_eq!(space(true, OutputSpace::Srgb, true), OutputSpace::Rec2020);
        let o = ExportOptions { format: ExportFormat::Jxl, hdr: true, ..Default::default() };
        assert!(o.hdr_output());
        assert_eq!(o.effective_depth(), OutputDepth::F32Hdr);
        assert_eq!(ExportFormat::Jxl.extension(), "jxl");
    }

    #[test]
    fn jxl_every_space_round_trips() {
        // wide-gamut green stays outside sRGB in P3, Rec. 2020 and (lossless) ProPhoto files
        for (lossless, space) in [
            (false, OutputSpace::Srgb),
            (false, OutputSpace::DisplayP3),
            (false, OutputSpace::Rec2020),
            (true, OutputSpace::DisplayP3),
            (true, OutputSpace::AdobeRgb),
            (true, OutputSpace::ProPhoto),
        ] {
            let o = ExportOptions { format: ExportFormat::Jxl, color_space: space, jxl_lossless: lossless, quality: 95, ..Default::default() };
            let png = ExportOptions { format: ExportFormat::Png, color_space: space, ..Default::default() };
            let img = p3_green(space);
            let bytes = encode_image(&img, &o).unwrap();
            assert_eq!(lightcraft_codecs::sniff(&bytes), Some(lightcraft_codecs::Format::Jxl));
            let (got, _) = decoded_in_srgb(&bytes);
            let (want, _) = decoded_in_srgb(&encode_image(&img, &png).unwrap());
            for c in 0..3 {
                assert!((got[c] - want[c]).abs() < 0.01, "{space:?} lossless={lossless}: {got:?} vs PNG {want:?}");
            }
            if space != OutputSpace::Srgb {
                assert!(got[0] < -0.03 || got[2] < -0.03, "{space:?}: outside sRGB after the round trip: {got:?}");
            }
        }
    }

    #[test]
    fn jxl_sixteen_bit() {
        let o = ExportOptions {
            format: ExportFormat::Jxl,
            bit_depth: Some(16),
            jxl_lossless: true,
            color_space: OutputSpace::DisplayP3,
            ..Default::default()
        };
        assert_eq!(o.effective_depth(), OutputDepth::U16);
        let (levels8, _) = distinct_levels(
            &encode_rendered(&ramp(OutputDepth::U8, OutputSpace::DisplayP3), &ExportOptions { bit_depth: Some(8), ..o.clone() }, None).unwrap(),
        );
        let (levels, d) = distinct_levels(&encode_rendered(&ramp(OutputDepth::U16, OutputSpace::DisplayP3), &o, None).unwrap());
        assert_eq!(d.bit_depth, 16);
        assert!(levels > 10 * levels8, "{levels} vs {levels8}");
        // lossy 16-bit encodes too
        let lossy = ExportOptions { jxl_lossless: false, ..o };
        assert!(encode_rendered(&ramp(OutputDepth::U16, OutputSpace::DisplayP3), &lossy, None).is_ok());
    }

    #[test]
    fn bit_depth_options_per_format() {
        let d = |format, bit_depth| ExportOptions { format, bit_depth, ..Default::default() }.effective_depth();
        assert_eq!(d(ExportFormat::Jpeg, Some(16)), OutputDepth::U8);
        assert_eq!(d(ExportFormat::Webp, None), OutputDepth::U8);
        assert_eq!(d(ExportFormat::Png, None), OutputDepth::U8);
        assert_eq!(d(ExportFormat::Png, Some(16)), OutputDepth::U16);
        assert_eq!(d(ExportFormat::Avif, Some(10)), OutputDepth::U16);
        assert_eq!(ExportOptions::from_json(&serde_json::json!({"bitDepth": 16})).bit_depth, Some(16));
        assert_eq!(ExportOptions::from_json(&serde_json::json!({"bitDepth": 12})).bit_depth, None);
        assert_eq!(d(ExportFormat::Jxl, Some(16)), OutputDepth::U16);
        assert_eq!(d(ExportFormat::Jxl, None), OutputDepth::U8);
        for f in [ExportFormat::Jpeg, ExportFormat::Png, ExportFormat::Tiff, ExportFormat::Webp, ExportFormat::Avif, ExportFormat::Jxl] {
            let first = ExportOptions::bit_depths(f)[0].0;
            let def = d(f, None);
            assert_eq!(d(f, Some(first)), def, "{f:?}: the first choice is the default");
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn avif_ten_bit() {
        let o = ExportOptions { format: ExportFormat::Avif, bit_depth: Some(10), quality: 60, ..Default::default() };
        let r = ramp(OutputDepth::U16, OutputSpace::Srgb);
        match encode_rendered(&r, &o, None) {
            Ok(b) => assert_eq!(&b[4..8], b"ftyp"),
            Err(e) => assert!(e.contains("not available"), "{e}"),
        }
    }

    /// Issue #181: a command about to export refuses what `from_json` would silently default —
    /// unknown keys (naming the nearest known one) and values of the wrong kind or range.
    #[test]
    fn export_params_are_validated() {
        use serde_json::json;
        let err = |p: serde_json::Value| ExportOptions::from_params(&p).err().map(|e| e.to_string()).unwrap_or_default();
        let e = err(json!({"longEdgee": 400}));
        assert!(e.starts_with("invalid parameters for `app.export`: unknown parameter `longEdgee` (did you mean `longEdge`?)"), "{e}");
        assert!(err(json!({"qualty": 5})).contains("did you mean `quality`"));
        assert!(err(json!({"longEdge": "banana"})).contains("`longEdge` must be a number"));
        assert!(err(json!({"quality": 0})).contains("`quality` must be an integer 1..100"));
        assert!(err(json!({"quality": "5"})).contains("`quality` must be an integer"));
        assert!(err(json!({"quality": 92.5})).contains("`quality` must be an integer"));
        assert!(err(json!({"sharpen": "lots"})).contains("`sharpen` must be one of none|screen|matte|glossy"));
        assert!(err(json!({"format": "bmp"})).contains("`format` must be one of"));
        assert!(err(json!({"bitDepth": 12})).contains("`bitDepth` must be 8, 10, 16 or 32"));
        assert!(err(json!({"colorSpace": "cmyk"})).contains("`colorSpace` must be one of"));
        assert!(err(json!({"dontEnlarge": "no"})).contains("`dontEnlarge` must be true or false"));
        assert!(err(json!({"fit": "sideways"})).contains("`fit` must be one of dimensions|widthHeight"));
        assert!(err(json!({"resize": {"mode": "longEdge", "valeu": 10}})).contains("did you mean `value`"));
        assert!(err(json!({"resize": {"mode": "diagonal", "value": 10}})).contains("`resize`:"));
        assert!(err(json!({"ids": [1, "x"]})).contains("`ids` must be an array of photo ids"));
        assert!(err(json!("jpeg")).contains("params must be an object"));
        assert!(
            ExportOptions::validate("export.savePreset", &json!({"qualty": 5}))
                .unwrap_err()
                .to_string()
                .starts_with("invalid parameters for `export.savePreset`")
        );
        // everything the presets, the dialog, Export with Previous and `render --opt` send passes
        for p in builtin_presets() {
            ExportOptions::from_params(&p.params).unwrap_or_else(|e| panic!("{}: {e}", p.name));
        }
        let full = ExportOptions {
            resize: Some(Resize::default()),
            limit_kb: Some(500),
            watermark: Some(Watermark { text: "©".into(), ..Default::default() }),
            format: ExportFormat::Png,
            bit_depth: Some(16),
            ..Default::default()
        };
        let mut j = full.to_json();
        for (k, v) in [
            ("dir", json!("/tmp/out")),
            ("background", json!(true)),
            ("preset", json!("x")),
            ("ids", json!([1, 2])),
            ("id", json!(1)),
            ("path", json!("a.jpg")),
        ] {
            j[k] = v;
        }
        assert_eq!(ExportOptions::from_params(&j).unwrap(), full);
        // Add to Apple Photos (issue #236): a switch and an album name; on only where Photos is
        assert!(err(json!({"addToPhotos": "yes"})).contains("`addToPhotos` must be true or false"));
        assert!(err(json!({"photosAlbum": 3})).contains("`photosAlbum` must be a string"));
        assert!(err(json!({"photosAlbum": "a\0b"})).contains("NUL"));
        assert!(err(json!({"photosAlbm": "x"})).contains("did you mean `photosAlbum`"));
        let album = ExportOptions { photos_album: "Trip “2026” \\ \"x\"".into(), ..Default::default() };
        assert_eq!(ExportOptions::from_params(&album.to_json()).unwrap(), album);
        let on = ExportOptions { add_to_photos: true, ..album };
        if cfg!(target_os = "macos") {
            assert_eq!(ExportOptions::from_params(&on.to_json()).unwrap(), on);
        } else {
            assert!(err(on.to_json()).contains("Apple Photos is only available on macOS"));
            // stored settings (a preset or Export with Previous made on a Mac) still read
            assert!(ExportOptions::from_json(&on.to_json()).add_to_photos);
        }
        assert_eq!(ExportOptions::from_json(&json!({"photosAlbum": "  Trip  "})).photos_album, "Trip");
        assert_eq!(ExportOptions::from_params(&ExportOptions::default().to_json()).unwrap(), ExportOptions::default());
        // null is "not given", as a dropped key is
        ExportOptions::from_params(&json!({"quality": 92, "longEdge": null, "watermark": null})).unwrap();
    }

    /// Issue #1952: a `bitDepth` the format doesn't have is refused (the Export dialog never offers
    /// it), not written at the format's own depth; the error names the depths the format takes.
    #[test]
    fn bit_depth_must_suit_the_format() {
        use serde_json::json;
        let err = |p: serde_json::Value| ExportOptions::from_params(&p).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(err(json!({"format": "jpeg", "bitDepth": 16})).contains("`bitDepth` must be 8 for jpeg"));
        assert!(err(json!({"format": "png", "bitDepth": 10})).contains("`bitDepth` must be 8 or 16 for png"));
        assert!(err(json!({"format": "tiff", "bitDepth": 10})).contains("`bitDepth` must be 8, 16 or 32 for tiff"));
        // no `format`: the one `path` names, else the default (JPEG)
        assert!(err(json!({"path": "/out/a.png", "bitDepth": 32})).contains("for png"));
        assert!(err(json!({"bitDepth": 16})).contains("for jpeg"));
        assert!(ExportOptions::from_params(&json!({"path": "/out/a.tif", "bitDepth": 32})).is_ok());
        // a path naming no format, or a `null` format, exports the default too
        assert!(err(json!({"path": "/out/output", "bitDepth": 10})).contains("for jpeg"));
        assert!(err(json!({"path": "/out/output.xyz", "bitDepth": 10})).contains("for jpeg"));
        assert!(err(json!({"format": null, "path": "/out/a.png", "bitDepth": 16})).contains("for jpeg"));
        // or the caller's own format for such a path, which is the one returned
        let png = |p: serde_json::Value| ExportOptions::from_params_or(&p, ExportFormat::Png);
        assert!(png(json!({"path": "/out/output", "bitDepth": 10})).unwrap_err().to_string().contains("for png"));
        assert_eq!(png(json!({"path": "/out/output.xyz", "bitDepth": 16})).unwrap().format, ExportFormat::Png);
        assert_eq!(png(json!({"path": "/out/a.tif", "bitDepth": 32})).unwrap().format, ExportFormat::Tiff);
        // stored settings (a preset, Export with Previous) lose such a depth instead of failing
        let stored = ExportOptions::known_keys_only(&json!({"format": "jpeg", "bitDepth": 16}));
        assert_eq!(stored.get("bitDepth"), None);
        assert_eq!(ExportOptions::known_keys_only(&json!({"format": "tiff", "bitDepth": 32}))["bitDepth"], 32);
        for f in [
            ExportFormat::Jpeg,
            ExportFormat::Png,
            ExportFormat::Tiff,
            ExportFormat::Psd,
            ExportFormat::Webp,
            ExportFormat::Avif,
            ExportFormat::Jxl,
            ExportFormat::Original,
            ExportFormat::Dng,
            ExportFormat::RenderedDng,
        ] {
            for &(b, _) in ExportOptions::bit_depths(f) {
                let o = ExportOptions { format: f, bit_depth: Some(b), ..Default::default() };
                assert_eq!(ExportOptions::from_params(&o.to_json()).ok(), Some(o), "{f:?} {b}");
            }
        }
    }

    /// Issue #956: `destinationMode` picks a specific folder (the default) or each photo's own
    /// folder; it is checked like the other params and kept in presets and Export with Previous.
    #[test]
    fn destination_mode_param() {
        use serde_json::json;
        assert_eq!(ExportOptions::default().destination_mode, DestinationMode::SpecificFolder);
        for (v, want) in [
            ("sameAsOriginal", DestinationMode::SameAsOriginal),
            ("SameAsOriginal", DestinationMode::SameAsOriginal),
            ("specificFolder", DestinationMode::SpecificFolder),
        ] {
            assert_eq!(ExportOptions::from_params(&json!({"destinationMode": v})).unwrap().destination_mode, want, "{v}");
        }
        let e = ExportOptions::from_params(&json!({"destinationMode": "nextToIt"})).unwrap_err().to_string();
        assert!(e.contains("`destinationMode` must be one of specificFolder|sameAsOriginal"), "{e}");
        assert!(ExportOptions::from_params(&json!({"destinationMode": 1})).is_err());
        assert!(
            ExportOptions::from_params(&json!({"destinationMod": "sameAsOriginal"}))
                .unwrap_err()
                .to_string()
                .contains("did you mean `destinationMode`")
        );
        // saved (a preset, `lastExport`) and read back
        let o = ExportOptions { destination_mode: DestinationMode::SameAsOriginal, subfolder: "Exports".into(), ..Default::default() };
        let j = o.to_json();
        assert_eq!(j["destinationMode"], "sameAsOriginal");
        assert_eq!(ExportOptions::from_params(&ExportOptions::known_keys_only(&j)).unwrap(), o);
    }

    /// A choice the validation accepts is the one exported: `metadata: "NONE"` passed the
    /// case-insensitive check, then failed the exact-case read and fell back to `all`, so GPS and
    /// camera data went out in a file meant to carry none. `sharpen`, `sharpenAmount` and
    /// `conflict` had the same gap.
    #[test]
    fn accepted_choices_are_the_ones_exported() {
        use serde_json::json;
        let read = |k: &str, v: &str| ExportOptions::from_params(&json!({ k: v })).map_err(|e| e.to_string());
        let metadata = [
            ("all", MetadataPolicy::All),
            ("allExceptCamera", MetadataPolicy::AllExceptCamera),
            ("copyright", MetadataPolicy::Copyright),
            ("none", MetadataPolicy::None),
        ];
        for (name, want) in metadata {
            for v in [name.to_string(), name.to_ascii_uppercase(), name.to_ascii_lowercase()] {
                assert_eq!(read("metadata", &v).unwrap().metadata, want, "metadata: {v}");
            }
        }
        for (name, want) in [("none", SharpenFor::None), ("screen", SharpenFor::Screen), ("matte", SharpenFor::Matte), ("glossy", SharpenFor::Glossy)]
        {
            assert_eq!(read("sharpen", &name.to_ascii_uppercase()).unwrap().sharpen, want);
        }
        for (name, want) in [("low", SharpenAmount::Low), ("standard", SharpenAmount::Standard), ("high", SharpenAmount::High)] {
            assert_eq!(read("sharpenAmount", &name.to_ascii_uppercase()).unwrap().sharpen_amount, want);
        }
        for (name, want) in [("unique", Conflict::Unique), ("overwrite", Conflict::Overwrite), ("skip", Conflict::Skip)] {
            assert_eq!(read("conflict", &name.to_ascii_uppercase()).unwrap().conflict, want);
        }
        // an explicit value nothing reads as is an error, never the permissive default
        for v in ["", " none", "nothing", "all-except-camera"] {
            let e = read("metadata", v).unwrap_err();
            assert!(e.contains("`metadata` must be one of all|allExceptCamera|copyright|none"), "{v:?}: {e}");
        }
        assert!(read("sharpenAmount", "max").unwrap_err().contains("`sharpenAmount` must be one of low|standard|high"));
        assert!(read("conflict", "replace").unwrap_err().contains("`conflict` must be one of unique|overwrite|skip"));
        // absent (or null) keeps the defaults
        let d = ExportOptions::from_params(&json!({"metadata": null})).unwrap();
        assert_eq!(
            (d.metadata, d.sharpen, d.sharpen_amount, d.conflict),
            (MetadataPolicy::All, SharpenFor::None, SharpenAmount::Standard, Conflict::Unique)
        );
        // `removeLocation` stays independent of the policy's spelling
        let o = ExportOptions::from_params(&json!({"metadata": "AllExceptCamera", "removeLocation": true})).unwrap();
        assert_eq!((o.metadata, o.remove_location), (MetadataPolicy::AllExceptCamera, true));
        // stored settings (lenient `from_json`) read the same spellings
        assert_eq!(ExportOptions::from_json(&json!({"metadata": "None"})).metadata, MetadataPolicy::None);
        // and `to_json` still writes what the MCP schema lists
        let j = ExportOptions { metadata: MetadataPolicy::AllExceptCamera, conflict: Conflict::Skip, ..Default::default() }.to_json();
        assert_eq!((j["metadata"].as_str(), j["conflict"].as_str()), (Some("allExceptCamera"), Some("skip")));
    }

    /// Issue #183: the `watermark` object's keys, kinds and ranges are checked (and documented in
    /// the `app.export` param string), instead of unknown keys passing and sizes clamping silently.
    #[test]
    fn watermark_params_are_validated() {
        use serde_json::json;
        let err = |w: serde_json::Value| ExportOptions::from_params(&json!({"watermark": w})).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(err(json!(5)).contains(
            "`watermark` must be text or an object {text, vertical, size, opacity, anchor, insetX, insetY, inset, rotation, color, shadow, image, imageWidth}"
        ));
        assert!(err(json!({"text": "x", "bogus": 1})).contains(
            "unknown watermark key `bogus` (one of text, vertical, size, opacity, anchor, insetX, insetY, inset, rotation, color, shadow, image, imageWidth)"
        ));
        assert!(err(json!({"text": "x", "sizee": 0.1})).contains("did you mean `size`"));
        for size in [json!(3), json!(0.5001), json!(0), json!("big")] {
            let e = err(json!({"text": "x", "size": size}));
            assert!(e.contains("`watermark.size` must be a number 0.005..0.5 (fraction of the short edge)"), "{size}: {e}");
        }
        assert!(err(json!({"text": "x", "opacity": 1.5})).contains("`watermark.opacity` must be a number 0..1"));
        assert!(err(json!({"text": "x", "inset": -0.1})).contains("`watermark.inset` must be a number 0..0.4"));
        assert!(err(json!({"text": "x", "insetX": 0.5})).contains("`watermark.insetX` must be a number 0..0.4"));
        assert!(err(json!({"text": "x", "insetY": "far"})).contains("`watermark.insetY` must be a number 0..0.4"));
        for rotation in [json!(45), json!(-90), json!(360), json!("90"), json!(90.5)] {
            let e = err(json!({"text": "x", "rotation": rotation}));
            assert!(e.contains("`watermark.rotation` must be 0, 90, 180 or 270 (degrees clockwise)"), "{rotation}: {e}");
        }
        assert!(ExportOptions::from_params(&json!({"watermark": {"text": "x", "rotation": 270}})).is_ok());
        assert!(err(json!({"image": "logo.png", "imageWidth": 2})).contains("`watermark.imageWidth` must be a number 0.01..1"));
        assert!(err(json!({"text": "x", "anchor": "middle"})).contains("`watermark.anchor` must be one of"));
        assert!(err(json!({"text": "x", "color": [255, 255]})).contains("`watermark.color` must be [r, g, b]"));
        assert!(err(json!({"text": "x", "color": [255, 255, 256]})).contains("`watermark.color`"));
        assert!(err(json!({"text": "x", "vertical": "yes"})).contains("`watermark.vertical` must be true or false"));
        assert!(err(json!({"text": 7})).contains("`watermark.text` must be a string"));
        // the documented shapes pass and read as before
        let o = ExportOptions::from_params(&json!({"watermark": "© me"})).unwrap();
        assert_eq!(o.watermark.map(|w| w.text).as_deref(), Some("© me"));
        let w = json!({"text": "日本語", "vertical": true, "size": 0.1, "opacity": 0.5, "anchor": "topLeft", "inset": 0.05, "color": [0, 0, 0], "shadow": false, "image": "", "imageWidth": 0.3});
        let wm = ExportOptions::from_params(&json!({"watermark": w})).unwrap().watermark.unwrap();
        assert!((wm.size - 0.1).abs() < 1e-6 && wm.vertical && wm.anchor == Anchor::TopLeft && wm.color == [0, 0, 0] && !wm.shadow);
        assert_eq!(ExportOptions::from_params(&json!({"watermark": {"text": "x", "size": 0.5}})).unwrap().watermark.map(|w| w.size), Some(0.5));
        // the Export dialog's sliders (1–15 % size, 5–100 % opacity, 2–100 % width) stay inside the accepted ranges
        assert!(WATERMARK_SIZE_RANGE.0 <= 0.01 && WATERMARK_SIZE_RANGE.1 >= 0.15);
        assert!(WATERMARK_IMAGE_WIDTH_RANGE.0 <= 0.02 && WATERMARK_IMAGE_WIDTH_RANGE.1 >= 1.0);
    }

    /// A batch with `lanes` photos in flight into a fake folder `out` (nothing exists there):
    /// results, writes (path, bytes) and progress calls; `progress` returns false at `cancel_at`.
    #[allow(clippy::type_complexity)]
    fn lanes_batch(
        s: &mut crate::Session,
        ids: &[lightcraft_catalog::PhotoId],
        o: &ExportOptions,
        lanes: usize,
        stop_on_error: bool,
        cancel_at: usize,
    ) -> (Result<Vec<serde_json::Value>, String>, Vec<(String, Vec<u8>)>, Vec<(usize, String)>) {
        let items = prepare_batch(s, ids, o).unwrap();
        let to = Destination { dir: "out".into(), exact: None };
        let mut written = Vec::new();
        let mut write = |p: &str, b: &[u8]| {
            written.push((p.to_string(), b.to_vec()));
            Ok(())
        };
        let mut seen = Vec::new();
        let mut progress = |i: usize, name: &str| {
            seen.push((i, name.to_string()));
            i < cancel_at
        };
        let files = run_batch_with(items, &mut Placer::new(o, &to, &mut write, &|_| false, stop_on_error), &mut progress, lanes);
        (files, written, seen)
    }

    /// The files of a batch without their byte counts: a GPU render (when another test turns the
    /// GPU off mid-run) may differ from a CPU one by an LSB, never in name, size or order.
    fn shape(files: &[serde_json::Value]) -> Vec<serde_json::Value> {
        files.iter().map(|f| json!({"path": f["path"], "width": f["width"], "height": f["height"], "error": f["error"]})).collect()
    }

    /// Issue #675: a batch reports a render that fell back from the GPU to the CPU on that file's
    /// entry (`gpuFallback`, the render's own reason) and says nothing on the others.
    #[test]
    fn a_batch_reports_a_gpu_fallback_on_the_file_it_hit() {
        if !lightcraft_gpu::available() {
            lightcraft_testkit::skip!(Gpu, "no GPU adapter ({})", lightcraft_gpu::unavailable_reason().unwrap_or_default());
            return;
        }
        let mut s = crate::Session::with_demo();
        let ids: Vec<_> = s.visible().iter().copied().take(2).collect();
        let o = ExportOptions::from_json(&json!({"format": "png", "width": 64}));
        // one lane: both photos render on this thread, and the fault hits the first render here
        lightcraft_gpu::inject_fault(lightcraft_gpu::Fault::Limit(1 << 10));
        let (files, _, _) = lanes_batch(&mut s, &ids, &o, 1, true, usize::MAX);
        let files = files.unwrap();
        assert_eq!(files.len(), 2, "{files:?}");
        let why: Vec<_> = files.iter().filter_map(|f| f.get("gpuFallback").and_then(serde_json::Value::as_str)).collect();
        assert_eq!(why.len(), 1, "exactly one file fell back: {files:?}");
        assert!(why[0].contains("limit"), "{files:?}");
        assert!(files[1].get("gpuFallback").is_none(), "the second render was fine: {files:?}");
        assert!(lightcraft_gpu::available(), "a limit is no device failure");
    }

    // Issue #496: photos rendered side by side give the same files, names, order and progress as
    // Render to DNG: the rendered photo as a linear float DNG that reads back as the same picture
    #[test]
    fn rendered_dng_matches_the_rendered_tiff() {
        let mut s = crate::Session::with_demo();
        let id = s.visible()[0];
        let dng = export_photo(&mut s, id, &ExportOptions::from_json(&json!({"format": "rendereddng", "width": 64})), 1).unwrap();
        assert!(dng.file_name.ends_with(".dng"), "{}", dng.file_name);
        let png = export_photo(&mut s, id, &ExportOptions::from_json(&json!({"format": "png", "width": 64})), 1).unwrap();
        let raw = lightcraft_raw::decode(&dng.bytes).unwrap();
        assert_eq!((raw.width, raw.height, raw.cpp), (dng.width, dng.height, 3));
        assert!(raw.color.profile.tone_curve.is_some(), "identity tone curve keeps converters from adding one");
        let back = raw.develop(lightcraft_raw::Method::Bilinear).unwrap();
        let reference = lightcraft_codecs::decode_thumbnail(&png.bytes, 4096).unwrap().image;
        assert_eq!((reference.width, reference.height), (back.width, back.height));
        // mean brightness: the PNG holds 8-bit sRGB, the DNG linear sRGB
        let mean_dng: f32 = back.data.iter().map(|p| lightcraft_color::transfer::linear_to_srgb(p[1].min(1.0))).sum::<f32>() / back.data.len() as f32;
        let mean_tif: f32 = reference.data.iter().map(|p| p[1] as f32 / 255.0).sum::<f32>() / reference.data.len() as f32;
        assert!((mean_dng - mean_tif).abs() < 0.02, "{mean_dng} vs {mean_tif}");
    }

    // one at a time, and a cancel stops at the same photo
    #[test]
    fn side_by_side_batches_match_one_at_a_time() {
        let mut s = crate::Session::with_demo();
        let ids: Vec<_> = s.visible().iter().copied().take(5).collect();
        // one name for all: the Unique numbering follows the order the files are placed in
        let o = ExportOptions::from_json(&json!({"format": "png", "width": 40, "naming": "same"}));
        let (files, written, seen) = lanes_batch(&mut s, &ids, &o, 1, true, usize::MAX);
        let files = files.unwrap();
        let paths: Vec<_> = written.iter().map(|(p, _)| p.clone()).collect();
        assert_eq!(paths, ["out/same.png", "out/same-2.png", "out/same-3.png", "out/same-4.png", "out/same-5.png"]);
        let heights: std::collections::HashSet<_> = files.iter().map(|f| f["height"].as_u64()).collect();
        assert!(heights.len() > 1, "photos of different shapes show the order: {files:?}");
        for lanes in [2, 3, 8] {
            let (f, w, sn) = lanes_batch(&mut s, &ids, &o, lanes, true, usize::MAX);
            assert_eq!(shape(&f.unwrap()), shape(&files), "{lanes} lanes");
            assert_eq!(w.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(), paths, "{lanes} lanes");
            assert_eq!(sn, seen, "{lanes} lanes");
        }
        let (f, w, sn) = lanes_batch(&mut s, &ids, &o, 3, true, 2);
        assert_eq!(shape(&f.unwrap()), shape(&files[..2]));
        assert_eq!((w.len(), sn.len()), (2, 3), "photos past the cancel are never written");
    }

    // A panic in `progress` while lanes run must end the batch (the lanes stop waiting), not hang it
    #[test]
    fn a_panic_in_progress_does_not_hang_the_lanes() {
        let mut s = crate::Session::with_demo();
        let ids: Vec<_> = s.visible().iter().copied().take(5).collect();
        let o = ExportOptions::from_json(&json!({"format": "png", "width": 40}));
        let items = prepare_batch(&mut s, &ids, &o).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let to = Destination { dir: "out".into(), exact: None };
            let mut write = |_: &str, _: &[u8]| Ok(());
            let mut calls = 0;
            let mut progress = |_: usize, _: &str| {
                calls += 1;
                assert!(calls < 2, "progress panics on its second call");
                true
            };
            let r = crate::guard::catch("test batch", || {
                run_batch_with(items, &mut Placer::new(&o, &to, &mut write, &|_| false, true), &mut progress, 2)
            });
            tx.send(r.and_then(|r| r)).ok();
        });
        let r = rx.recv_timeout(std::time::Duration::from_secs(60));
        assert!(matches!(r, Ok(Err(_))), "the batch returns an error instead of hanging: {r:?}");
    }

    // A photo that fails is reported in its place while the others are exported; with
    // stop_on_error the batch ends there and nothing after it is written
    #[test]
    fn side_by_side_batches_report_failures_in_order() {
        use lightcraft_catalog::{Op, Photo, PhotoId, Source};
        let mut s = crate::Session::with_demo();
        let mut ids: Vec<_> = s.visible().iter().copied().take(3).collect();
        let gone =
            Photo::new(PhotoId(9_496), Source::File { path: "no/such/dir/gone.jpg".into() }, "gone.jpg", "JPEG", 600, 400, "2026-10-09T00:00:00");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(gone) }).unwrap();
        ids.insert(1, PhotoId(9_496));
        let o = ExportOptions::from_json(&json!({"format": "jpeg", "width": 40}));
        let (files, written, _) = lanes_batch(&mut s, &ids, &o, 3, false, usize::MAX);
        let files = files.unwrap();
        assert_eq!(files.len(), 4);
        assert_eq!((files[1]["photo"].as_u64(), files[1]["file"].as_str()), (Some(9_496), Some("gone.jpg")), "{files:?}");
        assert!(files[1]["error"].is_string());
        assert!([0, 2, 3].iter().all(|&i| files[i]["path"].is_string()), "{files:?}");
        assert_eq!(written.len(), 3);
        let one = lanes_batch(&mut s, &ids, &o, 1, false, usize::MAX).0.unwrap();
        assert_eq!(shape(&files), shape(&one));

        let (r, written, _) = lanes_batch(&mut s, &ids, &o, 3, true, usize::MAX);
        assert!(r.is_err());
        assert_eq!(written.len(), 1, "only the photo before the failure");
    }

    #[test]
    fn export_weights_follow_the_source_and_output_sizes() {
        use crate::media::SourceLevel;
        let mb = |b: usize| b >> 20;
        // a full-size 24 MP export (measured peak ~1.2 GB)
        assert_eq!(mb(render_weight(6000 * 4000, 6000, SourceLevel::Full, 6000 * 4000)), 1098);
        // 2048 px from the 2560 px preview (measured ~150 MB)
        assert_eq!(mb(render_weight(6000 * 4000, 6000, SourceLevel::Preview, 2048 * 1365)), 145);
        // a photo smaller than the preview level is its own source
        assert_eq!(render_weight(1000 * 500, 1000, SourceLevel::Preview, 0), 1000 * 500 * 12);
        assert_eq!(render_weight(0, 0, SourceLevel::Full, 0), 0);
        assert!(render_weight(usize::MAX, 1, SourceLevel::Full, usize::MAX) > 0, "no overflow");
    }
}
