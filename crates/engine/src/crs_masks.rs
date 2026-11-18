//! Interchange: local corrections (masks) stored as `crs:` structures in XMP sidecars, XMP
//! presets and `.lrtemplate` files → our [`Mask`](lcc_develop::Mask)s.
//!
//! Four containers hold them: `MaskGroupBasedCorrections` (current: each correction has a list
//! of mask components) and the older `GradientBasedCorrections`, `CircularGradientBasedCorrections`
//! and `PaintBasedCorrections`. Every correction carries its local sliders (`Local*`, stored as
//! −1..1 fractions; exposure ×4 EV) and its components (`crs:What` = `Mask/Gradient`,
//! `Mask/CircularGradient`, `Mask/Paint`, `Mask/Image`, `Mask/RangeMask`).
//!
//! Implemented from the public XMP data model and black-box observation; best effort — the
//! pipelines differ, and the component combine modes and AI mask kinds are mapped from what the
//! fields evidently mean. Unsupported components are reported, never guessed.

use std::collections::BTreeMap;

use lcc_meta::XmpValue;
use serde_json::{Value, json};

/// Top-level properties keyed `prefix:name`, with structure.
pub type Values = BTreeMap<String, XmpValue>;

/// The containers this module reads.
pub const CONTAINERS: [&str; 4] =
    ["crs:MaskGroupBasedCorrections", "crs:GradientBasedCorrections", "crs:CircularGradientBasedCorrections", "crs:PaintBasedCorrections"];

fn text<'a>(v: &'a XmpValue, k: &str) -> Option<&'a str> {
    v.field(k).and_then(XmpValue::text).filter(|s| !s.is_empty())
}

fn num(v: &XmpValue, k: &str) -> Option<f64> {
    let s = text(v, k)?;
    let s = s.strip_prefix('+').unwrap_or(s);
    s.parse::<f64>().ok().filter(|x| x.is_finite())
}

fn flag(v: &XmpValue, k: &str) -> Option<bool> {
    text(v, k).map(|s| s.eq_ignore_ascii_case("true") || s == "1")
}

/// Local slider fields → (our `adjust` field, scale from the stored fraction).
const LOCAL: &[(&str, &str, f64)] = &[
    ("LocalExposure2012", "exposure", 4.0),
    ("LocalContrast2012", "contrast", 100.0),
    ("LocalHighlights2012", "highlights", 100.0),
    ("LocalShadows2012", "shadows", 100.0),
    ("LocalWhites2012", "whites", 100.0),
    ("LocalBlacks2012", "blacks", 100.0),
    ("LocalClarity2012", "clarity", 100.0),
    ("LocalTexture", "texture", 100.0),
    ("LocalDehaze", "dehaze", 100.0),
    ("LocalTemperature", "temp", 100.0),
    ("LocalTint", "tint", 100.0),
    ("LocalSaturation", "saturation", 100.0),
    ("LocalHue", "hue", 100.0),
    ("LocalSharpness", "sharpness", 100.0),
    ("LocalLuminanceNoise", "noise", 100.0),
    ("LocalMoire", "moire", 100.0),
    ("LocalDefringe", "defringe", 100.0),
    ("LocalToningSaturation", "color_sat", 100.0),
    ("LocalToningHue", "color_hue", 1.0),
    // older process versions
    ("LocalExposure", "exposure", 4.0),
    ("LocalContrast", "contrast", 100.0),
    ("LocalClarity", "clarity", 100.0),
];

/// The local slider fields (name without `crs:`, our `adjust` field, scale), newest process first.
pub(crate) fn local_fields() -> impl Iterator<Item = (&'static str, &'static str, f64)> {
    LOCAL.iter().copied()
}

/// Is `crs` (without the `crs:` prefix) a local slider field we map?
pub(crate) fn is_local_field(crs: &str) -> bool {
    LOCAL.iter().any(|(k, _, _)| *k == crs)
}

/// Our local adjustment values from the `Local*` slider fields that `get` finds (the name without
/// the `crs:` prefix → the stored fraction); process-version 2012 fields win over the older ones.
pub(crate) fn local_values(get: impl Fn(&str) -> Option<f64>) -> serde_json::Map<String, Value> {
    let mut a = serde_json::Map::new();
    for (crs, ours, k) in LOCAL {
        if a.contains_key(*ours) {
            continue;
        }
        if let Some(v) = get(crs).filter(|v| v.is_finite()) {
            let lim = if *ours == "exposure" {
                4.0
            } else if *ours == "color_hue" {
                360.0
            } else {
                100.0
            };
            let v = (v * k).clamp(if *ours == "color_hue" { 0.0 } else { -lim }, lim);
            a.insert((*ours).into(), json!(v));
        }
    }
    a
}

fn adjust(c: &XmpValue) -> Value {
    let mut a = local_values(|crs| num(c, &format!("crs:{crs}")));
    // a mask Hue past Lightroom's range, kept whole in our own namespace: wins over `crs:LocalHue`
    if let Some(deg) = num(c, crate::preset_xmp::LC_LOCAL_HUE).filter(|v| v.is_finite()) {
        a.insert("hue".into(), json!(deg.clamp(-lcc_develop::LOCAL_HUE_MAX, lcc_develop::LOCAL_HUE_MAX)));
        a.insert("hue_deg".into(), json!(true));
    }
    let amount = num(c, "crs:CorrectionAmount").unwrap_or(1.0);
    a.insert("amount".into(), json!((amount * 100.0).clamp(0.0, 200.0)));
    Value::Object(a)
}

/// The correction's local sliders we have no counterpart for (e.g. `LocalGrain`), when set:
/// reported, never dropped silently.
fn unmapped_local(c: &XmpValue) -> Vec<String> {
    let XmpValue::Struct(m) = c else { return Vec::new() };
    m.keys()
        .filter_map(|k| {
            let field = k.strip_prefix("crs:").filter(|f| f.starts_with("Local"))?;
            // 100 = "curve saturation unchanged", written with every correction
            let neutral = if field == "LocalCurveRefineSaturation" { 100.0 } else { 0.0 };
            let set = num(c, k).is_some_and(|v| v != neutral);
            (set && !LOCAL.iter().any(|(crs, ..)| *crs == field)).then(|| field.to_string())
        })
        .collect()
}

/// A local point curve (`"x,y"` items on 0..255) → our points on 0..1: `[]` for the straight line, `None`
/// when absent or unreadable.
fn local_curve(c: &XmpValue, k: &str) -> Option<Value> {
    let mut pts = Vec::new();
    for it in c.field(k)?.items().iter().filter_map(XmpValue::text).take(64) {
        let (x, y) = it.split_once(',')?;
        let n = |t: &str| t.trim().parse::<f64>().ok().filter(|v| v.is_finite());
        pts.push(((n(x)? / 255.0).clamp(0.0, 1.0), (n(y)? / 255.0).clamp(0.0, 1.0)));
    }
    if pts.len() < 2 {
        return None;
    }
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let identity = pts.iter().all(|(x, y)| (x - y).abs() < 1e-9);
    Some(if identity { json!([]) } else { Value::Array(pts.into_iter().map(|(x, y)| pt(x, y)).collect()) })
}

/// A correction's local Curve (`MainCurve`, `RedCurve`, `GreenCurve`, `BlueCurve`) as our
/// `PointCurves` JSON, when a channel is bent.
fn local_curves(c: &XmpValue) -> Option<Value> {
    let mut o = serde_json::Map::new();
    for (k, ch) in [("crs:MainCurve", "master"), ("crs:RedCurve", "red"), ("crs:GreenCurve", "green"), ("crs:BlueCurve", "blue")] {
        if let Some(v) = local_curve(c, k).filter(|v| v.as_array().is_some_and(|a| !a.is_empty())) {
            o.insert(ch.into(), v);
        }
    }
    (!o.is_empty()).then_some(Value::Object(o))
}

/// A correction's local Point Color samples (`LocalPointColors`, with `LocalColorVariance`) as our
/// `PointColor` JSON ([`crate::crs_point_color`]).
fn local_points(c: &XmpValue) -> Vec<Value> {
    let texts = |k: &str| -> Vec<&str> { c.field(k).map(XmpValue::items).unwrap_or_default().iter().filter_map(XmpValue::text).collect() };
    let var = crate::crs_point_color::variances(texts("crs:LocalColorVariance"));
    crate::crs_point_color::read(texts("crs:LocalPointColors"), &var).iter().filter_map(|p| serde_json::to_value(p).ok()).collect()
}

/// Perceptual lightness 0..1 (how range masks store luminance) → our range scale: log
/// luminance on −8..+4 EV mapped to 0..1.
fn lightness_to_range(l: f64) -> f64 {
    let y = l.clamp(1e-4, 1.0).powf(2.2);
    ((y.log2() + 8.0) / 12.0).clamp(0.0, 1.0)
}

fn pt(x: f64, y: f64) -> Value {
    json!({"x": x, "y": y})
}

/// Whether a `Mask/CircularGradient` component puts its effect inside the ellipse, given the
/// container it came from (`crs:Flipped` means different things in different formats).
///
/// What Lightroom Classic does, from its exports of synthetic DNGs whose masks we wrote (issue #1815):
/// - **`MaskGroupBasedCorrections`, `crs:Version` ≥ 2** (what Lightroom writes today): an ordinary
///   radial — effect inside — is written `Flipped="true"`, so `Flipped` is the "inside" flag.
///   Inverting the mask is a separate field, `crs:MaskInverted` (see [`component`]). Lightroom
///   Classic 15.6 (Camera Raw 18.7) writes an inverted radial as `MaskInverted="true"` *with*
///   `Flipped="false"`: so when `MaskInverted` is true, `Flipped` isn't read, or the two would cancel
///   out. Otherwise `Flipped="false"` is read as outside, the same sense as the legacy filter's;
///   Lightroom never wrote that combination for us.
/// - **`MaskGroupBasedCorrections` without `crs:Version`**: Lightroom renders the effect inside
///   whatever `Flipped` says, and rewrites both `Flipped="false"` and `"true"` as `Flipped="true"`
///   with `Version="2"`. So `Flipped` is ignored here.
/// - **`CircularGradientBasedCorrections`** (the process-2012 Radial Filter): its default effect was
///   outside, and "Invert Mask" — effect inside — was recorded as `Flipped="true"`. The same "inside"
///   flag as version 2, with outside as the default when it is missing.
fn radial_inside(m: &XmpValue, container: &str) -> bool {
    let flipped = flag(m, "crs:Flipped");
    if container == "crs:MaskGroupBasedCorrections" {
        let inverted = flag(m, "crs:MaskInverted").unwrap_or(false);
        if num(m, "crs:Version").is_some_and(|v| v >= 2.0) && !inverted { flipped.unwrap_or(true) } else { true }
    } else {
        flipped.unwrap_or(false)
    }
}

/// A `Mask/CircularGradient` component's feather on our 0..100 scale.
///
/// Lightroom halves `crs:Feather` when it upgrades a `MaskGroupBasedCorrections` radial without
/// `crs:Version` to version 2 (50 → 25, 100 → 50) and renders both the same, so the version-less
/// value is on twice the scale. Version 2's value is used as it is: its 25 and 50 put the middle of
/// Lightroom's fall-off at about 0.87 and 0.76 of the radius, ours at about 0.86 and 0.73. (The
/// shape differs: Lightroom's fall-off straddles the ellipse edge, ours ends at it.) The legacy
/// Radial Filter's value is used as it is: no observation says otherwise.
fn radial_feather(m: &XmpValue, container: &str) -> f64 {
    let f = num(m, "crs:Feather").unwrap_or(50.0);
    let versionless_group = container == "crs:MaskGroupBasedCorrections" && !num(m, "crs:Version").is_some_and(|v| v >= 2.0);
    (if versionless_group { f / 2.0 } else { f }).clamp(0.0, 100.0)
}

/// A dab line of a `Mask/Paint`'s `crs:Dabs`: `d x y` (a dab), `M x y` (an Auto Mask dab); `r …` and `h …`
/// (the radius or CenterWeight changing mid-stroke) and anything else are not dabs.
fn dab(line: &str) -> Option<(bool, f64, f64)> {
    let mut it = line.split_whitespace();
    let auto = match it.next()? {
        "d" => false,
        "M" => true,
        _ => return None,
    };
    let (x, y) = (it.next()?.parse::<f64>().ok()?, it.next()?.parse::<f64>().ok()?);
    (x.is_finite() && y.is_finite()).then_some((auto, x, y))
}

/// A Lightroom brush stroke (`Mask/Paint`) → our Brush component, painted with Lightroom's brush model
/// (`lcc_develop::brush_model`, notes 18/22): `crs:Radius` is where the halo ends (a fraction of the long
/// side) and `crs:CenterWeight` follows Feather, so Feather = the inverse of the measured CenterWeight table and
/// Size (the solid core) = Radius / outer_ratio(Feather). `M` dabs are Auto Mask dabs (Lightroom recomputes the
/// Auto Mask from them, as we do); a stroke mixing `d` and `M` dabs becomes one stroke per run.
fn paint(m: &XmpValue) -> Result<Value, String> {
    use lcc_develop::brush_model;
    let dabs: Vec<(bool, f64, f64)> =
        m.field("crs:Dabs").map(XmpValue::items).unwrap_or_default().iter().filter_map(XmpValue::text).filter_map(dab).collect();
    if dabs.is_empty() {
        return Err("empty brush".into());
    }
    let value = num(m, "crs:MaskValue").filter(|v| v.is_finite()).unwrap_or(1.0);
    let erase = value <= 0.0;
    // an eraser saved at 0 erases fully (a density of 0 would erase nothing)
    let density = if erase && value == 0.0 { 100.0 } else { (value.abs() * 100.0).clamp(0.0, 100.0) };
    let feather = num(m, "crs:CenterWeight").map_or(50.0, brush_model::feather_of);
    let radius = num(m, "crs:Radius").filter(|v| v.is_finite()).unwrap_or(0.03).clamp(0.001, 1.0);
    let size = (radius / brush_model::outer_ratio(feather)).clamp(0.0005, 1.0);
    let flow = (num(m, "crs:Flow").filter(|v| v.is_finite()).unwrap_or(1.0) * 100.0).clamp(0.0, 100.0);
    let mut strokes: Vec<Value> = Vec::new();
    for run in dabs.chunk_by(|a, b| a.0 == b.0) {
        let auto = run.first().is_some_and(|d| d.0);
        strokes.push(json!({
            "points": run.iter().map(|(_, x, y)| pt(*x, *y)).collect::<Vec<_>>(),
            "size": size,
            "feather": feather,
            "flow": flow,
            "density": density,
            "erase": erase,
            "auto_mask": auto,
            "model": lcc_develop::BRUSH_MODEL_LRC,
        }));
    }
    Ok(json!({"kind": "brush", "strokes": strokes}))
}

/// One mask component of a correction in `container` → our component JSON; `Err(kind)` names an
/// unsupported one.
/// Lightroom's People parts by `MaskSubCategoryID`, as Lightroom Classic writes them in presets
/// (read from presets made with it: face skin 2, iris and pupil 3, body skin 4, hair 5, lips 6,
/// eye sclera 8, clothes 11, teeth 12). Other parts are recognised by the mask's name.
fn people_part(id: i64) -> Option<&'static str> {
    Some(match id {
        2 => "Face Skin",
        3 => "Iris and Pupil",
        4 => "Body Skin",
        5 => "Hair",
        6 => "Lips",
        8 => "Eye Sclera",
        11 => "Clothes",
        12 => "Teeth",
        _ => return None,
    })
}

/// A People part from a mask's name ("Person 1 - Face Skin", "Clothes", "Eyebrows 2").
fn part_from_name(name: &str) -> Option<&'static str> {
    let tail = name.rsplit(" - ").next().unwrap_or(name).trim();
    let tail = tail.trim_end_matches(|c: char| c.is_ascii_digit() || c == ' ');
    const PARTS: [&str; 11] =
        ["Entire Person", "Face Skin", "Body Skin", "Hair", "Clothes", "Eyebrows", "Eye Sclera", "Iris and Pupil", "Lips", "Teeth", "Skin"];
    PARTS.into_iter().find(|p| p.eq_ignore_ascii_case(tail))
}

/// A Landscape class from a mask's name ("Water 1", "Landscape - Vegetation").
fn landscape_from_name(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    [
        ("artificial ground", "artificial ground"),
        ("natural ground", "natural ground"),
        ("vegetation", "vegetation"),
        ("mountain", "mountains"),
        ("architecture", "architecture"),
        ("water", "water"),
    ]
    .into_iter()
    .find(|(k, _)| n.contains(k))
    .map(|(_, v)| v)
}

/// A Lightroom AI mask (`Mask/Image`) → our shape. Lightroom keeps in presets what to select,
/// not the pixels, and selects again on each photo; so do we (a recipe, computed per
/// photo). People masks name "Person 1" after the photo the preset was made on: they become
/// everyone's (Lightroom detects the people of each photo again).
fn image_mask(m: &XmpValue) -> Result<Value, String> {
    let sub = text(m, "crs:MaskSubType").unwrap_or("?");
    let cat = text(m, "crs:MaskSubCategoryID").and_then(|c| c.trim().parse::<i64>().ok());
    let name = text(m, "crs:MaskName").unwrap_or("");
    Ok(match (sub, cat) {
        ("1", _) => json!({"kind": "subject"}),
        ("2", _) => json!({"kind": "sky"}),
        ("0", Some(22)) => json!({"kind": "background"}),
        ("0", _) if name.to_ascii_lowercase().starts_with("background") => json!({"kind": "background"}),
        ("3", _) => {
            let part = cat
                .and_then(people_part)
                .or_else(|| part_from_name(name))
                .ok_or_else(|| format!("AI mask: People part {}", cat.map_or("?".into(), |c| c.to_string())))?;
            json!({"kind": "people", "person": 0, "parts": [part]})
        }
        ("4", _) => return Err("AI mask: Objects (selected on the photo the preset was made on)".into()),
        _ => match landscape_from_name(name) {
            Some(class) => json!({"kind": "landscape", "class": class}),
            None => return Err(format!("AI mask ({sub}{})", cat.map_or(String::new(), |c| format!("/{c}")))),
        },
    })
}

fn component(m: &XmpValue, container: &str, aspect: f64) -> Result<Value, String> {
    let what = text(m, "crs:What").unwrap_or("");
    let op = match num(m, "crs:MaskBlendMode").unwrap_or(0.0) as i64 {
        1 => "subtract",
        2 => "intersect",
        _ => "add",
    };
    let invert = flag(m, "crs:MaskInverted").unwrap_or(false);
    // positions are fractions of the image's width / height; our radii are in long-edge units
    let (sx, sy) = if aspect >= 1.0 { (1.0, 1.0 / aspect) } else { (aspect, 1.0) };
    let shape = match what {
        "Mask/Gradient" => {
            let g = |k: &str, d: f64| num(m, &format!("crs:{k}")).unwrap_or(d);
            json!({"kind": "linear", "start": pt(g("FullX", 0.5), g("FullY", 0.0)), "end": pt(g("ZeroX", 0.5), g("ZeroY", 1.0))})
        }
        "Mask/CircularGradient" => {
            let g = |k: &str, d: f64| num(m, &format!("crs:{k}")).unwrap_or(d);
            let (l, r, t, b) = (g("Left", 0.25), g("Right", 0.75), g("Top", 0.25), g("Bottom", 0.75));
            json!({
                "kind": "radial",
                "center": pt((l + r) / 2.0, (t + b) / 2.0),
                "rx": ((r - l).abs() / 2.0 * sx).max(1e-3),
                "ry": ((b - t).abs() / 2.0 * sy).max(1e-3),
                "angle": g("Angle", 0.0),
                "feather": radial_feather(m, container),
                // our radial's `invert` = effect outside; `crs:MaskInverted` stays the component's
                "invert": !radial_inside(m, container),
            })
        }
        "Mask/Paint" => paint(m)?,
        "Mask/Image" => image_mask(m)?,
        "Mask/RangeMask" => {
            let r = m.field("crs:CorrectionRangeMask").unwrap_or(m);
            match text(r, "crs:Type") {
                Some("2") => {
                    let q: Vec<f64> = text(r, "crs:LumRange").unwrap_or("0 0 1 1").split_whitespace().filter_map(|v| v.parse().ok()).collect();
                    let [a, b, c, d] = q[..] else { return Err("luminance range".into()) };
                    // full strength at white (1, the top of their scale) takes in everything brighter: ours
                    // goes on above scene white, so that end stays open instead of cutting off at 0 EV
                    let open_top = c >= 1.0;
                    let (a, b, c, d) = (lightness_to_range(a), lightness_to_range(b), lightness_to_range(c), lightness_to_range(d));
                    let (hi, hi_feather) = if open_top { (1.0, 0.0) } else { (c, (d - c).max(0.0)) };
                    json!({"kind": "luminanceRange", "lo": b, "hi": hi, "lo_feather": (b - a).max(0.0), "hi_feather": hi_feather})
                }
                Some("3") => {
                    let q: Vec<f64> = text(r, "crs:DepthRange").unwrap_or("0 0 1 1").split_whitespace().filter_map(|v| v.parse().ok()).collect();
                    let [a, b, c, _] = q[..] else { return Err("depth range".into()) };
                    json!({"kind": "depthRange", "lo": b, "hi": c, "feather": (b - a).max(0.0)})
                }
                _ => return Err("colour range".into()),
            }
        }
        other => return Err(if other.is_empty() { "unknown mask".into() } else { other.trim_start_matches("Mask/").to_string() }),
    };
    Ok(json!({"op": op, "invert": invert, "shape": shape}))
}

/// Masks in `values` (JSON for `DevelopSettings::masks`), plus the kinds of component that
/// couldn't be carried over. `aspect` = width / height of the target (presets: assume 3:2).
pub fn masks(values: &Values, aspect: f64) -> (Vec<Value>, Vec<String>) {
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for container in CONTAINERS {
        let Some(list) = values.get(container) else { continue };
        for c in list.items() {
            if flag(c, "crs:CorrectionActive") == Some(false) {
                continue;
            }
            let comps = c.field("crs:CorrectionMasks").map(XmpValue::items).unwrap_or_default();
            let mut parts = Vec::new();
            for m in comps {
                match component(m, container, aspect) {
                    Ok(v) => parts.push(v),
                    Err(kind) => skipped.push(kind),
                }
            }
            if parts.is_empty() {
                continue;
            }
            skipped.extend(unmapped_local(c));
            // brush dabs of one correction are one brush component
            let mut merged: Vec<Value> = Vec::new();
            // where the brush of each op sits in `merged` (no scan of it per dab)
            let mut brush_at: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
            for p in parts {
                let is_brush = p["shape"]["kind"] == "brush";
                if is_brush
                    && let Some(prev) = brush_at.get(&p["op"].to_string()).and_then(|&i| merged.get_mut(i))
                    && let (Some(a), Some(b)) = (prev["shape"]["strokes"].as_array_mut(), p["shape"]["strokes"].as_array())
                {
                    a.extend(b.iter().cloned());
                    continue;
                }
                if is_brush {
                    brush_at.insert(p["op"].to_string(), merged.len());
                }
                merged.push(p);
            }
            let n = out.len() + 1;
            let name = text(c, "crs:CorrectionName").map(str::to_string).unwrap_or_else(|| format!("Mask {n}"));
            let mut mask = json!({"id": n, "name": name, "visible": true, "invert": false, "components": merged, "adjust": adjust(c)});
            if let Some(curve) = local_curves(c) {
                mask["curve"] = curve;
            }
            let points = local_points(c);
            if !points.is_empty() {
                mask["point_colors"] = Value::Array(points);
            }
            out.push(mask);
        }
    }
    skipped.sort();
    skipped.dedup();
    (out, skipped)
}

/// Put a Lightroom packet's spots and mask shapes (mapped by [`spots`] and [`masks`]) in the app's frame. Lightroom
/// normalizes them to the stored image, before its orientation `o`; the app to the oriented one. `stored_aspect` is the
/// stored image's width / height; radial sizes come in fitted to it and leave fitted to the oriented image (its aspect
/// inverted on a quarter turn). Points map with the image; a radial's sizes swap on a quarter turn and its angle turns
/// (a mirror reverses it); a spot's source offset turns as a vector. Range and AI masks carry no geometry, and
/// Lightroom's Upright guides and matrix aren't imported.
pub fn orient_geometry(partial: &mut Value, o: lcc_geom::Orientation, stored_aspect: f64) {
    if o == lcc_geom::Orientation::Normal || !(stored_aspect.is_finite() && stored_aspect > 0.0) {
        return;
    }
    let (w, h) = (stored_aspect, 1.0);
    let (ow, oh) = if o.swaps_axes() { (h, w) } else { (w, h) };
    let map = |x: f64, y: f64| {
        let (u, v) = o.map(x * w, y * h, w, h);
        (u / ow, v / oh)
    };
    let map_pt = |p: &mut Value| {
        if let (Some(x), Some(y)) = (p["x"].as_f64(), p["y"].as_f64()) {
            let (u, v) = map(x, y);
            *p = pt(u, v);
        }
    };
    if let Some(spots) = partial.get_mut("spots").and_then(Value::as_array_mut) {
        for s in spots {
            let first = s["points"].get(0).and_then(|p| Some((p["x"].as_f64()?, p["y"].as_f64()?)));
            if let (Some((x, y)), Some(dx), Some(dy)) = (first, s["source_offset"]["x"].as_f64(), s["source_offset"]["y"].as_f64()) {
                let ((u0, v0), (u1, v1)) = (map(x, y), map(x + dx, y + dy));
                s["source_offset"] = pt(u1 - u0, v1 - v0);
            }
            for p in s["points"].as_array_mut().into_iter().flatten() {
                map_pt(p);
            }
        }
    }
    let (flip, turns) = o.to_parts();
    for m in partial.get_mut("masks").and_then(Value::as_array_mut).into_iter().flatten() {
        for c in m["components"].as_array_mut().into_iter().flatten() {
            let s = &mut c["shape"];
            match s["kind"].as_str() {
                Some("linear") => {
                    map_pt(&mut s["start"]);
                    map_pt(&mut s["end"]);
                }
                Some("radial") => {
                    map_pt(&mut s["center"]);
                    if turns % 2 == 1 {
                        let (rx, ry) = (s["rx"].clone(), s["ry"].clone());
                        s["rx"] = ry;
                        s["ry"] = rx;
                    }
                    if let Some(a) = s["angle"].as_f64() {
                        // an ellipse looks the same half a turn on, so the turn's direction doesn't matter
                        let a = if flip { -a } else { a } + 90.0 * f64::from(turns);
                        s["angle"] = json!((a + 180.0).rem_euclid(360.0) - 180.0);
                    }
                }
                Some("brush") => {
                    for st in s["strokes"].as_array_mut().into_iter().flatten() {
                        for p in st["points"].as_array_mut().into_iter().flatten() {
                            map_pt(p);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// The containers spot removal is stored in: `RetouchAreas` (current, one struct per spot with a
/// mask component) and `RetouchInfo` (older, one `key = value, …` line per spot; still written
/// beside `RetouchAreas` for older readers and read on its own when that is all there is).
pub const SPOT_CONTAINERS: [&str; 2] = ["crs:RetouchAreas", "crs:RetouchInfo"];

/// One `RetouchAreas` item → a [`Spot`](lcc_develop::Spot) as JSON. `aspect` = width /
/// height of the target: horizontal sizes are stored as fractions of the width, ours of the long
/// edge. Returns the component kind it can't carry over.
fn spot(item: &XmpValue, aspect: f64) -> Result<Value, String> {
    let mode = match text(item, "crs:SpotType") {
        Some("clone") => "clone",
        // Lightroom's Content-Aware Remove (its fill is stored in Lightroom's own cache, not in the packet): ours
        // rebuilds the area from its surroundings too
        Some("heal_patchmatch") => "fill",
        _ => "heal",
    };
    let comps = item.field("crs:Masks").map(XmpValue::items).unwrap_or_default();
    let Some(m) = comps.first() else { return Err("spot without a mask".into()) };
    let width_to_long = aspect.min(1.0);
    let (points, size) = match text(m, "crs:What").unwrap_or("") {
        // a circle: centre + half-sizes in image fractions
        "Mask/Ellipse" => {
            let (Some(x), Some(y), Some(sx)) = (num(m, "crs:X"), num(m, "crs:Y"), num(m, "crs:SizeX")) else {
                return Err("ellipse spot".into());
            };
            (vec![pt(x, y)], sx * width_to_long)
        }
        // the shape exiftool's schema also allows for a spot
        "Mask/CircularGradient" => {
            let (Some(t), Some(l), Some(b), Some(r)) = (num(m, "crs:Top"), num(m, "crs:Left"), num(m, "crs:Bottom"), num(m, "crs:Right")) else {
                return Err("circular spot".into());
            };
            (vec![pt((l + r) / 2.0, (t + b) / 2.0)], (r - l).abs() / 2.0 * width_to_long)
        }
        // a brushed removal: the dabs are the path
        "Mask/Paint" => {
            let pts: Vec<Value> = m
                .field("crs:Dabs")
                .map(XmpValue::items)
                .unwrap_or_default()
                .iter()
                .filter_map(XmpValue::text)
                .filter_map(|d| {
                    let mut it = d.split_whitespace().skip(1).filter_map(|v| v.parse::<f64>().ok());
                    Some(pt(it.next()?, it.next()?))
                })
                .collect();
            if pts.is_empty() {
                return Err("empty brushed spot".into());
            }
            (pts, num(m, "crs:Radius").unwrap_or(0.02))
        }
        other => return Err(if other.is_empty() { "unknown spot".into() } else { other.trim_start_matches("Mask/").to_string() }),
    };
    let size = size.clamp(0.001, 0.25);
    // the source is stored as its absolute position (`SourceX`, and `OffsetY` despite the name);
    // ours is the offset from the target; anything but an explicit source leaves it automatic
    let explicit = text(item, "crs:SourceState").is_none_or(|s| s == "sourceSetExplicitly");
    let source_offset = match (explicit, num(item, "crs:SourceX"), num(item, "crs:OffsetY"), points.first()) {
        (true, Some(sx), Some(sy), Some(p)) => {
            let (Some(x), Some(y)) = (p["x"].as_f64(), p["y"].as_f64()) else { return Err("spot centre".into()) };
            let (dx, dy) = (sx - x, sy - y);
            (dx.is_finite() && dy.is_finite() && (dx != 0.0 || dy != 0.0)).then(|| pt(dx, dy))
        }
        _ => None,
    };
    let feather = (num(item, "crs:Feather").unwrap_or(0.0) * 100.0).clamp(0.0, 100.0);
    let opacity = (num(item, "crs:Opacity").unwrap_or(1.0) * 100.0).clamp(0.0, 100.0);
    Ok(json!({"mode": mode, "points": points, "size": size, "feather": feather, "opacity": opacity, "source_offset": source_offset}))
}

/// One `RetouchInfo` line (`centerX = 0.5, centerY = 0.5, radius = 0.01, sourceState = …,
/// sourceX = …, sourceY = …, spotType = heal`) → a spot as JSON.
fn legacy_spot(line: &str, aspect: f64) -> Option<Value> {
    let mut kv = BTreeMap::new();
    for part in line.split(',') {
        if let Some((k, v)) = part.split_once('=') {
            kv.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    let f = |k: &str| kv.get(k).and_then(|s| s.parse::<f64>().ok()).filter(|x| x.is_finite());
    let (x, y, r) = (f("centerX")?, f("centerY")?, f("radius")?);
    let mode = if kv.get("spotType").is_some_and(|s| s == "clone") { "clone" } else { "heal" };
    let explicit = kv.get("sourceState").is_none_or(|s| s == "sourceSetExplicitly");
    let source_offset = match (explicit, f("sourceX"), f("sourceY")) {
        (true, Some(sx), Some(sy)) if sx != x || sy != y => Some(pt(sx - x, sy - y)),
        _ => None,
    };
    let opacity = (f("opacity").unwrap_or(1.0) * 100.0).clamp(0.0, 100.0);
    Some(
        json!({"mode": mode, "points": [pt(x, y)], "size": (r * aspect.min(1.0)).clamp(0.001, 0.25), "feather": 0.0, "opacity": opacity, "source_offset": source_offset}),
    )
}

/// Spot removal in `values` (JSON for `DevelopSettings::spots`), plus the kinds of spot that
/// couldn't be carried over. `aspect` = width / height of the target (presets: assume 3:2).
pub fn spots(values: &Values, aspect: f64) -> (Vec<Value>, Vec<String>) {
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    if let Some(list) = values.get("crs:RetouchAreas") {
        for item in list.items() {
            match spot(item, aspect) {
                Ok(v) => out.push(v),
                Err(kind) => skipped.push(kind),
            }
        }
    }
    if out.is_empty()
        && let Some(list) = values.get("crs:RetouchInfo")
    {
        out.extend(list.items().iter().filter_map(XmpValue::text).filter_map(|l| legacy_spot(l, aspect)));
    }
    skipped.sort();
    skipped.dedup();
    (out, skipped)
}

/// A `.lrtemplate` Lua value as an XMP value (field names get the `crs:` prefix).
pub fn from_lua(v: &crate::preset_import::Lua) -> XmpValue {
    use crate::preset_import::Lua;
    match v {
        Lua::Nil => XmpValue::Text(String::new()),
        Lua::Bool(b) => XmpValue::Text(if *b { "true" } else { "false" }.into()),
        Lua::Num(n) => XmpValue::Text(format!("{n}")),
        Lua::Str(s) => XmpValue::Text(s.clone()),
        Lua::Table(arr, map) if map.is_empty() => XmpValue::Array(arr.iter().map(from_lua).collect()),
        Lua::Table(_, map) => XmpValue::Struct(map.iter().map(|(k, v)| (format!("crs:{k}"), from_lua(v))).collect()),
    }
}

/// The aspect radial masks are computed for when the target's shape isn't known yet.
pub const DEFAULT_ASPECT: f64 = 1.5;

/// Re-fit radial masks in `partial` (computed for aspect `from`) to a target of aspect `to`.
pub fn refit_radials(partial: &mut Value, from: f64, to: f64) {
    let scale = |a: f64| if a >= 1.0 { (1.0, 1.0 / a) } else { (a, 1.0) };
    let ((fx, fy), (tx, ty)) = (scale(from), scale(to));
    let Some(masks) = partial.get_mut("masks").and_then(Value::as_array_mut) else { return };
    for m in masks {
        for c in m["components"].as_array_mut().into_iter().flatten() {
            let s = &mut c["shape"];
            if s["kind"] == "radial" {
                if let Some(rx) = s["rx"].as_f64() {
                    s["rx"] = json!(rx / fx * tx);
                }
                if let Some(ry) = s["ry"].as_f64() {
                    s["ry"] = json!(ry / fy * ty);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcc_develop::{DevelopSettings, MaskOp, MaskShape};

    /// A packet in the shape local corrections take (values written for this test).
    const PACKET: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="0.2">
 <crs:MaskGroupBasedCorrections><rdf:Seq>
  <rdf:li><rdf:Description crs:What="Correction" crs:CorrectionAmount="1" crs:CorrectionActive="true" crs:CorrectionName="Darker sky"
     crs:LocalExposure2012="-0.25" crs:LocalSaturation="0.3" crs:LocalTemperature="-0.1">
   <crs:CorrectionMasks><rdf:Seq>
    <rdf:li crs:What="Mask/Image" crs:MaskSubType="2" crs:MaskBlendMode="0" crs:MaskInverted="false"/>
    <rdf:li crs:What="Mask/Gradient" crs:MaskBlendMode="2" crs:FullX="0.5" crs:FullY="0" crs:ZeroX="0.5" crs:ZeroY="0.6"/>
   </rdf:Seq></crs:CorrectionMasks>
  </rdf:Description></rdf:li>
  <rdf:li><rdf:Description crs:What="Correction" crs:CorrectionAmount="0.5" crs:LocalContrast2012="0.2">
   <crs:CorrectionMasks><rdf:Seq>
    <rdf:li crs:What="Mask/CircularGradient" crs:Top="0.3" crs:Left="0.2" crs:Bottom="0.7" crs:Right="0.8" crs:Angle="10" crs:Feather="40" crs:Flipped="true" crs:Version="2"/>
    <rdf:li crs:What="Mask/RangeMask" crs:MaskBlendMode="2"><crs:CorrectionRangeMask crs:Type="2" crs:LumRange="0.4 0.5 0.8 0.9"/></rdf:li>
    <rdf:li crs:What="Mask/Image" crs:MaskSubType="9"/>
   </rdf:Seq></crs:CorrectionMasks>
  </rdf:Description></rdf:li>
  <rdf:li><rdf:Description crs:What="Correction" crs:CorrectionActive="false" crs:LocalExposure2012="1">
   <crs:CorrectionMasks><rdf:Seq><rdf:li crs:What="Mask/Image" crs:MaskSubType="1"/></rdf:Seq></crs:CorrectionMasks>
  </rdf:Description></rdf:li>
 </rdf:Seq></crs:MaskGroupBasedCorrections>
 <crs:PaintBasedCorrections><rdf:Seq>
  <rdf:li crs:What="Correction" crs:LocalClarity2012="0.4"><crs:CorrectionMasks><rdf:Seq>
   <rdf:li crs:What="Mask/Paint" crs:Radius="0.05" crs:Flow="0.8" crs:CenterWeight="0.3" crs:MaskValue="1">
    <crs:Dabs><rdf:Seq><rdf:li>d 0.1 0.2</rdf:li><rdf:li>d 0.15 0.22</rdf:li></rdf:Seq></crs:Dabs></rdf:li>
   <rdf:li crs:What="Mask/Paint" crs:Radius="0.02" crs:MaskValue="1"><crs:Dabs><rdf:Seq><rdf:li>d 0.5 0.5</rdf:li></rdf:Seq></crs:Dabs></rdf:li>
  </rdf:Seq></crs:CorrectionMasks></rdf:li>
 </rdf:Seq></crs:PaintBasedCorrections>
</rdf:Description></rdf:RDF></x:xmpmeta>"#;

    fn settings() -> (DevelopSettings, Vec<String>) {
        let d = lcc_meta::parse_xmp(PACKET).unwrap();
        let (partial, unmapped) = crate::crs::to_partial_report(&d.properties, Some(&d.values), crate::crs::Target::Any, 1.5);
        (DevelopSettings::default().merged(&partial).expect("valid settings"), unmapped)
    }

    #[test]
    fn corrections_become_masks() {
        let (s, unmapped) = settings();
        assert_eq!(s.light.exposure, 0.2, "global settings still map");
        assert_eq!(s.masks.len(), 3, "inactive corrections are skipped: {:?}", s.masks);
        let sky = &s.masks[0];
        assert_eq!(sky.name, "Darker sky");
        assert_eq!((sky.adjust.exposure, sky.adjust.saturation, sky.adjust.temp, sky.adjust.amount), (-1.0, 30.0, -10.0, 100.0));
        assert_eq!(sky.components[0].shape, MaskShape::Sky);
        assert_eq!(sky.components[1].op, MaskOp::Intersect);
        let MaskShape::Linear { start, end } = &sky.components[1].shape else { panic!() };
        assert_eq!((start.y, end.y), (0.0, 0.6), "full effect at the Full point");
        let rad = &s.masks[1];
        assert_eq!((rad.name.as_str(), rad.adjust.contrast, rad.adjust.amount), ("Mask 2", 20.0, 50.0));
        let MaskShape::Radial { center, rx, ry, angle, feather, invert } = &rad.components[0].shape else { panic!() };
        assert!((center.x - 0.5).abs() < 1e-9 && (center.y - 0.5).abs() < 1e-9);
        assert!((rx - 0.3).abs() < 1e-9 && (ry - 0.2 / 1.5).abs() < 1e-9, "radii in long-edge units: {rx} {ry}");
        assert_eq!((*angle, *feather, *invert), (10.0, 40.0, false), "Flipped=\"true\" is Lightroom's ordinary radial: effect inside");
        assert!(!rad.components[0].invert);
        let MaskShape::LuminanceRange { lo, hi, lo_feather, .. } = &rad.components[1].shape else { panic!() };
        assert!(lo < hi && *lo_feather > 0.0);
        let brush = &s.masks[2];
        assert_eq!(brush.adjust.clarity, 40.0);
        assert_eq!(brush.components.len(), 1, "the brush's dab runs are one component");
        let MaskShape::Brush { strokes } = &brush.components[0].shape else { panic!() };
        assert_eq!(
            (strokes.len(), strokes[0].points.len(), strokes[0].flow, strokes[0].feather.round()),
            (2, 2, 80.0, lcc_develop::brush_model::feather_of(0.3).round())
        );
        assert_eq!(unmapped, vec!["Mask: AI mask (9)".to_string()]);
    }

    #[test]
    fn masks_render() {
        let (s, _) = settings();
        let img = lcc_scenes::demo_library()[0].render(96, 64);
        let render = |d: &DevelopSettings| lcc_pipeline::render(&img, &Default::default(), d, &lcc_pipeline::RenderRequest::fit(96, 64)).image.data;
        assert_ne!(render(&DevelopSettings { light: s.light, ..Default::default() }), render(&s), "the masks change the picture");
    }

    /// Synthetic single-dab correction, including the soft-brush attributes seen in the supplied
    /// Lightroom 6 sidecar. Keep personal metadata and the original photograph out of fixtures.
    fn brush_packet(container: &str, center_weight: &str) -> String {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
    <rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
     <crs:{container}><rdf:Seq><rdf:li><rdf:Description crs:What="Correction" crs:LocalExposure2012="0.25">
      <crs:CorrectionMasks><rdf:Seq><rdf:li crs:What="Mask/Paint" crs:Radius="0.065549" crs:Flow="1" crs:MaskValue="1" {center_weight}>
       <crs:Dabs><rdf:Seq><rdf:li>M 0.5 0.5</rdf:li></rdf:Seq></crs:Dabs>
      </rdf:li></rdf:Seq></crs:CorrectionMasks>
     </rdf:Description></rdf:li></rdf:Seq></crs:{container}>
    </rdf:Description></rdf:RDF></x:xmpmeta>"#
        )
    }

    /// Convert Lightroom hardness to feather in both brush containers, bounding invalid inputs.
    #[test]
    fn imported_brush_center_weight_is_hardness() {
        for container in ["PaintBasedCorrections", "MaskGroupBasedCorrections"] {
            for (attr, feather) in [
                ("", 50.0),
                (r#"crs:CenterWeight="0""#, 100.0),
                (r#"crs:CenterWeight="0.3""#, lcc_develop::brush_model::feather_of(0.3)),
                (r#"crs:CenterWeight="1""#, 0.0),
                (r#"crs:CenterWeight="-2""#, 100.0),
                (r#"crs:CenterWeight="2""#, 0.0),
                (r#"crs:CenterWeight="NaN""#, 50.0),
            ] {
                let d = lcc_meta::parse_xmp(&brush_packet(container, attr)).unwrap();
                let (partial, unmapped) = crate::crs::to_partial_report(&d.properties, Some(&d.values), crate::crs::Target::Any, 1.5);
                assert!(unmapped.is_empty(), "{unmapped:?}");
                let s = DevelopSettings::default().merged(&partial).unwrap();
                let MaskShape::Brush { strokes } = &s.masks[0].components[0].shape else { panic!() };
                // Lightroom's brush: Feather is the inverse of the measured CenterWeight table, Size the solid core
                assert!((strokes[0].feather - feather).abs() < 0.5, "{container} {attr}: {}", strokes[0].feather);
                let outer = strokes[0].size * lcc_develop::brush_model::outer_ratio(strokes[0].feather);
                assert!((outer - 0.065549).abs() < 1e-9, "feather conversion must preserve the outer radius: {outer}");
            }
        }
    }

    /// Preserve a soft brush's falloff and outer radius through a portrait crop at two resolutions.
    #[test]
    fn lr6_soft_brush_renders_a_falloff_after_crop_at_each_resolution() {
        let d = lcc_meta::parse_xmp(&brush_packet("PaintBasedCorrections", r#"crs:CenterWeight="0""#)).unwrap();
        let (mut partial, _) = crate::crs::to_partial_report(&d.properties, Some(&d.values), crate::crs::Target::Any, 4928.0 / 3264.0);
        // The supplied photo is cropped from a landscape sensor to a portrait frame.
        partial["crop"] = json!({"geometry": {"rect": {"x0": 0.297266, "y0": 0.0, "x1": 0.738825, "y1": 1.0}}});
        let s = DevelopSettings::default().merged(&partial).unwrap();
        let frame = lcc_pipeline::geometry::Frame::new(4928, 3264, &s, true);
        let shape = &s.masks[0].components[0].shape;
        for width in [220, 440] {
            let (w, h) = frame.fit(width, 1000);
            let img = lcc_raster::Rgb32f::filled(w, h, [0.18f32; 3]);
            let log_l = lcc_raster::Plane::new(w, h);
            let alpha = lcc_pipeline::masks::shape_alpha(shape, &frame, w, h, &img, &log_l, 0.0, None);
            let center = frame.norm_to_out(w, h).apply(lcc_geom::Point::new(0.5, 0.5));
            let radius = 0.065549 * frame.px_per_long(w);
            let sample = |fraction: f64| {
                let x = (center.x + radius * fraction).floor() as usize;
                alpha.data[center.y.floor() as usize * w + x]
            };
            assert!(sample(0.0) > 0.99);
            // a soft falloff all the way out (Lightroom's Feather-100 dab, a Gaussian)
            assert!(sample(0.5) > 0.05 && sample(0.5) < 0.6, "half-radius falloff at {w} px: {}", sample(0.5));
            assert!(sample(0.25) > sample(0.5) && sample(0.5) > sample(0.9), "falls off");
            assert!(sample(0.9) < 0.06, "the outer ring must not be a solid disk: {}", sample(0.9));
            assert_eq!(sample(1.1), 0.0);
        }
    }

    /// Preserve soft brush edges when importing catalog-style Lua develop settings.
    #[test]
    fn catalog_style_lua_brush_keeps_soft_edges() {
        // Catalog develop settings use the same Lua-table parser as lrtemplate presets.
        let t = r#"s = { title = "Soft", value = { settings = { PaintBasedCorrections = {
            { What = "Correction", LocalExposure2012 = 0.25, CorrectionMasks = {
                { What = "Mask/Paint", Radius = 0.065549, CenterWeight = 0, Flow = 1, MaskValue = 1,
                  Dabs = { "d 0.5 0.5" } } } } } } } }"#;
        let imported = crate::preset_import::read_presets("Soft.lrtemplate", t.as_bytes(), None).unwrap();
        let s = DevelopSettings::default().merged(&imported[0].preset.settings).unwrap();
        let MaskShape::Brush { strokes } = &s.masks[0].components[0].shape else { panic!() };
        assert!(strokes[0].feather > 99.5, "{}", strokes[0].feather);
        let outer = strokes[0].size * lcc_develop::brush_model::outer_ratio(strokes[0].feather);
        assert!((outer - 0.065549).abs() < 1e-9, "the outer radius is kept: {outer}");
    }

    /// Apply zero-valued erase strokes to earlier paint without changing paint outside the eraser.
    #[test]
    fn lightroom_zero_mask_value_erases_previous_paint() {
        // Lightroom 6 writes both M and d dab runs, and MaskValue=0 on erasers. The supplied
        // sidecar uses these to trim the animal's outline; zero-density erasers did nothing.
        let eraser = r#"<rdf:li crs:What="Mask/Paint" crs:Radius="0.02" crs:Flow="1" crs:CenterWeight="1" crs:MaskValue="0">
            <crs:Dabs><rdf:Seq><rdf:li>d 0.5 0.5</rdf:li></rdf:Seq></crs:Dabs></rdf:li>"#;
        for container in ["PaintBasedCorrections", "MaskGroupBasedCorrections"] {
            let packet = brush_packet(container, r#"crs:CenterWeight="1""#)
                .replace("</rdf:Seq></crs:CorrectionMasks>", &format!("{eraser}</rdf:Seq></crs:CorrectionMasks>"));
            let d = lcc_meta::parse_xmp(&packet).unwrap();
            let (partial, unmapped) = crate::crs::to_partial_report(&d.properties, Some(&d.values), crate::crs::Target::Any, 1.5);
            assert!(unmapped.is_empty(), "{unmapped:?}");
            let s = DevelopSettings::default().merged(&partial).unwrap();
            let shape = &s.masks[0].components[0].shape;
            let MaskShape::Brush { strokes } = shape else { panic!() };
            assert_eq!(strokes.len(), 2);
            assert!(!strokes[0].erase && strokes[1].erase);
            assert_eq!(strokes[1].density, 100.0, "MaskValue=0 is a full-strength eraser");
            let frame = lcc_pipeline::geometry::Frame::new(300, 200, &s, true);
            let img = lcc_raster::Rgb32f::filled(300, 200, [0.18f32; 3]);
            let log_l = lcc_raster::Plane::new(300, 200);
            let alpha = lcc_pipeline::masks::shape_alpha(shape, &frame, 300, 200, &img, &log_l, 0.0, None);
            assert_eq!(alpha.data[100 * 300 + 150], 0.0, "erase removes the painted centre");
            assert!(alpha.data[100 * 300 + 162] > 0.99, "paint beyond the eraser stays: {}", alpha.data[100 * 300 + 162]);
            assert_eq!(alpha.data[100 * 300 + 175], 0.0, "the outer brush radius stays fixed");
        }
    }

    #[test]
    fn range_to_white_stays_open_and_unmapped_local_sliders_are_reported() {
        // written for this test: a highlights range (full from 0.85 up to white) carrying a local slider we lack
        let x = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
 <crs:MaskGroupBasedCorrections><rdf:Seq>
  <rdf:li><rdf:Description crs:What="Correction" crs:LocalClarity2012="-0.3" crs:LocalGrain="0.4" crs:LocalCurveRefineSaturation="100">
   <crs:CorrectionMasks><rdf:Seq>
    <rdf:li crs:What="Mask/RangeMask"><crs:CorrectionRangeMask crs:Type="2" crs:LumRange="0.6 0.85 1 1"/></rdf:li>
   </rdf:Seq></crs:CorrectionMasks>
  </rdf:Description></rdf:li>
 </rdf:Seq></crs:MaskGroupBasedCorrections>
</rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let d = lcc_meta::parse_xmp(x).unwrap();
        let (partial, unmapped) = crate::crs::to_partial_report(&d.properties, Some(&d.values), crate::crs::Target::Any, 1.5);
        let s = DevelopSettings::default().merged(&partial).expect("valid settings");
        let MaskShape::LuminanceRange { hi, hi_feather, .. } = &s.masks[0].components[0].shape else { panic!() };
        assert_eq!((*hi, *hi_feather), (1.0, 0.0), "brighter than scene white is still in the range");
        assert_eq!(s.masks[0].adjust.clarity, -30.0);
        assert_eq!(unmapped, ["Mask: LocalGrain"], "a refine saturation of 100 is neutral, not reported");
    }

    #[test]
    fn local_point_color_and_curve_import_into_the_mask() {
        // written for this test: a range correction with a bent master curve and one Point Color sample
        let sample = "0.558493, 0.255117, 0.846338, 0.000000, -1.000000, 0.000000, 0.500000, 0.000000, 0.333333, 0.666667, 1.000000, 0.000000, 0.075117, 0.435117, 0.985117, 0.380000, 0.930000, 1.000000, 1.000000";
        let x = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
 <crs:MaskGroupBasedCorrections><rdf:Seq>
  <rdf:li><rdf:Description crs:What="Correction" crs:LocalExposure2012="0.2">
   <crs:CorrectionMasks><rdf:Seq>
    <rdf:li crs:What="Mask/RangeMask"><crs:CorrectionRangeMask crs:Type="2" crs:LumRange="0.2 0.4 0.6 0.8"/></rdf:li>
   </rdf:Seq></crs:CorrectionMasks>
   <crs:MainCurve><rdf:Seq><rdf:li>0, 0</rdf:li><rdf:li>128, 160</rdf:li><rdf:li>255, 255</rdf:li></rdf:Seq></crs:MainCurve>
   <crs:RedCurve><rdf:Seq><rdf:li>0, 0</rdf:li><rdf:li>255, 255</rdf:li></rdf:Seq></crs:RedCurve>
   <crs:LocalPointColors><rdf:Seq><rdf:li>{sample}</rdf:li><rdf:li>nonsense</rdf:li></rdf:Seq></crs:LocalPointColors>
   <crs:LocalColorVariance><rdf:Seq><rdf:li>-25</rdf:li></rdf:Seq></crs:LocalColorVariance>
  </rdf:Description></rdf:li>
 </rdf:Seq></crs:MaskGroupBasedCorrections>
</rdf:Description></rdf:RDF></x:xmpmeta>"#
        );
        let d = lcc_meta::parse_xmp(&x).unwrap();
        let (partial, unmapped) = crate::crs::to_partial_report(&d.properties, Some(&d.values), crate::crs::Target::Any, 1.5);
        let s = DevelopSettings::default().merged(&partial).expect("valid settings");
        let m = &s.masks[0];
        assert_eq!(m.curve.master.len(), 3, "the bent master curve");
        assert!(m.curve.red.is_empty(), "a straight channel stays empty");
        assert_eq!(m.point_colors.len(), 1, "the unreadable slot is skipped");
        assert_eq!(m.point_colors[0].sat_shift, -100.0);
        assert_eq!(m.point_colors[0].variance, -25.0);
        assert!(!unmapped.iter().any(|u| u.contains("Point") || u.contains("Curve")), "{unmapped:?}");
    }

    #[test]
    fn lrtemplate_corrections() {
        let t = r#"s = { title = "Grad", value = { settings = { Exposure2012 = 0.1, GradientBasedCorrections = {
            { What = "Correction", CorrectionAmount = 1, LocalExposure2012 = -0.5, CorrectionMasks = {
                { What = "Mask/Gradient", ZeroX = 0.5, ZeroY = 0.5, FullX = 0.5, FullY = 0, MaskValue = 1 } } } } } } }"#;
        let v = crate::preset_import::read_presets("Grad.lrtemplate", t.as_bytes(), None).unwrap();
        assert!(v[0].unmapped.is_empty(), "{:?}", v[0].unmapped);
        let s = DevelopSettings::default().merged(&v[0].preset.settings).unwrap();
        assert_eq!(s.masks.len(), 1);
        assert_eq!(s.masks[0].adjust.exposure, -2.0);
    }

    #[test]
    fn presets_add_masks_once() {
        let (s, _) = settings();
        let partial = json!({"masks": serde_json::to_value(&s.masks).unwrap(), "light": {"exposure": 0.5}});
        let p = lcc_develop::Preset { id: "t".into(), name: "t".into(), group: "g".into(), settings: partial, favorite: false, builtin: false };
        let mut mine = DevelopSettings::default();
        mine.masks.push(lcc_develop::Mask { id: 7, name: "Mine".into(), ..Default::default() });
        let once = p.apply(&mine, 1.0);
        assert_eq!(once.masks.len(), 4, "added to the photo's own mask");
        assert_eq!(once.masks[1].id, 8, "fresh ids");
        assert_eq!(p.apply(&once, 1.0).masks.len(), 4, "applying again adds nothing");
        let half = p.apply(&mine, 0.5);
        assert_eq!(half.masks[1].adjust.amount, 50.0, "amount scales the masks");
    }

    #[test]
    fn sidecar_radials_fit_the_photo() {
        let mut v = json!({"masks": [{"components": [{"shape": {"kind": "radial", "rx": 0.3, "ry": 0.2}}]}]});
        refit_radials(&mut v, 1.5, 1.0);
        let s = &v["masks"][0]["components"][0]["shape"];
        assert!((s["rx"].as_f64().unwrap() - 0.3).abs() < 1e-9);
        assert!((s["ry"].as_f64().unwrap() - 0.3).abs() < 1e-9, "square photo: y radius = its box half-height");
    }

    /// One radial correction in `container`, with `attrs` on its mask component (values written
    /// for this test; the component attributes follow what Lightroom Classic writes).
    fn radial_packet(container: &str, attrs: &str) -> String {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="0">
 <crs:{container}><rdf:Seq>
  <rdf:li><rdf:Description crs:What="Correction" crs:CorrectionAmount="1" crs:LocalExposure2012="0.25">
   <crs:CorrectionMasks><rdf:Seq>
    <rdf:li crs:What="Mask/CircularGradient" crs:MaskValue="1" crs:Top="0.25" crs:Left="0.25" crs:Bottom="0.75" crs:Right="0.75" crs:Angle="0" crs:Midpoint="50" crs:Roundness="0" {attrs}/>
   </rdf:Seq></crs:CorrectionMasks>
  </rdf:Description></rdf:li>
 </rdf:Seq></crs:{container}>
</rdf:Description></rdf:RDF></x:xmpmeta>"#
        )
    }

    fn radial_settings(container: &str, attrs: &str) -> DevelopSettings {
        let d = lcc_meta::parse_xmp(&radial_packet(container, attrs)).unwrap();
        let (partial, unmapped) = crate::crs::to_partial_report(&d.properties, Some(&d.values), crate::crs::Target::Any, 1.5);
        assert!(unmapped.is_empty(), "{unmapped:?}");
        let s = DevelopSettings::default().merged(&partial).unwrap();
        assert_eq!(s.masks.len(), 1, "{:?}", s.masks);
        s
    }

    /// (the radial's `invert`, its feather, the component's `invert`).
    fn radial_of(s: &DevelopSettings) -> (bool, f64, bool) {
        let c = &s.masks[0].components[0];
        let MaskShape::Radial { invert, feather, .. } = &c.shape else { panic!("{:?}", c.shape) };
        (*invert, *feather, c.invert)
    }

    /// How much brighter the mask makes the centre and a corner of a flat grey frame (8-bit green).
    fn lift_inside_outside(s: &DevelopSettings) -> (i32, i32) {
        let img = lcc_raster::Image { width: 90, height: 60, data: vec![[0.18f32; 3]; 90 * 60] };
        let render = |d: &DevelopSettings| lcc_pipeline::render(&img, &Default::default(), d, &lcc_pipeline::RenderRequest::fit(90, 60)).image;
        let (with, without) = (render(s), render(&DevelopSettings { light: s.light, ..Default::default() }));
        let g = |im: &lcc_raster::Rgba8, x: usize, y: usize| i32::from(im.data[y * im.width + x][1]);
        (g(&with, 45, 30) - g(&without, 45, 30), g(&with, 3, 3) - g(&without, 3, 3))
    }

    #[test]
    fn lightroom_radial_flipped_is_inside() {
        // as Lightroom Classic writes an ordinary radial mask today (#1815)
        let s = radial_settings("MaskGroupBasedCorrections", r#"crs:MaskInverted="false" crs:Feather="25" crs:Flipped="true" crs:Version="2""#);
        assert_eq!(radial_of(&s), (false, 25.0, false));
        let (inside, outside) = lift_inside_outside(&s);
        assert!(inside > 20 && outside == 0, "the effect is inside the circle: {inside} {outside}");
        // no MaskInverted at all reads the same
        let s = radial_settings("MaskGroupBasedCorrections", r#"crs:Feather="25" crs:Flipped="true" crs:Version="2""#);
        assert_eq!(radial_of(&s), (false, 25.0, false));
    }

    #[test]
    fn lightroom_radial_mask_inverted_is_outside() {
        let s = radial_settings("MaskGroupBasedCorrections", r#"crs:MaskInverted="true" crs:Feather="25" crs:Flipped="true" crs:Version="2""#);
        assert_eq!(radial_of(&s), (false, 25.0, true), "the inversion is the component's");
        let (inside, outside) = lift_inside_outside(&s);
        assert!(inside == 0 && outside > 20, "the effect is outside the circle: {inside} {outside}");
        // version 2 with Flipped="false": outside, the legacy sense of the flag
        let s = radial_settings("MaskGroupBasedCorrections", r#"crs:MaskInverted="false" crs:Feather="25" crs:Flipped="false" crs:Version="2""#);
        assert_eq!(radial_of(&s), (true, 25.0, false));
    }

    /// Lightroom Classic 15.6 (Camera Raw 18.7) writes an inverted radial with
    /// `MaskInverted="true"` and `Flipped="false"`. Both read as "outside" cancelled out, so the mask
    /// imported not inverted; the inversion is the component's, and the radial stays "inside".
    #[test]
    fn lightroom_15_inverted_radial_with_flipped_false_is_outside() {
        // the reporter's mask, as Lightroom 15.6 saved it to the XMP
        let s = radial_settings(
            "MaskGroupBasedCorrections",
            r#"crs:MaskInverted="true" crs:Midpoint="50" crs:Roundness="0" crs:Feather="38" crs:Flipped="false" crs:Version="2""#,
        );
        assert_eq!(radial_of(&s), (false, 38.0, true), "the inversion is the component's alone");
        let (inside, outside) = lift_inside_outside(&s);
        assert!(inside == 0 && outside > 20, "the effect is outside the circle: {inside} {outside}");
    }

    #[test]
    fn versionless_group_radial_is_inside_with_half_the_feather() {
        // Lightroom renders both inside and rewrites them Flipped="true", Feather halved, Version="2"
        for flipped in ["true", "false"] {
            let s = radial_settings("MaskGroupBasedCorrections", &format!(r#"crs:MaskInverted="false" crs:Feather="50" crs:Flipped="{flipped}""#));
            assert_eq!(radial_of(&s), (false, 25.0, false), "Flipped={flipped}");
            let (inside, outside) = lift_inside_outside(&s);
            assert!(inside > 20 && outside == 0, "Flipped={flipped}: {inside} {outside}");
        }
    }

    #[test]
    fn legacy_radial_filter_is_outside_unless_flipped() {
        // the process-2012 Radial Filter: effect outside by default, "Invert Mask" = Flipped="true"
        let s = radial_settings("CircularGradientBasedCorrections", r#"crs:Feather="50" crs:Flipped="false""#);
        assert_eq!(radial_of(&s), (true, 50.0, false));
        let (inside, outside) = lift_inside_outside(&s);
        assert!(inside == 0 && outside > 20, "default: outside: {inside} {outside}");
        assert_eq!(radial_of(&radial_settings("CircularGradientBasedCorrections", r#"crs:Feather="50""#)), (true, 50.0, false));
        let s = radial_settings("CircularGradientBasedCorrections", r#"crs:Feather="50" crs:Flipped="true""#);
        assert_eq!(radial_of(&s), (false, 50.0, false));
        let (inside, outside) = lift_inside_outside(&s);
        assert!(inside > 20 && outside == 0, "inverted: inside: {inside} {outside}");
    }

    #[test]
    fn lrtemplate_radial_filter_reads_flipped() {
        // .lrtemplate presets and catalog develop settings are the same Lua tables
        let t = |flipped: &str| {
            format!(
                r#"s = {{ title = "Rad", value = {{ settings = {{ CircularGradientBasedCorrections = {{
            {{ What = "Correction", CorrectionAmount = 1, LocalExposure2012 = 0.25, CorrectionMasks = {{
                {{ What = "Mask/CircularGradient", Top = 0.25, Left = 0.25, Bottom = 0.75, Right = 0.75, Angle = 0, Feather = 50, Flipped = {flipped}, MaskValue = 1 }} }} }} }} }} }} }}"#
            )
        };
        for (flipped, outside) in [("true", false), ("false", true)] {
            let v = crate::preset_import::read_presets("Rad.lrtemplate", t(flipped).as_bytes(), None).unwrap();
            let s = DevelopSettings::default().merged(&v[0].preset.settings).unwrap();
            assert_eq!(radial_of(&s).0, outside, "Flipped = {flipped}");
        }
    }

    /// Spot removal in the shape Lightroom Classic writes it (values written for this test):
    /// a healed circle with an explicit source, a cloned circle with an automatic source, a
    /// brushed removal, a circular-gradient spot, one without a mask, and the legacy list.
    const SPOTS: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="0.1">
 <crs:RetouchInfo><rdf:Seq>
  <rdf:li>centerX = 0.4, centerY = 0.3, radius = 0.01, sourceState = sourceSetExplicitly, sourceX = 0.45, sourceY = 0.3, spotType = heal</rdf:li>
 </rdf:Seq></crs:RetouchInfo>
 <crs:RetouchAreas><rdf:Seq>
  <rdf:li><rdf:Description crs:SpotType="heal" crs:SourceState="sourceSetExplicitly" crs:Method="poisson" crs:SourceX="0.45" crs:OffsetY="0.3" crs:Opacity="1" crs:Feather="0" crs:Seed="1">
   <crs:Masks><rdf:Seq><rdf:li crs:What="Mask/Ellipse" crs:MaskActive="true" crs:MaskValue="1" crs:X="0.4" crs:Y="0.3" crs:SizeX="0.01" crs:SizeY="0.015"/></rdf:Seq></crs:Masks>
  </rdf:Description></rdf:li>
  <rdf:li><rdf:Description crs:SpotType="clone" crs:SourceState="sourceAutoComputed" crs:Opacity="0.5" crs:Feather="0.25">
   <crs:Masks><rdf:Seq><rdf:li crs:What="Mask/Ellipse" crs:X="0.7" crs:Y="0.6" crs:SizeX="0.02" crs:SizeY="0.02"/></rdf:Seq></crs:Masks>
  </rdf:Description></rdf:li>
  <rdf:li><rdf:Description crs:SpotType="heal" crs:SourceState="sourceSetExplicitly" crs:SourceX="0.2" crs:OffsetY="0.9">
   <crs:Masks><rdf:Seq><rdf:li crs:What="Mask/Paint" crs:Radius="0.03" crs:MaskValue="1">
    <crs:Dabs><rdf:Seq><rdf:li>d 0.1 0.8</rdf:li><rdf:li>d 0.12 0.82</rdf:li><rdf:li>d 0.14 0.84</rdf:li></rdf:Seq></crs:Dabs></rdf:li></rdf:Seq></crs:Masks>
  </rdf:Description></rdf:li>
  <rdf:li><rdf:Description crs:SpotType="heal" crs:SourceX="0.6" crs:OffsetY="0.5">
   <crs:Masks><rdf:Seq><rdf:li crs:What="Mask/CircularGradient" crs:Top="0.48" crs:Left="0.49" crs:Bottom="0.52" crs:Right="0.51"/></rdf:Seq></crs:Masks>
  </rdf:Description></rdf:li>
  <rdf:li><rdf:Description crs:SpotType="heal">
   <crs:Masks><rdf:Seq><rdf:li crs:What="Mask/Gradient" crs:FullX="0" crs:FullY="0" crs:ZeroX="1" crs:ZeroY="1"/></rdf:Seq></crs:Masks>
  </rdf:Description></rdf:li>
 </rdf:Seq></crs:RetouchAreas>
</rdf:Description></rdf:RDF></x:xmpmeta>"#;

    #[test]
    fn retouch_areas_become_spots() {
        use lcc_develop::SpotMode;
        let d = lcc_meta::parse_xmp(SPOTS).unwrap();
        let (partial, unmapped) = crate::crs::to_partial_report(&d.properties, Some(&d.values), crate::crs::Target::Any, 1.5);
        let s = DevelopSettings::default().merged(&partial).expect("valid settings");
        assert_eq!(s.light.exposure, 0.1, "global settings still map");
        assert_eq!(s.spots.len(), 4, "the gradient 'spot' is skipped: {:?}", s.spots);
        let heal = &s.spots[0];
        assert_eq!((heal.mode, heal.points.len(), heal.opacity, heal.feather), (SpotMode::Heal, 1, 100.0, 0.0));
        assert!((heal.points[0].x - 0.4).abs() < 1e-9 && (heal.points[0].y - 0.3).abs() < 1e-9);
        assert!((heal.size - 0.01).abs() < 1e-9, "a landscape target: width fractions are long-edge fractions");
        let src = heal.source_offset.expect("explicit source");
        assert!((src.x - 0.05).abs() < 1e-9 && src.y.abs() < 1e-9, "source stored absolute, ours an offset: {src:?}");
        let clone = &s.spots[1];
        assert_eq!((clone.mode, clone.opacity, clone.feather, clone.source_offset), (SpotMode::Clone, 50.0, 25.0, None));
        let brushed = &s.spots[2];
        assert_eq!((brushed.points.len(), brushed.size), (3, 0.03));
        let src = brushed.source_offset.expect("explicit source");
        assert!((src.x - 0.1).abs() < 1e-9 && (src.y - 0.1).abs() < 1e-9);
        let circ = &s.spots[3];
        assert!((circ.points[0].x - 0.5).abs() < 1e-9 && (circ.points[0].y - 0.5).abs() < 1e-9 && (circ.size - 0.01).abs() < 1e-9);
        assert_eq!(unmapped, vec!["Spot: Gradient".to_string()], "{unmapped:?}");
    }

    #[test]
    fn portrait_target_scales_spot_sizes() {
        let d = lcc_meta::parse_xmp(SPOTS).unwrap();
        let (partial, _) = crate::crs::to_partial_report(&d.properties, Some(&d.values), crate::crs::Target::Any, 2.0 / 3.0);
        let s = DevelopSettings::default().merged(&partial).unwrap();
        assert!((s.spots[0].size - 0.01 * 2.0 / 3.0).abs() < 1e-9, "a width fraction on a portrait target: {}", s.spots[0].size);
    }

    #[test]
    fn legacy_retouch_info_is_read_on_its_own() {
        let packet = SPOTS.replace("crs:RetouchAreas", "crs:Ignored");
        let d = lcc_meta::parse_xmp(&packet).unwrap();
        let (partial, unmapped) = crate::crs::to_partial_report(&d.properties, Some(&d.values), crate::crs::Target::Any, 1.5);
        let s = DevelopSettings::default().merged(&partial).unwrap();
        assert_eq!(s.spots.len(), 1, "{:?}", s.spots);
        assert_eq!(s.spots[0].mode, lcc_develop::SpotMode::Heal);
        assert!((s.spots[0].points[0].x - 0.4).abs() < 1e-9 && (s.spots[0].size - 0.01).abs() < 1e-9);
        assert!(s.spots[0].source_offset.is_some_and(|o| (o.x - 0.05).abs() < 1e-9));
        assert!(unmapped.iter().any(|u| u == "Ignored"), "{unmapped:?}");
        assert!(!unmapped.iter().any(|u| u == "RetouchInfo"), "{unmapped:?}");
    }

    #[test]
    fn spots_render() {
        let d = lcc_meta::parse_xmp(SPOTS).unwrap();
        let (partial, _) = crate::crs::to_partial_report(&d.properties, Some(&d.values), crate::crs::Target::Any, 1.5);
        let s = DevelopSettings::default().merged(&partial).unwrap();
        let img = lcc_scenes::demo_library()[0].render(96, 64);
        let render = |d: &DevelopSettings| lcc_pipeline::render(&img, &Default::default(), d, &lcc_pipeline::RenderRequest::fit(96, 64)).image.data;
        assert_ne!(render(&DevelopSettings { light: s.light, ..Default::default() }), render(&s), "the spots change the picture");
    }
}

/// Lightroom brush strokes (notes 18/22): the measured Radius / CenterWeight of Lightroom's own strokes come in as
/// its Size and Feather, Auto Mask dabs as Auto Mask, and the strokes render with Lightroom's brush model.
#[cfg(test)]
mod brush_import_tests {
    use lcc_develop::{BRUSH_MODEL_LRC, MaskShape};

    /// One correction holding `paints` (`Mask/Paint` attributes, dab lines).
    fn packet(paints: &[(&str, &[&str])]) -> String {
        let items: String = paints
            .iter()
            .map(|(attrs, dabs)| {
                let lis: String = dabs.iter().map(|d| format!("<rdf:li>{d}</rdf:li>")).collect();
                format!("<rdf:li><rdf:Description crs:What=\"Mask/Paint\" crs:MaskBlendMode=\"0\" {attrs}><crs:Dabs><rdf:Seq>{lis}</rdf:Seq></crs:Dabs></rdf:Description></rdf:li>")
            })
            .collect();
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
   <crs:MaskGroupBasedCorrections><rdf:Seq><rdf:li><rdf:Description crs:What="Correction" crs:CorrectionAmount="1" crs:CorrectionActive="true" crs:LocalExposure2012="-1">
   <crs:CorrectionMasks><rdf:Seq>{items}</rdf:Seq></crs:CorrectionMasks></rdf:Description></rdf:li></rdf:Seq></crs:MaskGroupBasedCorrections>
  </rdf:Description></rdf:RDF></x:xmpmeta>"#
        )
    }

    fn strokes(xmp: &str) -> Vec<lcc_develop::BrushStroke> {
        let d = lcc_meta::parse_xmp(xmp).expect("well-formed XMP");
        let (masks, skipped) = super::masks(&d.values, 1.5);
        assert!(skipped.is_empty(), "{skipped:?}");
        let mask: lcc_develop::Mask = serde_json::from_value(masks[0].clone()).expect("a mask");
        assert_eq!(mask.components.len(), 1, "one brush component");
        let MaskShape::Brush { strokes } = &mask.components[0].shape else { panic!("a brush") };
        strokes.clone()
    }

    // The single clicks Lightroom saved at Size 20 (Feather 0 / 20 / 50 / 100: Radius and CenterWeight as
    // measured) all come back with the same Size, the core, and their Feather; before, CenterWeight 0 (Feather
    // 100) arrived as a hard Feather 0 brush and Radius as the size
    #[test]
    fn lightroom_radius_and_center_weight_become_size_and_feather() {
        let xmp = packet(&[
            ("crs:MaskValue=\"1\" crs:Radius=\"0.054416\" crs:Flow=\"1\" crs:CenterWeight=\"1\"", &["d 0.2 0.5"]),
            ("crs:MaskValue=\"1\" crs:Radius=\"0.059840\" crs:Flow=\"1\" crs:CenterWeight=\"0.501304\"", &["d 0.4 0.5"]),
            ("crs:MaskValue=\"1\" crs:Radius=\"0.104394\" crs:Flow=\"0.5\" crs:CenterWeight=\"0.105656\"", &["d 0.6 0.5"]),
            ("crs:MaskValue=\"0.8\" crs:Radius=\"0.135599\" crs:Flow=\"1\" crs:CenterWeight=\"0\"", &["d 0.8 0.5"]),
        ]);
        let s = strokes(&xmp);
        let got: Vec<(f64, f64)> = s.iter().map(|s| (s.size, s.feather)).collect();
        for ((size, feather), want) in got.iter().zip([0.0, 20.0, 60.0, 100.0]) {
            assert!((size - 0.0544).abs() < 0.0005, "{got:?}");
            assert!((feather - want).abs() < 0.5, "{got:?}");
        }
        assert!(s.iter().all(|s| s.model == BRUSH_MODEL_LRC && !s.auto_mask && !s.erase));
        // Lightroom's strokes paint with the default Auto Mask range (note 1, 2026-10-31)
        assert!(s.iter().all(|s| s.auto_range == 0.0));
        assert_eq!((s[2].flow, s[3].density), (50.0, 80.0));
    }

    // `M` dabs are Auto Mask dabs; `r` / `h` lines (radius or CenterWeight changing mid-stroke) are not dabs;
    // a stroke mixing `d` and `M` becomes one stroke per run; an eraser saved at 0 erases fully
    #[test]
    fn auto_mask_dabs_and_other_dab_lines() {
        let xmp = packet(&[
            (
                "crs:MaskValue=\"1\" crs:Radius=\"0.137885\" crs:Flow=\"1\" crs:CenterWeight=\"0\"",
                &["M 0.1 0.2", "M 0.1 0.25", "h 0.5013", "r 0.05", "d 0.1 0.3", "d 0.1 0.35"],
            ),
            ("crs:MaskValue=\"0\" crs:Radius=\"0.05\" crs:Flow=\"1\" crs:CenterWeight=\"1\"", &["d 0.5 0.5", "x 1 2", "d nan 3"]),
        ]);
        let s = strokes(&xmp);
        assert_eq!(s.len(), 3, "{s:?}");
        assert_eq!((s[0].auto_mask, s[0].points.len()), (true, 2));
        assert_eq!((s[1].auto_mask, s[1].points.len()), (false, 2));
        assert_eq!((s[2].erase, s[2].density, s[2].points.len()), (true, 100.0, 1));
    }
}
