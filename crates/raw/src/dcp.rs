#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
//! External camera profiles in the DNG Camera Profile format (`.dcp`): the colour tags of DNG
//! chapter 6 (`ColorMatrix1/2`, `ForwardMatrix1/2`, `CalibrationIlluminant1/2`,
//! `ProfileHueSatMap*`, `ProfileLookTable*`, `ProfileToneCurve`) in a TIFF-style file whose
//! header magic is `CR` (0x4352) instead of 42, with `UniqueCameraModel` naming the camera.
//!
//! Personal-fork feature: a profile the user owns is installed for its camera model
//! ([`install`]) and then replaces the colour model of that model's raws ([`for_camera`],
//! [`Dcp::apply`]). Nothing here ships a profile.

use crate::ColorData;
use crate::profile::ProfileLook;
use lcc_tiff::tags as t;
use lcc_tiff::{ByteOrder, Tiff};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

/// Largest `.dcp` read (Adobe-style profiles with 90×30×30 look tables are a few MB at most).
pub const MAX_FILE: usize = 32 << 20;

/// A parsed camera profile.
#[derive(Clone, Debug, PartialEq)]
pub struct Dcp {
    /// `UniqueCameraModel`, e.g. `NIKON Z 50`.
    pub model: String,
    /// `ProfileName`, else empty.
    pub name: String,
    /// `ProfileCopyright`, else empty.
    pub copyright: String,
    /// The profile's colour tags (white-balance fields left empty).
    pub color: ColorData,
}

/// Parse a `.dcp` file.
pub fn parse(bytes: &[u8]) -> Result<Dcp, String> {
    if bytes.len() > MAX_FILE {
        return Err(format!("{} bytes is too large for a camera profile", bytes.len()));
    }
    let order = match bytes.get(0..2) {
        Some(b"II") => ByteOrder::Little,
        Some(b"MM") => ByteOrder::Big,
        _ => return Err("not a DNG camera profile (no byte-order mark)".into()),
    };
    let magic = match bytes.get(2..4) {
        Some(&[a, b]) => order.u16([a, b]),
        _ => return Err("not a DNG camera profile (file too short)".into()),
    };
    // a DCP says `CR`; a TIFF/DNG holding only profile tags (42) is accepted as well
    if magic != 0x4352 && magic != 42 {
        return Err(format!("not a DNG camera profile (magic {magic:#06x})"));
    }
    let mut tiff_bytes = bytes.to_vec();
    let fortytwo = match order {
        ByteOrder::Little => [42, 0],
        ByteOrder::Big => [0, 42],
    };
    if let Some(m) = tiff_bytes.get_mut(2..4) {
        m.copy_from_slice(&fortytwo);
    }
    let tiff = Tiff::parse(&tiff_bytes).map_err(|e| format!("damaged camera profile: {e}"))?;
    let ifd = tiff.ifds.first().ok_or("damaged camera profile: no IFD")?;
    let model = ifd.string(t::UNIQUE_CAMERA_MODEL).map(|s| s.trim().to_string()).unwrap_or_default();
    if model.is_empty() {
        return Err("the profile names no camera (UniqueCameraModel)".into());
    }
    let mut color = crate::dng::color_data(ifd, ifd, order);
    // a profile has no white balance of its own
    color.as_shot_neutral = None;
    color.as_shot_white_xy = None;
    color.analog_balance = None;
    color.camera_calibration = [None, None];
    color.profile.gain_table_map = None;
    if !crate::color::has_matrix(&color) {
        return Err("the profile has no colour matrix".into());
    }
    let finite = |m: &Option<crate::Mat3>| m.as_ref().is_none_or(|m| m.0.iter().flatten().all(|v| v.is_finite() && v.abs() < 64.0));
    if !color.color_matrix.iter().all(finite) || !color.forward_matrix.iter().all(finite) {
        return Err("the profile's colour matrices are out of range".into());
    }
    Ok(Dcp {
        model,
        name: ifd.string(t::PROFILE_NAME).map(|s| s.trim().to_string()).unwrap_or_default(),
        copyright: ifd.string(PROFILE_COPYRIGHT).map(|s| s.trim().to_string()).unwrap_or_default(),
        color,
    })
}

/// `ProfileCopyright` (DNG 1.2).
const PROFILE_COPYRIGHT: u16 = 50942;

impl Dcp {
    /// Replace a raw's colour model with this profile's: matrices, illuminants, baseline exposure
    /// offset, hue/saturation map, look table and tone curve. The file's own white balance
    /// (`AsShotNeutral`, vendor multipliers, analog balance) is kept; its per-unit
    /// `CameraCalibration` is dropped, as it belongs to the file's own profile.
    pub fn apply(&self, color: &mut ColorData) {
        let p = &self.color;
        color.illuminant = p.illuminant;
        color.color_matrix = p.color_matrix;
        color.forward_matrix = p.forward_matrix;
        color.camera_calibration = [None, None];
        // a profile's BaselineExposureOffset adds to the file's BaselineExposure
        color.baseline_exposure += p.baseline_exposure;
        color.profile = ProfileLook {
            hue_sat_map: p.profile.hue_sat_map.clone(),
            look_table: p.profile.look_table.clone(),
            tone_curve: p.profile.tone_curve.clone(),
            gain_table_map: color.profile.gain_table_map.take(),
        };
    }

    /// Whether the profile carries a look of its own beside its calibration: a `ProfileLookTable`, or a
    /// `ProfileToneCurve` that isn't the identity (some profiles carry an identity curve only to stop
    /// converters adding theirs).
    pub fn has_look(&self) -> bool {
        let p = &self.color.profile;
        p.look_table.is_some()
            || p.tone_curve.as_ref().is_some_and(|c| (0..=32u8).any(|i| (c.eval(f32::from(i) / 32.0) - f32::from(i) / 32.0).abs() >= 0.005))
    }

    /// [`Self::apply`] with `look` false: only the profile's calibration (matrices, illuminants, baseline exposure,
    /// hue/saturation map), without its look table and tone curve. One of the app's looks sits on the calibration
    /// alone, so the profile's look isn't stacked under it (OneSlips, LR-PROF-LAYERS: "As profile" is the profile
    /// with its own look; any other look gets its calibration only).
    pub fn apply_with(&self, color: &mut ColorData, look: bool) {
        self.apply(color);
        if !look {
            color.profile.look_table = None;
            color.profile.tone_curve = None;
        }
    }
}

/// Upper-cased words, so `NIKON Z 50`, `Nikon  Z 50` and `NIKON_Z_50` compare equal.
fn key(s: &str) -> String {
    s.split(|c: char| c.is_whitespace() || c == '_').filter(|w| !w.is_empty()).map(str::to_uppercase).collect::<Vec<_>>().join(" ")
}

/// Whether a profile for `profile_model` (a `UniqueCameraModel`) is for the camera an image
/// names: its Exif model, else make and model joined (Adobe's unique names usually are
/// "make model", e.g. `RICOH GR III` for make `RICOH IMAGING COMPANY, LTD.`, model `GR III`).
fn matches(profile_model: &str, make: Option<&str>, model: &str) -> bool {
    let (p, m) = (key(profile_model), key(model));
    if p.is_empty() || m.is_empty() {
        return false;
    }
    if p == m {
        return true;
    }
    let first = make.and_then(|mk| mk.split_whitespace().next()).map(key).unwrap_or_default();
    !first.is_empty() && p == key(&format!("{first} {m}"))
}

/// A camera calibrated against Lightroom (`lcc_camdb::calibration`): bundled with the app, below a person's own
/// `.dcp` default. Each look is a profile chosen per photo as `cal:<id>`; the first is the camera's default.
#[derive(Clone, Debug)]
pub struct Calibrated {
    /// First word of the Exif make, upper case.
    pub make: String,
    /// Every name the camera goes by, as "make model" (profile-model form, see [`matches`]).
    pub names: Vec<String>,
    /// The lens model (Exif `LensModel`) the calibration is for; `None`: every lens (issue #1015). A photo takes its
    /// own lens's calibration, else the lens-less one, never another lens's ([`calibrated_for_lens`]).
    pub lens: Option<String>,
    /// `(cal:<id>, profile)`.
    pub looks: Vec<(String, Arc<Dcp>)>,
    /// The look photos of the camera start from when they choose none (a Lightroom calibration's first look);
    /// `None`: the looks are only there to be chosen (calibrations matched to other editors).
    pub default: Option<String>,
}

fn registry() -> &'static RwLock<Vec<Arc<Dcp>>> {
    static R: OnceLock<RwLock<Vec<Arc<Dcp>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// Install the active profiles (one per camera model; a later one for the same model wins).
pub fn install(list: Vec<Dcp>) {
    let mut out: Vec<Arc<Dcp>> = Vec::new();
    for d in list {
        out.retain(|x| key(&x.model) != key(&d.model));
        out.push(Arc::new(d));
    }
    *registry().write().unwrap_or_else(PoisonError::into_inner) = out;
}

/// The installed profiles.
pub fn installed() -> Vec<Arc<Dcp>> {
    registry().read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// The installed profile for a camera (Exif make and model).
pub fn for_camera(make: Option<&str>, model: &str) -> Option<Arc<Dcp>> {
    registry().read().unwrap_or_else(PoisonError::into_inner).iter().find(|d| matches(&d.model, make, model)).cloned()
}

/// [`for_camera`] for a camera written as one string, Exif make and model joined by a space (as
/// the catalog keeps it): every split into make and model is tried, then the whole string.
pub fn covers_camera(camera: &str) -> bool {
    resolve_camera("", camera).is_some()
}

/// Whether a profile for `profile_model` fits a camera written as one string (see [`covers_camera`]).
pub fn covers_model(profile_model: &str, camera: &str) -> bool {
    let words: Vec<&str> = camera.split_whitespace().collect();
    if words.is_empty() {
        return false;
    }
    (1..words.len()).filter_map(|i| words.split_at_checked(i)).any(|(make, model)| matches(profile_model, Some(&make.join(" ")), &model.join(" ")))
        || matches(profile_model, None, camera)
}

fn library_registry() -> &'static RwLock<Vec<(String, Arc<Dcp>)>> {
    static R: OnceLock<RwLock<Vec<(String, Arc<Dcp>)>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// Install the imported profiles that can be chosen per photo: `(id, profile)`, ids `dcp:…`.
pub fn install_library(list: Vec<(String, Dcp)>) {
    *library_registry().write().unwrap_or_else(PoisonError::into_inner) = list.into_iter().map(|(id, d)| (id, Arc::new(d))).collect();
}

/// The imported profiles, `(id, profile)`.
pub fn library() -> Vec<(String, Arc<Dcp>)> {
    library_registry().read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// The imported profile with id `id`.
pub fn library_get(id: &str) -> Option<Arc<Dcp>> {
    library_registry().read().unwrap_or_else(PoisonError::into_inner).iter().find(|(i, _)| i == id).map(|(_, d)| d.clone())
}

fn calibrated_registry() -> &'static RwLock<Vec<Arc<Calibrated>>> {
    static R: OnceLock<RwLock<Vec<Arc<Calibrated>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// Install the calibrated cameras. Calibrations for the same camera (fitted to different editors) pool their looks;
/// a look id seen again is replaced by the later one, and the first default given stays the default.
/// Calibrations for different lenses of one camera stay apart.
pub fn install_calibrated(list: Vec<Calibrated>) {
    let mut out: Vec<Calibrated> = Vec::new();
    for c in list {
        let first = c.names.first().map(|n| key(n));
        let lens = c.lens.as_deref().map(lens_key);
        match out.iter_mut().find(|x| x.names.first().map(|n| key(n)) == first && x.lens.as_deref().map(lens_key) == lens) {
            Some(x) => {
                for name in c.names {
                    if !x.names.iter().any(|n| key(n) == key(&name)) {
                        x.names.push(name);
                    }
                }
                if x.default.is_none() {
                    x.default = c.default;
                }
                for (id, look) in c.looks {
                    match x.looks.iter_mut().find(|(i, _)| *i == id) {
                        Some(slot) => slot.1 = look,
                        None => x.looks.push((id, look)),
                    }
                }
            }
            None => out.push(c),
        }
    }
    *calibrated_registry().write().unwrap_or_else(PoisonError::into_inner) = out.into_iter().map(Arc::new).collect();
}

/// The installed calibrations.
pub fn calibrated() -> Vec<Arc<Calibrated>> {
    calibrated_registry().read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// A lens model compared without case and runs of white space.
fn lens_key(lens: &str) -> String {
    lens.split_whitespace().map(str::to_lowercase).collect::<Vec<_>>().join(" ")
}

/// Whether two lens models (Exif `LensModel`) name the same lens: equal without case and runs of white space, and
/// not empty.
pub fn same_lens(a: &str, b: &str) -> bool {
    let a = lens_key(a);
    !a.is_empty() && a == lens_key(b)
}

/// Among the calibrations `fits` accepts, the one for `lens` (a photo's Exif lens model), else the lens-less one;
/// never one for another lens.
fn pick_calibrated(fits: impl Fn(&Calibrated) -> bool, lens: Option<&str>) -> Option<Arc<Calibrated>> {
    let reg = calibrated_registry().read().unwrap_or_else(PoisonError::into_inner);
    let own = lens.and_then(|l| reg.iter().find(|c| c.lens.as_deref().is_some_and(|x| same_lens(x, l)) && fits(c)));
    own.or_else(|| reg.iter().find(|c| c.lens.is_none() && fits(c))).cloned()
}

/// The lens-less calibration for a camera (Exif make and model): one of its names [`matches`] the camera. Photos
/// whose lens is known use [`calibrated_for_lens`].
pub fn calibrated_for(make: Option<&str>, model: &str) -> Option<Arc<Calibrated>> {
    calibrated_for_lens(make, model, None)
}

/// The calibration for a camera (Exif make and model) and a photo's lens (Exif lens model): that lens's own, else
/// the camera's lens-less one, never another lens's (issue #1015).
pub fn calibrated_for_lens(make: Option<&str>, model: &str, lens: Option<&str>) -> Option<Arc<Calibrated>> {
    pick_calibrated(|c| c.names.iter().any(|n| matches(n, make, model)), lens)
}

/// [`calibrated_for`] for a camera written as one string (see [`covers_camera`]).
pub fn calibrated_for_camera(camera: &str) -> Option<Arc<Calibrated>> {
    pick_calibrated(|c| c.names.iter().any(|n| covers_model(n, camera)), None)
}

/// Whether some installed calibration has a look with id `id` (`cal:…`).
pub fn calibrated_look_exists(id: &str) -> bool {
    calibrated_registry().read().unwrap_or_else(PoisonError::into_inner).iter().any(|c| c.looks.iter().any(|(i, _)| i == id))
}

/// The look id (`cal:…`) of a profile that came from a calibration, if it did.
pub fn calibrated_look_id(dcp: &Arc<Dcp>) -> Option<String> {
    let reg = calibrated_registry().read().unwrap_or_else(PoisonError::into_inner);
    reg.iter().flat_map(|c| c.looks.iter()).find(|(_, d)| Arc::ptr_eq(d, dcp)).map(|(id, _)| id.clone())
}

/// The person's default camera-profile choices (a library preference, issue #723): `(camera,
/// choice)`, `camera` empty for every camera, else the camera as "make model" (matched like a
/// profile's `UniqueCameraModel`). A choice is a `profile.camera` value: `lc` (the app's own
/// colour), `dcp:<id>`, `cal:<id>`; photos of that camera start from it when they choose none,
/// ahead of the camera's active `.dcp` and its calibration's default look. Installed by the
/// engine when a library opens and whenever the preference changes (the registry is per process).
fn user_default_registry() -> &'static RwLock<Vec<(String, String)>> {
    static R: OnceLock<RwLock<Vec<(String, String)>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// Install the person's default choices (replaces what a previous install put there).
pub fn install_user_defaults(list: Vec<(String, String)>) {
    *user_default_registry().write().unwrap_or_else(PoisonError::into_inner) =
        list.into_iter().map(|(camera, choice)| (camera.trim().to_string(), choice.trim().to_string())).collect();
}

/// The installed default choices, `(camera, choice)`.
pub fn user_defaults() -> Vec<(String, String)> {
    user_default_registry().read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// The person's default choice for a camera: its own entry, else the global one (`camera` empty).
fn user_default_for(make: Option<&str>, model: &str) -> Option<String> {
    let reg = user_default_registry().read().unwrap_or_else(PoisonError::into_inner);
    let own = reg.iter().rev().find(|(camera, _)| !camera.is_empty() && matches(camera, make, model));
    let global = reg.iter().find(|(camera, _)| camera.is_empty());
    own.or(global).map(|(_, choice)| choice.clone())
}

/// [`resolve`] for a choice without falling back to the camera's default: a gone or foreign
/// profile id simply leaves the app's own colour (this is the person's own choice).
fn choice_direct(choice: &str, make: Option<&str>, model: &str, lens: Option<&str>) -> Option<Arc<Dcp>> {
    match choice {
        "lc" | "" => None,
        c if c.starts_with("dcp:") => library_get(c).filter(|d| matches(&d.model, make, model)),
        c if c.starts_with("cal:") => look_of(calibrated_for_lens(make, model, lens), c),
        _ => None,
    }
}

/// A camera's default profile: the person's choice ([`user_default_for`]) when one is set, else
/// the person's own `.dcp` ([`for_camera`]), else its calibration's default look.
fn default_for(make: Option<&str>, model: &str, lens: Option<&str>) -> Option<Arc<Dcp>> {
    if let Some(choice) = user_default_for(make, model) {
        return choice_direct(&choice, make, model, lens);
    }
    for_camera(make, model).or_else(|| {
        let c = calibrated_for_lens(make, model, lens)?;
        let id = c.default.as_ref()?;
        c.looks.iter().find(|(i, _)| i == id).map(|(_, d)| d.clone())
    })
}

/// The profile a photo's camera-profile choice (`profile.camera`) gives a camera (Exif make and
/// model): `lc` none (the app's own colour); `dcp:<id>` that imported profile when it exists and
/// is for this camera; `cal:<id>` that calibrated look of this camera's calibration; anything else
/// (empty, or a profile that is gone or for another camera) the camera's default: the person's own
/// `.dcp` ([`for_camera`]), else its calibration's first look. The decoder and the catalog both decide through this.
/// The lens is unknown here: only lens-less calibrations serve ([`resolve_lens`] for a photo whose lens is known).
pub fn resolve(choice: &str, make: Option<&str>, model: &str) -> Option<Arc<Dcp>> {
    resolve_lens(choice, make, model, None)
}

/// [`resolve`] for a photo taken with `lens` (its Exif lens model): calibrated looks come from that lens's
/// calibration when there is one, else the camera's lens-less one (issue #1015).
pub fn resolve_lens(choice: &str, make: Option<&str>, model: &str, lens: Option<&str>) -> Option<Arc<Dcp>> {
    match choice {
        "lc" => None,
        c if c.starts_with("dcp:") => library_get(c).filter(|d| matches(&d.model, make, model)).or_else(|| default_for(make, model, lens)),
        c if c.starts_with("cal:") => look_of(calibrated_for_lens(make, model, lens), c).or_else(|| default_for(make, model, lens)),
        _ => default_for(make, model, lens),
    }
}

/// The look with id `id` of a calibration.
fn look_of(cal: Option<Arc<Calibrated>>, id: &str) -> Option<Arc<Dcp>> {
    cal?.looks.iter().find(|(i, _)| i == id).map(|(_, d)| d.clone())
}

/// [`resolve`] for a camera written as one string (see [`covers_camera`]).
pub fn resolve_camera(choice: &str, camera: &str) -> Option<Arc<Dcp>> {
    let words: Vec<&str> = camera.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    (1..words.len())
        .filter_map(|i| words.split_at_checked(i))
        .find_map(|(make, model)| resolve(choice, Some(&make.join(" ")), &model.join(" ")))
        .or_else(|| resolve(choice, None, camera))
}

/// Write `dcp` as a `.dcp` file (little-endian, `IIRC`): `UniqueCameraModel`, the colour and forward matrices with
/// their illuminants, `ProfileName`, the hue/saturation maps, look table and tone curve, `ProfileEmbedPolicy` (no
/// restrictions), `ProfileCopyright` and `BaselineExposureOffset` (the colour's `baseline_exposure`). [`parse`] reads
/// it back. Sources: DNG specification 1.7 chapter 6 (camera profile tags).
pub fn write(dcp: &Dcp) -> Result<Vec<u8>, String> {
    let (model, name, copyright, color) = (dcp.model.as_str(), dcp.name.as_str(), dcp.copyright.as_str(), &dcp.color);
    enum V {
        Ascii(String),
        Short(u16),
        Long(Vec<u32>),
        SRational(Vec<f64>),
        Float(Vec<f32>),
    }
    let mut tags: Vec<(u16, V)> = vec![(50708, V::Ascii(model.into()))];
    let mat = |m: &crate::Mat3| V::SRational(m.0.iter().flatten().copied().collect());
    for (k, tag) in [(0usize, 50721u16), (1, 50722)] {
        if let Some(Some(m)) = color.color_matrix.get(k) {
            tags.push((tag, mat(m)));
        }
    }
    for (k, tag) in [(0usize, 50778u16), (1, 50779)] {
        if let Some(&code) = color.illuminant.get(k)
            && code != 0
        {
            tags.push((tag, V::Short(code)));
        }
    }
    if !name.is_empty() {
        tags.push((50936, V::Ascii(name.into())));
    }
    let table_data = |t: &crate::profile::HsvTable| V::Float(t.data.iter().flatten().copied().collect());
    let dims = |t: &crate::profile::HsvTable| -> Result<V, String> {
        let d = [t.hue_divisions, t.sat_divisions, t.val_divisions].map(u32::try_from);
        match d {
            [Ok(h), Ok(s), Ok(v)] => Ok(V::Long(vec![h, s, v])),
            _ => Err("table too large for a camera profile".into()),
        }
    };
    let p = &color.profile;
    if let [Some(a), Some(b)] = &p.hue_sat_map {
        if (a.hue_divisions, a.sat_divisions, a.val_divisions) == (b.hue_divisions, b.sat_divisions, b.val_divisions) {
            tags.push((50937, dims(a)?));
            tags.push((50938, table_data(a)));
            tags.push((50939, table_data(b)));
            tags.push((51107, V::Long(vec![u32::from(a.srgb_value)])));
        }
    } else if let Some(a) = p.hue_sat_map.iter().flatten().next() {
        tags.push((50937, dims(a)?));
        tags.push((50938, table_data(a)));
        tags.push((51107, V::Long(vec![u32::from(a.srgb_value)])));
    }
    if let Some(curve) = &p.tone_curve {
        tags.push((50940, V::Float(curve.points.iter().flatten().copied().collect())));
    }
    tags.push((50941, V::Long(vec![3]))); // ProfileEmbedPolicy: no restrictions
    if !copyright.is_empty() {
        tags.push((50942, V::Ascii(copyright.into())));
    }
    for (k, tag) in [(0usize, 50964u16), (1, 50965)] {
        if let Some(Some(m)) = color.forward_matrix.get(k) {
            tags.push((tag, mat(m)));
        }
    }
    if let Some(t) = &p.look_table {
        tags.push((50981, dims(t)?));
        tags.push((50982, table_data(t)));
        tags.push((51108, V::Long(vec![u32::from(t.srgb_value)])));
    }
    if color.baseline_exposure != 0.0 && color.baseline_exposure.is_finite() {
        tags.push((51109, V::SRational(vec![color.baseline_exposure])));
    }
    tags.sort_by_key(|(t, _)| *t);

    let to_u32 = |n: usize| u32::try_from(n).map_err(|_| "camera profile too large".to_string());
    let encode = |v: &V| -> (u16, usize, Vec<u8>) {
        match v {
            V::Ascii(s) => {
                let mut b: Vec<u8> = s.bytes().filter(|&c| c != 0).collect();
                b.push(0);
                (2, b.len(), b)
            }
            V::Short(x) => (3, 1, x.to_le_bytes().to_vec()),
            V::Long(xs) => (4, xs.len(), xs.iter().flat_map(|x| x.to_le_bytes()).collect()),
            V::SRational(xs) => (
                10,
                xs.len(),
                xs.iter()
                    .flat_map(|x| {
                        let n = (x * 10000.0).round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
                        n.to_le_bytes().into_iter().chain(10000i32.to_le_bytes())
                    })
                    .collect(),
            ),
            V::Float(xs) => (11, xs.len(), xs.iter().flat_map(|x| x.to_le_bytes()).collect()),
        }
    };
    let n = tags.len();
    let too_large = || "camera profile too large".to_string();
    let ifd_len = n.checked_mul(12).and_then(|b| b.checked_add(6)).ok_or_else(too_large)?;
    let mut head = b"IIRC".to_vec();
    head.extend_from_slice(&8u32.to_le_bytes());
    head.extend_from_slice(&u16::try_from(n).map_err(|_| "too many tags".to_string())?.to_le_bytes());
    let mut data = Vec::new();
    let data_at = ifd_len.checked_add(8).ok_or_else(too_large)?;
    for (tag, v) in &tags {
        let (ty, count, bytes) = encode(v);
        head.extend_from_slice(&tag.to_le_bytes());
        head.extend_from_slice(&ty.to_le_bytes());
        head.extend_from_slice(&to_u32(count)?.to_le_bytes());
        if bytes.len() <= 4 {
            let mut inline = [0u8; 4];
            for (d, s) in inline.iter_mut().zip(&bytes) {
                *d = *s;
            }
            head.extend_from_slice(&inline);
        } else {
            if data.len() & 1 == 1 {
                data.push(0); // TIFF values start on a word boundary
            }
            head.extend_from_slice(&to_u32(data_at.checked_add(data.len()).ok_or_else(too_large)?)?.to_le_bytes());
            data.extend_from_slice(&bytes);
        }
    }
    head.extend_from_slice(&0u32.to_le_bytes());
    head.extend_from_slice(&data);
    Ok(head)
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    /// A minimal little-endian DCP: UniqueCameraModel, ProfileName, ColorMatrix1, illuminant 1.
    pub(crate) fn tiny_dcp(model: &str) -> Vec<u8> {
        let mut b = b"IIRC".to_vec();
        b.extend_from_slice(&8u32.to_le_bytes());
        let model_z = format!("{model}\0");
        let name_z = "Test Look\0".to_string();
        // entries: 50708 ASCII, 50721 SRATIONAL×9, 50778 SHORT, 50936 ASCII
        let n = 4u16;
        let ifd_len = 2 + 12 * n as usize + 4;
        let data_at = 8 + ifd_len;
        let mut data = Vec::new();
        let model_off = data_at + data.len();
        data.extend_from_slice(model_z.as_bytes());
        let matrix_off = data_at + data.len();
        for v in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]].iter().flatten() {
            data.extend_from_slice(&((*v * 10000.0) as i32).to_le_bytes());
            data.extend_from_slice(&10000i32.to_le_bytes());
        }
        let name_off = data_at + data.len();
        data.extend_from_slice(name_z.as_bytes());
        b.extend_from_slice(&n.to_le_bytes());
        let mut entry = |tag: u16, ty: u16, count: u32, value: u32| {
            b.extend_from_slice(&tag.to_le_bytes());
            b.extend_from_slice(&ty.to_le_bytes());
            b.extend_from_slice(&count.to_le_bytes());
            b.extend_from_slice(&value.to_le_bytes());
        };
        entry(50708, 2, model_z.len() as u32, model_off as u32);
        entry(50721, 10, 9, matrix_off as u32);
        entry(50778, 3, 1, 21);
        entry(50936, 2, name_z.len() as u32, name_off as u32);
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&data);
        b
    }

    #[test]
    fn parses_a_minimal_profile() {
        let d = parse(&tiny_dcp("NIKON Z 50")).unwrap();
        assert_eq!(d.model, "NIKON Z 50");
        assert_eq!(d.name, "Test Look");
        assert_eq!(d.color.illuminant[0], 21);
        assert!(d.color.color_matrix[0].is_some());
    }

    #[test]
    fn rejects_damaged_files_without_panicking() {
        let good = tiny_dcp("NIKON Z 50");
        for len in 0..good.len() {
            let _ = parse(&good[..len]);
        }
        assert!(parse(b"MMRC").is_err());
        assert!(parse(b"hello world").is_err());
        let mut no_model = good.clone();
        no_model[10..12].copy_from_slice(&1u16.to_le_bytes()); // tag 50708 → 1: ignored entry
        assert!(parse(&no_model).is_err());
    }

    #[test]
    fn registry_finds_profiles_by_camera() {
        install(vec![parse(&tiny_dcp("TESTCAM ONE")).unwrap()]);
        assert!(for_camera(Some("TESTCAM CO"), "ONE").is_some());
        assert!(covers_camera("TESTCAM CO TESTCAM ONE"));
        assert!(!covers_camera("OTHER CAM"));
        // a photo's choice: the app's own, an imported profile for the camera, or the default
        let mut other = parse(&tiny_dcp("TESTCAM ONE")).unwrap();
        other.name = "Other".into();
        install_library(vec![("dcp:other".into(), other), ("dcp:two".into(), parse(&tiny_dcp("TESTCAM TWO")).unwrap())]);
        assert!(resolve_camera("lc", "TESTCAM CO TESTCAM ONE").is_none());
        assert_eq!(resolve_camera("dcp:other", "TESTCAM CO TESTCAM ONE").unwrap().name, "Other");
        assert_eq!(resolve_camera("", "TESTCAM CO TESTCAM ONE").unwrap().name, "Test Look");
        // a profile for another camera, or one that is gone: the default
        assert_eq!(resolve_camera("dcp:two", "TESTCAM CO TESTCAM ONE").unwrap().name, "Test Look");
        assert_eq!(resolve_camera("dcp:gone", "TESTCAM CO TESTCAM ONE").unwrap().name, "Test Look");
        assert!(covers_model("TESTCAM TWO", "TESTCAM CO TESTCAM TWO"));
        install(Vec::new());
        assert!(!covers_camera("TESTCAM CO TESTCAM ONE"));
        assert!(resolve_camera("dcp:two", "TESTCAM CO TESTCAM TWO").is_some(), "a chosen profile needs no default");
        install_library(Vec::new());

        // calibrated cameras (one test: the registries are process-wide)
        let look = |name: &str| {
            let mut d = parse(&tiny_dcp("CALCAM ONE")).unwrap();
            d.name = name.into();
            Arc::new(d)
        };
        let names = vec!["CALCAM ONE".into(), "CALCAM ONE II".into()];
        install_calibrated(vec![Calibrated {
            make: "CALCAM".into(),
            names,
            lens: None,
            looks: vec![("cal:a".into(), look("A")), ("cal:b".into(), look("B"))],
            default: Some("cal:a".into()),
        }]);
        let cam = "CALCAM CORP CALCAM ONE";
        assert!(covers_camera(cam) && covers_camera("CALCAM CORP ONE II"));
        assert_eq!(resolve_camera("", cam).unwrap().name, "A", "the first look is the default");
        assert_eq!(resolve_camera("cal:b", cam).unwrap().name, "B");
        assert_eq!(resolve_camera("cal:gone", cam).unwrap().name, "A");
        assert!(resolve_camera("lc", cam).is_none());
        assert!(resolve_camera("cal:b", "TESTCAM CO TESTCAM ONE").is_none(), "another camera's look is not used");
        assert_eq!(calibrated_for_camera(cam).unwrap().looks.len(), 2);
        assert!(calibrated_look_exists("cal:b") && !calibrated_look_exists("cal:gone"));
        let b = resolve_camera("cal:b", cam).unwrap();
        assert_eq!(calibrated_look_id(&b).as_deref(), Some("cal:b"));
        // a person's own .dcp outranks the calibration as default, not an explicit look
        install(vec![parse(&tiny_dcp("CALCAM ONE")).unwrap()]);
        assert_eq!(resolve_camera("", cam).unwrap().name, "Test Look");
        assert_eq!(resolve_camera("cal:b", cam).unwrap().name, "B");
        assert!(calibrated_look_id(&resolve_camera("", cam).unwrap()).is_none());
        install(Vec::new());
        // a second calibration for the camera (another editor's: no default) pools its looks, the default stays;
        // a camera with only such a calibration has no default look, its looks are there to be chosen
        let other = |names: Vec<String>, id: &str, name: &str| Calibrated {
            make: "CALCAM".into(),
            names,
            lens: None,
            looks: vec![(id.into(), look(name))],
            default: None,
        };
        install_calibrated(vec![
            Calibrated {
                make: "CALCAM".into(),
                names: vec!["CALCAM ONE".into()],
                lens: None,
                looks: vec![("cal:a".into(), look("A"))],
                default: Some("cal:a".into()),
            },
            other(vec!["CALCAM ONE".into(), "CALCAM ONE X".into()], "cal:capture-one-standard", "C1"),
            other(vec!["CALCAM TWO".into()], "cal:darktable-default", "DT"),
        ]);
        assert_eq!(resolve_camera("", cam).unwrap().name, "A");
        assert_eq!(resolve_camera("cal:capture-one-standard", cam).unwrap().name, "C1");
        assert!(covers_camera("CALCAM CORP ONE X"), "names pool too");
        let two = "CALCAM CORP CALCAM TWO";
        assert!(resolve_camera("", two).is_none(), "another editor's look is never the default");
        assert_eq!(resolve_camera("cal:darktable-default", two).unwrap().name, "DT");
        install_calibrated(Vec::new());
        assert!(!covers_camera(cam));

        // calibrations per lens (issue #1015): a photo takes its lens's own, else the lens-less one, never another
        // lens's; lens-specific ones stay apart from the lens-less one when installed
        let lensed = |lens: Option<&str>, name: &str| Calibrated {
            make: "LENSCAM".into(),
            names: vec!["LENSCAM ONE".into()],
            lens: lens.map(str::to_string),
            looks: vec![("cal:adobe-color".into(), look(name))],
            default: Some("cal:adobe-color".into()),
        };
        install_calibrated(vec![lensed(Some("Wide 24mm"), "Wide"), lensed(None, "Any"), lensed(Some("Ultra 14mm"), "Ultra")]);
        let (mk, md) = (Some("LENSCAM CORP"), "ONE");
        let name = |choice: &str, lens: Option<&str>| resolve_lens(choice, mk, md, lens).map(|d| d.name.clone());
        assert_eq!(name("", Some("Wide 24mm")).as_deref(), Some("Wide"));
        assert_eq!(name("cal:adobe-color", Some("ULTRA  14mm")).as_deref(), Some("Ultra"));
        assert_eq!(name("", Some("Tele 70mm")).as_deref(), Some("Any"));
        assert_eq!(name("", None).as_deref(), Some("Any"));
        assert_eq!(resolve("", mk, md).map(|d| d.name.clone()).as_deref(), Some("Any"));
        assert_eq!(calibrated().len(), 3, "different lenses are not pooled");
        install_calibrated(vec![lensed(Some("Wide 24mm"), "Wide")]);
        assert!(name("", Some("Ultra 14mm")).is_none(), "another lens's calibration never serves");
        assert!(name("", None).is_none());
        assert!(!same_lens("", "") && same_lens("A  b", "a B"));
        install_calibrated(Vec::new());

        // the person's default choice (issue #723) outranks the camera's active `.dcp` and its
        // calibration's default look; `lc` means the app's own colour even when a `.dcp` is
        // active; a global choice (empty camera) covers the cameras without an entry of their own
        // (one test with the rest: the registries are process-wide)
        install(vec![parse(&tiny_dcp("PDCAM ONE")).unwrap()]);
        install_library(vec![("dcp:mine".into(), {
            let mut d = parse(&tiny_dcp("PDCAM ONE")).unwrap();
            d.name = "Mine".into();
            d
        })]);
        let pdcam = "PDCAM CO PDCAM ONE";
        // without a choice: the active `.dcp`
        assert_eq!(resolve_camera("", pdcam).unwrap().name, "Test Look");
        install_user_defaults(vec![("PDCAM ONE".into(), "dcp:mine".into())]);
        assert_eq!(resolve_camera("", pdcam).unwrap().name, "Mine");
        // a gone profile id: the app's own colour, no fallback to the active `.dcp`
        install_user_defaults(vec![("PDCAM ONE".into(), "dcp:gone".into())]);
        assert!(resolve_camera("", pdcam).is_none());
        // `lc`: the app's own colour
        install_user_defaults(vec![("PDCAM ONE".into(), "lc".into())]);
        assert!(resolve_camera("", pdcam).is_none());
        // the global choice covers the cameras without their own entry
        install_user_defaults(vec![("".into(), "lc".into())]);
        assert!(resolve_camera("", pdcam).is_none());
        install_user_defaults(vec![("PDCAM ONE".into(), "dcp:mine".into()), ("".into(), "lc".into())]);
        assert_eq!(resolve_camera("", pdcam).unwrap().name, "Mine", "the camera's own entry wins over the global one");
        install_user_defaults(Vec::new());
        assert_eq!(resolve_camera("", pdcam).unwrap().name, "Test Look");
        install_library(Vec::new());
        install(Vec::new());
        // matches camera names
        {
            assert!(matches("NIKON Z 50", Some("NIKON CORPORATION"), "NIKON Z 50"));
            assert!(matches("RICOH GR III", Some("RICOH IMAGING COMPANY, LTD."), "GR III"));
            assert!(matches("Canon EOS R5", None, "CANON  EOS_R5"));
            assert!(!matches("NIKON Z 50", Some("NIKON CORPORATION"), "NIKON Z 5"));
            assert!(!matches("", None, "X"));
        }
    }
}
