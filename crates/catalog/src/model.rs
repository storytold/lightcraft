//! Catalog records.

use std::sync::Arc;

use lcc_develop::{DevelopSettings, WbMode};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PhotoId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AlbumId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StackId(pub u64);

/// A stack: photos grouped under a top photo (bursts, brackets, merge results with their sources).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stack {
    pub id: StackId,
    /// Members in stack order; `photos[0]` is the top of the stack. At least two.
    pub photos: Vec<PhotoId>,
    /// Collapsed stacks show only their top photo in the grid.
    #[serde(default)]
    pub collapsed: bool,
}

impl Stack {
    pub fn top(&self) -> PhotoId {
        self.photos[0]
    }
    pub fn position(&self, id: PhotoId) -> Option<usize> {
        self.photos.iter().position(|p| *p == id)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    #[default]
    Image,
    Raw,
    Video,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Flag {
    #[default]
    None,
    Pick,
    Reject,
}

impl Flag {
    pub fn parse(s: &str) -> Option<Flag> {
        match s.to_ascii_lowercase().as_str() {
            "pick" | "picked" | "flagged" => Some(Flag::Pick),
            "reject" | "rejected" => Some(Flag::Reject),
            "none" | "unflagged" | "" => Some(Flag::None),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorLabel {
    Red,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl ColorLabel {
    pub const ALL: [ColorLabel; 5] = [ColorLabel::Red, ColorLabel::Yellow, ColorLabel::Green, ColorLabel::Blue, ColorLabel::Purple];
    pub fn parse(s: &str) -> Option<ColorLabel> {
        ColorLabel::ALL.into_iter().find(|c| format!("{c:?}").eq_ignore_ascii_case(s))
    }
}

/// Where the pixels come from.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Source {
    /// A file on disk (native) or in the browser's storage (web).
    File { path: String },
    /// A procedurally generated demo scene (by `lcc-scenes` id).
    Demo { scene: u32 },
}

/// Copyright status (IPTC / XMP Rights Management `xmpRights:Marked`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CopyrightStatus {
    /// Not stated (no `xmpRights:Marked`).
    #[default]
    Unknown,
    /// `xmpRights:Marked` = True.
    Copyrighted,
    /// `xmpRights:Marked` = False.
    PublicDomain,
}

impl CopyrightStatus {
    pub const ALL: [CopyrightStatus; 3] = [CopyrightStatus::Unknown, CopyrightStatus::Copyrighted, CopyrightStatus::PublicDomain];
    /// `unknown` / `copyrighted` / `publicDomain` (also `public domain`, `public-domain`).
    pub fn parse(s: &str) -> Option<CopyrightStatus> {
        match s.trim().to_ascii_lowercase().replace([' ', '-', '_'], "").as_str() {
            "unknown" | "" => Some(CopyrightStatus::Unknown),
            "copyrighted" => Some(CopyrightStatus::Copyrighted),
            "publicdomain" => Some(CopyrightStatus::PublicDomain),
            _ => None,
        }
    }
    /// The id used by commands (`photo.setMeta`'s `copyrightStatus`).
    pub fn id(self) -> &'static str {
        match self {
            CopyrightStatus::Unknown => "unknown",
            CopyrightStatus::Copyrighted => "copyrighted",
            CopyrightStatus::PublicDomain => "publicDomain",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            CopyrightStatus::Unknown => "Unknown",
            CopyrightStatus::Copyrighted => "Copyrighted",
            CopyrightStatus::PublicDomain => "Public Domain",
        }
    }
    /// As `xmpRights:Marked`: `Some(true)` copyrighted, `Some(false)` public domain.
    pub fn marked(self) -> Option<bool> {
        match self {
            CopyrightStatus::Unknown => None,
            CopyrightStatus::Copyrighted => Some(true),
            CopyrightStatus::PublicDomain => Some(false),
        }
    }
    pub fn from_marked(m: Option<bool>) -> CopyrightStatus {
        match m {
            None => CopyrightStatus::Unknown,
            Some(true) => CopyrightStatus::Copyrighted,
            Some(false) => CopyrightStatus::PublicDomain,
        }
    }
    pub fn is_unknown(&self) -> bool {
        *self == CopyrightStatus::Unknown
    }
}

/// A shutter speed as cameras write it (`1/250`, `0.5`, `2″`, `2"`, `30s`) in seconds; `None` for
/// anything that isn't a positive, finite time. Both primes of a whole second parse: `″` (U+2033) is what
/// [`lcc_meta::Metadata::exposure_display`] writes into the catalog, `"` what a user types.
pub fn parse_shutter_seconds(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches(['s', 'S']).trim().trim_end_matches(['″', '"', '\'', '”']).trim();
    match s.split_once('/') {
        Some((n, d)) => Some(n.trim().parse::<f64>().ok()? / d.trim().parse::<f64>().ok()?),
        None => s.parse().ok(),
    }
    .filter(|v: &f64| v.is_finite() && *v > 0.0)
}

/// Descriptive + capture metadata.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    pub camera: String,
    pub lens: String,
    pub focal_mm: Option<f32>,
    pub aperture: Option<f32>,
    pub shutter: String,
    pub iso: Option<u32>,
    /// Place (sublocation, e.g. a landmark).
    pub location: String,
    pub city: String,
    pub state: String,
    pub country: String,
    pub gps: Option<(f64, f64)>,
    pub title: String,
    pub caption: String,
    /// Accessibility text and long description.
    pub alt_text: String,
    pub extended_description: String,
    pub copyright: String,
    /// Copyright status, rights usage terms and copyright info URL (IPTC Core rights fields).
    #[serde(skip_serializing_if = "CopyrightStatus::is_unknown")]
    pub copyright_status: CopyrightStatus,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub usage_terms: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub copyright_url: String,
    pub creator: String,
    pub keywords: Vec<String>,
    /// Face/pet/focus regions read from XMP (MWG-RS), on the upright (EXIF-oriented) photo.
    /// Removing or resizing one edits the catalog only; the app never writes regions to XMP.
    /// Left out of the catalog JSON when empty (most photos), so older catalogs read unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<lcc_meta::Region>,
    /// Where the camera focused (read from the file's maker notes at import or Reload from Disk; Sony
    /// bodies whose files have been checked), on the upright (EXIF-oriented) photo. Read-only: never
    /// written to XMP. Left out of the catalog JSON when unknown, so older catalogs read unchanged.
    /// Catalog format 12.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<lcc_meta::FocusPoint>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Version {
    pub name: String,
    pub created: String,
    #[serde(with = "crate::settings_ref")]
    pub settings: Arc<DevelopSettings>,
    #[serde(default)]
    pub auto: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryStep {
    pub label: String,
    #[serde(with = "crate::settings_ref")]
    pub settings: Arc<DevelopSettings>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Photo {
    pub id: PhotoId,
    pub source: Source,
    pub file_name: String,
    #[serde(default)]
    pub kind: MediaKind,
    pub format: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub file_size: u64,
    /// ISO 8601 local capture time.
    #[serde(default)]
    pub captured: Option<String>,
    /// The file's modification time, when import dated the photo by it because neither the file nor
    /// its sidecar had a capture time (as Lightroom does). While [`Photo::captured`] is still this
    /// guess, it is not a capture time anyone stated: it is never written to XMP, and a capture time
    /// read from a sidecar later replaces it ([`Photo::captured_stated`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_fallback: Option<String>,
    pub imported: String,
    #[serde(default)]
    pub meta: Meta,
    #[serde(default)]
    pub rating: u8,
    #[serde(default)]
    pub flag: Flag,
    #[serde(default)]
    pub label: Option<ColorLabel>,
    #[serde(with = "crate::settings_ref")]
    pub develop: Arc<DevelopSettings>,
    #[serde(default)]
    pub edited: Option<String>,
    #[serde(default)]
    pub versions: Vec<Version>,
    #[serde(default)]
    pub history: Vec<HistoryStep>,
    /// The History step the photo's settings are at, when it isn't the latest one (Go to History
    /// Step moved back): later steps stay listed and can be returned to until a new edit drops
    /// them. `None` (and catalogs written before the cursor existed) = the latest step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_cursor: Option<usize>,
    /// In "Recently Deleted".
    #[serde(default)]
    pub deleted: bool,
    /// Seen while browsing a folder on disk (Local), not added to the library: only folder views
    /// show it until it is added.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local: bool,
    /// Video duration in seconds.
    #[serde(default)]
    pub duration: Option<f64>,
    /// Raw files: the camera's as-shot white balance (Kelvin, tint).
    #[serde(default)]
    pub as_shot_wb: Option<(f64, f64)>,
    /// Hash of the file's bytes (hex), for duplicate detection and preview-cache keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Lens corrections embedded in the file (DNG `WarpRectilinear` / `FixVignetteRadial`), applied when
    /// "Enable Profile Corrections" is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedded_lens: Option<Box<lcc_develop::EmbeddedLens>>,
    /// A virtual copy: the photo it was copied from (it shares that photo's file but has its own
    /// settings, metadata and history).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_of: Option<PhotoId>,
    /// Virtual copies' name ("Copy 1").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_name: Option<String>,
    /// The settings import gave the photo when a user default (a raw/JPEG default preset, see
    /// the engine's import defaults) changed them from [`Photo::camera_defaults`]. They count as
    /// unedited, and Reset returns to them.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "crate::settings_ref::opt")]
    pub import_look: Option<Arc<DevelopSettings>>,
    /// Assisted culling scores (`None` until analysed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis: Option<Analysis>,
    /// A raw file whose sensor data can't be decoded yet (an unsupported raw variant): why (the
    /// raw decoder's reason, e.g. "Nikon Huffman-compressed NEF …"). The photo is shown and
    /// edited from the camera's embedded JPEG preview — a rendered image with the camera's
    /// picture style baked in — so it is treated as a rendered (non-raw) source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_only: Option<String>,
    /// The app version (`CARGO_PKG_VERSION`) whose decoder set [`Photo::preview_only`]; empty when
    /// it isn't set, and in catalogs from before format v12. A preview-only photo whose stamp
    /// differs from the running version is rechecked when its library opens (upstream issue #662):
    /// newer builds may decode the file, older ones may decode less.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preview_checked: String,
    /// A Local record: the fingerprint of its state when the browse catalogued it, to tell
    /// whether the user changed anything since (see [`crate::local`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_baseline: Option<u64>,
    /// SHA-1 of the original file's bytes (hex), as remote services such as Immich identify
    /// assets by it. Catalog format 4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    /// The state at the last read from / write to the XMP sidecar (see [`crate::xmp_state`]).
    /// Catalog format 4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xmp: Option<crate::xmp_state::XmpStamp>,
    /// Plug-in custom metadata fields, `"<plug-in id>/<field key>"` → value: an in-memory index of
    /// what the plug-ins' own data (the source) holds for the photo, so rules can search them and
    /// sidecars carry them. Never saved with the catalog (the format doesn't change); the engine
    /// rebuilds it when a library opens and after plug-in calls ([`crate::Catalog::set_plugin_fields`]).
    #[serde(skip)]
    pub plugin_fields: std::collections::BTreeMap<String, serde_json::Value>,
    /// A video clip's stream facts, trim and poster frame (`None` for stills, and for clips imported
    /// before the video module). Catalog format 10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoMeta>,
}

/// A video clip: what its file holds (from a probe) plus the user's trim and poster frame.
/// Times are seconds from the clip's start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoMeta {
    pub duration: f64,
    pub fps_num: u32,
    pub fps_den: u32,
    pub width: u32,
    pub height: u32,
    /// e.g. "h264", "hevc".
    pub codec: String,
    #[serde(default)]
    pub has_audio: bool,
    /// Trim in point; 0.0 = the start.
    #[serde(default)]
    pub trim_in: f64,
    /// Trim out point; `None` = the end.
    #[serde(default)]
    pub trim_out: Option<f64>,
    /// The poster (thumbnail) frame's time; `None` = [`VideoMeta::default_poster_time`].
    #[serde(default)]
    pub poster_time: Option<f64>,
}

impl VideoMeta {
    /// Frames per second (`None` when the rate is unknown, `fps_den == 0`).
    pub fn fps(&self) -> Option<f64> {
        (self.fps_den != 0 && self.fps_num != 0).then(|| f64::from(self.fps_num) / f64::from(self.fps_den))
    }
    /// The trimmed range's end: `trim_out`, or the clip's duration.
    pub fn trim_end(&self) -> f64 {
        self.trim_out.unwrap_or(self.duration)
    }
    /// 10 % into the clip.
    pub fn default_poster_time(&self) -> f64 {
        self.duration * 0.1
    }
    /// The poster frame's time: the chosen one or the default.
    pub fn poster(&self) -> f64 {
        self.poster_time.unwrap_or_else(|| self.default_poster_time())
    }
    /// Checks the facts and trim/poster: finite, non-negative duration, `0 <= in < out <= duration`, poster
    /// within the clip. An error message, never a panic.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if !(self.duration.is_finite() && self.duration >= 0.0) {
            return Err("video duration is not a non-negative number".into());
        }
        validate_trim(self.duration, self.trim_in, self.trim_out)?;
        validate_poster(self.duration, self.poster_time)
    }
}

/// `0 <= trim_in < trim_out <= duration`, all finite.
pub fn validate_trim(duration: f64, trim_in: f64, trim_out: Option<f64>) -> std::result::Result<(), String> {
    if !trim_in.is_finite() || trim_out.is_some_and(|o| !o.is_finite()) {
        return Err("video trim is not a number".into());
    }
    let out = trim_out.unwrap_or(duration);
    if trim_in < 0.0 || trim_in >= out || out > duration {
        return Err(format!("video trim {trim_in}..{out} s is outside 0..{duration} s (in must be before out)"));
    }
    Ok(())
}

/// A poster time within `0..=duration`, finite.
pub fn validate_poster(duration: f64, time: Option<f64>) -> std::result::Result<(), String> {
    match time {
        Some(t) if !t.is_finite() => Err("video poster time is not a number".into()),
        Some(t) if t < 0.0 || t > duration => Err(format!("video poster time {t} s is outside 0..{duration} s")),
        _ => Ok(()),
    }
}

/// What assisted culling measured on a photo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    /// Focus: 0 (blurred) .. 100 (crisp), from the detail in the photo's sharpest area.
    pub sharpness: f32,
    /// Exposure: share of clipped pixels, 0..1 (shadows + highlights).
    pub clipped: f32,
    /// Similar-shot group (burst), when the photo has neighbours that look alike.
    pub group: Option<u32>,
    /// The best photo of its group.
    pub best: bool,
}

/// Raw formats whose files carry vendor white-balance multipliers but no colour matrices: their starting
/// look is fitted to the file's own JPEG (the engine's `camera_preview::file_local_look` lists the same
/// formats). `format` is the file's extension or Lightroom's `fileFormat` name, in any case. Whether such a
/// file's white balance is edited relative to its as-shot look or in Kelvin depends on the camera:
/// [`relative_wb_camera`].
pub fn relative_wb_format(format: &str) -> bool {
    ["ARW", "NEF", "NRW", "RW2", "RWL", "RAW", "RAF", "CR3", "CR2", "PEF", "SRW", "ORF", "ERF", "CRW", "MRW"]
        .iter()
        .any(|f| format.eq_ignore_ascii_case(f))
}

/// Whether a raw of `format` shot on `camera` (Exif make and model, as [`Meta::camera`] keeps them) is developed
/// with a white balance relative to its as-shot look (6500 K / 0 means as shot, as for rendered photographs):
/// a file without colour matrices ([`relative_wb_format`]) whose camera has none measured either, nor
/// white-balance presets to read Kelvin from. A camera with matrices in `lcc_raw::spectral` edits in Kelvin:
/// the matrices turn its as-shot multipliers into a temperature, and the slider back, while its colour look stays
/// whatever the file's fit gives (upstream #730). A camera in `lcc_raw::kelvin_presets` does the same through
/// its own presets. The engine's render path (`files.rs`) decides the same way from the file; both must agree.
pub fn relative_wb_camera(format: &str, camera: &str) -> bool {
    relative_wb_format(format) && lcc_raw::spectral::find_camera(camera).is_none() && !lcc_raw::kelvin_presets::covers_camera(camera)
}

/// This build's version: what a photo's [`Photo::preview_checked`] stamp is compared with. Every
/// workspace crate shares one version, so it is the app's.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The [`Photo::preview_checked`] stamp for a `preview_only` decided by this build: its version
/// when the photo is preview-only, empty otherwise.
pub fn preview_stamp(preview_only: &Option<String>) -> String {
    if preview_only.is_some() { APP_VERSION.to_string() } else { String::new() }
}

impl Photo {
    /// The History step the settings are at (the cursor, clamped to the list; `None` = no history).
    pub fn history_index(&self) -> Option<usize> {
        let last = self.history.len().checked_sub(1)?;
        Some(self.history_cursor.map_or(last, |c| c.min(last)))
    }

    pub fn new(id: PhotoId, source: Source, file_name: &str, format: &str, width: u32, height: u32, imported: &str) -> Photo {
        Photo {
            id,
            source,
            file_name: file_name.to_string(),
            kind: MediaKind::Image,
            format: format.to_string(),
            width,
            height,
            file_size: 0,
            captured: None,
            captured_fallback: None,
            imported: imported.to_string(),
            meta: Meta::default(),
            rating: 0,
            flag: Flag::None,
            label: None,
            develop: Arc::new(DevelopSettings::default()),
            edited: None,
            versions: Vec::new(),
            history: Vec::new(),
            history_cursor: None,
            deleted: false,
            local: false,
            duration: None,
            as_shot_wb: None,
            content_hash: None,
            embedded_lens: None,
            copy_of: None,
            copy_name: None,
            import_look: None,
            analysis: None,
            preview_only: None,
            preview_checked: String::new(),
            local_baseline: None,
            sha1: None,
            xmp: None,
            plugin_fields: Default::default(),
            video: None,
        }
    }
    /// [`Photo::captured`], unless it is still the file-date guess import made
    /// ([`Photo::captured_fallback`]): a capture time the file, a sidecar or the user stated.
    pub fn captured_stated(&self) -> Option<&str> {
        self.captured.as_deref().filter(|c| self.captured_fallback.as_deref() != Some(*c))
    }
    /// Sets [`Photo::preview_only`] as this build decided it, with its stamp.
    pub fn set_preview_only(&mut self, preview_only: Option<String>) {
        self.preview_checked = preview_stamp(&preview_only);
        self.preview_only = preview_only;
    }
    /// A preview-only photo whose flag was set by another app version (or before stamps existed),
    /// so this build should probe its file again (upstream issue #662).
    pub fn preview_only_stale(&self) -> bool {
        self.preview_only.is_some() && self.preview_checked != APP_VERSION
    }
    /// A raw file developed from its sensor data: not a rendered image, and not a raw shown from
    /// its embedded preview ([`Photo::preview_only`]).
    pub fn develops_raw(&self) -> bool {
        self.kind == MediaKind::Raw && self.preview_only.is_none()
    }
    /// A raw developed from a look fitted to its own JPEG (ARW, NEF, RW2, RAF, CR3, CR2, PEF, SRW, ORF: vendor
    /// WB multipliers, no colour matrices in the file; RWL and RAW are Leica's and the oldest Panasonic bodies'
    /// names for RW2 files). The engine's `camera_preview::file_local_look` covers the same formats.
    pub fn fitted_look(&self) -> bool {
        self.develops_raw() && relative_wb_format(&self.format)
    }
    /// White balance relative to the camera's as-shot look, as for rendered photographs: a
    /// [`Photo::fitted_look`] raw whose camera has no colour matrices measured either
    /// ([`relative_wb_camera`]). Other raws edit white balance in Kelvin.
    pub fn relative_wb(&self) -> bool {
        self.develops_raw() && relative_wb_camera(&self.format, &self.meta.camera)
    }
    /// The develop settings import gave this photo: [`Photo::camera_defaults`], or the user's
    /// default preset applied on top of them ([`Photo::import_look`]). Like the camera defaults,
    /// in the photo's own rendering process.
    pub fn import_defaults(&self) -> DevelopSettings {
        match &self.import_look {
            Some(l) => DevelopSettings { process: self.develop.process, ..(**l).clone() },
            None => self.camera_defaults(),
        }
    }
    /// The built-in defaults for this photo, before any user default preset: raws start from
    /// their as-shot white balance; embedded lens corrections on when the file has them. They
    /// carry the photo's own rendering process ([`lcc_develop::ProcessVersion`]): which
    /// process a photo renders with is not an edit, so comparisons with its defaults (edited?
    /// still as imported?) don't change when a newer process exists. A new photo has the latest.
    pub fn camera_defaults(&self) -> DevelopSettings {
        let relative = self.relative_wb();
        let wb = if relative { Some((6500.0, 0.0)) } else { self.as_shot_wb };
        let mut d = match wb {
            Some((t, tint)) => DevelopSettings::for_raw(t, tint),
            None => DevelopSettings::default(),
        };
        if self.develops_raw() && !relative {
            d.wb.scale = lcc_develop::WbScale::Kelvin;
        }
        if self.embedded_lens.is_some() {
            d.optics.lens_profile = true;
        }
        d.process = self.develop.process;
        d
    }
    /// The file name's extension without the dot, as written (`CR2` for `IMG_0042.CR2`); empty
    /// when the name has none (`README`, a dot file such as `.hidden`).
    pub fn extension(&self) -> &str {
        self.file_name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()).map_or("", |(_, ext)| ext)
    }
    /// Width and height in pixels as the photo is shown: turned by its orientation and cut by its
    /// crop. Straightening and Constrain Crop shrink the crop without changing its shape, so the
    /// aspect is exact and the edges can read a little larger than an export.
    pub fn shown_size(&self) -> (f64, f64) {
        let (w, h) = (self.width as f64, self.height as f64);
        let (w, h) = if self.develop.orientation.swaps_axes() { (h, w) } else { (w, h) };
        let r = self.develop.crop.geometry.rect;
        // a crop read from a file or an agent is hostile: no NaN, nothing outside the frame
        let part = |len: f64| if len.is_finite() { len.clamp(0.0, 1.0) } else { 1.0 };
        (w * part(r.width()), h * part(r.height()))
    }
    /// Cropped or straightened.
    pub fn is_cropped(&self) -> bool {
        !self.develop.crop.geometry.is_identity()
    }
    /// How many keywords the photo has: a hierarchical keyword (`travel|italy|rome`) is one, and
    /// the same keyword in different case or with stray spaces counts once; blank ones don't count.
    pub fn keyword_count(&self) -> usize {
        let mut seen: Vec<String> = self.meta.keywords.iter().map(|k| k.trim().to_lowercase()).filter(|k| !k.is_empty()).collect();
        seen.sort_unstable();
        seen.dedup();
        seen.len()
    }
    /// The people in the photo: the names on its face regions, trimmed, each once (names that
    /// differ only in case are one person, as first seen). Pets and unnamed faces are not people.
    pub fn people(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for r in self.meta.regions.iter().filter(|r| r.kind == lcc_meta::RegionKind::Face) {
            let Some(name) = r.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) else { continue };
            if !out.iter().any(|o| o.to_lowercase() == name.to_lowercase()) {
                out.push(name);
            }
        }
        out
    }
    /// Number of detected face regions on the photo (named or unnamed).
    pub fn face_count(&self) -> usize {
        self.meta.regions.iter().filter(|r| r.kind == lcc_meta::RegionKind::Face).count()
    }
    /// In the library: not deleted and not only browsed (Local).
    pub fn in_library(&self) -> bool {
        !self.deleted && !self.local
    }
    /// Edited by the user: settings differ from what import gave the photo.
    pub fn is_edited(&self) -> bool {
        let mut defaults = self.import_defaults();
        // As Shot reads the source's current WB; stored numbers can predate a decoder update.
        if self.develop.wb.mode == WbMode::AsShot && defaults.wb.mode == WbMode::AsShot {
            defaults.wb = self.develop.wb;
        }
        *self.develop != defaults && !self.develop.is_unedited()
    }
    /// Capture time if known, else import time (sort key).
    pub fn date(&self) -> &str {
        self.captured.as_deref().unwrap_or(&self.imported)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Album {
    pub id: AlbumId,
    pub name: String,
    /// Containing folder (an album with `folder = true`).
    #[serde(default)]
    pub parent: Option<AlbumId>,
    /// Folders hold albums, not photos.
    #[serde(default)]
    pub folder: bool,
    #[serde(default)]
    pub photos: Vec<PhotoId>,
    #[serde(default)]
    pub cover: Option<PhotoId>,
    /// A smart album: its photos are every (non-deleted) photo matching these rules, evaluated
    /// live; `photos` stays empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smart: Option<Box<crate::Filter>>,
    /// The Quick Collection: a regular album that B adds to (one per library).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quick: bool,
    /// Its place among the albums of the same kind in its folder, once the user ordered them by
    /// hand (see [`crate::Catalog::album_children`]); `None`: listed by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<u32>,
    /// A saved creation (Saved Print / Book / Slideshow / Web Gallery): the album holds its photos
    /// and this its layout document. Format version 7.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation: Option<Creation>,
    /// Custom Order: the photos in the order the user set by hand ([`crate::SortKey::Custom`],
    /// format version 9). Only members; photos added later (not listed) follow in the order they
    /// were added. Empty: no custom order yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub photo_order: Vec<PhotoId>,
}

/// The layout attached to a saved-creation album.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Creation {
    /// `print`, `book`, `slideshow` or `web` (the layout crate's kind names).
    pub kind: String,
    /// The layout document as JSON (the layout crate's `Document`; opaque to the catalog).
    pub document: String,
}

/// Largest layout document stored in an album (bytes).
pub const MAX_CREATION_BYTES: usize = 16 * 1024 * 1024;
/// The kinds of saved creation.
pub const CREATION_KINDS: &[&str] = &["print", "book", "slideshow", "web"];

impl Album {
    /// A regular (manual) album.
    pub fn new(id: AlbumId, name: impl Into<String>) -> Album {
        Album {
            id,
            name: name.into(),
            parent: None,
            folder: false,
            photos: Vec::new(),
            cover: None,
            smart: None,
            quick: false,
            order: None,
            creation: None,
            photo_order: Vec::new(),
        }
    }
    pub fn is_creation(&self) -> bool {
        self.creation.is_some()
    }
    pub fn is_smart(&self) -> bool {
        self.smart.is_some()
    }
}

#[cfg(test)]
mod edited_tests {
    use super::*;
    use lcc_develop::WbScale;

    #[test]
    fn raw_with_import_settings_is_not_edited() {
        let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.arw", "ARW", 10, 10, "2026-10-01T00:00:00");
        p.as_shot_wb = Some((5200.0, 4.0));
        p.develop = Arc::new(p.import_defaults());
        assert!(!p.is_edited(), "import look counts as unedited");
        let mut d = (*p.develop).clone();
        d.light.exposure = 0.5;
        p.develop = Arc::new(d);
        assert!(p.is_edited());
        p.develop = Arc::new(DevelopSettings::default());
        assert!(!p.is_edited(), "a full reset is unedited too");
    }

    #[test]
    fn the_process_a_photo_renders_with_is_not_an_edit() {
        use lcc_develop::ProcessVersion;
        // a photo on another process than the latest (a V1 photo once a later process is the
        // latest, or a photo saved by a newer version of the app)
        let other = ProcessVersion(ProcessVersion::LATEST.0 + 1);
        let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.dng", "DNG", 10, 10, "2026-10-01T00:00:00");
        p.kind = MediaKind::Raw;
        p.as_shot_wb = Some((5200.0, 4.0));
        assert_eq!(p.camera_defaults().process, ProcessVersion::LATEST, "a new photo gets the latest");
        p.develop = Arc::new(DevelopSettings { process: other, ..p.import_defaults() });
        assert_eq!((p.camera_defaults().process, p.import_defaults().process), (other, other));
        assert!(!p.is_edited(), "still as imported");
        // with a default preset's look as well
        let mut look = DevelopSettings::for_raw(5200.0, 4.0);
        look.light.exposure = 0.3;
        p.import_look = Some(Arc::new(look.clone()));
        p.develop = Arc::new(DevelopSettings { process: other, ..look });
        assert_eq!(p.import_defaults().process, other);
        assert!(!p.is_edited());
        let mut d = (*p.develop).clone();
        d.light.exposure = 1.0;
        p.develop = Arc::new(d);
        assert!(p.is_edited(), "an edit still is one");
    }

    #[test]
    fn supported_raws_use_relative_white_balance() {
        for (name, format, relative) in [
            ("a.arw", "ARW", true),
            ("a.nef", "NEF", true),
            ("a.nrw", "nrw", true),
            ("a.rw2", "RW2", true),
            ("a.rwl", "RWL", true),
            ("a.raw", "RAW", true),
            ("a.raf", "rAf", true),
            ("a.cr3", "cr3", true),
            ("a.srw", "SRW", true),
            ("a.cr2", "CR2", true),
            ("a.pef", "pef", true),
            ("a.orf", "ORF", true),
            ("a.erf", "ERF", true),
            ("a.crw", "CRW", true),
            ("a.mrw", "MRW", true),
            ("a.mef", "MEF", false),
            ("a.dng", "DNG", false),
        ] {
            let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, name, format, 10, 10, "2026-10-01T00:00:00");
            p.kind = MediaKind::Raw;
            p.meta.camera = "Pentax K-1".into();
            p.as_shot_wb = Some((5200.0, 4.0));
            assert_eq!(p.relative_wb(), relative, "{format}");
            assert_eq!(p.fitted_look(), relative, "{format}");
            let wb = p.camera_defaults().wb;
            assert_eq!((wb.temp, wb.tint), if relative { (6500.0, 0.0) } else { (5200.0, 4.0) }, "{format}");
            assert_eq!(wb.scale, if relative { WbScale::Legacy } else { WbScale::Kelvin }, "{format}");
            p.develop = Arc::new(DevelopSettings::for_raw(5200.0, 4.0));
            assert!(!p.is_edited(), "{format}: old As Shot values are not an edit");
            let mut custom = (*p.develop).clone();
            custom.wb.mode = WbMode::Custom;
            p.develop = Arc::new(custom);
            assert!(p.is_edited(), "{format}: Custom WB is still an edit");
            // shown from the embedded preview: a rendered image, not a relative-WB raw
            p.preview_only = Some("unsupported".into());
            assert!(!p.relative_wb() && !p.fitted_look());
        }
    }

    /// A raw without matrices in the file edits in Kelvin when its camera's matrices are measured (upstream #730):
    /// the decision reads the catalog's camera string, and the photo's defaults say which scale they are on.
    #[test]
    fn raws_of_cameras_with_measured_matrices_use_kelvin_white_balance() {
        for (camera, format, relative) in [
            ("SONY ILCE-7M3", "ARW", false),
            ("SONY ILCE-7RM4A", "ARW", false),
            ("NIKON CORPORATION NIKON D850", "NEF", false),
            ("Canon Canon EOS 5D Mark III", "CR2", false),
            ("Canon Canon EOS R6m2", "CR3", false),
            ("Canon Canon EOS R6 Mark II", "CR3", false),
            ("Canon Canon EOS R7", "CR3", true),
            ("FUJIFILM X-T4", "RAF", false),
            ("ILCE-7M3", "ARW", false),
            ("SONY ILCE-7M2", "ARW", false),
            ("SONY ILCA-99M2", "ARW", false),
            ("SONY SLT-A33", "ARW", false),
            ("SONY ILCE-1", "ARW", false),
            ("SONY ILCE-1M2", "ARW", false),
            ("SONY ZV-E10M2", "ARW", false),
            // the Sony bodies without presets, and a Sony sensor sold under another name
            ("SONY DSLR-A100", "ARW", true),
            ("HASSELBLAD Lunar", "ARW", true),
            ("", "ARW", true),
            ("SONY ILCE-7M3", "DNG", false),
        ] {
            let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.raw", format, 10, 10, "2026-10-01T00:00:00");
            p.kind = MediaKind::Raw;
            p.meta.camera = camera.into();
            p.as_shot_wb = Some((4986.0, -2.0));
            assert_eq!(p.relative_wb(), relative, "{camera} {format}");
            assert_eq!(relative_wb_camera(format, camera), relative, "{camera} {format}");
            assert_eq!(
                p.fitted_look(),
                format == "ARW" || format == "NEF" || format == "CR2" || format == "CR3" || format == "RAF",
                "{camera} {format}"
            );
            let wb = p.camera_defaults().wb;
            assert_eq!((wb.temp, wb.tint), if relative { (6500.0, 0.0) } else { (4986.0, -2.0) }, "{camera} {format}");
            assert_eq!(wb.scale, if relative { WbScale::Legacy } else { WbScale::Kelvin }, "{camera} {format}");
            // a photo from an older catalog: stored As Shot on the relative scale, still not an edit
            p.develop = Arc::new(DevelopSettings::for_raw(6500.0, 0.0));
            assert!(!p.is_edited(), "{camera} {format}: old As Shot values are not an edit");
            // its as-shot white not yet read through the matrices (6500 / 0 from the old import): defaults still As Shot
            p.as_shot_wb = Some((6500.0, 0.0));
            assert_eq!(p.camera_defaults().wb.mode, WbMode::AsShot);
            assert!(!p.is_edited());
        }
    }
}
