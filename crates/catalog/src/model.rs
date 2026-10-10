//! Catalog records.

use std::sync::Arc;

use dac_develop::{DevelopSettings, WbMode};
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
    /// A procedurally generated demo scene (by `dac-scenes` id).
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
    pub regions: Vec<dac_meta::Region>,
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
    pub embedded_lens: Option<Box<dac_develop::EmbeddedLens>>,
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

impl Photo {
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
            develop: Arc::new(DevelopSettings::default()),
            edited: None,
            versions: Vec::new(),
            history: Vec::new(),
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
            local_baseline: None,
            sha1: None,
            xmp: None,
        }
    }
    /// A raw file developed from its sensor data: not a rendered image, and not a raw shown from
    /// its embedded preview ([`Photo::preview_only`]).
    pub fn develops_raw(&self) -> bool {
        self.kind == MediaKind::Raw && self.preview_only.is_none()
    }
    /// The current ARW, NEF, RW2, RAF, CR3, CR2, PEF and SRW readers have vendor WB multipliers but no measured camera
    /// illuminant. Use adjustments relative to the camera's as-shot look, as for rendered
    /// photographs (the engine's `camera_preview::file_local_look` covers the same formats; RWL and
    /// RAW are Leica's and the oldest Panasonic bodies' names for RW2 files).
    pub fn relative_wb(&self) -> bool {
        self.develops_raw()
            && ["ARW", "NEF", "NRW", "RW2", "RWL", "RAW", "RAF", "CR3", "CR2", "PEF", "SRW"].iter().any(|f| self.format.eq_ignore_ascii_case(f))
    }
    /// The develop settings import gave this photo: [`Photo::camera_defaults`], or the user's
    /// default preset applied on top of them ([`Photo::import_look`]).
    pub fn import_defaults(&self) -> DevelopSettings {
        match &self.import_look {
            Some(l) => (**l).clone(),
            None => self.camera_defaults(),
        }
    }
    /// The built-in defaults for this photo, before any user default preset: raws start from
    /// their as-shot white balance; embedded lens corrections on when the file has them.
    pub fn camera_defaults(&self) -> DevelopSettings {
        let wb = if self.relative_wb() { Some((6500.0, 0.0)) } else { self.as_shot_wb };
        let mut d = match wb {
            Some((t, tint)) => DevelopSettings::for_raw(t, tint),
            None => DevelopSettings::default(),
        };
        if self.embedded_lens.is_some() {
            d.optics.lens_profile = true;
        }
        d
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
            ("a.dng", "DNG", false),
        ] {
            let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, name, format, 10, 10, "2026-10-01T00:00:00");
            p.kind = MediaKind::Raw;
            p.as_shot_wb = Some((5200.0, 4.0));
            assert_eq!(p.relative_wb(), relative, "{format}");
            let wb = p.camera_defaults().wb;
            assert_eq!((wb.temp, wb.tint), if relative { (6500.0, 0.0) } else { (5200.0, 4.0) }, "{format}");
            p.develop = Arc::new(DevelopSettings::for_raw(5200.0, 4.0));
            assert!(!p.is_edited(), "{format}: old As Shot values are not an edit");
            let mut custom = (*p.develop).clone();
            custom.wb.mode = WbMode::Custom;
            p.develop = Arc::new(custom);
            assert!(p.is_edited(), "{format}: Custom WB is still an edit");
            // shown from the embedded preview: a rendered image, not a relative-WB raw
            p.preview_only = Some("unsupported".into());
            assert!(!p.relative_wb());
        }
    }
}
