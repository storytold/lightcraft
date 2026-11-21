//! A variant's crop, rotation, flip, perspective and lens settings, and what they become in
//! LightCraft's develop settings (issue #4596).
//!
//! Learned from a Capture One 16.3 test catalog made for this and TIFF exports of its variants,
//! by cutting the expected region out of each original and comparing it with the export; nothing
//! from Capture One's program files. What was found:
//!
//! - Each variant has a default layer (Capture One's own values), an adjustment layer (what the
//!   user set, as absolute values: a photo the file turns 90° and the user 5° more stores 95) and
//!   a combined layer, which is the adjustment layer's value where it has one, else the default.
//! - `ZROTATION` is in degrees, clockwise, and includes the file's orientation: the default layer
//!   holds that (90 for Exif orientation 6). `ZFLIP` is 0, 1 (left ↔ right) or 2 (top ↔ bottom),
//!   applied after the rotation in the axes of the photo as shown.
//! - `ZCROP` is `cx;cy;w;h`: the centre in the stored image's pixels (before rotation and flip,
//!   after lens correction at the same scale about the centre), the size in the axes of the photo
//!   as shown. `0;0;0;0` is no crop of the user's own: Capture One then shows the largest
//!   rectangle of the photo's shape that a straightened photo fills, about the centre.
//! - Lens values are text: `ZLENSDISTORTION` is `curve|amount|…`, `ZLENSLIGHTFALLOFF`
//!   `curve|amount|…`, `ZLENSRADIALBLUR` (sharpness falloff) `…|…|amount|…`. Capture One's
//!   curves are its own data and are not used. [`GENERIC_PROFILE`] is the profile "Generic".
//!
//! - A keystone tilt (`ZGEOMETRYKEYSTONETILTX` / `TILTY`) turns a virtual camera and a skew shears,
//!   both along the stored image's axes; the aspect changes the shape as shown (see [`keystone`]).
//!   Without a crop of the user's own, Capture One shows the largest rectangle of the photo's
//!   shape centred on the corrected photo's bounding box (the working area, `ZCANVASSIZE`); with
//!   a keystone, a crop of the user's own is stored at another scale (not measured yet).
//!
//! Only the user's changes are carried over: Capture One's own values stand for what LightCraft
//! does by itself. A lens correction is the exception once a photo brings other changes: it is
//! set as Capture One showed it, so the crop is taken on the photo it was drawn on, and a profile
//! LightCraft finds only later still applies (an edited photo is not switched on by itself).

use std::collections::HashMap;

use lightcraft_geom::Orientation;
use serde_json::{Map, Value, json};

use super::notice::{self, Notice};
use super::tables::{Db, Row};
use super::{CatalogPhoto, ReadError};

/// The lens profile Capture One calls "Generic": no profile of the lens itself.
pub(super) const GENERIC_PROFILE: &str = "E936EE15-75CA-42EC-9602-629D90D9947E";
/// Most degrees a rotation is read at.
const MAX_ROTATION: f64 = 3600.0;
/// Below this a value written as a float is taken as zero (`ZLENSSHIFT` holds 1.4e-45).
const NONE: f64 = 1e-6;
/// The adjustment layer's columns this module reads, and those every layer has: any other
/// column set there is an adjustment the import doesn't carry.
const KNOWN: &[&str] = &[
    "Z_PK",
    "Z_ENT",
    "Z_OPT",
    "ZVARIANT",
    "ZMETADATA",
    "ZINDEX",
    "ZROTATION",
    "ZFLIP",
    "ZCROP",
    "ZCROPOUTSIDEIMAGE",
    "ZCROPASPECTRATIONAME",
    "ZGEOMETRYCANVASCLIPPING",
    "ZGEOMETRYKEYSTONEAMOUNT",
    "ZGEOMETRYKEYSTONEASPECT",
    "ZGEOMETRYKEYSTONEFOCALLENGTH",
    "ZGEOMETRYKEYSTONESKEW",
    "ZGEOMETRYKEYSTONETILTX",
    "ZGEOMETRYKEYSTONETILTY",
    "ZLENSIDGUID",
    "ZLENSREVISIONIDGUID",
    "ZLENSDISTORTION",
    "ZLENSDEFAULTCHROMABR",
    "ZLENSCHROMABRENABLED",
    "ZLENSCHROMABRORIGIN",
    "ZLENSCUSTOMCHROMABR",
    "ZLENSCUSTOMCHROMABRNAME",
    "ZLENSCHROMAFRINGING",
    "ZLENSPURPLEFRINGINGAMOUNT",
    "ZLENSLIGHTFALLOFF",
    "ZLENSRADIALBLUR",
    "ZLENSDIFFRACTIONCORRECTIONENABLED",
    "ZLENSSHIFT",
    "ZLENSSHIFTDIRECTION",
    "ZLENSSHIFTXY",
    "ZLENSTILT",
    "ZLENSTILTDIRECTION",
    "ZLENSOPTICCENTER",
    "ZLENSOPTICCENTERENABLED",
    "ZLENSDUSTREMOVALENABLED",
    "ZLENSAPERTURE",
    "ZLENSFOCALLENGTH",
    "ZLENSVIGNETTING",
    "ZLENSCOLORCASTENABLED",
    "ZLENSLCC",
    "ZLENSLCCLIGHTFALLOFF",
    "ZLENSLCCLIGHTFALLOFFAMOUNT",
    "ZLENSLCCBITMAPHASH",
];

/// One layer's values as stored; `None` where the layer leaves a value to the one below.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layer {
    /// Degrees, clockwise, including the file's orientation.
    pub rotation: Option<f64>,
    /// 0 none, 1 left ↔ right, 2 top ↔ bottom.
    pub flip: Option<i64>,
    /// `cx, cy, w, h` (see the module); all zero for no crop of the user's own.
    pub crop: Option<[f64; 4]>,
    pub crop_outside: Option<bool>,
    /// `Original`, `Unconstrained`, `Square`, `3x4`, …
    pub ratio: Option<String>,
    pub keystone: Keystone,
    /// 2 hides the areas a lens correction leaves empty (the default), 1 shows them.
    pub clipping: Option<i64>,
    pub lens: Lens,
}

/// Perspective ("keystone") values.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Keystone {
    pub tilt_x: Option<f64>,
    pub tilt_y: Option<f64>,
    pub skew: Option<f64>,
    pub aspect: Option<f64>,
    pub amount: Option<f64>,
    /// The shot's focal length over about 28.2 mm; 1.0 for a JPEG.
    pub focal: Option<f64>,
}

/// Lens correction values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lens {
    pub profile: Option<String>,
    /// The distortion amount (100 by default with a lens's own profile).
    pub distortion: Option<f64>,
    pub chromatic_aberration: Option<bool>,
    pub light_falloff: Option<f64>,
    pub sharpness_falloff: Option<f64>,
    pub diffraction: Option<bool>,
    pub purple_fringing: Option<f64>,
    /// Shift or tilt of a shift lens.
    pub movement: Option<bool>,
    /// An LCC (lens cast calibration) or its colour cast is applied.
    pub lcc: Option<bool>,
}

/// A variant's geometric settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Geometry {
    pub default: Layer,
    pub adjusted: Layer,
    /// `ZVARIANT.ZCANVASSIZE`: the working area after rotation, perspective and lens correction.
    pub canvas: Option<(f64, f64)>,
    /// The adjustment layer sets something this import doesn't read (exposure, colour, …).
    pub other_adjustments: bool,
}

/// What a variant's geometry becomes in LightCraft.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Develop {
    /// Develop settings to merge (`lightcraft_develop::apply_partial`); empty when nothing changes.
    pub partial: Map<String, Value>,
    /// Messages naming each setting that has no LightCraft counterpart.
    pub left_out: Vec<&'static str>,
}

/// Fill each photo's [`CatalogPhoto::geometry`], and [`CatalogPhoto::turned`] from it. A catalog
/// without the layer table leaves them unknown.
pub(super) fn read(db: &Db, photos: &mut [CatalogPhoto], warnings: &mut Vec<Notice>) -> Result<(), ReadError> {
    let layers = db.optional("ZVARIANTLAYER", warnings)?;
    if !layers.columns().any(|c| c == "ZROTATION") {
        return Ok(());
    }
    let variants = db.table("ZVARIANT")?;
    let layer_of: HashMap<i64, Row<'_>> = layers.rows().filter_map(|r| Some((r.id("Z_PK")?, r))).collect();
    const OWN: [&str; 3] = ["ZDEFAULTLAYER", "ZADJUSTMENTLAYER", "ZCOMBINEDSETTINGS"];
    let own: HashMap<i64, Vec<i64>> = variants.rows().filter_map(|v| Some((v.id("Z_PK")?, OWN.iter().filter_map(|c| v.id(c)).collect()))).collect();
    // a local adjustment or a style is a layer of its own that names the variant
    let more_layers: std::collections::HashSet<i64> = layers
        .rows()
        .filter_map(|r| {
            let variant = r.id("ZVARIANT")?;
            let pk = r.id("Z_PK")?;
            own.get(&variant).is_some_and(|layers| !layers.contains(&pk)).then_some(variant)
        })
        .collect();
    let mut of_variant: HashMap<i64, (Geometry, Option<f64>)> = HashMap::new();
    for variant in variants.rows() {
        let Some(id) = variant.id("Z_PK") else { continue };
        let row = |column: &str| variant.id(column).and_then(|layer| layer_of.get(&layer));
        let default = row("ZDEFAULTLAYER").map(read_layer).unwrap_or_default();
        let adjusted_row = row("ZADJUSTMENTLAYER");
        let adjusted = adjusted_row.map(read_layer).unwrap_or_default();
        // what Capture One shows: the combined layer's turn, else as combined from the two
        let shown_turn =
            row("ZCOMBINEDSETTINGS").and_then(|r| r.real("ZROTATION")).filter(|d| d.abs() <= MAX_ROTATION).or(adjusted.rotation).or(default.rotation);
        let geometry = Geometry {
            canvas: pair(variant.text("ZCANVASSIZE")),
            other_adjustments: more_layers.contains(&id) || adjusted_row.is_some_and(|r| r.names().any(|c| !KNOWN.contains(&c) && r.is_set(c))),
            default,
            adjusted,
        };
        of_variant.insert(id, (geometry, shown_turn));
    }
    for photo in photos {
        let Some((geometry, shown)) = photo.variant_id.and_then(|id| of_variant.remove(&id)) else {
            photo.turned = Some(false);
            continue;
        };
        // quarter turns, nearest; a straightened photo (a few degrees) isn't turned
        let quarter = |rotation: Option<f64>| rotation.map_or(0.0, |degrees| (degrees / 90.0).round()).rem_euclid(2.0) == 1.0;
        photo.turned = Some(quarter(shown));
        photo.turned_by_hand = quarter(shown) != quarter(geometry.default.rotation);
        photo.geometry = Some(geometry);
    }
    Ok(())
}

fn read_layer(r: &Row<'_>) -> Layer {
    let field = |column: &str, at: usize| {
        let text = r.text(column);
        (!text.is_empty()).then(|| text.split('|').nth(at).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite())).flatten()
    };
    let nonzero = |column: &str| r.real(column).map(|v| v.abs() > NONE);
    let movement = [nonzero("ZLENSSHIFT"), nonzero("ZLENSTILT"), pair(r.text("ZLENSSHIFTXY")).map(|(x, y)| x.abs() > NONE || y.abs() > NONE)];
    let text = |column: &str| Some(r.text(column).trim()).filter(|t| !t.is_empty()).map(str::to_string);
    let flag = |column: &str| r.int(column).map(|n| n != 0);
    Layer {
        // (a turn of more than ten whole turns is no photo's: left as not set)
        rotation: r.real("ZROTATION").filter(|d| d.abs() <= MAX_ROTATION),
        flip: r.int("ZFLIP"),
        crop: crop(r.text("ZCROP")),
        crop_outside: flag("ZCROPOUTSIDEIMAGE"),
        ratio: text("ZCROPASPECTRATIONAME"),
        keystone: Keystone {
            tilt_x: r.real("ZGEOMETRYKEYSTONETILTX"),
            tilt_y: r.real("ZGEOMETRYKEYSTONETILTY"),
            skew: r.real("ZGEOMETRYKEYSTONESKEW"),
            aspect: r.real("ZGEOMETRYKEYSTONEASPECT"),
            amount: r.real("ZGEOMETRYKEYSTONEAMOUNT"),
            focal: r.real("ZGEOMETRYKEYSTONEFOCALLENGTH"),
        },
        clipping: r.int("ZGEOMETRYCANVASCLIPPING"),
        lens: Lens {
            profile: text("ZLENSIDGUID"),
            distortion: field("ZLENSDISTORTION", 1),
            chromatic_aberration: flag("ZLENSCHROMABRENABLED"),
            light_falloff: field("ZLENSLIGHTFALLOFF", 1),
            sharpness_falloff: field("ZLENSRADIALBLUR", 2),
            diffraction: flag("ZLENSDIFFRACTIONCORRECTIONENABLED"),
            purple_fringing: r.real("ZLENSPURPLEFRINGINGAMOUNT"),
            movement: movement.iter().any(Option::is_some).then(|| movement.contains(&Some(true))),
            lcc: (r.is_set("ZLENSLCC") || r.is_set("ZLENSCOLORCASTENABLED"))
                .then(|| !r.text("ZLENSLCC").trim().is_empty() || flag("ZLENSCOLORCASTENABLED") == Some(true)),
        },
    }
}

/// `a;b` as two finite numbers.
fn pair(text: &str) -> Option<(f64, f64)> {
    let mut parts = text.split(';').map(|v| v.trim().parse::<f64>().ok().filter(|v| v.is_finite()));
    match (parts.next()??, parts.next()??, parts.next()) {
        (a, b, None) => Some((a, b)),
        _ => None,
    }
}

/// `cx;cy;w;h` as four finite numbers.
fn crop(text: &str) -> Option<[f64; 4]> {
    let values: Vec<f64> = text.split(';').map(|v| v.trim().parse::<f64>().ok().filter(|v| v.is_finite())).collect::<Option<_>>()?;
    <[f64; 4]>::try_from(values).ok()
}

impl Layer {
    /// This layer's values where it has them, else `below`'s: what Capture One shows.
    fn over(&self, below: &Layer) -> Layer {
        let k = (&self.keystone, &below.keystone);
        let l = (&self.lens, &below.lens);
        Layer {
            rotation: self.rotation.or(below.rotation),
            flip: self.flip.or(below.flip),
            crop: self.crop.or(below.crop),
            crop_outside: self.crop_outside.or(below.crop_outside),
            ratio: self.ratio.clone().or_else(|| below.ratio.clone()),
            keystone: Keystone {
                tilt_x: k.0.tilt_x.or(k.1.tilt_x),
                tilt_y: k.0.tilt_y.or(k.1.tilt_y),
                skew: k.0.skew.or(k.1.skew),
                aspect: k.0.aspect.or(k.1.aspect),
                amount: k.0.amount.or(k.1.amount),
                focal: k.0.focal.or(k.1.focal),
            },
            clipping: self.clipping.or(below.clipping),
            lens: Lens {
                profile: l.0.profile.clone().or_else(|| l.1.profile.clone()),
                distortion: l.0.distortion.or(l.1.distortion),
                chromatic_aberration: l.0.chromatic_aberration.or(l.1.chromatic_aberration),
                light_falloff: l.0.light_falloff.or(l.1.light_falloff),
                sharpness_falloff: l.0.sharpness_falloff.or(l.1.sharpness_falloff),
                diffraction: l.0.diffraction.or(l.1.diffraction),
                purple_fringing: l.0.purple_fringing.or(l.1.purple_fringing),
                movement: l.0.movement.or(l.1.movement),
                lcc: l.0.lcc.or(l.1.lcc),
            },
        }
    }
}

impl Keystone {
    /// Whether the photo's perspective is changed at all.
    fn is_set(&self) -> bool {
        [self.tilt_x, self.tilt_y, self.skew, self.aspect].iter().any(|v| v.is_some_and(|v| v.abs() > NONE))
            && self.amount.is_none_or(|a| a.abs() > NONE)
    }
}

/// A value the user changed: set in the adjustment layer and unlike the default.
fn changed<T: PartialEq>(adjusted: &Option<T>, default: &Option<T>) -> bool {
    adjusted.is_some() && adjusted != default
}

/// A number the user changed (beyond rounding).
fn changed_number(adjusted: Option<f64>, default: Option<f64>) -> bool {
    adjusted.is_some_and(|a| default.is_none_or(|d| (a - d).abs() > NONE))
}

/// An integer orientation as a 2 × 2 matrix on coordinates about the centre, `y` down.
type Turn = [[i32; 2]; 2];

const IDENTITY: Turn = [[1, 0], [0, 1]];
const MIRROR: Turn = [[-1, 0], [0, 1]];
/// A quarter turn clockwise: `(x, y)` → `(−y, x)`.
const QUARTER: Turn = [[0, -1], [1, 0]];

fn mul(a: Turn, b: Turn) -> Turn {
    let at = |i: usize, j: usize| a[i][0] * b[0][j] + a[i][1] * b[1][j];
    [[at(0, 0), at(0, 1)], [at(1, 0), at(1, 1)]]
}

/// The inverse of an orthogonal integer matrix: its transpose.
fn inverse(a: Turn) -> Turn {
    [[a[0][0], a[1][0]], [a[0][1], a[1][1]]]
}

fn quarters(turns: i64) -> Turn {
    (0..turns.rem_euclid(4)).fold(IDENTITY, |m, _| mul(QUARTER, m))
}

/// A Capture One flip as a matrix in the axes of the photo as shown.
fn flip_matrix(flip: Option<i64>) -> Turn {
    match flip {
        Some(1) => MIRROR,
        Some(2) => [[1, 0], [0, -1]],
        _ => IDENTITY,
    }
}

/// LightCraft's orientation (a mirror first, then quarter turns clockwise) as a matrix.
fn orientation_matrix(o: Orientation) -> Turn {
    let (mirror, turns) = o.to_parts();
    mul(quarters(i64::from(turns)), if mirror { MIRROR } else { IDENTITY })
}

fn orientation_of(m: Turn) -> Orientation {
    (0..8u8).map(|i| Orientation::from_parts(i >= 4, i % 4)).find(|o| orientation_matrix(*o) == m).unwrap_or_default()
}

/// What `photo`'s geometry becomes in LightCraft; `None` when it has none to carry.
pub(super) fn develop(photo: &CatalogPhoto) -> Option<Develop> {
    let g = photo.geometry.as_ref()?;
    let (def, adj) = (&g.default, &g.adjusted);
    let shown = adj.over(def);
    let mut out = Develop::default();
    let mut left_out = |message: &'static str| {
        if !out.left_out.contains(&message) {
            out.left_out.push(message);
        }
    };

    // rotation and flip: the file's own turn is LightCraft's too; the rest is the user's
    let theta = shown.rotation.unwrap_or(0.0);
    let turns = (theta / 90.0).round();
    let straighten = theta - turns * 90.0;
    let turns = turns as i64;
    let file_turn = def.rotation.map_or(0, |d| (d / 90.0).round() as i64);
    let file = mul(flip_matrix(def.flip), quarters(file_turn));
    let user = orientation_of(mul(quarters(turns), inverse(file)));
    let flip = shown.flip.unwrap_or(0);
    let turned_or_flipped = changed_number(adj.rotation, def.rotation) || changed(&adj.flip, &def.flip);

    // the crop, centred on the stored image's centre, turned as shown (the flip comes after it)
    let (w, h) = (f64::from(photo.width), f64::from(photo.height));
    let (ow, oh) = if turns.rem_euclid(2) == 1 { (h, w) } else { (w, h) };
    // A lens correction that hides the areas it leaves empty makes Capture One's working area
    // smaller than the photo, at the same scale about the centre; LightCraft's correction fills
    // its frame instead (as a Fujifilm camera's does), so the crop is taken relative to that area.
    let corrected = {
        let l = &shown.lens;
        l.profile.is_some() && l.profile.as_deref() != Some(GENERIC_PROFILE) && l.distortion.is_some_and(|a| a > NONE)
    };
    let (ow, oh) = match g.canvas {
        Some((cw, ch)) if corrected && !shown.keystone.is_set() && shown.clipping != Some(1) => {
            let (s, c) = (straighten.to_radians().sin().abs(), straighten.to_radians().cos().abs());
            let (fx, fy) = (cw / (ow * c + oh * s), ch / (ow * s + oh * c));
            let fill = |f: f64| if f.is_finite() && f > 0.5 && f <= 1.0 { f } else { 1.0 };
            (ow * fill(fx), oh * fill(fy))
        }
        _ => (ow, oh),
    };
    let own_crop = shown.crop.filter(|c| c[2] > 0.0 && c[3] > 0.0).filter(|_| w > 0.0 && h > 0.0);
    let tilt = shown.keystone.is_set().then(|| keystone(&shown.keystone, quarters(turns), w, h)).flatten();
    // a skew (LightCraft's Transform has no shear), or a crop of its own on top (not measured
    // yet): not carried
    if shown.keystone.is_set() {
        match tilt {
            None => left_out(notice::PERSPECTIVE),
            Some(_) if own_crop.is_some() => left_out(notice::TILT_WITH_CROP),
            Some(_) => {}
        }
    }
    let tilt = tilt.filter(|_| own_crop.is_none());
    let (sin, cos) = theta.to_radians().sin_cos();
    let rect = match own_crop {
        Some([cx, cy, cw, ch]) => {
            let (x, y) = (cx - w / 2.0, cy - h / 2.0);
            let (qx, qy) = (x * cos - y * sin, x * sin + y * cos);
            Some([0.5 + (qx - cw / 2.0) / ow, 0.5 + (qy - ch / 2.0) / oh, 0.5 + (qx + cw / 2.0) / ow, 0.5 + (qy + ch / 2.0) / oh])
        }
        // a straightened photo without a crop of its own shows the largest rectangle of its
        // shape that the turned photo fills
        None if straighten.abs() > NONE && ow > 0.0 && oh > 0.0 => {
            let (s, c) = (straighten.to_radians().sin().abs(), straighten.to_radians().cos().abs());
            let k = (ow / (ow * c + oh * s)).min(oh / (ow * s + oh * c));
            Some([0.5 - k / 2.0, 0.5 - k / 2.0, 0.5 + k / 2.0, 0.5 + k / 2.0])
        }
        None => None,
    };
    // after a tilt, the largest rectangle of the photo's shape the corrected photo fills: it sits
    // towards the widened edge, as Capture One shows it
    let fitted = tilt.and_then(|transform| largest_inside(transform, straighten, ow, oh));
    let rect = fitted.map(|(r, _)| r).or(rect);
    let rect = rect.map(|r| {
        // Capture One can let a crop reach past the photo; LightCraft keeps it on the turned photo
        let crop = lightcraft_geom::CropGeometry { rect: lightcraft_geom::Rect::new(r[0], r[1], r[2], r[3]), angle: straighten };
        if !crop.is_within_image(ow, oh) && shown.crop_outside == Some(true) {
            left_out(notice::CROP_OUTSIDE);
        }
        // within the bounding box of the photo turned by the straighten, which holds every crop
        // that stays on it (as `CropGeometry::bounded` keeps one)
        let (s, c) = (straighten.to_radians().sin().abs(), straighten.to_radians().cos().abs());
        let (bx, by) = ((ow * c + oh * s) / ow / 2.0, (ow * s + oh * c) / oh / 2.0);
        [r[0].clamp(0.5 - bx, 0.5 + bx), r[1].clamp(0.5 - by, 0.5 + by), r[2].clamp(0.5 - bx, 0.5 + bx), r[3].clamp(0.5 - by, 0.5 + by)]
    });
    let cropped = changed(&adj.crop, &def.crop) || straighten.abs() > NONE;

    let mut p = Map::new();
    if turned_or_flipped || cropped || tilt.is_some() {
        p.insert("orientation".into(), json!(user));
        let mut crop = Map::new();
        crop.insert("flip_h".into(), json!(flip == 1));
        crop.insert("flip_v".into(), json!(flip == 2));
        if let Some([x0, y0, x1, y1]) = rect {
            crop.insert("geometry".into(), json!({"rect": {"x0": x0, "y0": y0, "x1": x1, "y1": y1}, "angle": straighten}));
            crop.insert("aspect".into(), json!(aspect(shown.ratio.as_deref(), (x1 - x0) * ow >= (y1 - y0) * oh)));
        } else if straighten.abs() <= NONE {
            crop.insert("geometry".into(), json!({"rect": {"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}, "angle": 0.0}));
        }
        p.insert("crop".into(), Value::Object(crop));
    }
    if let Some(transform) = tilt {
        // Capture One shows the largest rectangle the corrected photo fills (the crop above);
        // Scale keeps it inside LightCraft's frame, and Constrain Crop any corner from going empty
        let scale = fitted.map_or(100.0, |(_, s)| 100.0 * s);
        p.insert(
            "geometry".into(),
            json!({
                "vertical": transform.vertical,
                "horizontal": transform.horizontal,
                "rotate": transform.rotate,
                "aspect": transform.aspect,
                "scale": scale,
                "constrain_crop": true
            }),
        );
    }

    // lens corrections
    let (l, dl, al) = (&shown.lens, &def.lens, &adj.lens);
    let generic = l.profile.as_deref() == Some(GENERIC_PROFILE);
    let amount = l.distortion.unwrap_or(0.0);
    let lens_changed = changed(&al.profile, &dl.profile) || changed_number(al.distortion, dl.distortion);
    if l.profile.is_some() && (lens_changed || !p.is_empty()) {
        let mut optics = Map::new();
        optics.insert("lens_profile".into(), json!(corrected));
        if corrected && (amount - 100.0).abs() > NONE {
            optics.insert("profile_distortion".into(), json!(amount.clamp(0.0, 200.0)));
        }
        p.insert("optics".into(), Value::Object(optics));
    }
    if changed(&al.chromatic_aberration, &dl.chromatic_aberration) {
        let optics = p.entry("optics").or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(o) = optics {
            o.insert("remove_ca".into(), json!(l.chromatic_aberration == Some(true)));
        }
    }
    if generic && amount > NONE && changed_number(al.distortion, dl.distortion) {
        left_out(notice::GENERIC_DISTORTION);
    }
    if changed(&al.profile, &dl.profile) && !generic {
        left_out(notice::PROFILE_BY_HAND);
    }
    if changed(&adj.clipping, &def.clipping) && shown.clipping == Some(1) {
        left_out(notice::DISTORTED_AREAS);
    }
    if l.sharpness_falloff.is_some_and(|v| v.abs() > NONE) {
        left_out(notice::SHARPNESS_FALLOFF);
    }
    if l.diffraction == Some(true) {
        left_out(notice::DIFFRACTION);
    }
    if changed_number(al.light_falloff, dl.light_falloff) && l.light_falloff.is_some_and(|v| v.abs() > NONE) {
        left_out(notice::LIGHT_FALLOFF);
    }
    if changed_number(al.purple_fringing, dl.purple_fringing) && l.purple_fringing.is_some_and(|v| v.abs() > NONE) {
        left_out(notice::PURPLE_FRINGING);
    }
    if l.movement == Some(true) {
        left_out(notice::LENS_MOVEMENT);
    }
    if l.lcc == Some(true) {
        left_out(notice::LCC);
    }
    out.partial = p;
    (!out.partial.is_empty() || !out.left_out.is_empty()).then_some(out)
}

/// LightCraft's Transform values for a Capture One keystone.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Transform {
    vertical: f64,
    horizontal: f64,
    rotate: f64,
    aspect: f64,
}

impl Transform {
    fn geometry(self) -> lightcraft_develop::Geometry {
        lightcraft_develop::Geometry {
            vertical: self.vertical,
            horizontal: self.horizontal,
            rotate: self.rotate,
            aspect: self.aspect,
            ..Default::default()
        }
    }
}

/// Capture One's keystone as LightCraft's Transform, for a `w × h` stored image turned by `turn`
/// (the quarter turns as shown); `None` when it holds a skew, which LightCraft's Transform has no
/// counterpart for.
///
/// Measured on exports (see the module): a tilt turns a virtual camera about an axis of the stored
/// image, with a focal length of `focal` (the keystone's) times the diagonal, by an angle whose
/// tangent is the value / 100 × `focal`; `TILTY` > 0 widens the top edge, `TILTX` > 0 the left one.
/// `ASPECT` changes the shape as shown by 1 + value / 100 (wider for > 0); a skew shears along the
/// stored image's axes by the tangent of half its value. LightCraft's camera turn has a focal
/// length of its own, so its Vertical, Horizontal, Rotate and Aspect are fitted to bring a grid on the
/// photo where Capture One shows it, up to a scale and a shift.
fn keystone(k: &Keystone, turn: Turn, w: f64, h: f64) -> Option<Transform> {
    let off = |v: Option<f64>| v.is_none_or(|v| v.abs() <= NONE);
    if !off(k.skew) || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let amount = k.amount.unwrap_or(100.0) / 100.0;
    let value = |v: Option<f64>| v.unwrap_or(0.0) * amount;
    // Capture One's camera turn on the stored image, centred
    let focal = k.focal.filter(|f| f.is_finite() && *f > 0.01).unwrap_or(1.0);
    let f = focal * w.hypot(h);
    let (ax, ay) = ((value(k.tilt_x) / 100.0 * focal).atan(), (value(k.tilt_y) / 100.0 * focal).atan());
    let (sx, cx, sy, cy) = (ax.sin(), ax.cos(), ay.sin(), ay.cos());
    // K · rot_y(−ax) · rot_x(ay) · K⁻¹, applied to (x, y, 1)
    let project = |x: f64, y: f64| {
        let (px, py, pz) = (x / f, y / f, 1.0);
        let (qx, qy, qz) = (px, cy * py - sy * pz, sy * py + cy * pz);
        let (rx, ry, rz) = (cx * qx - sx * qz, qy, sx * qx + cx * qz);
        (f * rx / rz, f * ry / rz, rz)
    };
    // a grid on the stored image: where Capture One shows it, and where it is before LightCraft's
    // Transform (turned as shown, in LightCraft's units of half the long edge)
    let (ow, oh) = if turn[0][0] == 0 { (h, w) } else { (w, h) };
    let half = ow.max(oh) / 2.0;
    let a = value(k.aspect);
    // Capture One's aspect: the photo as shown wider by 1 + a / 100 (a > 0) or narrower
    let widen = 1.0 + a.abs() / 100.0;
    let (wx, wy) = if a >= 0.0 { (1.0, 1.0 / widen) } else { (1.0 / widen, 1.0) };
    let mut pairs = Vec::with_capacity(81);
    for i in 0..9 {
        for j in 0..9 {
            let (x, y) = ((f64::from(i) / 8.0 - 0.5) * w, (f64::from(j) / 8.0 - 0.5) * h);
            let (px, py, pz) = project(x, y);
            if !(pz > NONE && px.is_finite() && py.is_finite()) {
                return None;
            }
            let turned =
                |x: f64, y: f64| (f64::from(turn[0][0]) * x + f64::from(turn[0][1]) * y, f64::from(turn[1][0]) * x + f64::from(turn[1][1]) * y);
            let (sx, sy) = turned(px, py);
            let (lx, ly) = turned(x, y);
            pairs.push(((lx / half, ly / half), (sx * wx, sy * wy)));
        }
    }
    // LightCraft's Transform that brings the grid where Capture One shows it, up to a scale and
    // a shift (the crop takes care of those): least squares, settled one slider at a time
    let misfit = |t: Transform| -> f64 {
        let warp = lightcraft_pipeline::transform::manual(&t.geometry());
        let got: Vec<((f64, f64), (f64, f64))> = pairs
            .iter()
            .map(|((x, y), c)| {
                let p = warp.apply(lightcraft_geom::Point::new(*x, *y));
                ((p.x, p.y), *c)
            })
            .collect();
        similarity_misfit(&got)
    };
    let mut t = Transform { vertical: 0.0, horizontal: 0.0, rotate: 0.0, aspect: 0.0 };
    let mut best = misfit(t);
    let mut step = 16.0;
    while step > 0.005 {
        let mut moved = false;
        for which in 0..4 {
            for sign in [-1.0, 1.0] {
                let mut next = t;
                let v = match which {
                    0 => &mut next.vertical,
                    1 => &mut next.horizontal,
                    2 => &mut next.rotate,
                    _ => &mut next.aspect,
                };
                // (Rotate is in degrees, −45..45; the others −100..100)
                let limit = if which == 2 { 45.0 } else { 100.0 };
                *v = (*v + sign * step).clamp(-limit, limit);
                let m = misfit(next);
                if m < best {
                    (t, best, moved) = (next, m, true);
                }
            }
        }
        if !moved {
            step /= 2.0;
        }
    }
    [t.vertical, t.horizontal, t.rotate, t.aspect].iter().all(|v| v.is_finite()).then_some(t)
}

/// How far `pairs` (point, target) are from one scale and shift mapping each point onto its target,
/// relative to the targets' spread.
fn similarity_misfit(pairs: &[((f64, f64), (f64, f64))]) -> f64 {
    let n = pairs.len().max(1) as f64;
    let mean = |f: &dyn Fn(&((f64, f64), (f64, f64))) -> f64| pairs.iter().map(f).sum::<f64>() / n;
    let (px, py, cx, cy) = (mean(&|p| p.0.0), mean(&|p| p.0.1), mean(&|p| p.1.0), mean(&|p| p.1.1));
    let (mut cross, mut spread, mut target) = (0.0, 0.0, 0.0);
    for ((x, y), (u, v)) in pairs {
        let (dx, dy, du, dv) = (x - px, y - py, u - cx, v - cy);
        cross += dx * du + dy * dv;
        spread += dx * dx + dy * dy;
        target += du * du + dv * dv;
    }
    if spread <= 0.0 || target <= 0.0 {
        return f64::MAX;
    }
    let s = cross / spread;
    let error: f64 = pairs.iter().map(|((x, y), (u, v))| (s * (x - px) - (u - cx)).powi(2) + (s * (y - py) - (v - cy)).powi(2)).sum();
    error / target
}

/// The corners of an `ow × oh` photo after LightCraft's Transform `g` and a straighten by `angle`
/// degrees, in pixels about the frame's centre, from the top left, clockwise.
fn lightcraft_quad(g: &lightcraft_develop::Geometry, angle: f64, ow: f64, oh: f64) -> [(f64, f64); 4] {
    let half = ow.max(oh) / 2.0;
    let warp = lightcraft_pipeline::transform::manual(g);
    let (sin, cos) = angle.to_radians().sin_cos();
    [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(x, y): (f64, f64)| {
        let p = warp.apply(lightcraft_geom::Point::new(x * ow / 2.0 / half, y * oh / 2.0 / half));
        let (x, y) = (p.x * half, p.y * half);
        (x * cos - y * sin, x * sin + y * cos)
    })
}

/// The largest rectangle of the photo's own shape inside an `ow × oh` photo after LightCraft's
/// Transform and a straighten by `angle` degrees: `[x0, y0, x1, y1]`
/// normalized to the crop's rotated frame, and the Scale (1 = 100) that keeps it inside the frame
/// (the corrected photo can reach past it). `None` when the photo leaves no room.
fn largest_inside(transform: Transform, angle: f64, ow: f64, oh: f64) -> Option<([f64; 4], f64)> {
    let (cx, cy, hw, hh) = inscribed(transform, angle, ow, oh)?;
    let reach = ((cx.abs() + hw) / (ow / 2.0)).max((cy.abs() + hh) / (oh / 2.0));
    let scale = if reach.is_finite() && reach > 1.0 { (1.0 / reach).max(0.1) } else { 1.0 };
    let (cx, cy, hw, hh) = (cx * scale, cy * scale, hw * scale, hh * scale);
    Some(([0.5 + (cx - hw) / ow, 0.5 + (cy - hh) / oh, 0.5 + (cx + hw) / ow, 0.5 + (cy + hh) / oh], scale))
}

/// The centre and half sizes (pixels, about the frame's centre) of [`largest_inside`]'s rectangle
/// before any Scale.
fn inscribed(transform: Transform, angle: f64, ow: f64, oh: f64) -> Option<(f64, f64, f64, f64)> {
    if ow <= 0.0 || oh <= 0.0 {
        return None;
    }
    let quad = lightcraft_quad(&transform.geometry(), angle, ow, oh);
    let area: f64 = (0..4).map(|i| quad[i].0 * quad[(i + 1) % 4].1 - quad[(i + 1) % 4].0 * quad[i].1).sum();
    if !area.is_finite() || area.abs() <= NONE {
        return None;
    }
    let sign = area.signum();
    // where the rectangle's centre may be for a rectangle `k` times the photo's size: inside every
    // edge with every corner
    let centres = |k: f64| {
        let (hw, hh) = (k * ow / 2.0, k * oh / 2.0);
        let mut region = vec![(-10.0 * ow, -10.0 * oh), (10.0 * ow, -10.0 * oh), (10.0 * ow, 10.0 * oh), (-10.0 * ow, 10.0 * oh)];
        for i in 0..4 {
            let (p0, p1) = (quad[i], quad[(i + 1) % 4]);
            let e = (p1.0 - p0.0, p1.1 - p0.1);
            for (cx, cy) in [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)] {
                // sign · cross(e, c + corner − p0) ≥ 0, linear in the centre c
                let a = (-sign * e.1, sign * e.0);
                let b = -sign * (e.0 * (cy - p0.1) - e.1 * (cx - p0.0));
                region = clip(&region, a, b);
                if region.is_empty() {
                    return None;
                }
            }
        }
        let n = region.len() as f64;
        Some((region.iter().map(|p| p.0).sum::<f64>() / n, region.iter().map(|p| p.1).sum::<f64>() / n))
    };
    // centred on the working area (the corrected photo's bounding box), as Capture One does
    let (x0, x1) = quad.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.0), b.max(p.0)));
    let (y0, y1) = quad.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
    let centre = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let fits = |k: f64| {
        centres(k).is_some_and(|_| {
            let (hw, hh) = (k * ow / 2.0, k * oh / 2.0);
            (0..4).all(|i| {
                let (p0, p1) = (quad[i], quad[(i + 1) % 4]);
                let e = (p1.0 - p0.0, p1.1 - p0.1);
                [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)]
                    .iter()
                    .all(|(dx, dy)| sign * (e.0 * (centre.1 + dy - p0.1) - e.1 * (centre.0 + dx - p0.0)) >= -NONE)
            })
        })
    };
    let (mut lo, mut hi) = (0.0, 1.5);
    for _ in 0..48 {
        let mid = (lo + hi) / 2.0;
        if fits(mid) { lo = mid } else { hi = mid }
    }
    (lo > 0.01).then_some((centre.0, centre.1, lo * ow / 2.0, lo * oh / 2.0))
}

/// `polygon` cut to the half-plane `a · p ≥ b`.
fn clip(polygon: &[(f64, f64)], a: (f64, f64), b: f64) -> Vec<(f64, f64)> {
    let inside = |p: (f64, f64)| a.0 * p.0 + a.1 * p.1 - b;
    let mut out = Vec::with_capacity(polygon.len() + 1);
    for (i, &p) in polygon.iter().enumerate() {
        let q = polygon[(i + 1) % polygon.len()];
        let (dp, dq) = (inside(p), inside(q));
        if dp >= 0.0 {
            out.push(p);
        }
        if (dp >= 0.0) != (dq >= 0.0) {
            let t = dp / (dp - dq);
            out.push((p.0 + t * (q.0 - p.0), p.1 + t * (q.1 - p.1)));
        }
    }
    out
}

/// A crop ratio's name as LightCraft's locked aspect, in the crop's own orientation; `None` for
/// the photo's own shape or no lock.
fn aspect(ratio: Option<&str>, landscape: bool) -> Option<(u32, u32)> {
    let (a, b) = match ratio? {
        "Square" => (1, 1),
        name => {
            let (a, b) = name.split_once(['x', 'X', ':'])?;
            (a.trim().parse::<u32>().ok()?, b.trim().parse::<u32>().ok()?)
        }
    };
    if a == 0 || b == 0 {
        return None;
    }
    let (long, short) = (a.max(b), a.min(b));
    Some(if landscape { (long, short) } else { (short, long) })
}
