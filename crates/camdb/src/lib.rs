//! Per-model camera data with provenance.
//!
//! One [`Entry`] per camera model (Exif make and model, plus aliases) carries:
//! - colour matrices for two calibration illuminants (DNG `ColorMatrix1/2`, XYZ → camera, and optionally
//!   `ForwardMatrix1/2`), interpolated by the white balance's correlated colour temperature through the DNG colour
//!   model in [`lcc_raw::color`] (inverse-CCT weighting, DNG Specification 1.7 chapter 6);
//! - white-balance presets (camera RGB multipliers, green = 1);
//! - black and white level overrides, for models whose files carry wrong or no levels;
//! - a [`Provenance`]: where the numbers came from and who contributed them.
//!
//! Two sources feed the database:
//! - human-editable TOML in `data/cameras/<maker>.toml`, embedded at build time (format below; `<cli> camdb extract`
//!   writes entries from camera-native DNGs);
//! - the matrices fitted to measured spectral sensitivities that [`lcc_raw::spectral`] already carries (rawtoaces-data,
//!   Apache-2.0), exposed here with [`Source::SpectralSensitivities`] provenance rather than copied.
//!
//! [`find`] returns the best entry for a camera: TOML entries ranked by [`Source::rank`], then the spectral table.
//!
//! **Data rules.** No Adobe DCP/LCP profile data, no matrices taken from Adobe-converted DNGs (the `Software` tag is
//! checked by [`extract`]), no data from GPL projects (dcraw's `adobe_coeff` table and its descendants, darktable,
//! RawTherapee). Every entry names its source.
//!
//! ```toml
//! [[camera]]
//! make = "Leica Camera AG"
//! model = "LEICA Q2"
//! aliases = []
//! illuminants = [17, 21]                       # DNG CalibrationIlluminant codes (17 = StdA, 21 = D65)
//! color_matrix_1 = [[..3..], [..3..], [..3..]] # XYZ → camera under illuminant 1 (row-major)
//! color_matrix_2 = [[..3..], [..3..], [..3..]] # … under illuminant 2; either may be left out
//! forward_matrix_1 = [...]                     # optional ForwardMatrix1/2, same shape
//! black = 512.0                                # optional override
//! white = 16383.0                              # optional override
//! wb_presets = [{ name = "Daylight", multipliers = [2.1, 1.0, 1.5] }]
//! provenance = { source = "native-dng", detail = "Software: Q2 1.0", contributor = "…" }
//! ```
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod extract;

use lcc_color::{Mat3, Xy};
use lcc_raw::{ColorData, RawImage};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Where an entry's numbers come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// A DNG written by the camera itself (never an Adobe or other converter's DNG).
    NativeDng,
    /// Our own measurement from a colour-chart shot.
    Chart,
    /// The manufacturer's published SDK or documentation.
    ManufacturerDoc,
    /// The camera's own maker notes (e.g. white-balance preset multipliers).
    MakerNote,
    /// Fitted to measured spectral sensitivities.
    SpectralSensitivities,
    /// Estimated (e.g. fitted to the camera's own JPEGs).
    Estimated,
}

impl Source {
    /// Preference when several entries name one camera (lower is better).
    pub fn rank(self) -> u8 {
        match self {
            Source::NativeDng => 0,
            Source::Chart => 1,
            Source::ManufacturerDoc => 2,
            Source::MakerNote => 3,
            Source::SpectralSensitivities => 4,
            Source::Estimated => 5,
        }
    }

    /// Whether the numbers are measured for this model (not a model-free estimate). Measured colour is preferred to
    /// a per-photo fit; spectral and estimated colour only replace the neutral fallback.
    pub fn is_measured(self) -> bool {
        matches!(self, Source::NativeDng | Source::Chart | Source::ManufacturerDoc)
    }
}

/// An entry's origin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub source: Source,
    /// What exactly: the DNG's `Software` tag, the chart and light, the dataset and commit…
    #[serde(default)]
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contributor: Option<String>,
}

/// A white-balance preset: camera RGB multipliers (green = 1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WbPreset {
    pub name: String,
    pub multipliers: [f64; 3],
}

/// One camera model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "EntryToml", into = "EntryToml")]
pub struct Entry {
    pub make: String,
    pub model: String,
    pub aliases: Vec<String>,
    /// DNG `CalibrationIlluminant1/2` codes of the two matrices.
    pub illuminants: [u16; 2],
    /// XYZ → camera under each illuminant (`ColorMatrix1/2`); one may be missing.
    pub color_matrix: [Option<Mat3>; 2],
    /// White-balanced camera → XYZ D50 (`ForwardMatrix1/2`), when known.
    pub forward_matrix: [Option<Mat3>; 2],
    pub wb_presets: Vec<WbPreset>,
    /// Black level override (sample units), every channel.
    pub black: Option<f32>,
    /// White (clip) level override (sample units), every channel.
    pub white: Option<f32>,
    pub provenance: Provenance,
}

/// [`Entry`] as written in a maker file (TOML has no null, so each matrix is its own optional key).
#[derive(Clone, Serialize, Deserialize)]
struct EntryToml {
    make: String,
    model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    aliases: Vec<String>,
    illuminants: [u16; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color_matrix_1: Option<Mat3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    color_matrix_2: Option<Mat3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    forward_matrix_1: Option<Mat3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    forward_matrix_2: Option<Mat3>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    black: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    white: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    wb_presets: Vec<WbPreset>,
    provenance: Provenance,
}

impl From<EntryToml> for Entry {
    fn from(t: EntryToml) -> Entry {
        Entry {
            make: t.make,
            model: t.model,
            aliases: t.aliases,
            illuminants: t.illuminants,
            color_matrix: [t.color_matrix_1, t.color_matrix_2],
            forward_matrix: [t.forward_matrix_1, t.forward_matrix_2],
            wb_presets: t.wb_presets,
            black: t.black,
            white: t.white,
            provenance: t.provenance,
        }
    }
}

impl From<Entry> for EntryToml {
    fn from(e: Entry) -> EntryToml {
        let [color_matrix_1, color_matrix_2] = e.color_matrix;
        let [forward_matrix_1, forward_matrix_2] = e.forward_matrix;
        EntryToml {
            make: e.make,
            model: e.model,
            aliases: e.aliases,
            illuminants: e.illuminants,
            color_matrix_1,
            color_matrix_2,
            forward_matrix_1,
            forward_matrix_2,
            black: e.black,
            white: e.white,
            wb_presets: e.wb_presets,
            provenance: e.provenance,
        }
    }
}

/// Why data could not be read.
#[derive(Debug, PartialEq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// What [`Entry::apply`] changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub matrix: bool,
    pub levels: bool,
}

impl Entry {
    /// The entry as DNG colour data (illuminants and matrices only).
    pub fn color_data(&self) -> ColorData {
        ColorData { illuminant: self.illuminants, color_matrix: self.color_matrix, forward_matrix: self.forward_matrix, ..Default::default() }
    }

    /// XYZ → camera for a white of `temp_k` kelvin (no tint), interpolated between the two matrices by inverse CCT.
    pub fn xyz_to_camera_at(&self, temp_k: f64) -> Option<Mat3> {
        if !lcc_raw::color::has_matrix(&self.color_data()) || !temp_k.is_finite() || temp_k <= 0.0 {
            return None;
        }
        Some(lcc_raw::color::xyz_to_camera(&self.color_data(), lcc_color::cct::temp_tint_to_xy(temp_k, 0.0)))
    }

    /// Camera transform (camera → XYZ D50) for white `xy`, as the raw input transform computes it.
    pub fn camera_to_xyz_d50(&self, white: Xy) -> Mat3 {
        lcc_raw::color::camera_to_xyz_d50(&self.color_data(), white)
    }

    /// Give `raw` this entry's data: the matrices when the file has none of its own, and the level overrides.
    pub fn apply(&self, raw: &mut RawImage) -> Applied {
        let mut applied = Applied::default();
        if !lcc_raw::color::has_matrix(&raw.color) && lcc_raw::color::has_matrix(&self.color_data()) {
            raw.color.illuminant = self.illuminants;
            raw.color.color_matrix = self.color_matrix;
            raw.color.forward_matrix = self.forward_matrix;
            applied.matrix = true;
        }
        let cpp = raw.cpp.max(1);
        if let Some(b) = self.black {
            raw.black = lcc_raw::BlackLevel { values: vec![b; cpp], ..lcc_raw::BlackLevel::uniform(b) };
            applied.levels = true;
        }
        if let Some(w) = self.white {
            raw.white = vec![w; cpp];
            applied.levels = true;
        }
        applied
    }

    /// Check the numbers: finite, invertible matrices, known illuminants, positive multipliers and levels.
    pub fn validate(&self) -> Result<(), Error> {
        let name = format!("{} {}", self.make, self.model);
        if self.make.trim().is_empty() || self.model.trim().is_empty() {
            return Err(Error(format!("{name}: make and model are required")));
        }
        if self.color_matrix.iter().all(Option::is_none) {
            return Err(Error(format!("{name}: no colour matrix")));
        }
        for (i, m) in self.color_matrix.iter().enumerate() {
            if m.is_some() && lcc_raw::color::illuminant_temperature(self.illuminants[i]).is_none() {
                return Err(Error(format!("{name}: unknown calibration illuminant {}", self.illuminants[i])));
            }
        }
        for m in self.color_matrix.iter().chain(&self.forward_matrix).flatten() {
            if !m.0.iter().flatten().all(|v| v.is_finite() && v.abs() < 100.0) || m.inverse().is_none() {
                return Err(Error(format!("{name}: matrix is not finite and invertible")));
            }
        }
        for p in &self.wb_presets {
            if !p.multipliers.iter().all(|v| v.is_finite() && *v > 0.0) {
                return Err(Error(format!("{name}: preset {} needs positive multipliers", p.name)));
            }
        }
        if let (Some(b), Some(w)) = (self.black, self.white)
            && !(b.is_finite() && w.is_finite() && b >= 0.0 && w > b)
        {
            return Err(Error(format!("{name}: levels need 0 ≤ black < white")));
        }
        for v in [self.black, self.white].into_iter().flatten() {
            if !v.is_finite() || v < 0.0 {
                return Err(Error(format!("{name}: levels must be finite and not negative")));
            }
        }
        Ok(())
    }

    fn matches(&self, make: Option<&str>, model: &str) -> bool {
        let row_brand = brand(&self.make);
        if make.map(brand).is_some_and(|b| !b.is_empty() && b != row_brand) {
            return false;
        }
        let wanted = key(&row_brand, model);
        !wanted.is_empty() && std::iter::once(&self.model).chain(&self.aliases).any(|m| key(&row_brand, m) == wanted)
    }
}

#[derive(Serialize, Deserialize)]
struct File {
    #[serde(default)]
    camera: Vec<Entry>,
}

/// Read a maker file. Every entry is validated.
pub fn parse_toml(text: &str) -> Result<Vec<Entry>, Error> {
    let file: File = toml::from_str(text).map_err(|e| Error(e.to_string()))?;
    for e in &file.camera {
        e.validate()?;
    }
    Ok(file.camera)
}

/// Write entries in the maker-file format.
pub fn to_toml(entries: &[Entry]) -> Result<String, Error> {
    toml::to_string(&File { camera: entries.to_vec() }).map_err(|e| Error(e.to_string()))
}

/// The embedded maker files.
const FILES: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/cameras.rs"));

/// Entries of the embedded maker files (a file that fails to parse contributes nothing; a test keeps them valid).
pub fn embedded() -> &'static [Entry] {
    static DB: OnceLock<Vec<Entry>> = OnceLock::new();
    DB.get_or_init(|| FILES.iter().filter_map(|(_, text)| parse_toml(text).ok()).flatten().collect())
}

fn spectral_entry(c: &lcc_raw::spectral::Camera) -> Entry {
    Entry {
        make: c.make.into(),
        model: c.model.into(),
        aliases: vec![],
        illuminants: [17, 21],
        color_matrix: c.color_matrix.map(Some),
        forward_matrix: c.forward_matrix.map(Some),
        wb_presets: vec![],
        black: None,
        white: None,
        provenance: Provenance {
            source: Source::SpectralSensitivities,
            detail: "rawtoaces-data spectral sensitivities (Academy Software Foundation, Apache-2.0)".into(),
            contributor: None,
        },
    }
}

/// The best entry for a camera's Exif make and model: embedded entries by provenance rank, then the spectral
/// sensitivity table (its aliases included).
pub fn find(make: Option<&str>, model: &str) -> Option<Entry> {
    let own = embedded().iter().filter(|e| e.matches(make, model)).min_by_key(|e| e.provenance.source.rank());
    own.cloned().or_else(|| lcc_raw::spectral::find(make, model).map(spectral_entry))
}

/// Every entry: the embedded files, then the spectral table.
pub fn entries() -> Vec<Entry> {
    embedded().iter().cloned().chain(lcc_raw::spectral::cameras().iter().map(spectral_entry)).collect()
}

/// First word of a make, upper case ("NIKON CORPORATION" → "NIKON").
fn brand(make: &str) -> String {
    make.split_whitespace().next().unwrap_or_default().to_uppercase()
}

/// A model name without case and a leading brand word ("LEICA Q2" → "Q2").
fn key(brand: &str, model: &str) -> String {
    let mut words: Vec<String> = model.split_whitespace().map(str::to_uppercase).collect();
    if words.len() > 1 && words.first().is_some_and(|w| *w == brand) {
        words.remove(0);
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[[camera]]
make = "Example Optical"
model = "EXAMPLE X1"
aliases = ["X1 Kit"]
illuminants = [17, 21]
color_matrix_1 = [[1.2, -0.3, -0.1], [-0.4, 1.3, 0.1], [-0.05, 0.2, 0.6]]
color_matrix_2 = [[0.8, -0.2, -0.05], [-0.4, 1.2, 0.2], [-0.1, 0.25, 0.55]]
black = 512.0
white = 16000.0
wb_presets = [{ name = "Daylight", multipliers = [2.0, 1.0, 1.6] }]
provenance = { source = "native-dng", detail = "Software: X1 1.0" }
"#;

    fn raw_image() -> RawImage {
        RawImage {
            format: lcc_raw::RawFormat::Nef,
            width: 2,
            height: 2,
            cpp: 1,
            data: lcc_raw::RawData::U16(vec![600; 4]),
            cfa: lcc_raw::Cfa::bayer("RGGB"),
            bits: 14,
            black: lcc_raw::BlackLevel::uniform(0.0),
            white: vec![16383.0],
            active_area: lcc_raw::Rect::new(0, 0, 2, 2),
            crop: lcc_raw::Rect::new(0, 0, 2, 2),
            orientation: lcc_raw::Orientation::Normal,
            color: ColorData::default(),
            wb_multipliers: None,
            linearized: false,
            opcodes: lcc_raw::OpcodeLists::default(),
            metadata: lcc_raw::Metadata::default(),
            lens_metadata: Default::default(),
            pixel_aspect: 1.0,
        }
    }

    fn sample() -> Entry {
        parse_toml(SAMPLE).unwrap().remove(0)
    }

    #[test]
    fn maker_files_round_trip() {
        let e = sample();
        assert_eq!(e.provenance.source, Source::NativeDng);
        assert_eq!(e.forward_matrix, [None, None]);
        assert_eq!(e.wb_presets[0].multipliers, [2.0, 1.0, 1.6]);
        let back = parse_toml(&to_toml(std::slice::from_ref(&e)).unwrap()).unwrap();
        assert_eq!(back, vec![e]);
    }

    #[test]
    fn embedded_files_parse() {
        for (name, text) in FILES {
            let entries = parse_toml(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            for e in entries {
                assert!(!e.provenance.detail.is_empty(), "{name}: {} needs a provenance detail", e.model);
                assert_ne!(e.provenance.source, Source::Estimated, "{name}: estimated data stays out of the database");
            }
        }
    }

    #[test]
    fn hostile_entries_are_rejected() {
        for bad in [
            SAMPLE.replace("1.2, -0.3", "NaN, -0.3"),
            SAMPLE.replace("illuminants = [17, 21]", "illuminants = [99, 21]"),
            SAMPLE.replace("[2.0, 1.0, 1.6]", "[0.0, 1.0, 1.6]"),
            SAMPLE.replace("white = 16000.0", "white = 100.0"),
            SAMPLE.replace("[[1.2, -0.3, -0.1], [-0.4, 1.3, 0.1], [-0.05, 0.2, 0.6]]", "[[1, 0, 0], [1, 0, 0], [0, 0, 1]]"),
            SAMPLE.replace("make = \"Example Optical\"", "make = \"\""),
            "[[camera]]\nmake = 3".to_string(),
        ] {
            assert!(parse_toml(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn matrices_interpolate_by_white_temperature() {
        let e = sample();
        let [a, d65] = e.color_matrix.map(Option::unwrap);
        let close = |x: Mat3, y: Mat3| x.0.iter().flatten().zip(y.0.iter().flatten()).all(|(p, q)| (p - q).abs() < 1e-9);
        // at and beyond the calibration illuminants: that matrix alone
        assert!(close(e.xyz_to_camera_at(2856.0).unwrap(), a));
        assert!(close(e.xyz_to_camera_at(2000.0).unwrap(), a));
        assert!(close(e.xyz_to_camera_at(10000.0).unwrap(), d65));
        // between them: weighted by inverse temperature (DNG spec chapter 6)
        let t = 4000.0;
        let xy = lcc_color::cct::temp_tint_to_xy(t, 0.0);
        let t_eff = lcc_color::cct::xy_to_temp_tint(xy).0;
        let g = (1.0 / t_eff - 1.0 / 6504.0) / (1.0 / 2856.0 - 1.0 / 6504.0);
        let m = e.xyz_to_camera_at(t).unwrap();
        for i in 0..3 {
            for j in 0..3 {
                let want = g * a.0[i][j] + (1.0 - g) * d65.0[i][j];
                assert!((m.0[i][j] - want).abs() < 1e-9);
            }
        }
        assert!(g > 0.2 && g < 0.8, "{g}");
        assert_eq!(e.xyz_to_camera_at(f64::NAN), None);
        assert_eq!(e.xyz_to_camera_at(-5.0), None);
    }

    #[test]
    fn apply_fills_matrices_and_levels_but_keeps_a_files_own_matrix() {
        let e = sample();
        let mut raw = raw_image();
        let applied = e.apply(&mut raw);
        assert_eq!(applied, Applied { matrix: true, levels: true });
        assert_eq!(raw.color.color_matrix, e.color_matrix);
        assert_eq!(raw.black.values, vec![512.0]);
        assert_eq!(raw.white, vec![16000.0]);
        // the white the raw input transform starts from now goes through the entry's interpolated matrices
        let wb = lcc_color::cct::temp_tint_to_xy(4000.0, 0.0);
        let t = lcc_raw::color::camera_transform(&raw, wb);
        assert!(!t.matrix_is_fallback);

        let own = Mat3([[0.5, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.7]]);
        let mut raw = raw_image();
        raw.color.color_matrix = [Some(own), None];
        assert!(!e.apply(&mut raw).matrix);
        assert_eq!(raw.color.color_matrix, [Some(own), None]);
    }

    #[test]
    fn lookup_matches_aliases_and_brand_and_falls_back_to_spectral() {
        let e = sample();
        assert!(e.matches(Some("EXAMPLE"), "EXAMPLE X1"));
        assert!(e.matches(Some("Example Optical"), "x1"));
        assert!(e.matches(None, "X1 Kit"));
        assert!(!e.matches(Some("Other"), "EXAMPLE X1"));
        assert!(!e.matches(Some("Example"), ""));

        let s = find(Some("Canon"), "Canon EOS 600D").unwrap();
        assert_eq!(s.provenance.source, Source::SpectralSensitivities);
        assert!(s.validate().is_ok());
        assert_eq!(find(Some("Nobody"), "Nothing"), None);
        assert!(entries().len() >= lcc_raw::spectral::cameras().len());
        for e in entries() {
            assert!(e.validate().is_ok(), "{} {}", e.make, e.model);
        }
    }
}
