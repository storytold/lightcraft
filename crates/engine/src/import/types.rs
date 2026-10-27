use super::*;

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
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ImportReport {
    pub imported: Vec<u64>,
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
