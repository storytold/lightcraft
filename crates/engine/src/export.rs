//! Export: encode a rendered image to JPEG / PNG / TIFF / WebP / AVIF / JPEG XL (or copy the original / write a
//! DNG, each with the edits in XMP) in sRGB, Display P3, Adobe RGB
//! (1998) compatible, ProPhoto RGB or Rec. 2020 with an embedded ICC profile we generate from the
//! published primaries and curves, optional output sharpening and a JPEG file-size limit. Pure (bytes in, bytes out) so the desktop
//! app, CLI, MCP and the web build share it; writing the file is the caller's job.

use lcc_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, JxlColour, JxlOptions, NamedSpace, Samples, encode, icc};
pub use lcc_codecs::{JxlEffort, TiffCompression};
use lcc_meta::{DateTime, Gps, Metadata};
pub use lcc_pipeline::{DeepImage, DeepSamples, OutputDepth, OutputSpace};
use lcc_raster::{Plane, Rgba8};
use serde::{Deserialize, Serialize};

/// Under `<PREFIX>_PROFILE`: print how long the step since the last lap took (`t` restarts),
/// with the thread and the time since the first lap (photos side by side overlap or queue).
fn lap(what: &str, t: &mut Option<std::time::Instant>) {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    if let Some(t) = t {
        let now = std::time::Instant::now();
        let at = now.duration_since(*EPOCH.get_or_init(|| *t)).as_secs_f64();
        let thread = std::thread::current();
        eprintln!("  export {what}: {:.1} ms [{} at {at:.1} s]", t.elapsed().as_secs_f64() * 1e3, thread.name().unwrap_or("main"));
        *t = now;
    }
}

/// The error of a photo whose export was cancelled before it was done ([`PreparedExport::run_until`]).
pub const CANCELLED: &str = "export cancelled";

/// A clock for [`lap`] when profiling (never read otherwise: `Instant::now` panics on wasm32).
fn lap_clock() -> Option<std::time::Instant> {
    lcc_pipeline::profiling().then(std::time::Instant::now)
}

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

    /// Formats whose exports keep a transparent source's alpha (straight in PNG and WebP,
    /// associated in TIFF); the others composite it onto white (issue #2976).
    pub fn keeps_alpha(self) -> bool {
        matches!(self, Self::Png | Self::Tiff | Self::Webp)
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
/// The develop pipeline holds its own renders to the same ceiling (`lcc_pipeline::geometry::MAX_RENDER_PIXELS`).
pub const MAX_ENLARGED_PIXELS: f64 = 512e6;

/// Below this many bits per pixel a JPEG file-size limit can force a quality low enough to break
/// smooth areas such as skies into flat blocks: baseline JPEG degrades sharply between about 0.5
/// and 0.25 bits per pixel, where each 8×8 block keeps little more than its average. 1 MB is about
/// 0.2 over a 40 MP photo, and about 3 over 2048 px on the long edge. A caller *estimates* with
/// [`size_limit_is_tight`] before anything is encoded; an export *reports* the quality the limit
/// settled on ([`Exported::limit_quality`]).
pub const TIGHT_LIMIT_BITS_PER_PIXEL: f64 = 0.5;

/// Whether `limit_kb` leaves an output of `pixels` fewer than [`TIGHT_LIMIT_BITS_PER_PIXEL`].
pub fn size_limit_is_tight(limit_kb: u32, pixels: usize) -> bool {
    let px = pixels as f64;
    px > 0.0 && f64::from(limit_kb) * 8192.0 / px < TIGHT_LIMIT_BITS_PER_PIXEL
}

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
    /// Lightroom Classic's "Choose folder later": the app asks for the folder when the export
    /// starts (handy for presets). Once a folder is given it is one folder for the batch, as
    /// [`SpecificFolder`](Self::SpecificFolder); a host with no folder dialog needs `dir`.
    ChooseLater,
}

impl DestinationMode {
    /// `specificFolder` / `sameAsOriginal` / `chooseLater` (any case).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "specificfolder" => Some(Self::SpecificFolder),
            "sameasoriginal" => Some(Self::SameAsOriginal),
            "chooselater" => Some(Self::ChooseLater),
            _ => None,
        }
    }
}

/// Why `name` can't be an export subfolder: it must be one plain folder name, created inside the
/// destination folder, never a path out of it (no `..`, no absolute path, no separator).
pub fn subfolder_problem(name: &str) -> Option<&'static str> {
    let t = name.trim();
    if t.is_empty() {
        return None;
    }
    if t.chars().any(|c| matches!(c, '/' | '\\' | ':') || c.is_control()) {
        return Some("the subfolder must be one folder name, without `/`, `\\`, `:` or control characters");
    }
    if t.chars().all(|c| c == '.') {
        return Some("the subfolder can't be `.` or `..`");
    }
    // a name every file system takes: no character Windows refuses (`* ? " < > |`), no trailing dot or space, and no
    // device name Windows reserves (`CON`, `NUL`, `COM1`…, also before a dot), as for a file name
    if portable_stem(t) != t {
        return Some("the subfolder can't hold `* ? \" < > |`, end in a dot or space, or be a reserved Windows name such as `CON` or `NUL`");
    }
    None
}

impl Conflict {
    /// `unique` / `overwrite` / `skip` (any case).
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
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExportPreset {
    pub name: String,
    pub params: serde_json::Value,
    /// Settings from an imported preset (e.g. Lightroom Classic `.lrtemplate`) that have no
    /// counterpart here and need to be configured manually.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmapped: Vec<String>,
}

/// The built-in export presets (File → Export with Preset).
pub fn builtin_presets() -> Vec<ExportPreset> {
    let p = |name: &str, params: serde_json::Value| ExportPreset { name: name.into(), params, unmapped: Vec::new() };
    vec![
        p("JPEG (Small)", serde_json::json!({"format": "jpeg", "quality": 85, "longEdge": 2048, "colorSpace": "srgb"})),
        p("JPEG (Large)", serde_json::json!({"format": "jpeg", "quality": 92, "longEdge": 0, "colorSpace": "srgb"})),
        p("Original + Settings", serde_json::json!({"format": "original"})),
        p("DNG", serde_json::json!({"format": "dng"})),
        p("Instagram Feed", serde_json::json!({"format": "jpeg", "quality": 85, "longEdge": 1080, "colorSpace": "srgb", "sharpen": "screen"})),
        p(
            "Instagram Story / Reel",
            serde_json::json!({"format": "jpeg", "quality": 85, "longEdge": 1920, "colorSpace": "srgb", "sharpen": "screen"}),
        ),
        p("Web / Portfolio", serde_json::json!({"format": "jpeg", "quality": 85, "longEdge": 2048, "colorSpace": "srgb", "metadata": "copyright"})),
        p("Full Resolution Master", serde_json::json!({"format": "jpeg", "quality": 95, "longEdge": 0, "colorSpace": "displayP3"})),
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
    watermark_coverage(img.width, img.height, wm, crate::fonts::craft_fonts(), floor_scale, |x, y, k, col| {
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
    watermark_coverage(w, h, wm, crate::fonts::craft_fonts(), 1.0, |x, y, k, col| {
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
fn draw_watermark_deep_over(img: &mut DeepImage, mut alpha: Option<&mut Plane>, wm: &Watermark) {
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
        DeepSamples::U16(v) => watermark_coverage(img.width, img.height, wm, crate::fonts::craft_fonts(), 1.0, |x, y, k, col| {
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
        DeepSamples::F32(v) => watermark_coverage(img.width, img.height, wm, crate::fonts::craft_fonts(), 1.0, |x, y, k, col| {
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
/// every covered pixel (shadow pass first). `craft` is [`crate::fonts::craft_fonts()`] (a parameter
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
    let bytes = crate::bounded_read::read_bounded(std::path::Path::new(path), 256 << 20).map_err(|e| format!("can't read {path}: {e}"))?;
    // a graphic is a logo or a layout image, not a photo: held to 8192 x 8192 pixels, not the general gigapixel cap
    let opts = lcc_codecs::DecodeOptions { max_pixels: 1 << 26, ..Default::default() };
    let decoded = lcc_codecs::decode(&bytes, opts).map_err(|e| format!("can't decode {path}: {e}"))?;
    Ok(decoded.to_srgb8().into_oriented(lcc_geom::Orientation::from_exif(decoded.orientation)))
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
    let pre: lcc_raster::Image<[f32; 4]> = lcc_raster::Image::from_fn(logo.width, logo.height, |x, y| {
        let p = logo.get(x, y);
        let a = p[3] as f32 / 255.0;
        [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, a]
    });
    let scaled = lcc_raster::resample::resize(&pre, lw, lh, lcc_raster::resample::Filter::Mitchell);
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
    /// Copyright, creator and the creator's contact details and job title.
    CopyrightContact,
    /// Nothing (the sRGB profile is still embedded).
    None,
}

impl MetadataPolicy {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "all" => Some(Self::All),
            "allexceptcamera" => Some(Self::AllExceptCamera),
            "copyright" => Some(Self::Copyright),
            "copyrightcontact" => Some(Self::CopyrightContact),
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
    fn codec(self) -> lcc_codecs::HdrTransfer {
        match self {
            Self::Pq => lcc_codecs::HdrTransfer::Pq,
            Self::Hlg => lcc_codecs::HdrTransfer::Hlg,
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
    /// The text of the `{custom}` naming token (Lightroom's Custom Text, as in "Custom Name -
    /// Sequence"); characters not allowed in file names are replaced.
    pub custom_text: String,
    /// What the naming's `{text:Label}` tags become, by label: typed at export, so a naming preset
    /// stays fixed (the couple's initials). Never kept in an export preset.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub texts: std::collections::BTreeMap<String, String>,
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
    /// The curve of an HDR AVIF: PQ (default) or HLG.
    pub hdr_transfer: HdrCurve,
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
    /// Video clips in the batch (rendered formats only): the movie format.
    pub video_format: crate::export_video::VideoFormat,
    /// Video quality 1–100 (H.264 CRF 40…14, or the ProRes flavour).
    pub video_quality: u8,
    /// Video: longest output side in pixels (`None` = the clip's size; never upscales).
    pub video_max_side: Option<u32>,
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
            custom_text: String::new(),
            texts: std::collections::BTreeMap::new(),
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
            hdr_transfer: HdrCurve::Pq,
            jxl_lossless: false,
            jxl_effort: JxlEffort::Normal,
            jxl_progressive: false,
            add_to_photos: false,
            photos_album: String::new(),
            video_format: crate::export_video::VideoFormat::H264,
            video_quality: 80,
            video_max_side: None,
        }
    }
}

/// The `app.export` params that are not export options: which photos, where to, a preset to start
/// from, and the desktop app's background flag.
pub const TARGET_PARAMS: &[&str] = &["id", "ids", "path", "dir", "preset", "background", "pluginDestination"];

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
    "customText",
    "texts",
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
    "hdrTransfer",
    "jxlLossless",
    "jxlEffort",
    "jxlProgressive",
    "addToPhotos",
    "photosAlbum",
    "videoFormat",
    "videoQuality",
    "videoMaxSide",
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
        let o = Self { format, ..Self::from_json(p) };
        // every `{text:…}` of the naming needs its text (asked in the dialog)
        crate::rename::texts_param(&o.naming, p.get("texts")).map_err(|e| crate::EngineError::BadParams { cmd: "app.export".into(), msg: e })?;
        Ok(o)
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
                "quality" | "videoQuality" => {
                    int(k, v, 1, 100)?;
                }
                "videoFormat" => one_of(k, v, crate::export_video::VideoFormat::NAMES)?,
                "videoMaxSide" => {
                    let (lo, hi) = crate::export_video::VIDEO_MAX_SIDE_RANGE;
                    if !whole(v).is_some_and(|i| i == 0 || (i64::from(lo)..=i64::from(hi)).contains(&i)) {
                        return Err(bad(format!("`{k}` must be 0 (the clip's size) or an integer {lo}..{hi}")));
                    }
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
                "subfolder" => {
                    if let Some(why) = subfolder_problem(string(cmd, k, v)?) {
                        return Err(bad(format!("`{k}`: {why}")));
                    }
                }
                "naming" | "customText" | "path" | "dir" | "preset" => {
                    string(cmd, k, v)?;
                }
                "texts" => {
                    let Some(o) = v.as_object() else { return Err(bad(format!("`{k}` must be an object of label: text"))) };
                    if o.values().any(|t| !t.is_string()) {
                        return Err(bad(format!("`{k}`: each text must be a string")));
                    }
                }
                "sharpen" => parses(k, v, |s| SharpenFor::parse(s).is_some(), &["none", "screen", "matte", "glossy"])?,
                "sharpenAmount" => parses(k, v, |s| SharpenAmount::parse(s).is_some(), &["low", "standard", "high"])?,
                "hdrTransfer" => one_of(k, v, &["pq", "hlg"])?,
                "conflict" => parses(k, v, |s| Conflict::parse(s).is_some(), &["unique", "overwrite", "skip"])?,
                "destinationMode" => one_of(k, v, &["specificFolder", "sameAsOriginal", "chooseLater"])?,
                "tiffCompression" => one_of(k, v, &["none", "lzw", "zip", "deflate"])?,
                "jxlEffort" => one_of(k, v, &["fast", "normal"])?,
                "dngCompression" => {
                    serde_json::from_value::<DngCompression>(v.clone())
                        .map_err(|_| bad(format!("`{k}` must be one of lossless|deflate|uncompressed|lossy")))?;
                }
                "metadata" => {
                    parses(k, v, |s| MetadataPolicy::parse(s).is_some(), &["all", "allExceptCamera", "copyright", "copyrightContact", "none"])?
                }
                "colorSpace" => {
                    if OutputSpace::parse(string(cmd, k, v)?).is_none() {
                        return Err(bad(format!("`{k}` must be one of srgb|displayP3|adobeRgb|proPhoto|rec2020")));
                    }
                }
                "watermark" => Self::validate_watermark(cmd, v)?,
                "pluginDestination" => {
                    // {plugin, destination, args?}: a plug-in export destination run after the files are written
                    let ok = v.get("plugin").is_some_and(Value::is_string)
                        && v.get("destination").is_some_and(Value::is_string)
                        && v.get("args").is_none_or(|a| a.is_object() || a.is_null());
                    if !ok {
                        return Err(bad(format!("`{k}` must be {{plugin, destination, args?: object}}")));
                    }
                }
                "id" => {
                    int(k, v, 0, i64::MAX)?;
                }
                "ids" => {
                    let Some(a) = v.as_array() else {
                        return Err(bad(format!("`{k}` must be an array of photo ids")));
                    };
                    if let Some(e) = crate::bad_ids_entry(a) {
                        return Err(bad(e));
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
            custom_text: s("customText").map_or(d.custom_text, str::to_string),
            texts: p
                .get("texts")
                .and_then(serde_json::Value::as_object)
                .map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).take(64).collect())
                .unwrap_or_default(),
            destination_mode: s("destinationMode").and_then(DestinationMode::parse).unwrap_or(d.destination_mode),
            // stored prefs are read leniently: a subfolder that is a path becomes one folder name
            subfolder: s("subfolder").map_or(d.subfolder, |v| {
                let v = v.trim().replace(['/', '\\', ':', '\0'], "_");
                if subfolder_problem(&v).is_some() { String::new() } else { v }
            }),
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
            hdr_transfer: s("hdrTransfer").and_then(HdrCurve::parse).unwrap_or(d.hdr_transfer),
            jxl_lossless: p.get("jxlLossless").and_then(Value::as_bool).unwrap_or(d.jxl_lossless),
            jxl_effort: s("jxlEffort").and_then(JxlEffort::parse).unwrap_or(d.jxl_effort),
            jxl_progressive: p.get("jxlProgressive").and_then(Value::as_bool).unwrap_or(d.jxl_progressive),
            add_to_photos: p.get("addToPhotos").and_then(Value::as_bool).unwrap_or(d.add_to_photos),
            photos_album: s("photosAlbum").map_or(d.photos_album, |a| a.trim().to_string()),
            video_format: s("videoFormat").and_then(crate::export_video::VideoFormat::parse).unwrap_or(d.video_format),
            video_quality: u("videoQuality").map_or(d.video_quality, |q| q.clamp(1, 100) as u8),
            video_max_side: u("videoMaxSide").filter(|v| *v > 0).map(|v| {
                v.clamp(u64::from(crate::export_video::VIDEO_MAX_SIDE_RANGE.0), u64::from(crate::export_video::VIDEO_MAX_SIDE_RANGE.1)) as u32
            }),
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
    pub fn file_name_for(&self, p: &lcc_catalog::Photo, seq: usize) -> String {
        let base = if self.naming.trim().is_empty() { "{name}" } else { self.naming.as_str() };
        let n = seq + self.start_number.max(1) as usize - 1;
        let base = crate::rename::fill_texts(base, &|l| self.texts.get(l).cloned());
        let name = crate::rename::expand_tokens(&base, p, n, 3).replace("{custom}", self.custom_text.trim());
        let mut name = portable_stem(&name);
        let (stem, ext) = p.file_name.rsplit_once('.').unwrap_or((&p.file_name, ""));
        // the catalog's own file name is cleaned like the expansion: a supplied catalog may hold separators
        let ext = ext.replace(['/', '\\', ':', '\0'], "_");
        let ext = ext.as_str();
        if name.trim().is_empty() {
            // every token came out empty (no title, no capture time…): keep the original name
            name = portable_stem(stem);
        }
        if self.format == ExportFormat::Original {
            let base = name.trim_end_matches('.');
            if ext.is_empty() { base.to_string() } else { format!("{base}.{ext}") }
        } else {
            format!("{name}.{}", self.format.extension())
        }
    }
}

/// A file name stem every file system takes (issue #2140): path separators, the characters Windows
/// forbids (`: * ? " < > |`) and control characters become `_`, trailing dots and spaces go (Windows
/// drops them), and a device name Windows reserves (`CON`, `NUL`, `COM1`…, also before a dot) gets
/// a `_` so it names a file.
pub(crate) fn portable_stem(name: &str) -> String {
    let mut s: String =
        name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c }).collect();
    s.truncate(s.trim_end_matches(['.', ' ']).len());
    let device = s.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let numbered =
        device.len() == 4 && (device.starts_with("COM") || device.starts_with("LPT")) && device.ends_with(|c: char| ('1'..='9').contains(&c));
    if matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered {
        s.insert(device.len(), '_');
    }
    s
}

/// Output sharpening of an 8-bit image encoded in `space`: an unsharp mask on its luma ([`unsharp`]).
pub fn output_sharpen(img: &mut Rgba8, space: OutputSpace, target: SharpenFor, amount: SharpenAmount) {
    if !sharpens(img.width, img.height, target) {
        return;
    }
    let src: Vec<[f32; 3]> = img.data.iter().map(|p| [p[0] as f32, p[1] as f32, p[2] as f32]).collect();
    if let Some(out) = unsharp(img.width, img.height, &src, space.luma(), target, amount) {
        for (p, v) in img.data.iter_mut().zip(out) {
            for c in 0..3 {
                p[c] = v[c].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// [`output_sharpen`] on a high-bit-depth image. Float images hold scene-linear values; they are
/// sharpened on the sRGB curve's (perceptual) axis, extended past 1.0, so a float export gets the
/// same sharpening as an 8- or 16-bit one rather than more in the shadows and less in the highlights.
pub fn output_sharpen_deep(img: &mut DeepImage, target: SharpenFor, amount: SharpenAmount) {
    let (w, h) = (img.width, img.height);
    let weights = img.space.luma();
    if !sharpens(w, h, target) {
        return;
    }
    match &mut img.samples {
        DeepSamples::U16(v) => {
            let src: Vec<[f32; 3]> = v.as_chunks::<3>().0.iter().map(|c| [c[0] as f32, c[1] as f32, c[2] as f32]).collect();
            if let Some(out) = unsharp(w, h, &src, weights, target, amount) {
                for (d, s) in v.iter_mut().zip(out.as_flattened()) {
                    *d = s.round().clamp(0.0, 65535.0) as u16;
                }
            }
        }
        DeepSamples::F32(v) => {
            // A NaN or infinite sample would poison its neighbours' blur and local range; treat it as black.
            let encode = |x: f32| if x.is_finite() { lcc_color::transfer::linear_to_srgb(x.max(0.0)) } else { 0.0 };
            let src: Vec<[f32; 3]> = v.as_chunks::<3>().0.iter().map(|c| [c[0], c[1], c[2]].map(encode)).collect();
            if let Some(out) = unsharp(w, h, &src, weights, target, amount) {
                for (d, s) in v.iter_mut().zip(out.as_flattened()) {
                    *d = lcc_color::transfer::srgb_to_linear(s.max(0.0));
                }
            }
        }
    }
}

/// Whether output sharpening for `target` changes a `w × h` image ([`unsharp`] returns `None` otherwise):
/// without it the float copy of the image is never made.
fn sharpens(w: usize, h: usize, target: SharpenFor) -> bool {
    target != SharpenFor::None && w >= 3 && h >= 3
}

/// How far a sharpened pixel may pass the darkest and lightest of its 3×3 neighbours, as a share
/// of their difference: enough for crisp edges, too little for the light and dark rims (halos)
/// a plain unsharp mask draws along strong edges.
const SHARPEN_OVERSHOOT: f32 = 0.1;

/// The sharpened values of `src` (`w × h`), or `None` when nothing is to be done: an unsharp mask
/// at the output's own pixel scale (a Gaussian of radius `sigma` output pixels), on luma only (no
/// colour fringes), with its overshoot past the local range limited ([`SHARPEN_OVERSHOOT`]). Print
/// media blur more than a screen and get a wider, stronger mask. The values may be on any scale;
/// `weights` are the luminance weights of their RGB primaries.
fn unsharp(w: usize, h: usize, src: &[[f32; 3]], weights: [f32; 3], target: SharpenFor, amount: SharpenAmount) -> Option<Vec<[f32; 3]>> {
    let (sigma, base) = match target {
        SharpenFor::None => return None,
        SharpenFor::Screen => (0.7f32, 0.65f32),
        SharpenFor::Glossy => (0.8, 0.85),
        SharpenFor::Matte => (1.0, 1.05),
    };
    let k = base
        * match amount {
            SharpenAmount::Low => 0.5,
            SharpenAmount::Standard => 1.0,
            SharpenAmount::High => 1.6,
        };
    if w < 3 || h < 3 {
        return None;
    }
    let luma: Vec<f32> = src.iter().map(|p| weights[0] * p[0] + weights[1] * p[1] + weights[2] * p[2]).collect();
    let r = (3.0 * sigma).ceil() as isize;
    let taps: Vec<f32> = (-r..=r).map(|d| (-((d * d) as f32) / (2.0 * sigma * sigma)).exp()).collect();
    let norm: f32 = taps.iter().sum();
    let taps: Vec<f32> = taps.iter().map(|t| t / norm).collect();
    let gauss = |acc: f32, d: isize, v: f32| acc + v * taps[(d + r) as usize];
    let blur = separable(&separable(&luma, w, h, r, true, 0.0, gauss), w, h, r, false, 0.0, gauss);
    let lo = |acc: f32, _: isize, v: f32| acc.min(v);
    let hi = |acc: f32, _: isize, v: f32| acc.max(v);
    let lo = separable(&separable(&luma, w, h, 1, true, f32::INFINITY, lo), w, h, 1, false, f32::INFINITY, lo);
    let hi = separable(&separable(&luma, w, h, 1, true, f32::NEG_INFINITY, hi), w, h, 1, false, f32::NEG_INFINITY, hi);
    Some(
        src.iter()
            .enumerate()
            .map(|(i, p)| {
                let (y, margin) = (luma[i], (hi[i] - lo[i]) * SHARPEN_OVERSHOOT);
                // max/min rather than clamp: never panics, even on bounds a non-finite input left inverted or NaN.
                let sharp = (y + (y - blur[i]) * k).max(lo[i] - margin).min(hi[i] + margin);
                p.map(|v| v + sharp - y)
            })
            .collect(),
    )
}

/// One pass of a separable filter over a `w × h` plane: each value folds (`f(acc, offset, value)`,
/// from `init`) its neighbours up to `reach` away along a row (`horizontal`) or a column, edges repeated.
fn separable(inp: &[f32], w: usize, h: usize, reach: isize, horizontal: bool, init: f32, f: impl Fn(f32, isize, f32) -> f32 + Sync) -> Vec<f32> {
    let mut out = vec![0.0; inp.len()];
    lcc_raster::par_rows(&mut out, w, |y, row| {
        for (x, o) in row.iter_mut().enumerate() {
            *o = (-reach..=reach).fold(init, |acc, d| {
                let i = if horizontal {
                    y * w + (x as isize + d).clamp(0, w as isize - 1) as usize
                } else {
                    (y as isize + d).clamp(0, h as isize - 1) as usize * w + x
                };
                f(acc, d, inp[i])
            });
        }
    });
    out
}

/// Encode a rendered display-referred image according to `o` (its pixels must already be in
/// `o.effective_space()`). Resizing to `long_edge` is the
/// caller's job (render at that size); sharpening is applied here.
pub fn encode_image(img: &Rgba8, o: &ExportOptions) -> Result<Vec<u8>, String> {
    encode_with_metadata(img, o, None)
}

/// Like [`encode_image`], embedding `meta` (already filtered by the policy) as EXIF + XMP.
pub fn encode_with_metadata(img: &Rgba8, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    encode_8bit(img, o, meta).map(|(bytes, _)| bytes)
}

/// [`encode_with_metadata`], with the JPEG quality a file-size limit settled on (None without one).
fn encode_8bit(img: &Rgba8, o: &ExportOptions, meta: Option<&Metadata>) -> Result<(Vec<u8>, Option<u8>), String> {
    note_encode();
    let img = finish_8bit(img, o);
    let space = o.effective_space();
    let profile = icc::write_named(named_space(space));
    let exif = meta.map(lcc_meta::write_exif);
    let xmp = meta.map(|m| lcc_meta::write_xmp(m, None));
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
            let fixed = profile.len() + exif.as_ref().map_or(0, Vec::len) + xmp.as_ref().map_or(0, String::len);
            return encode_jpeg_with_limit(o, &img, sub, fixed, &mut |q| encode::encode_jpeg(&e, q, sub, &meta).map_err(|e| e.to_string()));
        }
        ExportFormat::Png => encode::encode_png(&e, &meta),
        ExportFormat::Tiff => encode::encode_tiff_associated(&e, o.tiff_compression, &meta),
        ExportFormat::Psd => encode::encode_psd(&e, &meta),
        ExportFormat::Webp => encode::encode_webp_lossless(&e, &meta),
        ExportFormat::Avif => encode::encode_avif(&e, o.quality, 8, &meta),
        ExportFormat::Jxl => lcc_codecs::encode_jxl(&e, &o.jxl(space, false), &meta),
        f @ (ExportFormat::Original | ExportFormat::Dng) => return Err(format!("{f:?} export does not encode pixels")),
        ExportFormat::RenderedDng => return Err("Render to DNG needs a float render".into()),
    };
    r.map(|bytes| (bytes, None)).map_err(|e| e.to_string())
}

/// Apply the same file-size policy to ordinary JPEGs and complete HDR gain map JPEGs: `encode(q)`
/// is the whole file of `img` at quality q, `fixed` of its bytes (metadata, profile, a gain map)
/// don't change with q. Returns the file and, with a limit, the quality it was written at.
fn encode_jpeg_with_limit(
    o: &ExportOptions,
    img: &Rgba8,
    sub: ChromaSubsampling,
    fixed: usize,
    encode: &mut dyn FnMut(u8) -> Result<Vec<u8>, String>,
) -> Result<(Vec<u8>, Option<u8>), String> {
    match o.limit_kb {
        Some(kb) => fit_jpeg(img, o.quality, kb, sub, fixed, encode).map(|(bytes, q)| (bytes, Some(q))),
        None => encode(o.quality).map(|bytes| (bytes, None)),
    }
}

#[cfg(test)]
thread_local! {
    /// Full-size JPEG encodes [`fit_jpeg`] made on this thread so far (tests: at most two per photo).
    pub(crate) static FIT_ENCODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The JPEG of `img` at the highest quality up to `quality` whose file fits within `kb` KB, in
/// two full-size encodes (a binary search made seven or eight of a 6000 px photo).
///
/// The first is at `quality`; when it is too big, how the size falls with the quality is measured
/// on a small mosaic of the photo's own blocks (a sixteenth of it), scaled to the first encode's
/// size, and the second encode is at the quality that curve says fits, with a margin. Should the
/// photo still not fit (the mosaic misjudged it), the curve is corrected by that encode and a
/// lower quality tried, until quality 1. Returns the file and the quality it was written at.
fn fit_jpeg(
    img: &Rgba8,
    quality: u8,
    kb: u32,
    sub: ChromaSubsampling,
    fixed: usize,
    encode: &mut dyn FnMut(u8) -> Result<Vec<u8>, String>,
) -> Result<(Vec<u8>, u8), String> {
    let limit = (kb as usize).saturating_mul(1024);
    let mut full = |q: u8| {
        #[cfg(test)]
        FIT_ENCODES.with(|c| c.set(c.get() + 1));
        encode(q)
    };
    let quality = quality.clamp(1, 100);
    let first = full(quality)?;
    if first.len() <= limit {
        return Ok((first, quality));
    }
    let mosaic = sample_mosaic(img);
    let me = EncodeImage::rgba8(&mosaic);
    let none = EncodeMeta::default();
    let small = |q: u8| encode::encode_jpeg(&me, q, sub, &none).map(|b| b.len()).map_err(|e| e.to_string());
    let (mut q_at, mut file_at) = (quality, first.len());
    let mut m_at = small(q_at)?.max(1);
    // (an 8×8 encode's size: the headers every encode has)
    let headers = encode::encode_jpeg(&EncodeImage::rgba8(&Rgba8 { width: 8, height: 8, data: vec![[128, 128, 128, 255]; 64] }), quality, sub, &none)
        .map(|b| b.len())
        .unwrap_or(600);
    let mut margin = 0.96;
    loop {
        // file(q) ≈ fixed + k × (mosaic(q) − headers): `fixed` doesn't shrink with the quality
        let pixel_bytes = |m: usize| m.saturating_sub(headers).max(1) as f64;
        let k = file_at.saturating_sub(fixed) as f64 / pixel_bytes(m_at);
        let target = limit as f64 * margin;
        // highest quality below q_at predicted to fit (binary search on the mosaic)
        let (mut lo, mut hi) = (1u8, q_at.saturating_sub(1).max(1));
        let mut pick = 1u8;
        while lo <= hi {
            let mid = lo + (hi - lo) / 2;
            let predicted = fixed as f64 + k * pixel_bytes(small(mid)?);
            if predicted <= target {
                pick = mid;
                lo = mid + 1;
            } else if mid == 1 {
                break;
            } else {
                hi = mid - 1;
            }
        }
        let b = full(pick)?;
        if b.len() <= limit {
            return Ok((b, pick));
        }
        if pick <= 1 {
            return Err(format!("cannot fit within {kb} KB"));
        }
        // the mosaic misjudged this photo: correct the curve with what this encode gave
        (q_at, file_at) = (pick, b.len());
        m_at = small(q_at)?.max(1);
        margin *= 0.97;
    }
}

/// A sixteenth of `img`: its 32 px blocks at every fourth row and column of blocks, side by side
/// (whole JPEG MCUs, so the mosaic's blocks code as the photo's do). Small photos are used whole.
fn sample_mosaic(img: &Rgba8) -> Rgba8 {
    const T: usize = 32;
    const STEP: usize = 4;
    let (w, h) = (img.width, img.height);
    let (cols, rows) = (w / T / STEP, h / T / STEP);
    if cols == 0 || rows == 0 {
        return img.clone();
    }
    let (mw, mh) = (cols * T, rows * T);
    let mut data = vec![[0u8; 4]; mw * mh];
    for r in 0..rows {
        for c in 0..cols {
            let (sx, sy) = (c * T * STEP + T * (STEP / 2), r * T * STEP + T * (STEP / 2));
            for y in 0..T {
                let (src_y, dst_y) = (sy + y, r * T + y);
                let src = img.data.get(src_y * w + sx..src_y * w + sx + T);
                let dst = data.get_mut(dst_y * mw + c * T..dst_y * mw + c * T + T);
                if let (Some(src), Some(dst)) = (src, dst) {
                    dst.copy_from_slice(src);
                }
            }
        }
    }
    Rgba8 { width: mw, height: mh, data }
}

/// Output sharpening and the watermark on an 8-bit render in `o.effective_space()`: the render itself when
/// there is neither (a copy is ~100 MB at 24 MP).
fn finish_8bit<'a>(img: &'a Rgba8, o: &ExportOptions) -> std::borrow::Cow<'a, Rgba8> {
    if o.watermark.is_none() && !sharpens(img.width, img.height, o.sharpen) {
        return std::borrow::Cow::Borrowed(img);
    }
    let mut img = img.clone();
    let space = o.effective_space();
    output_sharpen(&mut img, space, o.sharpen, o.sharpen_amount);
    if let Some(wm) = &o.watermark {
        let wm = Watermark { color: srgb8_in(space, wm.color), target: Some(space), ..wm.clone() };
        draw_watermark(&mut img, &wm);
    }
    std::borrow::Cow::Owned(img)
}

/// Output sharpening and the watermark on a high-bit-depth render (the render itself when there is neither).
fn finish_deep<'a>(img: &'a DeepImage, o: &ExportOptions) -> std::borrow::Cow<'a, DeepImage> {
    finish_deep_over(img, None, o)
}

/// [`finish_deep`] of an image with straight alpha `alpha`, which the watermark raises.
fn finish_deep_over<'a>(img: &'a DeepImage, alpha: Option<&mut Plane>, o: &ExportOptions) -> std::borrow::Cow<'a, DeepImage> {
    if o.watermark.is_none() && !sharpens(img.width, img.height, o.sharpen) {
        return std::borrow::Cow::Borrowed(img);
    }
    let mut img = img.clone();
    output_sharpen_deep(&mut img, o.sharpen, o.sharpen_amount);
    if let Some(wm) = &o.watermark {
        let wm = Watermark { color: srgb8_in(img.space, wm.color), target: Some(img.space), ..wm.clone() };
        draw_watermark_deep_over(&mut img, alpha, &wm);
    }
    std::borrow::Cow::Owned(img)
}

/// Encode an ISO 21496-1 gain map JPEG from the SDR rendition `sdr` (8-bit, in the output space)
/// and the HDR rendition `hdr` (float linear, same space and size). The gain map is measured
/// against the base as written, so its 8-bit rounding is handed back by the map.
pub fn encode_gain_map_jpeg(sdr: &Rgba8, hdr: &DeepImage, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    encode_gain_map_with_quality(sdr, hdr, o, meta).map(|(bytes, _)| bytes)
}

/// [`encode_gain_map_jpeg`], with the JPEG quality a file-size limit settled on (None without one).
fn encode_gain_map_with_quality(sdr: &Rgba8, hdr: &DeepImage, o: &ExportOptions, meta: Option<&Metadata>) -> Result<(Vec<u8>, Option<u8>), String> {
    note_encode();
    use lcc_codecs::gainmap;
    let mut t = lap_clock();
    let base = finish_8bit(sdr, o);
    let hdr = finish_deep(hdr, o);
    lap("gain map: output sharpening and watermark", &mut t);
    let DeepSamples::F32(hv) = &hdr.samples else {
        return Err("HDR export needs a float HDR render".into());
    };
    let (w, h) = (base.width, base.height);
    if (hdr.width, hdr.height) != (w, h) || hv.len() < w * h * 3 {
        return Err("HDR and SDR renders differ in size".into());
    }
    let space = o.effective_space();
    let sdr_lin = linear_of_8bit(&base, space.trc());
    let hdr_lin: &[[f32; 3]] = hv.as_chunks::<3>().0;
    lap("gain map: SDR to linear", &mut t);
    let (map, gm) = gainmap::compute(&sdr_lin, hdr_lin, w, h, space.luma(), &gainmap::GainMapOptions::default()).map_err(|e| e.to_string())?;
    drop(sdr_lin);
    lap("gain map: compute", &mut t);
    let profile = icc::write_named(named_space(space));
    let exif = meta.map(lcc_meta::write_exif);
    let xmp = meta.map(|m| lcc_meta::write_xmp(m, None));
    let em = EncodeMeta { icc: Some(&profile), exif: exif.as_deref(), xmp: xmp.as_deref(), ppi: Some(o.ppi) };
    let sub = if o.limit_kb.is_some() { ChromaSubsampling::S420 } else { ChromaSubsampling::S444 };
    // the gain map is encoded once; a file size limit only encodes the primary image again
    let gain_jpeg = gainmap::encode_gain(&map, &gm, o.quality).map_err(|e| e.to_string())?;
    let e = EncodeImage::rgba8(&base);
    let fixed = gain_jpeg.len() + profile.len() + exif.as_ref().map_or(0, Vec::len) + xmp.as_ref().map_or(0, String::len);
    let r = encode_jpeg_with_limit(o, &base, sub, fixed, &mut |q| gainmap::encode_with_gain(&e, &gain_jpeg, q, sub, &em).map_err(|e| e.to_string()));
    lap("gain map: JPEG encode", &mut t);
    r
}

/// The linear values of an 8-bit image encoded with `trc` (a table of the 256 levels: the same
/// values as decoding each sample, in a fraction of the time), rows in parallel.
fn linear_of_8bit(img: &Rgba8, trc: lcc_pipeline::OutputTrc) -> Vec<[f32; 3]> {
    let lut: [f32; 256] = std::array::from_fn(|v| trc.decode(v as f32 / 255.0));
    let at = |v: u8| lut.get(v as usize).copied().unwrap_or(0.0);
    let mut out = vec![[0f32; 3]; img.data.len()];
    let w = img.width.max(1);
    lcc_raster::par_rows(&mut out, w, |y, row| {
        let src = img.data.get(y.saturating_mul(w)..).unwrap_or(&[]);
        for (o, p) in row.iter_mut().zip(src) {
            *o = [at(p[0]), at(p[1]), at(p[2])];
        }
    });
    out
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
    let lin = c.map(lcc_color::transfer::decode_srgb8);
    let m = lcc_color::SRGB.to_space(&space.rgb_space()).to_f32();
    let t = space.trc();
    std::array::from_fn(|i| {
        let v = m[i][0] * lin[0] + m[i][1] * lin[1] + m[i][2] * lin[2];
        (t.encode(v) * 255.0 + 0.5) as u8
    })
}

/// Encode a render according to `o`: its high-bit-depth samples when it has them (16-bit PNG/TIFF,
/// 10-bit AVIF, 32-bit float linear TIFF), else its 8-bit image.
///
/// A render with alpha ([`lcc_pipeline::Rendered::alpha`]) keeps it in the formats that
/// can ([`ExportFormat::keeps_alpha`]) when it is not opaque everywhere (issue #2976).
pub fn encode_rendered(r: &lcc_pipeline::Rendered, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    encode_rendered_with_quality(r, o, meta).map(|(bytes, _)| bytes)
}

/// [`encode_rendered`], with the JPEG quality a file-size limit settled on (None without one).
fn encode_rendered_with_quality(r: &lcc_pipeline::Rendered, o: &ExportOptions, meta: Option<&Metadata>) -> Result<(Vec<u8>, Option<u8>), String> {
    let alpha = r.alpha.as_ref().filter(|_| o.format.keeps_alpha());
    match &r.deep {
        Some(d) if o.effective_depth() != OutputDepth::U8 => encode_deep_with_alpha(d, alpha, o, meta).map(|bytes| (bytes, None)),
        _ => match alpha {
            Some(a) => encode_8bit(&with_alpha(&r.image, a)?, o, meta),
            None => encode_8bit(&r.image, o, meta),
        },
    }
}

/// `img` with straight alpha `a` (its size) in its fourth channel.
fn with_alpha(img: &Rgba8, a: &Plane) -> Result<Rgba8, String> {
    if (a.width, a.height) != (img.width, img.height) || a.data.len() != img.data.len() {
        return Err("export: the transparency does not match the rendered image".into());
    }
    let mut out = img.clone();
    for (p, &av) in out.data.iter_mut().zip(&a.data) {
        p[3] = if av.is_finite() { (av.clamp(0.0, 1.0) * 255.0).round() as u8 } else { 255 };
    }
    Ok(out)
}

/// Encode a high-bit-depth image (see [`encode_rendered`]).
pub fn encode_deep(img: &DeepImage, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    encode_deep_with_alpha(img, None, o, meta)
}

/// [`encode_deep`] keeping straight alpha `alpha` (the image's size) in PNG and TIFF when it is
/// not opaque everywhere (associated in TIFF).
fn encode_deep_with_alpha(img: &DeepImage, alpha: Option<&Plane>, o: &ExportOptions, meta: Option<&Metadata>) -> Result<Vec<u8>, String> {
    let meta_src = meta;
    let mut alpha = match alpha {
        Some(a) if (a.width, a.height) != (img.width, img.height) => return Err("export: the transparency does not match the rendered image".into()),
        a => a.cloned(),
    };
    let img = finish_deep_over(img, alpha.as_mut(), o);
    let alpha = alpha.filter(|a| match img.samples {
        DeepSamples::U16(_) => a.data.iter().any(|&v| (v.clamp(0.0, 1.0) * 65535.0).round() < 65535.0),
        DeepSamples::F32(_) => a.data.iter().any(|&v| v < 1.0),
    });
    let profile = match img.samples {
        DeepSamples::F32(_) => icc::write_matrix_trc(&img.space.rgb_space(), &lcc_codecs::Trc::Linear),
        DeepSamples::U16(_) => icc::write_named(named_space(img.space)),
    };
    let exif = meta.map(lcc_meta::write_exif);
    let xmp = meta.map(|m| lcc_meta::write_xmp(m, None));
    let meta = EncodeMeta { icc: Some(&profile), exif: exif.as_deref(), xmp: xmp.as_deref(), ppi: Some(o.ppi) };
    let (w, h) = (img.width as u32, img.height as u32);
    // RGB plus the alpha as a fourth sample
    fn rgba<T: Copy>(v: &[T], a: &Plane, alpha: impl Fn(f32) -> T) -> Vec<T> {
        v.as_chunks::<3>()
            .0
            .iter()
            .zip(&a.data)
            .flat_map(|(p, &av)| [p[0], p[1], p[2], alpha(if av.is_finite() { av.clamp(0.0, 1.0) } else { 1.0 })])
            .collect()
    }
    let (rgba16, rgba32);
    let e = match (&img.samples, alpha.as_ref().filter(|_| matches!(o.format, ExportFormat::Png | ExportFormat::Tiff))) {
        (DeepSamples::U16(v), Some(a)) => {
            rgba16 = rgba(v, a, |av| (av * 65535.0).round() as u16);
            EncodeImage::new(w, h, 4, Samples::U16(&rgba16))
        }
        (DeepSamples::F32(v), Some(a)) => {
            rgba32 = rgba(v, a, |av| av);
            EncodeImage::new(w, h, 4, Samples::F32(&rgba32))
        }
        (DeepSamples::U16(v), None) => EncodeImage::new(w, h, 3, Samples::U16(v)),
        (DeepSamples::F32(v), None) => EncodeImage::new(w, h, 3, Samples::F32(v)),
    };
    let r = match (o.format, &img.samples) {
        (ExportFormat::Png, DeepSamples::F32(v)) if img.space == OutputSpace::Rec2020 => lcc_codecs::encode_png_pq(w, h, v, &meta),
        (ExportFormat::Png, _) => encode::encode_png(&e, &meta),
        (ExportFormat::Tiff, _) => encode::encode_tiff_associated(&e, o.tiff_compression, &meta),
        (ExportFormat::Psd, _) => encode::encode_psd(&e, &meta),
        (ExportFormat::RenderedDng, DeepSamples::F32(v)) => {
            let m = meta_src.cloned().unwrap_or_default();
            return lcc_raw::write_rendered_dng(img.width, img.height, v, &img.space.rgb_space(), &m, xmp.clone()).map_err(|e| e.to_string());
        }
        (ExportFormat::Avif, DeepSamples::F32(v)) if img.space == OutputSpace::Rec2020 => {
            lcc_codecs::encode_avif_hdr(w, h, v, o.quality, 8, &meta, o.hdr_transfer.codec())
        }
        (ExportFormat::Avif, _) => encode::encode_avif(&e, o.quality, 8, &meta),
        (ExportFormat::Jxl, DeepSamples::F32(_)) if img.space == OutputSpace::Rec2020 => lcc_codecs::encode_jxl(&e, &o.jxl(img.space, true), &meta),
        (ExportFormat::Jxl, DeepSamples::U16(_)) => lcc_codecs::encode_jxl(&e, &o.jxl(img.space, false), &meta),
        (f, _) => return Err(format!("{f:?} export is 8-bit only")),
    };
    r.map_err(|e| e.to_string())
}

/// Whether the export of `photo` under `o` leaves out its location (GPS and place names).
/// (`photo` and `catalog` let library-level privacy rules add to the export option.)
pub fn removes_location(photo: &lcc_catalog::Photo, catalog: &lcc_catalog::Catalog, o: &ExportOptions) -> bool {
    // a photo inside a private saved location (Map module) is exported without its location
    o.remove_location || photo.meta.gps.is_some_and(|g| catalog.is_private_location(g))
}

/// The metadata to embed for `photo` under `o.metadata` / `o.remove_location`. `None` = embed nothing.
/// Keywords follow the catalog's keyword list (`Catalog::export_keywords`).
pub fn export_metadata(photo: &lcc_catalog::Photo, catalog: &lcc_catalog::Catalog, o: &ExportOptions) -> Option<Metadata> {
    let m = &photo.meta;
    let text = |s: &str| (!s.trim().is_empty()).then(|| s.to_string());
    // copyright info (also under "copyright only"): notice, creator, status, usage terms, info URL
    let mut out = Metadata {
        copyright: text(&m.copyright),
        copyright_marked: m.copyright_status.marked(),
        usage_terms: text(&m.usage_terms),
        copyright_url: text(&m.copyright_url),
        artist: text(&m.creator),
        software: Some(lcc_brand::DISPLAY_NAME.into()),
        ..Default::default()
    };
    match o.metadata {
        MetadataPolicy::None => return None,
        MetadataPolicy::Copyright => return Some(out),
        MetadataPolicy::CopyrightContact => {
            out.contact = m.contact.clone();
            return Some(out);
        }
        MetadataPolicy::All | MetadataPolicy::AllExceptCamera => {}
    }
    out.contact = m.contact.clone();
    out.title = text(&m.title);
    out.headline = text(&m.headline);
    out.caption = text(&m.caption);
    out.description_writer = text(&m.description_writer);
    out.category = text(&m.category);
    out.supplemental_categories = m.supplemental_categories.clone();
    out.subject_code = m.subject_code.clone();
    out.alt_text = text(&m.alt_text);
    out.extended_description = text(&m.extended_description);
    let remove_location = removes_location(photo, catalog, o);
    if !remove_location {
        out.sublocation = text(&m.location);
        out.city = text(&m.city);
        out.state = text(&m.state);
        out.country = text(&m.country);
        out.country_code = text(&m.country_code);
    }
    let keywords = catalog.export_keywords(&m.keywords);
    out.keywords = keywords.flat;
    out.hierarchical_keywords = keywords.hierarchical;
    out.capture_time = photo.captured.as_deref().and_then(DateTime::parse_iso);
    out.rating = (photo.rating > 0).then_some(photo.rating as i8);
    // Pixels are exported upright: orientation is baked in.
    out.orientation = Some(lcc_meta::Orientation::Normal);
    if !remove_location {
        out.gps = m.gps.map(|(latitude, longitude)| Gps { latitude, longitude, altitude: None });
    }
    if o.metadata == MetadataPolicy::All {
        // Make and Model as read (issue #1618); catalogs from before they were kept separately
        // only have the label, which stands in for the Model.
        let (make, model) = if m.camera_make.is_empty() && m.camera_model.is_empty() {
            (None, text(&m.camera))
        } else {
            (text(&m.camera_make), text(&m.camera_model))
        };
        out.make = make;
        out.model = model;
        out.lens_make = text(&m.lens_make);
        out.lens_model = text(&m.lens);
        out.focal_length = m.focal_mm.map(f64::from);
        out.f_number = m.aperture.map(f64::from);
        out.exposure_time = lcc_catalog::parse_shutter_seconds(&m.shutter);
        out.iso = m.iso;
    }
    Some(out)
}

/// Recognized raw XMP location properties make an Original unsafe to copy even when malformed or held
/// in an ordinary packet not projected into `Metadata` (port of upstream PR 3054). Every ordinary JPEG
/// packet and the extended one are parsed; a packet or chunk stream that can't be fully inspected
/// fails closed.
fn original_embeds_xmp_location(bytes: &[u8], label: &str) -> Result<bool, String> {
    if bytes.starts_with(b"8BPS") {
        return Ok(false);
    }
    let (embedded, ordinary_packets) = if bytes.starts_with(&[0xff, 0xd8]) {
        lcc_meta::jpeg_segments_with_all_xmp(bytes)
    } else {
        let embedded = lcc_meta::embedded(bytes);
        let packets = embedded.xmp.iter().cloned().collect();
        (embedded, packets)
    };
    let mut found = false;
    for packet in ordinary_packets.iter().map(String::as_str).chain(embedded.xmp_extended.as_deref()) {
        let xmp = lcc_meta::parse_xmp(packet)
            .map_err(|e| format!("{label}: can't verify location metadata in this original file: malformed embedded XMP: {e}"))?;
        if xmp.properties_truncated {
            return Err(format!("{label}: can't verify location metadata in this original file: embedded XMP property limit was exceeded"));
        }
        found |= [
            "exif:GPSLatitude",
            "exif:GPSLongitude",
            "exif:GPSAltitude",
            "photoshop:City",
            "photoshop:State",
            "photoshop:Country",
            "Iptc4xmpCore:Location",
        ]
        .into_iter()
        .any(|key| xmp.properties.get(key).is_some_and(|values| values.iter().any(|v| !v.trim().is_empty())));
    }
    if embedded.chunk_walk_incomplete {
        return Err(format!("{label}: can't verify location metadata in this original file: embedded PNG/WebP chunk stream is incomplete"));
    }
    Ok(found)
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
    /// The JPEG quality a file-size limit (`limitKb`) settled on: the highest up to the requested
    /// quality whose file fits. None without a limit (and for formats other than JPEG).
    pub limit_quality: Option<u8>,
    /// What the render reported without failing (e.g. a plug-in filter that was skipped).
    pub warnings: Vec<String>,
    /// A video clip's movie, in a temporary file (`bytes` is then empty): moved into place instead
    /// of written ([`Exported::save`]).
    pub video_file: Option<crate::export_video::TempFile>,
}

impl Exported {
    /// Write the export to `path` (the movie moved there, else the bytes; sidecars not included).
    pub fn save(&self, path: &str) -> Result<(), String> {
        match &self.video_file {
            Some(f) => f.move_to(path),
            None => write_file(path, &self.bytes),
        }
    }

    /// The exported file's size in bytes.
    pub fn len(&self) -> u64 {
        self.video_file.as_ref().map_or(self.bytes.len() as u64, |f| f.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Output size of photo `p` under `o` (its cropped full size when `o.resize` is `None`).
pub fn output_size(p: &lcc_catalog::Photo, o: &ExportOptions) -> (usize, usize) {
    let (w, h) = lcc_pipeline::native_output_size(p.width.max(1) as usize, p.height.max(1) as usize, &p.develop);
    // whole pixels first: a crop's float noise (5999.999… for 6000) must not decide a half-pixel tie
    let (w, h) = (w.round().max(1.0), h.round().max(1.0));
    match &o.resize {
        Some(r) => r.apply(w, h),
        None => (w as usize, h as usize),
    }
}

/// Render photo `id` at the requested size and encode it (or copy / convert its original for
/// [`ExportFormat::Original`] / [`ExportFormat::Dng`]).
pub fn export_photo(session: &mut crate::Session, id: lcc_catalog::PhotoId, o: &ExportOptions, seq: usize) -> Result<Exported, String> {
    prepare_export(session, id, o, seq)?.run()
}

/// One photo's export, set up from the session ([`prepare_export`]). [`PreparedExport::run`] does
/// the heavy part (read, decode, render, encode) without the session, so it can run on another
/// thread.
pub struct PreparedExport {
    pub photo: lcc_catalog::PhotoId,
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
    /// When the GPU is busy with other photos it renders on the processor instead of waiting
    /// ([`export_mix`] when it was prepared).
    mix: bool,
    /// Its renders may use the GPU ([`export_gpu`] when it was prepared).
    gpu: bool,
}

struct RenderWork {
    job: crate::media::RenderJob,
    /// The HDR rendition of a gain map JPEG (`job` renders its SDR base).
    hdr_job: Option<crate::media::RenderJob>,
    meta: Option<Metadata>,
    opts: ExportOptions,
    /// Source pixels the render reads (its GPU memory, for a gain map's turn at the device).
    src_px: usize,
}

enum Work {
    Render(Box<RenderWork>),
    /// A video clip exported as a movie ([`crate::export_video`]).
    Video(Box<crate::export_video::VideoWork>),
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
        /// The photo's camera-profile choice (`profile.camera`): a DNG carries the colour the photo renders with.
        camera: String,
    },
}

/// Set up the export of photo `id` at 1-based position `seq` of a batch. (For a whole batch use
/// [`prepare_batch`]: it looks at the library's originals once.)
pub fn prepare_export(session: &mut crate::Session, id: lcc_catalog::PhotoId, o: &ExportOptions, seq: usize) -> Result<PreparedExport, String> {
    let guard = std::sync::Arc::new(session.original_guard());
    prepare_guarded(session, id, o, seq, guard)
}

/// [`prepare_export`] for each of `ids` in order (`{seq}` = position, from 1).
pub fn prepare_batch(session: &mut crate::Session, ids: &[lcc_catalog::PhotoId], o: &ExportOptions) -> Result<Vec<PreparedExport>, String> {
    let guard = std::sync::Arc::new(session.original_guard());
    ids.iter().enumerate().map(|(i, id)| prepare_guarded(session, *id, o, i + 1, guard.clone())).collect()
}

fn prepare_guarded(
    session: &mut crate::Session,
    id: lcc_catalog::PhotoId,
    o: &ExportOptions,
    seq: usize,
    guard: std::sync::Arc<crate::originals::OriginalGuard>,
) -> Result<PreparedExport, String> {
    let p = session.catalog.photo(id).ok_or("no such photo")?;
    let mut file_name = o.file_name_for(p, seq);
    let source_dir = original_folder(&p.source);
    if let Some(v) = p.video.as_ref().filter(|_| o.format.is_rendered()) {
        let lcc_catalog::Source::File { path } = &p.source else {
            return Err(format!("{}: a video clip without a file", p.file_name));
        };
        let stem = file_name.strip_suffix(o.format.extension()).unwrap_or(&file_name).trim_end_matches('.');
        file_name = format!("{stem}.{}", o.video_format.extension());
        let px = (v.width as usize).saturating_mul(v.height as usize);
        let work = crate::export_video::VideoWork {
            src: std::path::PathBuf::from(path),
            trim: (v.trim_in, v.trim_out),
            settings: (*p.develop).clone(),
            format: o.video_format,
            quality: o.video_quality,
            max_side: o.video_max_side,
            file_name: file_name.clone(),
            hooks: Default::default(),
        };
        // a decoded frame, its graded copy and the pipeline's planes, plus the encoder's frames
        let weight = px.saturating_mul(96);
        return Ok(PreparedExport {
            photo: id,
            file_name,
            source_dir,
            work: Work::Video(Box::new(work)),
            guard,
            weight,
            mix: false,
            gpu: export_gpu(),
        });
    }
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
        let (nw, nh) = lcc_pipeline::native_output_size(p.width.max(1) as usize, p.height.max(1) as usize, &p.develop);
        let native_px = (nw.round().max(1.0) * nh.round().max(1.0)) as usize;
        let label = p.file_name.clone();
        let meta = export_metadata(p, &session.catalog, o);
        let gain_map = o.hdr_output() && o.format == ExportFormat::Jpeg;
        // (HDR files are written without alpha: their transparent areas stay on white)
        let alpha = o.format.keeps_alpha() && !o.hdr_output();
        let mut job = session.export_job_with_alpha(id, w, h, o.effective_space(), o.effective_depth(), alpha)?;
        job.request.exact_size = true;
        job.key ^= EXACT_SIZE_KEY;
        let weight = render_weight(full_px, long, job.level, w.saturating_mul(h));
        if let Some(why) = too_big_for_memory((w, h), native_px, weight, crate::memory::total_ram()) {
            return Err(format!("{label}: {why}"));
        }
        let hdr_job = if gain_map {
            let mut hdr = session.export_job(id, w, h, o.effective_space(), OutputDepth::F32Hdr)?;
            hdr.request.exact_size = true;
            hdr.key ^= EXACT_SIZE_KEY;
            Some(hdr)
        } else {
            None
        };
        // a gain map also holds the HDR rendition (float), the SDR one in linear light and the
        // stages the two renders share
        let weight = if gain_map { weight.saturating_add(w.saturating_mul(h).saturating_mul(GAIN_MAP_BYTES_PER_PX)) } else { weight };
        let src_px = source_px(full_px, long, job.level);
        (Work::Render(Box::new(RenderWork { job, hdr_job, meta, opts: o.clone(), src_px })), weight)
    } else {
        let lcc_catalog::Source::File { path } = &p.source else {
            return Err(format!("{} is a generated demo photo: it has no original file to export", p.file_name));
        };
        let dng = (o.format == ExportFormat::Dng).then_some(o.dng_compression);
        // the file's bytes; a DNG also decodes the raw data (16-bit samples) and writes it again
        let weight = (p.file_size as usize).saturating_add(if dng.is_some() { full_px.saturating_mul(6) } else { 0 });
        let work = Work::File {
            camera: p.develop.profile.camera.clone(),
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
    Ok(PreparedExport { photo: id, file_name, source_dir, work, guard, weight, mix: export_mix(), gpu: export_gpu() })
}

/// The folder a photo's original is in, for [`DestinationMode::SameAsOriginal`]. None for a demo
/// photo, and on the web, where an original lives in the browser's storage, not in a folder.
fn original_folder(source: &lcc_catalog::Source) -> Option<String> {
    let lcc_catalog::Source::File { path } = source else { return None };
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
    source_px(full_px, long, level).saturating_mul(12).saturating_add(out_px.saturating_mul(36))
}

/// Pixels of the source a render of a photo of `full_px` pixels (long edge `long`) reads from
/// source level `level`.
fn source_px(full_px: usize, long: usize, level: crate::media::SourceLevel) -> usize {
    let edge = level.max_edge().min(long.max(1));
    let scale = edge as f64 / long.max(1) as f64;
    (full_px as f64 * scale * scale).min(usize::MAX as f64 / 64.0) as usize
}

/// Give the clips among `items` the batch's cancel flag and progress callback.
pub fn set_video_hooks(items: &mut [PreparedExport], hooks: &crate::export_video::VideoHooks) {
    for i in items {
        if let Work::Video(w) = &mut i.work {
            w.hooks = hooks.clone();
        }
    }
}

impl PreparedExport {
    /// Check what it writes against `guard`, built now, instead of the library as it was when it was prepared: an
    /// export that waited its turn (issue #2965) may write after more photos were catalogued, and must not write
    /// over their originals either.
    pub fn use_guard(&mut self, guard: std::sync::Arc<crate::originals::OriginalGuard>) {
        self.guard = guard;
    }

    /// Whether this item is a video clip exported as a movie.
    pub fn is_video(&self) -> bool {
        matches!(self.work, Work::Video(_))
    }

    pub fn run(self) -> Result<Exported, String> {
        self.run_until(&|| false)
    }

    /// [`Self::run`], giving up ([`CANCELLED`]) between its steps (decode, renders, encode) once
    /// `stop()` says so: a cancelled export stops within a step, not after the photo.
    pub fn run_until(self, stop: &dyn Fn() -> bool) -> Result<Exported, String> {
        let check = || if stop() { Err(CANCELLED.to_string()) } else { Ok(()) };
        let file_name = self.file_name;
        let mix = self.mix;
        let gpu = self.gpu;
        match self.work {
            Work::Video(w) => {
                let out = crate::export_video::run(*w)?;
                Ok(Exported {
                    file_name,
                    bytes: Vec::new(),
                    width: out.width,
                    height: out.height,
                    sidecars: Vec::new(),
                    gpu_fallback: None,
                    limit_quality: None,
                    warnings: Vec::new(),
                    video_file: Some(out.file),
                })
            }
            Work::Render(w) => {
                let RenderWork { mut job, mut hdr_job, meta, opts, src_px } = *w;
                let out_px = job.request.max_w.saturating_mul(job.request.max_h);
                let mut t = lap_clock();
                check()?;
                // 1. The original is decoded before the photo's turn at the GPU, on this lane's
                // thread: other photos render on the GPU meanwhile (the turn used to cover the
                // decode and the encode too, so photos side by side took turns at an idle card).
                if needs_source(&job) {
                    let src = job.source.load_source()?;
                    if let Some(hj) = &mut hdr_job {
                        hj.source = crate::media::SourceRef::Loaded(Box::new(src.clone()));
                    }
                    job.source = crate::media::SourceRef::Loaded(Box::new(src));
                    lap("decode", &mut t);
                }
                // 2. The renders (the GPU's part): the SDR rendition, and a gain map's HDR one. A
                // gain map's HDR rendition starts from the SDR render's decoded original and its
                // stages instead of redoing them; the two take one turn at the GPU (the memory the
                // SDR render leaves for the HDR one is never another export's to take meanwhile).
                let one_turn = hdr_job.is_some();
                let renders = || -> Result<(lcc_pipeline::Rendered, Option<DeepImage>, Option<String>), String> {
                    lap("waiting for the GPU", &mut t);
                    // (cancelled while it waited for its turn: give the turn back at once)
                    check()?;
                    let _mark = TurnMark::enter();
                    let stages = hdr_job.as_ref().map(|_| std::sync::Arc::new(lcc_pipeline::StageCache::default()));
                    if let Some(st) = &stages {
                        job.stages = Some(st.clone());
                    }
                    let mut result = job.run();
                    lap("render", &mut t);
                    let loaded = result.loaded.take();
                    let r = result.rendered?;
                    let gpu_fallback = result.gpu_fallback;
                    let hdr = match hdr_job {
                        Some(mut hj) => {
                            check()?;
                            if let Some(src) = loaded {
                                hj.source = crate::media::SourceRef::Loaded(Box::new(src));
                            }
                            if let Some(st) = stages {
                                hj.stages = Some(st);
                            }
                            let hr = hj.run().rendered?;
                            lap("HDR render", &mut t);
                            Some(hr.deep.ok_or("the HDR render has no float samples")?)
                        }
                        None => None,
                    };
                    Ok((r, hdr, gpu_fallback))
                };
                let (r, hdr, gpu_fallback) = if mix {
                    // the GPU and the processor at once: this photo takes the GPU when it is free,
                    // else it renders on the processor rather than wait
                    match card_turn_now(src_px, out_px) {
                        Some(_turn) => renders(),
                        None => {
                            if lcc_pipeline::profiling() {
                                eprintln!("  export: the GPU is busy: rendering on the processor");
                            }
                            lcc_gpu::on_cpu(renders)
                        }
                    }
                } else if gpu && two_cards() {
                    // two cards: the photo waits for whichever has room and would finish it
                    // soonest, holding that card's turn for both its renditions (one card: as
                    // before, each render queues at the card after its plan)
                    match card_turn_waiting(src_px, out_px, &|| stop() || lcc_gpu::shutting_down()) {
                        Some(_turn) => renders(),
                        // no GPU to wait for (the renders go to the processor by themselves), or cancelled
                        None => {
                            check()?;
                            renders()
                        }
                    }
                } else if one_turn {
                    lcc_gpu::in_one_turn(src_px, out_px, renders)
                } else {
                    renders()
                }?;
                // 3. The encode, on this lane's thread after its turn (the next photo renders meanwhile)
                check()?;
                let (bytes, limit_quality) = match &hdr {
                    Some(hdr) => encode_gain_map_with_quality(&r.image, hdr, &opts, meta.as_ref())?,
                    None => encode_rendered_with_quality(&r, &opts, meta.as_ref())?,
                };
                lap("encode", &mut t);
                Ok(Exported {
                    file_name,
                    bytes,
                    width: r.image.width,
                    height: r.image.height,
                    sidecars: Vec::new(),
                    gpu_fallback,
                    limit_quality,
                    warnings: r.warnings.clone(),
                    video_file: None,
                })
            }

            Work::File { path, read, packet, dng, quality, lossy_edge, label, size, remove_location, camera } => {
                let bytes = match &read {
                    Some(r) => r(&path)?,
                    None => std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?,
                };
                let Some(dng) = dng else {
                    // the file is copied byte for byte: refuse rather than leak a location it embeds
                    if remove_location {
                        // fail closed: a PSD whose metadata can't be read might hide a location
                        let metadata = lcc_meta::try_extract(&bytes)
                            .map_err(|e| format!("{label}: can't verify location metadata in this original file: {e}"))?;
                        let embeds_xmp_gps = original_embeds_xmp_location(&bytes, &label)?;
                        let embeds_place_name =
                            [metadata.sublocation.as_deref(), metadata.city.as_deref(), metadata.state.as_deref(), metadata.country.as_deref()]
                                .into_iter()
                                .flatten()
                                .any(|value| !value.trim().is_empty());
                        if metadata.gps.is_some() || embeds_place_name || embeds_xmp_gps || lcc_meta::embeds_location(&bytes) {
                            // DNG only for what the DNG export below takes
                            let instead = if lcc_raw::probe(&bytes).is_some() { "JPEG, TIFF or DNG" } else { "JPEG or TIFF" };
                            return Err(format!(
                                "{label}: can't remove the location from this original file: it contains GPS or place names, or its location metadata could not be fully checked; export as {instead} instead"
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
                        limit_quality: None,
                        warnings: Vec::new(),
                        video_file: None,
                    });
                };
                if lcc_raw::probe(&bytes).is_none() {
                    return Err(format!("{label}: DNG export needs a raw photo"));
                }
                let mut raw = lcc_raw::decode(&bytes).map_err(|e| format!("{label}: {e}"))?;
                // the colour the photo renders with: its chosen .dcp or calibration look, else the starting colour
                crate::colour_export::with_photo_colour(&mut raw, &bytes, &camera);
                drop(bytes);
                let compression = match dng {
                    DngCompression::Lossless => lcc_raw::DngCompression::Lj92 { tile: 256 },
                    DngCompression::Deflate => lcc_raw::DngCompression::Deflate { tile: 256, half: false },
                    DngCompression::Uncompressed => lcc_raw::DngCompression::Uncompressed,
                    DngCompression::Lossy => lcc_raw::DngCompression::LossyJpeg { tile: 256, quality: quality.clamp(1, 100), long_edge: lossy_edge },
                };
                let dng = lcc_raw::write_dng(&raw, &lcc_raw::DngWriteOptions { xmp: Some(packet), compression, ..Default::default() })
                    .map_err(|e| e.to_string())?;
                Ok(Exported {
                    file_name,
                    bytes: dng,
                    width: raw.width,
                    height: raw.height,
                    sidecars: Vec::new(),
                    gpu_fallback: None,
                    limit_quality: None,
                    warnings: Vec::new(),
                    video_file: None,
                })
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
/// photo's own folder instead of `dir`), or one explicit file path (single photo). Both apply the
/// conflict policy to the file and its sidecars; Unique may change the requested basename. A
/// catalogued original (or its sidecar) is never written over: see [`crate::originals`].
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
    /// without a window (MCP, lcc-cli). `None` when there was nothing to do, else the
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
    ids: &[lcc_catalog::PhotoId],
    o: &ExportOptions,
    to: &Destination,
    write: &mut dyn FnMut(&str, &[u8]) -> Result<(), String>,
    exists: &dyn Fn(&str) -> bool,
) -> Result<Vec<serde_json::Value>, String> {
    let items = prepare_batch(session, ids, o)?;
    run_batch(items, o, to, write, exists, true, &mut |_, _| true)
}

/// Record that the photos `files` (the per-photo results of an export batch) wrote a file were exported now
/// ([`lcc_catalog::Photo::exported`]; saved with the library, not an undo step). Failures, skipped files and results that
/// name no photo (a contact sheet, a timelapse) leave their photos alone. Returns how many photos were marked.
pub fn mark_exported(session: &mut crate::Session, files: &[serde_json::Value]) -> usize {
    let now = (session.clock)();
    let mut marked = 0;
    for f in files {
        if f.get("path").is_none() || f.get("error").is_some() {
            continue;
        }
        let Some(id) = f.get("photo").and_then(serde_json::Value::as_u64).map(lcc_catalog::PhotoId) else { continue };
        if session.catalog.photo(id).is_none() {
            continue;
        }
        if session.apply_system(lcc_catalog::Op::SetExported { id, exported: Some(now.clone()) }).is_ok() {
            marked += 1;
        }
    }
    marked
}

/// [`export_batch`] with `lanes` photos side by side, on the GPU or not, the files thrown away
/// (a benchmark: [`crate::machine_test`]). Changes nothing process-wide.
pub(crate) fn export_batch_tuned(
    session: &mut crate::Session,
    ids: &[lcc_catalog::PhotoId],
    o: &ExportOptions,
    lanes: usize,
    gpu: bool,
    mix: bool,
) -> Result<Vec<serde_json::Value>, String> {
    let mut items = prepare_batch(session, ids, o)?;
    for it in &mut items {
        it.gpu = gpu;
        it.mix = gpu && mix;
    }
    let lanes = lanes.clamp(1, MAX_EXPORT_LANES).min(items.len().max(1));
    let to = Destination::default();
    let mut sink = |_: &str, _: &[u8]| Ok(());
    run_batch_with(items, &mut Placer::new(o, &to, &mut sink, &|_| false, true), &mut |_, _| true, lanes, None)
}

/// Write an exported or rendered file on disk: its folder is created if needed, and the file is
/// replaced atomically ([`lcc_catalog::safe_file::write_atomic_nosync`]: a temp file
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
    use lcc_catalog::safe_file::{write_atomic, write_atomic_nosync};
    let p = std::path::Path::new(path);
    if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    if durable { write_atomic(p, bytes) } else { write_atomic_nosync(p, bytes) }.map_err(|e| format!("{path}: {e}"))
}

/// The path of the sidecar with extension `ext` of the exported file `main`.
fn sidecar_path(main: &str, ext: &str) -> String {
    let (folder, stem, _) = split_file_name(main);
    format!("{folder}{stem}.{ext}")
}

/// `path` as folder (with its trailing separator), stem and extension. Both `/` and `\` separate
/// folders, so a file and its sidecars get the same stem on every platform; a leading dot (a
/// hidden file's) starts no extension (upstream #1415).
fn split_file_name(path: &str) -> (&str, &str, Option<&str>) {
    let (folder, name) = path.split_at(path.rfind(['/', '\\']).map_or(0, |i| i + 1));
    match name.rfind('.').filter(|&i| i > 0) {
        Some(i) => (folder, name.get(..i).unwrap_or(name), name.get(i + 1..)),
        None => (folder, name, None),
    }
}

#[cfg(test)]
pub(crate) fn sidecar_path_for_tests(main: &str, ext: &str) -> String {
    sidecar_path(main, ext)
}

#[cfg(test)]
pub(crate) fn split_file_name_for_tests(path: &str) -> (String, String, Option<String>) {
    let (a, b, c) = split_file_name(path);
    (a.to_string(), b.to_string(), c.map(str::to_string))
}

/// Why an explicitly requested export path can't be used, whatever the conflict policy: it names
/// a folder, or a catalogued original. (Its sidecars only collide, so they follow the policy.)
fn exact_target_refusal(requested: &str, guard: &crate::originals::OriginalGuard) -> Result<(), String> {
    if requested.ends_with(['/', '\\']) || std::path::Path::new(requested).is_dir() {
        return Err(format!("{requested}: is a folder; give the file's own name (or `dir` for a folder export)"));
    }
    guard.check(std::path::Path::new(requested))
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
    run_batch_cancellable(items, o, to, write, exists, stop_on_error, progress, None)
}

/// [`run_batch`] that also stops within about a second once `cancel` is set (a cancelled export
/// stayed "cancelling" for minutes while the photos in flight finished). The photos then in
/// flight are dropped (their lanes stop at their next step and give their memory back); the files
/// already written stay. Returns the files placed until then.
#[allow(clippy::too_many_arguments)]
pub fn run_batch_cancellable(
    items: Vec<PreparedExport>,
    o: &ExportOptions,
    to: &Destination,
    write: &mut dyn FnMut(&str, &[u8]) -> Result<(), String>,
    exists: &dyn Fn(&str) -> bool,
    stop_on_error: bool,
    progress: &mut dyn FnMut(usize, &str) -> bool,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<Vec<serde_json::Value>, String> {
    let lanes = export_parallelism(items.len());
    run_batch_with(items, &mut Placer::new(o, to, write, exists, stop_on_error), progress, lanes, cancel)
}

/// [`export_batch`] with `lanes` photos side by side and nothing written (measurements, tests).
#[cfg(test)]
pub(crate) fn export_batch_lanes(
    session: &mut crate::Session,
    ids: &[lcc_catalog::PhotoId],
    o: &ExportOptions,
    lanes: usize,
    mix: bool,
) -> Result<Vec<serde_json::Value>, String> {
    let mut items = prepare_batch(session, ids, o)?;
    for it in &mut items {
        it.mix = mix;
    }
    let to = Destination::default();
    let mut write = |_: &str, _: &[u8]| Ok(());
    let lanes = lanes.clamp(1, MAX_EXPORT_LANES).min(items.len().max(1));
    run_batch_with(items, &mut Placer::new(o, &to, &mut write, &|_| false, true), &mut |_, _| true, lanes, None)
}

/// How many photos of a batch of `n` render side by side. Each render already spreads its rows
/// over every core, but parts of it (decode, Smooth Skin, encode) run on one thread, so on a big
/// machine more lanes keep the cores busy: half the cores, up to 12 (14 saturated a 12 GB GPU;
/// memory still caps them through [`crate::memory::export_gate`]). One on wasm32 (no threads).
pub fn export_parallelism(n: usize) -> usize {
    if cfg!(target_arch = "wasm32") {
        return 1;
    }
    let env = lcc_brand::env("EXPORT_LANES").and_then(|v| v.trim().parse::<usize>().ok()).filter(|n| *n > 0);
    let set = Some(LANES.load(std::sync::atomic::Ordering::Relaxed)).filter(|n| *n > 0);
    let jobs = match export_cards() {
        _ if !two_cards() => (0, 0),
        // (the card for editing alone: as on one card)
        ExportCards::Editing => (0, 0),
        _ => lcc_gpu::batch_jobs_per_card(),
    };
    let lanes = env.or(set.map(|l| lanes_for_two_cards(l, jobs))).unwrap_or_else(|| {
        let cores = std::thread::available_parallelism().map_or(1, |c| c.get());
        auto_lanes(cores, jobs.1)
    });
    lanes.clamp(1, MAX_EXPORT_LANES).min(n.max(1))
}

/// The lanes for `set` lanes chosen on one card ([`set_export_lanes`], from Test my machine) when
/// two cards work: at least the turns both cards give out at once plus one, so a small setting
/// doesn't keep every photo on the second card (`<PREFIX>_EXPORT_LANES` still means exactly that).
fn lanes_for_two_cards(set: usize, jobs: (usize, usize)) -> usize {
    if jobs.1 == 0 { set } else { set.max(jobs.0.saturating_add(jobs.1).saturating_add(1)) }
}

/// The automatic lanes on a computer with `cores` threads whose second graphics card takes
/// `helper_jobs` renders at once: half the cores, up to 12, plus the second card's, so it has
/// photos to take while the others decode and encode (memory still caps them).
fn auto_lanes(cores: usize, helper_jobs: usize) -> usize {
    (cores / 2).clamp(1, 12).saturating_add(helper_jobs).min(MAX_EXPORT_LANES)
}

/// Exports use two graphics cards now: GPU export is on and the second card is ready.
fn two_cards() -> bool {
    export_gpu() && lcc_gpu::batch_cards() >= 2
}

/// The most photos an export renders side by side, whatever is asked.
pub const MAX_EXPORT_LANES: usize = 64;

/// The graphics cards exports use when the computer has a second one (Settings ▸ Performance ▸
/// Graphics cards for exports; [`set_export_cards`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExportCards {
    /// Both cards, each photo on the one that would finish it soonest (the default).
    #[default]
    Both,
    /// The card for editing only: the second card is left alone.
    Editing,
    /// The second card only, while it works (to measure it; the card for editing takes over when
    /// it isn't ready or stops).
    Second,
}

impl ExportCards {
    /// `both`, `editing` or `second` (and a benchmark's `on`, `off`, `only`).
    pub fn parse(s: &str) -> Option<ExportCards> {
        match s.trim() {
            "both" | "on" => Some(ExportCards::Both),
            "editing" | "off" => Some(ExportCards::Editing),
            "second" | "only" | "helper" => Some(ExportCards::Second),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ExportCards::Both => "both",
            ExportCards::Editing => "editing",
            ExportCards::Second => "second",
        }
    }
}

static EXPORT_CARDS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Use `cards` for exports from now on (Settings, benchmarks).
pub fn set_export_cards(cards: ExportCards) {
    let v = match cards {
        ExportCards::Both => 0,
        ExportCards::Editing => 1,
        ExportCards::Second => 2,
    };
    EXPORT_CARDS.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// The cards exports use ([`set_export_cards`]).
pub fn export_cards() -> ExportCards {
    match EXPORT_CARDS.load(std::sync::atomic::Ordering::Relaxed) {
        1 => ExportCards::Editing,
        2 => ExportCards::Second,
        _ => ExportCards::Both,
    }
}

/// Which card a photo of an export under `cards` may take a turn on now, in order (`true`: the
/// second card), when the second card is `helper_ready`; empty: either, whichever would finish it
/// soonest ([`lcc_gpu::batch_turn_any`]).
fn cards_for(cards: ExportCards, helper_ready: bool) -> &'static [bool] {
    match cards {
        ExportCards::Both => &[],
        ExportCards::Editing => &[false],
        ExportCards::Second if helper_ready => &[true],
        // (no second card ready: the card for editing, so the export isn't stuck)
        ExportCards::Second => &[false],
    }
}

/// A photo's turn on a card with room now ([`export_cards`]); `None`: busy (or no GPU), the photo
/// renders on the processor.
fn card_turn_now(src_px: usize, out_px: usize) -> Option<lcc_gpu::BatchTurn> {
    match cards_for(export_cards(), lcc_gpu::helper_ready()) {
        [] => lcc_gpu::try_batch_turn(src_px, out_px),
        only => only.iter().find_map(|h| lcc_gpu::try_batch_turn_on(*h, src_px, out_px)),
    }
}

/// [`card_turn_now`], waiting until the card has room. `None` when `stop()` says so (cancelled),
/// or at once without a GPU to wait for. A second card lost while the photo waits for it stops
/// being waited for: the card for editing takes the photo.
fn card_turn_waiting(src_px: usize, out_px: usize, stop: &dyn Fn() -> bool) -> Option<lcc_gpu::BatchTurn> {
    loop {
        let only = cards_for(export_cards(), lcc_gpu::helper_ready());
        if only.is_empty() {
            return lcc_gpu::wait_batch_turn_any(src_px, out_px, stop);
        }
        if let Some(t) = only.iter().find_map(|h| lcc_gpu::try_batch_turn_on(*h, src_px, out_px)) {
            return Some(t);
        }
        // (no GPU at all: the renders go to the processor by themselves)
        if lcc_gpu::batch_cards() == 0 || stop() || lcc_gpu::shutting_down() {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
}

/// Export lanes chosen by [`set_export_lanes`] (`0`: automatic).
static LANES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Render `lanes` photos side by side in exports from now on (`None`: the automatic number). The
/// environment's `<PREFIX>_EXPORT_LANES` still wins (for measurements).
pub fn set_export_lanes(lanes: Option<usize>) {
    LANES.store(lanes.unwrap_or(0).min(MAX_EXPORT_LANES), std::sync::atomic::Ordering::Relaxed);
}

/// The lanes asked for: `<PREFIX>_EXPORT_LANES`, else [`set_export_lanes`]'s; `None`: automatic.
pub fn export_lanes_override() -> Option<usize> {
    let env = lcc_brand::env("EXPORT_LANES").and_then(|v| v.trim().parse::<usize>().ok()).filter(|n| *n > 0);
    env.or(Some(LANES.load(std::sync::atomic::Ordering::Relaxed)).filter(|n| *n > 0))
}

/// Whether an export's render job decodes its original when it runs (no cached picture stands in
/// for it): [`PreparedExport::run`] then decodes it first, outside the photo's turn at the GPU.
fn needs_source(job: &crate::media::RenderJob) -> bool {
    job.cache.is_none() && job.view_cache.is_none() && job.preview_cut.is_none() && !matches!(job.source, crate::media::SourceRef::Loaded(_))
}

thread_local! {
    /// This thread is inside a photo's renders ([`TurnMark`]): what holds its turn at the GPU.
    static IN_RENDERS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Marks the renders of one photo (what may hold its turn at the GPU) on this thread.
struct TurnMark;

impl TurnMark {
    fn enter() -> Self {
        let _ = IN_RENDERS.try_with(|c| c.set(c.get().saturating_add(1)));
        TurnMark
    }
}

impl Drop for TurnMark {
    fn drop(&mut self) {
        let _ = IN_RENDERS.try_with(|c| c.set(c.get().saturating_sub(1)));
    }
}

/// Encodes that ran inside a photo's renders (tests: an encode must never hold the GPU turn).
#[cfg(test)]
pub(crate) static ENCODES_IN_TURN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Count an encode that runs inside a photo's renders (tests).
fn note_encode() {
    #[cfg(test)]
    if IN_RENDERS.try_with(|c| c.get() > 0).unwrap_or(false) {
        ENCODES_IN_TURN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Exports render on the processor too while the GPU is busy with other photos. Off by default;
/// "Test my machine" turns it on where it measured it faster.
static EXPORT_MIX: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Let photos of an export render on the processor while the GPU renders others ([`export_mix`]).
pub fn set_export_mix(on: bool) {
    EXPORT_MIX.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Whether exports use the processor and the GPU at once ([`set_export_mix`], or
/// `<PREFIX>_EXPORT_MIX=1` for measurements).
pub fn export_mix() -> bool {
    EXPORT_MIX.load(std::sync::atomic::Ordering::Relaxed) || lcc_brand::env("EXPORT_MIX").is_some_and(|v| v.trim() == "1")
}

/// Working memory a gain map JPEG adds per output pixel ([`encode_gain_map_jpeg`]): the float HDR
/// rendition (12 B), the SDR one in linear light (12 B) and the stages its two renders share.
const GAIN_MAP_BYTES_PER_PX: usize = 48;
/// Mixed into an export job's key: its exact-size request renders differently from a fitted one.
const EXACT_SIZE_KEY: u64 = 0x86e2_fadc_0917_b435;

/// [`run_batch`] with `lanes` photos in flight. Without `cancel` the calling thread is one of the
/// lanes; with it the lanes are threads of their own and this one only places the files, looking
/// at `cancel` while it waits, so it can leave at once: lanes still in flight then stop at their
/// next step on their own.
/// Exports render on the GPU when there is one (`false`: on the processor; the install settings,
/// when "Test my machine" found the processor alone faster).
static EXPORT_GPU: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Let exports use the GPU or not (views and the rest of the app keep it either way).
pub fn set_export_gpu(on: bool) {
    EXPORT_GPU.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Whether exports use the GPU ([`set_export_gpu`]).
pub fn export_gpu() -> bool {
    EXPORT_GPU.load(std::sync::atomic::Ordering::Relaxed)
}

fn run_batch_with(
    items: Vec<PreparedExport>,
    placer: &mut Placer<'_>,
    progress: &mut dyn FnMut(usize, &str) -> bool,
    lanes: usize,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<Vec<serde_json::Value>, String> {
    placer.single = items.len() == 1;
    let cancelled = || cancel.as_ref().is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed));
    if cfg!(target_arch = "wasm32") || (cancel.is_none() && (lanes <= 1 || items.len() <= 1)) {
        for (i, item) in items.into_iter().enumerate() {
            if !progress(i, &item.file_name) || cancelled() {
                break;
            }
            let (photo, name, guard, source_dir) = (item.photo, item.file_name.clone(), item.guard.clone(), item.source_dir.clone());
            let r = run_gated(item, &cancelled);
            if cancelled() {
                break;
            }
            placer.place(photo, name, source_dir.as_deref(), &guard, r)?;
        }
        return Ok(std::mem::take(&mut placer.out));
    }
    let heads: Vec<_> = items.iter().map(|i| (i.photo, i.file_name.clone(), i.guard.clone(), i.source_dir.clone())).collect();
    let lanes = lanes.max(1);
    let queue = std::sync::Arc::new(Queue {
        lane: std::sync::Mutex::new(Lane { items: items.into_iter().map(Some).collect(), next: 0, placed: 0, stop: false, done: Default::default() }),
        cv: std::sync::Condvar::new(),
        ahead: lanes,
        cancel: cancel.clone(),
        lanes: std::sync::atomic::AtomicUsize::new(0),
    });
    // stops the lanes however this thread leaves: an error, a cancel, a panic in `progress` or the
    // placer (lanes still in flight then stop at their next step on their own)
    let _stop = StopOnDrop(queue.clone());
    // (without a cancel flag the calling thread is a lane too: one thread fewer)
    let threads = if cancel.is_some() { lanes } else { lanes - 1 };
    for k in 0..threads {
        let q = queue.clone();
        // a lane that can't start leaves its photos to the others (and to this thread)
        if std::thread::Builder::new().name(format!("export-{}", k + 1)).spawn(move || q.work()).is_err() {
            break;
        }
        queue.lanes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    for (i, (photo, name, guard, source_dir)) in heads.into_iter().enumerate() {
        if !progress(i, &name) || queue.cancelled() {
            break;
        }
        let Some(e) = queue.result(i) else { break };
        placer.place(photo, name, source_dir.as_deref(), &guard, e)?;
    }
    Ok(std::mem::take(&mut placer.out))
}

/// Stops a [`Queue`] when dropped, also while unwinding from a panic.
struct StopOnDrop(std::sync::Arc<Queue>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stop();
    }
}

/// Run one photo's export holding its working memory from [`crate::memory::export_gate`]; a panic
/// becomes that photo's error, not the batch's end. `stop()`: the batch was cancelled.
fn run_gated(item: PreparedExport, stop: &dyn Fn() -> bool) -> Result<Exported, String> {
    if stop() {
        return Err(CANCELLED.into());
    }
    let _held = crate::memory::export_gate().acquire(item.weight);
    let what = format!("exporting {}", item.file_name);
    // (its GPU renders take turns with the other photos' and stay behind the window's: `lcc_gpu::as_batch`)
    // (on the processor when the GPU is turned off for exports: `set_export_gpu`)
    let gpu = item.gpu;
    let render = || lcc_gpu::as_batch(|| item.run_until(stop));
    crate::guard::catch(&what, || if gpu { render() } else { lcc_gpu::on_cpu(render) }).and_then(|r| r)
}

/// The photos of a batch shared by its lanes: each lane takes the next one, at most
/// [`Queue::ahead`] past the last placed. That bounds the finished files waiting in memory for
/// their turn too (their working memory is given back to the gate as soon as they are encoded).
struct Queue {
    lane: std::sync::Mutex<Lane>,
    cv: std::sync::Condvar,
    ahead: usize,
    /// The batch's cancel flag ([`run_batch_cancellable`]).
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Lane threads started.
    lanes: std::sync::atomic::AtomicUsize,
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

/// How often the placer looks at the cancel flag while it waits for a photo.
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(100);

impl Queue {
    fn lock(&self) -> std::sync::MutexGuard<'_, Lane> {
        self.lane.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn wait<'a>(&self, g: std::sync::MutexGuard<'a, Lane>) -> std::sync::MutexGuard<'a, Lane> {
        self.cv.wait_timeout(g, CANCEL_POLL).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0)
    }

    fn cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Stopped: cancelled, or the batch ended early.
    fn stopping(&self) -> bool {
        self.cancelled() || self.lock().stop
    }

    fn finish(&self, i: usize, r: Result<Exported, String>) {
        let mut l = self.lock();
        // (a cancelled or ended batch keeps no results nobody will collect)
        if !l.stop && !self.cancelled() {
            l.done.insert(i, r);
        }
        drop(l);
        self.cv.notify_all();
    }

    /// A lane thread: run photos until none is left or the batch stops.
    fn work(&self) {
        loop {
            let mut l = self.lock();
            let (i, item) = loop {
                if l.stop || self.cancelled() || l.next >= l.items.len() {
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
            let r = run_gated(item, &|| self.stopping());
            self.finish(i, r);
        }
    }

    /// Photo `i`'s result, waiting for it; run here when no lane has started it yet (and this
    /// thread is a lane, or no lane could start). `None`: the batch was cancelled meanwhile.
    fn result(&self, i: usize) -> Option<Result<Exported, String>> {
        let mut l = self.lock();
        loop {
            if self.cancelled() {
                return None;
            }
            if let Some(r) = l.done.remove(&i) {
                l.placed = i + 1;
                drop(l);
                self.cv.notify_all();
                return Some(r);
            }
            let runs_here = self.cancel.is_none() || self.lanes.load(std::sync::atomic::Ordering::Relaxed) == 0;
            if l.next == i && runs_here {
                let Some((_, item)) = l.take_next() else {
                    return Some(Err("export: photo missing from the batch".into()));
                };
                drop(l);
                let r = run_gated(item, &|| self.cancelled());
                self.finish(i, r);
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
    /// folder of its own, whose folder is gone, or a subfolder that would leave the folder: never
    /// a file written somewhere else instead.
    fn folder_for(&self, name: &str, source_dir: Option<&str>) -> Result<String, String> {
        if let Some(why) = subfolder_problem(&self.o.subfolder) {
            return Err(format!("{name}: {why}"));
        }
        let base = match self.o.destination_mode {
            DestinationMode::SpecificFolder | DestinationMode::ChooseLater => self.to.dir.clone(),
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
        let sub = self.o.subfolder.trim();
        Ok(if sub.is_empty() { base } else { join_path(&base, sub) })
    }

    /// Place photo `photo`'s export result (file `name`, its original in `source_dir`): write it
    /// and record the outcome. `Err` only when it failed and `stop_on_error` is set.
    fn place(
        &mut self,
        photo: lcc_catalog::PhotoId,
        name: String,
        source_dir: Option<&str>,
        guard: &crate::originals::OriginalGuard,
        e: Result<Exported, String>,
    ) -> Result<(), String> {
        use serde_json::json;
        // an exact file path (one photo) needs no folder
        let exact = self.to.exact.as_deref().filter(|_| self.single);
        let dir = if exact.is_some() { Ok(String::new()) } else { self.folder_for(&name, source_dir) };
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
        let requested = exact.map_or_else(|| join_path(dir, &e.file_name), str::to_string);
        // An explicitly requested folder or original is an error, not a candidate for renaming or skipping.
        let refused = if exact.is_some() { exact_target_refusal(&requested, guard) } else { Ok(()) };
        let taken = &self.taken;
        let busy = |main: &str| group(main).iter().any(|p| taken.contains(p) || exists(p));
        let unique = || {
            // keep the requested folder and basename, split as the sidecars' names are
            let (folder, stem, ext) = split_file_name(&requested);
            let name = |n: usize| ext.map_or_else(|| format!("{folder}{stem}-{n}"), |x| format!("{folder}{stem}-{n}.{x}"));
            (2..1_000_000).map(name).find(|p| !busy(p)).ok_or_else(|| format!("{requested}: no free file name"))
        };
        let path = if let Err(err) = refused {
            Err(err)
        } else if !busy(&requested) {
            Ok(requested.clone())
        } else {
            match o.conflict {
                // Overwrite replaces files from earlier runs, never another photo of this
                // same batch: a name already written by this batch gets the next free one
                Conflict::Overwrite if group(&requested).iter().any(|p| taken.contains(p)) => unique(),
                Conflict::Overwrite => Ok(requested.clone()),
                Conflict::Skip => {
                    self.out.push(json!({"skipped": requested}));
                    return Ok(());
                }
                Conflict::Unique => unique(),
            }
        };
        let file = path.clone().unwrap_or_else(|_| exact.map_or_else(|| name.clone(), str::to_string));
        let write = &mut *self.write;
        let written = path.and_then(|path| {
            let files = group(&path);
            // never over an original, whatever the conflict policy or the exact path said
            // (rechecked for the selected main file too: collision probes can outlive its early validation)
            for f in &files {
                guard.check(std::path::Path::new(f))?;
            }
            match &e.video_file {
                Some(f) => f.move_to(&path)?,
                None => write(&path, &e.bytes)?,
            }
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
                let mut entry = json!({"photo": photo.0, "path": path, "width": e.width, "height": e.height, "bytes": e.len(), "sidecars": sidecars});
                if e.video_file.is_some() {
                    entry["video"] = json!(true);
                }
                if let Some(why) = &e.gpu_fallback {
                    entry["gpuFallback"] = json!(why);
                }
                if let Some(q) = e.limit_quality {
                    entry["quality"] = json!(q);
                }
                if !e.warnings.is_empty() {
                    entry["warnings"] = json!(e.warnings);
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
mod tests_two_cards {
    // With a second card, the automatic lanes grow by the renders it takes at once (one card: as
    // before), and a lanes setting made on one card is raised so both cards get photos
    #[test]
    fn each_export_choice_takes_its_own_cards() {
        use super::{ExportCards, cards_for};
        assert_eq!(cards_for(ExportCards::Both, true), &[] as &[bool], "both: whichever would finish soonest");
        assert_eq!(cards_for(ExportCards::Editing, true), &[false], "the card for editing only");
        assert_eq!(cards_for(ExportCards::Second, true), &[true], "the second card only");
        assert_eq!(cards_for(ExportCards::Second, false), &[false], "no second card: the card for editing");
        // (the process-wide choice is left alone: other exports run beside this test)
        for c in [ExportCards::Both, ExportCards::Editing, ExportCards::Second] {
            assert_eq!(ExportCards::parse(c.name()), Some(c));
        }
        assert_eq!(
            (ExportCards::parse("off"), ExportCards::parse("only"), ExportCards::parse("x")),
            (Some(ExportCards::Editing), Some(ExportCards::Second), None)
        );
        let saved: ExportCards = serde_json::from_value(serde_json::json!("second")).unwrap();
        assert_eq!(saved, ExportCards::Second);
    }

    #[test]
    fn a_second_card_gets_lanes_of_its_own() {
        assert_eq!(super::auto_lanes(16, 0), 8);
        assert_eq!(super::auto_lanes(16, 4), 12);
        assert_eq!(super::auto_lanes(1, 0), 1);
        assert!(super::auto_lanes(usize::MAX, usize::MAX) <= super::MAX_EXPORT_LANES);
        assert_eq!(super::lanes_for_two_cards(2, (4, 0)), 2, "one card: the setting as it is");
        assert_eq!(super::lanes_for_two_cards(2, (1, 4)), 6, "both cards' turns plus one");
        assert_eq!(super::lanes_for_two_cards(10, (1, 4)), 10, "never fewer than set");
        assert_eq!(super::lanes_for_two_cards(usize::MAX, (usize::MAX, usize::MAX)), usize::MAX);
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

    /// A detailed photo-like image (gradients, texture, edges) of `w × h`.
    fn detailed(w: usize, h: usize) -> Rgba8 {
        let mut img = Rgba8::new(w, h);
        let mut seed = 12345u32;
        for y in 0..h {
            for x in 0..w {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let n = (seed >> 24) as i32 - 128;
                let base = ((x * 255 / w) as i32 + ((y / 40) % 2) as i32 * 60 + (((x as f32 / 9.0).sin() * 30.0) as i32)).clamp(0, 255);
                let c = |o: i32| (base + o + n / 3).clamp(0, 255) as u8;
                img.data[y * w + x] = [c(0), c(-20), c(25), 255];
            }
        }
        img
    }

    /// "Limit file size" made a full encode per step of a binary search (7-8 of a 6000 px
    /// photo). Now at most two: the quality is predicted from the first encode's size.
    #[test]
    fn a_file_size_limit_takes_at_most_two_encodes() {
        let img = detailed(1600, 1100);
        let full = encode_image(&img, &ExportOptions { quality: 90, limit_kb: None, ..Default::default() }).unwrap();
        for share in [0.8, 0.5, 0.3, 0.15] {
            let kb = ((full.len() as f64 * share) / 1024.0) as u32;
            FIT_ENCODES.with(|c| c.set(0));
            let b = encode_image(&img, &ExportOptions { quality: 90, limit_kb: Some(kb), ..Default::default() }).unwrap();
            let n = FIT_ENCODES.with(|c| c.get());
            assert!(b.len() <= kb as usize * 1024, "{} bytes over {kb} KB", b.len());
            assert!(n <= 2, "{n} encodes for {share} of the size");
            // and not much smaller than it may be: the quality is close to the best that fits
            assert!(b.len() as f64 >= kb as f64 * 1024.0 * 0.6, "{} bytes for {kb} KB", b.len());
        }
        // one encode when it fits already
        FIT_ENCODES.with(|c| c.set(0));
        encode_image(&img, &ExportOptions { quality: 90, limit_kb: Some(100_000), ..Default::default() }).unwrap();
        assert_eq!(FIT_ENCODES.with(|c| c.get()), 1);
    }

    /// The limit holds for a gain map JPEG too (it used to only switch to 4:2:0), in at most two
    /// encodes of the primary image and one of the gain map.
    #[test]
    fn a_gain_map_jpeg_fits_the_file_size_limit() {
        let sdr = detailed(1200, 800);
        let hdr = DeepImage {
            width: 1200,
            height: 800,
            space: OutputSpace::Srgb,
            samples: DeepSamples::F32(
                sdr.data
                    .iter()
                    .flat_map(|p| {
                        let l = |v: u8| lcc_pipeline::OutputTrc::Srgb.decode(v as f32 / 255.0);
                        let g = if p[0] > 200 { 3.0 } else { 1.0 };
                        [l(p[0]) * g, l(p[1]) * g, l(p[2]) * g]
                    })
                    .collect(),
            ),
        };
        let o = ExportOptions { quality: 90, hdr: true, ..Default::default() };
        let free = encode_gain_map_jpeg(&sdr, &hdr, &o, None).unwrap();
        let kb = (free.len() as f64 * 0.5 / 1024.0) as u32;
        FIT_ENCODES.with(|c| c.set(0));
        let b = encode_gain_map_jpeg(&sdr, &hdr, &ExportOptions { limit_kb: Some(kb), ..o }, None).unwrap();
        assert!(b.len() <= kb as usize * 1024, "{} bytes over {kb} KB", b.len());
        assert!(FIT_ENCODES.with(|c| c.get()) <= 2);
        assert!(lcc_codecs::gainmap::read_jpeg(&b).is_some(), "still a gain map JPEG");
    }

    /// The gain map's linear SDR (a table of the 256 levels) is each sample decoded on its own.
    #[test]
    fn the_linear_sdr_table_is_exact() {
        let img = Rgba8::from_fn(23, 11, |x, y| [(x * 11) as u8, (y * 23) as u8, ((x + y) * 7) as u8, 255]);
        for space in [OutputSpace::Srgb, OutputSpace::AdobeRgb, OutputSpace::ProPhoto, OutputSpace::Rec2020] {
            let trc = space.trc();
            let lin = linear_of_8bit(&img, trc);
            assert_eq!(lin.len(), img.data.len());
            for (p, l) in img.data.iter().zip(&lin) {
                for c in 0..3 {
                    assert_eq!(l[c].to_bits(), trc.decode(p[c] as f32 / 255.0).to_bits(), "{space:?}");
                }
            }
        }
    }

    /// A batch's lanes come from the setting (the environment's override aside), never more than
    /// the batch has photos.
    #[test]
    fn export_lanes_follow_the_setting() {
        if lcc_brand::env("EXPORT_LANES").is_some() {
            return;
        }
        set_export_lanes(Some(3));
        assert_eq!(export_lanes_override(), Some(3));
        assert_eq!(export_parallelism(10), 3);
        assert_eq!(export_parallelism(2), 2);
        set_export_lanes(Some(10_000));
        assert_eq!(export_parallelism(1000), MAX_EXPORT_LANES);
        set_export_lanes(None);
        assert_eq!(export_lanes_override(), None);
        assert!(export_parallelism(1000) >= 1);
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
                let decoded = lcc_codecs::decode(&bytes, Default::default()).unwrap();
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
            let decoded = lcc_codecs::decode(&bytes, Default::default()).unwrap();
            assert!(decoded.has_alpha, "{format:?} must retain real alpha");
            assert_eq!(decoded.to_srgb8().data, img.data, "{format:?} samples");
        }
    }

    /// Issue #2976: on a transparent area of an export that keeps alpha, the watermark composites
    /// over it and raises the alpha, so it shows; opaque pixels blend as before.
    #[test]
    fn watermark_shows_over_transparency() {
        let wm = Watermark { text: "LC".into(), size: 0.3, opacity: 1.0, shadow: false, color: [255, 255, 255], ..Default::default() };
        let mut clear = Rgba8::from_fn(200, 100, |_, _| [0, 0, 0, 0]);
        draw_watermark(&mut clear, &wm);
        let marked: Vec<&[u8; 4]> = clear.data.iter().filter(|p| p[3] > 0).collect();
        assert!(!marked.is_empty(), "the mark raises the alpha");
        assert!(marked.iter().all(|p| p[0] == 255 && p[1] == 255 && p[2] == 255), "in its own colour, not faded towards black");
        assert!(marked.iter().any(|p| p[3] == 255));
        let mut opaque = Rgba8::from_fn(200, 100, |_, _| [0, 0, 0, 255]);
        draw_watermark(&mut opaque, &wm);
        assert!(opaque.data.iter().all(|p| p[3] == 255));
        let lit = |i: &Rgba8| i.data.iter().filter(|p| p[0] > 128).count();
        assert!(lit(&opaque) > 0);
        // straight-alpha deep images: the plane is raised the same way
        let mut deep = DeepImage { width: 200, height: 100, space: OutputSpace::Srgb, samples: DeepSamples::U16(vec![0; 200 * 100 * 3]) };
        let mut alpha = Plane::new(200, 100);
        draw_watermark_deep_over(&mut deep, Some(&mut alpha), &wm);
        assert!(alpha.data.iter().any(|&a| a > 0.99) && alpha.data.contains(&0.0));
    }

    /// A size limit can cut the quality far below the requested one, so each file says what it
    /// settled on; without a limit the file is written at the requested quality and says nothing.
    #[test]
    fn a_size_limit_reports_the_quality_it_settled_on() {
        let img = test_image();
        let full = encode_image(&img, &ExportOptions { quality: 90, ..Default::default() }).unwrap();
        let kb = (full.len() / 1024 / 3).max(2) as u32;
        let (bytes, q) = encode_8bit(&img, &ExportOptions { quality: 90, limit_kb: Some(kb), ..Default::default() }, None).unwrap();
        assert!(bytes.len() <= kb as usize * 1024);
        assert!(q.is_some_and(|q| (1..90).contains(&q)), "{q:?}");
        let (_, roomy) = encode_8bit(&img, &ExportOptions { quality: 90, limit_kb: Some(u32::MAX), ..Default::default() }, None).unwrap();
        assert_eq!(roomy, Some(90), "a limit that changed nothing keeps the requested quality");
        let (_, plain) = encode_8bit(&img, &ExportOptions { quality: 90, ..Default::default() }, None).unwrap();
        assert_eq!(plain, None);
        let (_, png) = encode_8bit(&img, &ExportOptions { format: ExportFormat::Png, limit_kb: Some(kb), ..Default::default() }, None).unwrap();
        assert_eq!(png, None, "only JPEG has a quality to report");
    }

    #[test]
    fn a_size_limit_is_tight_below_half_a_bit_per_pixel() {
        assert!(size_limit_is_tight(1024, 5152 * 7728), "1 MB over 40 MP is about 0.2 bits per pixel");
        assert!(!size_limit_is_tight(1024, 1365 * 2048), "about 3 bits per pixel");
        assert!(!size_limit_is_tight(1024, 0));
        assert!(size_limit_is_tight(0, 1000));
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
            samples: DeepSamples::F32(img.data.iter().flat_map(|p| p[..3].iter().map(|v| lcc_color::transfer::decode_srgb8(*v) * 4.0)).collect()),
        };
        let opts = ExportOptions { hdr: true, quality: 100, limit_kb: Some(u32::MAX), ..Default::default() };
        let full = encode_gain_map_jpeg(&img, &hdr, &opts, None).unwrap();
        let kb = (full.len() / 2048) as u32;
        let (limited, q) = encode_gain_map_with_quality(&img, &hdr, &ExportOptions { limit_kb: Some(kb), ..opts.clone() }, None).unwrap();
        assert!(limited.len() <= kb as usize * 1024, "{} bytes exceeds {kb} KB", limited.len());
        assert!(q.is_some_and(|q| q < 100), "the limit lowered the quality: {q:?}");
        let decoded = lcc_codecs::gainmap::read_jpeg(&limited).unwrap();
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
            let meta = Metadata { orientation: Some(lcc_geom::Orientation::from_exif(orientation)), ..Default::default() };
            let exif = lcc_meta::write_exif(&meta);
            let bytes = lcc_codecs::encode_jpeg(
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
        std::fs::write(&path, lcc_codecs::encode_png(&EncodeImage::rgba8(&logo), &EncodeMeta::default()).unwrap()).unwrap();
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
        std::fs::write(&path, lcc_codecs::encode_png(&EncodeImage::rgba8(&logo), &EncodeMeta::default()).unwrap()).unwrap();
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

    /// With no output sharpening and no watermark an export encodes the render itself (no ~100 MB
    /// copy at 24 MP), and the image is left as it is; either one still gets its own copy.
    #[test]
    fn an_unsharpened_unwatermarked_render_is_not_copied() {
        let img = test_image();
        let plain = ExportOptions { sharpen: SharpenFor::None, watermark: None, ..ExportOptions::default() };
        assert!(matches!(finish_8bit(&img, &plain), std::borrow::Cow::Borrowed(_)));
        let mut same = test_image();
        output_sharpen(&mut same, OutputSpace::Srgb, SharpenFor::None, SharpenAmount::High);
        assert_eq!(same.data, img.data);
        let sharpened = ExportOptions { sharpen: SharpenFor::Screen, ..plain };
        assert!(matches!(finish_8bit(&img, &sharpened), std::borrow::Cow::Owned(_)));
    }

    #[test]
    fn sharpen_increases_edge_contrast() {
        let mut img = Rgba8::new(8, 8);
        for (i, p) in img.data.iter_mut().enumerate() {
            let v = if i % 8 < 4 { 80 } else { 170 };
            *p = [v, v, v, 255];
        }
        output_sharpen(&mut img, OutputSpace::Srgb, SharpenFor::Matte, SharpenAmount::High);
        assert!(img.data[3][0] < 80 && img.data[4][0] > 170);
    }

    /// Output sharpening (#2970): each step crisper than the one before, Screen / Standard roughly a
    /// typical web export's, and no wide light or dark rim along a strong edge (overshoot stays
    /// within a tenth of the edge's contrast). Colour is left alone: a grey edge stays grey.
    #[test]
    fn sharpen_steps_are_crisper_without_halos() {
        let edge = || {
            let mut img = Rgba8::new(32, 8);
            for (i, p) in img.data.iter_mut().enumerate() {
                let v = if i % 32 < 16 { 40 } else { 220 };
                *p = [v, v, v, 255];
            }
            img
        };
        let mut last = 0u8;
        for amount in [SharpenAmount::Low, SharpenAmount::Standard, SharpenAmount::High] {
            let mut img = edge();
            output_sharpen(&mut img, OutputSpace::Srgb, SharpenFor::Screen, amount);
            let (dark, light) = (img.data[15], img.data[16]);
            let spread = (40 - dark[0]).max(light[0] - 220);
            assert!(spread >= last && spread > 0, "{amount:?}: {dark:?} {light:?}");
            assert!(dark[0] >= 40 - 18 && light[0] <= 220 + 18, "{amount:?}: {dark:?} {light:?}");
            assert!(dark[0] == dark[1] && dark[1] == dark[2] && light[0] == light[2], "grey stays grey");
            assert_eq!(img.data[0][0], 40, "{amount:?}: flat areas untouched");
            last = spread;
        }
        let mut none = edge();
        output_sharpen(&mut none, OutputSpace::Srgb, SharpenFor::None, SharpenAmount::High);
        assert_eq!(none.data, edge().data);
    }

    /// A float export (scene-linear values) of an edge gets about the same sharpening as an 8-bit
    /// one, measured on the encoded (sRGB) axis: it used to be sharpened in linear light.
    #[test]
    fn float_and_8bit_exports_sharpen_alike() {
        let (dark, light) = (60u8, 200u8);
        let mut img8 = Rgba8::new(32, 8);
        for (i, p) in img8.data.iter_mut().enumerate() {
            let v = if i % 32 < 16 { dark } else { light };
            *p = [v, v, v, 255];
        }
        let lin = |v: u8| lcc_color::transfer::srgb_to_linear(v as f32 / 255.0);
        let samples: Vec<f32> = img8.data.iter().flat_map(|p| [lin(p[0]); 3]).collect();
        let mut deep = DeepImage { width: 32, height: 8, space: OutputSpace::Srgb, samples: DeepSamples::F32(samples) };
        output_sharpen(&mut img8, OutputSpace::Srgb, SharpenFor::Screen, SharpenAmount::High);
        output_sharpen_deep(&mut deep, SharpenFor::Screen, SharpenAmount::High);
        let DeepSamples::F32(out) = &deep.samples else { panic!("expected F32 samples") };
        for x in [15usize, 16] {
            let base = if x < 16 { dark } else { light } as f32;
            let d8 = img8.data[x][0] as f32 - base;
            let df = lcc_color::transfer::linear_to_srgb(out[x * 3]) * 255.0 - base;
            assert!(d8.abs() > 2.0, "the edge is sharpened: {d8}");
            assert!((d8 - df).abs() <= 1.5, "x={x}: 8-bit {d8}, float {df}");
        }
    }

    /// Output sharpening of a float image with NaN and infinite samples does not panic (#2970 review):
    /// they are sharpened as black and every result is finite.
    #[test]
    fn sharpen_deep_survives_non_finite_samples() {
        let (nan, inf) = (f32::NAN, f32::INFINITY);
        let mut samples = vec![0.5f32; 27];
        samples[..9].copy_from_slice(&[nan, nan, nan, inf, inf, inf, -inf, nan, inf]);
        samples[12] = nan;
        samples[20] = inf;
        for target in [SharpenFor::Screen, SharpenFor::Glossy, SharpenFor::Matte] {
            let mut img = DeepImage { width: 3, height: 3, space: OutputSpace::Srgb, samples: DeepSamples::F32(samples.clone()) };
            output_sharpen_deep(&mut img, target, SharpenAmount::High);
            let DeepSamples::F32(out) = &img.samples else { panic!("expected F32 samples") };
            assert!(out.iter().all(|v| v.is_finite() && *v >= 0.0), "{target:?}: {out:?}");
        }
        let mut all_nan = DeepImage { width: 3, height: 3, space: OutputSpace::Srgb, samples: DeepSamples::F32(vec![nan; 27]) };
        output_sharpen_deep(&mut all_nan, SharpenFor::Screen, SharpenAmount::Standard);
    }

    /// Exported files carry keywords as Lightroom Classic writes them: names flat in `dc:subject`
    /// (with the keywords containing them), full paths in `lr:hierarchicalSubject`, and nothing of
    /// a keyword left out of export. They used to carry the `a|b` paths in `dc:subject`.
    #[test]
    fn exported_keywords_follow_their_options() {
        use lcc_catalog::{Catalog, Op, Photo, PhotoId, Source, keywords::KeywordInfo};
        let mut c = Catalog::new();
        c.apply(Op::SetKeyword { path: "Places".into(), info: Some(KeywordInfo { include_on_export: false, ..KeywordInfo::default() }) }).unwrap();
        c.apply(Op::SetKeyword { path: "draft".into(), info: Some(KeywordInfo { include_on_export: false, ..KeywordInfo::default() }) }).unwrap();
        let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.jpg", "jpeg", 10, 10, "2026-09-30T00:00:00");
        p.meta.keywords = vec!["Places|Lisbon".into(), "travel|Italy".into(), "draft".into()];
        let m = export_metadata(&p, &c, &ExportOptions::default()).unwrap();
        assert_eq!(m.keywords, ["Lisbon", "Italy", "travel"]);
        assert_eq!(m.hierarchical_keywords, ["Places|Lisbon", "travel|Italy"]);
        let jpg = encode_with_metadata(&test_image(), &ExportOptions::default(), Some(&m)).unwrap();
        let back = lcc_meta::extract(&jpg);
        assert_eq!((back.keywords, back.hierarchical_keywords), (m.keywords.clone(), m.hierarchical_keywords.clone()));
    }

    /// A year or a month alone (`1983-04`, a scanned print's date) is exported in the XMP's
    /// `photoshop:DateCreated` as stated, and in neither Exif date nor `exif:DateTimeOriginal`, whose
    /// form can't say it; a full time still goes in all of them.
    #[test]
    fn a_partial_capture_time_is_exported_as_stated() {
        use lcc_catalog::{Catalog, Photo, PhotoId, Source};
        let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.jpg", "jpeg", 10, 10, "2026-09-30T00:00:00");
        for (captured, full) in [("1983", false), ("1983-04", false), ("2024-01-16T12:34:56", true)] {
            p.captured = Some(captured.into());
            let m = export_metadata(&p, &Catalog::new(), &ExportOptions::default()).unwrap();
            let xmp = lcc_meta::write_xmp(&m, None);
            let props = lcc_meta::parse_xmp(&xmp).unwrap().properties;
            assert_eq!(props.get("photoshop:DateCreated"), Some(&vec![captured.to_string()]), "{xmp}");
            assert_eq!(props.contains_key("exif:DateTimeOriginal"), full, "{xmp}");
            let exif = lcc_meta::read_exif(&lcc_meta::write_exif(&m));
            assert_eq!(exif.capture_time.is_some(), full, "{captured}: Exif dates");
            let jpg = encode_with_metadata(&test_image(), &ExportOptions::default(), Some(&m)).unwrap();
            assert_eq!(lcc_meta::extract(&jpg).capture_time.map(|d| d.to_iso()).as_deref(), Some(captured), "the exported file reads back as stated");
        }
    }

    #[test]
    fn metadata_policies() {
        use lcc_catalog::{Catalog, Photo, PhotoId, Source};
        let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.jpg", "jpeg", 10, 10, "2026-09-30T00:00:00");
        p.meta.camera = "Synthetic X2".into();
        p.meta.copyright = "(c) Me".into();
        p.meta.copyright_status = lcc_catalog::CopyrightStatus::Copyrighted;
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
        let back = lcc_meta::extract(&jpg);
        assert_eq!(back.model.as_deref(), Some("Synthetic X2"));
        assert_eq!(back.copyright.as_deref(), Some("(c) Me"));
        assert_eq!((back.copyright_marked, back.usage_terms.as_deref()), (Some(true), Some("Editorial use only")));
        assert_eq!(back.copyright_url.as_deref(), Some("https://example.com/rights"));
        assert!(back.gps.is_some());
    }

    /// Exports write EXIF Make and Model as read, not the label (#1618); a catalog from before the
    /// parts were kept separately falls back to the label for the Model.
    #[test]
    fn export_writes_camera_make_and_model() {
        use lcc_catalog::{Catalog, Photo, PhotoId, Source};
        let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.jpg", "jpeg", 10, 10, "2026-10-06T00:00:00");
        p.meta.camera = "Canon IXUS 185".into();
        p.meta.camera_make = "Canon".into();
        p.meta.camera_model = "Canon IXUS 185".into();
        let m = export_metadata(&p, &Catalog::new(), &ExportOptions::default()).unwrap();
        assert_eq!((m.make.as_deref(), m.model.as_deref()), (Some("Canon"), Some("Canon IXUS 185")));
        // embedded and readable back from the JPEG
        let jpg = encode_with_metadata(&test_image(), &ExportOptions::default(), Some(&m)).unwrap();
        let back = lcc_meta::extract(&jpg);
        assert_eq!((back.make.as_deref(), back.model.as_deref()), (Some("Canon"), Some("Canon IXUS 185")));
        // a catalog from before the parts were kept: the label stands in for the Model
        let mut old = Photo::new(PhotoId(2), Source::Demo { scene: 0 }, "b.jpg", "jpeg", 10, 10, "2026-10-06T00:00:00");
        old.meta.camera = "Canon Canon IXUS 185".into();
        let m = export_metadata(&old, &Catalog::new(), &ExportOptions::default()).unwrap();
        assert_eq!((m.make.as_deref(), m.model.as_deref()), (None, Some("Canon Canon IXUS 185")));
    }

    /// The lens maker (Exif LensMake, upstream #3828) is exported with metadata "All" and reads back from the JPEG.
    #[test]
    fn export_writes_lens_make() {
        use lcc_catalog::{Catalog, Photo, PhotoId, Source};
        let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.jpg", "jpeg", 10, 10, "2026-10-06T00:00:00");
        p.meta.lens = "RF 24-105mm f/4L IS USM".into();
        p.meta.lens_make = "Canon".into();
        let m = export_metadata(&p, &Catalog::new(), &ExportOptions::default()).unwrap();
        assert_eq!(m.lens_make.as_deref(), Some("Canon"));
        let jpg = encode_with_metadata(&test_image(), &ExportOptions::default(), Some(&m)).unwrap();
        assert_eq!(lcc_meta::extract(&jpg).lens_make.as_deref(), Some("Canon"));
        p.meta.lens_make.clear();
        let m = export_metadata(&p, &Catalog::new(), &ExportOptions::default()).unwrap();
        assert_eq!(m.lens_make, None);
    }

    #[test]
    fn watermark_draws_in_the_anchored_corner_only() {
        let mut img = Rgba8::new(400, 300);
        for p in img.data.iter_mut() {
            *p = [0, 0, 0, 255];
        }
        let o = ExportOptions::from_json(&serde_json::json!({"watermark": {"text": "The app", "size": 0.08, "opacity": 1.0, "shadow": false}}));
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
        let strict = ExportOptions::from_params(&json!({"watermark": {"text": "LC", "inset": 0.1}})).unwrap();
        assert_eq!(strict.watermark.map(|w| (w.inset_x, w.inset_y)), Some((0.1, 0.1)));
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
        let base = Watermark { text: "Watermark".into(), size: 0.08, opacity: 1.0, shadow: false, inset_x: 0.0, inset_y: 0.0, ..Default::default() };
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
        let wm = Watermark { text: "Watermark".into(), size: 0.01, opacity: 1.0, shadow: false, ..Default::default() };
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
        for wm in [vertical_japanese(), Watermark { text: "The app 日本語".into(), ..Watermark::default() }] {
            let mut covered = 0usize;
            watermark_coverage(400, 300, &wm, &[], 1.0, |_, _, k, _| covered += usize::from(k > 0.0));
            assert!(covered > 0, "{:?} draws something", wm.text);
        }
    }

    #[test]
    fn biz_ud_vertical_watermarks_place_punctuation_at_the_top_right() {
        use ab_glyph::Font;
        let Some(entry) = crate::fonts::craft_fonts().iter().find(|f| f.family == "BIZ UDMincho") else {
            lcc_testkit::skip!(Fonts, "built without BIZ UDMincho from craft-fonts");
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
        if crate::fonts::craft_fonts().iter().any(|f| f.family == "BIZ UDMincho") {
            for (composed, decomposed) in [("が日", "か\u{3099}日"), ("ぱ\n日", "は\u{309a}\n日")] {
                compare(composed, decomposed, crate::fonts::craft_fonts());
            }
        } else {
            lcc_testkit::skip!(Fonts, "Japanese comparisons: built without BIZ UDMincho from craft-fonts");
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
        let fonts = watermark_fonts(crate::fonts::craft_fonts());
        if fonts.len() < 2 {
            lcc_testkit::skip!(Fonts, "built without CRAFT_FONTS_DIR, so there is no Japanese watermark face");
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
        let png = lcc_codecs::encode_png(&EncodeImage::rgba8(&logo), &EncodeMeta::default()).unwrap();
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

    /// Titles become file names Windows can create: no forbidden or control characters, no trailing
    /// dot, no reserved device name (issue #2140).
    #[test]
    fn export_names_are_valid_on_windows() {
        let mut p = named("IMG_1.jpg");
        let o = ExportOptions { naming: "{title}".into(), ..Default::default() };
        for (title, want) in [
            ("What?", "What_.jpg"),
            ("\"Best\" <of> a|b*c", "_Best_ _of_ a_b_c.jpg"),
            ("tab\there", "tab_here.jpg"),
            ("ends with dots...", "ends with dots.jpg"),
            ("CON", "CON_.jpg"),
            ("nul.tar", "nul_.tar.jpg"),
            ("com7", "com7_.jpg"),
            ("COM0", "COM0.jpg"),
            ("Console", "Console.jpg"),
        ] {
            p.meta.title = title.into();
            assert_eq!(o.file_name_for(&p, 1), want, "{title:?}");
        }
    }

    fn named(file_name: &str) -> lcc_catalog::Photo {
        use lcc_catalog::{Photo, PhotoId, Source};
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
            std::sync::Arc::make_mut(&mut p.develop).crop.geometry.rect = lcc_geom::Rect::new(0.0, 0.0, x1, 1.0);
            let (bw, bh) = output_size(&p, &ExportOptions { resize: Some(r), ..Default::default() });
            lcc_pipeline::output_size(w as usize, h as usize, &p.develop, &lcc_pipeline::RenderRequest::fit(bw, bh))
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
        use lcc_color::{DISPLAY_P3, REC2020};
        let c = DISPLAY_P3.to_space(&REC2020).apply_f32([0.04, 0.45, 0.04]);
        let src = lcc_raster::Rgb32f::filled(16, 16, c);
        let req = lcc_pipeline::RenderRequest { space, ..lcc_pipeline::RenderRequest::fit(16, 16) };
        lcc_pipeline::render(&src, &Default::default(), &Default::default(), &req).image
    }

    /// Decode `bytes` and return the centre pixel in linear sRGB primaries (unclamped), plus the
    /// recognised space of the embedded profile.
    fn decoded_in_srgb(bytes: &[u8]) -> ([f32; 3], Option<NamedSpace>) {
        let d = lcc_codecs::decode(bytes, Default::default()).expect("decodes");
        let m = d.space.to_space(&lcc_color::SRGB);
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
            let src = lcc_raster::Rgb32f::filled(16, 16, [0.18; 3]);
            let base = lcc_pipeline::RenderRequest::fit(16, 16);
            move |space| lcc_pipeline::render(&src, &Default::default(), &Default::default(), &lcc_pipeline::RenderRequest { space, ..base }).image
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
    fn ramp(depth: OutputDepth, space: OutputSpace) -> lcc_pipeline::Rendered {
        let src = lcc_raster::Rgb32f::from_fn(1024, 4, |x, _| [0.10 + 0.03 * x as f32 / 1023.0; 3]);
        let req = lcc_pipeline::RenderRequest { depth, space, ..lcc_pipeline::RenderRequest::fit(1024, 4) };
        lcc_pipeline::render(&src, &Default::default(), &Default::default(), &req)
    }

    fn distinct_levels(bytes: &[u8]) -> (usize, lcc_codecs::Decoded) {
        let d = lcc_codecs::decode(bytes, Default::default()).expect("decodes");
        let mut v: Vec<u32> = (0..d.image.width).map(|x| d.image.get(x, 1)[1].to_bits()).collect();
        // sorted first: 8-bit output is dithered (process V8), so equal levels aren't all side by side
        v.sort_unstable();
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
            assert_eq!(lcc_codecs::sniff(&bytes), Some(lcc_codecs::Format::Jxl));
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
        assert!(err(json!({"ids": [1, "x"]})).contains("`ids[1]` is \"x\""));
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
            ("chooseLater", DestinationMode::ChooseLater),
        ] {
            assert_eq!(ExportOptions::from_params(&json!({"destinationMode": v})).unwrap().destination_mode, want, "{v}");
        }
        let e = ExportOptions::from_params(&json!({"destinationMode": "nextToIt"})).unwrap_err().to_string();
        assert!(e.contains("`destinationMode` must be one of specificFolder|sameAsOriginal|chooseLater"), "{e}");
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
            ("copyrightContact", MetadataPolicy::CopyrightContact),
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
            assert!(e.contains("`metadata` must be one of all|allExceptCamera|copyright|copyrightContact|none"), "{v:?}: {e}");
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
        let rotated = ExportOptions::from_params(&json!({"watermark": {"text": "x", "rotation": 270}})).unwrap();
        assert_eq!(rotated.watermark.map(|w| w.rotation), Some(270));
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
        ids: &[lcc_catalog::PhotoId],
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
        let files = run_batch_with(items, &mut Placer::new(o, &to, &mut write, &|_| false, stop_on_error), &mut progress, lanes, None);
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
        if !lcc_gpu::available() {
            lcc_testkit::skip!(Gpu, "no GPU adapter ({})", lcc_gpu::unavailable_reason().unwrap_or_default());
            return;
        }
        let mut s = crate::Session::with_demo();
        let ids: Vec<_> = s.visible().iter().copied().take(2).collect();
        let o = ExportOptions::from_json(&json!({"format": "png", "width": 64}));
        // one lane: both photos render on this thread, and the fault hits the first render here
        lcc_gpu::inject_fault(lcc_gpu::Fault::Limit(1 << 10));
        let (files, _, _) = lanes_batch(&mut s, &ids, &o, 1, true, usize::MAX);
        let files = files.unwrap();
        assert_eq!(files.len(), 2, "{files:?}");
        let why: Vec<_> = files.iter().filter_map(|f| f.get("gpuFallback").and_then(serde_json::Value::as_str)).collect();
        assert_eq!(why.len(), 1, "exactly one file fell back: {files:?}");
        assert!(why[0].contains("limit"), "{files:?}");
        assert!(files[1].get("gpuFallback").is_none(), "the second render was fine: {files:?}");
        assert!(lcc_gpu::available(), "a limit is no device failure");
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
        let raw = lcc_raw::decode(&dng.bytes).unwrap();
        assert_eq!((raw.width, raw.height, raw.cpp), (dng.width, dng.height, 3));
        assert!(raw.color.profile.tone_curve.is_some(), "identity tone curve keeps converters from adding one");
        let back = raw.develop(lcc_raw::Method::Bilinear).unwrap();
        let reference = lcc_codecs::decode_thumbnail(&png.bytes, 4096).unwrap().image;
        assert_eq!((reference.width, reference.height), (back.width, back.height));
        // mean brightness: the PNG holds 8-bit sRGB, the DNG linear sRGB
        let mean_dng: f32 = back.data.iter().map(|p| lcc_color::transfer::linear_to_srgb(p[1].min(1.0))).sum::<f32>() / back.data.len() as f32;
        let mean_tif: f32 = reference.data.iter().map(|p| p[1] as f32 / 255.0).sum::<f32>() / reference.data.len() as f32;
        assert!((mean_dng - mean_tif).abs() < 0.02, "{mean_dng} vs {mean_tif}");
    }

    // one at a time, and a cancel stops at the same photo
    #[test]
    fn side_by_side_batches_match_one_at_a_time() {
        let mut s = crate::Session::with_demo();
        // the newest demo photos come in different shapes
        s.execute("library.sort", &json!({"ascending": false})).unwrap();
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
                run_batch_with(items, &mut Placer::new(&o, &to, &mut write, &|_| false, true), &mut progress, 2, None)
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
        use lcc_catalog::{Op, Photo, PhotoId, Source};
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
