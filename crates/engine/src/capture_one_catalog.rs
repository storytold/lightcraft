//! Read-only reader for Capture One catalogs (issue #4250).
//!
//! A Capture One catalog is a folder (a `.cocatalog` package on macOS) holding a SQLite database,
//! `<Name>.cocatalogdb`, next to `Originals/`, `Adjustments/` and `Cache/`. This module finds the
//! database, checks that it has the tables and columns the reader needs, and reads its library
//! data into [`CaptureOneCatalog`]: one [`CatalogPhoto`] per variant with its original's path,
//! rating, colour tag, keywords and IPTC fields, and the user's albums, projects and groups.
//! Develop settings, masks and smart album rules are not read.
//!
//! Clean room: the layout was learned from public descriptions of the format and from catalogs
//! Capture One wrote, read as data. No Capture One code, resources or profiles were looked at.
//! Capture One is a trademark of Capture One A/S; LightCraft is not affiliated with it.
//!
//! Nothing is ever written to the catalog. The database is read through the in-tree SQLite page
//! reader, which notices a file that changes while it is read. Capture One keeps a rollback
//! journal (no write-ahead log): a journal next to the database means a write is under way or
//! was cut short, so the catalog is refused until Capture One has closed it cleanly.
//!
//! What the catalog stores, as observed in catalogs at schema 1621 (Capture One 16.3), one of them first made with
//! Capture One 21:
//! - Tables are Core Data–like: `Z_PK` keys, `Z_ENT` entity numbers named in `ZENTITIES`. The
//!   numbers differ between versions, so entities are told apart by name only.
//! - `ZVERSIONINFO` lists every program version that wrote the catalog; `ZVERSION` is the schema
//!   version (1621 for Capture One 16.3), not the program's.
//! - Managed and referenced originals mix in one catalog. A file's folder is its
//!   `ZPATHLOCATION` row: relative to the catalog (`./Originals`, also with Windows separators
//!   and `..`) or a root per system (`ZMACROOT`, `ZWINROOT`) plus a path below it.
//! - Each `ZVARIANT` names three `ZVARIANTLAYER` rows (default, adjustment, combined);
//!   `ZVARIANTMETADATA.ZLAYER` hangs rating, colour tag, keywords and IPTC fields on a layer. The
//!   combined layer carries what Capture One shows.
//! - Times are Unix seconds; `-62135596800` (year 1) stands for no date. Capture time is the
//!   camera's clock stored as if it were UTC. A missing reference is often `-1`, not NULL.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub use crate::lightroom_catalog::Original;
pub use notice::Notice;

mod collections;
pub(crate) mod convert;
mod keywords;
mod locate;
pub mod notice;
mod paths;
mod photos;
mod smart;
mod tables;

/// A Capture One catalog's library data.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureOneCatalog {
    /// The database file that was read.
    pub database: String,
    /// The catalog's own id (`ZDOCUMENTCONTENT.ZDOCUMENTUUID`), the same in the catalog and its
    /// backups. `None` when the catalog has none.
    pub document_uuid: Option<String>,
    /// Every program version that wrote the catalog, oldest first.
    pub versions: Vec<VersionInfo>,
    /// A `writelock` file says Capture One has the catalog open. It is still read (the database
    /// is consistent while no journal exists), but the caller should say so.
    pub in_use: bool,
    /// One per variant, in the order of the catalog's images.
    pub photos: Vec<CatalogPhoto>,
    /// The user's albums, smart albums, projects and groups. Capture One's own collections (all
    /// images, recent imports and captures, trash, folders) are left out.
    pub collections: Vec<Collection>,
    /// The catalog's keyword list (`ZKEYWORD`): every keyword it has, also those no photo carries,
    /// parents before their children.
    pub keywords: Vec<Keyword>,
    /// Images in Capture One's trash, which are not in [`CaptureOneCatalog::photos`].
    pub trashed: usize,
    /// What was left out or could not be read.
    pub warnings: Vec<Notice>,
}

/// A keyword of the catalog's keyword list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keyword {
    /// `parent|child`.
    pub path: String,
    /// Written into exported files (`ZISEXPORTABLE`; a catalog that doesn't say: yes).
    pub exportable: bool,
}

/// One row of `ZVERSIONINFO`: a program that wrote the catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionInfo {
    /// The program, such as `16.3.9.9 Pro Mac` (`ZAUTHOR`; upgrades are written by `Datacore CLI`).
    pub app: String,
    /// The schema version it wrote (`ZVERSION`), such as 1621.
    pub schema: i64,
    /// The oldest schema version that can still read the catalog (`ZCOMPATIBLEVERSION`).
    pub compatible: i64,
}

/// What kind of file an original is (`ZIMAGE.ZIMAGECLASSIFICATION`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OriginalKind {
    /// A camera raw file (17).
    Raw,
    /// A rendered image: JPEG, TIFF, PSD, PNG (19).
    Raster,
    /// A video (6).
    Movie,
    /// Any other value; the file's extension has to tell.
    #[default]
    Unknown,
}

/// Capture One's colour tags, in the order of `ZVARIANTMETADATA.ZCOLOR_TAG_INDEX` (0 to 7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorTag {
    #[default]
    None,
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Pink,
    Purple,
}

impl ColorTag {
    /// The tag for an index; `None` for an index Capture One does not use.
    pub fn from_index(index: i64) -> Option<Self> {
        Some(match index {
            0 => Self::None,
            1 => Self::Red,
            2 => Self::Orange,
            3 => Self::Yellow,
            4 => Self::Green,
            5 => Self::Blue,
            6 => Self::Pink,
            7 => Self::Purple,
            _ => return None,
        })
    }

    /// The tag's English name, as other programs write it to `xmp:Label`; `None` for no tag.
    pub fn name(self) -> Option<&'static str> {
        Some(match self {
            Self::None => return None,
            Self::Red => "Red",
            Self::Orange => "Orange",
            Self::Yellow => "Yellow",
            Self::Green => "Green",
            Self::Blue => "Blue",
            Self::Pink => "Pink",
            Self::Purple => "Purple",
        })
    }
}

/// A variant of an image: Capture One's unit of rating, tagging and editing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CatalogPhoto {
    /// `ZIMAGE.Z_PK`.
    pub image_id: i64,
    /// `ZIMAGE.ZIMAGEUUID`.
    pub image_uuid: String,
    /// `ZVARIANT.Z_PK`; `None` for an image the catalog lists without any variant.
    pub variant_id: Option<i64>,
    /// `ZVARIANT.ZVARIANTUUID`.
    pub variant_uuid: String,
    /// 0 for an image's first variant; later variants are its virtual copies.
    pub variant_index: usize,
    pub file_name: String,
    /// The original's path: resolved when [`CatalogPhoto::original`] is [`Original::Local`],
    /// otherwise as the catalog records it.
    pub path: String,
    /// Whether `path` may be looked at.
    pub original: Original,
    /// The original is stored inside the catalog folder ("Inside Catalog").
    pub managed: bool,
    pub kind: OriginalKind,
    pub width: u32,
    pub height: u32,
    pub file_size: u64,
    /// Capture time on the camera's clock, `YYYY-MM-DDTHH:MM:SS` without a zone.
    pub captured: Option<String>,
    /// When the image was added to the catalog, in UTC (`…Z`).
    pub imported: Option<String>,
    pub camera_make: String,
    pub camera_model: String,
    pub camera_serial: String,
    pub lens: String,
    pub iso: Option<u32>,
    pub aperture: Option<f32>,
    /// Exposure time in seconds.
    pub shutter_seconds: Option<f64>,
    pub focal_mm: Option<f32>,
    /// Exposure compensation in EV.
    pub exposure_bias: Option<f32>,
    /// Latitude and longitude in degrees.
    pub gps: Option<(f64, f64)>,
    /// 0 to 5 stars.
    pub rating: u8,
    pub color_tag: ColorTag,
    /// Keywords as `parent|child` paths; a parent that only comes with a child is left out.
    pub keywords: Vec<String>,
    pub iptc: Iptc,
    /// The variant has adjustments (`ZVARIANT.ZISMODIFIED`). They are not read.
    pub adjusted: bool,
    /// The variant has an annotation (`ZVARIANT.ZANNOTATION`, not read).
    pub annotated: bool,
    /// Capture One shows the variant turned a quarter: its rotation (`ZVARIANTLAYER.ZROTATION`,
    /// from the file's orientation or the user's) is near 90° or 270°, so width and height trade
    /// places. `None` when it wasn't read: only a smart album rule on orientation needs it.
    pub turned: Option<bool>,
    /// The image's other camera and exposure columns (`ZEXP_…`, `ZCAMERA_…`, `ZMULTISHOT_…`,
    /// `ZGPSALTITUDE`) by name without the leading `Z`, as text; empty when unset. Kept for the
    /// recovery archive and for smart album rules that test them.
    pub exif: BTreeMap<String, String>,
}

/// IPTC fields of a variant, trimmed. Fields without a place of their own are in `other`, keyed
/// by their column name without the leading `Z`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Iptc {
    pub title: String,
    pub headline: String,
    pub description: String,
    pub creator: String,
    pub copyright: String,
    pub usage_terms: String,
    pub location: String,
    pub city: String,
    pub state: String,
    pub country: String,
    pub country_code: String,
    pub other: BTreeMap<String, String>,
}

/// What a user collection is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionKind {
    Album,
    /// Its photos come from its rule ([`Collection::rule`]), not from memberships.
    SmartAlbum,
    Project,
    Group,
}

/// An album, smart album, project or group the user made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collection {
    /// `ZCOLLECTION.Z_PK`.
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub kind: CollectionKind,
    /// The collection it is in; `None` at the top level.
    pub parent: Option<i64>,
    /// An album's photos, as indexes into [`CaptureOneCatalog::photos`].
    pub photos: Vec<usize>,
    /// A smart album's rule as Capture One stores it (`ZSMARTCRITERIA`, XML); empty for the
    /// other kinds.
    pub rule: String,
}

/// Why a catalog was not read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// No `.cocatalogdb` at the path.
    NotFound(String),
    /// The folder holds several databases; the user has to pick one.
    Ambiguous {
        folder: String,
        databases: Vec<String>,
    },
    /// A journal says Capture One is writing to the catalog or did not close it cleanly.
    Busy,
    /// The database changed while it was read.
    Changed,
    /// The database lacks tables or columns the reader needs (`TABLE` or `TABLE.COLUMN`).
    Unsupported {
        written_by: Option<VersionInfo>,
        missing: Vec<String>,
    },
    /// Not a readable SQLite database, or one that breaks the reader's limits.
    Invalid(String),
    Cancelled,
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(path) => write!(f, "{path}: no Capture One catalog database (.cocatalogdb) found"),
            Self::Ambiguous { folder, databases } => {
                write!(f, "{folder}: more than one catalog database ({}); choose the .cocatalogdb file itself", databases.join(", "))
            }
            Self::Busy => f.write_str(
                "Capture One is writing to this catalog, or did not close it cleanly; open and close the catalog in Capture One, then retry",
            ),
            Self::Changed => f.write_str("the catalog changed while it was read; close Capture One and retry"),
            Self::Unsupported { written_by, missing } => {
                f.write_str("unsupported Capture One catalog")?;
                if let Some(v) = written_by {
                    write!(f, " (last written by {}, schema {})", v.app, v.schema)?;
                }
                write!(f, ": missing {}", missing.join(", "))
            }
            Self::Invalid(why) => write!(f, "not a readable Capture One catalog: {why}"),
            Self::Cancelled => f.write_str("Capture One import cancelled"),
        }
    }
}

impl std::error::Error for ReadError {}

/// Whether `path` is for this reader: a path named like a catalog's database (`.cocatalogdb`, or
/// the `.backup` an upgrade leaves) or package (`.cocatalog`), or a folder holding a database.
/// The database is not opened, and a path that is only named like one need not exist: [`read`]
/// says what is wrong with it, in this reader's words.
pub fn is_catalog(path: &Path) -> bool {
    let package = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cocatalog"));
    package || is_database_path(path) || !matches!(locate::locate(path), Err(ReadError::NotFound(_)))
}

/// The database file of the catalog at `path` (see [`read`] for what `path` may be).
pub fn database(path: &Path) -> Result<std::path::PathBuf, ReadError> {
    locate::locate(path).map(|found| found.database)
}

/// Whether `path` is named like a catalog's database (`.cocatalogdb`, or the `.backup` an
/// upgrade leaves). Nothing on disk is looked at.
pub fn is_database_path(path: &Path) -> bool {
    path.file_name().is_some_and(|name| locate::is_database_name(&name.to_string_lossy()))
}

/// Read the catalog at `path`: a `.cocatalog` package or folder, a catalog backup folder, or the
/// `.cocatalogdb` file itself.
pub fn read(path: &Path) -> Result<CaptureOneCatalog, ReadError> {
    read_with_cancel(path, &AtomicBool::new(false))
}

/// [`read`], stopping with [`ReadError::Cancelled`] once `cancel` is set.
pub fn read_with_cancel(path: &Path, cancel: &AtomicBool) -> Result<CaptureOneCatalog, ReadError> {
    let found = locate::locate(path)?;
    let db = tables::Db::open(&found.database)?;
    db.check_schema()?;
    let active = || if cancel.load(Ordering::Relaxed) { Err(ReadError::Cancelled) } else { Ok(()) };

    let mut warnings = Vec::new();
    active()?;
    let mut read = photos::read(&db, &found.database, &mut warnings, &active)?;
    active()?;
    let collections = collections::read(&db, &read, &mut warnings)?;
    active()?;
    // the layers are the catalog's largest table: read only for a rule that needs them
    if collections.iter().any(|c| smart::tests_orientation(&c.rule)) {
        photos::read_turns(&db, &mut read.photos, &mut warnings)?;
        active()?;
    }
    let keywords = keywords::list(&db, &mut warnings)?;
    db.ensure_unchanged()?;
    if found.in_use {
        warnings.push(Notice::new(notice::IN_USE));
    }
    Ok(CaptureOneCatalog {
        database: found.database.to_string_lossy().into_owned(),
        document_uuid: db.document_uuid(),
        versions: db.versions(),
        in_use: found.in_use,
        photos: read.photos,
        collections,
        keywords,
        trashed: read.trashed,
        warnings,
    })
}

#[cfg(test)]
mod tests;
