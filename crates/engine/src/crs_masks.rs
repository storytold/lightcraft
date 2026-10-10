//! Interchange: local corrections (masks) stored as `crs:` structures in XMP sidecars, XMP
//! presets and `.lrtemplate` files → our [`Mask`](lightcraft_develop::Mask)s.
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

use lightcraft_meta::XmpValue;
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

fn adjust(c: &XmpValue) -> Value {
    let mut a = serde_json::Map::new();
    for (crs, ours, k) in LOCAL {
        if a.contains_key(*ours) {
            continue;
        }
        if let Some(v) = num(c, &format!("crs:{crs}")) {
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
    let amount = num(c, "crs:CorrectionAmount").unwrap_or(1.0);
    a.insert("amount".into(), json!((amount * 100.0).clamp(0.0, 200.0)));
    Value::Object(a)
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

/// One mask component → our component JSON; `Err(kind)` names an unsupported one.
fn component(m: &XmpValue, aspect: f64) -> Result<Value, String> {
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
                "feather": g("Feather", 50.0).clamp(0.0, 100.0),
                "invert": flag(m, "crs:Flipped").unwrap_or(false),
            })
        }
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
                return Err("empty brush".into());
            }
            let erase = num(m, "crs:MaskValue").is_some_and(|v| v <= 0.0);
            json!({"kind": "brush", "strokes": [{
                "points": pts,
                "size": num(m, "crs:Radius").unwrap_or(0.03).clamp(0.001, 1.0),
                "feather": (num(m, "crs:CenterWeight").unwrap_or(0.5) * 100.0).clamp(0.0, 100.0),
                "flow": (num(m, "crs:Flow").unwrap_or(1.0) * 100.0).clamp(0.0, 100.0),
                "density": (num(m, "crs:MaskValue").unwrap_or(1.0).abs() * 100.0).clamp(0.0, 100.0),
                "erase": erase,
            }]})
        }
        "Mask/Image" => match text(m, "crs:MaskSubType") {
            Some("1") => json!({"kind": "subject"}),
            Some("2") => json!({"kind": "sky"}),
            other => return Err(format!("AI mask ({})", other.unwrap_or("?"))),
        },
        "Mask/RangeMask" => {
            let r = m.field("crs:CorrectionRangeMask").unwrap_or(m);
            match text(r, "crs:Type") {
                Some("2") => {
                    let q: Vec<f64> = text(r, "crs:LumRange").unwrap_or("0 0 1 1").split_whitespace().filter_map(|v| v.parse().ok()).collect();
                    let [a, b, c, d] = q[..] else { return Err("luminance range".into()) };
                    let (a, b, c, d) = (lightness_to_range(a), lightness_to_range(b), lightness_to_range(c), lightness_to_range(d));
                    json!({"kind": "luminanceRange", "lo": b, "hi": c, "lo_feather": (b - a).max(0.0), "hi_feather": (d - c).max(0.0)})
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
                match component(m, aspect) {
                    Ok(v) => parts.push(v),
                    Err(kind) => skipped.push(kind),
                }
            }
            if parts.is_empty() {
                continue;
            }
            // brush dabs of one correction are one brush component
            let mut merged: Vec<Value> = Vec::new();
            for p in parts {
                let is_brush = p["shape"]["kind"] == "brush";
                if is_brush
                    && let Some(prev) = merged.iter_mut().find(|q| q["shape"]["kind"] == "brush" && q["op"] == p["op"])
                    && let (Some(a), Some(b)) = (prev["shape"]["strokes"].as_array_mut(), p["shape"]["strokes"].as_array())
                {
                    a.extend(b.iter().cloned());
                    continue;
                }
                merged.push(p);
            }
            let n = out.len() + 1;
            let name = text(c, "crs:CorrectionName").map(str::to_string).unwrap_or_else(|| format!("Mask {n}"));
            out.push(json!({"id": n, "name": name, "visible": true, "invert": false, "components": merged, "adjust": adjust(c)}));
        }
    }
    skipped.sort();
    skipped.dedup();
    (out, skipped)
}

/// The containers spot removal is stored in: `RetouchAreas` (current, one struct per spot with a
/// mask component) and `RetouchInfo` (older, one `key = value, …` line per spot; still written
/// beside `RetouchAreas` for older readers and read on its own when that is all there is).
pub const SPOT_CONTAINERS: [&str; 2] = ["crs:RetouchAreas", "crs:RetouchInfo"];

/// One `RetouchAreas` item → a [`Spot`](lightcraft_develop::Spot) as JSON. `aspect` = width /
/// height of the target: horizontal sizes are stored as fractions of the width, ours of the long
/// edge. Returns the component kind it can't carry over.
fn spot(item: &XmpValue, aspect: f64) -> Result<Value, String> {
    let mode = match text(item, "crs:SpotType") {
        Some("clone") => "clone",
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
    use lightcraft_develop::{DevelopSettings, MaskOp, MaskShape};

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
    <rdf:li crs:What="Mask/CircularGradient" crs:Top="0.3" crs:Left="0.2" crs:Bottom="0.7" crs:Right="0.8" crs:Angle="10" crs:Feather="40" crs:Flipped="true"/>
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
        let d = lightcraft_meta::parse_xmp(PACKET).unwrap();
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
        assert_eq!((*angle, *feather, *invert), (10.0, 40.0, true));
        let MaskShape::LuminanceRange { lo, hi, lo_feather, .. } = &rad.components[1].shape else { panic!() };
        assert!(lo < hi && *lo_feather > 0.0);
        let brush = &s.masks[2];
        assert_eq!(brush.adjust.clarity, 40.0);
        assert_eq!(brush.components.len(), 1, "the brush's dab runs are one component");
        let MaskShape::Brush { strokes } = &brush.components[0].shape else { panic!() };
        assert_eq!((strokes.len(), strokes[0].points.len(), strokes[0].flow, strokes[0].feather), (2, 2, 80.0, 30.0));
        assert_eq!(unmapped, vec!["Mask: AI mask (9)".to_string()]);
    }

    #[test]
    fn masks_render() {
        let (s, _) = settings();
        let img = lightcraft_scenes::demo_library()[0].render(96, 64);
        let render = |d: &DevelopSettings| {
            lightcraft_pipeline::render(&img, &Default::default(), d, &lightcraft_pipeline::RenderRequest::fit(96, 64)).image.data
        };
        assert_ne!(render(&DevelopSettings { light: s.light, ..Default::default() }), render(&s), "the masks change the picture");
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
        let p =
            lightcraft_develop::Preset { id: "t".into(), name: "t".into(), group: "g".into(), settings: partial, favorite: false, builtin: false };
        let mut mine = DevelopSettings::default();
        mine.masks.push(lightcraft_develop::Mask { id: 7, name: "Mine".into(), ..Default::default() });
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
        use lightcraft_develop::SpotMode;
        let d = lightcraft_meta::parse_xmp(SPOTS).unwrap();
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
        let d = lightcraft_meta::parse_xmp(SPOTS).unwrap();
        let (partial, _) = crate::crs::to_partial_report(&d.properties, Some(&d.values), crate::crs::Target::Any, 2.0 / 3.0);
        let s = DevelopSettings::default().merged(&partial).unwrap();
        assert!((s.spots[0].size - 0.01 * 2.0 / 3.0).abs() < 1e-9, "a width fraction on a portrait target: {}", s.spots[0].size);
    }

    #[test]
    fn legacy_retouch_info_is_read_on_its_own() {
        let packet = SPOTS.replace("crs:RetouchAreas", "crs:Ignored");
        let d = lightcraft_meta::parse_xmp(&packet).unwrap();
        let (partial, unmapped) = crate::crs::to_partial_report(&d.properties, Some(&d.values), crate::crs::Target::Any, 1.5);
        let s = DevelopSettings::default().merged(&partial).unwrap();
        assert_eq!(s.spots.len(), 1, "{:?}", s.spots);
        assert_eq!(s.spots[0].mode, lightcraft_develop::SpotMode::Heal);
        assert!((s.spots[0].points[0].x - 0.4).abs() < 1e-9 && (s.spots[0].size - 0.01).abs() < 1e-9);
        assert!(s.spots[0].source_offset.is_some_and(|o| (o.x - 0.05).abs() < 1e-9));
        assert!(unmapped.iter().any(|u| u == "Ignored"), "{unmapped:?}");
        assert!(!unmapped.iter().any(|u| u == "RetouchInfo"), "{unmapped:?}");
    }

    #[test]
    fn spots_render() {
        let d = lightcraft_meta::parse_xmp(SPOTS).unwrap();
        let (partial, _) = crate::crs::to_partial_report(&d.properties, Some(&d.values), crate::crs::Target::Any, 1.5);
        let s = DevelopSettings::default().merged(&partial).unwrap();
        let img = lightcraft_scenes::demo_library()[0].render(96, 64);
        let render = |d: &DevelopSettings| {
            lightcraft_pipeline::render(&img, &Default::default(), d, &lightcraft_pipeline::RenderRequest::fit(96, 64)).image.data
        };
        assert_ne!(render(&DevelopSettings { light: s.light, ..Default::default() }), render(&s), "the spots change the picture");
    }
}
