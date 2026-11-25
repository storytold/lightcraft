//! Camera profiling for the app: photograph a ColorChecker, and build a DNG Camera Profile
//! (`.dcp`) for that camera from it.
//!
//! This is the profiling engine of RADcamProfiles (© RAD Films - Roman Alurkoff, MIT; relicensed
//! here as MIT OR Apache-2.0 by its author), embedded without its raw readers and container codecs:
//! the app's own decoders build the [`frame::RawFrame`] the chart is measured on, and the
//! profile is written by [`lcc_raw::dcp::write`]. `NOTICE` and `assets/ATTRIBUTION.md` say what came over and
//! where its data comes from.
//!
//! # Pipeline
//!
//! 1. [`find_chart`] looks for the chart by its colours (or [`refine`] moves hand-placed corners
//!    onto it).
//! 2. [`build`] measures the patches ([`reference::measure_rotated`]), evens out the light on the
//!    chart ([`uneven::even_out`]), fits a forward matrix, a colour matrix and a correction table
//!    ([`profile::fit`]), and expresses them as a DCP
//!    ([`dcp::build`]): `ColorMatrix1`, `ForwardMatrix1` and a `ProfileHueSatMap`.
//! 3. The [`Built`] report says how closely the profile reproduces each patch (CIEDE2000).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
// The numerics below are a port: keep their long-hand style.
// (`!(x > 0.0)` is deliberate: it is also true for NaN.)
#![allow(clippy::neg_cmp_op_on_partial_ord, clippy::explicit_counter_loop)]

pub mod align;
pub mod color;
mod data;
pub mod dcp;
pub mod detect;
pub mod evaluate;
pub mod frame;
pub mod illuminants;
pub mod jpeg_look;
pub mod linalg;
pub mod lut;
pub mod matopt;
pub mod observer;
pub mod profile;
pub mod reference;
pub mod simplex;
pub mod spectrum;
pub mod synthetic;
pub mod target;
pub mod uneven;

pub use frame::RawFrame;
pub use linalg::{Mat3, Vec3};
pub use reference::ChartCorners;

use lcc_raw::dcp::Dcp;

/// Generator name, for profile descriptions.
pub const GENERATOR: &str = "the app camera profiling (RADcamProfiles engine)";

/// How much of each patch's cell is sampled (the rest is margin against misplacement).
pub const SAMPLE_FRACTION: f64 = 0.6;

/// The calibration illuminants a chart photograph can be declared to be lit by.
pub const ILLUMINANTS: &[&str] = &["D50", "D55", "D65", "D75", "A"];

/// Why a profile could not be made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CamprofError {
    /// The chart could not be found or the corners don't lie on it.
    NoChart(String),
    /// An option is out of range (an unknown illuminant, empty name…).
    Options(String),
    /// The measurement could not be fitted.
    Fit(String),
}

impl core::fmt::Display for CamprofError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoChart(m) | Self::Options(m) | Self::Fit(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for CamprofError {}

/// Where the chart lies in a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Located {
    /// Outer corners of the patch grid, in frame pixels, screen order (top left, top right, bottom
    /// right, bottom left).
    pub corners: ChartCorners,
    /// Quarter turns 0 to 3: patch A01 at the top left, top right, bottom right or bottom left.
    pub turns: u8,
    /// How closely the cells' colours follow the chart's (−1 to 1), when found by them.
    pub score: Option<f64>,
    /// Which chart it is ([`reference::CHARTS`]).
    pub chart: &'static reference::Chart,
}

/// The chart assumed when none was found: the ColorChecker 24.
pub fn chart() -> &'static reference::Chart {
    &reference::COLORCHECKER_CLASSIC
}

/// Look for a chart in `frame` by its colours (any position, size and quarter turn): every built-in chart
/// ([`reference::CHARTS`]) is tried and the best match wins.
pub fn find_chart(frame: &RawFrame) -> Option<Located> {
    reference::CHARTS
        .iter()
        .filter_map(|chart| detect::find_by_pattern(frame, chart).map(|found| (found, *chart)))
        .max_by(|a, b| a.0.score.total_cmp(&b.0.score))
        .map(|(found, chart)| Located { corners: found.corners, turns: found.quarter_turns % 4, score: Some(found.score), chart })
}

/// Move corners placed by hand onto the chart (`turns` is kept: the refinement cannot tell a grid
/// from its half turn). `None` when they are too far from a chart.
pub fn refine(frame: &RawFrame, chart: &'static reference::Chart, corners: &ChartCorners, turns: u8) -> Option<Located> {
    if !corners.is_usable() {
        return None;
    }
    let placement = align::align(frame, chart, corners, SAMPLE_FRACTION)?;
    Some(Located { corners: placement.corners, turns: turns % 4, score: None, chart })
}

/// Which way round the chart lies inside corners placed by hand: the quarter turn whose matrix
/// fit is best (the corners' shape only says upright or on its side).
pub fn orient(frame: &RawFrame, chart: &reference::Chart, corners: &ChartCorners) -> u8 {
    let start = corners.plausible_turns(chart)[0];
    let placement = align::Placement { corners: *corners, quarter_turns: start, variation: f64::NAN };
    align::resolve_orientation(frame, chart, &placement, &profile::FitOptions::default(), SAMPLE_FRACTION).placement.quarter_turns % 4
}

/// How to build a profile.
#[derive(Clone, Debug, PartialEq)]
pub struct BuildOptions {
    /// The light the chart was photographed under ([`ILLUMINANTS`]).
    pub illuminant: String,
    /// "Photographic" (the default): colours match the chart, each keeping the lightness the camera
    /// recorded. Off: "Reproduction", everything matches the chart exactly, lightness included.
    pub natural: bool,
    /// Fit a correction table (a `HueSatMap`) on top of the matrix.
    pub correction_table: bool,
    /// `UniqueCameraModel`: the camera, as raw converters name it.
    pub camera_model: String,
    /// `ProfileName`.
    pub name: String,
    /// Even out the light on the chart ([`uneven::even_out`]): divide out the falloff across the
    /// card and subtract the glare on the patches, kept only when the grey ramp ends more linear.
    pub even_light: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self { illuminant: "D55".into(), natural: true, correction_table: true, camera_model: String::new(), name: String::new(), even_light: true }
    }
}

/// One patch of the fit report.
#[derive(Clone, Debug, PartialEq)]
pub struct PatchReport {
    /// `A01`…`D06`.
    pub id: String,
    /// "Dark Skin"…
    pub name: String,
    /// CIEDE2000 through the whole profile.
    pub delta_e: f64,
    /// CIEDE2000 through the matrix alone.
    pub matrix_delta_e: f64,
    /// The chart's value, CIELAB D50.
    pub reference_lab: [f64; 3],
    /// What the profile renders, CIELAB D50.
    pub rendered_lab: [f64; 3],
}

/// A built profile and how well it fits the chart.
#[derive(Clone, Debug)]
pub struct Built {
    /// The DCP.
    pub dcp: Dcp,
    /// Where the chart was measured.
    pub located: Located,
    /// Mean and largest CIEDE2000 over the patches used.
    pub mean_delta_e: f64,
    pub max_delta_e: f64,
    /// Mean CIEDE2000 of the matrix alone.
    pub matrix_mean_delta_e: f64,
    /// Patches measured and used (clipped or too-dark patches are left out).
    pub patches_read: usize,
    pub patches_used: usize,
    /// The grey patches the white balance was taken from.
    pub neutral_ids: Vec<String>,
    /// Per patch.
    pub patches: Vec<PatchReport>,
    /// Notes worth showing.
    pub notes: Vec<String>,
    /// The light it was calibrated for ([`ILLUMINANTS`]).
    pub illuminant: String,
    /// The camera's signal for a neutral surface under that light (largest channel 1).
    pub neutral: [f64; 3],
}

/// Measure the chart at `located` in `frame` and build a profile from it.
pub fn build(frame: &RawFrame, located: &Located, opts: &BuildOptions) -> Result<Built, CamprofError> {
    let chart = located.chart;
    if !located.corners.is_usable() {
        return Err(CamprofError::NoChart("the chart corners don't make a usable grid".into()));
    }
    let illuminant = opts.illuminant.trim();
    if !ILLUMINANTS.iter().any(|i| i.eq_ignore_ascii_case(illuminant)) {
        return Err(CamprofError::Options(format!("unknown illuminant `{illuminant}` (one of {})", ILLUMINANTS.join(", "))));
    }
    let illuminant = illuminant.to_ascii_uppercase();
    if opts.camera_model.trim().is_empty() {
        return Err(CamprofError::Options("the camera's name (UniqueCameraModel) is empty".into()));
    }
    let white = illuminants::white_point_by_name("1931_2", &illuminant).map_err(|e| CamprofError::Options(e.to_string()))?;
    let turns = located.turns % 4;
    let mut measurement = reference::measure_rotated(frame, chart, &located.corners, SAMPLE_FRACTION, turns);
    if measurement.sample_counts.contains(&0) {
        return Err(CamprofError::NoChart("part of the chart grid lies outside the picture".into()));
    }
    let light_notes = if opts.even_light {
        uneven::even_out(frame, &located.corners, uneven::frame_layout(chart.layout, turns), &mut measurement).describe()
    } else {
        Vec::new()
    };
    let patch_set = measurement.to_patch_set(white);
    let fit_options = profile::FitOptions {
        illuminant_name: Some(illuminant.clone()),
        camera_name: Some(opts.camera_model.trim().to_string()),
        make_lut: opts.correction_table,
        lut: profile::LutFitOptions { chart_lightness: if opts.natural { profile::NATURAL } else { profile::ACCURATE }, ..Default::default() },
        ..profile::FitOptions::default()
    };
    let (prof, report) = profile::fit(&patch_set, &fit_options, &mut |_| {}).map_err(|e| CamprofError::Fit(e.to_string()))?;
    let name = if opts.name.trim().is_empty() {
        default_name(opts.camera_model.trim(), &illuminant, opts.natural, "")
    } else {
        opts.name.trim().to_string()
    };
    let dcp = dcp::build(&prof, opts.camera_model.trim(), &name).map_err(|e| CamprofError::Fit(e.to_string()))?;

    let d50 = color::D50;
    let mut patches = Vec::with_capacity(prof.patch_ids.len());
    for (i, id) in prof.patch_ids.iter().enumerate() {
        let (Some(reference), Some(cam)) = (prof.reference_xyz.get(i), prof.camera_rgb.get(i)) else { continue };
        let rendered = prof.render(*cam);
        let name = chart.patches.iter().find(|p| p.id == id.as_str()).map(|p| p.name.to_string()).unwrap_or_else(|| id.clone());
        let lab = |xyz: Vec3| {
            let l = color::xyz_to_lab(xyz, d50);
            [l[0], l[1], l[2]]
        };
        patches.push(PatchReport {
            id: id.clone(),
            name,
            delta_e: prof.lut_patch_errors.get(i).copied().unwrap_or(f64::NAN),
            matrix_delta_e: prof.matrix_patch_errors.get(i).copied().unwrap_or(f64::NAN),
            reference_lab: lab(*reference),
            rendered_lab: lab(rendered),
        });
    }
    let errors: Vec<f64> = patches.iter().map(|p| p.delta_e).filter(|d| d.is_finite()).collect();
    let max_delta_e = errors.iter().copied().fold(0.0, f64::max);
    let mean_delta_e = if errors.is_empty() { report.lut_mean_de } else { errors.iter().sum::<f64>() / errors.len() as f64 };
    Ok(Built {
        dcp,
        located: Located { turns, ..*located },
        mean_delta_e,
        max_delta_e,
        matrix_mean_delta_e: report.matrix_mean_de,
        patches_read: report.patches_read,
        patches_used: report.patches_used,
        neutral_ids: report.neutral_ids,
        patches,
        notes: light_notes.into_iter().chain(report.notes).collect(),
        illuminant,
        neutral: [prof.white_balance[0], prof.white_balance[1], prof.white_balance[2]],
    })
}

/// How an illuminant reads in a profile's name and to people: D55 is "Daylight" (daylight and
/// flash), A (StdA) is "Tungsten"; any other keeps its standard name.
pub fn light_code(illuminant: &str) -> &str {
    match illuminant.trim().to_ascii_uppercase().as_str() {
        "A" | "STDA" => "Tungsten",
        "D55" => "Daylight",
        _ => illuminant.trim(),
    }
}

/// A profile's name when none is given: `<camera> · <light(s)> · <Photo | Repro> · <date>`, e.g.
/// `Canon EOS R5 C 71 · Daylight · Photo · 2026-10-13` or `Sony ILCE-9 SA · Tungsten + Daylight · Repro · 2026-10-13`
/// (Photo: Photographic, Repro: Reproduction; see [`BuildOptions::natural`]). `lights` is one
/// illuminant or `low + high`; an empty `date` is left out.
pub fn default_name(camera_model: &str, lights: &str, photographic: bool, date: &str) -> String {
    let lights = lights.split('+').map(light_code).collect::<Vec<_>>().join(" + ");
    let mut parts = vec![camera_model.trim().to_string(), lights, if photographic { "Photo".into() } else { "Repro".into() }];
    if !date.trim().is_empty() {
        parts.push(date.trim().to_string());
    }
    parts.retain(|p| !p.is_empty());
    parts.join(" · ")
}

/// A file name stem for a profile named `name`: ASCII letters, digits and `-`, everything else
/// (spaces, dots, `·`, `+`, characters some system forbids) one `_`; at most 100 characters; never
/// empty or a name Windows reserves.
pub fn file_stem(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let mut out: String = out.trim_matches(|c| c == '_' || c == '-').chars().take(100).collect();
    while out.ends_with('_') || out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out = "Camera_Profile".into();
    }
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5",
        "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&out.to_ascii_uppercase().as_str()) {
        out.insert(0, '_');
    }
    out
}

/// Correlated colour temperature (K) of a calibration illuminant ([`ILLUMINANTS`]).
pub fn illuminant_cct(name: &str) -> Option<f64> {
    Some(match name.trim().to_ascii_uppercase().as_str() {
        "A" | "STDA" => 2856.0,
        "D50" => 5003.0,
        "D55" => 5503.0,
        "D65" => 6504.0,
        "D75" => 7504.0,
        _ => return None,
    })
}

/// The calibration illuminant ([`ILLUMINANTS`]) nearest a colour temperature, in mired.
pub fn nearest_illuminant(cct: f64) -> &'static str {
    if !cct.is_finite() || cct <= 0.0 {
        return "D55";
    }
    let off = |name: &str| (1.0e6 / illuminant_cct(name).unwrap_or(5503.0) - 1.0e6 / cct).abs();
    ILLUMINANTS.iter().copied().min_by(|a, b| off(a).total_cmp(&off(b))).unwrap_or("D55")
}

/// Two chart photos must be lit at least this far apart (in mired, 10⁶ / K) for a dual-illuminant
/// profile: D50 and D65 are 46 apart, D65 and D75 21, StdA and D65 196.
pub const MIN_MIRED_APART: f64 = 25.0;

/// One chart photo of a dual-illuminant profile.
#[derive(Clone, Copy, Debug)]
pub struct Shot<'a> {
    pub frame: &'a RawFrame,
    pub located: Located,
    /// The light it was taken under ([`ILLUMINANTS`]).
    pub illuminant: &'a str,
}

/// A dual-illuminant profile: calibration 1 (the lower colour temperature) and 2.
#[derive(Clone, Debug)]
pub struct BuiltDual {
    /// The DCP, with `CalibrationIlluminant1/2`, `ColorMatrix1/2`, `ForwardMatrix1/2` and
    /// `ProfileHueSatMapData1/2`.
    pub dcp: Dcp,
    /// Each light's own fit and report, the lower colour temperature first.
    pub calibrations: [Built; 2],
}

/// Build a dual-illuminant profile from two chart photos under different lights (typically one
/// daylight, one tungsten). Each is fitted on its own; the raw converter interpolates between the
/// two by the photo's white balance (DNG 6.3, inverse colour temperature). Two photos under
/// effectively the same light are refused rather than made into a profile with two copies of one
/// calibration.
pub fn build_dual(a: &Shot<'_>, b: &Shot<'_>, opts: &BuildOptions) -> Result<BuiltDual, CamprofError> {
    let (Some(ta), Some(tb)) = (illuminant_cct(a.illuminant), illuminant_cct(b.illuminant)) else {
        return Err(CamprofError::Options(format!("unknown illuminant (one of {})", ILLUMINANTS.join(", "))));
    };
    let apart = (1.0e6 / ta - 1.0e6 / tb).abs();
    if apart < MIN_MIRED_APART {
        return Err(CamprofError::Options(format!(
            "the two chart photos are under effectively the same light ({} and {}, {apart:.0} mired apart; at least {MIN_MIRED_APART:.0} needed): set the second to another light, such as Tungsten (A)",
            light_code(a.illuminant),
            light_code(b.illuminant)
        )));
    }
    let one = |shot: &Shot<'_>| build(shot.frame, &shot.located, &BuildOptions { illuminant: shot.illuminant.to_string(), ..opts.clone() });
    let (first, second) = (one(a)?, one(b)?);
    // the camera itself must have seen two different lights, whatever they were declared as
    let ratio = |n: &[f64; 3]| [n[0] / n[1].max(1e-9), n[2] / n[1].max(1e-9)];
    let (ra, rb) = (ratio(&first.neutral), ratio(&second.neutral));
    if (0..2).all(|i| (ra[i] / rb[i].max(1e-9) - 1.0).abs() < 0.03) {
        return Err(CamprofError::Options(
            "the greys of the two chart photos read the same to the camera: they were taken under the same light, whatever it is called".into(),
        ));
    }
    let (low, high) = if ta <= tb { (first, second) } else { (second, first) };
    let name = if opts.name.trim().is_empty() {
        default_name(opts.camera_model.trim(), &format!("{} + {}", low.illuminant, high.illuminant), opts.natural, "")
    } else {
        opts.name.trim().to_string()
    };
    let (c1, c2) = (&low.dcp.color, &high.dcp.color);
    let identity = |like: &lcc_raw::profile::HsvTable| lcc_raw::profile::HsvTable { data: vec![[0.0, 1.0, 1.0]; like.data.len()], ..like.clone() };
    // the two maps share their grid and encoding, so the renderer can blend them; a light whose fit
    // left no table gets an identity map of the same shape
    let maps = match (&c1.profile.hue_sat_map[0], &c2.profile.hue_sat_map[0]) {
        (Some(m1), Some(m2)) => [Some(m1.clone()), Some(m2.clone())],
        (Some(m1), None) => [Some(m1.clone()), Some(identity(m1))],
        (None, Some(m2)) => [Some(identity(m2)), Some(m2.clone())],
        (None, None) => [None, None],
    };
    let mut color = c1.clone();
    color.illuminant = [c1.illuminant[0], c2.illuminant[0]];
    color.color_matrix = [c1.color_matrix[0], c2.color_matrix[0]];
    color.forward_matrix = [c1.forward_matrix[0], c2.forward_matrix[0]];
    color.profile.hue_sat_map = maps;
    let dcp = Dcp { name, model: low.dcp.model.clone(), color, copyright: String::new() };
    Ok(BuiltDual { dcp, calibrations: [low, high] })
}

/// The camera name raw converters match a DCP's `UniqueCameraModel` against, the way Adobe names
/// cameras: the model alone when it already starts with the maker (`Canon EOS R5 C`), otherwise the
/// maker's first word in title case, then the model (`Sony ILCE-9`). A DNG's own
/// `UniqueCameraModel`, when the caller has it, takes precedence over this.
pub fn camera_name(make: &str, model: &str) -> String {
    let (make, model) = (make.trim(), model.trim());
    if make.is_empty() {
        return model.to_string();
    }
    let first_word = make.split_whitespace().next().unwrap_or(make);
    if model.to_ascii_lowercase().starts_with(&first_word.to_ascii_lowercase()) {
        return model.to_string();
    }
    let mut maker = String::new();
    for (i, c) in first_word.chars().enumerate() {
        if i == 0 {
            maker.extend(c.to_uppercase());
        } else {
            maker.extend(c.to_lowercase());
        }
    }
    if model.is_empty() { maker } else { format!("{maker} {model}") }
}

#[cfg(test)]
mod tests {
    #[test]
    fn camera_names_follow_the_makers_spelling() {
        assert_eq!(super::camera_name("SONY", "ILCE-9"), "Sony ILCE-9");
        assert_eq!(super::camera_name("Canon", "Canon EOS R5 C"), "Canon EOS R5 C");
        assert_eq!(super::camera_name("NIKON CORPORATION", "NIKON Z 8"), "NIKON Z 8");
        assert_eq!(super::camera_name("", " X100V "), "X100V");
        assert_eq!(super::camera_name("FUJIFILM", ""), "Fujifilm");
    }

    #[test]
    fn default_names_and_file_names() {
        use super::{default_name, file_stem};
        let single = default_name("Canon EOS R5 C 71", "D55", true, "2026-10-13");
        assert_eq!(single, "Canon EOS R5 C 71 · Daylight · Photo · 2026-10-13");
        assert_eq!(file_stem(&single), "Canon_EOS_R5_C_71_Daylight_Photo_2026-10-13");
        let dual = default_name("Sony ILCE-9 SA", "A + D55", false, "2026-10-13");
        assert_eq!(dual, "Sony ILCE-9 SA · Tungsten + Daylight · Repro · 2026-10-13");
        assert_eq!(file_stem(&dual), "Sony_ILCE-9_SA_Tungsten_Daylight_Repro_2026-10-13");
        assert_eq!(default_name("X", "A + D65", true, ""), "X · Tungsten + D65 · Photo");
        assert_eq!(default_name("X", "D65", true, ""), "X · D65 · Photo");
        // awkward camera names
        assert_eq!(file_stem("NIKON Z 8/9: \"pro\" <cam>*?|"), "NIKON_Z_8_9_pro_cam");
        assert_eq!(file_stem("Leica M11 Monochrom · D55 · HSL"), "Leica_M11_Monochrom_D55_HSL");
        assert_eq!(file_stem("Пентакс К-1 · D55"), "1_D55");
        assert_eq!(file_stem("··· ..."), "Camera_Profile");
        assert_eq!(file_stem("con"), "_con");
        assert_eq!(file_stem(&"x".repeat(500)).len(), 100);
    }
}
