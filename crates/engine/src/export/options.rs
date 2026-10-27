use super::*;

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
    pub(super) fn codec(self) -> lightcraft_codecs::HdrTransfer {
        match self {
            Self::Pq => lightcraft_codecs::HdrTransfer::Pq,
            Self::Hlg => lightcraft_codecs::HdrTransfer::Hlg,
        }
    }
}
