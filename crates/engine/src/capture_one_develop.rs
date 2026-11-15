//! Capture One develop settings for the catalog import ([`crate::capture_one_catalog`]).
//!
//! Capture One keeps a variant's develop settings in `ZVARIANTLAYER`, one row per layer, told apart by the
//! Core Data entity in `Z_ENT` (named in `ZENTITIES`): the *adjustment* layer holds what the user set (a column
//! the user never touched is NULL), *style* layers what applied styles set, and the *default* layer the camera's
//! defaults (every column filled). A setting counts as the variant's when the adjustment layer (else a style)
//! sets it to something other than the default.
//!
//! Imported so far: exposure, the orientation and straighten angle, the crop and its aspect, flips and white
//! balance. Every other setting a variant uses is named in the import report as not imported. The ICC profile
//! choice is left out on purpose: photos get the app's standard camera colour.
//!
//! How Capture One writes the values (observed in catalogs written by Capture One 16.x, own design; no Capture One
//! code or documentation was used):
//! - `ZCROP` `cx;cy;w;h`: the crop's centre in the stored image's pixels (before any rotation), its width and height
//!   in pixels of the rotated (shown) frame.
//! - `ZROTATION` degrees, the whole turn: the camera's orientation (the default layer's value) plus quarter turns
//!   and the straighten angle.
//! - `ZFLIP` 0 none, 1 horizontal, 2 vertical.
//! - `ZWHITEBALANCE` `r;g;b`: the gains on the camera's raw channels (green 1).
//! - `ZEXPOSURE` EV.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::AtomicBool;

use lcc_geom::{Affine, Orientation, Point};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::lightroom_catalog::{Reader, Row};

/// Which way Capture One's `ZROTATION` turns: +1 when a positive angle turns the picture clockwise as shown.
/// Checked against Capture One's exports of a straightened landscape and a portrait photo.
pub(crate) const ROTATION_SIGN: f64 = 1.0;

/// The settings of one variant this import brings across, as Capture One stores them, with the names of the
/// settings it uses that aren't imported. Kept in the import archive.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureOneDevelop {
    /// Exposure (EV), when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposure: Option<f64>,
    /// The whole turn in degrees (`ZROTATION` of the adjustment layer, else of the default layer).
    pub rotation: f64,
    /// The camera's orientation in degrees (the default layer's `ZROTATION`): what the file's own orientation shows.
    pub camera_rotation: f64,
    /// `ZFLIP`, when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flip: Option<i64>,
    /// `ZCROP` (`cx, cy, w, h`), when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crop: Option<[f64; 4]>,
    /// `ZCROPASPECTRATIONAME`, when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect: Option<String>,
    /// The stored image's size in pixels, as Capture One has it (`ZWIDTH`, `ZHEIGHT`): the frame of the crop.
    pub size: [f64; 2],
    /// White balance through the camera's matrices, when the variant's differs from as shot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub white: Option<White>,
    /// The settings the variant uses that this import doesn't bring across (names as Capture One shows them).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub not_imported: Vec<String>,
}

/// A white balance as Kelvin and tint, with the as-shot white it is measured against, both through the
/// camera's matrices (`lcc_camdb`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct White {
    pub temp: f64,
    pub tint: f64,
    pub as_shot_temp: f64,
    pub as_shot_tint: f64,
}

/// The settings named in the report, by the Capture One tool they belong to: columns that start with a prefix
/// count for that tool. Columns not listed (bookkeeping, the ICC profile, values Capture One computes itself,
/// brush settings) are never reported.
const TOOLS: &[(&str, &[&str])] = &[
    ("Brightness", &["ZBRIGHTNESS"]),
    ("Contrast", &["ZCONTRAST"]),
    ("Saturation", &["ZSATURATION"]),
    ("High Dynamic Range", &["ZHIGHLIGHTRECOVERY", "ZSHADOWRECOVERY", "ZWHITERECOVERY", "ZBLACKRECOVERY"]),
    ("Levels", &["ZLEVELSHIGHLIGHT", "ZLEVELSSHADOW", "ZMIDTONE", "ZLEVELSTARGETHIGHLIGHT", "ZLEVELSTARGETSHADOW"]),
    ("Curve", &["ZGRADATIONCURVE"]),
    ("Base Characteristics curve", &["ZFILMCURVE"]),
    ("Clarity", &["ZCLARITY"]),
    ("Dehaze", &["ZDEHAZEAMOUNT", "ZDEHAZECOLOR"]),
    ("Sharpening", &["ZUSM"]),
    (
        "Noise Reduction",
        &[
            "ZNOISEREDUCTIONAMOUNT",
            "ZNOISEDETAILSAMOUNT",
            "ZCNRAMOUNT",
            "ZSURFACESMOOTHNESSAMOUNT",
            "ZCLEANLONGEXPOSUREAMOUNT",
            "ZDENOISEMETHOD",
            "ZAIDENOISEAMOUNT",
        ],
    ),
    ("Moiré", &["ZMOIRE"]),
    ("Film Grain", &["ZFILMGRAIN", "ZFINEGRAINAMOUNT"]),
    ("Black & White", &["ZBW"]),
    ("Color Balance", &["ZCOLORBALANCE"]),
    ("Color Editor", &["ZCOLORCORRECTIONS"]),
    ("Keystone", &["ZGEOMETRYKEYSTONE", "ZGEOMETRYCANVASCLIPPING", "ZCORRESPONDINGPOINTS"]),
    ("LCC", &["ZLENSLCC", "ZLENSCOLORCASTENABLED", "ZLENSDUSTREMOVALENABLED"]),
    (
        "Lens Correction",
        &[
            "ZLENSDISTORTION",
            "ZLENSLIGHTFALLOFF",
            "ZLENSRADIALBLUR",
            "ZLENSCHROMABRENABLED",
            "ZLENSCHROMABRORIGIN",
            "ZLENSCUSTOMCHROMABR",
            "ZLENSPURPLEFRINGINGAMOUNT",
            "ZLENSCHROMAFRINGING",
            "ZLENSDIFFRACTIONCORRECTIONENABLED",
            "ZLENSIDGUID",
            "ZLENSFOCALLENGTH",
            "ZLENSAPERTURE",
            "ZLENSOPTICCENTERENABLED",
            "ZLENSSHIFT",
            "ZLENSTILT",
            "ZLENSVIGNETTING",
        ],
    ),
    ("Spot Removal", &["ZSPOTS", "ZRELATIVESPOTS", "ZRELATIVEDUST"]),
    ("Crop outside the image", &["ZCROPOUTSIDEIMAGE"]),
    ("Portrait retouching", &["ZRETOUCHING"]),
];

/// Columns this import reads for itself.
const IMPORTED: &[&str] = &["ZEXPOSURE", "ZROTATION", "ZFLIP", "ZCROP", "ZCROPASPECTRATIONAME", "ZWHITEBALANCE"];

/// A column's tool in the report, or `None` for a column that is never reported. A longer prefix wins
/// (`ZLENSLCC…` is LCC, other `ZLENS…` columns lens correction).
fn tool_of(column: &str) -> Option<&'static str> {
    TOOLS
        .iter()
        .flat_map(|(tool, prefixes)| prefixes.iter().filter(|p| column.starts_with(**p)).map(move |p| (p.len(), *tool)))
        .max_by_key(|(len, _)| *len)
        .map(|(_, tool)| tool)
}

fn wanted(column: &str) -> bool {
    IMPORTED.contains(&column) || tool_of(column).is_some()
}

/// The numbers in a Capture One value list (`1.0;2;3`, `0,0;1,1`, `a|b`), or nothing when any part isn't one.
fn numbers(s: &str) -> Option<Vec<f64>> {
    let parts: Vec<&str> = s.split([';', ',', '|']).filter(|t| !t.trim().is_empty()).collect();
    if parts.is_empty() || parts.len() > 4096 {
        return None;
    }
    parts.iter().map(|t| t.trim().parse::<f64>().ok().filter(|v| v.is_finite())).collect()
}

/// Whether two stored values say the same: numbers within a thousandth (Capture One writes the same gains as
/// `1.852` and `1.852000`), lists of numbers element by element, anything else as written.
fn same(a: &Value, b: &Value) -> bool {
    let close = |x: f64, y: f64| (x - y).abs() <= 1e-3 * x.abs().max(y.abs()).max(1.0);
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => close(x, y),
            _ => x == y,
        },
        (Value::String(x), Value::String(y)) => {
            x == y || matches!((numbers(x), numbers(y)), (Some(p), Some(q)) if p.len() == q.len() && p.iter().zip(&q).all(|(x, y)| close(*x, *y)))
        }
        _ => a == b,
    }
}

fn is_set(v: Option<&Value>) -> Option<&Value> {
    v.filter(|v| !v.is_null())
}

/// One variant's layers: the adjustment layer's and the styles' set columns (the adjustment layer first, then
/// the styles in their order), the default layer's values for those columns, and how many local adjustment
/// layers it has.
#[derive(Debug, Default)]
pub(crate) struct Layers {
    adjustment: Row,
    styles: Vec<(i64, Row)>,
    default: Row,
    local: usize,
}

impl Layers {
    /// The variant's own value of `column`: the adjustment layer's, else the first style's that sets it.
    fn own(&self, column: &str) -> Option<&Value> {
        is_set(self.adjustment.get(column)).or_else(|| self.styles.iter().find_map(|(_, s)| is_set(s.get(column))))
    }

    fn default(&self, column: &str) -> Option<&Value> {
        is_set(self.default.get(column))
    }

    /// The variant's value of `column` when it differs from the default.
    fn changed(&self, column: &str) -> Option<&Value> {
        self.own(column).filter(|v| self.default(column).is_none_or(|d| !same(v, d)))
    }

    fn number(&self, column: &str) -> Option<f64> {
        self.changed(column).and_then(Value::as_f64).filter(|v| v.is_finite())
    }
}

/// The layers of every variant in `ZVARIANTLAYER`, by variant key. The table is streamed twice so only what is
/// needed is kept: first the adjustment and style layers' set columns, then the default layer's values for those
/// columns. A catalog without the table, or without the entity names, has no develop settings to read.
pub(crate) fn read_layers(reader: &Reader, entities: &HashMap<i64, String>, cancel: &AtomicBool) -> Result<HashMap<i64, Layers>, String> {
    let kind = |name: &str| entities.iter().filter(|(_, n)| n.as_str() == name).map(|(e, _)| *e).collect::<HashSet<i64>>();
    let (adjustment, default, style, local) =
        (kind("VariantAdjustmentLayer"), kind("VariantDefaultLayer"), kind("VariantStyleLayer"), kind("LocalAdjustmentsLayer"));
    let mut out: HashMap<i64, Layers> = HashMap::new();
    if adjustment.is_empty() || default.is_empty() {
        return Ok(out);
    }
    let key = |r: &Row, k: &str| r.get(k).and_then(Value::as_i64);
    reader.each_row("ZVARIANTLAYER", false, false, cancel, |row, _| {
        let (Some(ent), Some(variant)) = (key(&row, "Z_ENT"), key(&row, "ZVARIANT").filter(|v| *v > 0)) else { return Ok(()) };
        let set = |row: Row| -> Row { row.into_iter().filter(|(k, v)| !v.is_null() && wanted(k)).collect() };
        if adjustment.contains(&ent) {
            out.entry(variant).or_default().adjustment = set(row);
        } else if style.contains(&ent) {
            let index = key(&row, "ZINDEX").unwrap_or(0);
            let styles = &mut out.entry(variant).or_default().styles;
            if styles.len() < 64 {
                styles.push((index, set(row)));
            }
        } else if local.contains(&ent) && row.get("ZENABLED").and_then(Value::as_i64) != Some(0) {
            let layers = out.entry(variant).or_default();
            layers.local = layers.local.saturating_add(1);
        }
        Ok(())
    })?;
    for layers in out.values_mut() {
        layers.styles.sort_by_key(|(index, _)| *index);
    }
    // the default layer: its rotation always (the camera's orientation), the rest where the variant sets something
    reader.each_row("ZVARIANTLAYER", false, false, cancel, |row, _| {
        let (Some(ent), Some(variant)) = (key(&row, "Z_ENT"), key(&row, "ZVARIANT")) else { return Ok(()) };
        if !default.contains(&ent) {
            return Ok(());
        }
        // (a variant with no adjustment or style layer has nothing to import)
        let Some(layers) = out.get_mut(&variant) else { return Ok(()) };
        let used: HashSet<String> = layers.adjustment.keys().chain(layers.styles.iter().flat_map(|(_, s)| s.keys())).cloned().collect();
        layers.default = row.into_iter().filter(|(k, v)| !v.is_null() && (k == "ZROTATION" || used.contains(k))).collect();
        Ok(())
    })?;
    Ok(out)
}

/// A crop rectangle that is a crop: finite, positive size.
fn crop_of(v: &Value) -> Option<[f64; 4]> {
    let n = numbers(v.as_str()?)?;
    let [cx, cy, w, h] = <[f64; 4]>::try_from(n).ok()?;
    (w > 0.0 && h > 0.0 && cx.is_finite() && cy.is_finite()).then_some([cx, cy, w, h])
}

fn gains(v: &Value) -> Option<[f32; 3]> {
    let n = numbers(v.as_str()?)?;
    let [r, g, b] = <[f64; 3]>::try_from(n).ok()?;
    let ok = |x: f64| x > 1e-4 && x < 1e4;
    (ok(r) && ok(g) && ok(b)).then_some([r as f32, g as f32, b as f32])
}

/// White balance gains on a camera's raw channels as Kelvin and tint, read the way the app reads a raw's own gains
/// (`crate::files::WbScaleOf`): through the camera's matrices when the app has them (`camera`: measured, or the
/// camera database's), else through the white a raw without colour tags has (`lcc_raw::color::as_shot_white_of`).
/// Only the change from as shot is applied ([`partial`]), so a raw whose own file carries matrices still gets
/// Capture One's change.
fn kelvin(camera: Option<&lcc_camdb::Entry>, gains: [f32; 3]) -> Option<(f64, f64)> {
    let none = lcc_raw::ColorData::default();
    let xy = match camera {
        Some(c) => c.as_shot_white_through(&none, Some(gains))?,
        None => lcc_raw::color::as_shot_white_of(&none, Some(gains)),
    };
    let (t, tint) = lcc_color::cct::xy_to_temp_tint(xy);
    (t.is_finite() && tint.is_finite() && t > 0.0).then_some((t, tint))
}

/// One variant's develop settings, for an image of `size` pixels shot on `camera` (Exif make and model).
pub(crate) fn develop_of(layers: &Layers, size: [f64; 2], camera: (&str, &str)) -> CaptureOneDevelop {
    let mut d = CaptureOneDevelop { size, ..Default::default() };
    let mut not_imported: BTreeMap<&'static str, ()> = BTreeMap::new();
    d.exposure = layers.number("ZEXPOSURE");
    d.camera_rotation = layers.default("ZROTATION").and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(0.0);
    d.rotation = layers.own("ZROTATION").and_then(Value::as_f64).filter(|v| v.is_finite() && v.abs() < 1e6).unwrap_or(d.camera_rotation);
    d.flip = layers.changed("ZFLIP").and_then(Value::as_i64).filter(|f| *f != 0);
    d.crop = layers.own("ZCROP").and_then(crop_of);
    d.aspect = layers.own("ZCROPASPECTRATIONAME").and_then(Value::as_str).map(str::to_string);
    if let Some(own) = layers.changed("ZWHITEBALANCE") {
        // (Capture One names a rendered file's format in place of a camera: no raw channels to balance)
        let raw = !camera.1.is_empty();
        let make = Some(camera.0).filter(|m| !m.is_empty());
        let entry = raw.then(|| lcc_camdb::find(make, camera.1)).flatten().filter(|e| lcc_raw::color::has_matrix(&e.color_data()));
        let white = (|| {
            let (temp, tint) = kelvin(entry.as_ref(), gains(own)?)?;
            let (as_shot_temp, as_shot_tint) = kelvin(entry.as_ref(), layers.default("ZWHITEBALANCE").and_then(gains)?)?;
            Some(White { temp, tint, as_shot_temp, as_shot_tint })
        })();
        match white.filter(|_| raw) {
            Some(w) => d.white = Some(w),
            None if !raw => {
                not_imported.insert("White Balance (not a raw file)", ());
            }
            None => {
                not_imported.insert("White Balance (unreadable values)", ());
            }
        }
    }
    let columns: HashSet<&String> = layers.adjustment.keys().chain(layers.styles.iter().flat_map(|(_, s)| s.keys())).collect();
    for column in columns {
        if let Some(tool) = tool_of(column)
            && layers.changed(column).is_some()
        {
            not_imported.insert(tool, ());
        }
    }
    if layers.local > 0 {
        not_imported.insert("Local adjustment layers", ());
    }
    d.not_imported = not_imported.into_keys().map(str::to_string).collect();
    d
}

impl CaptureOneDevelop {
    /// Whether there is nothing to apply: a variant with nothing but its camera's orientation keeps the settings
    /// the import gives any photo.
    pub fn is_empty(&self) -> bool {
        self.exposure.is_none()
            && self.flip.is_none()
            && self.crop.is_none()
            && self.white.is_none()
            && (self.rotation - self.camera_rotation).abs() < 1e-9
    }
}

/// The partial develop settings (the app's names) for `d` on a photo developed for `target`, whose as-shot white
/// balance the app read from the file (`as_shot`, Kelvin and tint, when known), with what couldn't be applied.
pub fn partial(d: &CaptureOneDevelop, target: crate::crs::Target, as_shot: Option<(f64, f64)>) -> (Value, Vec<String>) {
    let mut out = serde_json::Map::new();
    let mut skipped = d.not_imported.clone();
    if let Some(ev) = d.exposure {
        out.insert("light".into(), json!({"exposure": ev.clamp(-5.0, 5.0)}));
    }
    if let Some(w) = d.white {
        use crate::crs::Target;
        let wb = match target {
            Target::RawAbsolute => {
                // Capture One's change from as shot, on the app's own as-shot white when it read the file: a white
                // left as shot stays exactly as shot whatever matrices either side uses
                let (temp, tint) = match as_shot {
                    Some((t0, tint0)) if t0 > 0.0 => {
                        let mired = 1e6 / t0 + (1e6 / w.temp - 1e6 / w.as_shot_temp);
                        (if mired > 0.0 { 1e6 / mired } else { w.temp }, tint0 + (w.tint - w.as_shot_tint))
                    }
                    _ => (w.temp, w.tint),
                };
                Some(json!({"mode": "custom", "temp": temp.clamp(2000.0, 50000.0), "tint": tint.clamp(-150.0, 150.0), "scale": "kelvin"}))
            }
            Target::RawRelative => lcc_pipeline::local::kelvin_to_legacy(w.temp, w.tint, (w.as_shot_temp, w.as_shot_tint))
                .map(|(t, tint)| json!({"mode": "custom", "temp": t.clamp(2000.0, 50000.0), "tint": tint.clamp(-150.0, 150.0)})),
            Target::Rendered | Target::Any => None,
        };
        match wb {
            Some(wb) => {
                out.insert("wb".into(), wb);
            }
            None => skipped.push("White Balance (not a raw file)".into()),
        }
    }
    geometry(d, &mut out, &mut skipped);
    skipped.sort();
    skipped.dedup();
    (Value::Object(out), skipped)
}

/// The aspect `ZCROPASPECTRATIONAME` names (`16x9`, `1x1 (Square)`), turned to the crop's shape; `None` for
/// Original, Unconstrained and anything else.
fn aspect_of(name: &str, portrait: bool) -> Option<(u32, u32)> {
    let ratio = name.split_whitespace().next()?;
    let (a, b) = ratio.split_once(['x', 'X', ':'])?;
    let (a, b): (u32, u32) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    if a == 0 || b == 0 || a > 10_000 || b > 10_000 {
        return None;
    }
    Some(if portrait == (a > b) { (b, a) } else { (a, b) })
}

/// Orientation, straighten angle, crop and flips.
fn geometry(d: &CaptureOneDevelop, out: &mut serde_json::Map<String, Value>, skipped: &mut Vec<String>) {
    let turn = ROTATION_SIGN * d.rotation;
    let quarters = (turn / 90.0).round();
    let angle = turn - 90.0 * quarters;
    let whole = (quarters as i64).rem_euclid(4) as u8;
    let camera = ((ROTATION_SIGN * d.camera_rotation / 90.0).round() as i64).rem_euclid(4) as u8;
    let user = (i64::from(whole) - i64::from(camera)).rem_euclid(4) as u8;
    if user != 0
        && let Ok(o) = serde_json::to_value(Orientation::from_parts(false, user))
    {
        out.insert("orientation".into(), o);
    }
    let mut crop = serde_json::Map::new();
    match d.flip {
        None => {}
        Some(1) => {
            crop.insert("flip_h".into(), json!(true));
        }
        Some(2) => {
            crop.insert("flip_v".into(), json!(true));
        }
        Some(_) => skipped.push("Flip".into()),
    }
    let [w, h] = d.size;
    let rect = match d.crop {
        Some([cx, cy, cw, ch]) if w > 0.0 && h > 0.0 => {
            // the crop's centre goes through the whole orientation into the shown frame, then the straighten turn
            let o = Orientation::from_parts(false, whole);
            let (ow, oh) = if o.swaps_axes() { (h, w) } else { (w, h) };
            let (u, v) = o.map(cx, cy, w, h);
            let c = Affine::rotate_about(angle.to_radians(), Point::new(ow / 2.0, oh / 2.0)).apply(Point::new(u, v));
            let r = [(c.x - cw / 2.0) / ow, (c.y - ch / 2.0) / oh, (c.x + cw / 2.0) / ow, (c.y + ch / 2.0) / oh].map(|v| v.clamp(0.0, 1.0));
            if r[2] - r[0] > 1e-4 && r[3] - r[1] > 1e-4 {
                // (its width and height are pixels of the shown frame: taller than wide is a portrait crop)
                Some((r, ch > cw))
            } else {
                skipped.push("Crop (outside the image)".into());
                None
            }
        }
        _ => None,
    };
    if rect.is_some() || angle.abs() > 1e-6 {
        let r = rect.map_or([0.0, 0.0, 1.0, 1.0], |(r, _)| r);
        crop.insert("geometry".into(), json!({"rect": {"x0": r[0], "y0": r[1], "x1": r[2], "y1": r[3]}, "angle": angle}));
    }
    if let (Some((_, portrait)), Some(name)) = (rect, d.aspect.as_deref())
        && let Some((a, b)) = aspect_of(name, portrait)
    {
        crop.insert("aspect".into(), json!([a, b]));
    }
    if !crop.is_empty() {
        out.insert("crop".into(), Value::Object(crop));
    }
}

#[cfg(test)]
#[path = "capture_one_develop_tests.rs"]
mod tests;
