//! Lens correction profiles: distortion, transverse chromatic aberration (TCA) and vignetting.
//!
//! - [`model`]: the radial models (distortion poly3 / poly5 / PTLens, TCA linear / poly3, vignetting `pa`), and the
//!   least-squares conversion to the DNG opcode polynomials (`WarpRectilinear`, `FixVignetteRadial`) the pipeline
//!   already applies in its single geometry resample.
//! - [`LensDb`]: profiles read from lensfun-format XML files in a directory the user supplies. The lensfun database
//!   is CC-BY-SA; it is never bundled with the code or the binary, only read at run time from where the user put it.
//! - [`LensDb::find_camera`] / [`LensDb::find_lens`]: identification from EXIF make / model / lens model strings,
//!   with mount and crop-factor handling.
//! - [`Profile::correction`]: the profile at a shot's focal length (and aperture / distance for vignetting),
//!   interpolated between calibrated points, expressed as [`lightcraft_develop::EmbeddedLens`] for a given image.
//!
//! Sources: the lensfun database format and model formulas as described in lensfun's published documentation
//! (database file format and lens calibration pages; no lensfun code was read), Panorama Tools' distortion model
//! (a, b, c) as documented by the PanoTools wiki, Adobe DNG specification 1.6 (opcodes `WarpRectilinear`,
//! `FixVignetteRadial`). Interpolation and fitting: own design.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod model;
mod xml;

use std::path::Path;

use lightcraft_develop::{EmbeddedLens, EmbeddedVignette, EmbeddedWarp};
use lightcraft_geom::Point;
pub use model::{Distortion, Tca, Vignetting};

/// Largest database file read (lensfun's biggest file is well under 1 MB).
const MAX_FILE_BYTES: u64 = 16 << 20;
/// Most files read from one directory.
const MAX_FILES: usize = 2000;
/// Largest radial error (half-diagonal units) accepted when converting a distortion to the DNG polynomial.
pub const MAX_WARP_ERROR: f64 = 0.001;
/// Largest relative error accepted when converting vignetting to the DNG polynomial.
pub const MAX_VIGNETTE_ERROR: f64 = 0.01;

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    Io(String),
    Xml(String),
    TooLarge,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "lens database: {e}"),
            Error::Xml(e) => write!(f, "lens database: invalid XML: {e}"),
            Error::TooLarge => write!(f, "lens database: file too large or too deeply nested"),
        }
    }
}

impl std::error::Error for Error {}

/// A camera body: what is needed to pick lenses (mount) and scale profiles (crop factor).
#[derive(Clone, Debug, PartialEq)]
pub struct Camera {
    pub maker: String,
    pub model: String,
    pub mount: String,
    pub crop_factor: f64,
}

/// One calibrated vignetting point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VignettingPoint {
    pub focal: f64,
    pub aperture: f64,
    /// Focus distance, metres (lensfun uses 1000 for infinity).
    pub distance: f64,
    pub model: Vignetting,
}

/// A lens profile: names, mounts, the calibration frame and the calibrated points.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub maker: String,
    /// Every name the lens goes by (EXIF variants, translations).
    pub models: Vec<String>,
    pub mounts: Vec<String>,
    /// Crop factor of the camera the calibration was made on.
    pub crop_factor: f64,
    /// Long / short side of the calibration frame.
    pub aspect_ratio: f64,
    /// False for fisheye and other non-rectilinear projections: distortion is then not applied.
    pub rectilinear: bool,
    /// Sorted by focal length.
    pub distortion: Vec<(f64, Distortion)>,
    pub tca: Vec<(f64, Tca)>,
    pub vignetting: Vec<VignettingPoint>,
}

/// The shot a correction is wanted for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shot {
    /// Focal length, mm.
    pub focal: f64,
    pub aperture: Option<f64>,
    /// Focus distance, metres.
    pub distance: Option<f64>,
    /// Crop factor of the recorded frame: the camera's (from [`LensDb::find_camera`]), or larger when the camera
    /// recorded a crop of its sensor; the profile's own when unknown.
    pub crop_factor: Option<f64>,
    /// Image size in pixels (the frame the correction is expressed for; the optical centre is its centre).
    pub width: f64,
    pub height: f64,
}

/// A set of lens and camera profiles.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LensDb {
    pub cameras: Vec<Camera>,
    pub lenses: Vec<Profile>,
}

impl LensDb {
    /// Read every `*.xml` file in `dir` (lensfun database format). Unreadable or invalid files are skipped and
    /// reported in the second value; only a directory that can't be listed is an error.
    pub fn load_dir(dir: &Path) -> Result<(LensDb, Vec<(String, Error)>), Error> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| Error::Io(format!("{}: {e}", dir.display())))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("xml")))
            .collect();
        entries.sort();
        entries.truncate(MAX_FILES);
        let mut db = LensDb::default();
        let mut errors = Vec::new();
        for path in entries {
            let name = path.display().to_string();
            match read_capped(&path).and_then(|s| LensDb::parse(&s)) {
                Ok(part) => db.extend(part),
                Err(e) => errors.push((name, e)),
            }
        }
        Ok((db, errors))
    }

    /// Parse one lensfun-format XML document.
    pub fn parse(xml: &str) -> Result<LensDb, Error> {
        let root = xml::parse(xml)?;
        let Some(top) = root.kid("lensdatabase") else { return Err(Error::Xml("no <lensdatabase> element".into())) };
        let cameras = top.kids("camera").filter_map(camera).collect();
        let lenses = top.kids("lens").flat_map(lens).collect();
        Ok(LensDb { cameras, lenses })
    }

    pub fn extend(&mut self, other: LensDb) {
        self.cameras.extend(other.cameras);
        self.lenses.extend(other.lenses);
    }

    pub fn is_empty(&self) -> bool {
        self.cameras.is_empty() && self.lenses.is_empty()
    }

    /// The camera with this EXIF make and model.
    pub fn find_camera(&self, make: &str, model: &str) -> Option<&Camera> {
        let (mk, md) = (tokens(make), tokens(model));
        if md.is_empty() {
            return None;
        }
        self.cameras.iter().find(|c| tokens(&c.model) == md && maker_matches(&tokens(&c.maker), &mk))
    }

    /// The best profile for an EXIF lens model (and lens make, when the file has one), preferring lenses for the
    /// camera's mount. `None` when no name is a convincing match.
    pub fn find_lens(&self, lens_make: Option<&str>, lens_model: &str, camera: Option<&Camera>) -> Option<&Profile> {
        self.best_lens(lens_make, lens_model, camera, |_| true)
    }

    /// [`find_lens`](Self::find_lens) among the profiles one of whose names is exactly `name`
    /// ([`Profile::names_exactly`]), for names that carry no maker, such as [`spec_name`]s: another maker's lens with
    /// the same specification scores as high, and must neither win nor, by coming first, hide the profile named so.
    pub fn find_lens_named(&self, lens_make: Option<&str>, name: &str, camera: Option<&Camera>) -> Option<&Profile> {
        self.best_lens(lens_make, name, camera, |l| l.names_exactly(name))
    }

    fn best_lens(&self, lens_make: Option<&str>, lens_model: &str, camera: Option<&Camera>, eligible: impl Fn(&Profile) -> bool) -> Option<&Profile> {
        let exif = tokens(lens_model);
        if exif.is_empty() {
            return None;
        }
        let make = lens_make.map(tokens).unwrap_or_default();
        let mut best: Option<(f64, &Profile)> = None;
        for lens in self.lenses.iter().filter(|l| eligible(l)) {
            let maker = tokens(&lens.maker);
            if !make.is_empty() && !maker.is_empty() && !maker_matches(&maker, &make) {
                continue;
            }
            let mut score = lens.models.iter().map(|m| name_score(&tokens(m), &exif, &maker)).fold(0.0, f64::max);
            if let Some(cam) = camera
                && !lens.mounts.is_empty()
                && !lens.mounts.iter().any(|m| m.eq_ignore_ascii_case(&cam.mount))
            {
                score *= 0.8;
            }
            if score >= 0.6 && best.is_none_or(|(s, _)| score > s) {
                best = Some((score, lens));
            }
        }
        best.map(|(_, l)| l)
    }
}

fn read_capped(path: &Path) -> Result<String, Error> {
    let len = std::fs::metadata(path).map_err(|e| Error::Io(e.to_string()))?.len();
    if len > MAX_FILE_BYTES {
        return Err(Error::TooLarge);
    }
    let bytes = std::fs::read(path).map_err(|e| Error::Io(e.to_string()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Lower-case word tokens; letters and digits are split ("24-105mm" → 24, 105, mm), decimals kept ("2.8").
fn tokens(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_digit: Option<bool> = None;
    for c in s.chars().flat_map(char::to_lowercase) {
        let digit = c.is_ascii_digit() || (c == '.' && prev_digit == Some(true));
        if c.is_alphanumeric() || digit {
            if prev_digit.is_some_and(|p| p != digit) && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.push(c);
            prev_digit = Some(digit);
        } else {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_digit = None;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    for t in &mut out {
        // "4.0" and "4" are the same aperture
        if t.contains('.') && t.starts_with(|c: char| c.is_ascii_digit()) {
            let trimmed = t.trim_end_matches('0').trim_end_matches('.');
            *t = trimmed.to_string();
        }
    }
    out.retain(|t| !t.is_empty());
    out
}

fn is_number(t: &str) -> bool {
    t.starts_with(|c: char| c.is_ascii_digit())
}

/// Makers match when one's first word appears in the other ("NIKON CORPORATION" / "Nikon").
fn maker_matches(a: &[String], b: &[String]) -> bool {
    match (a.first(), b.first()) {
        (Some(x), Some(y)) => b.contains(x) || a.contains(y),
        _ => true,
    }
}

/// How well a database name matches the EXIF name, 0..1. Every number in the database name (focal lengths,
/// apertures) must appear in the EXIF name; the maker's words are ignored on both sides (EXIF often omits them).
fn name_score(db: &[String], exif: &[String], maker: &[String]) -> f64 {
    let db: Vec<&String> = db.iter().filter(|t| !maker.contains(t)).collect();
    let exif: Vec<&String> = exif.iter().filter(|t| !maker.contains(t)).collect();
    if db.is_empty() || exif.is_empty() {
        return 0.0;
    }
    if db.iter().any(|t| is_number(t) && !exif.contains(t)) || exif.iter().any(|t| is_number(t) && !db.contains(t)) {
        return 0.0;
    }
    let common = db.iter().filter(|t| exif.contains(t)).count() as f64;
    common / db.len().max(exif.len()) as f64
}

fn camera(n: &xml::Node) -> Option<Camera> {
    Some(Camera {
        maker: n.kid_text("maker")?.to_string(),
        model: n.kid_text("model")?.to_string(),
        mount: n.kid_text("mount").unwrap_or_default().to_string(),
        crop_factor: crop_of(n.kid_text("cropfactor")).unwrap_or(1.0),
    })
}

fn crop_of(s: Option<&str>) -> Option<f64> {
    s.and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite() && (0.05..=20.0).contains(v))
}

fn aspect_of(s: Option<&str>) -> Option<f64> {
    let s = s?.trim();
    let v = match s.split_once(':') {
        Some((a, b)) => a.trim().parse::<f64>().ok()? / b.trim().parse::<f64>().ok()?,
        None => s.parse::<f64>().ok()?,
    };
    let v = if v < 1.0 { 1.0 / v } else { v };
    (v.is_finite() && (1.0..=10.0).contains(&v)).then_some(v)
}

fn positive(v: Option<f64>) -> Option<f64> {
    v.filter(|v| *v > 0.0 && *v < 1e6)
}

/// One `<lens>` element → one profile per `<calibration>` (newer files give each its own crop factor and aspect).
fn lens(n: &xml::Node) -> Vec<Profile> {
    let Some(maker) = n.kid_text("maker") else { return Vec::new() };
    let models: Vec<String> = n.kids("model").map(|m| m.text.trim().to_string()).filter(|m| !m.is_empty()).collect();
    if models.is_empty() {
        return Vec::new();
    }
    let crop = crop_of(n.kid_text("cropfactor")).unwrap_or(1.0);
    let aspect = aspect_of(n.kid_text("aspect-ratio")).unwrap_or(1.5);
    let rectilinear = n.kid_text("type").is_none_or(|t| t.eq_ignore_ascii_case("rectilinear"));
    let mounts = n.kids("mount").map(|m| m.text.trim().to_string()).filter(|m| !m.is_empty()).collect::<Vec<_>>();
    n.kids("calibration")
        .map(|cal| {
            let mut p = Profile {
                maker: maker.to_string(),
                models: models.clone(),
                mounts: mounts.clone(),
                crop_factor: crop_of(cal.attr("cropfactor")).unwrap_or(crop),
                aspect_ratio: aspect_of(cal.attr("aspect-ratio")).unwrap_or(aspect),
                rectilinear,
                distortion: Vec::new(),
                tca: Vec::new(),
                vignetting: Vec::new(),
            };
            for c in &cal.children {
                let Some(focal) = positive(c.num("focal")) else { continue };
                let k = |name: &str| c.num(name).unwrap_or(0.0);
                match (c.name.as_str(), c.attr("model")) {
                    ("distortion", Some("poly3")) => p.distortion.push((focal, Distortion::Poly3 { k1: k("k1") })),
                    ("distortion", Some("poly5")) => p.distortion.push((focal, Distortion::Poly5 { k1: k("k1"), k2: k("k2") })),
                    ("distortion", Some("ptlens")) => p.distortion.push((focal, Distortion::PtLens { a: k("a"), b: k("b"), c: k("c") })),
                    ("tca", Some("linear")) => {
                        let one = |name: &str| c.num(name).unwrap_or(1.0);
                        p.tca.push((focal, Tca::Linear { kr: one("kr"), kb: one("kb") }));
                    }
                    ("tca", Some("poly3")) => {
                        let one = |name: &str| c.num(name).unwrap_or(1.0);
                        p.tca.push((focal, Tca::Poly3 { vr: one("vr"), vb: one("vb"), cr: k("cr"), cb: k("cb"), br: k("br"), bb: k("bb") }));
                    }
                    ("vignetting", Some("pa")) => {
                        let (Some(aperture), distance) = (positive(c.num("aperture")), positive(c.num("distance")).unwrap_or(1000.0)) else {
                            continue;
                        };
                        let model = Vignetting { k1: k("k1"), k2: k("k2"), k3: k("k3") };
                        p.vignetting.push(VignettingPoint { focal, aperture, distance, model });
                    }
                    _ => {}
                }
            }
            p.distortion.sort_by(|a, b| a.0.total_cmp(&b.0));
            p.tca.sort_by(|a, b| a.0.total_cmp(&b.0));
            p
        })
        .filter(|p| !p.distortion.is_empty() || !p.tca.is_empty() || !p.vignetting.is_empty())
        .collect()
}

/// Linear interpolation weights in a sorted key list: the bracketing indices and the weight of the upper one.
fn bracket(keys: &[f64], x: f64) -> Option<(usize, usize, f64)> {
    let last = keys.len().checked_sub(1)?;
    let hi = keys.iter().position(|&k| k >= x).unwrap_or(last);
    let lo = hi.saturating_sub(1);
    let (a, b) = (keys.get(lo).copied()?, keys.get(hi).copied()?);
    if x <= a || b <= a {
        return Some((lo, lo, 0.0));
    }
    if x >= b {
        return Some((hi, hi, 0.0));
    }
    Some((lo, hi, (x - a) / (b - a)))
}

/// A value interpolated in focal length between calibrated points, by evaluating both neighbours.
fn by_focal<T: Copy>(points: &[(f64, T)], focal: f64, eval: impl Fn(&T) -> f64) -> Option<f64> {
    let keys: Vec<f64> = points.iter().map(|p| p.0).collect();
    let (lo, hi, t) = bracket(&keys, focal)?;
    let (a, b) = (eval(&points.get(lo)?.1), eval(&points.get(hi)?.1));
    Some(a + (b - a) * t)
}

/// A lens name built from a lens specification (min / max focal length, f-number at each), for files that name
/// no lens model, only its specification (Nikon DSLRs' maker note): "24-70mm f/2.8", "18-55mm f/3.5-5.6",
/// "85mm f/1.8". Such a name says nothing about the maker, so [`Profile::names_exactly`] must confirm a match.
pub fn spec_name(spec: [f64; 4]) -> String {
    fn num(v: f64) -> String {
        let t = format!("{v:.1}");
        t.trim_end_matches('0').trim_end_matches('.').to_string()
    }
    fn range(a: f64, b: f64) -> String {
        if (a - b).abs() < 0.05 { num(a) } else { format!("{}-{}", num(a), num(b)) }
    }
    format!("{}mm f/{}", range(spec[0], spec[1]), range(spec[2], spec[3]))
}

impl Profile {
    /// Whether one of the profile's names is exactly `name` (same words and numbers, ignoring case and
    /// punctuation): the check for names that carry no maker, such as [`spec_name`]s, where the best partial match
    /// could be another maker's lens with the same focal range. The maker's words count too, so "Samyang 85mm f/1.4"
    /// is not "85mm f/1.4"; only a name given for the purpose (a user's alias) matches.
    pub fn names_exactly(&self, name: &str) -> bool {
        let want = tokens(name);
        !want.is_empty() && self.models.iter().any(|m| tokens(m) == want)
    }

    /// Distortion ratio `r_d / r_u` at lensfun radius `r` for `focal` (1 when the profile has none).
    pub fn distortion_ratio(&self, focal: f64, r: f64) -> f64 {
        if !self.rectilinear {
            return 1.0;
        }
        by_focal(&self.distortion, focal, |d| d.ratio(r)).unwrap_or(1.0)
    }

    /// TCA ratio for channel `ch` at lensfun radius `r` for `focal`.
    pub fn tca_ratio(&self, focal: f64, ch: usize, r: f64) -> f64 {
        by_focal(&self.tca, focal, |t| t.ratio(ch, r)).unwrap_or(1.0)
    }

    /// Vignetting attenuation at radius `r` (half-diagonal units): linear in focal length, in stops of aperture and
    /// in 1 / distance between the calibrated points.
    pub fn vignetting_attenuation(&self, focal: f64, aperture: Option<f64>, distance: Option<f64>, r: f64) -> Option<f64> {
        let mut focals: Vec<f64> = self.vignetting.iter().map(|v| v.focal).collect();
        focals.sort_by(f64::total_cmp);
        focals.dedup();
        let (lo, hi, t) = bracket(&focals, focal)?;
        let at = |f: f64| -> Option<f64> {
            let pts: Vec<&VignettingPoint> = self.vignetting.iter().filter(|v| v.focal == f).collect();
            // the calibrated distance nearest the shot's (in 1/d; infinity when unknown)
            let inv = |d: f64| 1.0 / d.max(0.01);
            let want = distance.map_or(0.0, inv);
            let near = pts.iter().map(|v| v.distance).min_by(|a, b| (inv(*a) - want).abs().total_cmp(&(inv(*b) - want).abs()))?;
            let mut at_d: Vec<&&VignettingPoint> = pts.iter().filter(|v| v.distance == near).collect();
            at_d.sort_by(|a, b| a.aperture.total_cmp(&b.aperture));
            let stops: Vec<f64> = at_d.iter().map(|v| v.aperture.log2()).collect();
            // unknown aperture: the widest calibrated one (the strongest falloff)
            let n = aperture.filter(|a| *a > 0.0).map_or(stops.first().copied()?, f64::log2);
            let (i, j, u) = bracket(&stops, n)?;
            let (a, b) = (at_d.get(i)?.model.attenuation(r), at_d.get(j)?.model.attenuation(r));
            Some(a + (b - a) * u)
        };
        let (a, b) = (at(*focals.get(lo)?)?, at(*focals.get(hi)?)?);
        Some(a + (b - a) * t)
    }

    /// The correction for `shot`, as the DNG opcode polynomials the pipeline applies, centred on the image.
    /// Distortion and TCA are combined per plane (R, G, B); a model the polynomial can't follow closely
    /// ([`MAX_WARP_ERROR`], [`MAX_VIGNETTE_ERROR`]) or that would fold the image is left out.
    pub fn correction(&self, shot: &Shot) -> Option<EmbeddedLens> {
        let (w, h) = (shot.width, shot.height);
        if !(w >= 2.0 && h >= 2.0 && w.is_finite() && h.is_finite() && shot.focal.is_finite() && shot.focal > 0.0) {
            return None;
        }
        let cam = shot.crop_factor.filter(|c| c.is_finite() && *c > 0.0).unwrap_or(self.crop_factor);
        // DNG radius (1 = image half diagonal) → lensfun radii
        let to_vig = self.crop_factor / cam;
        let to_dist = to_vig * (1.0 + self.aspect_ratio * self.aspect_ratio).sqrt();
        let center = Point::new(0.5, 0.5);
        let radius = w.hypot(h) / 2.0 / w.max(h);
        let f = shot.focal;
        let mut lens = EmbeddedLens::default();
        let has_dist = self.rectilinear && !self.distortion.is_empty();
        if has_dist || !self.tca.is_empty() {
            let plane = |ch: usize| -> Option<[f64; 6]> {
                let ratio = |r: f64| {
                    let rl = r * to_dist;
                    let d = self.distortion_ratio(f, rl);
                    d * self.tca_ratio(f, ch, rl * d)
                };
                let (k, err) = model::fit_even4(ratio)?;
                (err <= MAX_WARP_ERROR).then_some([k[0], k[1], k[2], k[3], 0.0, 0.0])
            };
            if let (Some(r), Some(g), Some(b)) = (plane(0), plane(1), plane(2)) {
                lens.warp = Some(EmbeddedWarp { planes: [r, g, b], center, radius });
            }
        }
        if !self.vignetting.is_empty() {
            let att = |r: f64| self.vignetting_attenuation(f, shot.aperture, shot.distance, r * to_vig).unwrap_or(1.0);
            if let Some((k, err)) = model::fit_gain5(|r| 1.0 / att(r).max(0.05))
                && err <= MAX_VIGNETTE_ERROR
            {
                lens.vignette = Some(EmbeddedVignette { k, center, radius });
            }
        }
        (lens.warp.is_some() || lens.vignette.is_some()).then_some(lens)
    }
}

#[cfg(test)]
mod tests;
