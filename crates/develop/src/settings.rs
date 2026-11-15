//! The develop settings schema. Every field has a neutral default; serde uses `#[serde(default)]` so
//! older/newer files load (unknown fields are ignored, missing fields take defaults). The one
//! exception is `process`: missing (or unreadable), it is V1, the rendering those older files were
//! made with.

use lcc_geom::{CropGeometry, Homography, Orientation, Point};
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

// Most items each list of the settings holds (issue #2121). Every one costs per-pixel work or a
// full-size plane when a photo renders (a mask keeps a plane; each brush stroke blends one; each
// colour sample, spot and red eye is visited for every pixel), and settings arrive as JSON from
// sidecars, presets, `develop.merge`, `render --settings` and the catalog: 2,000 tiny masks in
// 200 KB of JSON asked 128 GB at 4096². Settings past a limit keep their first items
// ([`capped`] while reading, [`DevelopSettings::cap_lists`] for settings built in code); the
// limits are far past what an edit uses.
/// Masks a photo has.
pub const MAX_MASKS: usize = 100;
/// Components (shapes added, subtracted or intersected) of one mask.
pub const MAX_MASK_COMPONENTS: usize = 64;
/// Colours sampled by one Color Range component (Lightroom's and the Masking panel's limit).
pub const MAX_COLOR_SAMPLES: usize = 5;
/// Brush strokes of one brush component.
pub const MAX_STROKES: usize = 2_000;
/// Points (and pressure values) of one brush stroke.
pub const MAX_STROKE_POINTS: usize = 100_000;
/// Include or exclude clicks of one Object component.
pub const MAX_PROMPT_POINTS: usize = 256;
/// Body parts named by one People component.
pub const MAX_PEOPLE_PARTS: usize = 32;
/// Spot removals, heals and clones of a photo.
pub const MAX_SPOTS: usize = 1_000;
/// Points of one brushed spot.
pub const MAX_SPOT_POINTS: usize = 10_000;
/// Red eye / pet eye corrections of a photo.
pub const MAX_RED_EYES: usize = 64;
/// Points of each point curve.
pub const MAX_CURVE_POINTS: usize = 256;
/// Guided Upright guides (the tool keeps 4).
pub const MAX_GUIDES: usize = 16;
/// Switched-off Edit panel sections.
pub const MAX_DISABLED_SECTIONS: usize = 64;

/// Read a list keeping its first `N` items: the rest are skipped without being built, so a huge
/// list in a file costs neither memory nor time past `N` (issue #2121).
pub(crate) fn capped<'de, const N: usize, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Keep<T, const N: usize>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const N: usize> serde::de::Visitor<'de> for Keep<T, N> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a list")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<T>, A::Error> {
            let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(N).min(1024));
            while out.len() < N {
                match seq.next_element()? {
                    Some(x) => out.push(x),
                    None => return Ok(out),
                }
            }
            while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
            Ok(out)
        }
    }
    d.deserialize_seq(Keep::<T, N>(std::marker::PhantomData))
}

impl DevelopSettings {
    /// Some list holds more than its limit (`MAX_MASKS` and the rest): settings built in code, or
    /// stored before the limits existed. The same lists as [`DevelopSettings::cap_lists`].
    pub fn over_list_limits(&self) -> bool {
        let over = |n: usize, max: usize| n > max;
        let c = &self.curve;
        over(self.point_colors.len(), MAX_POINT_COLORS)
            || over(self.masks.len(), MAX_MASKS)
            || over(self.spots.len(), MAX_SPOTS)
            || over(self.red_eye.len(), MAX_RED_EYES)
            || over(self.disabled_sections.len(), MAX_DISABLED_SECTIONS)
            || over(self.geometry.guides.len(), MAX_GUIDES)
            || [&c.master, &c.red, &c.green, &c.blue].into_iter().any(|v| over(v.len(), MAX_CURVE_POINTS))
            || self.spots.iter().any(|sp| over(sp.points.len(), MAX_SPOT_POINTS))
            || self.masks.iter().any(|m| {
                over(m.components.len(), MAX_MASK_COMPONENTS)
                    || over(m.point_colors.len(), MAX_POINT_COLORS)
                    || m.components.iter().any(|comp| match &comp.shape {
                        MaskShape::Brush { strokes } => {
                            over(strokes.len(), MAX_STROKES)
                                || strokes.iter().any(|st| over(st.points.len(), MAX_STROKE_POINTS) || over(st.pressure.len(), MAX_STROKE_POINTS))
                        }
                        MaskShape::ColorRange { samples, .. } => over(samples.len(), MAX_COLOR_SAMPLES),
                        MaskShape::Object { hint, exclude, .. } => over(hint.len(), MAX_PROMPT_POINTS) || over(exclude.len(), MAX_PROMPT_POINTS),
                        MaskShape::People { parts, .. } => over(parts.len(), MAX_PEOPLE_PARTS),
                        _ => false,
                    })
            })
    }

    /// Cut every list to its limit, keeping its first items.
    pub fn cap_lists(&mut self) {
        self.point_colors.truncate(MAX_POINT_COLORS);
        self.masks.truncate(MAX_MASKS);
        self.spots.truncate(MAX_SPOTS);
        self.red_eye.truncate(MAX_RED_EYES);
        self.disabled_sections.truncate(MAX_DISABLED_SECTIONS);
        self.geometry.guides.truncate(MAX_GUIDES);
        let c = &mut self.curve;
        for v in [&mut c.master, &mut c.red, &mut c.green, &mut c.blue] {
            v.truncate(MAX_CURVE_POINTS);
        }
        for sp in &mut self.spots {
            sp.points.truncate(MAX_SPOT_POINTS);
        }
        for m in &mut self.masks {
            m.components.truncate(MAX_MASK_COMPONENTS);
            m.point_colors.truncate(MAX_POINT_COLORS);
            for comp in &mut m.components {
                match &mut comp.shape {
                    MaskShape::Brush { strokes } => {
                        strokes.truncate(MAX_STROKES);
                        for st in strokes {
                            st.points.truncate(MAX_STROKE_POINTS);
                            st.pressure.truncate(MAX_STROKE_POINTS);
                        }
                    }
                    MaskShape::ColorRange { samples, areas, .. } => {
                        samples.truncate(MAX_COLOR_SAMPLES);
                        areas.truncate(MAX_COLOR_SAMPLES);
                    }
                    MaskShape::Object { hint, exclude, .. } => {
                        hint.truncate(MAX_PROMPT_POINTS);
                        exclude.truncate(MAX_PROMPT_POINTS);
                    }
                    MaskShape::People { parts, .. } => parts.truncate(MAX_PEOPLE_PARTS),
                    _ => {}
                }
            }
        }
    }
}

/// The rendering process a photo's settings are interpreted by (what Lightroom calls the process
/// version). A change that would alter how existing settings render ships as a new process, so a
/// photo keeps its look until someone updates it (`develop.updateProcess`). See
/// `docs/process-versions.md`.
///
/// Stored as a plain number: one written by a newer version of the app loads, is kept as it is, and
/// renders with the newest process this build knows ([`ProcessVersion::process`]). Reading is
/// lenient: a whole number from 0 to 2^32 - 1 (also written as `2.0`) is that process; anything
/// else (negative, fractional, too large, text, null, a list…) reads as V1, like a missing field,
/// so a damaged value never costs the rest of the settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ProcessVersion(pub u32);

impl<'de> Deserialize<'de> for ProcessVersion {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Whole(u64),
            Number(f64),
            Other(serde::de::IgnoredAny),
        }
        Ok(match Wire::deserialize(d)? {
            Wire::Whole(n) => u32::try_from(n).map_or_else(|_| ProcessVersion::legacy(), ProcessVersion),
            Wire::Number(x) if x.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&x) => ProcessVersion(x as u32),
            Wire::Number(_) | Wire::Other(_) => ProcessVersion::legacy(),
        })
    }
}

impl ProcessVersion {
    /// The first process: the app's rendering from before process versions existed. Frozen.
    pub const V1: ProcessVersion = Process::V1.version();
    /// The process new photos and Reset get.
    pub const LATEST: ProcessVersion = Process::LATEST.version();

    /// The process of settings saved without one (catalogs, snapshots, XMP sidecars and browser
    /// libraries written before process versions existed): V1, whatever [`Self::LATEST`] becomes.
    pub fn legacy() -> ProcessVersion {
        ProcessVersion::V1
    }

    /// Saved settings leave the field out for V1, so files and preview-cache keys of settings made
    /// before process versions existed stay byte for byte what they were.
    pub fn is_legacy(&self) -> bool {
        *self == ProcessVersion::legacy()
    }

    /// The process this build renders with: the newest one it knows that isn't newer than this
    /// number (a number from a newer version of the app renders with [`Process::LATEST`]; one below V1,
    /// which no version of the app writes, with V1).
    pub fn process(self) -> Process {
        Process::ALL.into_iter().rev().find(|p| p.version() <= self).unwrap_or(Process::V1)
    }

    /// A process this build implements.
    pub fn is_known(self) -> bool {
        Process::ALL.iter().any(|p| p.version() == self)
    }

    /// Older than [`Self::LATEST`]: Update to Current Process applies.
    pub fn is_outdated(self) -> bool {
        self < ProcessVersion::LATEST
    }
}

/// A rendering process this build implements. Matches on it are exhaustive, so adding a process
/// makes the compiler point at every stage whose behaviour depends on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Process {
    /// The rendering of the app before process versions existed (2026-10).
    V1,
    /// Highlights and Shadows keep the local contrast of the tones they move: their edge-aware base
    /// is taken of a fine median of log luminance (issue #632).
    V2,
    /// Dehaze estimates haze from a patch-minimum dark channel, eases in, and never pushes a
    /// pixel below a tenth of its darkest channel (issue #1237).
    V3,
    /// Sharpening is an unsharp mask at the Radius (in source pixels) with Detail soft-clipping its
    /// halos, instead of a boost of the Texture band (upstream #757 / #762).
    V4,
    /// Colours brighter than the output can show keep their hue and saturation: the brightest
    /// channel is limited to white and the others keep their places between it and the darkest,
    /// instead of the colour moving toward its luminance (which turned bright saturated reds pink).
    /// Adapted from upstream #2262.
    V5,
    /// AI Denoise's amount is eased (`1 − (1 − a)³`, [`denoise_weight`]) instead of a straight mix, so
    /// half-way already removes most of the noise, as Lightroom's slider does (fork issue: grainy at 50).
    V6,
    /// Film grain as clumps of irregular particles (a filtered Boolean model, resolution independent),
    /// with Size Variation, Clumping, Color and Response, calibrated against Lightroom Classic exports
    /// (OneSlips); V1–V6 keep their value-noise grain.
    V7,
    /// 8-bit output is dithered with an 8×8 ordered (Bayer) pattern before it is rounded, so smooth
    /// gradients (skies, vignettes) show no banding; black and white stay exact. Adapted from
    /// upstream #3509. Colour noise reduction filters the linear colour differences R−Y and B−Y
    /// jointly, leaving Y alone, with edge-aware weights from a denoised guide and a brake for
    /// isolated colour specks (adapted from upstream #1348).
    V8,
    /// Calibration's primary Saturation scales a primary's distance from grey in linear light
    /// instead of its OkLab chroma, so it no longer turns blues in hue like the Hue slider
    /// (fork issue #1031).
    V9,
}

/// How much of the AI-denoised picture to mix in for a Denoise slider fraction `a` (0 to 1) under
/// `process`: `a` itself up to V5; from V6 `1 − (1 − a)³`, which leaves an eighth of the noise at 50
/// (a straight mix leaves half) and still rises over the whole slider. Not-a-number gives 0.
pub fn denoise_weight(process: Process, a: f32) -> f32 {
    let a = if a.is_finite() { a.clamp(0.0, 1.0) } else { 0.0 };
    match process {
        Process::V1 | Process::V2 | Process::V3 | Process::V4 | Process::V5 => a,
        Process::V6 | Process::V7 | Process::V8 | Process::V9 => 1.0 - (1.0 - a).powi(3),
    }
}

impl Process {
    /// Every process, oldest first.
    pub const ALL: [Process; 9] =
        [Process::V1, Process::V2, Process::V3, Process::V4, Process::V5, Process::V6, Process::V7, Process::V8, Process::V9];
    pub const LATEST: Process = Process::V9;

    pub const fn version(self) -> ProcessVersion {
        match self {
            Process::V1 => ProcessVersion(1),
            Process::V2 => ProcessVersion(2),
            Process::V3 => ProcessVersion(3),
            Process::V4 => ProcessVersion(4),
            Process::V5 => ProcessVersion(5),
            Process::V6 => ProcessVersion(6),
            Process::V7 => ProcessVersion(7),
            Process::V8 => ProcessVersion(8),
            Process::V9 => ProcessVersion(9),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DevelopSettings {
    /// Schema version of this structure (not the rendering process, see `process`).
    pub version: u32,
    /// The rendering process ([`ProcessVersion`]): the latest for new settings, V1 for settings
    /// saved without the field (the field-level default: not the struct's `Default`) or with a
    /// value that isn't a process number.
    #[serde(default = "ProcessVersion::legacy", skip_serializing_if = "ProcessVersion::is_legacy")]
    pub process: ProcessVersion,
    pub profile: Profile,
    pub treatment: Treatment,
    pub wb: WhiteBalance,
    pub light: Light,
    /// HDR editing: highlights above SDR white, and the SDR rendition derived from them. Not written while it is
    /// the default, so settings without HDR keep their JSON and `hash64` (and every cached preview stays valid).
    #[serde(skip_serializing_if = "Hdr::is_default")]
    pub hdr: Hdr,
    pub curve: ToneCurve,
    pub color: ColorAdj,
    pub mixer: Mixer,
    /// Point Color: up to [`MAX_POINT_COLORS`] sampled colours, each with its own adjustment.
    #[serde(deserialize_with = "capped::<MAX_POINT_COLORS, _, _>")]
    pub point_colors: Vec<PointColor>,
    pub bw_mix: BwMix,
    pub grading: ColorGrading,
    /// The colourist primaries (the Witchcraft grading mode): lift, gamma, gain and offset wheels
    /// with the Log variant's range boundaries. Not written while it is the default, so settings
    /// without them keep their JSON and `hash64` (and every cached preview stays valid).
    #[serde(skip_serializing_if = "Primaries::is_default")]
    pub primaries: Primaries,
    /// Resolve-style HSL curves (the Witchcraft mode's Curves): free points over the hue circle,
    /// one curve each for hue, saturation and luminance. Not written while neutral.
    #[serde(default, skip_serializing_if = "HslCurves::is_neutral")]
    pub hsl_curves: HslCurves,
    pub effects: Effects,
    pub vignette: Vignette,
    pub grain: Grain,
    pub detail: Detail,
    pub optics: Optics,
    pub geometry: Geometry,
    pub crop: Crop,
    pub orientation: Orientation,
    #[serde(deserialize_with = "capped::<MAX_MASKS, _, _>")]
    pub masks: Vec<Mask>,
    #[serde(deserialize_with = "capped::<MAX_SPOTS, _, _>")]
    pub spots: Vec<Spot>,
    #[serde(deserialize_with = "capped::<MAX_RED_EYES, _, _>")]
    pub red_eye: Vec<RedEye>,
    pub lens_blur: LensBlur,
    pub enhance: Enhance,
    /// Camera calibration: shadows tint and primary hue/saturation (applied before tone mapping).
    pub calibration: Calibration,
    /// Smooth Skin (beyond Lightroom). Not written while it is the default, so settings without it
    /// keep their JSON and `hash64`.
    #[serde(skip_serializing_if = "SkinSmoothing::is_default", deserialize_with = "lenient")]
    pub skin: SkinSmoothing,
    /// The skin and faces the models found for Smooth Skin on this photo (its own data: in no
    /// settings group, so copy, paste, sync and presets never carry it).
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub skin_analysis: Option<SkinAnalysis>,
    /// The Retouch sections beyond Smooth Skin (Teeth). Not written while off, so settings without
    /// it keep their JSON and `hash64`.
    #[serde(skip_serializing_if = "crate::Retouch::is_default", deserialize_with = "lenient")]
    pub retouch: crate::Retouch,
    /// The parts of people the models found for them on this photo (its own data, like
    /// `skin_analysis`: in no settings group).
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub retouch_analysis: Option<crate::RetouchAnalysis>,
    /// Sensor repairs made on the raw data before it is developed (hot, dead and stuck pixels). Not written while
    /// it is the default (on), so settings from before it existed keep their JSON and hash.
    #[serde(skip_serializing_if = "Sensor::is_default")]
    pub sensor: Sensor,
    /// Film dust removal from a scanner's infrared pass (dust, hairs and scratches filled in before the picture is
    /// converted). Not written while it is the default (off), so settings from before it existed keep their JSON and hash.
    #[serde(default, skip_serializing_if = "FilmDust::is_default")]
    pub film_dust: FilmDust,
    /// Section on/off toggles (the "eye" buttons on panel headers): section id → enabled.
    #[serde(deserialize_with = "capped::<MAX_DISABLED_SECTIONS, _, _>")]
    pub disabled_sections: Vec<String>,
    /// Pixel-filter plug-ins applied in order (section id `pluginFilters` for the eye toggle). Not written while
    /// empty, so settings saved before plug-in filters keep their JSON and `hash64`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugin_filters: Vec<PluginFilter>,
}

/// Section id of the pixel-filter plug-in stack (its eye toggle in `disabled_sections`).
pub const PLUGIN_FILTERS_SECTION: &str = "pluginFilters";

/// Most pixel-filter plug-ins one photo's stack holds (input-derived lengths are capped).
pub const MAX_PLUGIN_FILTERS: usize = 32;

/// Where in the pipeline a pixel-filter plug-in runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterStage {
    /// On the decoded source, before white balance and tone (e.g. negative conversion).
    Source,
    /// On the display-referred result, after the develop adjustments.
    #[default]
    Display,
}

/// One pixel-filter plug-in in a photo's stack.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginFilter {
    /// Stable per photo: how the UI and commands name this entry.
    pub uid: u32,
    /// Plug-in id from its manifest.
    pub plugin: String,
    /// Hash of the wasm module the parameters were made for.
    pub module_hash: String,
    /// Display label cached from the manifest.
    pub name: String,
    /// Parameter values by manifest key.
    pub params: serde_json::Map<String, serde_json::Value>,
    pub enabled: bool,
    pub stage: FilterStage,
    /// Whole-image measurement the filter computed once (e.g. film base / black / white points).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analysis: Option<serde_json::Value>,
}

impl Default for PluginFilter {
    fn default() -> Self {
        Self {
            uid: 0,
            plugin: String::new(),
            module_hash: String::new(),
            name: String::new(),
            params: serde_json::Map::new(),
            enabled: true,
            stage: FilterStage::Display,
            analysis: None,
        }
    }
}

impl Default for DevelopSettings {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            process: ProcessVersion::LATEST,
            profile: Profile::default(),
            treatment: Treatment::Color,
            wb: WhiteBalance::default(),
            light: Light::default(),
            hdr: Hdr::default(),
            curve: ToneCurve::default(),
            color: ColorAdj::default(),
            mixer: Mixer::default(),
            point_colors: Vec::new(),
            bw_mix: BwMix::default(),
            grading: ColorGrading::default(),
            primaries: Primaries::default(),
            hsl_curves: HslCurves::default(),
            effects: Effects::default(),
            vignette: Vignette::default(),
            grain: Grain::default(),
            detail: Detail::default(),
            optics: Optics::default(),
            geometry: Geometry::default(),
            crop: Crop::default(),
            orientation: Orientation::Normal,
            masks: Vec::new(),
            spots: Vec::new(),
            red_eye: Vec::new(),
            lens_blur: LensBlur::default(),
            enhance: Enhance::default(),
            calibration: Calibration::default(),
            skin: SkinSmoothing::default(),
            skin_analysis: None,
            retouch: crate::Retouch::default(),
            retouch_analysis: None,
            sensor: Sensor::default(),
            film_dust: FilmDust::default(),
            disabled_sections: Vec::new(),
            plugin_filters: Vec::new(),
        }
    }
}

/// Sensor repairs on a raw photo's data, before demosaic (`lcc_raw::defects`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sensor {
    /// Fix hot, dead and stuck pixels: 1 = on (the default: the camera body's known defects and the clear ones this
    /// photo shows), 0 = off (the data as the camera recorded it). A number so it is a develop control
    /// (`sensor.pixelFix`).
    pub pixel_fix: f64,
}

impl Default for Sensor {
    fn default() -> Self {
        Self { pixel_fix: 1.0 }
    }
}

/// Film dust removal from a film scanner's infrared pass (the pass is dark under dust, hairs and
/// scratches, which block infrared while the dyes pass it): the marked pixels are filled in from
/// their surroundings at the Source stage, before plug-in filters and the develop adjustments, as
/// a scanner's own infrared cleaning does. Nothing happens on a photo without the pass.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilmDust {
    /// 1 = on, 0 = off (a number so it is a develop control, `filmDust.remove`).
    pub remove: f64,
    /// How far below its local level the pass must fall to count, 0..100 (`filmDust.sensitivity`).
    pub sensitivity: f64,
    /// Pixels each mark is grown (shrunk when negative) at a 2400 px long edge, −8..40 (`filmDust.expand`).
    pub expand: f64,
}

impl Default for FilmDust {
    fn default() -> Self {
        Self { remove: 0.0, sensitivity: 50.0, expand: 1.0 }
    }
}

impl FilmDust {
    pub fn is_default(&self) -> bool {
        *self == FilmDust::default()
    }

    /// Whether the marks are filled in.
    pub fn on(&self) -> bool {
        self.remove >= 0.5
    }
}

impl Sensor {
    pub fn is_default(&self) -> bool {
        *self == Sensor::default()
    }

    /// Whether hot, dead and stuck pixels are fixed.
    pub fn fixes_pixels(&self) -> bool {
        self.pixel_fix >= 0.5 || self.pixel_fix.is_nan()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Treatment {
    #[default]
    Color,
    Bw,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    /// Profile id from the profile registry (e.g. `lc.color`, `lc.vivid`, `lc.mono`).
    pub id: String,
    /// Creative profile amount 0..200 (%).
    pub amount: f64,
    /// Render the raw's own local tone mapping (DNG `ProfileGainTableMap`, Apple ProRAW), as the
    /// camera does. Off: rendered as Lightroom Classic does, without it. Omitted when off, so
    /// settings without it keep their JSON (and hash).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub camera_local_tone: bool,
    /// Where a raw without colour matrices of its own takes its starting colour from
    /// (`docs/camera-preview-colour.md`). Omitted when [`CameraColour::Auto`], so settings without
    /// it keep their JSON (and hash).
    #[serde(skip_serializing_if = "CameraColour::is_auto")]
    pub camera_colour: CameraColour,
    /// How a raw's colour model is chosen: empty for its camera's default, `lc` for the app's own
    /// colour, `dcp:<id>` for an imported `.dcp` camera profile. Omitted when empty, so settings
    /// without it keep their JSON (and hash).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub camera: String,
}

impl Default for Profile {
    fn default() -> Self {
        Self { id: "lc.color".into(), amount: 100.0, camera_local_tone: false, camera_colour: CameraColour::Auto, camera: String::new() }
    }
}

/// The starting colour of a raw that carries no colour matrices (NEF, ARW, CR3, …).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CameraColour {
    /// The first available of: a camera profile with tone fitted to the file's JPEG, a look fitted
    /// to the file's JPEG, the model's spectral matrices, the neutral fallback.
    #[default]
    Auto,
    /// The model's matrices from measured spectral sensitivities with the app's default tone
    /// curve, never the embedded JPEG; [`CameraColour::Auto`] for a model without them.
    Colorimetric,
}

impl CameraColour {
    pub fn is_auto(&self) -> bool {
        *self == CameraColour::Auto
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WbMode {
    #[default]
    AsShot,
    Auto,
    Daylight,
    Cloudy,
    Shade,
    Tungsten,
    Fluorescent,
    Flash,
    Custom,
}

impl WbMode {
    pub const ALL: [WbMode; 9] = [
        WbMode::AsShot,
        WbMode::Auto,
        WbMode::Daylight,
        WbMode::Cloudy,
        WbMode::Shade,
        WbMode::Tungsten,
        WbMode::Fluorescent,
        WbMode::Flash,
        WbMode::Custom,
    ];
    pub fn label(self) -> &'static str {
        match self {
            WbMode::AsShot => "As Shot",
            WbMode::Auto => "Auto",
            WbMode::Daylight => "Daylight",
            WbMode::Cloudy => "Cloudy",
            WbMode::Shade => "Shade",
            WbMode::Tungsten => "Tungsten",
            WbMode::Fluorescent => "Fluorescent",
            WbMode::Flash => "Flash",
            WbMode::Custom => "Custom",
        }
    }
    /// Preset temperature/tint for raw files with a Kelvin scale (K, tint): the CIE daylight series D55 / D65 /
    /// D75 (5503, 6504, 7504 K; CIE 15:2004) for Daylight / Cloudy / Shade, CIE Illuminant A (2856 K) for
    /// Tungsten, a typical cool-white fluorescent (CIE F2, 4230 K, is the lamp; 3800 K with a magenta correction
    /// is what a camera's preset neutralises) and a flash at daylight; the tints are the small magenta
    /// corrections daylight-series whites need above the Planckian locus, as Lightroom's presets of the same
    /// names show them in its panel (observed values, no Adobe data). On the relative scale (rendered files, raws
    /// without matrices) `develop.wb` scales them around 6500 K = as shot.
    pub fn preset(self) -> Option<(f64, f64)> {
        match self {
            WbMode::Daylight => Some((5500.0, 10.0)),
            WbMode::Cloudy => Some((6500.0, 10.0)),
            WbMode::Shade => Some((7500.0, 10.0)),
            WbMode::Tungsten => Some((2850.0, 0.0)),
            WbMode::Fluorescent => Some((3800.0, 21.0)),
            WbMode::Flash => Some((5500.0, 0.0)),
            _ => None,
        }
    }
}

/// Which scale a [`WhiteBalance`]'s `temp` / `tint` were written on.
///
/// Raws whose camera has colour matrices (in the file, or measured for the model) edit white balance in Kelvin;
/// before that, raws without matrices in the file were all edited relative to their as-shot look, where 6500 K / 0
/// means *as shot*. Settings written then carry no scale and keep that meaning (`Legacy`): the pipeline turns them
/// into the same colour change on the Kelvin scale (`lcc_pipeline::local::legacy_to_kelvin`), so no photo's
/// look changes. Settings written on the Kelvin scale say so (`Kelvin`). The scale is irrelevant on files the
/// Kelvin scale does not reach (rendered photos, raws without matrices): their values keep the relative meaning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WbScale {
    /// Written before the scale existed, or by a preset / partial JSON without one: relative (6500 K / 0 = as shot)
    /// on a raw developed from a fitted look, Kelvin on a raw whose file carries matrices. Not written.
    #[default]
    Legacy,
    /// Absolute Kelvin and tint, as Lightroom writes them for raws.
    Kelvin,
}

impl WbScale {
    fn is_legacy(&self) -> bool {
        *self == WbScale::Legacy
    }
}

/// White balance. For raw files with colour matrices `temp` is the scene illuminant in Kelvin (`scale` says the
/// values were written on that scale); for rendered files (JPEG etc.) and raws without matrices the image is
/// assumed D65-balanced and `temp`/`tint` are the same model with an "as shot" of 6500 K / 0 (the UI shows a
/// relative −100..100 scale for those, like Lightroom).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WhiteBalance {
    pub mode: WbMode,
    pub temp: f64,
    /// Correction direction: negative adds green, positive adds magenta.
    pub tint: f64,
    /// The scale `temp` / `tint` were written on; left out of the JSON when [`WbScale::Legacy`], so settings
    /// from before it existed read and write unchanged.
    #[serde(skip_serializing_if = "WbScale::is_legacy")]
    pub scale: WbScale,
}

impl Default for WhiteBalance {
    fn default() -> Self {
        // (`Legacy`, like an absent field: a default photo's settings JSON stays what it was; the engine marks the
        // scale when white balance is written on a raw that has one, `lcc_pipeline::local::normalize_wb`)
        Self { mode: WbMode::AsShot, temp: 6500.0, tint: 0.0, scale: WbScale::Legacy }
    }
}

/// How the Contrast and Blacks sliders act on colour (`Light::contrast_mode`, issue #839):
/// `Luminance` scales all three RGB channels by the luminance tone ratio (preserving linear colour ratios
/// and saturation); `Rgb` applies Contrast and Blacks per RGB channel like an RGB tone curve (deepening
/// shadow contrast and saturation, matching the classic per-channel curve look).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContrastMode {
    #[default]
    Luminance,
    Rgb,
}

impl ContrastMode {
    pub const fn is_default(&self) -> bool {
        matches!(self, Self::Luminance)
    }

    pub const fn is_rgb(self) -> bool {
        matches!(self, Self::Rgb)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Light {
    pub exposure: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub whites: f64,
    pub blacks: f64,
    /// Mode for Contrast and Blacks: `Luminance` (default) or `Rgb` (issue #839).
    #[serde(skip_serializing_if = "ContrastMode::is_default")]
    pub contrast_mode: ContrastMode,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToneCurve {
    pub highlights: f64,
    pub lights: f64,
    pub darks: f64,
    pub shadows: f64,
    /// Region split points (percent), Lightroom defaults 25/50/75.
    pub split_shadows: f64,
    pub split_mid: f64,
    pub split_highlights: f64,
    /// Point curves in 0..1 (identity = empty or [(0,0),(1,1)]).
    #[serde(deserialize_with = "capped::<MAX_CURVE_POINTS, _, _>")]
    pub master: Vec<Point>,
    #[serde(deserialize_with = "capped::<MAX_CURVE_POINTS, _, _>")]
    pub red: Vec<Point>,
    #[serde(deserialize_with = "capped::<MAX_CURVE_POINTS, _, _>")]
    pub green: Vec<Point>,
    #[serde(deserialize_with = "capped::<MAX_CURVE_POINTS, _, _>")]
    pub blue: Vec<Point>,
    /// Refine Saturation 0..100: 100 keeps the saturation a curve produces, lower values pull it
    /// back towards the saturation before the curve (strong contrast curves oversaturate).
    pub refine_saturation: f64,
}

impl Default for ToneCurve {
    fn default() -> Self {
        Self {
            highlights: 0.0,
            lights: 0.0,
            darks: 0.0,
            shadows: 0.0,
            split_shadows: 25.0,
            split_mid: 50.0,
            split_highlights: 75.0,
            master: Vec::new(),
            red: Vec::new(),
            green: Vec::new(),
            blue: Vec::new(),
            refine_saturation: 100.0,
        }
    }
}

impl ToneCurve {
    pub fn point_curve_is_identity(pts: &[Point]) -> bool {
        pts.is_empty() || pts.iter().all(|p| (p.x - p.y).abs() < 1e-9)
    }

    /// The four point curves (the parametric curve and Refine Saturation left out).
    pub fn points(&self) -> PointCurves {
        PointCurves { master: self.master.clone(), red: self.red.clone(), green: self.green.clone(), blue: self.blue.clone() }
    }

    /// Replace the four point curves (the parametric curve and Refine Saturation stay).
    pub fn set_points(&mut self, p: PointCurves) {
        self.master = p.master;
        self.red = p.red;
        self.green = p.green;
        self.blue = p.blue;
    }
}

/// Point curves without a parametric part: a mask's local Curve. Points in 0..1 (in, out); a
/// channel is linear when it is empty or every point lies on the diagonal. The RGB (`master`)
/// curve applies to all three channels and each colour channel's curve after it, as in the Tone
/// Curve.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PointCurves {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub master: Vec<Point>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub red: Vec<Point>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub green: Vec<Point>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blue: Vec<Point>,
}

impl PointCurves {
    /// The channels, in the order the curve editor shows them.
    pub const CHANNELS: [&str; 4] = ["master", "red", "green", "blue"];

    /// No points in any channel (the default).
    pub fn is_empty(&self) -> bool {
        self.master.is_empty() && self.red.is_empty() && self.green.is_empty() && self.blue.is_empty()
    }

    /// Every channel linear.
    pub fn is_identity(&self) -> bool {
        [&self.master, &self.red, &self.green, &self.blue].iter().all(|c| ToneCurve::point_curve_is_identity(c))
    }

    /// Channel `ch`: `master` (also `rgb`), `red`, `green` or `blue`.
    pub fn channel(&self, ch: &str) -> Option<&Vec<Point>> {
        match ch {
            "master" | "rgb" => Some(&self.master),
            "red" => Some(&self.red),
            "green" => Some(&self.green),
            "blue" => Some(&self.blue),
            _ => None,
        }
    }

    /// Channel `ch` to change (see [`PointCurves::channel`]).
    pub fn channel_mut(&mut self, ch: &str) -> Option<&mut Vec<Point>> {
        match ch {
            "master" | "rgb" => Some(&mut self.master),
            "red" => Some(&mut self.red),
            "green" => Some(&mut self.green),
            "blue" => Some(&mut self.blue),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorAdj {
    pub vibrance: f64,
    pub saturation: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hsl {
    pub hue: f64,
    pub sat: f64,
    pub lum: f64,
}

/// The 8 colour-mixer bands in Lightroom order.
pub const MIXER_BANDS: [&str; 8] = ["red", "orange", "yellow", "green", "aqua", "blue", "purple", "magenta"];
/// Centre hue of each band in degrees (sRGB-ish HSV hue).
pub const MIXER_HUES: [f64; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 225.0, 270.0, 315.0];

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mixer {
    pub red: Hsl,
    pub orange: Hsl,
    pub yellow: Hsl,
    pub green: Hsl,
    pub aqua: Hsl,
    pub blue: Hsl,
    pub purple: Hsl,
    pub magenta: Hsl,
}

impl Mixer {
    pub fn bands(&self) -> [Hsl; 8] {
        [self.red, self.orange, self.yellow, self.green, self.aqua, self.blue, self.purple, self.magenta]
    }
    pub fn is_neutral(&self) -> bool {
        self.bands().iter().all(|b| *b == Hsl::default())
    }
}

/// Most Point Color samples per photo (Lightroom's limit).
pub const MAX_POINT_COLORS: usize = 8;

/// One Point Color sample: a colour picked on the image (OkLCh, at the stage where Point Color
/// applies: after the colour mixer) and the adjustment of the colours around it. The range is a
/// box in OkLCh around the sample, widened by `range` (overall) and the per-axis widths, with a
/// soft edge.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PointColor {
    /// Sampled OkLab lightness (0..1).
    pub lum: f64,
    /// Sampled OkLCh chroma (0..~0.37).
    pub chroma: f64,
    /// Sampled OkLCh hue, degrees.
    pub hue: f64,
    /// Hue shift −100..100 (±~29° at the sample).
    pub hue_shift: f64,
    /// Saturation shift −100..100 (chroma scale).
    pub sat_shift: f64,
    /// Luminance shift −100..100.
    pub lum_shift: f64,
    /// −100 compresses the colour spread within the range towards the sample (evens out e.g.
    /// skin), +100 expands it.
    pub variance: f64,
    /// Overall range 0..100 (scales all three widths).
    pub range: f64,
    pub hue_range: f64,
    pub sat_range: f64,
    pub lum_range: f64,
}

impl Default for PointColor {
    fn default() -> Self {
        Self {
            lum: 0.5,
            chroma: 0.1,
            hue: 0.0,
            hue_shift: 0.0,
            sat_shift: 0.0,
            lum_shift: 0.0,
            variance: 0.0,
            range: 50.0,
            hue_range: 50.0,
            sat_range: 50.0,
            lum_range: 50.0,
        }
    }
}

impl PointColor {
    /// True when the sample changes nothing.
    pub fn is_neutral(&self) -> bool {
        self.hue_shift == 0.0 && self.sat_shift == 0.0 && self.lum_shift == 0.0 && self.variance == 0.0
    }
}

/// B&W mix (used when treatment = Bw): per-band luminance contribution −100..100.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BwMix {
    pub red: f64,
    pub orange: f64,
    pub yellow: f64,
    pub green: f64,
    pub aqua: f64,
    pub blue: f64,
    pub purple: f64,
    pub magenta: f64,
}

impl BwMix {
    pub fn bands(&self) -> [f64; 8] {
        [self.red, self.orange, self.yellow, self.green, self.aqua, self.blue, self.purple, self.magenta]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Wheel {
    /// Hue in degrees 0..360.
    pub hue: f64,
    /// Saturation 0..100.
    pub sat: f64,
    /// Luminance −100..100.
    pub lum: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorGrading {
    pub shadows: Wheel,
    pub midtones: Wheel,
    pub highlights: Wheel,
    pub global: Wheel,
    /// 0..100, default 50.
    pub blending: f64,
    /// −100..100.
    pub balance: f64,
}

impl Default for ColorGrading {
    fn default() -> Self {
        Self {
            shadows: Wheel::default(),
            midtones: Wheel::default(),
            highlights: Wheel::default(),
            global: Wheel::default(),
            blending: 50.0,
            balance: 0.0,
        }
    }
}

impl ColorGrading {
    pub fn is_neutral(&self) -> bool {
        [self.shadows, self.midtones, self.highlights, self.global].iter().all(|w| w.sat == 0.0 && w.lum == 0.0)
    }
}

/// The colourist primaries (the Witchcraft grading mode's wheels): two sets that both render, as
/// Resolve's do — the **Color wheels** (lift, gamma, gain, offset; wide tonal overlap) establish
/// the range, and the **Log wheels** (`log_wheels`; narrow ranges between `low` and `high`) refine
/// it. Each wheel is a [`Wheel`]: hue/saturation is the puck, luminance the master wheel beneath.
/// Semantics read from Blackmagic's *Colorist Guide to DaVinci Resolve 18* (pp. xviii, 12, 23–33);
/// the maths is this codebase's own. (An older experimental shape of this group wrote a `log`
/// switch into the same four wheels; that key is left unread.)
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Primaries {
    pub lift: Wheel,
    pub gamma: Wheel,
    pub gain: Wheel,
    pub offset: Wheel,
    pub log_wheels: LogWheels,
    /// Shadow/midtone boundary 0..1 (the Log wheels' ranges).
    pub low: f64,
    /// Midtone/highlight boundary 0..1 (the Log wheels' ranges).
    pub high: f64,
}

/// The Log wheels: narrowly separated shadows / midtones / highlights with their own offset —
/// complimentary to the Color wheels, not an alternative (both apply).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogWheels {
    pub shadow: Wheel,
    pub midtone: Wheel,
    pub highlight: Wheel,
    pub offset: Wheel,
}

impl Default for Primaries {
    fn default() -> Self {
        Self {
            lift: Wheel::default(),
            gamma: Wheel::default(),
            gain: Wheel::default(),
            offset: Wheel::default(),
            log_wheels: LogWheels::default(),
            low: 0.35,
            high: 0.7,
        }
    }
}

impl Primaries {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// No wheel moves anything (the range boundaries alone cannot).
    pub fn is_neutral(&self) -> bool {
        [&self.lift, &self.gamma, &self.gain, &self.offset]
            .into_iter()
            .chain([&self.log_wheels.shadow, &self.log_wheels.midtone, &self.log_wheels.highlight, &self.log_wheels.offset])
            .all(|w| w.sat == 0.0 && w.lum == 0.0)
    }
}

/// Resolve's HSL curves ("the first word prompts the selection, and the second word affects the
/// change"): free points anywhere over the hue circle — the curve bends through them, not through
/// fixed colour bands. `x` is hue 0..1 around the OkLCh hue circle (0 = the hue the colour mixer
/// calls red), `y` −1..1 (the curve's own strength).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HslCurves {
    #[serde(deserialize_with = "capped::<MAX_CURVE_POINTS, _, _>")]
    pub hue: Vec<Point>,
    #[serde(deserialize_with = "capped::<MAX_CURVE_POINTS, _, _>")]
    pub sat: Vec<Point>,
    #[serde(deserialize_with = "capped::<MAX_CURVE_POINTS, _, _>")]
    pub lum: Vec<Point>,
}

impl HslCurves {
    pub fn is_neutral(&self) -> bool {
        self.hue.is_empty() && self.sat.is_empty() && self.lum.is_empty()
    }

    pub fn channel(&self, id: &str) -> Option<&Vec<Point>> {
        match id {
            "hue" => Some(&self.hue),
            "sat" => Some(&self.sat),
            "lum" => Some(&self.lum),
            _ => None,
        }
    }

    pub fn channel_mut(&mut self, id: &str) -> Option<&mut Vec<Point>> {
        match id {
            "hue" => Some(&mut self.hue),
            "sat" => Some(&mut self.sat),
            "lum" => Some(&mut self.lum),
            _ => None,
        }
    }
}

/// Camera calibration (Lightroom Classic's Calibration panel): a green/magenta tint of the shadows,
/// and a hue rotation / saturation scale of each working-space primary (−100..100), i.e. a
/// white-preserving 3×3 matrix applied in scene-linear light before tone mapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Calibration {
    pub shadows_tint: f64,
    pub red_hue: f64,
    pub red_sat: f64,
    pub green_hue: f64,
    pub green_sat: f64,
    pub blue_hue: f64,
    pub blue_sat: f64,
}

impl Calibration {
    /// (hue, saturation) of the red, green and blue primaries.
    pub fn primaries(&self) -> [(f64, f64); 3] {
        [(self.red_hue, self.red_sat), (self.green_hue, self.green_sat), (self.blue_hue, self.blue_sat)]
    }
    pub fn is_neutral(&self) -> bool {
        *self == Calibration::default()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Effects {
    pub texture: f64,
    pub clarity: f64,
    pub dehaze: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VignetteStyle {
    #[default]
    HighlightPriority,
    ColorPriority,
    PaintOverlay,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Vignette {
    pub amount: f64,
    pub midpoint: f64,
    pub roundness: f64,
    pub feather: f64,
    pub highlights: f64,
    pub style: VignetteStyle,
}

impl Default for Vignette {
    fn default() -> Self {
        Self { amount: 0.0, midpoint: 50.0, roundness: 0.0, feather: 50.0, highlights: 0.0, style: VignetteStyle::HighlightPriority }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grain {
    pub amount: f64,
    /// Grain size, relative to the frame (0..100). From process V7 the mean grain radius is 0.3–3 px on a
    /// 6000 px export (≈ 1.8–18 µm on a 35 mm frame); finer grain is also stronger, as in Lightroom Classic.
    pub size: f64,
    /// 0 = fine, round, regular grain; 100 = coarse, elongated, irregular grains.
    pub roughness: f64,
    /// Seed for the procedural grain (stable per photo).
    pub seed: u32,
    /// Colour (chroma) grain, 0..100: independent dye-layer grain on top of the luminance grain (V7).
    #[serde(skip_serializing_if = "is_zero")]
    pub color: f64,
    /// Where grain is strongest (V7): −100 shadows … 0 equal at every tone … +100 highlights.
    #[serde(skip_serializing_if = "is_zero")]
    pub response: f64,
    /// Spread of grain sizes, 0..100 (V7): 0 = all grains the mean size.
    pub size_variation: f64,
    /// Clumping, 0..100 (V7): 0 = single grains; 100 = clusters of several grains.
    pub clumping: f64,
}

impl Default for Grain {
    fn default() -> Self {
        Self { amount: 0.0, size: 25.0, roughness: 50.0, seed: 0, color: 0.0, response: 0.0, size_variation: 40.0, clumping: 30.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Detail {
    pub sharpen_amount: f64,
    pub sharpen_radius: f64,
    pub sharpen_detail: f64,
    pub sharpen_masking: f64,
    pub nr_luminance: f64,
    pub nr_detail: f64,
    pub nr_contrast: f64,
    pub nr_color: f64,
    pub nr_color_detail: f64,
    pub nr_color_smoothness: f64,
    /// Replace single outlier photosites (noise spikes far above all their same-colour neighbours) in the
    /// raw mosaic before demosaicing, so they do not become coloured dots (#1345). On for newly imported
    /// raws ([`crate::DevelopSettings::for_raw`]); off for edits saved before it existed, so they look the
    /// same. Raw files only.
    pub nr_outliers: bool,
}

impl Default for Detail {
    /// Lightroom's raw defaults are sharpening 40/1.0/25/0 and colour NR 25/50/50; we use neutral
    /// values for rendered files and apply raw defaults at import time (`DevelopSettings::for_raw`).
    fn default() -> Self {
        Self {
            sharpen_amount: 0.0,
            sharpen_radius: 1.0,
            sharpen_detail: 25.0,
            sharpen_masking: 0.0,
            nr_luminance: 0.0,
            nr_detail: 50.0,
            nr_contrast: 0.0,
            nr_color: 0.0,
            nr_color_detail: 50.0,
            nr_color_smoothness: 50.0,
            nr_outliers: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Optics {
    pub remove_ca: bool,
    pub lens_profile: bool,
    pub profile_distortion: f64,
    pub profile_vignetting: f64,
    pub defringe_purple_amount: f64,
    pub defringe_purple_hue_lo: f64,
    pub defringe_purple_hue_hi: f64,
    pub defringe_green_amount: f64,
    pub defringe_green_hue_lo: f64,
    pub defringe_green_hue_hi: f64,
    pub distortion: f64,
    pub vignetting: f64,
    pub vignetting_midpoint: f64,
    /// Manual lateral chromatic aberration (red/cyan, blue/yellow fringes), −100..100, added to the
    /// automatic estimate when `remove_ca` is on.
    pub ca_red: f64,
    pub ca_blue: f64,
    /// A lens profile the user picked from the lens database for this photo (an adapted or manual lens, one its
    /// metadata misnames); `None`: the file's own correction, else the database's match by the lens name. Not
    /// written to XMP.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lens: Option<ChosenLens>,
    /// The flat-field map (Capture One's LCC "Uniform Light", [`crate::vignetting_calibration`]) that evens out the lens's
    /// fall-off as measured from a reference shot, whatever `lens_profile` says: its id; the catalog holds the map.
    /// Not written while there is none, so settings without one keep their JSON and `hash64`; anything but an id
    /// reads as none.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub vignetting_calibration: Option<crate::VignettingMapId>,
    /// How much of the map's correction applies, % (0 to 120). Not written at the default (100).
    #[serde(skip_serializing_if = "is_default_vignetting_calibration_amount")]
    pub vignetting_calibration_amount: f64,
}

fn is_default_vignetting_calibration_amount(a: &f64) -> bool {
    *a == crate::DEFAULT_VIGNETTING_CALIBRATION_PERCENT
}

/// A lens picked from the lens database ([`Optics::lens`]): its maker and model as the database names them, and the
/// focal length to use when the photo records none.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChosenLens {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub make: Option<String>,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focal: Option<f64>,
}

impl Default for Optics {
    fn default() -> Self {
        Self {
            remove_ca: false,
            lens_profile: false,
            profile_distortion: 100.0,
            profile_vignetting: 100.0,
            defringe_purple_amount: 0.0,
            defringe_purple_hue_lo: 30.0,
            defringe_purple_hue_hi: 70.0,
            defringe_green_amount: 0.0,
            defringe_green_hue_lo: 40.0,
            defringe_green_hue_hi: 60.0,
            distortion: 0.0,
            vignetting: 0.0,
            vignetting_midpoint: 50.0,
            ca_red: 0.0,
            ca_blue: 0.0,
            lens: None,
            vignetting_calibration: None,
            vignetting_calibration_amount: crate::DEFAULT_VIGNETTING_CALIBRATION_PERCENT,
        }
    }
}

/// Lens corrections embedded in the file, converted to the oriented, default-cropped image: a DNG's `OpcodeList3`
/// (`WarpRectilinear`, `FixVignetteRadial`), or a raw reader's equivalent of the camera's own correction (Panasonic /
/// Leica RW2 distortion, `lcc_raw`'s `vendor/rw2.rs`). This is camera/file data (stored on the photo record, not in the develop
/// settings); "Enable Profile Corrections" applies it, scaled by the profile distortion/vignetting amounts.
/// The app never uses Adobe LCP lens profiles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbeddedLens {
    pub warp: Option<EmbeddedWarp>,
    pub vignette: Option<EmbeddedVignette>,
    /// A DNG 1.6 `WarpRectilinear2` chain (up to [`WARP_STAGES`] stages, in `OpcodeList3` order; each stage warps
    /// the result of the one before it). When any is set, `warp` is `None`: the chain holds every warp opcode.
    pub stages: [Option<EmbeddedWarpStage>; WARP_STAGES],
}

/// Most warp stages an [`EmbeddedLens`] chain keeps.
pub const WARP_STAGES: usize = 3;

/// One DNG `WarpRectilinear2` (or a `WarpRectilinear` in a chain with one): per plane (R, G, B)
/// `[kr0 .. kr14, kt0, kt1, min_valid_radius, max_valid_radius]`. For an output point at offset
/// `d = (p − center) / radius` with `r = |d|` and `f = Σ krₚ·clamp(r, min, max)ᵖ`, the source point is
/// `center + radius · (d·f + tangential(d))`, or `d / f` for the radial part when `reciprocal`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EmbeddedWarpStage {
    pub planes: [[f64; 19]; 3],
    /// Optical centre, normalized to the oriented image (0..1).
    pub center: Point,
    /// Normalisation radius as a fraction of the oriented image's long edge.
    pub radius: f64,
    pub reciprocal: bool,
}

impl EmbeddedLens {
    /// Whether a `WarpRectilinear2` chain is set.
    pub fn has_stages(&self) -> bool {
        self.stages.iter().any(Option::is_some)
    }
}

/// DNG `WarpRectilinear`: per plane (R, G, B) `[kr0, kr1, kr2, kr3, kt0, kt1]`. For an output (corrected) point at
/// offset `d = (p − center) / radius`, the source point is `center + radius · (d·f(r²) + tangential(d))`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EmbeddedWarp {
    pub planes: [[f64; 6]; 3],
    /// Optical centre, normalized to the oriented image (0..1).
    pub center: Point,
    /// Normalisation radius as a fraction of the oriented image's long edge.
    pub radius: f64,
}

/// DNG `FixVignetteRadial`: gain `1 + k0 r² + k1 r⁴ + … + k4 r¹⁰` with `r = |p − center| / radius`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EmbeddedVignette {
    pub k: [f64; 5],
    pub center: Point,
    pub radius: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Upright {
    #[default]
    Off,
    Auto,
    Guided,
    Level,
    Vertical,
    Full,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Geometry {
    pub upright: Upright,
    /// Guided Upright guides, up to 4 line segments, in normalized coordinates of the lens-corrected
    /// (pre-perspective) oriented image.
    #[serde(deserialize_with = "capped::<MAX_GUIDES, _, _>")]
    pub guides: Vec<(Point, Point)>,
    /// The automatic Upright correction found by analysis for `upright` (Auto/Level/Vertical/Full), as a
    /// homography lens-corrected → transformed in centred coordinates (`(p − centre) / (long edge / 2)`).
    /// Stored so previews and exports use the identical transform; recomputed by `geometry.upright`.
    pub upright_transform: Option<Homography>,
    /// Lightroom Classic's own solved Upright correction for `upright`, read from its XMP
    /// (`crs:UprightTransform_<mode>`), in Lightroom's convention: lens-corrected → transformed in normalized
    /// coordinates of the oriented image (`x / width`, `y / height`), with the crop in the transformed frame.
    /// Used (converted to centred coordinates) when `upright_transform` is empty, so the photo's geometry and
    /// crop match Lightroom's instead of being detected afresh. Any Upright edit made here clears it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lrc_upright: Option<Homography>,
    pub vertical: f64,
    pub horizontal: f64,
    pub rotate: f64,
    pub aspect: f64,
    pub scale: f64,
    pub offset_x: f64,
    pub offset_y: f64,
    pub constrain_crop: bool,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            upright: Upright::Off,
            guides: Vec::new(),
            upright_transform: None,
            lrc_upright: None,
            vertical: 0.0,
            horizontal: 0.0,
            rotate: 0.0,
            aspect: 0.0,
            scale: 100.0,
            offset_x: 0.0,
            offset_y: 0.0,
            constrain_crop: false,
        }
    }
}

impl Geometry {
    pub fn is_identity(&self) -> bool {
        self.upright == Upright::Off
            && self.vertical == 0.0
            && self.horizontal == 0.0
            && self.rotate == 0.0
            && self.aspect == 0.0
            && self.scale == 100.0
            && self.offset_x == 0.0
            && self.offset_y == 0.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Crop {
    pub geometry: CropGeometry,
    /// Locked aspect (w, h); None = free / original.
    pub aspect: Option<(u32, u32)>,
    pub flip_h: bool,
    pub flip_v: bool,
}

// ---------------------------------------------------------------------------------------------
// Masks and local adjustments

/// The sliders available inside a mask (Lightroom's local adjustments).
///
/// Serialized through [`LocalAdjustmentsWire`], which converts the Hue of older catalogs, presets
/// and effect files (a −100..100 slider value) to degrees.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "LocalAdjustmentsWire", into = "LocalAdjustmentsWire")]
pub struct LocalAdjustments {
    pub temp: f64,
    pub tint: f64,
    pub exposure: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub whites: f64,
    pub blacks: f64,
    pub texture: f64,
    pub clarity: f64,
    pub dehaze: f64,
    /// Hue shift in degrees, −180..180 ([`LOCAL_HUE_MAX`]).
    pub hue: f64,
    pub saturation: f64,
    pub sharpness: f64,
    pub noise: f64,
    pub moire: f64,
    pub defringe: f64,
    /// Colour overlay (hue degrees, saturation 0..100); sat 0 = none.
    pub color_hue: f64,
    pub color_sat: f64,
    /// Colour grading inside the mask: per zone (shadows, midtones, highlights, global) a hue
    /// (degrees 0..360), saturation (0..100) and luminance (−100..100), as the Color Grading
    /// panel's wheels, with its default blending and balance. Absent in older catalogs: 0.
    pub grade_shadows_hue: f64,
    pub grade_shadows_sat: f64,
    pub grade_shadows_lum: f64,
    pub grade_midtones_hue: f64,
    pub grade_midtones_sat: f64,
    pub grade_midtones_lum: f64,
    pub grade_highlights_hue: f64,
    pub grade_highlights_sat: f64,
    pub grade_highlights_lum: f64,
    pub grade_global_hue: f64,
    pub grade_global_sat: f64,
    pub grade_global_lum: f64,
    /// Local vignette/grain are not in Lightroom; we keep the struct extensible.
    pub amount: f64,
}

/// The mask Hue slider's reach either way, in degrees.
pub const LOCAL_HUE_MAX: f64 = 180.0;

/// Degrees per step of the old mask Hue slider: it was stored as −100..100 (Lightroom's
/// `crs:LocalHue` × 100) and rendered as `value / 100 × 0.6` radians (about ±34°).
pub const LEGACY_LOCAL_HUE_DEG: f64 = 0.6 * 180.0 / std::f64::consts::PI / 100.0;

/// The mask Hue's standard reach (Lightroom's `crs:LocalHue` ±1), in degrees: about 34.38°.
pub const LOCAL_HUE_STANDARD: f64 = 100.0 * LEGACY_LOCAL_HUE_DEG;

macro_rules! local_wire {
    ($($f:ident),* $(,)?) => {
        /// How [`LocalAdjustments`] is stored. `hue_deg` marks a Hue in degrees; without it (catalogs,
        /// presets and effect files from before the change, partial JSON) `hue` is the old −100..100
        /// slider value and converts to the same angle in degrees.
        #[derive(Clone, Copy, Serialize, Deserialize)]
        #[serde(default)]
        pub struct LocalAdjustmentsWire {
            $($f: f64,)*
            hue: f64,
            hue_deg: bool,
        }

        impl Default for LocalAdjustmentsWire {
            fn default() -> Self {
                let d = LocalAdjustments::default();
                Self { $($f: d.$f,)* hue: d.hue, hue_deg: false }
            }
        }

        impl From<LocalAdjustmentsWire> for LocalAdjustments {
            fn from(w: LocalAdjustmentsWire) -> Self {
                let hue = if w.hue_deg { w.hue } else { w.hue * LEGACY_LOCAL_HUE_DEG };
                Self { $($f: w.$f,)* hue }
            }
        }

        impl From<LocalAdjustments> for LocalAdjustmentsWire {
            fn from(a: LocalAdjustments) -> Self {
                Self { $($f: a.$f,)* hue: a.hue, hue_deg: true }
            }
        }
    };
}

local_wire!(
    temp,
    tint,
    exposure,
    contrast,
    highlights,
    shadows,
    whites,
    blacks,
    texture,
    clarity,
    dehaze,
    saturation,
    sharpness,
    noise,
    moire,
    defringe,
    color_hue,
    color_sat,
    grade_shadows_hue,
    grade_shadows_sat,
    grade_shadows_lum,
    grade_midtones_hue,
    grade_midtones_sat,
    grade_midtones_lum,
    grade_highlights_hue,
    grade_highlights_sat,
    grade_highlights_lum,
    grade_global_hue,
    grade_global_sat,
    grade_global_lum,
    amount,
);

impl LocalAdjustments {
    /// The mask's colour-grading wheels (shadows, midtones, highlights, global) as [`Wheel`]s.
    pub fn grading_wheels(&self) -> [Wheel; 4] {
        let w = |hue: f64, sat: f64, lum: f64| Wheel { hue, sat, lum };
        [
            w(self.grade_shadows_hue, self.grade_shadows_sat, self.grade_shadows_lum),
            w(self.grade_midtones_hue, self.grade_midtones_sat, self.grade_midtones_lum),
            w(self.grade_highlights_hue, self.grade_highlights_sat, self.grade_highlights_lum),
            w(self.grade_global_hue, self.grade_global_sat, self.grade_global_lum),
        ]
    }
}

impl Default for LocalAdjustments {
    fn default() -> Self {
        Self {
            temp: 0.0,
            tint: 0.0,
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            texture: 0.0,
            clarity: 0.0,
            dehaze: 0.0,
            hue: 0.0,
            saturation: 0.0,
            sharpness: 0.0,
            noise: 0.0,
            moire: 0.0,
            defringe: 0.0,
            color_hue: 0.0,
            color_sat: 0.0,
            grade_shadows_hue: 0.0,
            grade_shadows_sat: 0.0,
            grade_shadows_lum: 0.0,
            grade_midtones_hue: 0.0,
            grade_midtones_sat: 0.0,
            grade_midtones_lum: 0.0,
            grade_highlights_hue: 0.0,
            grade_highlights_sat: 0.0,
            grade_highlights_lum: 0.0,
            grade_global_hue: 0.0,
            grade_global_sat: 0.0,
            grade_global_lum: 0.0,
            amount: 100.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MaskOp {
    #[default]
    Add,
    Subtract,
    Intersect,
}

/// One brush dab sequence. Coordinates normalized to the uncropped oriented image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrushStroke {
    #[serde(deserialize_with = "capped::<MAX_STROKE_POINTS, _, _>")]
    pub points: Vec<Point>,
    /// Radius as a fraction of the image's long edge: the whole dab ([`BRUSH_MODEL_LEGACY`]) or its solid core
    /// ([`BRUSH_MODEL_LRC`], Lightroom's Size; the halo reaches [`crate::brush_model::outer_ratio`] times it).
    pub size: f64,
    /// 0..100.
    pub feather: f64,
    pub flow: f64,
    pub density: f64,
    pub erase: bool,
    pub auto_mask: bool,
    /// Pen pressure (0..1) at each of `points`, recorded when the brush's pressure switch is on: it scales each dab's
    /// radius and flow. Empty (the default, a mouse or the switch off) is full pressure everywhere; a point past the
    /// end of a shorter list takes the last value.
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "capped::<MAX_STROKE_POINTS, _, _>")]
    pub pressure: Vec<f32>,
    /// The brush model the stroke is painted with: [`BRUSH_MODEL_LEGACY`] (absent, strokes saved before
    /// the app followed Lightroom: `size` is the whole footprint, Feather softens inward, strokes combine by
    /// their maximum) or [`BRUSH_MODEL_LRC`] (Lightroom Classic's brush, [`crate::brush_model`]: `size` is the
    /// solid core, Feather adds a halo outside it, dabs and strokes add up). Old strokes keep their look.
    #[serde(skip_serializing_if = "is_legacy_brush")]
    pub model: u8,
    /// Auto Mask range (−100 tight … 0 … +100 wide), fixed when the stroke is painted: how different a pixel
    /// may be from the one under the dab and still take the paint. 0 (the default, strokes saved before the
    /// slider and strokes imported from Lightroom) is the limits measured on Lightroom Classic; the range
    /// scales them all together, by ½ at −100 and 2× at +100 ([`BrushStroke::auto_range_scale`]). Only a
    /// Lightroom-model stroke with Auto Mask on uses it.
    #[serde(skip_serializing_if = "is_zero")]
    pub auto_range: f64,
}

/// The brush model of strokes saved before the app followed Lightroom's ([`BrushStroke::model`]).
pub const BRUSH_MODEL_LEGACY: u8 = 0;
/// Lightroom Classic's brush model ([`crate::brush_model`]): new strokes and strokes imported from Lightroom.
pub const BRUSH_MODEL_LRC: u8 = 1;

fn is_legacy_brush(m: &u8) -> bool {
    *m == BRUSH_MODEL_LEGACY
}

impl BrushStroke {
    /// Whether the stroke follows Lightroom Classic's brush model (any model past the legacy one).
    pub fn is_lrc(&self) -> bool {
        self.model != BRUSH_MODEL_LEGACY
    }

    /// The pressure at point `i` (0..1; 1 without pressure data, a hostile value clamped, NaN as 1).
    pub fn pressure_at(&self, i: usize) -> f32 {
        let p = self.pressure.get(i).or(self.pressure.last()).copied().unwrap_or(1.0);
        if p.is_nan() { 1.0 } else { p.clamp(0.0, 1.0) }
    }

    /// How much [`BrushStroke::auto_range`] widens Auto Mask's limits: `2^(range / 100)`, ½ (tight) … 1 … 2
    /// (wide); a hostile value is clamped, NaN is 1.
    pub fn auto_range_scale(&self) -> f32 {
        let v = if self.auto_range.is_finite() { self.auto_range.clamp(-AUTO_RANGE_MAX, AUTO_RANGE_MAX) } else { 0.0 };
        2f64.powf(v / AUTO_RANGE_MAX) as f32
    }
}

/// The ends of [`BrushStroke::auto_range`]: −this is tightest, +this widest.
pub const AUTO_RANGE_MAX: f64 = 100.0;

impl Default for BrushStroke {
    fn default() -> Self {
        Self {
            points: Vec::new(),
            size: 0.03,
            feather: 50.0,
            flow: 100.0,
            density: 100.0,
            erase: false,
            auto_mask: false,
            pressure: Vec::new(),
            model: BRUSH_MODEL_LEGACY,
            auto_range: 0.0,
        }
    }
}

/// The place of a colour range sample picked before its pins existed (no pin shows for it).
pub const UNKNOWN_COLOR_AREA: [f64; 4] = [-1.0, -1.0, -1.0, -1.0];

/// A colour range's pins: each place its samples were picked (`areas`, one per sample, see
/// [`MaskShape::ColorRange`]) and the samples it gave (consecutive ones from the same place make
/// one pin). Places not known (or not on the photo) have no pin.
pub fn color_range_pins(areas: &[[f64; 4]]) -> Vec<([f64; 4], std::ops::Range<usize>)> {
    let mut out: Vec<([f64; 4], std::ops::Range<usize>)> = Vec::new();
    for (i, a) in areas.iter().enumerate() {
        if !a.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
            continue;
        }
        match out.last_mut() {
            Some((last, r)) if last == a && r.end == i => r.end = i + 1,
            _ => out.push((*a, i..i + 1)),
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MaskShape {
    Brush {
        #[serde(deserialize_with = "capped::<MAX_STROKES, _, _>")]
        strokes: Vec<BrushStroke>,
    },
    /// Linear gradient: full effect at `start`, fading to none at `end` (normalized coords).
    Linear {
        start: Point,
        end: Point,
    },
    /// Radial gradient: ellipse centre, radii (normalized to the long edge), rotation (deg), feather 0..100.
    Radial {
        center: Point,
        rx: f64,
        ry: f64,
        angle: f64,
        feather: f64,
        invert: bool,
    },
    /// Colour range: sampled OkLab colours with a refine amount 0..100.
    ColorRange {
        #[serde(deserialize_with = "capped::<MAX_COLOR_SAMPLES, _, _>")]
        samples: Vec<[f64; 3]>,
        refine: f64,
        /// Where each of `samples` was picked (normalized `[x0, y0, x1, y1]`; a click is a point,
        /// `x0 == x1`), one per sample: the photo shows a pin per picked place (an area dragged
        /// gives up to three samples, one pin), which can be moved or removed. Empty for samples
        /// picked before the pins existed (no pins then).
        #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "capped::<MAX_COLOR_SAMPLES, _, _>")]
        areas: Vec<[f64; 4]>,
    },
    /// Luminance range (0..1 bounds with feathered falloff 0..1).
    LuminanceRange {
        lo: f64,
        hi: f64,
        lo_feather: f64,
        hi_feather: f64,
    },
    DepthRange {
        lo: f64,
        hi: f64,
        feather: f64,
    },
    /// AI / automatic selections, evaluated by a segmenter.
    Subject,
    Sky,
    Background,
    /// One object picked by clicks (SAM 3 point prompts): `hint` holds the clicks that include,
    /// `exclude` the ones that exclude; `seg` the segmentation computed from them.
    Object {
        #[serde(deserialize_with = "capped::<MAX_PROMPT_POINTS, _, _>")]
        hint: Vec<Point>,
        #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "capped::<MAX_PROMPT_POINTS, _, _>")]
        exclude: Vec<Point>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seg: Option<crate::SegMask>,
        /// Zoomed-in passes over parts of the image (higher resolution than `seg`), used
        /// inside their rectangles.
        #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "crate::segmask::de_detail")]
        detail: Vec<crate::SegMask>,
        /// Edge −100..100: below 0 harder (a steeper transition), above 0 softer (feathered).
        #[serde(default, skip_serializing_if = "is_zero")]
        edge: f64,
    },
    /// Everything a description names ("sky", "the red car": SAM 3 concept prompts); `seg` is
    /// the segmentation computed for it.
    Prompt {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seg: Option<crate::SegMask>,
        #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "crate::segmask::de_detail")]
        detail: Vec<crate::SegMask>,
        /// Edge −100..100, as for `Object`.
        #[serde(default, skip_serializing_if = "is_zero")]
        edge: f64,
    },
    People {
        person: u32,
        #[serde(deserialize_with = "capped::<MAX_PEOPLE_PARTS, _, _>")]
        parts: Vec<String>,
        /// Who it is about by name or role ("Sam", the Bride of this shoot), resolved on each
        /// photo when the mask is computed; `None`: by `person` (position).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        who: Option<crate::recipe::Who>,
        /// The selection the people models computed on this photo (`None`: not computed, or no
        /// models installed: the pipeline's heuristic).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seg: Option<crate::SegMask>,
    },
    Landscape {
        class: String,
    },
    /// A remembered object ("Wedding dress"), taught by example (`concepts.*`) and found again on
    /// each photo; `seg` what was found here (`None`: not yet, or not found: nothing selected).
    Concept {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seg: Option<crate::SegMask>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaskComponent {
    /// A name given in the Masking panel (Rename); `None` shows the shape's kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A gradient placed relative to a selection's box ("from the bottom of the couple"): the
    /// recipe, resolved into `shape` on each photo when its AI masks are computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<crate::recipe::Anchor>,
    #[serde(default)]
    pub op: MaskOp,
    #[serde(default)]
    pub invert: bool,
    pub shape: MaskShape,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mask {
    pub id: u32,
    pub name: String,
    pub visible: bool,
    pub invert: bool,
    #[serde(deserialize_with = "capped::<MAX_MASK_COMPONENTS, _, _>")]
    pub components: Vec<MaskComponent>,
    pub adjust: LocalAdjustments,
    /// Refine Edges 0..100: the mask's edges snap to the photo's (guided filter).
    #[serde(skip_serializing_if = "is_zero")]
    pub refine: f64,
    /// Feather 0..100: the whole mask's edges soften (a Gaussian blur of up to 2 % of the long edge,
    /// so previews and exports match).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub feather: f64,
    /// The local Curve: point curves applied inside the mask, after the photo's Tone Curve. Not written while it has
    /// no points, so masks without one keep their JSON (and every cached preview its `hash64`).
    #[serde(skip_serializing_if = "PointCurves::is_empty")]
    pub curve: PointCurves,
    /// The local Point Color: up to [`MAX_POINT_COLORS`] samples, applied inside the mask after the
    /// photo's colour tools, weighted by the mask. Not written while empty.
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "capped::<MAX_POINT_COLORS, _, _>")]
    pub point_colors: Vec<PointColor>,
    /// The local Color Mixer: the global mixer's 8 bands, applied inside the mask after the photo's
    /// colour tools and before the mask's Point Color, weighted by the mask. Not written while neutral.
    #[serde(skip_serializing_if = "Mixer::is_neutral")]
    pub mixer: Mixer,
    /// Where its coordinates (gradients, brush strokes, Object clicks and AI selections) live:
    /// `true`, on the photo as shot (user orientation, before lens corrections, Upright and
    /// Transform), so the mask stays on the same content whatever those do; `false`, a mask from
    /// before that, on the corrected frame. Read as `false` when absent; [`Mask::default`] (every
    /// new mask) is `true`, and the next edit moves an old one there (`lcc_pipeline::mask_space`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub on_photo: bool,
    /// The preset that added it (applying that preset again refreshes it instead of adding a copy).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// The linked group it belongs to: the copies of one mask applied to several photos in one go
    /// (Auto Sync, a preset on a selection, Paste, Sync Settings, Paste from Previous) share it.
    /// With Auto Sync on, an edit of one of them goes to its copies on the other selected photos
    /// ([`crate::recipe::sync_masks`]); a copy on a photo that isn't selected, or any edit with Auto
    /// Sync off, stays as it is. Empty: not linked (every mask from before links existed).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub link: String,
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl Default for Mask {
    fn default() -> Self {
        Self {
            id: 0,
            name: "Mask 1".into(),
            visible: true,
            invert: false,
            components: Vec::new(),
            adjust: LocalAdjustments::default(),
            refine: 0.0,
            feather: 0.0,
            curve: PointCurves::default(),
            point_colors: Vec::new(),
            mixer: Mixer::default(),
            on_photo: true,
            preset: None,
            link: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Heal / remove / red eye

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpotMode {
    #[default]
    Remove,
    Heal,
    Clone,
    /// Content-aware: the spot is rebuilt from patches of its surroundings (no single source).
    Fill,
    /// Even out (flat-field): the spot keeps its own pixels and is lifted by the smooth gain that brings its tone to
    /// its surroundings' (divides by the estimated transmission of a sensor dust shadow). No source.
    Flat,
    /// Generative: the spot is filled by a cloud image model the user chose (their own API key),
    /// stored as a patch of the photo's own pixels ([`GenFill`]). Until it is generated (or when
    /// it was moved since, or its patch file is missing) it changes nothing: it only marks the area.
    Generate,
}

/// A generative fill: which patches a [`SpotMode::Generate`] spot shows. The pixels are never in the
/// settings (or XMP): they are files in the library's `genfill` folder, named by the photo and `id`
/// (`docs/generative-fill.md`); these settings only refer to them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GenFill {
    /// Id of this generation (16 hex digits): its variations are the files `<id>-<n>.lcpatch`.
    pub id: String,
    /// How many variations were generated, and which one (0-based) the spot shows.
    pub variants: u32,
    pub variant: u32,
    /// The provider that made them (`openai`, `stability`, …) and the prompt, if any.
    pub provider: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub prompt: String,
    /// [`Spot::stroke_key`] of the stroke they were made for: once the spot is moved or resized the
    /// patch no longer fits and the spot renders as a content-aware fill until generated again.
    pub stroke: u64,
    /// Generation resolution relative to the photo's own pixels: 1 = native, below 1 = upscaled
    /// (a softer patch; the panel says so).
    pub scale: f64,
    /// The stroke these results were made for, so a spot moved or resized since can be put back where its fill
    /// fits ([`Spot::put_back`]). `None` in fills made before it was kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub made_for: Option<GenStroke>,
    /// The spot's earlier generations, in the order they were made (each a paid result: kept, and offered with
    /// the variations). Never nested: their own `earlier` is empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub earlier: Vec<GenFill>,
    /// Where this generation sits among `earlier` (how many of them come before it).
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub place: u32,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

/// A stroke a generative fill was made for: its points and size (see [`Spot`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GenStroke {
    pub points: Vec<Point>,
    pub size: f64,
}

impl GenStroke {
    /// [`Spot::stroke_key`] of a spot with this stroke.
    pub fn key(&self) -> u64 {
        stroke_key(&self.points, self.size)
    }
}

/// See [`Spot::stroke_key`].
fn stroke_key(points: &[Point], size: f64) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |v: f64| {
        for b in v.to_bits().to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for p in points {
        eat(p.x);
        eat(p.y);
    }
    eat(size);
    h
}

/// Most generations a spot keeps (the oldest beyond are dropped from the spot; their files stay in the library and
/// can be restored from there).
pub const MAX_GENERATIONS: usize = 32;

impl GenFill {
    /// This spot's generations in the order they were made, this one among them (each without its `earlier`).
    pub fn generations(&self) -> Vec<GenFill> {
        let one = |g: &GenFill| GenFill { earlier: Vec::new(), place: 0, ..g.clone() };
        let place = (self.place as usize).min(self.earlier.len());
        let mut all: Vec<GenFill> = self.earlier.iter().take(place).map(one).collect();
        all.push(one(self));
        all.extend(self.earlier.iter().skip(place).map(one));
        all
    }

    /// `generations` with generation `index` shown (its variation `variant`), the others kept as earlier ones.
    fn from_generations(mut all: Vec<GenFill>, index: usize, variant: u32) -> Option<GenFill> {
        if index >= all.len() {
            return None;
        }
        let mut cur = all.remove(index);
        cur.variant = variant.min(cur.variants.saturating_sub(1));
        cur.place = index as u32;
        cur.earlier = all;
        Some(cur)
    }

    /// Every result of every generation: how many, and which one (0-based) is shown.
    pub fn results(&self) -> (u32, u32) {
        let place = (self.place as usize).min(self.earlier.len());
        let before: u32 = self.earlier.iter().take(place).map(|g| g.variants.max(1)).fold(0, u32::saturating_add);
        let after: u32 = self.earlier.iter().skip(place).map(|g| g.variants.max(1)).fold(0, u32::saturating_add);
        let total = before.saturating_add(self.variants.max(1)).saturating_add(after);
        (total, before.saturating_add(self.variant.min(self.variants.max(1) - 1)))
    }

    /// Show result `k` of [`GenFill::results`] (any generation's variation); `false` when there is no such result.
    pub fn choose(&mut self, k: u32) -> bool {
        let all = self.generations();
        let mut at = 0u32;
        for (i, g) in all.iter().enumerate() {
            let n = g.variants.max(1);
            if k < at.saturating_add(n) {
                let v = k - at;
                return match GenFill::from_generations(all, i, v) {
                    Some(g) => {
                        *self = g;
                        true
                    }
                    None => false,
                };
            }
            at = at.saturating_add(n);
        }
        false
    }

    /// `next`, made after this generation, with this one and its earlier ones kept as earlier generations (at
    /// most [`MAX_GENERATIONS`] in all).
    pub fn followed_by(&self, mut next: GenFill) -> GenFill {
        let mut all = self.generations();
        all.extend(next.generations().into_iter().skip(1));
        let drop = (all.len() + 1).saturating_sub(MAX_GENERATIONS);
        all.drain(..drop.min(all.len()));
        next.place = all.len() as u32;
        next.earlier = all;
        next
    }

    /// The name of the patch file of variation `variant` (without the folder).
    pub fn file_name(&self, variant: u32) -> String {
        format!("{}-{variant}.lcpatch", self.id)
    }

    /// Whether `id` is a generation id this build writes (16 lower-case hex digits): anything else
    /// is never turned into a file name.
    pub fn valid_id(id: &str) -> bool {
        id.len() == 16 && id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Spot {
    pub mode: SpotMode,
    /// Target: a single point (circle spot) or a brushed path. Normalized coords.
    #[serde(deserialize_with = "capped::<MAX_SPOT_POINTS, _, _>")]
    pub points: Vec<Point>,
    /// Radius as a fraction of the long edge.
    pub size: f64,
    pub feather: f64,
    pub opacity: f64,
    /// Source offset (normalized) from the target; None = auto (content-aware remove).
    pub source_offset: Option<Point>,
    /// The generated patches of a [`SpotMode::Generate`] spot (none until generated).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated: Option<GenFill>,
    /// Added by an automatic finder (Remove Dust), not drawn by hand; `None` once the user moves or resizes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto: Option<AutoSpot>,
}

impl Default for Spot {
    fn default() -> Self {
        Self {
            mode: SpotMode::Remove,
            points: Vec::new(),
            size: 0.02,
            feather: 50.0,
            opacity: 100.0,
            source_offset: None,
            generated: None,
            auto: None,
        }
    }
}

impl Spot {
    /// A stable fingerprint of the stroke (points and size; FNV-1a over their bits, the same in
    /// every build): a generated patch is shown only while the stroke is the one it was made for.
    pub fn stroke_key(&self) -> u64 {
        stroke_key(&self.points, self.size)
    }

    /// The generated patch this spot shows: its [`GenFill`] while it fits the stroke.
    pub fn shown_gen(&self) -> Option<&GenFill> {
        self.generated.as_ref().filter(|g| self.mode == SpotMode::Generate && g.stroke == self.stroke_key() && GenFill::valid_id(&g.id))
    }

    /// Put a spot that was moved or resized since its fill was generated back on the stroke the fill was made for,
    /// so it shows again. `false` when it already fits or the stroke isn't known.
    pub fn put_back(&mut self) -> bool {
        let Some(g) = &self.generated else { return false };
        if g.stroke == self.stroke_key() {
            return false;
        }
        let Some(at) = g.made_for.clone() else { return false };
        let (points, size) = (std::mem::replace(&mut self.points, at.points), self.size);
        self.size = at.size;
        if self.generated.as_ref().is_some_and(|g| g.stroke == self.stroke_key()) {
            return true;
        }
        // (a stroke that doesn't hash to its key: leave the spot as it was)
        (self.points, self.size) = (points, size);
        false
    }

    /// Move the spot's points with `f` (a change of coordinates, e.g. to follow a change of geometry), its
    /// generated fills' strokes with them: a fill that showed keeps showing.
    pub fn map_points(&mut self, f: impl Fn(Point) -> Point) {
        let fitted = self.generated.as_ref().is_some_and(|g| g.stroke == self.stroke_key());
        self.points.iter_mut().for_each(|p| *p = f(*p));
        let key = self.stroke_key();
        if let Some(g) = &mut self.generated {
            let remap = |g: &mut GenFill| {
                if let Some(at) = &mut g.made_for {
                    at.points.iter_mut().for_each(|p| *p = f(*p));
                    g.stroke = at.key();
                }
            };
            g.earlier.iter_mut().for_each(remap);
            remap(g);
            if fitted {
                g.stroke = key;
            }
        }
    }
}

/// What added a spot automatically.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AutoSpotKind {
    /// Sensor dust.
    Dust,
    /// Dust, a hair or a scratch on scanned film (Find Film Dust and Hair).
    FilmDust,
}

/// A spot an automatic finder added: what it is and how sure it was.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoSpot {
    pub kind: AutoSpotKind,
    /// The camera body's dust map has it (seen in several photos), rather than this photo alone.
    #[serde(default)]
    pub confirmed: bool,
    /// 0..1.
    #[serde(default)]
    pub confidence: f32,
}

/// A red eye / pet eye correction: the user's ellipse (centre normalized, radii as fractions of
/// the long edge); the pupil inside it is found automatically.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RedEye {
    pub center: Point,
    pub rx: f64,
    pub ry: f64,
    /// 0..100 (50 = the detected pupil).
    pub pupil_size: f64,
    /// 0..100.
    pub darken: f64,
    pub pet: bool,
    /// Pet eye catchlight: offset of its centre from the pupil centre, in pupil radii.
    #[serde(default)]
    pub catchlight: Option<Point>,
}

impl Default for RedEye {
    fn default() -> Self {
        Self { center: Point::new(0.5, 0.5), rx: 0.02, ry: 0.015, pupil_size: 50.0, darken: 50.0, pet: false, catchlight: None }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LensBlur {
    pub enabled: bool,
    pub amount: f64,
    pub focal_distance: f64,
    pub focal_range: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Enhance {
    pub denoise: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denoise_on: Option<bool>,
    pub raw_details: bool,
    pub super_resolution: bool,
}

impl Enhance {
    pub fn denoise_enabled(&self) -> bool {
        self.denoise_on.unwrap_or(self.denoise > 0.0)
    }
}

/// Which skin Smooth Skin works on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkinArea {
    /// The face's skin (never the eyes, brows, lips or facial hair).
    #[default]
    Face,
    /// Shoulders, arms, back, hands: skin that is not the face.
    Body,
    Both,
}

impl SkinArea {
    /// `face`, `body` or `both` (case-insensitive).
    pub fn parse(name: &str) -> Option<SkinArea> {
        match name.to_ascii_lowercase().as_str() {
            "face" | "face skin" => Some(SkinArea::Face),
            "body" | "body skin" => Some(SkinArea::Body),
            "both" | "all" | "face and body" => Some(SkinArea::Both),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            SkinArea::Face => "face",
            SkinArea::Body => "body",
            SkinArea::Both => "both",
        }
    }
}

/// Smooth Skin: frequency separation on the scene-linear image inside the skin the models found
/// ([`SkinAnalysis`]). Its bands are measured in each face's eye distance, so a preview and an
/// export look alike. All zero is off.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkinSmoothing {
    /// 0..100: softens the middle band (uneven texture, small bumps), keeping edges.
    pub amount: f64,
    /// −100..100: the fine band (pores). 0 keeps it as it is; below softens it, above brings it out.
    pub texture: f64,
    /// 0..100: evens out blotches of tone and colour (the low band).
    pub even_tone: f64,
    /// 0..100: reduces patches redder than the skin around them.
    pub redness: f64,
    /// 0..100: tones down shine (hot spots brighter than the skin around them) towards that skin.
    pub shine: f64,
    pub area: SkinArea,
    /// 0: everyone; n: the n-th person from the left (by their face), as People masks count.
    pub person: u32,
}

impl Default for SkinSmoothing {
    fn default() -> Self {
        Self { amount: 0.0, texture: 0.0, even_tone: 0.0, redness: 0.0, shine: 0.0, area: SkinArea::Face, person: 0 }
    }
}

impl SkinSmoothing {
    /// Most people Smooth Skin tells apart (Person 1 to 4, as the People mask offers).
    pub const MAX_PERSON: u32 = 4;
    /// The subtle starting point Smooth Skin is switched on with.
    pub const SUBTLE: SkinSmoothing =
        SkinSmoothing { amount: 35.0, texture: 0.0, even_tone: 25.0, redness: 20.0, shine: 0.0, area: SkinArea::Face, person: 0 };

    pub fn is_default(&self) -> bool {
        *self == SkinSmoothing::default()
    }

    /// Whether it changes anything (some slider away from zero).
    pub fn active(&self) -> bool {
        let on = |v: f64| v.is_finite() && v != 0.0;
        on(self.amount) || on(self.texture) || on(self.even_tone) || on(self.redness) || on(self.shine)
    }
}

/// A face Smooth Skin measures its bands by: its centre (normalized, of the uncropped, upright
/// photo) and the distance between its eyes (a fraction of the photo's long edge).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkinFace {
    pub x: f64,
    pub y: f64,
    pub eyes: f64,
}

/// Most faces a stored analysis keeps (a hostile document can't make rendering explode).
pub const MAX_SKIN_FACES: usize = 64;

/// What the models found for Smooth Skin on one photo: its skin (for [`SkinArea`] `area` and
/// `person`, with the photo turned to `orientation`) and its faces. Recomputed when any of
/// those changes; rendering never needs the models.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkinAnalysis {
    pub area: SkinArea,
    pub person: u32,
    pub orientation: Orientation,
    pub seg: crate::SegMask,
    #[serde(default, deserialize_with = "de_skin_faces")]
    pub faces: Vec<SkinFace>,
}

impl SkinAnalysis {
    /// Whether it was made for these settings on a photo turned to `orientation`.
    pub fn is_for(&self, s: &SkinSmoothing, orientation: Orientation) -> bool {
        self.area == s.area && self.person == s.person && self.orientation == orientation
    }
}

/// A value that doesn't read (damaged, or from a newer version) is its default: it costs that
/// value, never the photo's other settings.
fn lenient<'de, D: serde::Deserializer<'de>, T: Default + serde::de::DeserializeOwned>(d: D) -> Result<T, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

/// Keep at most [`MAX_SKIN_FACES`] faces (the rest are skipped unread).
fn de_skin_faces<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<SkinFace>, D::Error> {
    struct Capped;
    impl<'de> serde::de::Visitor<'de> for Capped {
        type Value = Vec<SkinFace>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a list of faces")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<SkinFace>, A::Error> {
            let mut out = Vec::new();
            while out.len() < MAX_SKIN_FACES {
                match seq.next_element::<SkinFace>()? {
                    Some(f) => out.push(f),
                    None => return Ok(out),
                }
            }
            while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
            Ok(out)
        }
    }
    d.deserialize_seq(Capped)
}

/// HDR editing. When enabled, an HDR render ([`Hdr::peak`]) keeps highlights above SDR white (1.0)
/// up to `2^max_ev`; every SDR render (previews on SDR displays, 8/16-bit exports and a gain map's
/// base image) uses the SDR rendition ([`DevelopSettings::sdr_rendition`]): the same edit with the
/// `sdr_*` offsets applied.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hdr {
    pub enabled: bool,
    /// Headroom limit: stops above SDR white the HDR render may reach.
    pub max_ev: f64,
    /// SDR rendition offsets (−100..100): brightness (±1 EV), then added to the matching sliders.
    pub sdr_brightness: f64,
    pub sdr_contrast: f64,
    pub sdr_highlights: f64,
    pub sdr_shadows: f64,
    pub sdr_whites: f64,
    pub sdr_clarity: f64,
    /// SDR blend (−100..100, own design; see [`DevelopSettings::sdr_rendition`]): positive values fit more
    /// of the HDR highlight range into the SDR rendition (highlights and whites pulled down in proportion to
    /// the headroom limit), negative ones clip it harder (highlights and whites pushed up).
    pub sdr_blend: f64,
    /// Set only on an SDR rendition ([`DevelopSettings::sdr_rendition`]): the HDR edit's peak
    /// ([`Hdr::peak`]), whose highlights the SDR tone map rolls off into SDR white instead of
    /// clipping them (see `lcc_pipeline::tone::ToneMap::sdr_rolloff`). 1 (the default) = none.
    /// Never saved or read: settings files, presets and agents can't set it.
    #[serde(skip, default = "Hdr::no_rolloff")]
    pub rolloff_peak: f32,
}

impl Default for Hdr {
    fn default() -> Self {
        Self {
            enabled: false,
            max_ev: Hdr::DEFAULT_MAX_EV,
            sdr_brightness: 0.0,
            sdr_contrast: 0.0,
            sdr_highlights: 0.0,
            sdr_shadows: 0.0,
            sdr_whites: 0.0,
            sdr_clarity: 0.0,
            sdr_blend: 0.0,
            rolloff_peak: 1.0,
        }
    }
}

impl Hdr {
    pub const DEFAULT_MAX_EV: f64 = 3.0;
    pub const MAX_EV_LIMIT: f64 = 5.0;

    /// Whether these are the default HDR settings (HDR off, nothing changed).
    pub fn is_default(&self) -> bool {
        *self == Hdr::default()
    }

    fn no_rolloff() -> f32 {
        1.0
    }

    /// The HDR peak an SDR rendition rolls its highlights off from: 1 (none) unless this is the
    /// rendition of an HDR edit. Damaged values (non-finite, below 1) mean none; capped at the
    /// largest HDR peak.
    pub fn rolloff_peak(&self) -> f32 {
        if self.enabled || !self.rolloff_peak.is_finite() {
            return 1.0;
        }
        self.rolloff_peak.clamp(1.0, (Hdr::MAX_EV_LIMIT as f32).exp2())
    }

    /// Peak linear value of an HDR render relative to SDR white (1.0 when HDR is off). A non-finite headroom
    /// (a damaged file or argument) falls back to the default, so the peak is always finite and at least 1.
    pub fn peak(&self) -> f32 {
        if !self.enabled {
            return 1.0;
        }
        let ev = if self.max_ev.is_finite() { self.max_ev } else { Hdr::DEFAULT_MAX_EV };
        (ev.clamp(0.0, Hdr::MAX_EV_LIMIT) as f32).exp2()
    }
}

impl DevelopSettings {
    /// The settings an SDR render uses: unchanged when HDR is off; otherwise HDR off and the
    /// SDR rendition offsets applied to exposure, contrast, highlights, shadows, whites, clarity,
    /// and [`Hdr::rolloff_peak`] set so the tone map compresses the HDR highlights into SDR.
    pub fn sdr_rendition(&self) -> std::borrow::Cow<'_, DevelopSettings> {
        if !self.hdr.enabled {
            return std::borrow::Cow::Borrowed(self);
        }
        let h = self.hdr;
        let mut s = self.clone();
        s.hdr.rolloff_peak = h.peak();
        s.hdr.enabled = false;
        let add = |v: f64, d: f64| (v + d).clamp(-100.0, 100.0);
        s.light.exposure = (s.light.exposure + h.sdr_brightness / 100.0).clamp(-5.0, 5.0);
        s.light.contrast = add(s.light.contrast, h.sdr_contrast);
        s.light.highlights = add(s.light.highlights, h.sdr_highlights);
        s.light.shadows = add(s.light.shadows, h.sdr_shadows);
        s.light.whites = add(s.light.whites, h.sdr_whites);
        s.effects.clarity = add(s.effects.clarity, h.sdr_clarity);
        // SDR blend: scaled by the headroom (stops, relative to the default limit) so a larger HDR range
        // takes a stronger compression; damaged values contribute nothing
        let blend = if h.sdr_blend.is_finite() { h.sdr_blend.clamp(-100.0, 100.0) } else { 0.0 };
        let ev = if h.max_ev.is_finite() { h.max_ev.clamp(0.0, Hdr::MAX_EV_LIMIT) } else { Hdr::DEFAULT_MAX_EV };
        let k = blend * (ev / Hdr::DEFAULT_MAX_EV).min(1.5);
        s.light.highlights = add(s.light.highlights, -0.6 * k);
        s.light.whites = add(s.light.whites, -0.3 * k);
        std::borrow::Cow::Owned(s)
    }
}
