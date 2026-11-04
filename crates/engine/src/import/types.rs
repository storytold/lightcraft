use super::{Deserialize, HashMap, MediaKind, Photo, PhotoId, Serialize};

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
    pub preset: Option<lightcraft_develop::Preset>,
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
    /// What the import makes of a file whose bytes a library photo already has.
    pub same_content: SameContent,
    /// A batch-edit recipe: develop control id / value pairs (e.g. `light.exposure` → 0.5),
    /// applied to every imported photo *after* its XMP sidecar, camera default and import preset,
    /// as one more History step (`Import adjustments`). Only the named controls change; the recipe
    /// is a snapshot taken when the import starts, so editing a preset afterwards cannot change it.
    /// It names plain adjustments only (no white balance, geometry or crop: `cmd::check_recipe`),
    /// and [`ImportJob::new`] refuses anything else.
    /// Empty (the default) = no recipe. A photo the library already has keeps its own settings:
    /// one skipped as a duplicate, restored from Recently Deleted, or joined from a browsed
    /// folder (Local) is never touched, as the preset and keywords never are.
    pub recipe: Vec<(String, f64)>,
}

/// What an import makes of a file whose bytes a library photo already has (a duplicate by
/// content). What is read of the file follows from it: a file the job makes a record of is read in
/// full, its XMP sidecar included; one it only matches to a library photo needs no more than its
/// capture time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SameContent {
    /// No record: the report lists it in [`ImportReport::duplicates`]. Its sidecar is read only for
    /// the capture time its file lacks, and not looked up otherwise (a round trip per name on a
    /// network share, #2954).
    #[default]
    Skip,
    /// A record of its own ([`Origin::Created`]), which the Lightroom catalog import makes from
    /// [`Duplicate::file`] (catalogs hold distinct originals with identical bytes): read as any
    /// file the job makes a record of, its XMP sidecar included (#2589).
    Create,
}

/// Where a library record an import job made or landed on came from. The job keeps one origin per
/// record ([`Origins`]), set where the record is made or first matched, and what the import does
/// to a record follows from it: whether its develop settings are an edit to keep, and how the
/// report counts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// This job made it, from a file (its metadata, the import defaults, its XMP sidecar) or from a
    /// Lightroom catalog: nothing in it is an edit made in the library.
    Created,
    /// A photo browsed in Local that this job brought into the library: there before the job,
    /// with whatever was done to it while it was browsed.
    Promoted,
    /// In the library before the job.
    Existing,
}

impl Origin {
    /// There before the job: what it has may be the user's own work.
    pub fn was_there(self) -> bool {
        self != Origin::Created
    }
}

/// The origin of each record an import job made or landed on ([`Origin`]): the file import
/// records the ones it makes and promotes ([`ImportReport::origins`]), and a Lightroom catalog
/// import goes on with the same record for the ones it matches and makes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Origins(HashMap<PhotoId, Origin>);

impl Origins {
    /// `id`'s origin in this job: the one recorded first. A record the job made stays
    /// [`Origin::Created`] when a later step of the job finds it in the library.
    pub fn record(&mut self, id: PhotoId, origin: Origin) -> Origin {
        *self.0.entry(id).or_insert(origin)
    }
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
        let date = p.date();
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
    /// The photo that already has it.
    pub existing: Option<u64>,
    /// Not readable.
    pub error: Option<String>,
    /// A raw variant that can't be decoded yet (why): it imports as preview only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_only: Option<String>,
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
    /// What the file itself says, as a file import reads it; `None` when the file wasn't read (a
    /// path match). A Lightroom import makes a record of its own for a duplicate by content and
    /// starts it from these, not from the other photo's record, which may have been edited (#2589).
    #[serde(skip)]
    pub file: Option<Box<FileFacts>>,
}

/// What a file import reads from a file itself, before anything in the library is edited.
#[derive(Clone, Debug, Default)]
pub struct FileFacts {
    /// The metadata the file carries (EXIF, IPTC, embedded XMP).
    pub meta: lightcraft_catalog::Meta,
    /// The capture time a file import gives it: the file's, else its XMP sidecar's, else the
    /// file's modification time.
    pub captured: Option<String>,
    /// The file's own orientation, when it states one ([`crate::media::ProbeInfo::orientation`]): it turns what its
    /// sidecar's regions were drawn on, the photo as stored, upright (#2674).
    pub orientation: Option<lightcraft_geom::Orientation>,
    /// Its XMP sidecar (or a raw's embedded XMP), which a file import merges into the record.
    pub sidecar: Option<crate::sidecar::SidecarData>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ImportReport {
    pub imported: Vec<u64>,
    /// The origin of each record in `imported`: made from its file, or a photo browsed in Local
    /// that joined the library. (Not in the report.)
    #[serde(skip)]
    pub origins: Origins,
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
    /// Files found after expanding folders.
    pub scanned: usize,
    /// Photos whose metadata/develop settings were read from an XMP sidecar (or embedded XMP).
    pub sidecars: usize,
    /// Per-photo diagnostics from foreign XMP develop settings that could not be fully carried over.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ImportWarning>,
}

/// A structured diagnostic from foreign XMP develop settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImportWarningCode {
    /// A named camera profile was found but cannot be applied as Lightroom does.
    CameraProfile,
    /// Lightroom geometry data was found but its saved transform is not retained.
    UprightGeometry,
    /// One or more other XMP develop fields have no LightCraft mapping.
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
