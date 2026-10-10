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
    /// Removing or resizing one edits the catalog only; LightCraft never writes regions to XMP.
    /// Left out of the catalog JSON when empty (most photos), so older catalogs read unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<lightcraft_meta::Region>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Version {
    pub name: String,
    pub created: String,
    pub settings: Arc<DevelopSettings>,
    #[serde(default)]
    pub auto: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryStep {
    pub label: String,
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
    pub embedded_lens: Option<lightcraft_develop::EmbeddedLens>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    ["ARW", "NEF", "NRW", "RW2", "RWL", "RAW", "RAF", "CR3", "CR2", "PEF", "SRW", "ORF"].iter().any(|f| format.eq_ignore_ascii_case(f))
}

/// Whether a raw of `format` shot on `camera` (Exif make and model, as [`Meta::camera`] keeps them) is developed
/// with a white balance relative to its as-shot look (6500 K / 0 means as shot, as for rendered photographs):
/// a file without colour matrices ([`relative_wb_format`]) whose camera has none measured either. A camera
/// with matrices in `lightcraft_raw::spectral` edits in Kelvin: the matrices turn its as-shot multipliers into
/// a temperature, and the slider back, while its colour look stays whatever the file's fit gives (issue #730).
/// The engine's render path (`files.rs`) decides the same way from the file; both must agree.
pub fn relative_wb_camera(format: &str, camera: &str) -> bool {
    relative_wb_format(format) && lightcraft_raw::spectral::find_camera(camera).is_none()
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
        }
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
            ("FUJIFILM X-T4", "RAF", false),
            ("ILCE-7M3", "ARW", false),
            ("SONY ILCE-7M2", "ARW", true),
            ("SONY ILCE-1", "ARW", true),
            ("", "ARW", true),
            ("SONY ILCE-7M3", "DNG", false),
        ] {
            let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.raw", format, 10, 10, "2026-10-01T00:00:00");
            p.kind = MediaKind::Raw;
            p.meta.camera = camera.into();
            p.as_shot_wb = Some((4986.0, -2.0));
            assert_eq!(p.relative_wb(), relative, "{camera} {format}");
            assert_eq!(relative_wb_camera(format, camera), relative, "{camera} {format}");
            assert_eq!(p.fitted_look(), format == "ARW" || format == "NEF" || format == "CR2" || format == "RAF", "{camera} {format}");
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
