//! Import: files and folders (recursive) → photos.
//!
//! 1. Expand folders recursively into supported files (hidden entries and the library's own
//!    folder are skipped), sorted for a stable order.
//! 2. Skip paths already in the catalog.
//! 3. Probe the rest — dimensions, metadata, and the **content hash** of the bytes — in parallel
//!    on native targets.
//! 4. Skip **duplicates by content** (same bytes already in the library, or twice in this batch).
//!    A duplicate (by path or content) of a photo in Recently Deleted is restored instead, with
//!    its edits, unless the import says otherwise ([`OnDeleted`]).
//! 5. *Add* in place (the photo points at the original file), *copy* into the library's
//!    `Originals/YYYY/YYYY-MM-DD/` folder (names made unique) and point at the copy, or *move*
//!    there: placed like a copy (with its XMP sidecars), and the original removed only once its
//!    catalog record is committed and saved (see [`crate::import_move`] for the safety rules).
//! 6. Apply each photo's XMP sidecar (or a raw/DNG file's embedded XMP): the sidecar wins for
//!    metadata and develop settings are restored (see [`crate::sidecar`]). Its capture time fills
//!    in when the file has none (read before step 5, so it also files and names the copy).
//! 7. Optionally apply a preset, then a batch-edit recipe (one more History step), and add
//!    keywords (the import dialog's options).
//! 8. Commit all new photos as one undoable op. (Undoing a move removes the catalog records;
//!    the files stay at the destination — undo never moves files back over a card.)
//! 9. Copy and Move rename sequences use checked platform-sized numbers: the last number is usable once, and any
//!    further renamed files are reported as failed without losing earlier completed transfers.
//!
//! The import dialog first calls [`scan`] (`library.importPreview`): the same expansion, probing
//! and duplicate detection without adding anything, so the user can review the candidates. The
//! probes are kept and reused by the import that follows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lcc_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use serde::{Deserialize, Serialize};

use crate::Session;
use crate::media::ProbeInfo;

/// File extensions offered by import discovery and file pickers (lower case).
/// This includes preview-only raws; the file probe decides whether sensor decoding is available.
pub const EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "tif", "tiff", "webp", "dng", "cr2", "cr3", "nef", "nrw", "arw", "sr2", "srf", "raf", "orf", "rw2", "rwl", "raw", "pef",
    "srw", "psd", "psb", "jxl", "gif", "bmp",
    // Decoded when built with the codecs' `heif` feature; otherwise the import reports them as
    // failed with the reason ("HEIC/HEIF support isn't included in this build").
    "heic", "heif", "avif", // a HEIC without the `heif` feature is reported as failed
    "iiq", "crw", "mrw", "x3f", "kdc", "mos", "erf", "3fr", "fff", "mef", "gpr", // video clips (MediaKind::Video; see VIDEO_EXTENSIONS)
    "mp4", "m4v", "mov", "mkv", "webm", "mts", "m2ts", "3gp", "mxf",
];

/// Video clip extensions (lower case): imported as [`MediaKind::Video`].
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "mkv", "webm", "mts", "m2ts", "3gp", "mxf"];

/// Whether a file name's extension is a video clip's.
pub fn is_video_name(name: &str) -> bool {
    Path::new(name).extension().is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.to_string_lossy().to_lowercase().as_str()))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImportMode {
    /// Reference the files where they are.
    #[default]
    Add,
    /// Copy the files into the library's `Originals/` folder.
    Copy,
    /// Move the files there: as Copy, then each original (and its sidecars) is removed from the
    /// source once the copy is verified and catalogued.
    Move,
}

/// What an import does with a file the library already has in Recently Deleted.
///
/// Someone who imports such a file wants the photo in the library again, so the default brings
/// it back (issues #298, #706: before, the import skipped it as "already in the library" while
/// the grid showed it deleted, and the user was stuck). Restoring loses nothing — the edits,
/// rating and albums come back with the photo and Reset Edits gives a clean start — where
/// importing afresh would silently drop them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OnDeleted {
    /// Leave it there; the report says so ([`Duplicate::existing_deleted`]).
    Skip,
    /// Bring the trashed photo back, with its edits ([`ImportReport::restored`]); a photo whose
    /// file is no longer where the library had it is relinked to the imported file.
    #[default]
    Restore,
    /// Delete the trashed record permanently and import the file as a new photo.
    Fresh,
}

impl OnDeleted {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "skip" => Some(Self::Skip),
            "restore" => Some(Self::Restore),
            "fresh" => Some(Self::Fresh),
            _ => None,
        }
    }
}

/// What an import does besides adding the photos.
#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    /// A file that is in Recently Deleted: restore (default), skip, or import afresh.
    pub on_deleted: OnDeleted,
    pub mode: ImportMode,
    /// Applied to every imported photo (one History entry).
    pub preset: Option<lcc_develop::Preset>,
    /// A preset stack, applied in order after `preset` (one History entry each): the Import tab's
    /// presets (OneSlips, LR-IMP-WINDOW).
    pub presets: Vec<lcc_develop::Preset>,
    /// Added to every imported photo.
    pub keywords: Vec<String>,
    /// Browsing a folder: photos come in as `local` (not in the library), and a file with the
    /// same content as one already known is still listed.
    pub local: bool,
    /// Copy: where the copies go (default: the library's `Originals/`).
    pub destination: Option<String>,
    /// Copy: the folders inside the destination.
    pub organize: Organize,
    /// Copy: a file-name template for the copies ([`crate::rename::expand`] tokens: `{name}`,
    /// `{seq:N}`, `{date:%Y%m%d}`, `{camera}`…), numbered from `rename_start`.
    pub rename: Option<String>,
    pub rename_start: usize,
    /// A metadata preset (by name) applied to every imported photo.
    pub metadata_preset: Option<String>,
    /// Copy: raws are copied as DNG (Copy as DNG).
    pub convert_dng: bool,
    /// The camera profile the raws get ([`crate::profile_bodies::ImportProfile`]); a sidecar's own wins.
    pub camera_profile: crate::profile_bodies::ImportProfile,
    /// A preset's own camera profile replaces the body's (`presetProfile`). Off (the default): a raw
    /// keeps its camera profile through the preset stack (OneSlips, LR-IMP-PROFILE).
    pub preset_profile: bool,
    /// A batch-edit recipe: develop control id / value pairs (e.g. `light.exposure` → 0.5),
    /// applied to every imported photo *after* its XMP sidecar, camera default and import preset,
    /// as one more History step (`Import adjustments`). Only the named controls change; the recipe
    /// is a snapshot taken when the import starts, so editing a preset afterwards cannot change it.
    /// It names plain adjustments only (no white balance, geometry or crop: `cmd::check_recipe`),
    /// and [`ImportJob::new`] refuses anything else.
    /// Empty (the default) = no recipe. A photo the library already has keeps its own settings:
    /// one skipped as a duplicate, restored from Recently Deleted, or joined from a browsed
    /// folder (Local) is never touched by the recipe. (A browsed photo joining the library does
    /// get the import's metadata preset, keywords and presets: [`promote_ops`].)
    pub recipe: Vec<(String, f64)>,
    /// Import files whose content the library already has as new photos (Lightroom's "Don't
    /// Import Suspected Duplicates" unchecked). A file at a path the library already lists, or one
    /// in Recently Deleted, is still handled as a duplicate.
    pub import_duplicates: bool,
}

/// How copies are filed in the destination. The date is the capture time, else the time of the
/// import.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Organize {
    /// `YYYY/YYYY-MM-DD/` by capture date.
    #[default]
    ByDay,
    /// `YYYY/YYYY-MM/`.
    ByMonth,
    /// All in the destination itself.
    Flat,
    /// A folder template, e.g. `{date:%Y}/{date:%Y%m%d}` → `2026/20260114/`: the template's own
    /// `/` make the levels, each expanded with the file-name tokens ([`crate::rename::expand_folder`];
    /// never outside the destination).
    Template(String),
}

impl Organize {
    /// `date` | `month` | `flat`, or a folder template (anything with a `{`, `/` or `\`).
    pub fn parse(s: &str) -> Option<Organize> {
        match s {
            "date" | "day" | "byDay" => Some(Organize::ByDay),
            "month" | "byMonth" => Some(Organize::ByMonth),
            "flat" | "none" | "intoOneFolder" => Some(Organize::Flat),
            t if t.contains(['{', '/', '\\']) => Some(Organize::Template(t.trim().to_string())),
            _ => None,
        }
    }

    /// The folders (inside the destination) a copy of `p` goes to.
    pub fn folders(&self, p: &Photo) -> Vec<String> {
        // a year or a month alone (`1983`) files under the start of its period
        let date = lcc_catalog::dates::capture_start(p.date());
        let day = date.get(..10).filter(|d| d.len() == 10).unwrap_or("undated");
        let year = day.get(..4).unwrap_or("undated");
        match self {
            Organize::ByDay => vec![year.into(), day.into()],
            Organize::ByMonth => vec![year.into(), day.get(..7).unwrap_or("undated").into()],
            Organize::Flat => Vec::new(),
            Organize::Template(t) => crate::rename::expand_folder(t, p, 1),
        }
    }
}

/// A file found by [`scan`], for the import review.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImportCandidate {
    pub path: String,
    pub name: String,
    pub format: String,
    pub kind: MediaKind,
    pub width: u32,
    pub height: u32,
    pub file_size: u64,
    pub captured: Option<String>,
    /// `"path"` (already in the library), `"content"` (same bytes in the library or earlier in this
    /// list): skipped by the import.
    pub duplicate: Option<String>,
    /// The photo that already has it. On a browsed candidate (`local`): its record.
    pub existing: Option<u64>,
    /// The file was only browsed in Local: the library has a record of it (`existing`) that is not in
    /// the library, so it is not a duplicate and importing it promotes that record.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub local: bool,
    /// Not readable.
    pub error: Option<String>,
    /// A raw variant that can't be decoded yet (why): it imports as preview only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_only: Option<String>,
    /// A raw's camera body as `profiles.bodies` keys it (`model|serial`, else `model|file:XX`),
    /// for the review's camera-profile rows ([`crate::profile_bodies::plan`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// A raw's camera (`Make Model`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub camera: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Duplicate {
    pub path: String,
    /// The photo that already has these bytes (or this path).
    pub existing: Option<u64>,
    /// `"path"` (already imported from there) or `"content"` (same bytes elsewhere).
    pub reason: &'static str,
    /// That photo is in Recently Deleted (see [`OnDeleted`]).
    #[serde(rename = "existingDeleted", skip_serializing_if = "std::ops::Not::not")]
    pub existing_deleted: bool,
}

/// Files an import's folder walk left out ([`Expansion::unreadable`], [`Expansion::unsupported`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Skipped {
    /// Folders whose contents couldn't be listed.
    pub unreadable: usize,
    /// Files in the folders walked that are not a supported photo type (XMP sidecars not counted).
    pub unsupported: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ImportReport {
    pub imported: Vec<u64>,
    /// Local (browsed) records among `imported` that were changed after browsing: those edits are
    /// the library's own, unlike settings browsing read from the file.
    #[serde(skip)]
    pub promoted_changed: Vec<u64>,
    pub duplicates: Vec<Duplicate>,
    /// [`OnDeleted::Restore`]: photos brought back from Recently Deleted instead of imported.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub restored: Vec<u64>,
    /// Move: the originals that were moved (and removed from the source).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub moved: Vec<Moved>,
    /// Move: originals (or sidecars) left at the source, and why — the photo may still have been
    /// imported (from its copy, or in place).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kept: Vec<Kept>,
    /// (path, error)
    pub failed: Vec<(String, String)>,
    /// Files skipped as still being copied in (`lcc_catalog::incoming`): a later scan or import picks them up.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub still_copying: Vec<String>,
    /// Files found after expanding folders.
    pub scanned: usize,
    /// Photos whose metadata/develop settings were read from an XMP sidecar (or embedded XMP).
    pub sidecars: usize,
    /// What the folder walk left out ([`Expansion`]), as `library.importPreview` reports it.
    pub skipped: Skipped,
    /// Per-photo diagnostics from foreign XMP develop settings that could not be fully carried over.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ImportWarning>,
    /// The camera profile each imported raw got, by profile: `{id ("" = the camera's default): {name, photos, bodies}}`.
    #[serde(rename = "cameraProfiles", skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub camera_profiles: std::collections::BTreeMap<String, crate::profile_bodies::ProfileCount>,
}

/// A structured diagnostic from foreign XMP develop settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportWarningCode {
    /// A named camera profile was found but cannot be applied as Lightroom does.
    CameraProfile,
    /// Lightroom geometry data was found but its saved transform is not retained.
    UprightGeometry,
    /// One or more other XMP develop fields have no mapping in the app.
    UnmappedXmp,
}

/// The saved Lightroom Upright mode associated with a foreign geometry transform.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportUprightMode {
    Off,
    Auto,
    Level,
    Vertical,
    Full,
    Guided,
    Unknown,
}

/// One actionable XMP import diagnostic. `path` is filled when its photo is committed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportWarning {
    pub path: String,
    pub code: ImportWarningCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ImportUprightMode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    /// The photo it was imported as, filled with `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub photo: Option<u64>,
}

/// A file moved by an import.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Moved {
    pub from: String,
    pub to: String,
    /// Its sidecars' new paths.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sidecars: Vec<String>,
}

/// A source a Move left where it was.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Kept {
    pub path: String,
    pub reason: String,
}

/// The develop settings a photo gets on import: raws start from their as-shot white balance with
/// default sharpening / colour noise reduction, and file-embedded lens corrections on (as the
/// camera intended); a user default preset ([`ImportDefaults`]) goes on top.
pub fn import_defaults(p: &Photo) -> lcc_develop::DevelopSettings {
    p.import_defaults()
}

/// Does the photo still look as the camera rendered it (its embedded camera preview is then a
/// fair stand-in)? Not when a default preset changed it on import. As Shot counts as As Shot
/// whatever numbers it stores ([`Photo::develop_matches`]), as for [`Photo::is_edited`].
pub fn has_import_look(p: &Photo) -> bool {
    // Remove Hot Pixels doesn't count: raws imported since the box exists start with it on, older
    // ones with it off, and the camera's JPEG is the picture as imported either way (upstream #4389).
    let mut defaults = p.camera_defaults();
    defaults.detail.nr_outliers = p.develop.detail.nr_outliers;
    p.develop_matches(defaults)
}

/// [`has_import_look`], or the only change is the camera profile (colour calibration), such as the
/// body's default profile picked for a photo browsed before import: the camera's JPEG then shows
/// the photo as framed and exposed, a fair stand-in until the render arrives (labelled "Camera
/// preview" in the loupe). Any other edit is never shown as the camera JPEG.
pub fn looks_as_shot(p: &Photo) -> bool {
    if has_import_look(p) {
        return true;
    }
    let neutral = |d: &lcc_develop::DevelopSettings| {
        let mut d = d.clone();
        d.profile.camera.clear();
        d.profile.id = lcc_develop::Profile::default().id;
        d
    };
    neutral(&p.develop) == neutral(&p.camera_defaults())
}

/// The name `library.setImportDefault` saves its preset under when the caller doesn't name one
/// (Develop ▸ Optics ▸ Save as… pre-fills it).
pub const NEW_PHOTO_DEFAULT: &str = "Optics for new photos";

/// User defaults applied on import (Settings → Import; saved in the library's `prefs.json`).
/// Presets are referenced by id; a preset that no longer exists is ignored.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImportDefaults {
    /// Preset applied to raw files (`None` = the the app default).
    pub raw_preset: Option<String>,
    /// Use a camera's own default (below) when the photo's camera has one.
    pub per_camera: bool,
    /// Per-camera raw defaults, keyed by the camera's make + model as shown in Info
    /// (`"Canon EOS R5"`).
    pub cameras: Vec<CameraDefault>,
    /// Preset applied to non-raw images (JPEG, PNG, TIFF, HEIC…; `None` = none).
    pub other_preset: Option<String>,
    /// Copyright notice given to imported photos that don't carry one (empty = none).
    pub copyright: String,
    /// Creator given to imported photos that don't name one (empty = none).
    pub creator: String,
    /// Metadata preset applied to every imported photo (`metadata.*`; `None` = none).
    pub metadata_preset: Option<String>,
    /// Auto import: a watched folder whose new photos are added as they arrive (`None` = off),
    /// copied into the library's Originals instead of added in place when `auto_copy`, and an
    /// album they go to.
    pub auto_folder: Option<String>,
    pub auto_copy: bool,
    pub auto_album: Option<String>,
    /// Auto import is switched off (File ▸ Auto Import ▸ Enable Auto Import unticked) while the watched
    /// folder stays set. Stored inverted so settings written before the switch (folder set, no flag)
    /// keep watching.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub auto_paused: bool,
    /// What Auto Import does beyond adding the files (subfolders, naming, presets, keywords).
    #[serde(skip_serializing_if = "AutoImportOptions::is_default")]
    pub auto_options: AutoImportOptions,
    /// Open the import review when a camera or memory card is connected.
    pub show_on_card: bool,
    /// New photos start with Constrain Crop off (Settings ▸ Import ▸ "Constrain crop by default"
    /// unticked). Off by default: new photos start with it on, so a crop stays inside the photo
    /// after a rotation or lens correction. Photos already in the library keep theirs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub crop_unconstrained: bool,
    /// Synchronize Folder brings in files whose content a photo of the library already has (at
    /// another path) as photos of their own: "Don't import files already in the catalog" turned
    /// off, and kept as it was last left. Off (such files left out) unless turned on.
    pub import_duplicates: bool,
    /// AI Denoise on for new raw photos shot at this ISO or above (`None` = off). Their denoised
    /// pictures are queued after the import, under one row in the activity stack.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denoise_above_iso: Option<u32>,
    /// After an import, Remove Dust runs on the new raw photos in the background (`spot.findDust`; off by default).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub remove_dust: bool,
}

/// Auto Import's settings besides the folder (Lightroom Classic's Auto Import Settings): where it
/// looks, how copies are named, and what every arriving photo gets. All off / empty by default;
/// settings written before these existed load as that.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AutoImportOptions {
    /// Photos in subfolders of the watched folder arrive too (hidden folders, folder links and the
    /// library's own folder are never entered; at most [`AUTO_IMPORT_DEPTH`] levels).
    pub subfolders: bool,
    /// File-name template for copies ([`crate::rename::expand`] tokens; empty = keep the names).
    pub rename: String,
    /// Develop preset applied to arriving photos, by id (`None` = the import defaults only).
    pub preset: Option<String>,
    /// Metadata preset applied to arriving photos, by name.
    pub metadata_preset: Option<String>,
    /// Keywords added to arriving photos.
    pub keywords: Vec<String>,
}

impl AutoImportOptions {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Folder levels Auto Import looks into with Include Subfolders.
pub const AUTO_IMPORT_DEPTH: usize = 16;
/// Directory entries Auto Import looks at in one listing with Include Subfolders.
pub const AUTO_IMPORT_ENTRIES: usize = 200_000;

impl ImportDefaults {
    /// The folder Auto Import watches right now: set and not switched off.
    pub fn auto_active_folder(&self) -> Option<&String> {
        self.auto_folder.as_ref().filter(|_| !self.auto_paused)
    }
}

impl ImportDefaults {
    /// Whether a new photo gets AI Denoise switched on ([`ImportDefaults::denoise_above_iso`]).
    pub fn denoise_on_import(&self, p: &Photo) -> bool {
        self.denoise_above_iso.is_some_and(|min| min > 0 && p.meta.iso.is_some_and(|iso| iso >= min)) && crate::denoise::eligible(p)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CameraDefault {
    /// Make + model (`Meta::camera`).
    pub camera: String,
    /// Preset id; `None` = the the app default for this camera.
    pub preset: Option<String>,
}

impl ImportDefaults {
    /// The preset id that applies to a photo of this kind and camera, if any.
    pub fn preset_for(&self, raw: bool, camera: &str) -> Option<&str> {
        if !raw {
            return self.other_preset.as_deref();
        }
        if self.per_camera
            && let Some(c) = self.cameras.iter().find(|c| !camera.is_empty() && c.camera.eq_ignore_ascii_case(camera))
        {
            return c.preset.as_deref();
        }
        self.raw_preset.as_deref()
    }
}

/// Give a freshly imported photo its default settings: the camera defaults, then the matching
/// default preset ([`ImportDefaults::preset_for`]), remembered as the photo's import look.
pub fn apply_import_defaults(s: &Session, p: &mut Photo) {
    // metadata defaults fill gaps only: the file's own copyright / creator win
    let d = &s.import_defaults;
    if p.meta.copyright.trim().is_empty() && !d.copyright.trim().is_empty() {
        p.meta.copyright = d.copyright.trim().to_string();
    }
    if p.meta.creator.trim().is_empty() && !d.creator.trim().is_empty() {
        p.meta.creator = d.creator.trim().to_string();
    }
    if let Some(mp) = d.metadata_preset.as_ref().and_then(|n| s.metadata_presets.iter().find(|m| m.name.eq_ignore_ascii_case(n))) {
        crate::cmd::metadata::apply_to(&mut p.meta, &mp.fields);
    }
    // a new photo starts with Constrain Crop as the preference says (on unless turned off); it is
    // part of the photo's defaults, like its process, so it is not an edit
    if p.develop.geometry.constrain_crop == d.crop_unconstrained {
        let mut fresh = (*p.develop).clone();
        fresh.geometry.constrain_crop = !d.crop_unconstrained;
        p.develop = std::sync::Arc::new(fresh);
    }
    let base = p.camera_defaults();
    // a raw shown from its embedded JPEG gets the rendered-file default, not the raw one
    let raw = p.develops_raw();
    let preset = s.import_defaults.preset_for(raw, &p.meta.camera).and_then(|id| s.presets.iter().find(|x| x.id == id));
    match preset {
        Some(pr) => {
            let look = std::sync::Arc::new(pr.apply(&base, 1.0));
            p.develop = look.clone();
            p.import_look = (*look != base).then_some(look);
        }
        None => {
            p.develop = std::sync::Arc::new(base);
            p.import_look = None;
        }
    }
    // AI Denoise above the ISO chosen (Settings ▸ Import): part of the look the photo starts from
    if s.import_defaults.denoise_on_import(p) && !p.develop.enhance.denoise_enabled() {
        let mut d = (*p.develop).clone();
        d.enhance.denoise_on = Some(true);
        if !(d.enhance.denoise.is_finite() && d.enhance.denoise > 0.0) {
            d.enhance.denoise = 50.0;
        }
        let look = std::sync::Arc::new(d);
        p.develop = look.clone();
        p.import_look = Some(look);
    }
    // the body's own camera profile, while the photo is on the camera's default (OneSlips, LR-PROF-BODY)
    if p.develop.profile.camera.is_empty()
        && let Some(id) = s.body_default(p).map(str::to_string)
    {
        let mut d = (*p.develop).clone();
        d.profile.camera = id;
        let look = std::sync::Arc::new(d);
        p.develop = look.clone();
        p.import_look = Some(look);
    }
}

/// The web file picker's accept list, derived from [`EXTENSIONS`]. Each extension
/// is listed in both cases: browsers' `accept` matching is case-insensitive per
/// spec, but some implementations (e.g. Windows/Edge) match case-sensitively and
/// rejected uppercase `.NEF` files, so a lowercase-only list silently hid raws
/// such as CR3, NRW, RAF, ORF, RW2, RWL, RAW, PEF and SRW from the picker.
///
/// The picker only offers files this build can actually import: `.avif` has no
/// AV1 decoder in any build, and `.heic`/`.heif` decode only when the codecs'
/// `heif` feature is enabled ([`lcc_codecs::Format::Heif::can_decode`]).
/// [`EXTENSIONS`] itself stays complete — files on disk are still recognised,
/// and import reports them as failed, saying why.
pub fn accept_list() -> String {
    let heif_decodable = lcc_codecs::Format::Heif.can_decode();
    let mut out = String::new();
    for &ext in EXTENSIONS {
        // Skip extensions a web user could pick but this build could not import.
        let pickable = match ext {
            "avif" => false,
            "heic" | "heif" => heif_decodable,
            _ => true,
        };
        if !pickable {
            continue;
        }
        if !out.is_empty() {
            out.push(',');
        }
        out.push('.');
        out.push_str(ext);
        out.push(',');
        out.push('.');
        out.push_str(&ext.to_uppercase());
    }
    out
}

/// An XMP sidecar (`.xmp`, any letter case): read with its photo on import, never a photo itself.
fn is_xmp_sidecar(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
}

pub fn is_supported(path: &Path) -> bool {
    path.extension().is_some_and(|e| EXTENSIONS.contains(&e.to_string_lossy().to_lowercase().as_str()))
}

/// Expand files and folders (recursively) into supported files. `skip` (e.g. the library folder)
/// is never descended into. Each folder's walk is bounded ([`crate::walk::Limits::default`]); a
/// walk that stops at a bound is logged.
pub fn expand(paths: &[String], skip: Option<&Path>) -> Vec<String> {
    expand_within(paths, skip, crate::walk::Limits::default()).0
}

/// [`expand`] with the walk bounded by `limits` → (files, whether a walk stopped at a bound).
pub fn expand_within(paths: &[String], skip: Option<&Path>, limits: crate::walk::Limits) -> (Vec<String>, bool) {
    expand_noting(paths, skip, limits, None)
}

/// [`expand_within`], handing the listing of every folder that is walked to `note`
/// ([`crate::walk::files_in_noting`]).
pub(crate) fn expand_noting(
    paths: &[String],
    skip: Option<&Path>,
    limits: crate::walk::Limits,
    note: Option<crate::walk::NoteFolder>,
) -> (Vec<String>, bool) {
    let e = expand_report_noting(paths, skip, limits, note, true);
    (e.files, e.truncated)
}

/// What expanding import sources found, and what the walks left out ([`expand_report`]), so a
/// review can say that it did not see everything.
#[derive(Clone, Debug, Default)]
pub struct Expansion {
    /// Supported files, folders walked recursively and deduplicated by path, in walk order.
    pub files: Vec<String>,
    /// A walk stopped at a depth or entry bound: there may be more files.
    pub truncated: bool,
    /// Folders whose contents couldn't be listed (permissions, a folder that vanished or went offline).
    pub unreadable: usize,
    /// Files found inside the folders walked that are not a supported type (see [`is_supported`]),
    /// not counting XMP sidecars, which the import reads with their photos. A file named directly
    /// is attempted whatever its extension and is never counted here. Both counts add up over the
    /// sources, so a folder named twice (or inside another source) counts twice.
    pub unsupported: usize,
}

/// [`expand_within`] keeping the walks' diagnostics. A walk that stops at a bound is logged here
/// (once per folder), as it always was.
pub fn expand_report(paths: &[String], skip: Option<&Path>, limits: crate::walk::Limits) -> Expansion {
    expand_report_noting(paths, skip, limits, None, true)
}

/// [`expand_report`] with an option to include or exclude subfolders of directory paths.
pub fn expand_report_subfolders(paths: &[String], skip: Option<&Path>, limits: crate::walk::Limits, subfolders: bool) -> Expansion {
    expand_report_noting(paths, skip, limits, None, subfolders)
}

/// ([`expand_report`], handing the listing of every folder that is walked to `note`, as
/// [`expand_noting`] does.)
fn expand_report_noting(
    paths: &[String],
    skip: Option<&Path>,
    limits: crate::walk::Limits,
    mut note: Option<crate::walk::NoteFolder>,
    subfolders: bool,
) -> Expansion {
    let mut out = Expansion::default();
    for p in paths {
        let p = Path::new(p);
        if skip.is_some_and(|s| p == s) {
            continue;
        }
        if p.is_dir() {
            let walk_limits = if subfolders { limits } else { crate::walk::Limits { max_depth: 1, ..limits } };
            // XMP sidecars are read with their photos, not skipped: they are left out of the count
            let mut sidecars = 0usize;
            let keep = |f: &Path| {
                let keep = is_supported(f);
                if !keep && is_xmp_sidecar(f) {
                    sidecars = sidecars.saturating_add(1);
                }
                keep
            };
            let w = match note.as_mut() {
                Some(note) => crate::walk::files_in_noting(p, skip, walk_limits, keep, &mut **note),
                None => crate::walk::files_in(p, skip, walk_limits, keep),
            };
            let truncated = if subfolders { w.truncated } else { w.entries >= limits.max_entries };
            if truncated {
                log::warn!(
                    "import: stopped looking in {} after {} entries (limits: {} entries, {} folder levels)",
                    p.display(),
                    w.entries,
                    limits.max_entries,
                    limits.max_depth
                );
            }
            out.truncated |= truncated;
            out.unreadable = out.unreadable.saturating_add(w.unreadable);
            out.unsupported = out.unsupported.saturating_add(w.skipped.saturating_sub(sidecars));
            out.files.extend(w.files.into_iter().map(|f| f.to_string_lossy().to_string()));
        } else {
            // explicitly named files are attempted even with an unknown extension (sniffed)
            out.files.push(p.to_string_lossy().to_string());
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.files.retain(|p| seen.insert(p.clone()));
    out
}

/// What a path given on the command line at launch becomes ([`launch_path`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchPath {
    /// Import this (absolute) file or folder, in the background.
    Import(String),
    /// Not imported: `path` (absolute) is a whole drive or the whole home folder, which is never
    /// walked without being chosen in the app (issue #374).
    Refused { path: String, why: String },
    /// Open this (absolute) library folder: the argument was a library's `catalog.snap` or its folder, as a
    /// double-click on a catalog in the file manager gives. Never imported.
    Library(String),
}

/// The library folder `path` names: a library's catalog file (`catalog.snap`, or the v4 store's `catalog.redb`; its
/// folder), or a folder holding one. `None` for anything else (photos, folders of photos).
pub fn library_dir_of(path: &Path) -> Option<PathBuf> {
    #[cfg(not(target_arch = "wasm32"))]
    let names = [lcc_catalog::journal::SNAPSHOT, lcc_catalog::db::DB_FILE];
    #[cfg(target_arch = "wasm32")]
    let names = [lcc_catalog::journal::SNAPSHOT];
    let named = path.file_name().is_some_and(|n| names.iter().any(|c| n.to_string_lossy().eq_ignore_ascii_case(c)));
    if named && path.is_file() {
        return path.parent().map(Path::to_path_buf);
    }
    (path.is_dir() && names.iter().any(|c| path.join(c).is_file())).then(|| path.to_path_buf())
}

/// Resolve a launch argument for importing (issue #374). An empty argument is ignored (`None`);
/// a relative one (`.`, `..`, `photos`; `/` or `\` on Windows, the current drive's root) is
/// resolved against `cwd` — the working folder a launcher picked, often a drive root or
/// `C:\Windows\System32` — and refused when that leaves no folder (`cwd` unknown). A drive
/// or file-system root is replaced by its camera folder (`DCIM`, from `dcim`: a memory card) or
/// refused, and so is the home folder `home`: either would walk most of the disk.
pub fn launch_path(arg: &str, cwd: Option<&Path>, home: Option<&Path>, dcim: impl Fn(&Path) -> Option<PathBuf>) -> Option<LaunchPath> {
    if arg.trim().is_empty() {
        return None;
    }
    let given = Path::new(arg);
    let joined = if given.is_absolute() {
        given.to_path_buf()
    } else {
        match cwd {
            Some(c) => c.join(given),
            None => return Some(LaunchPath::Refused { path: arg.to_string(), why: "a relative path, and the working folder is unknown".into() }),
        }
    };
    // `.` and `..` resolved by name (no file-system access)
    let mut abs = PathBuf::new();
    for c in joined.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                abs.pop();
            }
            other => abs.push(other.as_os_str()),
        }
    }
    let shown = abs.to_string_lossy().to_string();
    if abs.parent().is_none() {
        return Some(match dcim(&abs) {
            Some(d) => LaunchPath::Import(d.to_string_lossy().to_string()),
            None => LaunchPath::Refused { path: shown, why: "a whole drive".into() },
        });
    }
    let key = lcc_catalog::query::folder_key;
    if home.is_some_and(|h| key(&h.to_string_lossy()) == key(&shown)) {
        return Some(LaunchPath::Refused { path: shown, why: "the whole home folder".into() });
    }
    if let Some(dir) = library_dir_of(&abs) {
        return Some(LaunchPath::Library(dir.to_string_lossy().to_string()));
    }
    Some(LaunchPath::Import(shown))
}

/// Probe files (headers + content hash) as an import would.
pub(crate) fn probe_paths(s: &Session, paths: &[String]) -> Vec<Result<ProbeInfo, String>> {
    let progress = ScanProgress::default();
    probe_all(s.media.file_probe.as_ref(), paths, &progress.done, &progress.cancel)
}

/// Probe files with `probe` (a clone of the session's file probe), off the session: on a worker thread.
pub(crate) fn probe_with(probe: &crate::media::FileProbe, paths: &[String]) -> Vec<Result<ProbeInfo, String>> {
    let progress = ScanProgress::default();
    probe_all(Some(probe), paths, &progress.done, &progress.cancel)
}

/// Probe `paths` (several at once), counting each in `done`. Stops before the next file once
/// `cancel` is set: the files not reached come back as errors.
pub(crate) fn probe_all(
    probe: Option<&crate::media::FileProbe>,
    paths: &[String],
    done: &std::sync::atomic::AtomicUsize,
    cancel: &std::sync::atomic::AtomicBool,
) -> Vec<Result<ProbeInfo, String>> {
    use std::sync::atomic::Ordering::Relaxed;
    let Some(probe) = probe.cloned() else {
        return paths
            .iter()
            .map(|p| {
                Ok(ProbeInfo {
                    format: Path::new(p).extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default(),
                    ..Default::default()
                })
            })
            .collect();
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let n = workers(paths.len());
        if n > 1 {
            let mut results: Vec<Option<Result<ProbeInfo, String>>> = vec![None; paths.len()];
            let next = std::sync::atomic::AtomicUsize::new(0);
            let out = std::sync::Mutex::new(&mut results);
            std::thread::scope(|sc| {
                for _ in 0..n {
                    sc.spawn(|| {
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if i >= paths.len() {
                                break;
                            }
                            if cancel.load(Relaxed) {
                                break;
                            }
                            let r = probe(&paths[i]);
                            done.fetch_add(1, Relaxed);
                            out.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(r);
                        }
                    });
                }
            });
            return results.into_iter().map(|r| r.unwrap_or_else(|| Err("not probed".into()))).collect();
        }
    }
    paths
        .iter()
        .map(|p| {
            if cancel.load(Relaxed) {
                return Err("cancelled".into());
            }
            let r = probe(p);
            done.fetch_add(1, Relaxed);
            r
        })
        .collect()
}

/// Copy `src` into `dir` as `name` (default: its own name), made unique with -1, -2…; returns the
/// new path. The copy is verified ([`crate::import_move::copy_new`]: a new file, synced and
/// checked against `probe_hash`, the content hash the probe computed — or compared byte for byte
/// with the source when there is none); a bad one is removed and the photo reported as failed.
fn copy_into(dir: &Path, src: &str, name: Option<&str>, probe_hash: Option<&str>) -> Result<String, String> {
    let name = name
        .map(str::to_string)
        .unwrap_or_else(|| Path::new(src).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "photo".into()));
    let expect = probe_hash.and_then(lcc_preview::Hash128::parse);
    crate::import_move::copy_new(Path::new(src), dir, &name, expect).map(|p| p.to_string_lossy().to_string())
}

/// What a [`scan`] needs from the session, so it can run on another thread (a folder on a network
/// share can take minutes to read).
pub struct ScanInput {
    probe: Option<crate::media::FileProbe>,
    skip: Option<PathBuf>,
    /// The library as it is now (photos are shared with the session, not copied): looked up for
    /// the files the scan finds ([`library_files`]) and the contents it probes
    /// ([`library_hashes`]), on the scan's thread.
    library: std::sync::Arc<lcc_catalog::Catalog>,
    cache: HashMap<String, ProbeInfo>,
}

/// The library's photos of `files`: path → (photo, its summary as a candidate that is there
/// already). One pass over the library, and a record only for a file that was found: a folder of
/// a hundred files in a library of millions costs a hundred records.
fn library_files(library: &lcc_catalog::Catalog, files: &[String]) -> HashMap<PathBuf, (PhotoId, ImportCandidate, Option<String>)> {
    let found: std::collections::HashSet<&Path> = files.iter().map(Path::new).collect();
    let mut by_path = HashMap::new();
    if found.is_empty() {
        return by_path;
    }
    for p in library.photos() {
        if let Source::File { path } = &p.source
            && found.contains(Path::new(path))
        {
            let c = ImportCandidate {
                path: path.clone(),
                format: p.format.clone(),
                kind: p.kind,
                width: p.width,
                height: p.height,
                file_size: p.file_size,
                captured: p.captured.clone(),
                // a file that was only browsed (Local) is not in the library: importing it promotes it
                // (a trashed browse record still counts as the path's)
                duplicate: (!p.local || p.deleted).then(|| "path".into()),
                existing: Some(p.id.0),
                local: p.local && !p.deleted,
                ..Default::default()
            };
            by_path.insert(PathBuf::from(path), (p.id, c, p.content_hash.clone()));
        }
    }
    by_path
}

/// Records that photo `id`, whose catalog key is `key`, has the content `key` stands for: the map
/// is keyed by the content (a Duplicate's `:dup<id>` key is the original's plus a suffix, and a
/// file's own hash is the plain one). Where several photos have the content, the one with the
/// plain key is kept, else the later one.
fn note_content<V>(by_hash: &mut HashMap<String, (bool, V)>, key: &str, value: V) {
    let base = crate::sync::base_hash(key);
    let plain = base.len() == key.len();
    match by_hash.get_mut(base) {
        Some((had_plain, _)) if *had_plain && !plain => {}
        Some(slot) => *slot = (plain, value),
        None => {
            by_hash.insert(base.to_string(), (plain, value));
        }
    }
}

/// Which photo of the library has each of the contents `hashes` (the later one where several
/// have it; a copy that was only browsed in Local isn't in the library). One pass over the
/// library, and an entry only for a content that was asked about.
fn library_hashes<'a>(library: &lcc_catalog::Catalog, hashes: impl Iterator<Item = &'a str>) -> HashMap<String, PhotoId> {
    let wanted: std::collections::HashSet<&str> = hashes.collect();
    let mut by_hash = HashMap::new();
    if wanted.is_empty() {
        return HashMap::new();
    }
    for p in library.photos() {
        if let Some(h) = &p.content_hash
            && !p.local
            && wanted.contains(crate::sync::base_hash(h))
        {
            note_content(&mut by_hash, h, p.id);
        }
    }
    by_hash.into_iter().map(|(h, (_, id))| (h, id)).collect()
}

/// Progress and cancellation of a running [`scan_with`].
#[derive(Default)]
pub struct ScanProgress {
    /// Files to probe, set once the folders are expanded.
    pub total: std::sync::atomic::AtomicUsize,
    pub done: std::sync::atomic::AtomicUsize,
    /// Shared with the scan's row in the activity stack (its ✕ sets it).
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// The result of [`scan_with`]: the candidates, and the probes to keep for the import that follows.
#[derive(Default)]
pub struct ScanOutput {
    pub candidates: Vec<ImportCandidate>,
    pub probes: HashMap<String, ProbeInfo>,
    /// A folder walk stopped at a depth or entry bound: there may be more photos ([`Expansion`]).
    pub truncated: bool,
    /// Folders whose contents couldn't be listed.
    pub unreadable: usize,
    /// Files inside the folders that are not a supported photo type (XMP sidecars not counted).
    pub unsupported: usize,
}

impl ScanInput {
    pub fn new(s: &mut Session, paths: &[String]) -> (Self, Vec<String>) {
        let skip = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
        // (no table of the library here: this is the session's thread, and the library may hold
        // millions of photos. The scan looks up what it finds)
        let library = std::sync::Arc::new(s.catalog.clone());
        let input = ScanInput { probe: s.media.file_probe.clone(), skip, library, cache: std::mem::take(&mut s.import_probes) };
        (input, paths.to_vec())
    }

    /// The folder a scan never enters (the library's own).
    pub(crate) fn skip(&self) -> Option<&Path> {
        self.skip.as_deref()
    }

    /// The library as the scan sees it (shared, for [`crate::sync`], which looks at the same one).
    pub(crate) fn library(&self) -> std::sync::Arc<lcc_catalog::Catalog> {
        std::sync::Arc::clone(&self.library)
    }
}

/// What browsing a folder (Local) has to read: the files of `files` the library has no photo at,
/// in the order the view will show them (see [`browse_order`]). Unlike [`scan_with`] nothing is
/// read, so the import that follows adds the photos batch by batch as it reads them, and the first
/// screen of a 3,000-raw wedding shows within a second instead of after every file was read
/// (measured on 2,933 EOS R5 C CR3s: 30.6 s to the first photo before).
pub fn browse_queue(input: &ScanInput, files: Vec<String>, sort: &lcc_catalog::Sort) -> Vec<String> {
    browse_candidates(input, files, sort).into_iter().filter(|c| c.duplicate.is_none() && !c.local).map(|c| c.path).collect()
}

/// Every file of `files` as a browse candidate, in the order the view will show them: those the
/// library has a photo at are its summaries of them, as [`scan_with`] gives (so a browse still
/// counts them as listed and doesn't forget their records), the rest are unread, for the import.
pub fn browse_candidates(input: &ScanInput, mut files: Vec<String>, sort: &lcc_catalog::Sort) -> Vec<ImportCandidate> {
    let known = library_files(&input.library, &files);
    browse_order(&mut files, sort);
    files
        .into_iter()
        .map(|path| match known.get(Path::new(&path)) {
            Some((_, c, _)) => c.clone(),
            None => ImportCandidate { path, ..Default::default() },
        })
        .collect()
}

/// Order `files` the way a view sorted by `sort` will show them, as far as the file system can
/// tell before the files are read: by modification time for the date sorts (a camera's file time
/// is its capture time, near enough to fill the first screen first), by name, or by size. Other
/// sorts keep the given order. Files whose time or size can't be read go last.
pub fn browse_order(files: &mut [String], sort: &lcc_catalog::Sort) {
    use lcc_catalog::SortKey;
    let asc = sort.ascending;
    match sort.key {
        SortKey::CaptureDate | SortKey::ImportDate | SortKey::EditDate => {
            let mut keyed: Vec<(Option<std::time::SystemTime>, String)> =
                files.iter().map(|f| (std::fs::metadata(f).and_then(|m| m.modified()).ok(), f.clone())).collect();
            keyed.sort_by(|a, b| match (a.0, b.0) {
                (Some(x), Some(y)) => if asc { x.cmp(&y) } else { y.cmp(&x) }.then_with(|| a.1.cmp(&b.1)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.1.cmp(&b.1),
            });
            for (slot, (_, f)) in files.iter_mut().zip(keyed) {
                *slot = f;
            }
        }
        SortKey::FileName => {
            let name = |f: &String| Path::new(f).file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            files.sort_by(|a, b| if asc { name(a).cmp(&name(b)) } else { name(b).cmp(&name(a)) });
        }
        SortKey::FileSize => {
            files.sort_by_cached_key(|f| std::fs::metadata(f).map(|m| m.len()).unwrap_or(0));
            if !asc {
                files.reverse();
            }
        }
        SortKey::Rating | SortKey::Random | SortKey::Custom => {}
    }
}

/// Find and probe what importing `paths` would add, marking duplicates (by path, or by content
/// against the library and earlier candidates). Nothing is added; the probes are kept for the
/// import that follows.
pub fn scan(s: &mut Session, paths: &[String]) -> Vec<ImportCandidate> {
    scan_report(s, paths).candidates
}

/// [`scan`] keeping what the walks left out. The probes are already kept in the session for the
/// import that follows, so `probes` of the output is empty.
pub fn scan_report(s: &mut Session, paths: &[String]) -> ScanOutput {
    let (input, paths) = ScanInput::new(s, paths);
    let mut out = scan_with(input, &paths, &ScanProgress::default());
    s.import_probes = std::mem::take(&mut out.probes);
    out
}

/// [`scan`] without the session, so it can run on a worker thread. Stops early (returning what it
/// has) when `progress.cancel` is set.
pub fn scan_with(input: ScanInput, paths: &[String], progress: &ScanProgress) -> ScanOutput {
    scan_with_subfolders(input, paths, progress, true)
}

/// [`scan_with`] with an option to include or exclude subfolders of directory paths.
pub fn scan_with_subfolders(input: ScanInput, paths: &[String], progress: &ScanProgress, subfolders: bool) -> ScanOutput {
    let expansion = expand_report_subfolders(paths, input.skip.as_deref(), crate::walk::Limits::default(), subfolders);
    let mut out = scan_files(input, expansion.files, progress);
    out.truncated = expansion.truncated;
    out.unreadable = expansion.unreadable;
    out.unsupported = expansion.unsupported;
    out
}

/// [`scan_with`] for files that were found already (`paths` expanded, [`expand`]). The walks'
/// diagnostics ([`ScanOutput::truncated`] and friends) are left empty: who expanded the paths has
/// them ([`expand_report`]).
pub(crate) fn scan_files(mut input: ScanInput, files: Vec<String>, progress: &ScanProgress) -> ScanOutput {
    use std::sync::atomic::Ordering::Relaxed;
    let by_path = library_files(&input.library, &files);
    let todo: Vec<String> = files.iter().filter(|f| !by_path.contains_key(Path::new(f))).cloned().collect();
    progress.total.store(todo.len(), Relaxed);
    // probes from a preceding `scan` are reused when the file is unchanged (same size and modification time)
    let cached: Vec<Option<ProbeInfo>> = todo
        .iter()
        .map(|f| {
            let info = input.cache.remove(f)?;
            // (✕ set while the files' cached probes are checked: no more round trips to the disk; what is
            // not reached counts as not probed, like the files after it)
            if progress.cancel.load(Relaxed) {
                return None;
            }
            // (a file that can't be looked at counts as unchanged; one that can is reused only as it was,
            // size and modification time)
            (std::fs::metadata(f).is_err() || crate::files::probe_still_valid(f, &info)).then_some(info)
        })
        .collect();
    let missing: Vec<String> = todo.iter().zip(&cached).filter(|(_, c)| c.is_none()).map(|(f, _)| f.clone()).collect();
    progress.done.store(todo.len() - missing.len(), Relaxed);
    let mut fresh = probe_all(input.probe.as_ref(), &missing, &progress.done, &progress.cancel).into_iter();
    let probed: Vec<Result<ProbeInfo, String>> =
        cached.into_iter().map(|c| c.map(Ok).unwrap_or_else(|| fresh.next().unwrap_or_else(|| Err("not probed".into())))).collect();
    crate::memory::release();
    let mut probes: HashMap<String, Result<ProbeInfo, String>> = todo.into_iter().zip(probed).collect();
    // (who in the library has a content that was probed: asked once the probes are in)
    let probed_hashes = probes
        .values()
        .filter_map(|r| r.as_ref().ok())
        .filter_map(|info| info.content_hash.as_deref())
        // (and the browsed files the library has by path: they are offered unless it has the content too)
        .chain(by_path.values().filter(|(_, c, _)| c.local).filter_map(|(_, _, h)| h.as_deref()));
    let mut seen_hash: HashMap<String, Option<u64>> =
        library_hashes(&input.library, probed_hashes).into_iter().map(|(h, id)| (h, Some(id.0))).collect();
    let mut out = Vec::with_capacity(files.len());
    let mut kept = HashMap::new();
    for f in files {
        if let Some((id, c, hash)) = by_path.get(Path::new(&f)) {
            let mut c = c.clone();
            c.name = Path::new(&f).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| f.clone());
            // a browsed file the library has the content of (or an earlier file here promotes) is a duplicate
            if c.local
                && let Some(h) = hash
            {
                match seen_hash.get(h) {
                    Some(existing) => {
                        c.duplicate = Some("content".into());
                        c.existing = *existing;
                    }
                    None => {
                        seen_hash.insert(h.clone(), Some(id.0));
                    }
                }
            }
            out.push(c);
            continue;
        }
        let name = Path::new(&f).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| f.clone());
        let mut c = ImportCandidate { path: f.clone(), name, ..Default::default() };
        match probes.remove(&f) {
            Some(Ok(info)) => {
                (c.format, c.kind, c.width, c.height, c.file_size, c.captured) =
                    (info.format.clone(), info.kind, info.width, info.height, info.file_size, info.captured.clone());
                // as the import will date it (a sidecar's date, when there is one, still wins there)
                c.captured = c.captured.or_else(|| info.file_modified.clone());
                c.preview_only = info.preview_only.clone();
                if info.kind == MediaKind::Raw {
                    c.camera = if info.meta.camera.trim().is_empty() {
                        info.meta.camera_model.trim().to_string()
                    } else {
                        info.meta.camera.trim().to_string()
                    };
                    c.body = crate::profile_bodies::key_from(&info.meta.camera_model, &info.meta.camera, &info.meta.camera_serial, &c.name);
                }
                if let Some(h) = &info.content_hash {
                    match seen_hash.get(h) {
                        Some(existing) => {
                            c.duplicate = Some("content".into());
                            c.existing = *existing;
                        }
                        None => {
                            seen_hash.insert(h.clone(), None);
                        }
                    }
                }
                kept.insert(f, info);
            }
            // still being copied in: not listed now, found by a later scan (issue #123)
            Some(Err(e)) if lcc_catalog::incoming::is_still_being_written(&e) => continue,
            Some(Err(e)) => c.error = Some(e),
            None => c.error = Some("not probed".into()),
        }
        out.push(c);
    }
    // (the walks' diagnostics are [`scan_with`]'s: these files were found already)
    ScanOutput { candidates: out, probes: kept, truncated: false, unreadable: 0, unsupported: 0 }
}

/// Import files/folders. See the module docs.
pub fn import(s: &mut Session, paths: &[String], mode: ImportMode) -> crate::Result<ImportReport> {
    import_with(s, paths, &ImportOptions { mode, ..Default::default() })
}

/// Import files/folders with options (preset, keywords). See the module docs.
///
/// The file-system half ([`ImportJob::prepare`]: expanding folders, probing, reading sidecars,
/// copying / placing files) and the catalog half ([`commit_prepared`]) are separate so the app can
/// run the first on a worker thread; here both run in turn.
pub fn import_with(s: &mut Session, paths: &[String], opts: &ImportOptions) -> crate::Result<ImportReport> {
    let mut job = ImportJob::new(s, opts.clone())?;
    let prepared = job.prepare(paths, &std::sync::atomic::AtomicBool::new(false));
    // probes a scan left for files not imported now stay for a later import
    s.import_probes.extend(std::mem::take(&mut job.cache));
    commit_prepared(s, &job.opts, &job.now, prepared)
}

/// Everything the file-system half of an import needs, detached from the session so it can run
/// on another thread (a network share or a card can take minutes). Built by [`ImportJob::new`];
/// [`ImportJob::prepare`] may be called for several batches in turn (renamed copies keep counting,
/// duplicates are found across batches); each batch is then [`commit_prepared`] on the session.
pub struct ImportJob {
    pub opts: ImportOptions,
    /// The mode in effect (Copy / Move become Add in a library without its own folder).
    mode: ImportMode,
    lib_dir: Option<PathBuf>,
    copy_root: Option<PathBuf>,
    /// The library as it was when the import was set up, until it is looked up
    /// ([`ImportJob::look_up_library`], with the first files): setting an import up is the
    /// session's thread, and a table of a library of millions of photos takes seconds. With it,
    /// whether a photo in Recently Deleted is passed over (an import that replaces such photos,
    /// as the options said when the import was set up).
    library: Option<(lcc_catalog::Catalog, bool)>,
    /// path → (photo, a Local browse record?)
    by_path: HashMap<String, (PhotoId, bool, Option<String>)>,
    /// content hash → the photo that has it (`None`: a file earlier in this import)
    by_hash: HashMap<String, Option<PhotoId>>,
    probe: Option<crate::media::FileProbe>,
    file_bytes: Option<crate::merge::ByteReader>,
    /// Probes from a preceding scan (not read again).
    cache: HashMap<String, ProbeInfo>,
    naming: crate::sidecar::SidecarNaming,
    now: String,
    /// Next rename number; `None` once the final representable value has been reserved.
    seq: Option<usize>,
    /// Files found so far (folders expanded) and handled so far, for progress.
    pub total: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub done: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Of those, the files that have to be read first (not probed by a preceding scan) and the
    /// ones read so far: the step before they are handled.
    pub read_total: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub read_done: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// The outcome of [`ImportJob::prepare`] for one batch, in file order.
#[derive(Debug, Default)]
pub struct Prepared {
    pub scanned: usize,
    /// A Cancel left files of the batch undone (they have no item): the batch is not the whole of
    /// what was asked. A Cancel that came after the last file was prepared leaves this `false`.
    pub cut_short: bool,
    /// What [`ImportJob::prepare`]'s folder walk left out.
    pub skipped: Skipped,
    items: Vec<PreparedItem>,
}

#[derive(Debug)]
enum PreparedItem {
    /// A Local (browsed) record imported for real.
    Promote(PhotoId),
    Duplicate {
        path: String,
        existing: Option<PhotoId>,
        reason: &'static str,
        hash: Option<String>,
    },
    Failed(String, String),
    /// Still being copied in: left for a later scan.
    Copying(String),
    Kept(Kept),
    Ready(Box<ReadyFile>),
}

#[derive(Debug)]
struct ReadyFile {
    path: String,
    stored: String,
    info: ProbeInfo,
    sidecar: Option<crate::sidecar::SidecarData>,
    placed: Option<crate::import_move::Placed>,
    /// A Local (browsed) record whose file was copied or moved: the record follows the copy and
    /// joins the library, keeping its culling and edits, instead of a new photo being added.
    promote: Option<PhotoId>,
}

impl Prepared {
    /// Undo what the batch did on disk that only a commit would make permanent: files placed by a
    /// Move (their sources were never touched). Copies stay, like copies of a failed commit.
    pub fn rollback(&self) {
        for it in &self.items {
            if let PreparedItem::Ready(r) = it
                && let Some(pl) = &r.placed
            {
                crate::import_move::rollback(pl);
            }
        }
    }

    /// Entries in the batch (after expanding folders).
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Recheck Add-only entries against the owner catalog immediately before commit.
    /// Preparation may have run while another import added the same path or content.
    pub fn revalidate_add(&mut self, s: &Session) {
        let mut paths = HashMap::new();
        let mut hashes: HashMap<String, (bool, PhotoId)> = HashMap::new();
        for photo in s.catalog.photos() {
            if let Source::File { path } = &photo.source {
                paths.insert(normalize_import_path(path), photo.id);
            }
            if let Some(hash) = &photo.content_hash {
                note_content(&mut hashes, hash, photo.id);
            }
        }
        let mut items = Vec::with_capacity(self.items.len());
        for item in std::mem::take(&mut self.items) {
            let PreparedItem::Ready(ready) = item else {
                items.push(item);
                continue;
            };
            let ReadyFile { path, stored, info, sidecar, placed, promote } = *ready;
            let existing_path = paths.get(&normalize_import_path(&stored)).copied().or_else(|| paths.get(&normalize_import_path(&path)).copied());
            let existing_hash = info.content_hash.as_ref().and_then(|hash| hashes.get(crate::sync::base_hash(hash)).map(|(_, id)| *id));
            if let Some(existing) = existing_path.or(existing_hash) {
                if let Some(placed) = &placed {
                    crate::import_move::rollback(placed);
                }
                items.push(PreparedItem::Duplicate {
                    path,
                    existing: Some(existing),
                    reason: if existing_path.is_some() { "path" } else { "content" },
                    hash: info.content_hash,
                });
            } else {
                items.push(PreparedItem::Ready(Box::new(ReadyFile { path, stored, info, sidecar, placed, promote })));
            }
        }
        self.items = items;
    }
}

fn normalize_import_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    if cfg!(windows) { path.to_lowercase() } else { path }
}

impl ImportJob {
    /// Snapshot what an import with `opts` needs from the session (no file-system calls). Takes
    /// the probes a preceding scan left ([`Session::import_probes`]).
    pub fn new(s: &mut Session, opts: ImportOptions) -> crate::Result<Self> {
        // a recipe is checked before any file is read or placed, not when the photos are committed
        crate::cmd::check_recipe(&opts.recipe).map_err(|e| crate::cmd::bad("library.import", e))?;
        let mode = opts.mode;
        // Only a library on disk has an `Originals/` folder. The browser build keeps the bytes of
        // every added file in its own storage already, so "copy" there means "add".
        let transfer = matches!(mode, ImportMode::Copy | ImportMode::Move);
        let mode = if transfer && s.library.as_ref().is_some_and(|l| !l.on_disk) { ImportMode::Add } else { mode };
        if mode == ImportMode::Move && opts.local {
            return Err(crate::EngineError::Other("browsing a folder never moves its files".into()));
        }
        let lib_dir = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
        // where copies go: the chosen folder, else the library's Originals/
        let copy_root = opts
            .destination
            .as_deref()
            .filter(|d| !d.trim().is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| lib_dir.as_ref().map(|l| l.join("Originals")));
        if matches!(mode, ImportMode::Copy | ImportMode::Move) && copy_root.is_none() {
            return Err(crate::EngineError::Other("copying or moving needs a destination folder or an open catalog".into()));
        }
        let fresh = opts.on_deleted == OnDeleted::Fresh;
        Ok(ImportJob {
            seq: Some(opts.rename_start.max(1)),
            opts,
            mode,
            lib_dir,
            copy_root,
            library: Some((s.catalog.clone(), fresh)),
            by_path: HashMap::new(),
            by_hash: HashMap::new(),
            probe: s.media.file_probe.clone(),
            file_bytes: s.media.file_bytes.clone(),
            cache: std::mem::take(&mut s.import_probes),
            naming: s.xmp.naming,
            now: (s.clock)(),
            total: Default::default(),
            done: Default::default(),
            read_total: Default::default(),
            read_done: Default::default(),
        })
    }

    /// The file-system half of importing `paths`: expand folders, skip known paths, probe, skip
    /// duplicates by content, read sidecars, copy or place (Move) the files — several at once,
    /// with the names and outcomes of one after another. Nothing in the catalog changes. Stops
    /// before the next file once `cancel` is set.
    pub fn prepare(&mut self, paths: &[String], cancel: &std::sync::atomic::AtomicBool) -> Prepared {
        let found = self.expand_report(paths);
        let mut prepared = self.prepare_files(found.files, cancel);
        prepared.skipped = Skipped { unreadable: found.unreadable, unsupported: found.unsupported };
        prepared
    }

    /// Expand `paths` (folders recursively, the library's own folder skipped) into the files an
    /// import would handle; counted in [`ImportJob::total`].
    pub fn expand(&self, paths: &[String]) -> Vec<String> {
        self.expand_report(paths).files
    }

    /// [`ImportJob::expand`], with what the folder walk left out.
    fn expand_report(&self, paths: &[String]) -> Expansion {
        let found = expand_report(paths, self.lib_dir.as_deref(), crate::walk::Limits::default());
        self.total.fetch_add(found.files.len(), std::sync::atomic::Ordering::Relaxed);
        found
    }

    /// Whether `path` is copied or moved into the destination (else it is added where it is).
    fn transfers(&self, path: &str) -> bool {
        matches!(self.mode, ImportMode::Copy | ImportMode::Move)
            && self.copy_root.as_ref().is_some_and(|root| {
                !crate::import_move::inside(Path::new(path), root)
                    && !self.lib_dir.as_ref().is_some_and(|l| crate::import_move::inside(Path::new(path), l))
            })
    }

    /// When the import started ([`Session::clock`]), for [`commit_prepared`].
    pub fn now(&self) -> &str {
        &self.now
    }

    /// What the library held when the import was set up, by path and by content: made once, with
    /// the first files (on the thread that prepares them), from the library as it was then. The
    /// snapshot comes back: the files the import is adding are ranked against it for the ownership
    /// of the shared stem sidecars they read ([`crate::sidecar::ImportStems`]).
    fn look_up_library(&mut self) -> Option<lcc_catalog::Catalog> {
        let (library, fresh) = self.library.take()?;
        let mut by_hash = HashMap::new();
        for p in library.photos() {
            // a trashed photo is replaced by a fresh import, so it doesn't count as a duplicate
            if p.deleted && fresh {
                continue;
            }
            if let Source::File { path } = &p.source {
                self.by_path.insert(path.clone(), (p.id, p.local, p.content_hash.clone()));
            }
            // importing suspected duplicates: only a trashed photo's content still matches
            // (and a copy that was only browsed in Local isn't in the library)
            if let Some(h) = &p.content_hash
                && !p.local
                && (p.deleted || !self.opts.import_duplicates)
            {
                note_content(&mut by_hash, h, Some(p.id));
            }
        }
        self.by_hash.extend(by_hash.into_iter().map(|(h, (_, id))| (h, id)));
        Some(library)
    }

    /// [`ImportJob::prepare`] for files [`ImportJob::expand`] returned.
    pub fn prepare_files(&mut self, files: Vec<String>, cancel: &std::sync::atomic::AtomicBool) -> Prepared {
        use std::sync::atomic::Ordering::Relaxed;
        // the library as it was, kept for the ownership of the shared stem sidecars below
        let library = self.look_up_library();
        let opts = self.opts.clone();
        let mode = self.mode;
        let mut out = Prepared { scanned: files.len(), ..Default::default() };
        let mut todo = Vec::new();
        // browsed (Local) records whose files are copied or moved: the record follows its file
        let mut follow: HashMap<String, PhotoId> = HashMap::new();
        for f in files {
            match self.by_path.get(f.as_str()).cloned() {
                Some((id, true, hash)) if !opts.local && self.transfers(&f) => {
                    self.by_path.insert(f.clone(), (id, false, hash));
                    follow.insert(f.clone(), id);
                    todo.push(f);
                }
                Some((id, true, hash)) if !opts.local => {
                    // a library photo with the same content comes first, and a file promoted earlier
                    // (here or in a previous batch) counts as the library's from then on
                    if let Some(h) = &hash {
                        if let Some(existing) = self.by_hash.get(h) {
                            out.items.push(PreparedItem::Duplicate { path: f, existing: *existing, reason: "content", hash });
                            self.done.fetch_add(1, Relaxed);
                            continue;
                        }
                        self.by_hash.insert(h.clone(), Some(id));
                    }
                    // a file that was only browsed (Local) joins the library when it is imported for real
                    self.by_path.insert(f, (id, false, hash));
                    out.items.push(PreparedItem::Promote(id));
                    self.done.fetch_add(1, Relaxed);
                }
                Some((id, _, _)) => {
                    out.items.push(PreparedItem::Duplicate { path: f, existing: Some(id), reason: "path", hash: None });
                    self.done.fetch_add(1, Relaxed);
                }
                None => todo.push(f),
            }
        }

        // files a preceding scan already probed are not read again (a network share is slow to read),
        // as long as they are still there, as they were: a file gone or changed since is probed again
        let cached: Vec<Option<ProbeInfo>> = todo
            .iter()
            .map(|f| {
                // (the web's files are not std::fs files: kept as probed)
                self.cache.remove(f).filter(|info| cfg!(target_arch = "wasm32") || crate::files::probe_still_valid(f, info))
            })
            .collect();
        let missing: Vec<String> = todo.iter().zip(&cached).filter(|(_, c)| c.is_none()).map(|(f, _)| f.clone()).collect();
        // Reading the files is the slow part of adding them where they are: it is counted on its
        // own, and `cancel` stops it too (a large library takes long enough to want both).
        self.read_total.fetch_add(missing.len(), Relaxed);
        let mut fresh = probe_all(self.probe.as_ref(), &missing, &self.read_done, cancel).into_iter();
        let probed: Vec<Result<ProbeInfo, String>> =
            cached.into_iter().map(|c| c.map(Ok).unwrap_or_else(|| fresh.next().unwrap_or_else(|| Err("not probed".into())))).collect();
        crate::memory::release();
        // Who owns the shared stem sidecar (`IMG_0001.xmp`) among the library's files and the ones
        // arriving with this import: a JPEG or a TIFF arriving beside its raw reads its own sidecar
        // and never the raw's packet (issue #1905).
        let stems = crate::sidecar::ImportStems::for_import(
            library.as_ref(),
            todo.iter().zip(&probed).filter_map(|(path, info)| info.as_ref().ok().map(|i| (path.as_str(), i.kind))),
        );
        // Files are planned one by one, in order (duplicates, sidecars, names, folders); their
        // copies or moves — the slow part, reading and writing every byte and verifying it — are
        // collected and run on several threads (issue #367), then their outcomes take their places.
        let mut plan = Plan::default();
        // sidecar names are answered from one listing per folder (round trips cost on a network share)
        let mut listing = crate::sidecar::FolderListing::default();
        for (path, info) in todo.into_iter().zip(probed) {
            if cancel.load(Relaxed) {
                out.cut_short = true;
                break;
            }
            let mut info = match info {
                Ok(i) => i,
                // still being copied in: skipped, and picked up by a later scan (issue #123)
                Err(e) if lcc_catalog::incoming::is_still_being_written(&e) => {
                    log::info!("import: skipped {e}");
                    self.done.fetch_add(1, Relaxed);
                    plan.slots.push(Slot::Item(PreparedItem::Copying(path)));
                    continue;
                }
                Err(e) => {
                    log::warn!("import {path}: {e}");
                    self.done.fetch_add(1, Relaxed);
                    plan.slots.push(Slot::Item(PreparedItem::Failed(path, e)));
                    continue;
                }
            };
            let promote = follow.get(&path).copied();
            // a copy of a file whose transfer is still pending: a duplicate only if that one succeeds
            if !opts.local
                && let Some(h) = &info.content_hash
                && plan.transfers.iter().any(|t| t.info.content_hash.as_ref() == Some(h))
            {
                self.run_plan(&mut plan, &mut out, cancel);
                if cancel.load(Relaxed) {
                    out.cut_short = true;
                    break;
                }
            }
            if let Some(h) = &info.content_hash
                && !opts.local
                && let Some(existing) = self.by_hash.get(h)
                // (the browsed record of this very file is not a duplicate of it)
                && !(promote.is_some() && *existing == promote)
            {
                self.done.fetch_add(1, Relaxed);
                plan.slots.push(Slot::Item(PreparedItem::Duplicate { path, existing: *existing, reason: "content", hash: Some(h.clone()) }));
                continue;
            }
            // the XMP sidecar (or the XMP embedded in a raw or a JPEG, as the probe found it), read before
            // copying: its capture time files and names the copy when the file itself has none. A sidecar
            // found keeps precedence even when it can't be read, as on a manual reread
            // (`sidecar::read_packet`): the embedded values don't apply (upstream #3607).
            let raw = info.kind == MediaKind::Raw;
            let packet = match crate::sidecar::find_sidecar_for_import(&stems, self.naming, &path, &mut listing) {
                Some(f) => std::fs::read_to_string(&f).inspect_err(|e| log::warn!("import {path}: sidecar {}: {e}", f.display())).ok(),
                None => info.xmp.clone(),
            };
            let target = crate::crs::Target::for_file(info.kind, &info.format, info.preview_only.is_some(), &info.meta.camera);
            let sidecar = packet.and_then(|x| match crate::sidecar::parse_sidecar(&x, target) {
                Ok(sc) => Some(sc),
                Err(e) => {
                    log::warn!("import {path}: XMP: {e}");
                    None
                }
            });
            if info.captured.is_none() {
                info.captured = sidecar.as_ref().and_then(|sc| sc.captured.clone());
            }
            // no date in the file or its sidecar: the file's own date, as Lightroom does (a guess:
            // never written to a sidecar as the capture time)
            if info.captured.is_none() {
                info.captured = info.file_modified.clone();
                info.captured_is_file_date = info.captured.is_some();
            }
            let stored = match (mode, &self.copy_root) {
                (ImportMode::Copy | ImportMode::Move, Some(root))
                    if !crate::import_move::inside(Path::new(&path), root)
                        && !self.lib_dir.as_ref().is_some_and(|l| crate::import_move::inside(Path::new(&path), l)) =>
                {
                    // the templates see the photo as it will be catalogued (undated: the import time)
                    let own = Path::new(&path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let mut q = Photo::new(PhotoId(0), Source::File { path: path.clone() }, &own, &info.format, 0, 0, &self.now);
                    q.captured = info.captured.clone();
                    q.meta = info.meta.clone();
                    q.rating = info.rating;
                    q.flag = info.flag;
                    q.label = info.label;
                    if let Some(sc) = &sidecar {
                        crate::sidecar::merge_into(&mut q, sc, &self.now);
                    }
                    let name = if let Some(template) = opts.rename.as_deref().filter(|t| !t.trim().is_empty()) {
                        let Some(seq) = self.seq else {
                            self.done.fetch_add(1, Relaxed);
                            plan.slots.push(Slot::Item(PreparedItem::Failed(
                                path,
                                "rename sequence exhausted; choose a lower renameStart or leave rename blank".into(),
                            )));
                            continue;
                        };
                        self.seq = seq.checked_add(1);
                        Some(crate::rename::expand(template, &q, seq))
                    } else {
                        None
                    };
                    let folders = opts.organize.folders(&q);
                    let dir = folders.iter().fold(root.to_path_buf(), |d, f| d.join(f));
                    // the name this file gets depends on what a pending transfer of a like-named file
                    // into the same folder ends up with: that one goes first
                    let as_named = name.as_deref().unwrap_or(if own.is_empty() { "photo" } else { own.as_str() });
                    let family = (dir.to_string_lossy().to_lowercase(), name_family(as_named));
                    if plan.transfers.iter().any(|t| t.family == family) {
                        self.run_plan(&mut plan, &mut out, cancel);
                        if cancel.load(Relaxed) {
                            out.cut_short = true;
                            break;
                        }
                    }
                    let how = if mode == ImportMode::Move && !crate::import_move::is_symlink(Path::new(&path)) {
                        How::Place { name: name.unwrap_or(own) }
                    } else {
                        if mode == ImportMode::Move {
                            let reason = "a symbolic link: the file it points to was copied, the link and its target stay";
                            plan.slots.push(Slot::Item(PreparedItem::Kept(Kept { path: path.clone(), reason: reason.into() })));
                        }
                        How::Copy { name }
                    };
                    // Copy as DNG: the copied raw becomes a DNG (the copy is ours to replace)
                    let dng = opts.convert_dng && mode == ImportMode::Copy && raw && !info.format.eq_ignore_ascii_case("DNG");
                    plan.slots.push(Slot::Transfer);
                    plan.transfers.push(Transfer { path, info, sidecar, dir, how, dng, promote, family });
                    continue;
                }
                (ImportMode::Move, _) => {
                    let reason = "already inside the destination or the library: added where it is";
                    plan.slots.push(Slot::Item(PreparedItem::Kept(Kept { path: path.clone(), reason: reason.into() })));
                    path.clone()
                }
                _ => path.clone(),
            };
            if let Some(h) = &info.content_hash
                && !self.opts.import_duplicates
            {
                self.by_hash.insert(h.clone(), None);
            }
            self.done.fetch_add(1, Relaxed);
            plan.slots.push(Slot::Item(match promote {
                // (planned for a transfer that turned out not to need one: joins where it is)
                Some(id) => PreparedItem::Promote(id),
                None => PreparedItem::Ready(Box::new(ReadyFile { path, stored, info, sidecar, placed: None, promote: None })),
            }));
        }
        self.run_plan(&mut plan, &mut out, cancel);
        // (`cut_short` says whether a Cancel stopped a file from being prepared: the flag itself may
        // have been set after the last one was)
        out
    }

    /// Run the transfers `plan` collected (on several threads) and append its items to `out` in
    /// file order. Transfers not started because `cancel` was set leave no item.
    fn run_plan(&mut self, plan: &mut Plan, out: &mut Prepared, cancel: &std::sync::atomic::AtomicBool) {
        let mut done = run_transfers(std::mem::take(&mut plan.transfers), self.file_bytes.as_ref(), cancel, &self.done).into_iter();
        for slot in std::mem::take(&mut plan.slots) {
            let item = match slot {
                Slot::Item(it) => it,
                Slot::Transfer => match done.next().flatten() {
                    Some(it) => it,
                    None => {
                        out.cut_short = true;
                        continue;
                    }
                },
            };
            if let PreparedItem::Ready(r) = &item
                && let Some(h) = &r.info.content_hash
                && !self.opts.import_duplicates
            {
                self.by_hash.insert(h.clone(), None);
            }
            out.items.push(item);
        }
    }
}

/// A batch's items in file order while its transfers are pending ([`ImportJob::run_plan`]).
#[derive(Default)]
struct Plan {
    slots: Vec<Slot>,
    transfers: Vec<Transfer>,
}

enum Slot {
    Item(PreparedItem),
    /// The outcome of the next of [`Plan::transfers`].
    Transfer,
}

/// A file to copy or move into `dir`, planned in order and run on any thread ([`run_transfer`]).
struct Transfer {
    path: String,
    info: ProbeInfo,
    sidecar: Option<crate::sidecar::SidecarData>,
    dir: PathBuf,
    how: How,
    /// Copy as DNG.
    dng: bool,
    /// The browsed record that follows the copy ([`ReadyFile::promote`]).
    promote: Option<PhotoId>,
    /// The folder (lowercase) and [`name_family`]: transfers of one family can take each other's
    /// names, so they never run at the same time.
    family: (String, String),
}

enum How {
    /// A verified copy, under `name` (default: the file's own).
    Copy { name: Option<String> },
    /// Move: placed (linked or copied) without touching the source.
    Place { name: String },
}

/// The names a file called `name` could end up with, as one key: the transfers number a taken name
/// `IMG-1.JPG`, `IMG-2.JPG`…, and Copy as DNG names its DNG `IMG.dng`, `IMG-2.dng`…, so `IMG.JPG`,
/// `IMG-1.jpg` and `img.dng` are all `img` (case-insensitive: so are some file systems).
fn name_family(name: &str) -> String {
    let mut stem = match name.rsplit_once('.') {
        Some((s, _)) if !s.is_empty() => s,
        _ => name,
    };
    while let Some((head, tail)) = stem.rsplit_once('-')
        && !head.is_empty()
        && !tail.is_empty()
        && tail.bytes().all(|b| b.is_ascii_digit())
    {
        stem = head;
    }
    stem.to_lowercase()
}

/// Threads for `jobs` file reads (probes, copies) at once.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn workers(jobs: usize) -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8).min(jobs.max(1))
}

/// Files per batch for a caller that prepares an import in batches ([`ImportJob::prepare_files`]):
/// enough to keep every probe and copy thread busy (a batch waits for its slowest file), few
/// enough that each batch soon joins the catalog.
pub fn batch_size() -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        workers(usize::MAX).saturating_mul(2).clamp(8, 16)
    }
    #[cfg(target_arch = "wasm32")]
    {
        8
    }
}

/// Run `transfers` (several at once where threads exist); their outcomes in order, `None` for one
/// not started because `cancel` was set. `done` counts each one finished.
fn run_transfers(
    transfers: Vec<Transfer>,
    file_bytes: Option<&crate::merge::ByteReader>,
    cancel: &std::sync::atomic::AtomicBool,
    done: &std::sync::atomic::AtomicUsize,
) -> Vec<Option<PreparedItem>> {
    use std::sync::atomic::Ordering::Relaxed;
    let one = |t: Transfer| {
        if cancel.load(Relaxed) {
            return None;
        }
        let path = t.path.clone();
        // last-resort guard: a panic is this file's failure, not the import's
        let item = crate::guard::catch("import", || run_transfer(t, file_bytes)).unwrap_or_else(|e| PreparedItem::Failed(path, e));
        done.fetch_add(1, Relaxed);
        Some(item)
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let n = workers(transfers.len());
        if n > 1 {
            let mut results: Vec<Option<PreparedItem>> = std::iter::repeat_with(|| None).take(transfers.len()).collect();
            let queue = std::sync::Mutex::new(transfers.into_iter().enumerate());
            let out = std::sync::Mutex::new(&mut results);
            // (the tests' injected faults hold for the workers too)
            #[cfg(test)]
            let fault = crate::import_move::injected_fault();
            let work = &|| {
                #[cfg(test)]
                crate::import_move::inject(fault);
                loop {
                    let next = queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).next();
                    let Some((i, t)) = next else { break };
                    let r = one(t);
                    if let Some(slot) = out.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_mut(i) {
                        *slot = r;
                    }
                }
            };
            std::thread::scope(|sc| {
                let handles: Vec<_> =
                    (0..n).filter_map(|i| std::thread::Builder::new().name(format!("lc-import-copy-{i}")).spawn_scoped(sc, work).ok()).collect();
                if handles.is_empty() {
                    // no thread to be had: copy here, one after another
                    work();
                }
                for h in handles {
                    if h.join().is_err() {
                        log::error!("import: a copy worker stopped unexpectedly");
                    }
                }
            });
            return results;
        }
    }
    transfers.into_iter().map(one).collect()
}

/// Copy or move one planned file (and Copy as DNG); its item.
fn run_transfer(t: Transfer, file_bytes: Option<&crate::merge::ByteReader>) -> PreparedItem {
    let Transfer { path, mut info, sidecar, dir, how, dng, promote, .. } = t;
    let mut placed = None;
    let mut stored = match how {
        How::Place { name } => match crate::import_move::place(Path::new(&path), &dir, &name) {
            Ok(pl) => {
                let dst = pl.dst.to_string_lossy().to_string();
                placed = Some(pl);
                dst
            }
            Err(e) => return PreparedItem::Failed(path, e),
        },
        How::Copy { name } => match copy_into(&dir, &path, name.as_deref(), info.content_hash.as_deref()) {
            Ok(p) => p,
            Err(e) => return PreparedItem::Failed(path, e),
        },
    };
    if dng {
        // decoding the raw twice and encoding it takes several times the file's size in memory
        let weight = usize::try_from(info.file_size).unwrap_or(usize::MAX).saturating_mul(6);
        let _permit = crate::memory::work_gate().acquire(weight);
        match crate::cmd::convert::write_dng_with(file_bytes, &stored, String::new()) {
            Ok(dng) => {
                let _ = std::fs::remove_file(&stored);
                stored = dng;
                info.format = "DNG".into();
            }
            Err(e) => log::warn!("import {path}: copy as DNG: {e}"),
        }
    }
    PreparedItem::Ready(Box::new(ReadyFile { path, stored, info, sidecar, placed, promote }))
}

/// A trashed photo's file is not where the library had it (never on the web, where paths are
/// storage keys, not files).
fn trashed_file_is_gone(cat: &lcc_catalog::Catalog, id: PhotoId) -> bool {
    !cfg!(target_arch = "wasm32")
        && match cat.photo(id).map(|p| &p.source) {
            Some(Source::File { path }) => !Path::new(path).exists(),
            _ => false,
        }
}

/// The catalog half of an import: add the photos [`ImportJob::prepare`] readied (one undoable op;
/// a Move's sources are removed once it is saved). On a failed commit a Move's placed files are
/// taken back.
pub fn commit_prepared(s: &mut Session, opts: &ImportOptions, now: &str, prepared: Prepared) -> crate::Result<ImportReport> {
    let now = now.to_string();
    let mut report = ImportReport { scanned: prepared.scanned, skipped: prepared.skipped, ..Default::default() };
    let mut ops = Vec::new();
    // Move: placed at the destination, sources untouched until the records are committed
    let mut placed: Vec<crate::import_move::Placed> = Vec::new();
    // plug-in fields from sidecars: into the plug-ins' data once the photos are catalogued
    let mut plugin_fields: Vec<(PhotoId, Vec<(String, String, String)>)> = Vec::new();
    // photos added by this batch, by content (a duplicate of a file earlier in the import names it)
    let mut new_hash: HashMap<String, PhotoId> = HashMap::new();
    // Fresh: trashed photos a new import replaces (by content or by file), each removed once
    let mut trash_content: HashMap<String, (bool, PhotoId)> = HashMap::new();
    let mut trash_path: HashMap<String, PhotoId> = HashMap::new();
    if opts.on_deleted == OnDeleted::Fresh {
        for p in s.catalog.photos().filter(|p| p.deleted) {
            if let Some(h) = &p.content_hash {
                note_content(&mut trash_content, h, p.id);
            }
            if let Source::File { path } = &p.source {
                trash_path.insert(path.clone(), p.id);
            }
        }
    }
    // "Don't Import Suspected Duplicates" off: contents already in the library (or earlier in this
    // import), so a copy gets its own `<hash>:dup<id>` key, as Photo > Duplicate does
    let mut live_hash: std::collections::HashSet<String> = if opts.import_duplicates {
        s.catalog.photos().filter(|p| !p.deleted).filter_map(|p| p.content_hash.clone()).collect()
    } else {
        Default::default()
    };
    let mut purge: Vec<PhotoId> = Vec::new();
    let mut purged: std::collections::HashSet<PhotoId> = Default::default();
    for it in prepared.items {
        match it {
            PreparedItem::Promote(id) => {
                if s.catalog.photo(id).is_some_and(|p| p.local) {
                    ops.push(Op::SetLocal { id, local: false });
                    promote_ops(s, id, opts, &now, &mut ops);
                }
                // "Import as new": a trashed photo with the same content is replaced, as for a new file
                if let Some(old) = s
                    .catalog
                    .photo(id)
                    .and_then(|p| p.content_hash.as_ref())
                    .and_then(|h| trash_content.get(crate::sync::base_hash(h)).map(|(_, id)| id))
                    && purged.insert(*old)
                {
                    purge.push(*old);
                }
                report.imported.push(id.0);
                if s.catalog.photo(id).is_some_and(|p| !p.untouched_local()) {
                    report.promoted_changed.push(id.0);
                }
            }
            PreparedItem::Duplicate { path, existing, reason, hash } => {
                let existing = existing.or_else(|| {
                    let h = hash?;
                    new_hash
                        .get(&h)
                        .copied()
                        .or_else(|| s.catalog.photos().find(|p| !p.local && p.content_hash.as_deref() == Some(h.as_str())).map(|p| p.id))
                        // a Duplicate's `<hash>:dup<id>` stands for its content too
                        .or_else(|| {
                            s.catalog
                                .photos()
                                .find(|p| !p.local && p.content_hash.as_deref().is_some_and(|k| crate::sync::base_hash(k) == h.as_str()))
                                .map(|p| p.id)
                        })
                });
                let trashed = existing.filter(|id| s.catalog.photo(*id).is_some_and(|p| p.deleted));
                match trashed {
                    // (browsing a folder in Local only looks: it never restores)
                    Some(id) if opts.on_deleted == OnDeleted::Restore && !opts.local => {
                        if !report.restored.contains(&id.0) {
                            ops.push(Op::SetDeleted { id, deleted: false });
                            // matched by content from another path, and the file the record
                            // points at is gone (moved, renamed): the photo follows the file
                            // (not onto a path another record holds, a file only browsed in Local)
                            if reason == "content"
                                && trashed_file_is_gone(&s.catalog, id)
                                && !s.catalog.photos().any(|p| matches!(&p.source, Source::File { path: held } if *held == path))
                            {
                                ops.push(crate::cmd::missing::relink_op(id, &path));
                            }
                            report.restored.push(id.0);
                        }
                    }
                    _ => report.duplicates.push(Duplicate { path, existing: existing.map(|i| i.0), reason, existing_deleted: trashed.is_some() }),
                }
            }
            PreparedItem::Failed(path, e) => report.failed.push((path, e)),
            PreparedItem::Copying(path) => report.still_copying.push(path),
            PreparedItem::Kept(k) => report.kept.push(k),
            PreparedItem::Ready(r) if r.promote.is_some_and(|id| s.catalog.photo(id).is_some_and(|p| p.local)) => {
                // a browsed photo copied or moved: its record follows the file into the library,
                // keeping its culling and edits
                let ReadyFile { stored, info, placed: pl, promote, .. } = *r;
                placed.extend(pl);
                let Some(id) = promote else { continue };
                let file_name = Path::new(&stored).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| stored.clone());
                let format = s.catalog.photo(id).filter(|p| p.format != info.format).map(|_| info.format.clone());
                ops.push(Op::Relink { id, file_name, source: Source::File { path: stored.clone() }, format });
                ops.push(Op::SetLocal { id, local: false });
                promote_ops(s, id, opts, &now, &mut ops);
                report.imported.push(id.0);
            }
            PreparedItem::Ready(r) => {
                let ReadyFile { path, stored, info, sidecar, placed: pl, .. } = *r;
                placed.extend(pl);
                let id = s.catalog.alloc_photo_id();
                if let Some(h) = &info.content_hash {
                    new_hash.insert(h.clone(), id);
                }
                for old in info.content_hash.as_ref().and_then(|h| trash_content.get(h)).map(|(_, id)| id).into_iter().chain(trash_path.get(&stored))
                {
                    if purged.insert(*old) {
                        purge.push(*old);
                    }
                }
                let sidecar = sidecar.map(|sc| sc.resolve_label(&s.catalog)).filter(|sc| *sc != crate::sidecar::SidecarData::default());
                // the photo is upright as the file's own orientation turns it, when the file states one
                let sidecar = sidecar.map(|sc| crate::sidecar::SidecarData {
                    orientation: info.orientation.or(sc.orientation),
                    ..sc.turn_from(info.orientation).crop_from(info.orientation, (info.width, info.height))
                });
                // a copy is catalogued under its new name
                let name = Path::new(&stored).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.clone());
                let mut p = Photo::new(id, Source::File { path: stored }, &name, &info.format, info.width, info.height, &now);
                p.kind = info.kind;
                p.file_size = info.file_size;
                p.captured_fallback = info.captured.clone().filter(|_| info.captured_is_file_date);
                p.captured = info.captured;
                p.meta = info.meta;
                p.rating = info.rating;
                p.flag = info.flag;
                p.label = info.label_text.as_deref().and_then(|t| s.catalog.label_from_name(t)).or(info.label);
                p.as_shot_wb = info.as_shot_wb;
                p.content_hash = info.content_hash;
                if opts.import_duplicates
                    && let Some(h) = p.content_hash.clone()
                    && !live_hash.insert(h.clone())
                {
                    p.content_hash = Some(format!("{h}:dup{}", id.0));
                }
                p.sha1 = info.sha1.filter(|h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()));
                p.embedded_lens = info.embedded_lens.map(Box::new);
                p.set_preview_only(info.preview_only.clone());
                // a clip's stream facts land with the photo record: one undo step with the import
                p.video = info.video;
                p.duration = info.duration;
                apply_import_defaults(s, &mut p);
                if let Some(sc) = &sidecar {
                    if !sc.plugin_fields.is_empty() {
                        plugin_fields.push((id, sc.plugin_fields.clone()));
                    }
                    crate::sidecar::merge_into(&mut p, sc, &now);
                    // a flat-field map our own sidecar carried is stored ahead of the photo that uses it
                    ops.extend(sc.vignetting_calibration_ops(&s.catalog, &now));
                    // a Lightroom edit (crs: settings) keeps Constrain Crop as it always read: off
                    if matches!(sc.develop, Some(crate::sidecar::DevelopPatch::Partial(_))) && p.develop.geometry.constrain_crop {
                        let mut d = (*p.develop).clone();
                        d.geometry.constrain_crop = false;
                        p.develop = std::sync::Arc::new(d);
                    }
                    report.sidecars += 1;
                    report.warnings.extend(sc.warnings.iter().cloned().map(|mut warning| {
                        warning.path = path.clone();
                        warning.photo = Some(id.0);
                        warning
                    }));
                }
                // the camera profile chosen for this import (a sidecar that sets develop settings keeps its own)
                if sidecar.as_ref().is_none_or(|sc| sc.develop.is_none())
                    && let Some(id) = opts.camera_profile.for_photo(s, &p, true)
                    && p.develop.profile.camera != id
                {
                    let mut d = (*p.develop).clone();
                    d.profile.camera = id;
                    let look = std::sync::Arc::new(d);
                    p.develop = look.clone();
                    p.import_look = Some(look);
                }
                if p.develops_raw() {
                    let id = p.develop.profile.camera.clone();
                    let e = report.camera_profiles.entry(id.clone()).or_insert_with(|| crate::profile_bodies::ProfileCount {
                        name: crate::profile_bodies::profile_name(&id),
                        ..Default::default()
                    });
                    e.photos += 1;
                    if let Some(b) = crate::profile_bodies::key_of(&p)
                        && !e.bodies.contains(&b)
                    {
                        e.bodies.push(b);
                    }
                }
                apply_settings(s, &mut p, opts, &now);
                // the batch-edit recipe goes on top of everything above (sidecar, camera default,
                // preset): only the controls it names change, through the same specs the sliders use
                if !opts.recipe.is_empty() {
                    let mut d = (*p.develop).clone();
                    for (id, v) in &opts.recipe {
                        // (`ImportJob::new` refused any id `set` would not write, and `set` clamps to the range)
                        lcc_develop::controls::set(&mut d, id, *v);
                    }
                    let d = std::sync::Arc::new(d);
                    p.develop = d.clone();
                    p.edited = Some(now.clone());
                    p.history.push(lcc_catalog::HistoryStep { label: "Import adjustments".into(), settings: d, batch: None });
                }
                // the photo as the import leaves it, where that is not simply its import defaults:
                // the Before view's default (`Photo::before_base`)
                p.import_state = (*p.develop != p.import_defaults()).then(|| p.develop.clone());
                report.imported.push(id.0);
                p.local = opts.local;
                if opts.local {
                    // the state as browsed: a later change keeps the record from being forgotten
                    p.set_local_baseline();
                }
                ops.push(Op::AddPhoto { photo: Box::new(p) });
            }
        }
    }
    // (built against the catalog as it is: one op list for all of them, ahead of the additions)
    if !purge.is_empty() {
        ops.insert(0, s.catalog.delete_photos_permanently_ops(&purge));
    }
    let log0 = s.pending_log.len();
    let (verb, n) = if report.imported.is_empty() && !report.restored.is_empty() {
        ("Restore", report.restored.len())
    } else {
        ("Add", report.imported.len().max(1))
    };
    if !ops.is_empty()
        && let Err(e) = s.commit(&format!("{verb} {n} Photo{}", if n == 1 { "" } else { "s" }), Op::Batch { ops })
    {
        // nothing was catalogued: take the moves back (their sources were never touched)
        placed.iter().for_each(crate::import_move::rollback);
        return Err(e);
    }
    if !placed.is_empty() {
        finish_moves(s, placed, log0, &mut report);
    }
    // AI Denoise switched on at import (above the ISO chosen): their pictures are made in the background
    // (not for a folder being browsed: nothing was imported)
    if s.import_defaults.denoise_above_iso.is_some() && !opts.local {
        let on: Vec<PhotoId> =
            report.imported.iter().map(|i| PhotoId(*i)).filter(|id| s.develop_of(*id).is_some_and(|d| d.denoise_amount() > 0.0)).collect();
        if !on.is_empty() {
            s.denoise_enqueue_all(&on);
        }
    }
    crate::cmd::plugins::restore_fields(s, &plugin_fields);
    // photos that came with an XMP sidecar start in step with it (the metadata-vs-file badge)
    // (not browsed Local records: a stamp would count as a change the user made and keep them)
    let with_xmp: Vec<PhotoId> = report.imported.iter().map(|i| PhotoId(*i)).filter(|id| s.catalog.photo(*id).is_some_and(|p| !p.local)).collect();
    s.record_xmp_stamps(&with_xmp, true);
    // Edit In versions (TIFF/PSD with an xmpMM:DerivedFrom link) stack on their source again
    crate::cmd::edit_in::stack_derived(s, &with_xmp);
    if s.import_defaults.remove_dust {
        let raws: Vec<lcc_catalog::PhotoId> =
            report.imported.iter().map(|id| lcc_catalog::PhotoId(*id)).filter(|id| s.catalog.photo(*id).is_some_and(|p| p.develops_raw())).collect();
        crate::cmd::sensor::remove_dust_after_import(s, raws);
    }
    Ok(report)
}

/// What every photo an import adds gets: the metadata preset, the keywords, then the preset and the
/// preset stack (one History entry each).
fn apply_settings(s: &Session, p: &mut Photo, opts: &ImportOptions, now: &str) {
    if let Some(mp) = opts.metadata_preset.as_ref().and_then(|n| s.metadata_presets.iter().find(|m| m.name.eq_ignore_ascii_case(n))) {
        crate::cmd::metadata::apply_to(&mut p.meta, &mp.fields);
    }
    for k in &opts.keywords {
        let k = lcc_catalog::keywords::clean(k);
        if !k.is_empty() && !p.meta.keywords.iter().any(|x| x.eq_ignore_ascii_case(&k)) {
            p.meta.keywords.push(k);
        }
    }
    // the camera profile before the presets: a preset doesn't take it away unless the import says so
    let body = (!opts.preset_profile && p.develops_raw()).then(|| p.develop.profile.camera.clone()).filter(|c| !c.is_empty());
    for preset in opts.preset.iter().chain(&opts.presets) {
        let mut d = preset.apply(&p.develop, 1.0);
        if let Some(cal) = &body {
            d.profile.camera.clone_from(cal);
        }
        let d = std::sync::Arc::new(d);
        let label = format!("Preset: {}", preset.name);
        p.develop = d.clone();
        p.edited = Some(now.to_string());
        p.history.push(lcc_catalog::HistoryStep { label, settings: d, batch: None });
    }
}

/// The ops that give a browsed (Local) photo joining the library what the import sets
/// ([`apply_settings`]): the presets' History entries and settings, and its metadata. Its culling
/// and its own edits stay (the presets go on top of them). Ported from OneSlips (LR-IMP-WINDOW).
fn promote_ops(s: &Session, id: PhotoId, opts: &ImportOptions, now: &str, ops: &mut Vec<Op>) {
    let Some(old) = s.catalog.photo(id) else { return };
    let mut p = (**old).clone();
    let history0 = p.history.len();
    // browsed before the app kept serial numbers: the file's, so its body is known from now on
    if let Some(serial) = s.missing_serial(&p, true) {
        p.meta.camera_serial = serial;
    }
    // browsed before its body had the profile it has now (set in the Import panel, say), or with a
    // choice made for this import: the profile as it is at import (OneSlips, LR-IMP-PROFILE), the
    // one the Import tab showed it with ([`Session::browse_profile`])
    if let Some(id) = s.import_profile_for(&p, &opts.camera_profile, true) {
        let mut d = (*p.develop).clone();
        d.profile.camera = id;
        p.develop = std::sync::Arc::new(d);
    }
    apply_settings(s, &mut p, opts, now);
    if p.develop != old.develop {
        let label = p.history.last().map(|h| h.label.clone()).unwrap_or_else(|| "Import".into());
        for step in p.history.iter().skip(history0) {
            ops.push(Op::PushHistory { id, step: step.clone() });
        }
        ops.push(Op::SetDevelop { id, settings: p.develop.clone(), label, edited: p.edited.clone() });
    }
    if p.meta != old.meta {
        ops.push(Op::SetMeta { id, meta: Box::new(p.meta) });
    }
}

/// Move, after the commit: save the library, then remove each source whose destination is
/// still intact. Nothing is removed when the library can't be saved.
fn finish_moves(s: &mut Session, placed: Vec<crate::import_move::Placed>, log0: usize, report: &mut ImportReport) {
    // (saving here clears the pending ops, so the sidecars `execute` would auto-write are written here)
    let auto: Vec<Op> = if s.xmp.auto_write { s.pending_log.get(log0..).map(<[Op]>::to_vec).unwrap_or_default() } else { Vec::new() };
    let before = s.pending_log.len();
    if let Err(e) = s.persist() {
        for p in placed {
            let reason = format!("the catalog could not be saved ({e}): the original stays; the photo uses the copy at {}", p.dst.display());
            report.kept.push(Kept { path: p.src.to_string_lossy().to_string(), reason });
        }
        return;
    }
    for p in placed {
        let from = p.src.to_string_lossy().to_string();
        match crate::import_move::finish(&p) {
            Ok(kept) => {
                let sidecars = p.sidecars.iter().map(|sc| sc.dst.to_string_lossy().to_string()).collect();
                report.moved.push(Moved { from, to: p.dst.to_string_lossy().to_string(), sidecars });
                report.kept.extend(kept.into_iter().map(|(path, reason)| Kept { path, reason }));
            }
            Err(reason) => {
                log::warn!("import: move {from}: {reason}");
                report.kept.push(Kept { path: from, reason });
            }
        }
    }
    if s.pending_log.len() < before && s.interaction.is_none() && !auto.is_empty() {
        s.auto_write_sidecars(&auto);
    }
}

/// The current local time as RFC 3339 with its offset (`YYYY-MM-DDTHH:MM:SS-05:00`); for
/// [`Session::clock`] on native hosts.
pub fn system_clock() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        lcc_catalog::dates::now_local()
    }
    #[cfg(target_arch = "wasm32")]
    {
        "2026-01-01T00:00:00".to_string()
    }
}

/// Unix seconds → `YYYY-MM-DDTHH:MM:SS` (UTC), proleptic Gregorian.
pub fn civil(secs: i64) -> String {
    lcc_catalog::dates::civil(secs)
}

impl Session {
    /// A thumbnail for an import candidate (not in the catalog): a raw's embedded preview, else a
    /// small render of the file. `slot` makes the job's photo id unique (`u64::MAX - slot`).
    pub fn candidate_thumb_job(&mut self, c: &ImportCandidate, edge: usize, slot: u64) -> Option<crate::media::QuickJob> {
        if c.error.is_some() {
            return None;
        }
        let id = PhotoId(u64::MAX - slot);
        let mut p = Photo::new(id, Source::File { path: c.path.clone() }, &c.name, &c.format, c.width.max(1), c.height.max(1), "");
        p.kind = c.kind;
        if let Some(info) = self.import_probes.get(&c.path) {
            p.as_shot_wb = info.as_shot_wb;
            p.embedded_lens = info.embedded_lens.map(Box::new);
            p.preview_only = info.preview_only.clone();
        }
        p.develop = std::sync::Arc::new(p.import_defaults());
        let edge = edge.clamp(64, crate::media::SourceLevel::Thumb.max_edge());
        let level = crate::media::SourceLevel::Thumb;
        let source = self.media.origin_ref(&p.source, level.max_edge());
        let key = lcc_preview::Hasher128::new().str(&c.path).u64(c.file_size).u64(edge as u64).finish().0 as u64;
        let small = crate::media::RenderJob {
            drawn_view_cache: false,
            on_screen: false,
            request_id: 0,
            cache_generation: self.media.rendered.generation(),
            source_key: None,
            photo: id,
            level,
            source,
            origin: p.source.clone(),
            info: crate::media::source_info(&p),
            settings: p.develop.clone(),
            request: lcc_pipeline::RenderRequest::fit(edge, edge),
            key,
            cache: None,
            stages: None,
            view_cache: None,
            display: None,
            slide: None,
            preview_cut: None,
            display_fit: None,
            hdr_texels: None,
            superseded: None,
            discard_source: false,
            latest: None,
            kept: None,
            placement: None,
        };
        let embedded = match (&self.media.preview_loader, c.kind) {
            (Some(l), MediaKind::Raw) => Some((c.path.clone(), l.clone(), edge)),
            _ => None,
        };
        Some(crate::media::QuickJob {
            request_id: 0,
            photo: id,
            key,
            cached: Vec::new(),
            embedded,
            embedded_cache: None,
            embedded_on_disk: false,
            small: Some(Box::new(small)),
            latest: Vec::new(),
            fit: None,
            draft: None,
            want: None,
            kept: None,
        })
    }

    /// Use the system clock for import/edit times (native hosts).
    pub fn with_system_clock(mut self) -> Self {
        self.clock = Box::new(system_clock);
        self
    }
}

#[cfg(test)]
#[path = "import/tests_known.rs"]
mod tests_known;

#[cfg(test)]
mod prepared_tests {
    use super::*;

    /// Extensions the picker may offer: every one in [`EXTENSIONS`], minus the
    /// files this build cannot import (see [`accept_list`]).
    fn pickable_extensions() -> Vec<&'static str> {
        let heif_decodable = lcc_codecs::Format::Heif.can_decode();
        EXTENSIONS
            .iter()
            .copied()
            .filter(|ext| match *ext {
                "avif" => false,
                "heic" | "heif" => heif_decodable,
                _ => true,
            })
            .collect()
    }

    #[test]
    fn accept_list_covers_every_pickable_extension_in_both_cases() {
        let list = accept_list();
        for ext in pickable_extensions() {
            assert!(list.contains(&format!(".{ext}")), "lowercase .{ext} missing from accept list");
            assert!(list.contains(&format!(".{}", ext.to_uppercase())), "uppercase .{} missing from accept list", ext.to_uppercase());
        }
    }

    /// The web picker must not offer files this build cannot import: `.avif` has
    /// no AV1 decoder in any build, so it is never listed, in either case.
    #[test]
    fn accept_list_never_offers_avif() {
        let list = accept_list();
        assert!(!list.contains(".avif"), "{list}");
        assert!(!list.contains(".AVIF"), "{list}");
    }

    /// `.heic`/`.heif` follow the codecs' `heif` feature: listed (in both cases)
    /// only when this build can decode them; otherwise hidden, so a web user
    /// cannot pick a file that would fail to import.
    #[test]
    fn accept_list_offers_heic_heif_only_when_decodable() {
        let list = accept_list();
        let listed = lcc_codecs::Format::Heif.can_decode();
        for ext in ["heic", "heif"] {
            assert_eq!(list.contains(&format!(".{ext}")), listed, "lowercase .{ext}");
            assert_eq!(list.contains(&format!(".{}", ext.to_uppercase())), listed, "uppercase .{ext}");
        }
    }

    #[test]
    fn revalidate_add_only_converts_matching_ready_item() {
        let mut s = Session::new();
        let existing_path = std::env::temp_dir().join("app-revalidate-existing.jpg");
        let other_path = std::env::temp_dir().join("app-revalidate-other.jpg");
        let existing = Photo::new(PhotoId(1), Source::File { path: existing_path.to_string_lossy().into() }, "existing.jpg", "JPG", 1, 1, "now");
        s.commit("existing", Op::AddPhoto { photo: Box::new(existing) }).unwrap();
        let ready = |path: &Path| ReadyFile {
            path: path.to_string_lossy().into(),
            stored: path.to_string_lossy().into(),
            info: ProbeInfo { format: "JPG".into(), ..ProbeInfo::default() },
            sidecar: None,
            placed: None,
            promote: None,
        };
        let mut prepared = Prepared {
            scanned: 2,
            cut_short: false,
            skipped: Skipped::default(),
            items: vec![PreparedItem::Ready(Box::new(ready(&existing_path))), PreparedItem::Ready(Box::new(ready(&other_path)))],
        };
        prepared.revalidate_add(&s);
        assert!(matches!(prepared.items.first(), Some(PreparedItem::Duplicate { reason: "path", .. })));
        assert!(matches!(prepared.items.get(1), Some(PreparedItem::Ready(_))));
    }

    fn unread_paths(n: usize) -> Vec<String> {
        (0..n).map(|i| std::env::temp_dir().join(format!("lcc-probe-progress-{i}.jpg")).to_string_lossy().into()).collect()
    }

    /// Reading the files of a large import (a Lightroom catalog's originals) reported nothing:
    /// the progress stood still at "14 of 24554" for as long as it took.
    #[test]
    fn prepare_counts_the_files_it_reads() {
        use std::sync::atomic::Ordering::Relaxed;
        let mut s = Session::new();
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (seen_in_probe, done_in_probe) = (seen.clone(), std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)));
        let mut job = ImportJob::new(&mut s, ImportOptions { mode: ImportMode::Add, ..ImportOptions::default() }).unwrap();
        let (done, last) = (job.read_done.clone(), done_in_probe.clone());
        job.probe = Some(std::sync::Arc::new(move |_: &str| {
            seen_in_probe.fetch_add(1, Relaxed);
            last.fetch_max(done.load(Relaxed), Relaxed);
            Ok(ProbeInfo { format: "JPEG".into(), ..ProbeInfo::default() })
        }));
        let prepared = job.prepare(&unread_paths(40), &std::sync::atomic::AtomicBool::new(false));
        assert_eq!(prepared.items.len(), 40);
        assert_eq!(seen.load(Relaxed), 40);
        assert!(done_in_probe.load(Relaxed) > 0, "progress moves while files are read");
        assert_eq!((job.read_done.load(Relaxed), job.read_total.load(Relaxed)), (40, 40), "every file read is counted");
        assert_eq!((job.done.load(Relaxed), job.total.load(Relaxed)), (40, 40), "handling them is counted on its own");
    }

    /// Cancel had no effect until every file had been read.
    #[test]
    fn cancel_stops_reading_files() {
        use std::sync::atomic::Ordering::Relaxed;
        let mut s = Session::new();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut job = ImportJob::new(&mut s, ImportOptions { mode: ImportMode::Add, ..ImportOptions::default() }).unwrap();
        let (seen_in_probe, cancel_in_probe) = (seen.clone(), cancel.clone());
        job.probe = Some(std::sync::Arc::new(move |_: &str| {
            seen_in_probe.fetch_add(1, Relaxed);
            cancel_in_probe.store(true, Relaxed);
            Ok(ProbeInfo { format: "JPEG".into(), ..ProbeInfo::default() })
        }));
        let prepared = job.prepare(&unread_paths(400), &cancel);
        // at most the files the (up to 8) readers had already started
        assert!(seen.load(Relaxed) <= 8, "read {} files after cancel", seen.load(Relaxed));
        assert!(prepared.items.is_empty());
    }
}
