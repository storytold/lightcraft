use super::options::{Conflict, DestinationMode, ExportFormat, HdrCurve, MetadataPolicy, Resize, ResizeMode, SharpenAmount, SharpenFor};
use super::prepare::DngCompression;
use super::watermark::{Anchor, Watermark};
use super::{ExportOptions, JxlColour, JxlEffort, JxlOptions, OutputDepth, OutputSpace, TiffCompression};

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
    pub(super) fn jxl(&self, space: OutputSpace, hdr: bool) -> JxlOptions {
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
