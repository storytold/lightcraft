//! Camera calibrations matched to another editor: per-model colour and tone fitted black-box to Lightroom's (or, with
//! [`Editor`], Capture One's, darktable's, RawTherapee's or DxO PhotoLab's) exports of a contributor's own photos
//! (`<cli> calibrate-lightroom fit [--editor ID]`), so photos opened in the app render as they do there.
//!
//! One file per camera model, lens and editor (`data/calibrations/<Make>_<Model>.json` for Lightroom,
//! `<Make>_<Model>_<editor>.json` for another, `…_<Lens>…` before the editor when the fit is for one lens, embedded at
//! build time) holds:
//! - the camera's colour model in the DNG profile form (DNG 1.7 chapter 6): `ColorMatrix1/2` (they place the white,
//!   so Kelvin and tint read as Lightroom's), `ForwardMatrix1/2` and an optional dual `HueSatMap`, for two
//!   calibration illuminants;
//! - one or more named looks, each the `crs:CameraProfile` name Lightroom reports ("Adobe Standard",
//!   "Adobe Color", "Camera Standard", …) with its own look table, tone curve and exposure offset. The first look
//!   is the camera's default;
//! - the evidence: how many photos and samples, the white-balance range covered and the held-out colour error
//!   before and after.
//!
//! **Data rules** (maintainer decision 2026-10-25): the numbers are fitted to Lightroom's *output images* of the
//! contributor's own photos, as aggregate statistics. No Adobe profile (DCP/LCP), no matrix read from an
//! Adobe-converted DNG, no image content, file name, date, serial number or location enters a file. The one lens
//! datum allowed is the lens *model identifier* (Exif `LensModel`, issue #1015) of a fit made for one lens, e.g. one
//! of a phone's cameras that share an Exif model; never a lens serial number.
//!
//! **Lens matching** (issue #1015): a calibration without a lens is for every photo of the camera; one with a lens
//! only for photos whose lens model is that lens. A photo takes the calibration for its own lens when there is one,
//! else the lens-less one, and never a calibration for another lens.
//! [`Calibration::validate`] enforces the shape and bounds, and every field is on an allow-list: a file with an
//! unknown field is refused.
//!
//! A look becomes a [`Dcp`] ([`Calibration::look_dcp`]) and reaches photos through the same path as a person's own
//! `.dcp` (`lcc_raw::dcp::resolve`): per-photo choice `cal:<look id>`, a person's own `.dcp` default outranking it.
//! Calibrations of one camera for several editors pool their looks; only a Lightroom calibration gives the camera
//! its default look, the other editors' looks are chosen per photo (Profile ▸ Camera profile).

use lcc_color::Mat3;
use lcc_raw::ColorData;
use lcc_raw::dcp::Dcp;
use lcc_raw::profile::{HsvTable, ProfileLook, ToneCurve};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Format version of calibration files.
pub const VERSION: u32 = 1;
/// Largest calibration file read (two 90 × 30 × 1 maps and a few looks are well under 1 MB of JSON).
pub const MAX_FILE: usize = 4 << 20;
/// The licence every calibration is contributed under (as the camera-JPEG profiles).
pub const LICENCE: &str = "MIT OR Apache-2.0";
/// How the numbers were made (Lightroom's calibrations; [`Editor::method`] for every editor).
pub const METHOD: &str = "black-box fit to Lightroom exports of the contributor's own photos";

/// The editor whose exports a calibration was fitted to. Its looks are opt-in per photo (`cal:<id>`), so
/// calibrations for several editors can sit side by side for one camera.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Editor {
    #[default]
    Lightroom,
    CaptureOne,
    Darktable,
    Rawtherapee,
    DxoPhotolab,
}

impl Editor {
    pub const ALL: [Editor; 5] = [Editor::Lightroom, Editor::CaptureOne, Editor::Darktable, Editor::Rawtherapee, Editor::DxoPhotolab];

    /// The editor's name as people write it.
    pub fn name(self) -> &'static str {
        match self {
            Editor::Lightroom => "Lightroom",
            Editor::CaptureOne => "Capture One",
            Editor::Darktable => "darktable",
            Editor::Rawtherapee => "RawTherapee",
            Editor::DxoPhotolab => "DxO PhotoLab",
        }
    }

    /// The id used on the command line and in files (`capture-one`, …).
    pub fn id(self) -> &'static str {
        match self {
            Editor::Lightroom => "lightroom",
            Editor::CaptureOne => "capture-one",
            Editor::Darktable => "darktable",
            Editor::Rawtherapee => "rawtherapee",
            Editor::DxoPhotolab => "dxo-photolab",
        }
    }

    /// The editor for an id or name, in any case (`capture-one`, `Capture One`, `captureone`).
    pub fn parse(s: &str) -> Option<Editor> {
        let norm = |t: &str| t.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
        let want = norm(s);
        Editor::ALL.into_iter().find(|e| norm(e.id()) == want || norm(e.name()) == want)
    }

    /// The `method` a calibration fitted to this editor records.
    pub fn method(self) -> String {
        match self {
            Editor::Lightroom => METHOD.to_string(),
            e => format!("black-box fit to {} exports of the contributor's own photos", e.name()),
        }
    }
}
/// Prefix of a look's id in a photo's camera-profile choice (`profile.camera`).
pub const LOOK_PREFIX: &str = "cal:";

/// Largest table accepted (hue × saturation × value entries).
const MAX_TABLE: usize = 1 << 16;
/// Largest number of looks per camera.
const MAX_LOOKS: usize = 16;
/// Largest number of tone-curve points.
const MAX_TONE_POINTS: usize = 1024;

fn is_lightroom(e: &Editor) -> bool {
    *e == Editor::Lightroom
}

/// One camera model's calibration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Calibration {
    pub version: u32,
    /// Exif make and model, e.g. `SONY` / `ILCE-7M4`.
    pub make: String,
    pub model: String,
    /// Other Exif model names of the same camera.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// The lens model (Exif `LensModel`) the fit is for; `None`: every lens of the camera (see the module doc).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lens: Option<String>,
    /// The editor the looks match (Lightroom when the file doesn't say).
    #[serde(default, skip_serializing_if = "is_lightroom")]
    pub editor: Editor,
    /// [`Editor::method`] of [`Calibration::editor`].
    pub method: String,
    /// The tool that made the file (the CLI's version).
    pub tool_version: String,
    /// Who contributed it (a name or handle, for the attribution).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contributor: Option<String>,
    /// [`LICENCE`].
    pub licence: String,
    pub camera: CameraModel,
    /// The looks, the default first.
    pub looks: Vec<Look>,
    pub evidence: Evidence,
}

/// The camera's colour model: DNG `CalibrationIlluminant1/2`, `ColorMatrix1/2`, `ForwardMatrix1/2` and an optional
/// `ProfileHueSatMapData1/2`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraModel {
    pub illuminants: [u16; 2],
    /// XYZ → camera under each illuminant.
    pub color_matrix: [Mat3; 2],
    /// White-balanced camera → XYZ D50 under each illuminant.
    pub forward_matrix: [Mat3; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hue_sat_map: Option<[HsvTable; 2]>,
}

/// A named look on top of the camera model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Look {
    /// The `crs:CameraProfile` name Lightroom gives the profile this look matches, e.g. `Adobe Color`.
    pub name: String,
    /// EV added to the file's baseline exposure (`BaselineExposureOffset`).
    #[serde(default)]
    pub baseline_exposure_offset: f64,
    /// `ProfileLookTableData`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look_table: Option<HsvTable>,
    /// `ProfileToneCurve`: linear (input, output) pairs from (0, 0) to (1, 1); empty: none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tone_curve: Vec<[f32; 2]>,
}

/// What the calibration was fitted on and how well it does.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    /// Photos the camera model was fitted on.
    pub photos: u32,
    /// Colour samples used.
    pub samples: u64,
    /// Photos per white-balance range, from `from_k` kelvin up to the next bin's.
    #[serde(default)]
    pub cct: Vec<CctBin>,
    /// Share of the 36 hue sectors (10° each) the samples cover with saturated colour, 0..1.
    pub hue_coverage: f32,
    /// Photos whose Lightroom white balance (temperature and tint) placed the white points.
    #[serde(default)]
    pub white_points: u32,
    /// Held-out colour error per look.
    #[serde(default)]
    pub looks: Vec<LookEvidence>,
}

/// Photos in one white-balance range.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CctBin {
    pub from_k: u32,
    pub photos: u32,
}

/// Colour error (CIEDE2000) against Lightroom on photos the fit did not see.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookEvidence {
    pub name: String,
    /// Photos the look was fitted on, and held out.
    pub photos: u32,
    pub held_out: u32,
    /// The app's colour before (its estimated colour) and with this calibration.
    pub before: DeltaE,
    pub after: DeltaE,
}

/// Median and 95th percentile of ΔE00.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeltaE {
    pub median: f32,
    pub p95: f32,
}

/// Why a calibration was refused.
#[derive(Clone, Debug, PartialEq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// The id of a look in a photo's camera-profile choice: `cal:` and the name in lower case, words joined by `-`
/// (`Adobe Color` → `cal:adobe-color`).
pub fn look_id(name: &str) -> String {
    let slug: Vec<String> = name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect();
    format!("{LOOK_PREFIX}{}", slug.join("-"))
}

/// The name the app shows for a look: Lightroom's profile name (kept in the file, and what photos are matched by)
/// with the vendor's mark replaced by our own look family's and British spelling, so "Adobe Color" shows as
/// "Eboda Colour". Other names keep their words.
pub fn display_name(name: &str) -> String {
    let words: Vec<String> = name
        .split_whitespace()
        .map(|w| match w {
            "Adobe" => "Eboda".to_string(),
            "Color" => "Colour".to_string(),
            "color" => "colour".to_string(),
            other => other.to_string(),
        })
        .collect();
    words.join(" ")
}

/// A short text field: trimmed, not empty, at most `max` characters, no control characters and nothing that reads
/// as a path.
fn check_text(what: &str, s: &str, max: usize) -> Result<(), Error> {
    if s.trim().is_empty() {
        return Err(Error(format!("{what} is empty")));
    }
    if s.chars().count() > max {
        return Err(Error(format!("{what} is longer than {max} characters")));
    }
    if s.chars().any(|c| c.is_control() || c == '/' || c == '\\') {
        return Err(Error(format!("{what} {s:?} holds a control character or a path separator")));
    }
    Ok(())
}

/// A camera or look identifier (make, model, alias, look name): trimmed, not empty, at most `max` characters,
/// no control characters, no backslashes, no path traversal (`..`), and no leading or trailing slash. Slashes
/// inside the identifier are permitted (e.g. phone models like "Xiaomi 17 Ultra/2512BPNDAG" or look names like
/// "Camera PROVIA/STANDARD").
fn check_identifier(what: &str, s: &str, max: usize) -> Result<(), Error> {
    if s.trim().is_empty() {
        return Err(Error(format!("{what} is empty")));
    }
    if s.chars().count() > max {
        return Err(Error(format!("{what} is longer than {max} characters")));
    }
    let trimmed = s.trim();
    if s.chars().any(|c| c.is_control() || c == '\\') || trimmed.starts_with('/') || trimmed.ends_with('/') || s.contains("//") || s.contains("..") {
        return Err(Error(format!("{what} {s:?} holds a control character or reads as a path")));
    }
    Ok(())
}

fn check_matrix(what: &str, m: &Mat3) -> Result<(), Error> {
    if !m.0.iter().flatten().all(|v| v.is_finite() && v.abs() < 16.0) {
        return Err(Error(format!("{what} has an entry that is not finite or too large")));
    }
    if m.determinant().abs() < 1e-6 || m.inverse().is_none() {
        return Err(Error(format!("{what} is not invertible")));
    }
    Ok(())
}

fn check_table(what: &str, t: &HsvTable) -> Result<(), Error> {
    let dims = (1..=360).contains(&t.hue_divisions) && (2..=64).contains(&t.sat_divisions) && (1..=64).contains(&t.val_divisions);
    let len = t.hue_divisions.checked_mul(t.sat_divisions).and_then(|n| n.checked_mul(t.val_divisions));
    if !dims || len.is_none_or(|n| n > MAX_TABLE || n != t.data.len()) {
        return Err(Error(format!("{what}: table dimensions don't match its data")));
    }
    let ok = t
        .data
        .iter()
        .all(|[h, s, v]| h.is_finite() && h.abs() <= 180.0 && s.is_finite() && (0.0..=8.0).contains(s) && v.is_finite() && (0.0..=8.0).contains(v));
    if !ok {
        return Err(Error(format!("{what}: a table entry is out of range")));
    }
    Ok(())
}

impl Calibration {
    /// Read a calibration file and [`validate`](Self::validate) it.
    pub fn parse(text: &str) -> Result<Calibration, Error> {
        if text.len() > MAX_FILE {
            return Err(Error(format!("{} bytes is too large for a calibration", text.len())));
        }
        let c: Calibration = serde_json::from_str(text).map_err(|e| Error(format!("not a calibration file: {e}")))?;
        c.validate()?;
        Ok(c)
    }

    /// The file's text: pretty JSON with the fields in a fixed order (what is committed, so contributions diff
    /// cleanly and carry nothing but the allowed fields).
    pub fn to_json(&self) -> Result<String, Error> {
        let mut s = serde_json::to_string_pretty(self).map_err(|e| Error(e.to_string()))?;
        s.push('\n');
        Ok(s)
    }

    /// Check every field: version, method and licence, text fields, matrices (finite, bounded, invertible), table
    /// shapes and ranges, tone curves, look names (unique ids) and the evidence.
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != VERSION {
            return Err(Error(format!("calibration version {} (this app reads {VERSION})", self.version)));
        }
        check_identifier("make", &self.make, 64)?;
        check_identifier("model", &self.model, 64)?;
        let name = format!("{} {}", self.make.trim(), self.model.trim());
        let err = |e: Error| Error(format!("{name}: {}", e.0));
        if self.aliases.len() > 8 {
            return Err(Error(format!("{name}: more than 8 aliases")));
        }
        for a in &self.aliases {
            check_identifier("alias", a, 64).map_err(err)?;
        }
        if let Some(l) = &self.lens {
            check_identifier("lens", l, 64).map_err(err)?;
        }
        if self.method != self.editor.method() {
            return Err(Error(format!("{name}: unknown method {:?} for {}", self.method, self.editor.name())));
        }
        if self.licence != LICENCE {
            return Err(Error(format!("{name}: licence must be {LICENCE:?}, not {:?}", self.licence)));
        }
        check_text("tool version", &self.tool_version, 40).map_err(err)?;
        if let Some(c) = &self.contributor {
            check_text("contributor", c, 80).map_err(err)?;
        }
        let m = &self.camera;
        for (i, code) in m.illuminants.iter().enumerate() {
            if lcc_raw::color::illuminant_temperature(*code).is_none() {
                return Err(Error(format!("{name}: unknown calibration illuminant {code} (#{})", i + 1)));
            }
        }
        if m.illuminants[0] == m.illuminants[1] {
            return Err(Error(format!("{name}: the two calibration illuminants are the same")));
        }
        for (i, cm) in m.color_matrix.iter().enumerate() {
            check_matrix(&format!("colour matrix {}", i + 1), cm).map_err(err)?;
        }
        for (i, fm) in m.forward_matrix.iter().enumerate() {
            check_matrix(&format!("forward matrix {}", i + 1), fm).map_err(err)?;
        }
        if let Some([a, b]) = &m.hue_sat_map {
            check_table("hue/saturation map 1", a).map_err(err)?;
            check_table("hue/saturation map 2", b).map_err(err)?;
            if (a.hue_divisions, a.sat_divisions, a.val_divisions) != (b.hue_divisions, b.sat_divisions, b.val_divisions) {
                return Err(Error(format!("{name}: the two hue/saturation maps differ in shape")));
            }
        }
        if self.looks.is_empty() || self.looks.len() > MAX_LOOKS {
            return Err(Error(format!("{name}: needs 1 to {MAX_LOOKS} looks")));
        }
        let mut ids: Vec<String> = Vec::new();
        for look in &self.looks {
            check_identifier("look name", &look.name, 64).map_err(err)?;
            let id = look_id(&look.name);
            if id == LOOK_PREFIX || ids.contains(&id) {
                return Err(Error(format!("{name}: look {:?} has no usable or a duplicate id", look.name)));
            }
            ids.push(id);
            if !look.baseline_exposure_offset.is_finite() || look.baseline_exposure_offset.abs() > 4.0 {
                return Err(Error(format!("{name}: look {:?} has an exposure offset out of range", look.name)));
            }
            if let Some(t) = &look.look_table {
                check_table(&format!("look table of {:?}", look.name), t).map_err(err)?;
            }
            if !look.tone_curve.is_empty() {
                let flat: Vec<f64> = look.tone_curve.iter().flatten().map(|v| f64::from(*v)).collect();
                if look.tone_curve.len() > MAX_TONE_POINTS || ToneCurve::from_tag(&flat).is_none() {
                    return Err(Error(format!("{name}: look {:?} has an invalid tone curve", look.name)));
                }
            }
        }
        let e = &self.evidence;
        if !(e.hue_coverage.is_finite() && (0.0..=1.0).contains(&e.hue_coverage)) || e.cct.len() > 64 || e.looks.len() > MAX_LOOKS {
            return Err(Error(format!("{name}: evidence out of range")));
        }
        for l in &e.looks {
            check_identifier("evidence look name", &l.name, 64).map_err(err)?;
            let finite = [l.before.median, l.before.p95, l.after.median, l.after.p95].iter().all(|v| v.is_finite() && (0.0..=200.0).contains(v));
            if !finite {
                return Err(Error(format!("{name}: evidence for {:?} out of range", l.name)));
            }
        }
        Ok(())
    }

    /// The camera's default look (the first).
    pub fn default_look(&self) -> Option<&Look> {
        self.looks.first()
    }

    /// The look with id `id` (`cal:<slug>`, [`look_id`]).
    pub fn look(&self, id: &str) -> Option<&Look> {
        self.looks.iter().find(|l| look_id(&l.name) == id)
    }

    /// Whether the calibration is for a camera (Exif make and model): its model or an alias, compared without case
    /// and a leading brand word; a make naming another brand never matches.
    /// The lens is not compared: see [`Calibration::fits_lens`].
    pub fn matches(&self, make: Option<&str>, model: &str) -> bool {
        let own = super::brand(&self.make);
        if make.map(super::brand).is_some_and(|b| !b.is_empty() && b != own) {
            return false;
        }
        let wanted = super::key(&own, model);
        !wanted.is_empty() && std::iter::once(&self.model).chain(&self.aliases).any(|m| super::key(&own, m) == wanted)
    }

    /// Whether the calibration may serve a photo taken with `lens` (its Exif lens model): a lens-less calibration
    /// serves every photo, a lens-specific one only photos of that lens.
    pub fn fits_lens(&self, lens: Option<&str>) -> bool {
        match &self.lens {
            None => true,
            Some(own) => lens.is_some_and(|l| lcc_raw::dcp::same_lens(own, l)),
        }
    }

    /// Whether this and `other` are for the same camera, lens and editor (one file's worth: `add` refuses to
    /// replace one without `--replace`).
    pub fn same_slot(&self, other: &Calibration) -> bool {
        self.editor == other.editor
            && self.matches(Some(&other.make), &other.model)
            && match (&self.lens, &other.lens) {
                (None, None) => true,
                (Some(a), Some(b)) => lcc_raw::dcp::same_lens(a, b),
                _ => false,
            }
    }

    /// Colour tags of the camera model with `look` on top: what a photo of this camera is developed with.
    pub fn color_data(&self, look: &Look) -> ColorData {
        let m = &self.camera;
        let tone_curve = if look.tone_curve.is_empty() { None } else { Some(ToneCurve { points: look.tone_curve.clone() }) };
        ColorData {
            illuminant: m.illuminants,
            color_matrix: m.color_matrix.map(Some),
            forward_matrix: m.forward_matrix.map(Some),
            baseline_exposure: look.baseline_exposure_offset,
            profile: ProfileLook {
                hue_sat_map: m.hue_sat_map.clone().map(|[a, b]| [Some(a), Some(b)]).unwrap_or_default(),
                look_table: look.look_table.clone(),
                tone_curve,
                gain_table_map: None,
            },
            ..Default::default()
        }
    }

    /// `look` as a camera profile, applied the way a person's own `.dcp` is ([`Dcp::apply`]).
    pub fn look_dcp(&self, look: &Look) -> Dcp {
        Dcp {
            model: profile_model(&super::brand(&self.make), &self.model),
            name: display_name(&look.name),
            copyright: self.contributor.clone().unwrap_or_default(),
            color: self.color_data(look),
        }
    }

    /// The registry form: every name the camera goes by and each look's id and profile.
    pub fn calibrated(&self) -> lcc_raw::dcp::Calibrated {
        let names: Vec<String> = std::iter::once(&self.model).chain(&self.aliases).map(|m| profile_model(&super::brand(&self.make), m)).collect();
        let looks = self.looks.iter().map(|l| (look_id(&l.name), std::sync::Arc::new(self.look_dcp(l)))).collect();
        // only a Lightroom calibration gives the camera a default look; other editors' looks are chosen per photo
        let default = (self.editor == Editor::Lightroom).then(|| self.default_look().map(|l| look_id(&l.name))).flatten();
        lcc_raw::dcp::Calibrated { make: super::brand(&self.make), names, lens: self.lens.as_ref().map(|l| l.trim().to_string()), looks, default }
    }

    /// The file name a calibration is kept under: `<Make>_<Model>.json` for Lightroom, `<Make>_<Model>_<editor>.json`
    /// for another editor, `_<Lens>` after the model for a lens-specific fit; characters outside letters, digits, `-`
    /// and `.` as `_`.
    pub fn file_name(&self) -> String {
        let mut raw = format!("{}_{}", super::brand(&self.make), self.model.trim());
        if let Some(l) = &self.lens {
            raw.push('_');
            raw.push_str(l.trim());
        }
        if self.editor != Editor::Lightroom {
            raw.push('_');
            raw.push_str(self.editor.id());
        }
        let safe: String = raw.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect();
        format!("{safe}.json")
    }
}

/// A camera's profile-model name ("make model", see [`lcc_raw::dcp::covers_model`]): `brand` and the model, or the
/// model alone when it already starts with the brand (Exif "Canon EOS 6D" stays "Canon EOS 6D", issue #1119).
fn profile_model(brand: &str, model: &str) -> String {
    let model = model.trim();
    if model.split_whitespace().next().is_some_and(|w| w.eq_ignore_ascii_case(brand)) { model.to_string() } else { format!("{brand} {model}") }
}

/// The embedded calibration files `(file name, JSON)`.
const FILES: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/calibrations.rs"));

/// The embedded calibrations (a file that fails to validate contributes nothing; a test keeps them valid).
pub fn embedded() -> &'static [Calibration] {
    static DB: OnceLock<Vec<Calibration>> = OnceLock::new();
    DB.get_or_init(|| FILES.iter().filter_map(|(_, text)| Calibration::parse(text).ok()).collect())
}

/// The embedded files as written, `(file name, JSON)`.
pub fn embedded_files() -> &'static [(&'static str, &'static str)] {
    FILES
}

/// The Lightroom calibration for a camera (Exif make and model) and a photo's lens (Exif lens model), if one is
/// embedded: the one for that lens, else the lens-less one, never one for another lens (see the module doc).
pub fn find(make: Option<&str>, model: &str, lens: Option<&str>) -> Option<&'static Calibration> {
    pick(embedded(), make, model, lens)
}

/// [`find`] over `list`: Lightroom calibrations of the camera, the lens's own first, else the lens-less one.
pub fn pick<'a>(list: &'a [Calibration], make: Option<&str>, model: &str, lens: Option<&str>) -> Option<&'a Calibration> {
    let mut of_camera = list.iter().filter(|c| c.editor == Editor::Lightroom && c.matches(make, model) && c.fits_lens(lens));
    let first = of_camera.next()?;
    if first.lens.is_some() {
        return Some(first);
    }
    Some(of_camera.find(|c| c.lens.is_some()).unwrap_or(first))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample() -> Calibration {
        let fm = Mat3([[0.70, 0.18, 0.08], [0.28, 0.80, -0.08], [0.02, -0.15, 0.95]]);
        let cm1 = Mat3([[0.9, -0.3, -0.05], [-0.5, 1.4, 0.1], [-0.1, 0.3, 0.6]]);
        let cm2 = Mat3([[0.8, -0.25, -0.05], [-0.45, 1.3, 0.15], [-0.08, 0.22, 0.7]]);
        let table = HsvTable { hue_divisions: 6, sat_divisions: 2, val_divisions: 1, data: vec![[0.0, 1.0, 1.0]; 12], srgb_value: false };
        Calibration {
            version: VERSION,
            make: "SONY".into(),
            model: "ILCE-7M4".into(),
            aliases: vec![],
            lens: None,
            editor: Editor::Lightroom,
            method: METHOD.into(),
            tool_version: "26.43.4".into(),
            contributor: Some("A Contributor".into()),
            licence: LICENCE.into(),
            camera: CameraModel {
                illuminants: [17, 21],
                color_matrix: [cm1, cm2],
                forward_matrix: [fm, fm],
                hue_sat_map: Some([table.clone(), table.clone()]),
            },
            looks: vec![
                Look {
                    name: "Adobe Color".into(),
                    baseline_exposure_offset: 0.25,
                    look_table: Some(table),
                    tone_curve: vec![[0.0, 0.0], [0.18, 0.3], [1.0, 1.0]],
                },
                Look { name: "Adobe Standard".into(), baseline_exposure_offset: 0.0, look_table: None, tone_curve: vec![] },
            ],
            evidence: Evidence {
                photos: 30,
                samples: 90_000,
                cct: vec![CctBin { from_k: 3000, photos: 8 }, CctBin { from_k: 5000, photos: 22 }],
                hue_coverage: 0.8,
                white_points: 30,
                looks: vec![LookEvidence {
                    name: "Adobe Color".into(),
                    photos: 24,
                    held_out: 6,
                    before: DeltaE { median: 5.0, p95: 12.0 },
                    after: DeltaE { median: 1.2, p95: 3.4 },
                }],
            },
        }
    }

    #[test]
    fn editors_name_their_files_methods_and_defaults() {
        let lr = sample();
        assert_eq!(lr.editor, Editor::Lightroom);
        // a file from before the field reads as Lightroom's, and is written without it
        let json = lr.to_json().unwrap();
        assert!(!json.contains("\"editor\""));
        assert_eq!(Calibration::parse(&json).unwrap(), lr);
        let mut c1 = Calibration { editor: Editor::CaptureOne, method: Editor::CaptureOne.method(), ..lr.clone() };
        c1.validate().unwrap();
        assert!(c1.file_name().ends_with("_capture-one.json") && c1.file_name() != lr.file_name());
        let back = Calibration::parse(&c1.to_json().unwrap()).unwrap();
        assert_eq!(back.editor, Editor::CaptureOne);
        // the method must be the editor's
        c1.method = METHOD.into();
        assert!(c1.validate().is_err());
        assert!(Calibration::parse(&json.replace("\"version\"", "\"editor\": \"paint\",\n  \"version\"")).is_err());
        // only Lightroom's calibration gives the camera a default look
        assert!(lr.calibrated().default.is_some());
        let c1 = Calibration { editor: Editor::Darktable, method: Editor::Darktable.method(), ..lr };
        assert!(c1.calibrated().default.is_none() && !c1.calibrated().looks.is_empty());
        for e in Editor::ALL {
            assert_eq!(Editor::parse(e.id()), Some(e));
            assert_eq!(Editor::parse(e.name()), Some(e));
        }
        assert_eq!(Editor::parse("CaptureOne"), Some(Editor::CaptureOne));
        assert_eq!(Editor::parse("paint"), None);
        assert_eq!(Editor::parse(""), None);
    }

    fn with_lens(lens: Option<&str>, look: &str) -> Calibration {
        let mut c = sample();
        c.lens = lens.map(str::to_string);
        c.looks[0].name = look.into();
        c
    }

    #[test]
    fn lens_calibrations_name_their_files_and_round_trip() {
        let plain = sample();
        assert_eq!(plain.file_name(), "SONY_ILCE-7M4.json");
        assert!(!plain.to_json().unwrap().contains("\"lens\""), "a lens-less file is written as before");
        let wide = with_lens(Some("FE 24mm F1.4 GM"), "Adobe Color");
        wide.validate().unwrap();
        assert_eq!(wide.file_name(), "SONY_ILCE-7M4_FE_24mm_F1.4_GM.json");
        assert_eq!(Calibration::parse(&wide.to_json().unwrap()).unwrap(), wide);
        let c1 = Calibration { editor: Editor::CaptureOne, method: Editor::CaptureOne.method(), ..wide.clone() };
        assert_eq!(c1.file_name(), "SONY_ILCE-7M4_FE_24mm_F1.4_GM_capture-one.json");
        // a slash inside a lens name is allowed (as in models) and never reaches the file name
        let slashed = with_lens(Some("Main 23mm f/1.6"), "Adobe Color");
        slashed.validate().unwrap();
        assert!(!slashed.file_name().contains('/'));
        assert_ne!(slashed.file_name(), wide.file_name());
        assert!(wide.calibrated().lens.is_some() && plain.calibrated().lens.is_none());
    }

    #[test]
    fn lens_match_is_preferred_lens_less_is_the_fallback_and_other_lenses_never_serve() {
        let list = vec![with_lens(Some("Wide 24mm"), "Wide Look"), with_lens(None, "Any Look"), with_lens(Some("Ultra 14mm"), "Ultra Look")];
        let name = |lens: Option<&str>| pick(&list, Some("SONY"), "ILCE-7M4", lens).map(|c| c.looks[0].name.clone());
        assert_eq!(name(Some("Wide 24mm")).as_deref(), Some("Wide Look"));
        assert_eq!(name(Some("  wide   24MM ")).as_deref(), Some("Wide Look"), "case and spacing don't matter");
        assert_eq!(name(Some("Ultra 14mm")).as_deref(), Some("Ultra Look"));
        assert_eq!(name(Some("Tele 70mm")).as_deref(), Some("Any Look"), "an unknown lens takes the lens-less fit");
        assert_eq!(name(None).as_deref(), Some("Any Look"), "no lens on the photo: the lens-less fit");
        assert_eq!(name(Some("")).as_deref(), Some("Any Look"));
        // without a lens-less fit, a photo of another (or no) lens gets none rather than another lens's
        let only_wide = vec![with_lens(Some("Wide 24mm"), "Wide Look")];
        assert!(pick(&only_wide, Some("SONY"), "ILCE-7M4", Some("Ultra 14mm")).is_none());
        assert!(pick(&only_wide, Some("SONY"), "ILCE-7M4", None).is_none());
        assert!(pick(&only_wide, Some("CANON"), "ILCE-7M4", Some("Wide 24mm")).is_none(), "another camera never");
        // the slot a file holds: camera, lens and editor
        assert!(list[0].same_slot(&with_lens(Some("WIDE 24mm"), "x")));
        assert!(!list[0].same_slot(&list[1]) && !list[0].same_slot(&list[2]) && list[1].same_slot(&sample()));
    }

    #[test]
    fn round_trips_through_json() {
        let c = sample();
        let back = Calibration::parse(&c.to_json().unwrap()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn unknown_fields_are_refused() {
        // nothing outside the allow-list can travel in a contributed file (file names, dates, GPS…)
        let json = sample().to_json().unwrap();
        let top = json.replacen("\"version\"", "\"file_names\": [\"DSC0001.ARW\"],\n  \"version\"", 1);
        assert!(Calibration::parse(&top).is_err());
        let nested = json.replacen("\"photos\"", "\"gps\": [1.0, 2.0],\n    \"photos\"", 1);
        assert!(Calibration::parse(&nested).is_err());
    }

    #[test]
    fn hostile_files_are_errors_not_panics() {
        let ok = sample();
        let mut cases: Vec<Calibration> = Vec::new();
        let mut c = ok.clone();
        c.version = 2;
        cases.push(c);
        let mut c = ok.clone();
        c.camera.forward_matrix[0] = Mat3([[f64::NAN; 3]; 3]);
        cases.push(c);
        let mut c = ok.clone();
        c.camera.color_matrix[1] = Mat3([[0.0; 3]; 3]);
        cases.push(c);
        let mut c = ok.clone();
        c.camera.illuminants = [21, 21];
        cases.push(c);
        let mut c = ok.clone();
        c.camera.illuminants = [999, 21];
        cases.push(c);
        let mut c = ok.clone();
        if let Some([a, _]) = c.camera.hue_sat_map.as_mut() {
            a.hue_divisions = 7;
        }
        cases.push(c);
        let mut c = ok.clone();
        if let Some([a, _]) = c.camera.hue_sat_map.as_mut() {
            a.data[0] = [400.0, 1.0, 1.0];
        }
        cases.push(c);
        let mut c = ok.clone();
        c.looks[0].tone_curve = vec![[0.0, 0.0], [0.5, 0.4], [0.4, 0.6], [1.0, 1.0]];
        cases.push(c);
        let mut c = ok.clone();
        c.looks[1].name = "adobe  COLOR".into();
        cases.push(c);
        let mut c = ok.clone();
        c.looks.clear();
        cases.push(c);
        let mut c = ok.clone();
        c.model = "/etc/passwd".into();
        cases.push(c);
        let mut c = ok.clone();
        c.model = "foo/../bar".into();
        cases.push(c);
        let mut c = ok.clone();
        c.model = "foo//bar".into();
        cases.push(c);
        let mut c = ok.clone();
        c.model = "foo\\bar".into();
        cases.push(c);
        let mut c = ok.clone();
        c.model = "model/".into();
        cases.push(c);
        let mut c = ok.clone();
        c.contributor = Some("/home/someone/photos".into());
        cases.push(c);
        let mut c = ok.clone();
        c.licence = "proprietary".into();
        cases.push(c);
        let mut c = ok.clone();
        c.evidence.hue_coverage = f32::INFINITY;
        cases.push(c);
        let mut c = ok.clone();
        c.looks[0].baseline_exposure_offset = 1e9;
        cases.push(c);
        // a lens is an identifier like the model: no paths, control characters, empties or huge strings
        for lens in ["", "   ", "/etc/passwd", "a/../b", "lens\\x", "lens\u{0}", "lens\n2", &"L".repeat(65)] {
            let mut c = ok.clone();
            c.lens = Some(lens.to_string());
            cases.push(c);
        }
        for (i, c) in cases.iter().enumerate() {
            assert!(c.validate().is_err(), "case {i} should be refused");
            // and through the text form, where it arrives from
            if let Ok(json) = serde_json::to_string(c) {
                assert!(Calibration::parse(&json).is_err(), "case {i} should be refused as text");
            }
        }
        for text in ["", "{", "[]", "null", "{\"version\": 1}", &"[".repeat(10_000)] {
            assert!(Calibration::parse(text).is_err());
        }
        assert!(Calibration::parse(&" ".repeat(MAX_FILE + 1)).is_err());
    }

    #[test]
    fn slash_in_camera_model_and_look_names_is_allowed() {
        let mut c = sample();
        c.make = "Xiaomi".into();
        c.model = "Xiaomi 17 Ultra/2512BPNDAG".into();
        c.aliases = vec!["17 Ultra/2512BPNDAG".into()];
        c.looks[0].name = "Camera PROVIA/STANDARD".into();
        c.evidence.looks[0].name = "Camera PROVIA/STANDARD".into();
        assert!(c.validate().is_ok());
        assert_eq!(c.file_name(), "XIAOMI_Xiaomi_17_Ultra_2512BPNDAG.json");
        assert_eq!(look_id(&c.looks[0].name), "cal:camera-provia-standard");
        assert!(c.matches(Some("Xiaomi"), "Xiaomi 17 Ultra/2512BPNDAG"));
        assert!(c.matches(None, "17 ultra/2512bpndag"));
        assert_eq!(c.look_dcp(&c.looks[0]).model, "Xiaomi 17 Ultra/2512BPNDAG");
    }

    #[test]
    fn profile_model_does_not_repeat_the_make() {
        let mut c = sample();
        c.make = "Canon".into();
        c.model = "Canon EOS 6D".into();
        c.aliases = vec![];
        let d = c.look_dcp(&c.looks[0]);
        assert_eq!(d.model, "Canon EOS 6D");
        assert!(lcc_raw::dcp::covers_model(&d.model, "Canon EOS 6D"));
        assert!(c.calibrated().names.iter().any(|n| lcc_raw::dcp::covers_model(n, "Canon EOS 6D")));
        c.make = "SONY".into();
        c.model = "ILCE-7M5".into();
        assert_eq!(c.look_dcp(&c.looks[0]).model, "SONY ILCE-7M5");
    }

    #[test]
    fn looks_have_stable_ids() {
        let c = sample();
        assert_eq!(look_id("Adobe Color"), "cal:adobe-color");
        assert_eq!(look_id("Camera  Neutral v2"), "cal:camera-neutral-v2");
        assert_eq!(c.look("cal:adobe-standard").map(|l| l.name.as_str()), Some("Adobe Standard"));
        assert_eq!(c.default_look().map(|l| l.name.as_str()), Some("Adobe Color"));
    }

    #[test]
    fn looks_show_our_names_but_match_lightrooms() {
        assert_eq!(display_name("Adobe Color"), "Eboda Colour");
        assert_eq!(display_name("Adobe Vivid"), "Eboda Vivid");
        assert_eq!(display_name("Adaptive Color"), "Adaptive Colour");
        assert_eq!(display_name("Camera Standard"), "Camera Standard");
        let c = sample();
        // the profile shows our name, the id still matches Lightroom's `crs:Look` name
        assert_eq!(c.look_dcp(&c.looks[0]).name, "Eboda Colour");
        assert_eq!(c.calibrated().looks[0].0, "cal:adobe-color");
    }

    #[test]
    fn matches_its_camera_only() {
        let c = sample();
        assert!(c.matches(Some("SONY"), "ILCE-7M4"));
        assert!(c.matches(None, "ilce-7m4"));
        assert!(c.matches(Some("Sony Corporation"), "SONY ILCE-7M4"));
        assert!(!c.matches(Some("NIKON"), "ILCE-7M4"));
        assert!(!c.matches(Some("SONY"), "ILCE-7M3"));
    }

    #[test]
    fn a_look_becomes_a_profile_with_its_tables_and_curve() {
        let c = sample();
        let d = c.look_dcp(&c.looks[0]);
        assert!(lcc_raw::color::has_matrix(&d.color));
        assert!(d.color.profile.look_table.is_some());
        assert!(d.color.profile.tone_curve.is_some());
        assert!(d.color.profile.hue_sat_map.iter().all(Option::is_some));
        assert_eq!(d.color.baseline_exposure, 0.25);
        let plain = c.look_dcp(&c.looks[1]);
        assert!(plain.color.profile.tone_curve.is_none() && plain.color.profile.look_table.is_none());
        assert_eq!(c.file_name(), "SONY_ILCE-7M4.json");
    }

    #[test]
    fn embedded_calibrations_are_valid() {
        for (name, text) in embedded_files() {
            let c = Calibration::parse(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(*name, c.file_name(), "{name} should be named after its camera");
            // (a Windows checkout may have turned LF into CRLF)
            assert_eq!(
                c.to_json().unwrap(),
                text.replace("\r\n", "\n"),
                "{name} should be written in the canonical form (`calibrate-lightroom add`)"
            );
        }
        // embedded calibrations have sane hue shifts
        {
            for (name, text) in embedded_files() {
                let c = Calibration::parse(text).unwrap();
                if let Some(tables) = &c.camera.hue_sat_map {
                    for (i, t) in tables.iter().enumerate() {
                        for entry in &t.data {
                            assert!(entry[0].abs() <= 75.0, "{name} table {i}: extreme hue shift {:.1}° (must be <= 75°)", entry[0]);
                            assert!(entry[1] <= 4.0, "{name} table {i}: extreme sat scale {:.2} (must be <= 4.0)", entry[1]);
                        }
                    }
                }
            }
        }
        // nikon z6 3 calibration has no extreme hue sat map
        {
            let Some(cal) = embedded().iter().find(|c| c.model == "NIKON Z6_3") else {
                panic!("the Nikon Z 6III calibration is embedded");
            };
            // The overfitted hue_sat_map caused purple discoloration in skies and cool shadows.
            // It must either be absent or strictly bounded.
            if let Some(tables) = &cal.camera.hue_sat_map {
                for t in tables {
                    for entry in &t.data {
                        assert!(entry[0].abs() < 45.0, "Z6_3 hue shift too large: {}", entry[0]);
                    }
                }
            }
        }
    }

    /// Issue #451 (Sony a1, in-camera tungsten white balance): the shipped a1 calibration carries
    /// both illuminants of its dual-illuminant pair, and an extreme (tungsten) as-shot white
    /// balance read through it — the ARW `WB_RGGBLevels` gains and the camera neutral they name —
    /// renders that neutral neutral and uses the tungsten-side (illuminant A) matrices, as an ARW
    /// of the a1 does at decode ([`lcc_raw::dcp::Dcp::apply`] writes the calibration's colour into
    /// the raw before the as-shot white is read).
    #[test]
    fn the_a1_calibration_balances_an_extreme_as_shot_white() {
        let Some(cal) = embedded().iter().find(|c| c.model == "ILCE-1") else {
            panic!("the Sony a1 calibration is embedded");
        };
        assert_eq!(cal.camera.illuminants, [17, 21], "the calibration must cover tungsten (A) and daylight (D65)");
        let color = cal.color_data(cal.default_look().expect("the a1 calibration ships a default look"));
        // gains `[R/G, 1, B/G]` of a neutral surface, as an ARW records them at each preset
        for kelvin in [3000.0_f64, 3200.0, 5600.0] {
            let gains = lcc_raw::color::wb_multipliers(&color, lcc_color::cct::temp_tint_to_xy(kelvin, 0.0));
            let xy = lcc_raw::color::as_shot_white_of(&color, Some(gains.map(|v| v as f32)));
            let (t, _) = lcc_color::cct::xy_to_temp_tint(xy);
            assert!((t - kelvin).abs() < 30.0, "{kelvin} K reads {t:.0} K");
            let tr = lcc_raw::color::camera_transform_of(&color, xy);
            let neutral = lcc_raw::color::camera_neutral(&color, xy);
            let pixel = tr.matrix.apply(std::array::from_fn(|i| neutral[i] * f64::from(tr.wb[i])));
            let ok = (0..3).all(|i| (pixel[i] - 1.0).abs() < 1e-6);
            assert!(ok, "{kelvin} K: {pixel:?}");
            // the picker / auto white balance read the same white back off the developed pixel
            let seen = lcc_raw::color::neutral_white(&color, xy, pixel).unwrap();
            assert!((seen.x - xy.x).abs() < 1e-6 && (seen.y - xy.y).abs() < 1e-6, "{kelvin} K: {seen:?} vs {xy:?}");
        }
        // the tungsten white sits on the calibration's illuminant-A side, not D65's
        let tungsten = lcc_raw::color::as_shot_white_of(
            &color,
            Some(lcc_raw::color::wb_multipliers(&color, lcc_color::cct::temp_tint_to_xy(2856.0, 0.0)).map(|v| v as f32)),
        );
        assert!(lcc_raw::color::illuminant_weight(&color, tungsten) > 0.99, "{}", lcc_raw::color::illuminant_weight(&color, tungsten));
    }
}
