//! Catalog records.

use std::sync::Arc;

use lightcraft_develop::{DevelopSettings, WbMode};
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
    /// A procedurally generated demo scene (by `lightcraft-scenes` id).
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

/// A shutter speed as cameras write it (`1/250`, `0.5`, `2"`, `30s`) in seconds; `None` for
/// anything that isn't a positive, finite time.
pub fn parse_shutter_seconds(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches(['s', '"']).trim();
    match s.split_once('/') {
        Some((n, d)) => {
            let (n, d) = (n.trim().parse::<f64>().ok()?, d.trim().parse::<f64>().ok()?);
            if n > 0.0 && d > 0.0 { Some(n / d) } else { None }
        }
        None => s.parse().ok(),
    }
    .filter(|v: &f64| v.is_finite() && *v > 0.0)
}

/// Descriptive + capture metadata.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    pub camera: String,
    /// The camera's maker, from Exif `Make` (`camera` is maker and model together).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub make: String,
    pub lens: String,
    /// The lens's maker, from Exif `LensMake`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub lens_make: String,
    pub focal_mm: Option<f32>,
    pub aperture: Option<f32>,
    /// Shutter speed for display, from [`lightcraft_meta::format_exposure_time`] (`1/3`, `0.3571`, `2.5`).
    pub shutter: String,
    /// The file's exact exposure time in seconds, kept beside the rounded `shutter` text so export
    /// writes the original duration; read through [`Meta::shutter_seconds`]. Left out of the catalog
    /// JSON when unknown, so older catalogs read unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure_time: Option<f64>,
    pub iso: Option<u32>,
    /// Whether the flash fired, from Exif `Flash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flash: Option<bool>,
    /// Exposure compensation in EV, from Exif `ExposureBiasValue`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure_bias: Option<f32>,
    /// The camera body's serial number.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub serial_number: String,
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
    /// Removing or resizing one edits the catalog only; LightCraft never writes regions to XMP.
    /// Left out of the catalog JSON when empty (most photos), so older catalogs read unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<lightcraft_meta::Region>,
    /// Where the camera focused (read from the file's maker notes at import or Reload from Disk; Sony
    /// bodies whose files have been checked), on the upright (EXIF-oriented) photo. Read-only: never
    /// written to XMP. Left out of the catalog JSON when unknown, so older catalogs read unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<lightcraft_meta::FocusPoint>,
}

impl Meta {
    /// Shutter speed in seconds: the exact [`Meta::exposure_time`] while `shutter` still shows it,
    /// otherwise whatever `shutter` says (set by hand, or a catalog from before the exact time was kept).
    pub fn shutter_seconds(&self) -> Option<f64> {
        self.exposure_time
            .filter(|t| lightcraft_meta::format_exposure_time(*t).is_some_and(|text| text == self.shutter.trim()))
            .or_else(|| parse_shutter_seconds(&self.shutter))
    }

    /// The camera's maker and model as people name them, for display; None when the file names no camera. The maker
    /// loses its company words ("OLYMPUS IMAGING CORP." is Olympus) and a name in capitals is written as a name
    /// ("SONY" is Sony; an initialism such as "DJI" stays); the model is kept as the camera writes it, without the
    /// maker said again ("NIKON CORPORATION NIKON Z 8" is Nikon Z 8). [`Meta::make`] says which words of `camera` are
    /// the maker's; for a photo added before it was kept, the first word is.
    pub fn camera_name(&self) -> Option<String> {
        let (camera, make) = (self.camera.trim(), self.make.trim());
        let after_make = camera.strip_prefix(make).filter(|rest| !make.is_empty() && (rest.is_empty() || rest.starts_with(char::is_whitespace)));
        let (maker, model) = match after_make {
            Some(model) => (make, model),
            None => camera.split_once(char::is_whitespace).unwrap_or((camera, "")),
        };
        let maker: Vec<&str> = maker.split_whitespace().filter(|w| !company_word(w)).collect();
        let said: Vec<String> = maker.iter().map(|w| word_key(w)).collect();
        let model: Vec<&str> = if after_make.is_some() {
            // drop the end of the make said again, with or without its company words ("NIKON", "ACME CAMERA SYSTEMS",
            // or "KODAK" after "EASTMAN KODAK COMPANY"), then the make's own company words ("NIKON CORPORATION"); any
            // other word, a company word included, is the model's; an end of company words alone is not the make
            // ("ACME COMPUTER Computer 1000")
            let model: Vec<&str> = model.split_whitespace().collect();
            let keys: Vec<String> = model.iter().map(|w| word_key(w)).collect();
            let named = |end: &&[String]| keys.starts_with(end) && end.iter().any(|w| !company_word(w));
            let longest = |of: &[String]| (0..of.len()).filter_map(|i| of.get(i..)).find(named).map_or(0, <[String]>::len);
            let full: Vec<String> = make.split_whitespace().map(word_key).collect();
            let run = longest(&full).max(longest(&said));
            let own: Vec<String> = make.split_whitespace().filter(|w| company_word(w)).map(word_key).collect();
            let rest = model.get(run..).unwrap_or_default();
            rest.iter().skip_while(|w| run > 0 && own.contains(&word_key(w))).copied().collect()
        } else {
            // the maker taken from the first word: drop the company words it left behind when they hold a legal form
            // ("CASIO COMPUTER CO.,LTD.", with the maker said again among them), otherwise only the maker said again
            // ("ACME Computer 1000" keeps Computer)
            let model: Vec<&str> = model.split_whitespace().collect();
            let run: Vec<&str> = model.iter().copied().take_while(|w| said.contains(&word_key(w)) || company_word(w)).collect();
            let drop = if run.iter().any(|w| legal_form(w)) { run.len() } else { run.iter().take_while(|w| said.contains(&word_key(w))).count() };
            model.get(drop..).unwrap_or_default().to_vec()
        };
        let words: Vec<String> = maker.iter().map(|w| maker_word(w)).chain(model.into_iter().map(str::to_string)).collect();
        (!words.is_empty()).then(|| words.join(" "))
    }
}

/// A word of a camera's name as words are compared: its letters and digits in lower case ("CORP." is corp, "CO.,LTD."
/// coltd).
fn word_key(w: &str) -> String {
    w.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Is `w` a word of a maker's company name ("IMAGING CORP.", "COMPANY, LTD.", "CAMERA AG"), or punctuation alone?
fn company_word(w: &str) -> bool {
    const DESCRIPTIVE: &str = "imaging camera computer optical electronics";
    let key = word_key(w);
    key.is_empty() || legal_form(w) || DESCRIPTIVE.split(' ').any(|c| c == key)
}

/// Is `w` the legal form of a company ("CORP.", "CO.,LTD.", "AG")? Unlike "CAMERA" or "COMPUTER", no model starts
/// with one.
fn legal_form(w: &str) -> bool {
    const FORMS: &str = "corporation corp company co ltd coltd limited inc llc gmbh ag";
    let key = word_key(w);
    FORMS.split(' ').any(|c| c == key)
}

/// A word of a maker's name as a name: one in capitals of more than three letters ("OLYMPUS") or in lower case
/// ("samsung") with only its first letter a capital; an initialism ("DJI"), a name in mixed case ("GoPro") and a word
/// with digits or punctuation as written.
fn maker_word(w: &str) -> String {
    let letters = w.chars().all(char::is_alphabetic);
    let capitals = w.chars().all(char::is_uppercase) && w.chars().count() > 3;
    let lower = w.chars().all(char::is_lowercase);
    if !(letters && (capitals || lower)) {
        return w.to_string();
    }
    let mut chars = w.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars.flat_map(char::to_lowercase)).collect()).unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Version {
    pub name: String,
    pub created: String,
    #[serde(deserialize_with = "crate::shared::deserialize")]
    pub settings: Arc<DevelopSettings>,
    #[serde(default)]
    pub auto: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryStep {
    pub label: String,
    #[serde(deserialize_with = "crate::shared::deserialize")]
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
    pub imported: String,
    #[serde(default)]
    pub meta: Meta,
    #[serde(default)]
    pub rating: u8,
    #[serde(default)]
    pub flag: Flag,
    #[serde(default)]
    pub label: Option<ColorLabel>,
    #[serde(deserialize_with = "crate::shared::deserialize_current")]
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
    pub embedded_lens: Option<Box<lightcraft_develop::EmbeddedLens>>,
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
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::shared::deserialize_optional")]
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
    /// The app version (`CARGO_PKG_VERSION`) whose decoder checked [`Photo::preview_only`].
    /// Also set for supported ORF files: compressed ORF support was withdrawn after #3775, so
    /// old sensor-decodable records need rechecking too. Empty for other supported formats and
    /// legacy records. A stale stamp is rechecked when the library opens (issue #662).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preview_checked: String,
    /// A Local record: the fingerprint of its state when the browse catalogued it, to tell
    /// whether the user changed anything since (see [`crate::local`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_baseline: Option<u64>,
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
    /// Set when the person arranged this photo's burst by hand (Window ▸ Bursts, or
    /// `photo.moveToBurst`): its place in [`Analysis::group`], 0 being the pick, or `Some(0)` for
    /// a photo taken out of every burst. Culling run again keeps such a photo's burst, place and
    /// pick (unless asked to regroup). `None` while the burst is as culling made it; left out of the
    /// catalog JSON then, so older catalogs read unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<u32>,
}

impl Analysis {
    /// Whether the person placed this photo by hand (see [`Analysis::rank`]).
    pub fn by_hand(&self) -> bool {
        self.rank.is_some()
    }
}

/// Raw formats whose files carry vendor white-balance multipliers but no colour matrices: their starting
/// look is fitted to the file's own JPEG (the engine's `camera_preview::file_local_look` lists the same
/// formats). `format` is the file's extension or Lightroom's `fileFormat` name, in any case. Whether such a
/// file's white balance is edited relative to its as-shot look or in Kelvin depends on the camera:
/// [`relative_wb_camera`].
pub fn relative_wb_format(format: &str) -> bool {
    ["ARW", "NEF", "NRW", "RW2", "RWL", "RAW", "RAF", "CR3", "CR2", "PEF", "SRW", "ORF", "ERF", "MRW", "CRW"]
        .iter()
        .any(|f| format.eq_ignore_ascii_case(f))
}

/// Whether a raw of `format` shot on `camera` (Exif make and model, as [`Meta::camera`] keeps them) is developed
/// with a white balance relative to its as-shot look (6500 K / 0 means as shot, as for rendered photographs):
/// a file without colour matrices ([`relative_wb_format`]) whose camera has none measured either, nor
/// white-balance presets to read Kelvin from. A camera with matrices in `lightcraft_raw::spectral` edits in Kelvin:
/// the matrices turn its as-shot multipliers into a temperature, and the slider back, while its colour look stays
/// whatever the file's fit gives (issue #730). A camera in `lightcraft_raw::kelvin_presets` does the same through
/// its own presets. The engine's render path (`files.rs`) decides the same way from the file; both must agree.
pub fn relative_wb_camera(format: &str, camera: &str) -> bool {
    relative_wb_format(format) && lightcraft_raw::spectral::find_camera(camera).is_none() && !lightcraft_raw::kelvin_presets::covers_camera(camera)
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
    /// Keep the newest [`HISTORY_LIMIT`](crate::HISTORY_LIMIT) History steps; the cursor moves with
    /// its step, or to the oldest step kept when its own is dropped. Returns how many were dropped.
    pub fn trim_history(&mut self) -> usize {
        let n = self.history.len().saturating_sub(crate::HISTORY_LIMIT);
        if n > 0 {
            self.history.drain(..n);
            self.history_cursor = self.history_cursor.map(|c| c.saturating_sub(n));
        }
        n
    }

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
            imported: imported.to_string(),
            meta: Meta::default(),
            rating: 0,
            flag: Flag::None,
            label: None,
            develop: crate::shared::default_settings(),
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
        }
    }
    /// Sets [`Photo::preview_only`] as this build decided it, with its stamp.
    pub fn set_preview_only(&mut self, preview_only: Option<String>) {
        self.preview_checked = self.preview_stamp(&preview_only);
        self.preview_only = preview_only;
    }
    /// The stamp for a fresh probe. Supported ORFs keep it too, so the support-withdrawal
    /// recheck runs once, without repeatedly probing uncompressed ORFs on every library open.
    pub fn preview_stamp(&self, preview_only: &Option<String>) -> String {
        if self.format.eq_ignore_ascii_case("orf") { APP_VERSION.to_string() } else { preview_stamp(preview_only) }
    }
    /// A preview-only photo or ORF whose support was checked by another app version (or before
    /// stamps existed), so this build should probe its file again (issues #662 and #3775).
    pub fn preview_only_stale(&self) -> bool {
        (self.preview_only.is_some() || self.format.eq_ignore_ascii_case("orf")) && self.preview_checked != APP_VERSION
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
    /// Every settings holder of this photo that can carry segmentation grids: the develop
    /// settings, the versions', the history steps' and the import look. A snapshot's shared grid
    /// table ([`crate::Catalog`]'s `grids`) covers all of them, so a new holder belongs here.
    pub(crate) fn settings_holders(&self) -> impl Iterator<Item = &Arc<DevelopSettings>> {
        std::iter::once(&self.develop)
            .chain(self.versions.iter().map(|v| &v.settings))
            .chain(self.history.iter().map(|h| &h.settings))
            .chain(self.import_look.iter())
    }
    /// [`Photo::settings_holders`] with mutable access (resolving a snapshot's grid references).
    pub(crate) fn settings_holders_mut(&mut self) -> impl Iterator<Item = &mut Arc<DevelopSettings>> {
        let Photo { develop, versions, history, import_look, .. } = self;
        std::iter::once(develop)
            .chain(versions.iter_mut().map(|v| &mut v.settings))
            .chain(history.iter_mut().map(|h| &mut h.settings))
            .chain(import_look.as_mut())
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
    /// carry the photo's own rendering process ([`lightcraft_develop::ProcessVersion`]): which
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
            d.wb.scale = lightcraft_develop::WbScale::Kelvin;
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
    /// A video clip (an MP4, a MOV, …), which the image decoders can't read. Besides the kind, the
    /// format and the extension count: a Lightroom catalog lists its videos with the format `VIDEO`,
    /// and libraries made before they were imported as videos hold them as images.
    pub fn is_video(&self) -> bool {
        const EXTENSIONS: [&str; 12] = ["mp4", "mov", "m4v", "avi", "mts", "m2ts", "3gp", "mkv", "webm", "wmv", "mpg", "mpeg"];
        self.kind == MediaKind::Video
            || self.format.eq_ignore_ascii_case("VIDEO")
            || EXTENSIONS.iter().any(|e| self.extension().eq_ignore_ascii_case(e))
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
        for r in self.meta.regions.iter().filter(|r| r.kind == lightcraft_meta::RegionKind::Face) {
            let Some(name) = r.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) else { continue };
            if !out.iter().any(|o| o.to_lowercase() == name.to_lowercase()) {
                out.push(name);
            }
        }
        out
    }
    /// In the library: not deleted and not only browsed (Local).
    pub fn in_library(&self) -> bool {
        !self.deleted && !self.local
    }
    /// The file Missing Photos checks for this photo, if it is in scope: a library photo (not in
    /// Recently Deleted, not a Local-only record seen while browsing a folder) whose original is a
    /// file. Browse records are out of scope on purpose: browsing can leave tens of thousands of
    /// them in the catalog, they never show in library views (Missing Photos included), and a
    /// folder that is gone is simply not browsed again. The sidebar count, the Missing Photos view,
    /// `library.missing` and Find Missing Photos all use this one rule.
    pub fn library_file(&self) -> Option<&str> {
        match &self.source {
            Source::File { path } if self.in_library() => Some(path),
            _ => None,
        }
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
}

impl Album {
    /// A regular (manual) album.
    pub fn new(id: AlbumId, name: impl Into<String>) -> Album {
        Album { id, name: name.into(), parent: None, folder: false, photos: Vec::new(), cover: None, smart: None, quick: false, order: None }
    }
    pub fn is_smart(&self) -> bool {
        self.smart.is_some()
    }
}

#[cfg(test)]
mod edited_tests {
    use super::*;
    use lightcraft_develop::WbScale;

    #[test]
    fn a_video_is_known_by_its_kind_format_or_extension() {
        let file = |name: &str, format: &str| Photo::new(PhotoId(1), Source::Demo { scene: 0 }, name, format, 10, 10, "2026-10-01T00:00:00");
        let mut clip = file("a.jpg", "JPEG");
        clip.kind = MediaKind::Video;
        assert!(clip.is_video(), "by its kind");
        assert!(file("clip", "video").is_video(), "by the format a Lightroom catalog gives videos");
        for name in ["VID_1.mp4", "a.MOV", "b.m4v", "c.mts", "d.webm"] {
            assert!(file(name, "").is_video(), "{name}");
        }
        // (a name without an extension, or a dot file, has none)
        for (name, format) in [("a.jpg", "JPEG"), ("a.arw", "ARW"), ("a.heic", "HEIC"), ("mp4", ""), (".mov", "")] {
            assert!(!file(name, format).is_video(), "{name}");
        }
    }

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
        use lightcraft_develop::ProcessVersion;
        // a photo on another process than the latest (a V1 photo once a later process is the
        // latest, or a photo saved by a newer LightCraft)
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
            ("a.mrw", "MRW", true),
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

    /// A raw without matrices in the file edits in Kelvin when its camera's matrices are measured (issue #730):
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
            ("SONY ILCE-7M2", "ARW", true),
            ("SONY ILCA-99M2", "ARW", false),
            ("SONY SLT-A33", "ARW", false),
            ("SONY ILCE-1", "ARW", true),
            ("", "ARW", true),
            ("SONY ILCE-7M3", "DNG", false),
            ("NIKON CORPORATION NIKON Z5_2", "NEF", false),
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

#[cfg(test)]
mod shutter_tests {
    use super::*;

    #[test]
    fn shutter_seconds_prefers_the_exact_time_while_the_text_still_shows_it() {
        let mut m = Meta { shutter: "0.3571".into(), exposure_time: Some(1.0 / 2.8), ..Default::default() };
        assert_eq!(m.shutter_seconds(), Some(1.0 / 2.8));
        m.shutter = "1/3".into();
        m.exposure_time = Some(1.0 / 3.0);
        assert_eq!(m.shutter_seconds(), Some(1.0 / 3.0));
        // the text no longer matches the stored time (edited elsewhere): the text wins
        m.shutter = "1/250".into();
        assert_eq!(m.shutter_seconds(), Some(1.0 / 250.0));
        // an older catalog without the exact time
        m.exposure_time = None;
        m.shutter = "2.5".into();
        assert_eq!(m.shutter_seconds(), Some(2.5));
        // the exact time stays out of the JSON when unknown, and round-trips when known
        assert!(!serde_json::to_string(&m).unwrap().contains("exposure_time"));
        m.exposure_time = Some(2.5);
        let back: Meta = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back.exposure_time, Some(2.5));
    }

    #[test]
    fn a_fraction_needs_a_positive_numerator_and_denominator() {
        for s in ["-1/-250", "1/-250", "-1/250", "0/250"] {
            assert_eq!(parse_shutter_seconds(s), None, "{s}");
        }
        assert_eq!(parse_shutter_seconds("1/250"), Some(0.004));
    }
}

#[cfg(test)]
mod camera_name_tests {
    use super::*;

    fn name(make: &str, camera: &str) -> Option<String> {
        Meta { make: make.into(), camera: camera.into(), ..Default::default() }.camera_name()
    }

    #[test]
    fn a_cameras_name_is_its_maker_once_without_company_words_then_its_model_as_written() {
        for (make, camera) in [
            ("OLYMPUS IMAGING CORP.", "OLYMPUS IMAGING CORP. E-M5MarkII"),
            ("NIKON CORPORATION", "NIKON CORPORATION NIKON Z 8"),
            ("RICOH IMAGING COMPANY, LTD.", "RICOH IMAGING COMPANY, LTD. PENTAX K-1"),
            ("LEICA CAMERA AG", "LEICA CAMERA AG LEICA Q3"),
            ("Canon", "Canon Canon EOS R5"),
        ] {
            // the maker known, and a photo added before it was kept (its first word taken for the maker)
            for make in [make, ""] {
                let shown = name(make, camera).unwrap_or_else(|| panic!("{camera}: no name"));
                let (maker, model) = shown.split_once(' ').unwrap_or_else(|| panic!("{shown}: no model"));
                assert!(camera.ends_with(model), "{shown}: the model as {camera:?} writes it");
                assert!(!model.to_lowercase().contains(&maker.to_lowercase()), "{shown}: the maker said once");
                assert!(camera.to_lowercase().starts_with(&maker.to_lowercase()), "{shown}");
            }
        }
        // a maker in capitals is written as a name, an initialism or a name in mixed case as it is
        assert!(name("SONY", "SONY ILCE-7M4").is_some_and(|n| n.starts_with("Sony ")));
        assert!(name("DJI", "DJI FC3582").is_some_and(|n| n.starts_with("DJI ")));
        assert_eq!(name("", "Apple iPhone 15 Pro").as_deref(), Some("Apple iPhone 15 Pro"));
        // a maker of several words, and a model with no maker before it, as written
        assert_eq!(name("OM Digital Solutions", "OM Digital Solutions OM-1").as_deref(), Some("OM Digital Solutions OM-1"));
        assert_eq!(name("", "ILCE-7M4").as_deref(), Some("ILCE-7M4"));
        assert_eq!(name("", " "), None, "no camera");
        // a maker whose company words follow its name ("COMPUTER CO.,LTD."), known or taken from the first word
        for make in ["CASIO COMPUTER CO.,LTD.", ""] {
            assert_eq!(name(make, "CASIO COMPUTER CO.,LTD. EX-Z1050").as_deref(), Some("Casio EX-Z1050"), "make {make:?}");
        }
        // with the maker known, a company word that starts the model is the model's
        assert_eq!(name("ACME", "ACME Computer 1000").as_deref(), Some("Acme Computer 1000"));
        // but the maker said again in full, company words and all, is dropped
        let repeated = "NIKON CORPORATION NIKON CORPORATION Z 8";
        assert_eq!(name("NIKON CORPORATION", repeated).as_deref(), Some("Nikon Z 8"));
        // a model word that is only some word of the maker, or a company word the make never had, is the model's
        let om = "OM Digital Solutions Digital 1000";
        assert_eq!(name("OM Digital Solutions", om).as_deref(), Some("OM Digital Solutions Digital 1000"));
        assert_eq!(name("ACME", "ACME ACME Computer 1000").as_deref(), Some("Acme Computer 1000"));
        assert_eq!(name("ACME COMPUTER", "ACME COMPUTER Computer 1000").as_deref(), Some("Acme Computer 1000"));
        // with the maker taken from the first word, a company word with no legal form after it is the model's, and
        // the first word ends at any space
        assert_eq!(name("", "ACME Computer 1000").as_deref(), Some("Acme Computer 1000"));
        assert_eq!(name("", "CANON\tALPHA R5").as_deref(), Some("Canon ALPHA R5"));
        // the end of the maker said again is dropped ("EASTMAN KODAK" then "KODAK")
        let kodak = "EASTMAN KODAK COMPANY KODAK EASYSHARE C875 ZOOM DIGITAL CAMERA";
        let shown = name("EASTMAN KODAK COMPANY", kodak);
        assert_eq!(shown.as_deref(), Some("Eastman Kodak EASYSHARE C875 ZOOM DIGITAL CAMERA"));
        // the make said again with a company word inside it is dropped whole
        let systems = "ACME CAMERA SYSTEMS ACME CAMERA SYSTEMS 1000";
        assert_eq!(name("ACME CAMERA SYSTEMS", systems).as_deref(), Some("Acme Systems 1000"));
    }
}
