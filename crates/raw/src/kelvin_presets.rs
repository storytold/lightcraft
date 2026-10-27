//! A Kelvin scale from the camera's own white-balance presets, for raws whose camera has no colour matrices.
//!
//! Sony ARWs from the DSLR-A200 on store, in their encrypted `SR2SubIFD` (see `vendor::sr2`), the multipliers the
//! camera would apply for each of its white-balance presets, among them five at a stated colour temperature:
//! `WB_RGBLevels2500K` (`0x782d`), `3200K` (`0x782c`), `4500K` (`0x7824`), `6000K` (`0x782b`) and `8500K` (`0x782a`;
//! tag numbers from `exiftool -v3`, run as a black-box tool on the files). They are Sony's own reading of its sensor:
//! the as-shot multipliers placed on the curve through them give the temperature Sony's Kelvin dial names for that
//! white. That is all a Kelvin slider needs; the look stays the file's JPEG fit, and the picture is adapted from the
//! as-shot white with Bradford, as for a camera read through measured matrices (`crate::spectral`, issue #730).
//!
//! Canon CR2s list theirs by name in the maker note's `ColorData`, each with Canon's temperature for it (see
//! `vendor::cr2`): five planckian ones (Daylight, Shade, Cloudy, Tungsten, Flash) make the curve the same way,
//! taken in Kelvin order ([`KelvinPresets::from_unordered`]). On the PowerShot G1 X Mark III's nine settings the
//! curve reads the as-shot levels within 5 % of the camera's own `ColorTempAsShot`.
//!
//! The model, in `(ln R/G, ln B/G)` of the multipliers ([`KelvinPresets::temp_tint`]):
//! - The presets (three or more) are joined piecewise linearly, with mired linear along each segment and the end segments
//!   extended. On every Sony body measured, mired is nearly linear in arc length along the whole curve (2500 K to
//!   8500 K within ±3 %), so the segments add no visible kinks.
//! - A white off the curve is placed by its perpendicular offset from the nearest segment. Tint is that offset scaled
//!   by the segment's own spacing of the Planckian locus in CIE 1960 (u, v), per unit arc length.
//! - The isotherms are not perpendicular to the curve in these coordinates: an offset also changes mired by
//!   [`ISOTHERM_TILT`] times the mired the same distance along the curve would.
//!
//! The two constants come from the 14 Sony bodies that have both measured matrices and the presets (one CC0
//! raw.pixls.us sample each for the presets, a grid of multipliers through `crate::spectral` for the matrices):
//! the tilt measured 0.59–0.84 (mean 0.74), the tint spacing 0.90–1.04 of the locus's (mean 0.98, taken as 1).
//! On 150 ILCE-7SM2 photos read both ways, the temperatures agree to ±4 mired around a constant offset (Sony's
//! Kelvin is about 8 % above the matrices', as #730 found for the ILCE-7M3's dial) and the tint to −9 ± 4: the
//! matrices put Sony's presets slightly off the locus, the presets themselves are tint 0 here.
use crate::ColorData;
use lightcraft_color::{Xy, cct};
use serde::{Deserialize, Serialize};

/// Mired change per unit offset from the preset curve, in units of the mired change per unit distance along it.
pub const ISOTHERM_TILT: f64 = 0.74;

/// The slider's range: an as-shot reading is kept inside it.
const MIN_K: f64 = 2000.0;
const MAX_K: f64 = 50000.0;
const MAX_TINT: f64 = 150.0;

/// Models whose white balance is edited in Kelvin through their presets, as (brand, Exif model without the brand),
/// each checked to carry them: the Sony bodies on 80–150 own photos each, or the public corpus samples for the
/// ILCE-3500 and ILCE-7S; the Canon PowerShot G1 X Mark III on a reporter's nine photos, one per white-balance
/// setting (issue #157 of a downstream fork). A camera with measured matrices (`crate::spectral`) is read through
/// those instead.
const MODELS: &[(&str, &str)] = &[
    ("SONY", "DSC-RX10"),
    ("SONY", "ILCA-77M2"),
    ("SONY", "ILCA-99M2"),
    ("SONY", "ILCE-3500"),
    ("SONY", "ILCE-7S"),
    ("SONY", "SLT-A33"),
    ("CANON", "PowerShot G1 X Mark III"),
];

/// Whether a raw of this Exif make and model is edited in Kelvin through its presets (when it has no colour
/// matrices of its own or measured: see [`crate::spectral::find`], which comes first), from the file's presets or
/// built-in ones ([`crate::kelvin_builtin`]).
pub fn covers(make: Option<&str>, model: &str) -> bool {
    if crate::kelvin_builtin::covers(make, model) {
        return true;
    }
    let model = model.trim();
    let brand = make.and_then(|m| m.split_whitespace().next());
    MODELS.iter().any(|(b, m)| {
        let bare = model
            .get(..b.len())
            .zip(model.get(b.len()..))
            .filter(|(p, rest)| p.eq_ignore_ascii_case(b) && rest.starts_with(' '))
            .map_or(model, |(_, rest)| rest.trim_start());
        brand.is_none_or(|x| x.eq_ignore_ascii_case(b)) && bare.eq_ignore_ascii_case(m)
    })
}

/// [`covers`] for make and model joined by a space, as the catalog keeps them (`"SONY ILCA-99M2"`).
pub fn covers_camera(camera: &str) -> bool {
    let words: Vec<&str> = camera.split_whitespace().collect();
    // the make can be several words ("NIKON CORPORATION"), so try every split, as `spectral::find_camera` does
    (1..words.len()).any(|i| covers(Some(&words[..i].join(" ")), &words[i..].join(" "))) || covers(None, camera)
}

/// Most points a curve keeps (Canon writes about seven at a stated temperature, Sony five).
const MAX_POINTS: usize = 32;

/// A camera's multipliers at stated temperatures (at least three), ascending, green = 1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KelvinPresets {
    points: Vec<(f32, [f32; 3])>,
}

fn usable(m: [f32; 3]) -> bool {
    m.iter().all(|v| *v > 0.0 && v.is_finite())
}

fn log_point(m: [f32; 3]) -> [f64; 2] {
    [(m[0] as f64 / m[1] as f64).ln(), (m[2] as f64 / m[1] as f64).ln()]
}

fn locus_uv(t: f64) -> (f64, f64) {
    cct::xy_to_uv(cct::temp_tint_to_xy(t, 0.0))
}

impl KelvinPresets {
    /// `points` as (Kelvin, multipliers), ascending in Kelvin; `None` unless they make a curve: three to
    /// [`MAX_POINTS`] of them, positive multipliers, red rising and blue falling from one preset to the next.
    pub fn new(points: impl IntoIterator<Item = (f32, [f32; 3])>) -> Option<KelvinPresets> {
        let points: Vec<_> = points.into_iter().take(MAX_POINTS + 1).collect();
        let ok = (3..=MAX_POINTS).contains(&points.len())
            && points.iter().all(|(k, m)| *k > 0.0 && k.is_finite() && usable(*m))
            && points.windows(2).all(|w| {
                let (a, b) = (log_point(w[0].1), log_point(w[1].1));
                w[0].0 < w[1].0 && b[0] > a[0] && b[1] < a[1]
            });
        ok.then(|| KelvinPresets { points: points.into_iter().map(|(k, m)| (k, [m[0] / m[1], 1.0, m[2] / m[1]])).collect() })
    }

    /// A curve from presets in any order, as a camera lists them by name (Canon's Daylight, Shade, Cloudy, …):
    /// sorted by Kelvin, a repeated temperature kept once, unusable points and any that would turn the curve back
    /// (red not rising or blue not falling from the warmer neighbour kept) dropped; then [`Self::new`].
    pub fn from_unordered(points: impl IntoIterator<Item = (f32, [f32; 3])>) -> Option<KelvinPresets> {
        let mut pts: Vec<_> = points.into_iter().take(MAX_POINTS).filter(|(k, m)| *k > 0.0 && k.is_finite() && usable(*m)).collect();
        pts.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut kept: Vec<(f32, [f32; 3])> = Vec::with_capacity(pts.len());
        for p in pts {
            let fits = kept.last().is_none_or(|q| {
                let (a, b) = (log_point(q.1), log_point(p.1));
                q.0 < p.0 && b[0] > a[0] && b[1] < a[1]
            });
            if fits {
                kept.push(p);
            }
        }
        KelvinPresets::new(kept)
    }

    /// Temperature (K) and tint of the white these multipliers balance (see the module docs); `None` for unusable
    /// multipliers.
    pub fn temp_tint(&self, wb_multipliers: [f32; 3]) -> Option<(f64, f64)> {
        if !usable(wb_multipliers) {
            return None;
        }
        let q = log_point(wb_multipliers);
        let last = self.points.len().saturating_sub(2);
        let mut best: Option<(f64, f64, f64)> = None;
        for (i, w) in self.points.windows(2).enumerate() {
            let ((k0, m0), (k1, m1)) = (w[0], w[1]);
            let (p0, p1) = (log_point(m0), log_point(m1));
            let d = [p1[0] - p0[0], p1[1] - p0[1]];
            let len = d[0].hypot(d[1]);
            // only a deserialized curve can repeat a point
            if !(len > 0.0 && len.is_finite()) {
                continue;
            }
            let t = [d[0] / len, d[1] / len];
            // the normal towards higher red and blue: a green white
            let n = if t[0] - t[1] < 0.0 { [t[1], -t[0]] } else { [-t[1], t[0]] };
            let r = [q[0] - p0[0], q[1] - p0[1]];
            let along = (r[0] * t[0] + r[1] * t[1]) / len;
            let off = r[0] * n[0] + r[1] * n[1];
            let lo = if i == 0 { f64::NEG_INFINITY } else { 0.0 };
            let hi = if i == last { f64::INFINITY } else { 1.0 };
            let c = along.clamp(lo, hi);
            let dist = (r[0] - c * d[0]).hypot(r[1] - c * d[1]);
            let (mired0, mired1) = (1e6 / k0 as f64, 1e6 / k1 as f64);
            let mired_per_len = (mired1 - mired0) / len;
            let ((u0, v0), (u1, v1)) = (locus_uv(k0 as f64), locus_uv(k1 as f64));
            let tint_per_len = cct::TINT_SCALE * (u1 - u0).hypot(v1 - v0) / len;
            let mired = mired0 + along * (mired1 - mired0) + ISOTHERM_TILT * mired_per_len * off;
            if best.is_none_or(|b| dist < b.0) {
                best = Some((dist, mired, tint_per_len * off));
            }
        }
        let (_, mired, tint) = best?;
        let t = 1e6 / mired.clamp(1e6 / MAX_K, 1e6 / MIN_K);
        Some((t, tint.clamp(-MAX_TINT, MAX_TINT)))
    }

    /// The as-shot white of a file through its presets: [`Self::temp_tint`] of its multipliers as a chromaticity.
    pub fn as_shot_white(&self, wb_multipliers: Option<[f32; 3]>) -> Option<Xy> {
        let (t, tint) = self.temp_tint(wb_multipliers?)?;
        Some(cct::temp_tint_to_xy(t, tint))
    }
}

/// The as-shot white of a file through its own presets ([`ColorData::kelvin_presets`]); `None` without presets or
/// usable multipliers.
pub fn as_shot_white(color: &ColorData, wb_multipliers: Option<[f32; 3]>) -> Option<Xy> {
    color.kelvin_presets.as_ref()?.as_shot_white(wb_multipliers)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ILCA-99M2's presets (every file of the body carries the same).
    fn a99m2() -> KelvinPresets {
        let g = |r: f32, b: f32| [r / 1024.0, 1.0, b / 1024.0];
        KelvinPresets::new([
            (2500.0, g(1336.0, 3672.0)),
            (3200.0, g(1720.0, 2620.0)),
            (4500.0, g(2256.0, 1880.0)),
            (6000.0, g(2680.0, 1544.0)),
            (8500.0, g(3136.0, 1300.0)),
        ])
        .unwrap()
    }

    #[test]
    fn a_preset_reads_as_its_own_temperature_with_no_tint() {
        let p = a99m2();
        for &(k, m) in &p.points {
            let (t, tint) = p.temp_tint(m).unwrap();
            assert!((t - k as f64).abs() < 0.5 && tint.abs() < 1e-6, "{k}: {t} {tint}");
            // the scale of the multipliers doesn't matter, only their ratios to green
            let (t2, tint2) = p.temp_tint(m.map(|v| v * 2.3)).unwrap();
            assert!((t2 - t).abs() < 1e-3 && (tint2 - tint).abs() < 1e-4, "{t2} {tint2}");
        }
    }

    #[test]
    fn between_presets_the_temperature_follows_the_curve() {
        let p = a99m2();
        let mid = |a: [f32; 3], b: [f32; 3]| [(a[0] * b[0]).sqrt(), 1.0, (a[2] * b[2]).sqrt()];
        let (t, tint) = p.temp_tint(mid(p.points[2].1, p.points[3].1)).unwrap();
        assert!((4500.0..6000.0).contains(&t) && tint.abs() < 1.0, "{t} {tint}");
        // beyond the ends: extended, then held to the slider's range
        let (hot, _) = p.temp_tint([3.5, 1.0, 1.15]).unwrap();
        let (cold, _) = p.temp_tint([1.15, 1.0, 4.0]).unwrap();
        let (coldest, _) = p.temp_tint([0.3, 1.0, 20.0]).unwrap();
        assert!((8500.0..MAX_K).contains(&hot) && (MIN_K..2500.0).contains(&cold) && coldest == MIN_K, "{hot} {cold} {coldest}");
    }

    #[test]
    fn less_green_in_the_multipliers_is_a_green_white_and_a_magenta_correction() {
        let p = a99m2();
        let day = [2680.0 / 1024.0, 1.0, 1544.0 / 1024.0];
        let (t0, _) = p.temp_tint(day).unwrap();
        // a green light: the camera raises red and blue against green
        let (t1, tint1) = p.temp_tint([day[0] * 1.08, 1.0, day[2] * 1.08]).unwrap();
        assert!(tint1 > 15.0 && tint1 < 40.0, "{tint1}");
        // the tilted isotherm: a higher temperature than the curve point it sits beside
        assert!(t1 > t0 + 100.0, "{t1} {t0}");
        let (_, magenta) = p.temp_tint([day[0] / 1.08, 1.0, day[2] / 1.08]).unwrap();
        assert!(magenta < -15.0, "{magenta}");
    }

    #[test]
    fn unusable_presets_or_multipliers_give_nothing() {
        let p = a99m2();
        assert_eq!(p.temp_tint([0.0, 1.0, 1.0]), None);
        assert_eq!(p.temp_tint([f32::NAN, 1.0, 1.0]), None);
        assert_eq!(p.as_shot_white(None), None);
        let mut pts = p.points.clone();
        pts.swap(1, 2);
        assert_eq!(KelvinPresets::new(pts), None, "out of order");
        let mut pts = p.points.clone();
        pts[4].1 = pts[3].1;
        assert_eq!(KelvinPresets::new(pts), None, "no step");
        let mut pts = p.points.clone();
        pts[0].1[0] = 0.0;
        assert_eq!(KelvinPresets::new(pts), None);
        assert_eq!(as_shot_white(&ColorData::default(), Some([2.0, 1.0, 1.5])), None);
        assert_eq!(KelvinPresets::new(p.points[..2].to_vec()), None, "too few");
        let with = ColorData { kelvin_presets: Some(p), ..Default::default() };
        assert!(as_shot_white(&with, Some([2.0, 1.0, 1.5])).is_some());
    }

    /// Canon lists its presets by name, not by temperature, with repeats and an off-curve one among them: the
    /// curve takes them in Kelvin order, each temperature once, and leaves out a point that would turn it back.
    #[test]
    fn a_curve_from_presets_in_any_order() {
        let g = |r: f32, b: f32| [r / 1024.0, 1.0, b / 1024.0];
        let p = KelvinPresets::from_unordered([
            (5651.0, g(1418.0, 1357.0)),
            (7505.0, g(1653.0, 1171.0)),
            (6594.0, g(1533.0, 1259.0)),
            (3174.0, g(963.0, 2016.0)),
            (5651.0, g(1418.0, 1357.0)),
            (8000.0, g(1400.0, 1400.0)),
            (8529.0, g(1807.0, 1080.0)),
            (f32::NAN, g(1000.0, 1000.0)),
        ])
        .unwrap();
        assert_eq!(p.points.len(), 5);
        assert!(p.points.windows(2).all(|w| w[0].0 < w[1].0));
        let (t, tint) = p.temp_tint(g(1653.0, 1171.0)).unwrap();
        assert!((t - 7505.0).abs() < 1.0 && tint.abs() < 0.1, "{t} {tint}");
        assert_eq!(KelvinPresets::from_unordered([(5000.0, g(1400.0, 1400.0)), (6000.0, g(1500.0, 1300.0))]), None);
        assert_eq!(KelvinPresets::from_unordered(std::iter::empty()), None);
        // a stored curve with a repeated point (only deserializing can make one) reads without dividing by zero
        let bad = KelvinPresets { points: vec![(5000.0, g(1400.0, 1400.0)); 3] };
        assert_eq!(bad.temp_tint(g(1400.0, 1400.0)), None);
        assert_eq!(KelvinPresets { points: vec![] }.temp_tint(g(1400.0, 1400.0)), None);
    }

    #[test]
    fn covered_models() {
        assert!(covers_camera("NIKON CORPORATION NIKON Z5_2") && covers_camera("NIKON Z5_2") && !covers_camera("NIKON CORPORATION NIKON Z6"));
        for c in [
            "SONY ILCA-99M2",
            "SONY DSC-RX10",
            "SONY ILCA-77M2",
            "SONY SLT-A33",
            "ILCA-99M2",
            "Sony ilca-99m2",
            "Canon Canon PowerShot G1 X Mark III",
            "Canon PowerShot G1 X Mark III",
        ] {
            assert!(covers_camera(c), "{c}");
        }
        for c in
            ["SONY ILCE-7M3", "SONY DSC-RX10M4", "Canon SLT-A33", "", "SONY", "Canon Canon PowerShot G1 X Mark II", "SONY PowerShot G1 X Mark III"]
        {
            assert!(!covers_camera(c), "{c}");
        }
        assert!(covers(Some("SONY"), "ILCA-99M2") && covers(None, "ILCA-99M2") && !covers(Some("NIKON CORPORATION"), "ILCA-99M2"));
    }
}
