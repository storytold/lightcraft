//! Write presets as Lightroom-style XMP preset files (`preset.exportXmp`), masks included:
//! Subject, Sky, Background and People parts become Lightroom AI masks (`Mask/Image`, which
//! Lightroom recomputes on each photo, as we do), gradients `Mask/Gradient` /
//! `Mask/CircularGradient`, luminance ranges `Mask/RangeMask`, brush strokes `Mask/Paint`, with the `Local*` sliders, the
//! mask's Curve (`MainCurve`, `RedCurve`, …) and Point Color (`LocalPointColors`).
//!
//! What Lightroom has no equivalent for — People by name or role, anchored gradients, Landscape,
//! colour ranges, object selections, and the global settings (no writer for Lightroom's
//! global `crs:` fields yet) — is listed in the report, and the whole preset is kept in our own
//! namespace (`lc:PresetSettings`, JSON), which Lightroom ignores and we read back: an exported
//! preset imports unchanged.
//!
//! Ported from OneSlips' fork (own design there).

use lcc_develop::{LocalAdjustments, Mask, MaskOp, MaskShape, PointCurves, Preset};
use serde_json::Value;

/// The property holding our own preset in an exported file.
pub const LC_SETTINGS: &str = "lc:PresetSettings";

/// A mask Hue past Lightroom's range, whole, in degrees (only then written; `crs:LocalHue` holds it
/// clamped). Read back in place of `crs:LocalHue`.
pub const LC_LOCAL_HUE: &str = "lc:LocalHueDegrees";

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            c => o.push(c),
        }
    }
    o
}

/// A stable 32-hex-digit id from `seed` (Lightroom's UUIDs and sync ids).
fn hex_id(seed: &str) -> String {
    let mut a: u64 = 0xcbf2_9ce4_8422_2325;
    let mut b: u64 = 0x8422_2325_cbf2_9ce4;
    for byte in seed.bytes() {
        a = (a ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        b = (b ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3).rotate_left(17);
    }
    format!("{a:016X}{b:016X}")
}

fn f(v: f64) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    format!("{v:.6}")
}

/// Lightroom's People part ids (`MaskSubCategoryID`) we know.
fn people_part_id(part: &str) -> Option<u32> {
    Some(match part.to_ascii_lowercase().as_str() {
        "face skin" | "faceskin" => 2,
        "iris and pupil" | "iris" => 3,
        "body skin" | "bodyskin" => 4,
        "hair" => 5,
        "lips" => 6,
        "eye sclera" | "sclera" => 8,
        "clothes" | "clothing" => 11,
        "teeth" => 12,
        _ => return None,
    })
}

/// Our 0..1 range scale (log luminance on −8..+4 EV) → Lightroom's lightness 0..1.
fn range_to_lightness(r: f64) -> f64 {
    let y = (r.clamp(0.0, 1.0) * 12.0 - 8.0).exp2();
    y.powf(1.0 / 2.2).clamp(0.0, 1.0)
}

/// The `Local*` attributes of a mask's adjustments (the importer's table, read backwards).
fn local_attrs(a: &LocalAdjustments) -> String {
    let ours = serde_json::to_value(a).unwrap_or_default();
    let mut s = String::new();
    let mut done: Vec<&str> = Vec::new();
    for (crs, field, scale) in crate::crs_masks::local_fields() {
        if done.contains(&field) {
            continue;
        }
        done.push(field);
        let mut v = ours.get(field).and_then(Value::as_f64).unwrap_or(0.0);
        if field == "hue" {
            // degrees → Lightroom's −100..100 scale (about ±34°); a wider shift is clamped there and
            // written whole in our own namespace, which only a value past the standard range gets
            v = (a.hue / lcc_develop::LEGACY_LOCAL_HUE_DEG).clamp(-100.0, 100.0);
            if a.hue.is_finite() && a.hue.abs() > lcc_develop::LOCAL_HUE_STANDARD + 1e-9 {
                let deg = a.hue.clamp(-lcc_develop::LOCAL_HUE_MAX, lcc_develop::LOCAL_HUE_MAX);
                s.push_str(&format!("\n       {LC_LOCAL_HUE}=\"{}\"", f(deg)));
            }
        }
        s.push_str(&format!("\n       crs:{crs}=\"{}\"", f(v / scale)));
    }
    s
}

/// A local curve channel as Lightroom's `"x, y"` points on 0..255 (`None` while straight).
fn curve_seq(tag: &str, pts: &[lcc_geom::Point]) -> Option<String> {
    if pts.len() < 2 {
        return None;
    }
    let items: Vec<String> = pts
        .iter()
        .take(64)
        .map(|p| {
            let c = |v: f64| if v.is_finite() { (v.clamp(0.0, 1.0) * 255.0).round() } else { 0.0 };
            format!("\n         <rdf:li>{}, {}</rdf:li>", c(p.x), c(p.y))
        })
        .collect();
    Some(format!("\n      <crs:{tag}>\n       <rdf:Seq>{}\n       </rdf:Seq>\n      </crs:{tag}>", items.concat()))
}

/// The mask's Curve and Point Color as child elements of its correction.
fn local_children(m: &Mask) -> String {
    let PointCurves { master, red, green, blue } = &m.curve;
    let mut s: String = [("MainCurve", master), ("RedCurve", red), ("GreenCurve", green), ("BlueCurve", blue)]
        .into_iter()
        .filter_map(|(tag, pts)| curve_seq(tag, pts))
        .collect();
    if !m.point_colors.is_empty() {
        let (lines, vars): (Vec<String>, Vec<String>) = m
            .point_colors
            .iter()
            .map(|p| {
                let (lr, var) = crate::crs_point_color::from_ours(p);
                (format!("\n         <rdf:li>{}</rdf:li>", crate::crs_point_color::format(&lr)), format!("\n         <rdf:li>{}</rdf:li>", f(var)))
            })
            .unzip();
        s.push_str(&format!("\n      <crs:LocalPointColors>\n       <rdf:Seq>{}\n       </rdf:Seq>\n      </crs:LocalPointColors>", lines.concat()));
        s.push_str(&format!(
            "\n      <crs:LocalColorVariance>\n       <rdf:Seq>{}\n       </rdf:Seq>\n      </crs:LocalColorVariance>",
            vars.concat()
        ));
    }
    s
}

/// One mask component as a `CorrectionMasks` item; `Err` names what Lightroom can't hold.
fn component(m: &Mask, k: usize, aspect: f64, notes: &mut Vec<String>) -> Option<String> {
    let c = m.components.get(k)?;
    let (mode, inverted) = match c.op {
        MaskOp::Add => (0, c.invert),
        MaskOp::Subtract => (1, c.invert),
        // Lightroom writes "intersect with" as subtracting the inverted component
        MaskOp::Intersect => (1, !c.invert),
    };
    if c.anchor.is_some() {
        notes.push(format!("{}: the gradient follows the people on each photo here; Lightroom gets it fixed on the photo", m.name));
    }
    let sync = hex_id(&format!("{}-{}-{k}", m.name, m.id));
    let head = |what: &str, name: &str| {
        format!(
            "<rdf:li\n         crs:What=\"{what}\"\n         crs:MaskActive=\"true\"\n         crs:MaskName=\"{}\"\n         crs:MaskBlendMode=\"{mode}\"\n         crs:MaskInverted=\"{}\"\n         crs:MaskSyncID=\"{sync}\"\n         crs:MaskValue=\"1\"",
            esc(name),
            if inverted { "true" } else { "false" }
        )
    };
    let image = |sub: u32, cat: Option<u32>, name: &str| {
        let cat = cat.map(|c| format!("\n         crs:MaskSubCategoryID=\"{c}\"")).unwrap_or_default();
        format!(
            "{}\n         crs:MaskVersion=\"1\"\n         crs:MaskSubType=\"{sub}\"{cat}\n         crs:ReferencePoint=\"0.500000 0.500000\"\n         crs:ErrorReason=\"0\"/>",
            head("Mask/Image", name)
        )
    };
    let (sx, sy) = if aspect >= 1.0 { (1.0, 1.0 / aspect) } else { (aspect, 1.0) };
    Some(match &c.shape {
        MaskShape::Subject => image(1, None, "Subject 1"),
        MaskShape::Sky => image(2, None, "Sky 1"),
        MaskShape::Background => image(0, Some(22), "Background 1"),
        MaskShape::People { person, parts, who, .. } => {
            if who.is_some() || *person > 0 {
                notes.push(format!(
                    "{}: Lightroom selects every person, not {}",
                    m.name,
                    who.as_ref().map_or_else(|| format!("person {person}"), |w| w.label())
                ));
            }
            let parts: Vec<&String> = if parts.is_empty() { Vec::new() } else { parts.iter().collect() };
            let ids: Vec<(u32, &String)> = parts.iter().filter_map(|p| people_part_id(p).map(|i| (i, *p))).collect();
            if ids.len() != parts.len() || parts.is_empty() {
                notes.push(format!("{}: People parts Lightroom's presets don't name (entire person, eyebrows) are left out", m.name));
            }
            // one Lightroom component per part
            let items: Vec<String> = ids.iter().map(|(i, p)| image(3, Some(*i), p)).collect();
            if items.is_empty() {
                return None;
            }
            items.join("\n        ")
        }
        MaskShape::Linear { start, end } => format!(
            "{}\n         crs:ZeroX=\"{}\"\n         crs:ZeroY=\"{}\"\n         crs:FullX=\"{}\"\n         crs:FullY=\"{}\"/>",
            head("Mask/Gradient", "Linear Gradient 1"),
            f(end.x),
            f(end.y),
            f(start.x),
            f(start.y)
        ),
        MaskShape::Radial { center, rx, ry, angle, feather, invert } => {
            let (hw, hh) = (rx / sx, ry / sy);
            format!(
                "{}\n         crs:Top=\"{}\"\n         crs:Left=\"{}\"\n         crs:Bottom=\"{}\"\n         crs:Right=\"{}\"\n         crs:Angle=\"{}\"\n         crs:Midpoint=\"50\"\n         crs:Roundness=\"0\"\n         crs:Feather=\"{}\"\n         crs:Flipped=\"{}\"/>",
                head("Mask/CircularGradient", "Radial Gradient 1"),
                f(center.y - hh),
                f(center.x - hw),
                f(center.y + hh),
                f(center.x + hw),
                f(*angle),
                f(feather.clamp(0.0, 100.0)),
                if *invert { "true" } else { "false" }
            )
        }
        MaskShape::LuminanceRange { lo, hi, lo_feather, hi_feather } => {
            let q = [lo - lo_feather, *lo, *hi, hi + hi_feather].map(|v| f(range_to_lightness(v)));
            format!(
                "<rdf:li>\n        <rdf:Description\n         crs:What=\"Mask/RangeMask\"\n         crs:MaskActive=\"true\"\n         crs:MaskName=\"Luminance Range 1\"\n         crs:MaskBlendMode=\"{mode}\"\n         crs:MaskInverted=\"{}\"\n         crs:MaskSyncID=\"{sync}\"\n         crs:MaskValue=\"1\">\n        <crs:CorrectionRangeMask\n         crs:Version=\"3\"\n         crs:Type=\"2\"\n         crs:Invert=\"false\"\n         crs:LumRange=\"{} {} {} {}\"/>\n        </rdf:Description>\n        </rdf:li>",
                if inverted { "true" } else { "false" },
                q[0],
                q[1],
                q[2],
                q[3]
            )
        }
        MaskShape::Brush { strokes } => {
            if strokes.iter().any(|s| !s.is_lrc()) {
                notes.push(format!(
                    "{}: brush strokes painted before the app used Lightroom's brush are written in Lightroom's terms and may look a little different there",
                    m.name
                ));
            }
            let items: Vec<String> = strokes.iter().filter_map(|s| paint(s, mode, inverted, &sync, aspect)).collect();
            if items.is_empty() {
                return None;
            }
            items.join("\n        ")
        }
        other => {
            let what = match other {
                MaskShape::Landscape { class, .. } => format!("Landscape ({class})"),
                MaskShape::ColorRange { .. } => "a colour range".into(),
                MaskShape::Prompt { text, .. } => format!("the description “{text}”"),
                MaskShape::Object { .. } => "an object selection".into(),
                MaskShape::DepthRange { .. } => "a depth range".into(),
                _ => "a component".into(),
            };
            notes.push(format!("{}: {what} has no Lightroom equivalent in presets and is left out", m.name));
            return None;
        }
    })
}

/// One brush stroke as a Lightroom `Mask/Paint` (Lightroom's brush model, `lcc_develop::brush_model`):
/// `crs:Radius` where the halo ends (a fraction of the long side), `crs:CenterWeight` from Feather, `crs:Flow`,
/// the density as `crs:MaskValue` (0 for an eraser, as Lightroom reads it), and the dabs Lightroom saves: one
/// every `DAB_SPACING` × Radius along the path, `d x y` (or `M x y`, an Auto Mask dab) in fractions of the
/// width and height. A legacy stroke ([`lcc_develop::BRUSH_MODEL_LEGACY`], its size the whole dab) is
/// written with that size as Radius. `None` for a stroke without points.
fn paint(s: &lcc_develop::BrushStroke, mode: u8, inverted: bool, sync: &str, aspect: f64) -> Option<String> {
    use lcc_develop::brush_model;
    use lcc_geom::Point;
    let feather = s.feather.clamp(0.0, 100.0);
    let radius = if s.is_lrc() { s.size * brush_model::outer_ratio(feather) } else { s.size };
    let radius = if radius.is_finite() { radius.clamp(0.0001, 1.0) } else { 0.03 };
    // the walk on a photo 10 000 units along its long side (it works in pixels), the dabs back in fractions of
    // the width and height
    const LONG: f64 = 10_000.0;
    let (sx, sy) = if aspect >= 1.0 { (LONG, LONG / aspect) } else { (LONG * aspect, LONG) };
    let mut dabs = Vec::new();
    let (mut walk, mut emitted, mut ts) = (lcc_pipeline::masks::DabWalk::default(), 0usize, Vec::new());
    for p in &s.points {
        lcc_pipeline::masks::walk_dabs(
            &mut dabs,
            &mut emitted,
            &mut walk,
            Point::new(p.x * sx, p.y * sy),
            brush_model::DAB_SPACING * radius * LONG,
            &mut ts,
        );
    }
    if dabs.is_empty() {
        return None;
    }
    let kind = if s.auto_mask { "M" } else { "d" };
    let lines: Vec<String> = dabs.iter().map(|d| format!("<rdf:li>{kind} {} {}</rdf:li>", f(d.x / sx), f(d.y / sy))).collect();
    let value = if s.erase { 0.0 } else { (s.density / 100.0).clamp(0.0, 1.0) };
    Some(format!(
        "<rdf:li>\n        <rdf:Description\n         crs:What=\"Mask/Paint\"\n         crs:MaskActive=\"true\"\n         crs:MaskName=\"Brush 1\"\n         crs:MaskBlendMode=\"{mode}\"\n         crs:MaskInverted=\"{}\"\n         crs:MaskSyncID=\"{sync}\"\n         crs:MaskValue=\"{}\"\n         crs:Radius=\"{}\"\n         crs:Flow=\"{}\"\n         crs:CenterWeight=\"{}\">\n        <crs:Dabs>\n         <rdf:Seq>\n          {}\n         </rdf:Seq>\n        </crs:Dabs>\n        </rdf:Description>\n        </rdf:li>",
        if inverted { "true" } else { "false" },
        f(value),
        f(radius),
        f((s.flow / 100.0).clamp(0.0, 1.0)),
        f(brush_model::center_weight(feather)),
        lines.join("\n          ")
    ))
}

/// Preset `p` as a Lightroom XMP preset file, and notes on what Lightroom won't get.
pub fn to_xmp(p: &Preset) -> (String, Vec<String>) {
    let mut notes = Vec::new();
    let aspect = lcc_develop::recipe::RECIPE_ASPECT;
    let masks: Vec<Mask> = p
        .settings
        .get("masks")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|m| serde_json::from_value(m.clone()).ok()).collect())
        .unwrap_or_default();
    if p.settings.as_object().is_some_and(|o| o.keys().any(|k| k != "masks")) {
        notes.push("Global settings are kept for this app only (Lightroom gets the masks)".into());
    }
    let mut corrections = String::new();
    for m in &masks {
        let items: Vec<String> = (0..m.components.len()).filter_map(|k| component(m, k, aspect, &mut notes)).collect();
        if items.is_empty() {
            continue;
        }
        corrections.push_str(&format!(
            "\n     <rdf:li>\n      <rdf:Description\n       crs:What=\"Correction\"\n       crs:CorrectionAmount=\"{}\"\n       crs:CorrectionActive=\"{}\"\n       crs:CorrectionName=\"{}\"\n       crs:CorrectionSyncID=\"{}\"{}>\n      <crs:CorrectionMasks>\n       <rdf:Seq>\n        {}\n       </rdf:Seq>\n      </crs:CorrectionMasks>{}\n      </rdf:Description>\n     </rdf:li>",
            f((m.adjust.amount / 100.0).clamp(0.0, 2.0)),
            if m.visible { "true" } else { "false" },
            esc(&m.name),
            hex_id(&format!("{}-{}", p.id, m.id)),
            local_attrs(&m.adjust),
            items.join("\n        "),
            local_children(m)
        ));
        if m.invert || m.feather > 0.0 || m.refine > 0.0 {
            notes.push(format!("{}: the mask's Invert, Feather and Refine Edges stay here; Lightroom gets the mask without them", m.name));
        }
    }
    let mask_block = if corrections.is_empty() {
        String::new()
    } else {
        format!("\n   <crs:MaskGroupBasedCorrections>\n    <rdf:Seq>{corrections}\n    </rdf:Seq>\n   </crs:MaskGroupBasedCorrections>")
    };
    let lc = serde_json::to_string(&p.settings).unwrap_or_default();
    let alt = |tag: &str, v: &str| {
        format!("\n   <crs:{tag}>\n    <rdf:Alt>\n     <rdf:li xml:lang=\"x-default\">{}</rdf:li>\n    </rdf:Alt>\n   </crs:{tag}>", esc(v))
    };
    let xmp = format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  <rdf:Description rdf:about=\"\"\n    xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\"\n    xmlns:lc=\"{}\"\n   crs:PresetType=\"Normal\"\n   crs:UUID=\"{}\"\n   crs:SupportsAmount2=\"True\"\n   crs:SupportsAmount=\"True\"\n   crs:SupportsColor=\"True\"\n   crs:SupportsMonochrome=\"True\"\n   crs:SupportsHighDynamicRange=\"True\"\n   crs:SupportsNormalDynamicRange=\"True\"\n   crs:SupportsSceneReferred=\"True\"\n   crs:SupportsOutputReferred=\"True\"\n   crs:RequiresRGBTables=\"False\"\n   crs:Version=\"16.4\"\n   crs:ProcessVersion=\"15.4\"\n   crs:HasSettings=\"True\"\n   {LC_SETTINGS}=\"{}\">{}{}{}\n  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n",
        lcc_meta::LC_NS,
        hex_id(&p.id),
        esc(&lc),
        alt("Name", &p.name),
        alt("Group", &p.group),
        mask_block
    );
    notes.sort();
    notes.dedup();
    (xmp, notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn preset() -> Preset {
        let masks = json!([
            {"id": 1, "name": "Sun & \"warm\"", "components": [
                {"op": "add", "shape": {"kind": "background"}},
                {"op": "subtract", "anchor": {"to": "people", "shape": {"kind": "linear", "start": {"x": 0.5, "y": 1.0}, "end": {"x": 0.5, "y": 0.55}}},
                 "shape": {"kind": "linear", "start": {"x": 0.5, "y": 1.0}, "end": {"x": 0.5, "y": 0.55}}}],
             "adjust": {"temp": 18.0, "exposure": 0.4, "amount": 100.0}},
            {"id": 2, "name": "Suit", "components": [
                {"op": "add", "shape": {"kind": "people", "person": 0, "parts": ["Clothes"], "who": {"role": "Groom"}}},
                {"op": "intersect", "shape": {"kind": "luminanceRange", "lo": 0.2, "hi": 0.6, "lo_feather": 0.1, "hi_feather": 0.1}},
                {"op": "add", "shape": {"kind": "prompt", "text": "Wedding dress"}}],
             "adjust": {"saturation": 30.0, "amount": 100.0},
             "curve": {"master": [{"x": 0.0, "y": 0.0}, {"x": 0.5, "y": 0.6}, {"x": 1.0, "y": 1.0}]},
             "point_colors": [{"lum": 0.7, "chroma": 0.08, "hue": 60.0, "hue_shift": 10.0, "sat_shift": -20.0, "lum_shift": 5.0}]}
        ]);
        Preset {
            id: "user.t".into(),
            name: "Sun <kissed>".into(),
            group: "Smart".into(),
            settings: json!({"masks": masks, "light": {"contrast": 10.0}}),
            favorite: false,
            builtin: false,
        }
    }

    #[test]
    fn exported_presets_read_back_in_lightroom_terms_and_in_full() {
        let (xmp, notes) = to_xmp(&preset());
        let d = lcc_meta::parse_xmp(&xmp).expect("well-formed XMP");
        // Lightroom's view: the crs: masks, as our own importer reads them
        let (masks, unmapped) = crate::crs_masks::masks(&d.values, 1.5);
        assert!(unmapped.is_empty(), "{unmapped:?}");
        assert_eq!(masks.len(), 2);
        assert_eq!(masks[0]["components"][0]["shape"]["kind"], "background");
        assert_eq!(masks[0]["components"][1]["op"], "subtract");
        assert!((masks[0]["adjust"]["exposure"].as_f64().unwrap() - 0.4).abs() < 1e-6);
        assert_eq!(masks[1]["components"][0]["shape"]["parts"], json!(["Clothes"]));
        // intersect survives as Lightroom writes it (subtract the inverted range)
        assert_eq!((masks[1]["components"][1]["op"].as_str(), masks[1]["components"][1]["invert"].as_bool()), (Some("subtract"), Some(true)));
        let lo = masks[1]["components"][1]["shape"]["lo"].as_f64().unwrap();
        assert!((lo - 0.2).abs() < 0.01, "luminance range round trip: {lo}");
        // the mask's Curve and Point Color travel with it
        let master = masks[1]["curve"]["master"].as_array().expect("local curve");
        assert_eq!(master.len(), 3);
        assert!((master[1]["y"].as_f64().unwrap() - 0.6).abs() < 0.01, "{master:?}");
        let pc = masks[1]["point_colors"].as_array().expect("point colours");
        assert_eq!(pc.len(), 1);
        assert!((pc[0]["hue_shift"].as_f64().unwrap() - 10.0).abs() < 1.0, "{pc:?}");
        // what Lightroom can't hold is said
        assert!(notes.iter().any(|n| n.contains("Groom")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("Wedding dress")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("Global settings")), "{notes:?}");
        // our own: the whole preset
        let lc = d.properties.get(LC_SETTINGS).and_then(|v| v.first()).expect("lc settings");
        let back: Value = serde_json::from_str(lc).unwrap();
        assert_eq!(back, preset().settings);
        assert!(xmp.contains("Sun &lt;kissed&gt;"));
    }

    // issue #1145: the mask Hue is in degrees (±180); Lightroom's `crs:LocalHue` holds about ±34°
    #[test]
    fn mask_hue_writes_lightroom_scale_and_clamps_beyond_it() {
        let attr = |a: &LocalAdjustments| {
            let s = local_attrs(a);
            let v = s.split("crs:LocalHue=\"").nth(1).and_then(|t| t.split('"').next()).unwrap().to_string();
            v.parse::<f64>().unwrap()
        };
        // an old in-range value (stored −100..100) writes exactly what it wrote before
        for old in [-100.0, -42.0, 0.0, 7.0, 100.0] {
            let a: LocalAdjustments = serde_json::from_value(json!({"hue": old})).unwrap();
            assert_eq!(f(attr(&a)), f(old / 100.0), "{old}");
            assert!(!local_attrs(&a).contains(LC_LOCAL_HUE), "in range: no extra field ({old})");
        }
        // past the standard range: the extra field holds the whole angle
        let wide = local_attrs(&LocalAdjustments { hue: 150.0, ..Default::default() });
        assert!(wide.contains(&format!("{LC_LOCAL_HUE}=\"150.000000\"")), "{wide}");
        // read back: the extra field wins over the clamped `crs:LocalHue`, which is used without it
        let corr = |extra: &str| {
            format!(
                r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmlns:lc="{}">
  <crs:MaskGroupBasedCorrections><rdf:Seq><rdf:li><rdf:Description crs:What="Correction" crs:LocalHue="1" {extra}>
   <crs:CorrectionMasks><rdf:Seq><rdf:li crs:What="Mask/Gradient" crs:MaskValue="1" crs:ZeroX="0.5" crs:ZeroY="0" crs:FullX="0.5" crs:FullY="1"/></rdf:Seq></crs:CorrectionMasks>
  </rdf:Description></rdf:li></rdf:Seq></crs:MaskGroupBasedCorrections></rdf:Description></rdf:RDF></x:xmpmeta>"#,
                lcc_meta::LC_NS
            )
        };
        let hue_of = |x: String| {
            let d = lcc_meta::parse_xmp(&x).unwrap();
            let (masks, _) = crate::crs_masks::masks(&d.values, 1.5);
            serde_json::from_value::<LocalAdjustments>(masks[0]["adjust"].clone()).unwrap().hue
        };
        assert_eq!(hue_of(corr(&format!("{LC_LOCAL_HUE}=\"150\""))), 150.0);
        assert!((hue_of(corr("")) - lcc_develop::LOCAL_HUE_STANDARD).abs() < 1e-9);
        assert_eq!(hue_of(corr(&format!("{LC_LOCAL_HUE}=\"1e300\""))), 180.0, "hostile: clamped");
        // beyond Lightroom's reach: clamped there; the full value stays in our own settings
        assert_eq!(attr(&LocalAdjustments { hue: 150.0, ..Default::default() }), 1.0);
        assert_eq!(attr(&LocalAdjustments { hue: -180.0, ..Default::default() }), -1.0);
        assert_eq!(attr(&LocalAdjustments { hue: f64::NAN, ..Default::default() }), 0.0);
        let mut p = preset();
        p.settings["masks"][0]["adjust"] = serde_json::to_value(LocalAdjustments { hue: 150.0, ..Default::default() }).unwrap();
        let (xmp, _) = to_xmp(&p);
        let d = lcc_meta::parse_xmp(&xmp).unwrap();
        let lc: Value = serde_json::from_str(d.properties.get(LC_SETTINGS).and_then(|v| v.first()).unwrap()).unwrap();
        let back: LocalAdjustments = serde_json::from_value(lc["masks"][0]["adjust"].clone()).unwrap();
        assert_eq!(back.hue, 150.0);
    }

    // Notes 18/22: brush strokes go to Lightroom as `Mask/Paint` (Radius = where the halo ends, CenterWeight from
    // Feather, `d` / `M` dabs every 0.3 × Radius), and come back as the same strokes; written again, the values
    // are the same
    #[test]
    fn brush_strokes_round_trip_through_lightroom_terms() {
        use lcc_develop::{BRUSH_MODEL_LRC, BrushStroke, brush_model};
        use lcc_geom::Point;
        let st = |points: Vec<Point>, feather: f64, auto_mask: bool, erase: bool| BrushStroke {
            points,
            size: 0.04,
            feather,
            flow: 70.0,
            density: if erase { 100.0 } else { 90.0 },
            erase,
            auto_mask,
            model: BRUSH_MODEL_LRC,
            ..Default::default()
        };
        // a dragged stroke: samples a hair apart, as the pointer records them
        let drag: Vec<Point> = (0..=300).map(|k| Point::new(0.2 + 0.002 * k as f64, 0.4)).collect();
        let strokes =
            vec![st(drag, 40.0, false, false), st(vec![Point::new(0.5, 0.6)], 100.0, true, false), st(vec![Point::new(0.5, 0.42)], 0.0, false, true)];
        let masks = json!([{"id": 1, "name": "Brushed", "components": [{"op": "add", "shape": {"kind": "brush", "strokes": strokes}}], "adjust": {"exposure": -1.0, "amount": 100.0}}]);
        let p =
            Preset { id: "user.b".into(), name: "B".into(), group: "G".into(), settings: json!({"masks": masks}), favorite: false, builtin: false };
        let (xmp, notes) = to_xmp(&p);
        assert!(!notes.iter().any(|n| n.contains("brush")), "{notes:?}");
        assert!(xmp.contains("crs:What=\"Mask/Paint\""));
        assert!(xmp.contains(&format!("crs:Radius=\"{}\"", f(0.04 * brush_model::outer_ratio(40.0)))), "{xmp}");
        assert!(xmp.contains(&format!("crs:CenterWeight=\"{}\"", f(brush_model::center_weight(40.0)))));
        assert!(xmp.contains("<rdf:li>M 0.500000 0.600000</rdf:li>"), "an Auto Mask dab");
        let d = lcc_meta::parse_xmp(&xmp).expect("well-formed XMP");
        let (back, unmapped) = crate::crs_masks::masks(&d.values, lcc_develop::recipe::RECIPE_ASPECT);
        assert!(unmapped.is_empty(), "{unmapped:?}");
        let mask: Mask = serde_json::from_value(back[0].clone()).expect("a mask");
        let MaskShape::Brush { strokes: got } = &mask.components[0].shape else { panic!("a brush") };
        assert_eq!(got.len(), 3);
        for (g, want) in got.iter().zip(&strokes) {
            assert!((g.size - want.size).abs() < 1e-5 && (g.feather - want.feather).abs() < 0.05, "{g:?}");
            assert_eq!((g.flow, g.density, g.erase, g.auto_mask, g.model), (want.flow, want.density, want.erase, want.auto_mask, BRUSH_MODEL_LRC));
        }
        // the drag was saved as Lightroom saves it: a dab every 0.3 × Radius (0.6 of the long side / ~0.017)
        let spacing = brush_model::DAB_SPACING * 0.04 * brush_model::outer_ratio(40.0);
        assert_eq!(got[0].points.len(), (0.6 / spacing).floor() as usize + 1, "{}", got[0].points.len());
        // and written again, Lightroom gets the same values
        let p2 = Preset { settings: json!({"masks": [serde_json::to_value(&mask).unwrap()]}), ..p };
        let (xmp2, _) = to_xmp(&p2);
        let attrs = |x: &str| -> Vec<String> {
            x.lines()
                .filter(|l| l.contains("crs:Radius") || l.contains("crs:CenterWeight") || l.contains("rdf:li>d ") || l.contains("rdf:li>M "))
                .map(|l| l.trim().to_string())
                .collect()
        };
        assert_eq!(attrs(&xmp2), attrs(&xmp));
    }
}

#[cfg(test)]
mod hostile {
    use super::*;
    use serde_json::json;

    #[test]
    fn hostile_presets_export_without_panicking() {
        for settings in [
            json!({"masks": "nope"}),
            json!({"masks": [{"name": "<&>", "components": [{"shape": {"kind": "radial", "center": {"x": 1e308, "y": -1e308}, "rx": 0.0, "ry": 0.0, "angle": 0.0, "feather": 1e9, "invert": true}}]}]}),
            json!({"masks": [{"curve": {"master": [{"x": -5.0, "y": 9.0}, {"x": 2.0, "y": -1.0}]}, "components": [{"shape": {"kind": "sky"}}]}]}),
            json!(null),
        ] {
            let p = Preset { id: "x".into(), name: "\u{0}bad\u{7}".into(), group: String::new(), settings, favorite: false, builtin: false };
            let (xmp, _) = to_xmp(&p);
            assert!(lcc_meta::parse_xmp(&xmp).is_ok(), "{xmp}");
        }
    }
}
