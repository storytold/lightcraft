//! Interchange: camera-raw-settings (`crs:`) develop fields from XMP → our develop settings.
//!
//! Other raw developers store their edits as `crs:` properties in XMP sidecars, in DNG-embedded
//! XMP and in XMP preset files. This module maps the commonly used, documented-by-observation
//! fields to a *partial* [`DevelopSettings`](lightcraft_develop::DevelopSettings) JSON object
//! (only the fields present in the packet), which is then merged like a preset. Our pipeline is
//! not theirs, so the result is a best-effort approximation of the look, not a pixel match.
//! The full table is in `docs/xmp-interop.md`.
//!
//! Implemented from the public XMP specification (ISO 16684-1) and black-box observation of what
//! each field means; no third-party code or preset content was used.

use std::collections::BTreeMap;

use lightcraft_develop::{MIXER_BANDS, Preset, Upright};
use lightcraft_geom::{Affine, Homography, Orientation, Point, Rect};
use serde_json::{Map, Value, json};

/// Properties keyed `prefix:name` (as produced by [`lightcraft_meta::parse_xmp`]).
pub type Props = BTreeMap<String, Vec<String>>;

/// What the mapped settings will develop. It decides how a `crs:` white balance is read: a raw
/// with a Kelvin scale (colour matrices in the file, or measured for its camera) takes `Temperature`
/// as Kelvin; everything developed relative to its as-shot look (rendered files, and raws without
/// matrices anywhere, [`lightcraft_catalog::relative_wb_camera`]) needs a shift from the other app's
/// as-shot white instead, because on that scale 6500 K / 0 means *as shot* (issue #510).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// A preset: raw or rendered unknown. Absolute Kelvin first, else the relative scale.
    Any,
    /// A rendered photo (or a raw shown from its embedded preview): `Incremental*` fields on our
    /// relative scale.
    Rendered,
    /// A raw developed with an absolute white balance (DNG and other files with matrices, and raws
    /// whose camera has measured matrices): Kelvin as written.
    RawAbsolute,
    /// A raw developed relative to its as-shot look ([`lightcraft_catalog::Photo::relative_wb`]).
    RawRelative,
}

impl Target {
    /// The target for a file of this media kind and format (its extension or Lightroom's
    /// `fileFormat` name) shot on `camera` (Exif make and model as the catalog keeps them, or
    /// empty when unknown); `preview_only` for a raw that can't be decoded yet.
    pub fn for_file(kind: lightcraft_catalog::MediaKind, format: &str, preview_only: bool, camera: &str) -> Target {
        if kind != lightcraft_catalog::MediaKind::Raw || preview_only {
            Target::Rendered
        } else if lightcraft_catalog::relative_wb_camera(format, camera) {
            Target::RawRelative
        } else {
            Target::RawAbsolute
        }
    }
    /// The target for a photo in the catalog.
    pub fn for_photo(p: &lightcraft_catalog::Photo) -> Target {
        Target::for_file(p.kind, &p.format, p.preview_only.is_some(), &p.meta.camera)
    }
}

/// Band names as they appear in `crs:` field names, in our mixer order.
const CRS_BANDS: [&str; 8] = ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"];

/// `crs:` fields that describe the packet rather than an adjustment.
const NON_ADJUSTMENT: &[&str] = &[
    "crs:Version",
    "crs:ProcessVersion",
    "crs:Name",
    "crs:ShortName",
    "crs:Group",
    "crs:UUID",
    "crs:PresetType",
    "crs:SupportsAmount",
    "crs:SupportsColor",
    "crs:SupportsMonochrome",
    "crs:SupportsHighDynamicRange",
    "crs:SupportsNormalDynamicRange",
    "crs:SupportsSceneReferred",
    "crs:SupportsOutputReferred",
    "crs:CameraModelRestriction",
    "crs:Copyright",
    "crs:ContactInfo",
    "crs:HasSettings",
    "crs:AlreadyApplied",
    "crs:RawFileName",
    "crs:HasCrop",
    // preset/library bookkeeping and tool options that don't change the rendering
    "crs:Cluster",
    "crs:SupportsAmount2",
    "crs:RequiresRGBTables",
    "crs:SortName",
    "crs:Description",
    "crs:CropConstrainToWarp",
    "crs:OverrideLookVignette",
    // the file's as-shot white, recorded next to the edit (ours comes from the file itself)
    "crs:AsShotTemperature",
    "crs:AsShotTint",
    // which Lightroom version may read the edit, and checksums of the profiles it names
    "crs:CompatibleVersion",
    "crs:CameraProfileDigest",
    "crs:LensProfileDigest",
    // the lens profile Lightroom picked: ours is the file's own, only `LensProfileEnable` carries over
    "crs:LensProfileName",
    "crs:LensProfileFilename",
    "crs:LensProfileIsEmbedded",
    "crs:LensProfileSetup",
    // a crop tool option (keep the crop inside the photo), not the crop itself
    "crs:CropConstrainToUnitSquare",
];

/// True if the packet carries any `crs:` adjustment (not just bookkeeping fields).
pub fn has_adjustments(props: &Props) -> bool {
    if props.get("crs:AlreadyApplied").and_then(|v| v.first()).is_some_and(|s| s.eq_ignore_ascii_case("true")) {
        // The pixels already contain these settings (e.g. an exported/rendered file).
        return false;
    }
    props.keys().any(|k| k.starts_with("crs:") && !NON_ADJUSTMENT.contains(&k.split('/').next().unwrap_or(k)))
}

thread_local! {
    /// Keys read by [`to_partial`] while [`to_partial_report`] runs.
    static READ: std::cell::RefCell<Option<std::collections::BTreeSet<String>>> = const { std::cell::RefCell::new(None) };
    /// Adjustments [`to_partial`] read but deliberately left out, for the report.
    static SKIPPED: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}

fn note(k: &str) {
    READ.with(|r| {
        if let Some(s) = r.borrow_mut().as_mut() {
            s.insert(k.to_string());
        }
    });
}

fn skipped(what: &str) {
    SKIPPED.with(|r| {
        if let Some(s) = r.borrow_mut().as_mut() {
            s.push(what.to_string());
        }
    });
}

fn first<'a>(props: &'a Props, k: &str) -> Option<&'a str> {
    note(k);
    props.get(k).and_then(|v| v.first()).map(|s| s.trim()).filter(|s| !s.is_empty())
}

fn num(props: &Props, k: &str) -> Option<f64> {
    let s = first(props, k)?;
    let s = s.strip_prefix('+').unwrap_or(s);
    if let Some((n, d)) = s.split_once('/') {
        let (n, d) = (n.trim().parse::<f64>().ok()?, d.trim().parse::<f64>().ok()?);
        return (d != 0.0).then_some(n / d).filter(|v| v.is_finite());
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn boolean(props: &Props, k: &str) -> Option<bool> {
    first(props, k).map(|s| s.eq_ignore_ascii_case("true") || s == "1")
}

/// Insert `v` at the dotted `path` of a JSON object, creating intermediate objects.
fn put(out: &mut Value, path: &str, v: Value) {
    let mut cur = out;
    let mut parts = path.split('.').peekable();
    while let Some(p) = parts.next() {
        let Value::Object(m) = cur else { return };
        if parts.peek().is_none() {
            m.insert(p.to_string(), v);
            return;
        }
        cur = m.entry(p.to_string()).or_insert_with(|| Value::Object(Map::new()));
    }
}

/// Relative temperature (−100..100, used for rendered files) → Kelvin, matching the UI's scale
/// (a mired shift around 6500 K).
pub fn rel_to_kelvin(r: f64) -> f64 {
    1e6 / (1e6 / 6500.0 - r.clamp(-100.0, 100.0) * 0.8)
}

/// Parse a point curve stored as an `rdf:Seq` of `"x, y"` strings in 0..255.
fn curve(props: &Props, k: &str) -> Option<Value> {
    note(k);
    let items = props.get(k)?;
    let mut pts = Vec::new();
    for it in items {
        let (x, y) = it.split_once(',')?;
        // (`parse` reads "NaN" and "inf" too: a curve with such a point is not a curve)
        let (x, y) = (x.trim().parse::<f64>().ok().filter(|v| v.is_finite())?, y.trim().parse::<f64>().ok().filter(|v| v.is_finite())?);
        pts.push(json!({"x": (x / 255.0).clamp(0.0, 1.0), "y": (y / 255.0).clamp(0.0, 1.0)}));
    }
    if pts.len() < 2 {
        return None;
    }
    // A straight 0,0 → 255,255 line is the identity: store it as "no curve".
    let identity = pts.len() == 2 && items.iter().all(|it| it.split(',').map(|v| v.trim()).collect::<Vec<_>>().windows(2).all(|w| w[0] == w[1]));
    Some(if identity { json!([]) } else { Value::Array(pts) })
}

/// Map the `crs:` fields of an XMP packet to a partial develop-settings JSON object.
///
/// `target` decides how the white balance is read (see [`Target`]).
pub fn to_partial(props: &Props, target: Target) -> Value {
    let mut out = Value::Object(Map::new());
    let o = &mut out;
    let n = |o: &mut Value, crs: &str, path: &str| {
        if let Some(v) = num(props, &format!("crs:{crs}")) {
            put(o, path, json!(v));
        }
    };
    // ---- Light (process version 2012+ names)
    n(o, "Exposure2012", "light.exposure");
    n(o, "Contrast2012", "light.contrast");
    n(o, "Highlights2012", "light.highlights");
    n(o, "Shadows2012", "light.shadows");
    n(o, "Whites2012", "light.whites");
    n(o, "Blacks2012", "light.blacks");
    // Lightroom's Light panel "HDR" button
    if let Some(on) = boolean(props, "crs:HDREditMode") {
        put(o, "hdr.enabled", json!(on));
    }
    // older process versions (most `.lrtemplate` presets): approximate the 2012 sliders from the
    // earlier ones, relative to their defaults (contrast 25, blacks 5, brightness 50)
    let has = |o: &Value, path: &str| path.split('.').try_fold(o, |v, k| v.get(k)).is_some();
    // presets written by newer versions keep the old fields at their defaults: ignore them there
    let pv2012 = props.keys().any(|k| k.ends_with("2012"));
    let old = |o: &mut Value, crs: &str, path: &str, f: &dyn Fn(f64) -> f64| {
        let v = num(props, &format!("crs:{crs}"));
        if !pv2012
            && !has(o, path)
            && let Some(v) = v
        {
            put(o, path, json!(f(v).clamp(-100.0, 100.0)));
        }
    };
    old(o, "Exposure", "light.exposure", &|v| v);
    old(o, "Contrast", "light.contrast", &|v| v - 25.0);
    old(o, "HighlightRecovery", "light.highlights", &|v| -v);
    old(o, "FillLight", "light.shadows", &|v| v);
    old(o, "Shadows", "light.blacks", &|v| -(v - 5.0) * 4.0);
    old(o, "Brightness", "light.whites", &|v| (v - 50.0) * 0.6);
    // ---- Presence
    n(o, "Texture", "effects.texture");
    n(o, "Clarity2012", "effects.clarity");
    old(o, "Clarity", "effects.clarity", &|v| v);
    n(o, "Dehaze", "effects.dehaze");
    n(o, "Vibrance", "color.vibrance");
    n(o, "Saturation", "color.saturation");

    // ---- White balance
    let mode = first(props, "crs:WhiteBalance").map(|m| match m.to_ascii_lowercase().replace(' ', "").as_str() {
        "asshot" => "asShot",
        "auto" => "auto",
        "daylight" => "daylight",
        "cloudy" => "cloudy",
        "shade" => "shade",
        "tungsten" => "tungsten",
        "fluorescent" => "fluorescent",
        "flash" => "flash",
        _ => "custom",
    });
    let abs = num(props, "crs:Temperature").map(|t| (t, num(props, "crs:Tint")));
    let rel = num(props, "crs:IncrementalTemperature")
        .or(num(props, "crs:IncrementalTint").map(|_| 0.0))
        .map(|t| (rel_to_kelvin(t), num(props, "crs:IncrementalTint")));
    // A Kelvin value for something we develop relative to its as-shot look: the other app's
    // as-shot white (`AsShotTemperature` / `AsShotTint`, written next to the edit) is the
    // reference, so the edit is the same mired shift from ours (issue #510).
    let shifted = || {
        let (t, tint) = abs?;
        let as_shot = num(props, "crs:AsShotTemperature").filter(|k| *k > 0.0)?;
        if t <= 0.0 {
            return None;
        }
        let r = (1e6 / as_shot - 1e6 / t) / 0.8;
        let tint = tint.map(|v| v - num(props, "crs:AsShotTint").unwrap_or(0.0));
        Some((rel_to_kelvin(r), tint))
    };
    let wb = match target {
        Target::RawAbsolute => abs,
        Target::Any => abs.or(rel),
        Target::Rendered => rel.or_else(shifted).or(abs),
        Target::RawRelative => rel.or_else(shifted),
    };
    match (mode, wb) {
        (Some(m), _) if m != "custom" => put(o, "wb.mode", json!(m)),
        (m, Some((t, tint))) => {
            put(o, "wb.mode", json!(m.unwrap_or("custom")));
            put(o, "wb.temp", json!(t));
            if let Some(tint) = tint {
                put(o, "wb.tint", json!(tint));
            }
            // Kelvin as the other app wrote it: say so, so the value stays Kelvin on a raw with that scale (a
            // preset's Kelvin is read as the relative scale on the files that have only that, as before)
            if wb == abs && matches!(target, Target::RawAbsolute | Target::Any) {
                put(o, "wb.scale", json!("kelvin"));
            }
        }
        // a custom Kelvin with nothing to shift it from: read on the relative scale it would be a
        // large colour shift (ΔE 22 against Lightroom's export on the photos of issue #510), so
        // the photo stays As Shot (ΔE 12 there) and the report says so
        (_, None) if target == Target::RawRelative && abs.is_some() => {
            put(o, "wb.mode", json!("asShot"));
            skipped("Temperature, Tint (custom white balance without AsShotTemperature: kept As Shot)");
        }
        (Some(m), None) => put(o, "wb.mode", json!(m)),
        (None, None) => {}
    }

    // ---- Treatment + B&W mix
    if let Some(bw) = boolean(props, "crs:ConvertToGrayscale") {
        put(o, "treatment", json!(if bw { "bw" } else { "color" }));
    }
    for (band, crs) in MIXER_BANDS.iter().zip(CRS_BANDS) {
        n(o, &format!("HueAdjustment{crs}"), &format!("mixer.{band}.hue"));
        n(o, &format!("SaturationAdjustment{crs}"), &format!("mixer.{band}.sat"));
        n(o, &format!("LuminanceAdjustment{crs}"), &format!("mixer.{band}.lum"));
        n(o, &format!("GrayMixer{crs}"), &format!("bw_mix.{band}"));
    }

    // ---- Tone curve: parametric regions + point curves
    n(o, "ParametricShadows", "curve.shadows");
    n(o, "ParametricDarks", "curve.darks");
    n(o, "ParametricLights", "curve.lights");
    n(o, "ParametricHighlights", "curve.highlights");
    n(o, "ParametricShadowSplit", "curve.split_shadows");
    n(o, "ParametricMidtoneSplit", "curve.split_mid");
    n(o, "ParametricHighlightSplit", "curve.split_highlights");
    n(o, "CurveRefineSaturation", "curve.refine_saturation");
    if let Some(c) = curve(props, "crs:ToneCurve").filter(|c| c.as_array().is_some_and(|a| !a.is_empty())) {
        // the older single curve, used when no 2012 curve is present
        if !props.contains_key("crs:ToneCurvePV2012") {
            put(o, "curve.master", c);
        }
    }
    // Lightroom Classic reads the red / green / blue curves only when the packet holds the master
    // 2012 curve and all three channel curves (as its own packets always do): a packet missing any
    // of them imports without channel curves (master + red, master + red + green, or red / green /
    // blue without the master all leave the photo's channels unchanged there)
    if let Some(c) = curve(props, "crs:ToneCurvePV2012") {
        put(o, "curve.master", c);
        let channels = [("ToneCurvePV2012Red", "red"), ("ToneCurvePV2012Green", "green"), ("ToneCurvePV2012Blue", "blue")]
            .map(|(crs, ch)| (curve(props, &format!("crs:{crs}")), ch));
        if channels.iter().all(|(c, _)| c.is_some()) {
            for (c, ch) in channels {
                if let Some(c) = c {
                    put(o, &format!("curve.{ch}"), c);
                }
            }
        }
    }

    // ---- Color grading (split toning fields are shared with the older split-toning panel)
    n(o, "SplitToningShadowHue", "grading.shadows.hue");
    n(o, "SplitToningShadowSaturation", "grading.shadows.sat");
    n(o, "ColorGradeShadowLum", "grading.shadows.lum");
    n(o, "SplitToningHighlightHue", "grading.highlights.hue");
    n(o, "SplitToningHighlightSaturation", "grading.highlights.sat");
    n(o, "ColorGradeHighlightLum", "grading.highlights.lum");
    n(o, "ColorGradeMidtoneHue", "grading.midtones.hue");
    n(o, "ColorGradeMidtoneSat", "grading.midtones.sat");
    n(o, "ColorGradeMidtoneLum", "grading.midtones.lum");
    n(o, "ColorGradeGlobalHue", "grading.global.hue");
    n(o, "ColorGradeGlobalSat", "grading.global.sat");
    n(o, "ColorGradeGlobalLum", "grading.global.lum");
    n(o, "ColorGradeBlending", "grading.blending");
    n(o, "SplitToningBalance", "grading.balance");

    // ---- Detail
    n(o, "Sharpness", "detail.sharpen_amount");
    n(o, "SharpenRadius", "detail.sharpen_radius");
    n(o, "SharpenDetail", "detail.sharpen_detail");
    n(o, "SharpenEdgeMasking", "detail.sharpen_masking");
    n(o, "LuminanceSmoothing", "detail.nr_luminance");
    n(o, "LuminanceNoiseReductionDetail", "detail.nr_detail");
    n(o, "LuminanceNoiseReductionContrast", "detail.nr_contrast");
    n(o, "ColorNoiseReduction", "detail.nr_color");
    n(o, "ColorNoiseReductionDetail", "detail.nr_color_detail");
    n(o, "ColorNoiseReductionSmoothness", "detail.nr_color_smoothness");

    // ---- Effects: post-crop vignette + grain
    n(o, "PostCropVignetteAmount", "vignette.amount");
    n(o, "PostCropVignetteMidpoint", "vignette.midpoint");
    n(o, "PostCropVignetteRoundness", "vignette.roundness");
    n(o, "PostCropVignetteFeather", "vignette.feather");
    n(o, "PostCropVignetteHighlightContrast", "vignette.highlights");
    if let Some(st) = num(props, "crs:PostCropVignetteStyle") {
        let style = match st as i64 {
            2 => "colorPriority",
            3 => "paintOverlay",
            _ => "highlightPriority",
        };
        put(o, "vignette.style", json!(style));
    }
    n(o, "GrainAmount", "grain.amount");
    n(o, "GrainSize", "grain.size");
    n(o, "GrainFrequency", "grain.roughness");

    // ---- Optics (manual corrections; lens profiles are ours, only the switch carries over).
    // Our profile corrections are the file's own (DNG-embedded) ones, which Lightroom applies
    // whatever its "Enable Profile Corrections" box says [inferred]: so only an enabling switch
    // carries over, and `LensProfileEnable=0` (the default in most presets) leaves ours as it is.
    if boolean(props, "crs:LensProfileEnable") == Some(true) {
        put(o, "optics.lens_profile", json!(true));
    }
    if let Some(b) = boolean(props, "crs:AutoLateralCA") {
        put(o, "optics.remove_ca", json!(b));
    }
    n(o, "LensManualDistortionAmount", "optics.distortion");
    n(o, "VignetteAmount", "optics.vignetting");
    n(o, "VignetteMidpoint", "optics.vignetting_midpoint");
    n(o, "DefringePurpleAmount", "optics.defringe_purple_amount");
    n(o, "DefringePurpleHueLo", "optics.defringe_purple_hue_lo");
    n(o, "DefringePurpleHueHi", "optics.defringe_purple_hue_hi");
    n(o, "DefringeGreenAmount", "optics.defringe_green_amount");
    n(o, "DefringeGreenHueLo", "optics.defringe_green_hue_lo");
    n(o, "DefringeGreenHueHi", "optics.defringe_green_hue_hi");

    // ---- Calibration
    n(o, "ShadowTint", "calibration.shadows_tint");
    n(o, "RedHue", "calibration.red_hue");
    n(o, "RedSaturation", "calibration.red_sat");
    n(o, "GreenHue", "calibration.green_hue");
    n(o, "GreenSaturation", "calibration.green_sat");
    n(o, "BlueHue", "calibration.blue_hue");
    n(o, "BlueSaturation", "calibration.blue_sat");

    // ---- Geometry
    n(o, "PerspectiveVertical", "geometry.vertical");
    n(o, "PerspectiveHorizontal", "geometry.horizontal");
    n(o, "PerspectiveRotate", "geometry.rotate");
    n(o, "PerspectiveScale", "geometry.scale");
    n(o, "PerspectiveAspect", "geometry.aspect");
    n(o, "PerspectiveX", "geometry.offset_x");
    n(o, "PerspectiveY", "geometry.offset_y");
    if let Some(u) = num(props, "crs:PerspectiveUpright") {
        put(o, "geometry.upright", json!(upright_mode(u).unwrap_or(Upright::Off)));
        // a stated mode never keeps a transform solved for another one: it is solved again for this
        // photo, unless the photo's frame is known and Lightroom's own saved matrix is placed
        // ([`saved_upright`], [`crate::sidecar::SidecarData::set_frame`])
        put(o, "geometry.upright_transform", Value::Null);
    }

    // ---- Crop (normalized edges of the unrotated image + straighten angle)
    match boolean(props, "crs:HasCrop") {
        Some(true) => {
            let e = |k: &str, d: f64| num(props, &format!("crs:Crop{k}")).unwrap_or(d).clamp(0.0, 1.0);
            let (l, t, r, b) = (e("Left", 0.0), e("Top", 0.0), e("Right", 1.0), e("Bottom", 1.0));
            if r > l && b > t {
                put(
                    o,
                    "crop.geometry",
                    json!({"rect": {"x0": l, "y0": t, "x1": r, "y1": b}, "angle": num(props, "crs:CropAngle").unwrap_or(0.0).clamp(-45.0, 45.0)}),
                );
            }
        }
        Some(false) => {
            // "no crop": the edges Lightroom keeps writing beside it are read, not lost adjustments
            for k in ["Left", "Top", "Right", "Bottom", "Angle"] {
                note(&format!("crs:Crop{k}"));
            }
            put(o, "crop.geometry", json!({"rect": {"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}, "angle": 0.0}))
        }
        None => {}
    }
    out
}

/// [`to_partial`], plus the adjustments in the packet it could not carry over (field names
/// without the `crs:` prefix, grouped: `CameraProfile`, `Look`, `MaskGroupBasedCorrections`…).
///
/// With `values` (the packet's structured properties), local corrections become masks too
/// ([`crate::crs_masks`]; radial masks fitted to `aspect` = width / height); components that
/// can't be carried over are reported as `Mask: <kind>`.
pub fn to_partial_report(props: &Props, values: Option<&crate::crs_masks::Values>, target: Target, aspect: f64) -> (Value, Vec<String>) {
    READ.with(|r| *r.borrow_mut() = Some(Default::default()));
    SKIPPED.with(|r| *r.borrow_mut() = Some(Vec::new()));
    let mut out = to_partial(props, target);
    let mut read = READ.with(|r| r.borrow_mut().take()).unwrap_or_default();
    let mut mask_skips = SKIPPED.with(|r| r.borrow_mut().take()).unwrap_or_default();
    if let Some(values) = values {
        let (masks, skipped) = crate::crs_masks::masks(values, aspect);
        if !masks.is_empty() {
            put(&mut out, "masks", Value::Array(masks));
        }
        if values.keys().any(|k| crate::crs_masks::CONTAINERS.contains(&k.as_str())) {
            read.extend(crate::crs_masks::CONTAINERS.iter().map(|c| c.to_string()));
            mask_skips.extend(skipped.into_iter().map(|k| format!("Mask: {k}")));
        }
        let (spots, skipped) = crate::crs_masks::spots(values, aspect);
        if !spots.is_empty() {
            put(&mut out, "spots", Value::Array(spots));
        }
        if values.keys().any(|k| crate::crs_masks::SPOT_CONTAINERS.contains(&k.as_str())) {
            read.extend(crate::crs_masks::SPOT_CONTAINERS.iter().map(|c| c.to_string()));
            mask_skips.extend(skipped.into_iter().map(|k| format!("Spot: {k}")));
        }
    }
    // fields that only switch a panel on/off or name things: not adjustments by themselves; and
    // Point Color slots that are all empty (-1)
    let value_is = |k: &str, f: &dyn Fn(&[String]) -> bool| props.get(&format!("crs:{k}")).is_some_and(|v| f(v));
    let empty_points = |v: &[String]| v.iter().all(|s| s.split(',').all(|n| n.trim().parse::<f64>().is_ok_and(|x| x == -1.0) || n.trim().is_empty()));
    // Upright's analysis record (version, saved matrices, centre, focal length, guides): placed or
    // reported by the photo import's `uprightGeometry` diagnostic, never an adjustment of its own
    let quiet = |k: &str| {
        k.starts_with("Enable")
            || k.starts_with("Upright")
            || k.starts_with("ToneCurveName")
            || k == "AutoTone"
            || k == "AutoGrayscaleMix"
            || (k == "PointColors" && value_is(k, &empty_points))
            // the lens profile's strength, written at its default (100 %) next to every profile
            || (matches!(k, "LensProfileDistortionScale" | "LensProfileVignettingScale") && value_is(k, &|v| v.iter().all(|s| s.trim() == "100")))
    };
    let mut unmapped: Vec<String> = props
        .keys()
        .filter(|k| k.starts_with("crs:"))
        .map(|k| k.split('/').next().unwrap_or(k))
        .filter(|k| !NON_ADJUSTMENT.contains(k) && !read.contains(*k))
        .map(|k| k.trim_start_matches("crs:").to_string())
        .filter(|k| !quiet(k))
        .collect();
    unmapped.dedup();
    unmapped.extend(mask_skips);
    (out, unmapped)
}

/// The selected camera profile name from an XMP packet. The name is metadata only: it does not
/// identify or carry the profile's rendering data.
pub fn camera_profile_name(props: &Props) -> Option<&str> {
    first(props, "crs:CameraProfile")
}

/// Whether an XMP packet contains a saved Lightroom Upright matrix (usable or not: see
/// [`saved_upright`]).
pub fn has_saved_upright_transform(props: &Props) -> bool {
    props.keys().any(|key| {
        key.strip_prefix("crs:").is_some_and(|name| {
            name.starts_with("UprightTransform_") && props.get(key).is_some_and(|values| values.iter().any(|v| !v.trim().is_empty()))
        })
    })
}

/// The Upright mode a `crs:PerspectiveUpright` value names: 0 Off, 1 Auto, 2 Full, 3 Level,
/// 4 Vertical, 5 Guided (issue #205: the matrix saved for each value has that mode's shape, and
/// ExifTool names the values the same way). `None` for anything else.
pub fn upright_mode(value: f64) -> Option<Upright> {
    if value.fract() != 0.0 {
        return None;
    }
    Some(match value as i64 {
        0 => Upright::Off,
        1 => Upright::Auto,
        2 => Upright::Full,
        3 => Upright::Level,
        4 => Upright::Vertical,
        5 => Upright::Guided,
        _ => return None,
    })
}

/// The Upright mode a packet selects (`crs:PerspectiveUpright`), if it names one.
pub fn selected_upright(props: &Props) -> Option<Upright> {
    num(props, "crs:PerspectiveUpright").and_then(upright_mode)
}

/// Whether a packet has Guided Upright guides (`crs:UprightFourSegments_<n>`, or a positive
/// `crs:UprightFourSegmentsCount`). LightCraft doesn't read them.
pub fn has_upright_guides(props: &Props) -> bool {
    let stated = |values: &Vec<String>| values.iter().any(|v| !v.trim().is_empty());
    props.iter().any(|(key, values)| {
        key.strip_prefix("crs:UprightFourSegments_").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) && stated(values)
    }) || num(props, "crs:UprightFourSegmentsCount").is_some_and(|n| n > 0.0)
}

/// The Upright geometry an import loses unless Lightroom's saved matrix is kept, by the mode the
/// packet selects: saved matrices for any mode but Off (an unreadable mode value is `Unknown`), and
/// Guided Upright's guides. `None`: nothing would be lost (Upright Off, or no Upright record).
pub fn upright_at_risk(props: &Props) -> Option<crate::import::ImportUprightMode> {
    use crate::import::ImportUprightMode as M;
    let (matrices, guides) = (has_saved_upright_transform(props), has_upright_guides(props));
    let stated = props.get("crs:PerspectiveUpright").is_some_and(|v| v.iter().any(|s| !s.trim().is_empty()));
    match selected_upright(props) {
        Some(Upright::Off) => None,
        None if !stated => None,
        None => (matrices || guides).then_some(M::Unknown),
        Some(Upright::Guided) => (matrices || guides).then_some(M::Guided),
        Some(Upright::Auto) => matrices.then_some(M::Auto),
        Some(Upright::Full) => matrices.then_some(M::Full),
        Some(Upright::Level) => matrices.then_some(M::Level),
        Some(Upright::Vertical) => matrices.then_some(M::Vertical),
    }
}

/// The `crs:UprightVersion` whose saved matrices were measured against Lightroom's exports (the
/// only one seen, in packets written by Camera Raw 15.0 to 17.2: Lightroom Classic 12 to 14 and
/// the Lightroom desktop app). Matrices of any other version are not read: their convention is
/// unknown.
pub const UPRIGHT_VERSION: u64 = 151_388_160;

/// Lightroom's saved Upright correction for the selected mode (`crs:UprightTransform_<value>`,
/// where value is `crs:PerspectiveUpright`).
///
/// The convention was measured, not assumed (issue #205; three Upright Auto exports of public raws,
/// one of them portrait and one cropped, matched to 0.5 px): a forward homography (uncorrected →
/// corrected, column vectors, `p' = M·p`) in coordinates normalized to the *stored* image (x / width,
/// y / height, origin top left, y down, before the EXIF orientation), applied before the crop, with
/// Lightroom's scale to fill the frame already in it. Guided Upright's slot holds no result (its
/// correction is solved from the guides when drawn), so Guided is never read from here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SavedUpright {
    pub mode: Upright,
    /// Row-major, scaled so the last element is 1.
    pub matrix: [f64; 9],
}

/// Largest magnitude accepted for an element of a saved matrix (normalized coordinates: a real
/// correction is of order 1).
const UPRIGHT_MAX_ELEMENT: f64 = 100.0;

/// The packet's saved Upright correction for its selected mode, when LightCraft can apply it: a
/// mode with an automatic result (Auto, Full, Level, Vertical), [`UPRIGHT_VERSION`], a
/// `crs:UprightTransformCount` that includes the mode's slot, and nine finite numbers that make a
/// sensible correction (no point of the photo at or beyond the horizon, no mirror, no collapse).
pub fn saved_upright(props: &Props) -> Option<SavedUpright> {
    // the slot is the mode's `crs:PerspectiveUpright` value; Off and Guided have no automatic result
    let mode = selected_upright(props)?;
    let slot = match mode {
        Upright::Auto => 1,
        Upright::Full => 2,
        Upright::Level => 3,
        Upright::Vertical => 4,
        Upright::Off | Upright::Guided => return None,
    };
    if first(props, "crs:UprightVersion")?.parse::<u64>().ok()? != UPRIGHT_VERSION {
        return None;
    }
    let count = first(props, "crs:UprightTransformCount")?.parse::<usize>().ok()?;
    if count <= slot {
        return None;
    }
    let matrix = parse_upright_matrix(first(props, &format!("crs:UprightTransform_{slot}"))?)?;
    Some(SavedUpright { mode, matrix })
}

/// Nine comma-separated numbers as a usable saved Upright matrix (see [`saved_upright`]).
fn parse_upright_matrix(text: &str) -> Option<[f64; 9]> {
    let mut m = [0.0; 9];
    let mut parts = text.split(',');
    for v in &mut m {
        *v = parts.next()?.trim().parse::<f64>().ok().filter(|x| x.is_finite())?;
    }
    if parts.next().is_some() || m[8].abs() < 1e-9 {
        return None;
    }
    let last = m[8];
    for v in &mut m {
        *v /= last;
    }
    if m.iter().any(|v| !v.is_finite() || v.abs() > UPRIGHT_MAX_ELEMENT) {
        return None;
    }
    // every point of the photo stays in front of the horizon (w is affine: its corners bound it)
    let w = |x: f64, y: f64| m[6] * x + m[7] * y + m[8];
    if [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)].iter().any(|&(x, y)| w(x, y) < 1e-3) {
        return None;
    }
    // with w > 0 there, the sign of the determinant is the sign of the local area: no mirror, no collapse
    let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6]) + m[2] * (m[3] * m[7] - m[4] * m[6]);
    (det.is_finite() && det > 1e-6).then_some(m)
}

/// Normalized stored coordinates → normalized oriented coordinates, as a homography.
fn orientation_homography(orientation: Orientation) -> Homography {
    let at = |x: f64, y: f64| {
        let (u, v) = orientation.map(x, y, 1.0, 1.0);
        Point::new(u, v)
    };
    let (o, ex, ey) = (at(0.0, 0.0), at(1.0, 0.0), at(0.0, 1.0));
    Homography([ex.x - o.x, ey.x - o.x, o.x, ex.y - o.y, ey.y - o.y, o.y, 0.0, 0.0, 1.0])
}

/// Centred oriented coordinates (`(p − centre) / (long edge / 2)`) → normalized oriented
/// coordinates, for an oriented image of `ow × oh`.
fn centred_to_normalized(ow: f64, oh: f64) -> Homography {
    let l = ow.max(oh) / 2.0;
    Homography([l / ow, 0.0, 0.5, 0.0, l / oh, 0.5, 0.0, 0.0, 1.0])
}

/// A saved Upright correction in LightCraft's frame: the homography of
/// `Geometry::upright_transform` (lens-corrected → transformed, centred coordinates of the oriented
/// image). `orientation` turns the stored image, `stored_w × stored_h` (any unit; only the aspect
/// matters), into LightCraft's oriented image. `None` for a frame without a finite, positive size.
pub fn upright_in_frame(saved: &SavedUpright, orientation: Orientation, stored_w: f64, stored_h: f64) -> Option<Homography> {
    let a = centred_to_stored(orientation, stored_w, stored_h)?;
    normalized(a.inverse()?.mul(&Homography(saved.matrix)).mul(&a))
}

/// Centred coordinates of the oriented image → normalized coordinates of the stored image.
fn centred_to_stored(orientation: Orientation, stored_w: f64, stored_h: f64) -> Option<Homography> {
    if !(stored_w.is_finite() && stored_h.is_finite() && stored_w > 0.0 && stored_h > 0.0) {
        return None;
    }
    let (ow, oh) = if orientation.swaps_axes() { (stored_h, stored_w) } else { (stored_w, stored_h) };
    Some(orientation_homography(orientation).inverse()?.mul(&centred_to_normalized(ow, oh)))
}

/// `h` scaled so its last element is 1, when that leaves it finite.
fn normalized(h: Homography) -> Option<Homography> {
    let last = h.0[8];
    let h = Homography(h.0.map(|v| v / last));
    h.0.iter().all(|v| v.is_finite()).then_some(h)
}

/// Put a placed Upright correction ([`upright_in_frame`]) into a partial settings object.
pub fn put_upright_transform(partial: &mut Value, h: Homography) {
    put(partial, "geometry.upright_transform", json!(h));
}

/// Lightroom's crop in LightCraft's frame (Lightroom catalogs and, given the photo's frame, XMP
/// sidecars: [`crate::sidecar::SidecarData::set_frame`]). `crs` gives the crop's top-left (`CropLeft`, `CropTop`)
/// and bottom-right (`CropRight`, `CropBottom`) corners on the stored image (before orientation and
/// straightening), normalized to its size, plus a straighten angle that turns the other way to
/// LightCraft's. LightCraft's crop is a rectangle in the oriented image rotated by `angle` about its
/// centre, normalized to the oriented image's size, so the corners go through the orientation and
/// then the rotation; `stored_aspect` (width / height of the stored image) matters once the
/// rectangle turns. On a real catalog this reproduced the frame aspect of all 223 straightened crops
/// in Lightroom's exports within 1%.
pub fn crop_in_frame(partial: &mut Value, orientation: Orientation, stored_aspect: f64) {
    let Some(geometry) = partial.pointer_mut("/crop/geometry").and_then(Value::as_object_mut) else { return };
    let edge = |k: &str| geometry.get("rect").and_then(|r| r.get(k)).and_then(Value::as_f64);
    let (Some(x0), Some(y0), Some(x1), Some(y1)) = (edge("x0"), edge("y0"), edge("x1"), edge("y1")) else { return };
    let lightroom_angle = geometry.get("angle").and_then(Value::as_f64).unwrap_or(0.0);
    if orientation == Orientation::Normal && lightroom_angle == 0.0 {
        return;
    }
    let w = if stored_aspect.is_finite() && stored_aspect > 0.0 { stored_aspect } else { 1.0 };
    let (ow, oh) = if orientation.swaps_axes() { (1.0, w) } else { (w, 1.0) };
    // a mirror turns the straighten the other way too
    let angle = if orientation.to_parts().0 { lightroom_angle } else { -lightroom_angle };
    let to_rotated = Affine::rotate_about(angle.to_radians(), Point::new(ow / 2.0, oh / 2.0));
    let corner = |x: f64, y: f64| {
        let (u, v) = orientation.map(x * w, y, w, 1.0);
        to_rotated.apply(Point::new(u, v))
    };
    let r = Rect::from_points(corner(x0, y0), corner(x1, y1));
    geometry.insert("rect".into(), json!({"x0": r.x0 / ow, "y0": r.y0 / oh, "x1": r.x1 / ow, "y1": r.y1 / oh}));
    geometry.insert("angle".into(), json!(angle));
}

/// Read an XMP preset (`crs:` fields + `crs:Name` / `crs:Group`) into one of our presets.
/// Returns `None` when the packet has no adjustments we understand.
pub fn preset_from_xmp(xmp: &str, fallback_name: &str) -> Option<Preset> {
    let d = lightcraft_meta::parse_xmp(xmp).ok()?;
    let props = &d.properties;
    let settings = to_partial(props, Target::Any);
    if settings.as_object().is_none_or(Map::is_empty) {
        return None;
    }
    let name = first(props, "crs:Name").unwrap_or(fallback_name).to_string();
    let group = first(props, "crs:Group").unwrap_or("Imported Presets").to_string();
    let uuid = first(props, "crs:UUID").map(|u| u.to_ascii_lowercase());
    let id = format!("user.xmp.{}", uuid.unwrap_or_else(|| crate::presets::slug(&format!("{group}-{name}"))));
    Some(Preset { id, name, group, settings, favorite: false, builtin: false })
}

#[cfg(test)]
mod tests;
