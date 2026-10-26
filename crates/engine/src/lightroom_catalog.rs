//! Direct, read-only Lightroom Classic catalog import. SQLite pages are read in Rust;
//! Lightroom, Python and a C SQLite runtime are not needed. Original paths stay in place.
//! Raw settings are archived before mutation; mapped edits are approximations, not Adobe renders.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use crate::lightroom_sqlite::{Database, FileSource, LiveTable, Record, RowError, Value as SqlValue};
use lightcraft_catalog::{Album, Flag, Op, Photo, PhotoId, Source};
use lightcraft_geom::{Affine, Orientation, Point, Rect};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

type Row = BTreeMap<String, Value>;
const COMMAND: &str = "library.importLightroom";
/// Largest catalog database or WAL read into memory (4 GiB). The whole file is held in RAM, so the cap stays finite.
const MAX_FILE: u64 = 1 << 32;

fn error(why: impl Into<String>) -> crate::EngineError {
    crate::EngineError::BadParams { cmd: COMMAND.into(), msg: why.into() }
}

fn text<'a>(r: &'a Row, k: &str) -> &'a str {
    r.get(k).and_then(Value::as_str).unwrap_or("")
}
fn number(r: &Row, k: &str) -> i64 {
    r.get(k).and_then(Value::as_f64).filter(|n| n.is_finite()).unwrap_or(0.0) as i64
}

pub(crate) fn number_for_job(r: &Row, k: &str) -> i64 {
    number(r, k)
}
fn key(path: &str) -> String {
    let p = path.replace('\\', "/");
    if cfg!(windows) { p.to_lowercase() } else { p }
}
fn source_path(p: &Photo) -> Option<&str> {
    match &p.source {
        Source::File { path } => Some(path),
        _ => None,
    }
}

/// Most history and snapshot data (estimated bytes, see [`row_bytes`]) kept for the recovery
/// archive. The archive holds at most [`crate::lightroom_archive::MAX_ARCHIVE_BYTES`] (32 MiB)
/// compressed, and Lightroom's settings text compresses about 8:1, so keeping more could not be
/// archived anyway; steps past the budget are counted in a warning instead of held in memory.
const MAX_KEPT_HISTORY: usize = 256 << 20;

/// One table's columns, for picking fields out of a streamed record by name.
struct Columns<'a>(&'a [String]);
impl Columns<'_> {
    fn at(&self, name: &str) -> Option<usize> {
        self.0.iter().position(|c| c == name)
    }
    /// The column as `number` reads it from a [`Row`]: integers, finite reals truncated, else 0.
    fn int(&self, r: &Record, name: &str) -> i64 {
        match self.at(name).and_then(|i| r.values.get(i)) {
            Some(SqlValue::Integer(n)) => *n,
            Some(SqlValue::Real(n)) if n.is_finite() => *n as i64,
            _ => 0,
        }
    }
    /// Move the column's text out of the record ("" when it isn't text).
    fn take_text(&self, r: &mut Record, name: &str) -> String {
        match self.at(name).and_then(|i| r.values.get_mut(i)) {
            Some(SqlValue::Text(s)) => std::mem::take(s),
            _ => String::new(),
        }
    }
    /// Every non-blob column as a [`Row`] (opaque Lightroom payloads never become JSON).
    fn row(&self, r: Record) -> Row {
        self.0
            .iter()
            .zip(r.values)
            .filter_map(|(column, value)| {
                let value = match value {
                    SqlValue::Null => Value::Null,
                    SqlValue::Integer(n) => json!(n),
                    SqlValue::Real(n) => json!(n),
                    SqlValue::Text(s) => Value::String(s),
                    SqlValue::Blob(_) => return None,
                };
                Some((column.clone(), value))
            })
            .collect()
    }
}

/// Rough bytes a [`Row`] holds: its keys and texts plus per-entry overhead.
fn row_bytes(row: &Row) -> usize {
    row.iter().map(|(k, v)| k.len() + v.as_str().map_or(0, str::len) + 64).sum()
}

struct Reader<'c> {
    db: Database,
    tables: Vec<LiveTable>,
    cancel: &'c AtomicBool,
}
impl Reader<'_> {
    /// Stream `name`'s rows to `visit`. A missing table is an error when `required`, else no rows.
    /// [`RowError::Table`] means the table could not be read; [`RowError::Visitor`] is `visit`'s
    /// own error (or cancellation), which always stops the import.
    fn each(&self, name: &str, required: bool, visit: &mut dyn FnMut(&Columns, Record) -> Result<(), String>) -> Result<(), RowError<String>> {
        let Some(t) = self.tables.iter().find(|t| t.name == name) else {
            return if required { Err(RowError::Visitor(format!("unsupported Lightroom catalog: missing {name}"))) } else { Ok(()) };
        };
        let columns = Columns(t.column_names.as_deref().ok_or_else(|| RowError::Table(format!("cannot read {name} columns")))?);
        let cancel = self.cancel;
        self.db
            .for_each_row(t.rootpage, columns.0.len(), &mut |record| {
                ensure_read_active(cancel)?;
                visit(&columns, record)
            })
            .map_err(|e| match e {
                RowError::Table(why) => RowError::Table(format!("{name}: {why}")),
                visitor => visitor,
            })
    }

    /// A table the import needs: any failure stops it.
    fn required(&self, name: &str, visit: &mut dyn FnMut(&Columns, Record) -> Result<(), String>) -> Result<(), String> {
        self.each(name, true, visit).map_err(|e| match e {
            RowError::Table(why) | RowError::Visitor(why) => why,
        })
    }

    /// A table the import can do without: a read failure is a warning (rows streamed before it are
    /// kept); the visitor's own errors and cancellation still stop the import.
    fn optional(&self, name: &str, warnings: &mut Vec<String>, visit: &mut dyn FnMut(&Columns, Record) -> Result<(), String>) -> Result<(), String> {
        match self.each(name, false, visit) {
            Ok(()) => Ok(()),
            Err(RowError::Table(why)) => {
                warnings.push(format!("skipping optional Lightroom table {name}: {why}"));
                Ok(())
            }
            Err(RowError::Visitor(why)) => Err(why),
        }
    }

    /// A table kept whole as rows (the small collection tables).
    fn optional_rows(&self, name: &str, warnings: &mut Vec<String>) -> Result<Vec<Row>, String> {
        let mut rows = Vec::new();
        self.optional(name, warnings, &mut |columns, record| {
            rows.push(columns.row(record));
            Ok(())
        })?;
        Ok(rows)
    }

    /// History or snapshot rows grouped by image, kept up to the shared `budget` (bytes left).
    fn grouped_rows(&self, name: &str, warnings: &mut Vec<String>, budget: &mut usize) -> Result<HashMap<i64, Vec<Row>>, String> {
        let mut out: HashMap<i64, Vec<Row>> = HashMap::new();
        let mut dropped = 0usize;
        self.optional(name, warnings, &mut |columns, record| {
            let image = columns.int(&record, "image");
            let row = columns.row(record);
            let size = row_bytes(&row);
            if size > *budget {
                dropped = dropped.saturating_add(1);
                *budget = 0;
                return Ok(());
            }
            *budget -= size;
            out.entry(image).or_default().push(row);
            Ok(())
        })?;
        if dropped > 0 {
            let kept: usize = out.values().map(Vec::len).sum();
            warnings.push(format!(
                "{name}: kept {kept} rows for the recovery archive; {dropped} more are not archived ({} MiB budget). Imported edits are not affected.",
                MAX_KEPT_HISTORY >> 20
            ));
        }
        Ok(out)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogPhoto {
    pub source_id: i64,
    pub uuid: String,
    pub path: String,
    pub image: Row,
    pub settings: String,
    pub xmp: String,
    pub keywords: Vec<String>,
    pub history: Vec<Row>,
    pub snapshots: Vec<Row>,
    /// Whether `path` may be looked at (derived when the catalog is read; not archived).
    #[serde(skip)]
    pub original: Original,
}

/// Whether the original a catalog lists may be looked at. Its path is resolved with the catalog
/// as the document ([`crate::doc_path::resolve`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Original {
    /// `path` is a local file path, or one on the network share the catalog is on.
    Local,
    /// `path` is not a path on this computer (a catalog made on another system): the photo is
    /// kept as an offline record for later relinking, and the path is not looked at.
    #[default]
    NotHere,
    /// `path` is refused (why): it is on another computer or a device, or its folder or file name
    /// is an absolute path. The photo is reported missing and not added to the library.
    Refused(String),
}

impl CatalogPhoto {
    /// Whether the original is a file here; only a [`Original::Local`] path is looked at.
    pub fn original_is_file(&self) -> bool {
        self.original == Original::Local && Path::new(&self.path).is_file()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogImport {
    pub source: String,
    pub photos: Vec<CatalogPhoto>,
    pub collections: Vec<Row>,
    pub members: Vec<Row>,
    pub collection_content: Vec<Row>,
    pub warnings: Vec<String>,
    /// Photos whose Lightroom edits could not be read: the catalog has an
    /// `Adobe_imageDevelopSettings` table but it failed to read, so these photos arrive unedited.
    /// Reported as `editsNotImported` and shown in the import summary, never only as a warning.
    #[serde(default)]
    pub edits_not_imported: usize,
    /// Catalog images left out because their records are incomplete; reported as data (not text)
    /// so each front end can say so in its own language.
    #[serde(default)]
    pub skipped: Vec<SkippedImage>,
}

/// A catalog image that was not imported, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedImage {
    /// The image's `Adobe_images.id_local`.
    pub image: i64,
    pub reason: SkipReason,
}

/// Why a catalog image was not imported. Lightroom leaves such records behind and ignores them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SkipReason {
    /// Its `AgLibraryFile` record is gone.
    MissingFile,
    /// Its file's `AgLibraryFolder` record is gone.
    MissingFolder,
    /// Its folder's `AgLibraryRootFolder` record is gone.
    MissingRoot,
    /// Its file name or root folder path is empty.
    IncompletePath,
    /// A virtual copy whose master was skipped.
    MasterSkipped,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct ImportIndex {
    pub(crate) photos: BTreeMap<String, u64>,
    pub(crate) collections: BTreeMap<String, u64>,
}

pub(crate) fn load_index(path: Option<&Path>) -> crate::Result<ImportIndex> {
    match path {
        Some(path) => {
            let file = match std::fs::File::open(path) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ImportIndex::default()),
                Err(e) => return Err(error(e.to_string())),
            };
            if file.metadata().map_err(|e| error(e.to_string()))?.len() > 16 * 1024 * 1024 {
                return Err(error("Lightroom import index exceeds 16 MiB"));
            }
            let mut bytes = Vec::new();
            use std::io::Read;
            file.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|e| error(e.to_string()))?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err(error("Lightroom import index exceeds 16 MiB"));
            }
            serde_json::from_slice(&bytes).map_err(|e| error(format!("invalid Lightroom import index: {e}")))
        }
        None => Ok(ImportIndex::default()),
    }
}

pub(crate) struct AppliedImport {
    pub(crate) report: Value,
    pub(crate) index: ImportIndex,
}

fn identity(data: &CatalogImport, photo: &CatalogPhoto) -> String {
    if photo.uuid.is_empty() { format!("{}:{}", key(&data.source), photo.source_id) } else { photo.uuid.clone() }
}

fn validate(data: &CatalogImport) -> Result<(), String> {
    let photos: HashMap<_, _> = data.photos.iter().map(|p| (p.source_id, p)).collect();
    if photos.len() != data.photos.len() {
        return Err("duplicate source photo ids".into());
    }
    for photo in &data.photos {
        let master = number(&photo.image, "masterImage");
        if master != 0
            && (master == photo.source_id
                || !photos.contains_key(&master)
                || photos.get(&master).is_some_and(|p| number(&p.image, "masterImage") != 0))
        {
            return Err(format!("invalid virtual copy master for {}", photo.source_id));
        }
    }
    let collections: HashMap<_, _> = data.collections.iter().map(|r| (number(r, "id_local"), r)).collect();
    for id in collections.keys() {
        let mut seen = HashSet::new();
        let mut at = *id;
        while let Some(r) = collections.get(&at) {
            if seen.len() >= 64 || !seen.insert(at) {
                return Err("cyclic or overly deep collection hierarchy".into());
            }
            at = number(r, "parent");
            if at == 0 {
                break;
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_for_job(data: &CatalogImport) -> Result<(), String> {
    validate(data)
}

fn bytes(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    if len > MAX_FILE {
        return Err("catalog or WAL exceeds 4 GiB import limit".into());
    }
    let mut out = Vec::new();
    // Reserve the exact size up front: growing by doubling would briefly need about twice the catalog's size.
    let wanted = usize::try_from(len).map_err(|_| "catalog is too large for this platform".to_string())?;
    out.try_reserve_exact(wanted).map_err(|_| "not enough memory to read the catalog".to_string())?;
    f.take(MAX_FILE + 1).read_to_end(&mut out).map_err(|e| e.to_string())?;
    if out.len() as u64 > MAX_FILE {
        return Err("catalog grew beyond import limit".into());
    }
    Ok(out)
}

fn decode_xmp_bytes(bytes: &[u8]) -> Result<String, String> {
    let size = bytes.get(..4).and_then(|s| <[u8; 4]>::try_from(s).ok()).map(u32::from_be_bytes).ok_or("truncated XMP header")?;
    if size > 16 << 20 {
        return Err("XMP packet exceeds 16 MiB".into());
    }
    let decoded = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(bytes.get(4..).ok_or("truncated XMP")?, 16 << 20)
        .map_err(|_| "invalid compressed XMP")?;
    if decoded.len() != size as usize {
        return Err("XMP packet length mismatch".into());
    }
    String::from_utf8(decoded).map_err(|e| e.to_string())
}

fn be_word(bytes: &[u8], at: usize) -> Result<u32, String> {
    let word = bytes.get(at..at + 4).and_then(|s| <[u8; 4]>::try_from(s).ok()).ok_or("truncated WAL")?;
    Ok(u32::from_be_bytes(word))
}

fn wal_checksum(bytes: &[u8], big: bool, mut sum: (u32, u32)) -> (u32, u32) {
    for pair in bytes.as_chunks::<8>().0 {
        let first = [pair[0], pair[1], pair[2], pair[3]];
        let second = [pair[4], pair[5], pair[6], pair[7]];
        let (first, second) =
            if big { (u32::from_be_bytes(first), u32::from_be_bytes(second)) } else { (u32::from_le_bytes(first), u32::from_le_bytes(second)) };
        sum.0 = sum.0.wrapping_add(first).wrapping_add(sum.1);
        sum.1 = sum.1.wrapping_add(second).wrapping_add(sum.0);
    }
    sum
}

// SQLite file format §4: validate the live checksum chain in constant space, then overlay
// only its last committed prefix. The forensic timeline materializes EVERY historical
// database snapshot; it must never be used to validate a large, actively used catalog.
fn committed_wal_len(main: &[u8], wal: &[u8]) -> Result<usize, String> {
    if wal.is_empty() {
        return Ok(0);
    }
    let header = wal.get(..32).ok_or("truncated WAL header")?;
    let magic = be_word(header, 0)?;
    if !matches!(magic, 0x377f0682 | 0x377f0683) || be_word(header, 4)? != 3_007_000 {
        return Err("invalid WAL header".into());
    }
    let size = main.get(16..18).and_then(|s| <[u8; 2]>::try_from(s).ok()).map(u16::from_be_bytes).ok_or("truncated SQLite header")?;
    let size = if size == 1 { 65536 } else { u32::from(size) };
    if !(512..=65536).contains(&size) || !size.is_power_of_two() || be_word(header, 8)? != size {
        return Err("WAL page size mismatch".into());
    }
    let big = magic == 0x377f0683;
    let mut sum = wal_checksum(&header[..24], big, (0, 0));
    if sum != (be_word(header, 24)?, be_word(header, 28)?) {
        return Err("invalid WAL header checksum".into());
    }
    let salt = (be_word(header, 16)?, be_word(header, 20)?);
    let mut committed = 0;
    let stride = size as usize + 24;
    for (i, frame) in wal[32..].chunks_exact(stride).enumerate() {
        if be_word(frame, 0)? == 0 || salt != (be_word(frame, 8)?, be_word(frame, 12)?) {
            break;
        }
        sum = wal_checksum(&frame[..8], big, sum);
        sum = wal_checksum(&frame[24..], big, sum);
        if sum != (be_word(frame, 16)?, be_word(frame, 20)?) {
            break;
        }
        if be_word(frame, 4)? != 0 {
            committed = 32 + (i + 1) * stride;
        }
    }
    Ok(committed)
}

/// Where a root folder is when its `absolutePath` isn't a folder here (the catalog came from
/// another computer): `relativePathFromCatalog`, which Lightroom records for roots on the
/// catalog's volume, resolved against the catalog's folder, if that is a folder. Both paths are
/// looked at only when [`crate::doc_path::resolve`] follows them.
fn relocated_root(absolute: &str, relative: &str, catalog: &Path) -> Option<PathBuf> {
    if relative.is_empty() || !Path::new(relative).is_relative() || crate::doc_path::resolve(absolute, Some(catalog)).is_ok_and(|p| p.is_dir()) {
        return None;
    }
    crate::doc_path::resolve(relative, Some(catalog)).ok().filter(|dir| dir.is_dir())
}

/// Whether a folder path or file name from the catalog is absolute: `Path::join` would put it in
/// place of the root folder.
fn is_absolute_part(part: &str) -> bool {
    let bytes = part.as_bytes();
    part.starts_with(['/', '\\'])
        || (bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1) == Some(&b':'))
        || Path::new(part).is_absolute()
}

/// The path of an original under `root` and whether it may be looked at, resolved with the
/// catalog at `catalog` as the document.
fn original_path(root: &str, folder: &str, name: &str, catalog: &Path) -> (String, Original) {
    let stored = Path::new(root).join(folder).join(name).to_string_lossy().to_string();
    if is_absolute_part(folder) || is_absolute_part(name) {
        return (stored, Original::Refused("its folder or file name is an absolute path".into()));
    }
    match crate::doc_path::resolve(&stored, Some(catalog)) {
        Ok(found) => (found.to_string_lossy().to_string(), Original::Local),
        Err(crate::doc_path::NotFollowed::NotHere) => (stored, Original::NotHere),
        Err(crate::doc_path::NotFollowed::Refused(why)) => (stored, Original::Refused(why.to_string())),
    }
}

/// Inspect `.lrcat` plus committed WAL pages without modifying either source file.
pub fn read(path: &Path) -> Result<CatalogImport, String> {
    let cancel = AtomicBool::new(false);
    let total = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    read_with_progress(path, &cancel, &total, &done)
}

/// Read a Lightroom Classic catalog for import, counting the tables read in `total` / `done` and
/// stopping when `cancel` is set. Images whose catalog records are incomplete are left out and
/// listed in [`CatalogImport::skipped`] instead of failing the read.
pub fn read_with_progress(path: &Path, cancel: &AtomicBool, total: &AtomicUsize, done: &AtomicUsize) -> Result<CatalogImport, String> {
    // 13 catalog tables plus one validation stage; ImportJob adds one unit per original path.
    total.store(14, Ordering::Relaxed);
    done.store(0, Ordering::Relaxed);
    ensure_read_active(cancel)?;
    let before = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let wal_path = PathBuf::from(format!("{}-wal", path.display()));
    let wal_before = std::fs::metadata(&wal_path).ok().map(|m| (m.len(), m.modified().ok()));
    let main = FileSource::open(path)?;
    let wal = match std::fs::metadata(&wal_path) {
        Ok(_) => bytes(&wal_path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.to_string()),
    };
    let wal_len = committed_wal_len(main.header(), &wal)?;
    ensure_read_active(cancel)?;
    let db = Database::open_file_with_wal(main, &wal[..wal_len]).map_err(|e| format!("invalid SQLite catalog: {e:?}"))?;
    let after = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let wal_after = std::fs::metadata(&wal_path).ok().map(|m| (m.len(), m.modified().ok()));
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() || wal_before != wal_after {
        return Err("catalog changed during reading; close Lightroom and retry".into());
    }
    let reader = Reader { tables: db.live_tables()?, db, cancel };
    let mut warnings = Vec::new();
    // Each table is streamed; per row only the fields the import maps are kept.
    let mut roots: BTreeMap<i64, (String, String, String)> = BTreeMap::new();
    reader.required("AgLibraryRootFolder", &mut |c, mut r| {
        let id = c.int(&r, "id_local");
        roots.insert(id, (c.take_text(&mut r, "absolutePath"), c.take_text(&mut r, "name"), c.take_text(&mut r, "relativePathFromCatalog")));
        Ok(())
    })?;
    done.fetch_add(1, Ordering::Relaxed);
    let mut folders: HashMap<i64, (i64, String)> = HashMap::new();
    reader.required("AgLibraryFolder", &mut |c, mut r| {
        folders.insert(c.int(&r, "id_local"), (c.int(&r, "rootFolder"), c.take_text(&mut r, "pathFromRoot")));
        Ok(())
    })?;
    done.fetch_add(1, Ordering::Relaxed);
    let mut files: HashMap<i64, (i64, String)> = HashMap::new();
    reader.required("AgLibraryFile", &mut |c, mut r| {
        files.insert(c.int(&r, "id_local"), (c.int(&r, "folder"), c.take_text(&mut r, "idx_filename")));
        Ok(())
    })?;
    done.fetch_add(1, Ordering::Relaxed);
    // Develop settings carry every photo's edits: a table that exists but can't be read is not an
    // optional table quietly skipped. Rows read before a failure are kept, and the photos left
    // without settings are counted into `edits_not_imported` and reported prominently.
    let mut develops: HashMap<i64, String> = HashMap::new();
    let develop_error = match reader.each("Adobe_imageDevelopSettings", false, &mut |c, mut r| {
        develops.insert(c.int(&r, "image"), c.take_text(&mut r, "text"));
        Ok(())
    }) {
        Ok(()) => None,
        Err(RowError::Table(why)) => Some(why),
        Err(RowError::Visitor(why)) => return Err(why),
    };
    done.fetch_add(1, Ordering::Relaxed);
    let mut metadata: HashMap<i64, String> = HashMap::new();
    let mut metadata_warnings = Vec::new();
    reader.optional("Adobe_AdditionalMetadata", &mut warnings, &mut |c, r| {
        let image = c.int(&r, "image");
        let packet = match c.at("xmp").and_then(|i| r.values.get(i)) {
            Some(SqlValue::Text(s)) => s.clone(),
            Some(SqlValue::Blob(bytes)) => match decode_xmp_bytes(bytes) {
                Ok(packet) => packet,
                Err(error) => {
                    metadata_warnings.push(format!("image {image}: {error}"));
                    return Ok(());
                }
            },
            _ => return Ok(()),
        };
        metadata.insert(image, packet);
        Ok(())
    })?;
    warnings.extend(metadata_warnings);
    done.fetch_add(1, Ordering::Relaxed);
    let mut keyword_rows: HashMap<i64, (i64, String)> = HashMap::new();
    reader.optional("AgLibraryKeyword", &mut warnings, &mut |c, mut r| {
        keyword_rows.insert(c.int(&r, "id_local"), (c.int(&r, "parent"), c.take_text(&mut r, "name")));
        Ok(())
    })?;
    done.fetch_add(1, Ordering::Relaxed);
    let mut keywords: HashMap<i64, Vec<String>> = HashMap::new();
    reader.optional("AgLibraryKeywordImage", &mut warnings, &mut |c, r| {
        let mut parts = Vec::new();
        let mut at = c.int(&r, "tag");
        let mut seen = HashSet::new();
        while at != 0 {
            if parts.len() >= 64 || !seen.insert(at) {
                return Err("cyclic or overly deep keyword hierarchy".into());
            }
            let Some((parent, name)) = keyword_rows.get(&at) else { break };
            if !name.is_empty() {
                parts.push(name.as_str());
            }
            at = *parent;
        }
        parts.reverse();
        if !parts.is_empty() {
            keywords.entry(c.int(&r, "image")).or_default().push(parts.join("|"));
        }
        Ok(())
    })?;
    done.fetch_add(1, Ordering::Relaxed);
    let mut root_paths = HashMap::new();
    let mut refused_roots = HashSet::new();
    for (id, (absolute, name, relative)) in &roots {
        let resolved = match relocated_root(absolute, relative, path) {
            Some(found) => {
                let found = found.to_string_lossy().to_string();
                warnings.push(format!("root folder {name}: {absolute} not found; using {found} (relative to the catalog)"));
                found
            }
            None => absolute.clone(),
        };
        if let Err(crate::doc_path::NotFollowed::Refused(why)) = crate::doc_path::resolve(&resolved, Some(path)) {
            warnings.push(format!("root folder {name}: {resolved} is not opened: {why}; its photos are listed as missing and not added"));
            refused_roots.insert(*id);
        }
        root_paths.insert(*id, resolved);
    }
    let mut budget = MAX_KEPT_HISTORY;
    let mut history = reader.grouped_rows("Adobe_libraryImageDevelopHistoryStep", &mut warnings, &mut budget)?;
    done.fetch_add(1, Ordering::Relaxed);
    let mut snapshots = reader.grouped_rows("Adobe_libraryImageDevelopSnapshot", &mut warnings, &mut budget)?;
    done.fetch_add(1, Ordering::Relaxed);
    let collections = reader.optional_rows("AgLibraryCollection", &mut warnings)?;
    done.fetch_add(1, Ordering::Relaxed);
    let members = reader.optional_rows("AgLibraryCollectionImage", &mut warnings)?;
    done.fetch_add(1, Ordering::Relaxed);
    let collection_content = reader.optional_rows("AgLibraryCollectionContent", &mut warnings)?;
    done.fetch_add(1, Ordering::Relaxed);
    let mut out = CatalogImport {
        source: path.to_string_lossy().into(),
        photos: Vec::new(),
        collections,
        members,
        collection_content,
        warnings,
        edits_not_imported: 0,
        skipped: Vec::new(),
    };
    // images whose file, folder or root record is gone, or whose path is empty (Lightroom leaves
    // these behind and ignores them); they and their virtual copies are skipped and reported
    // instead of failing the import
    let mut orphans = HashSet::new();
    let mut photos = Vec::new();
    reader.required("Adobe_images", &mut |c, r| {
        let image = c.row(r);
        let id = number(&image, "id_local");
        let record = files.get(&number(&image, "rootFile")).ok_or(SkipReason::MissingFile).and_then(|(folder_id, name)| {
            let (root_id, folder_path) = folders.get(folder_id).ok_or(SkipReason::MissingFolder)?;
            if !roots.contains_key(root_id) {
                return Err(SkipReason::MissingRoot);
            }
            Ok((root_id, folder_path, name))
        });
        let (root_id, folder_path, name) = match record {
            Ok(found) => found,
            Err(reason) => {
                out.skipped.push(SkippedImage { image: id, reason });
                orphans.insert(id);
                return Ok(());
            }
        };
        let root_path = root_paths.get(root_id).map_or("", String::as_str);
        if root_path.is_empty() || name.is_empty() {
            out.skipped.push(SkippedImage { image: id, reason: SkipReason::IncompletePath });
            orphans.insert(id);
            return Ok(());
        }
        let (photo_path, original) = original_path(root_path, folder_path, name, path);
        if let Original::Refused(why) = &original
            && !refused_roots.contains(root_id)
        {
            out.warnings.push(format!("image {id}: {photo_path} is not opened: {why}; listed as missing and not added"));
        }
        photos.push(CatalogPhoto {
            source_id: id,
            uuid: text(&image, "id_global").into(),
            path: photo_path,
            settings: develops.remove(&id).unwrap_or_default(),
            xmp: metadata.remove(&id).unwrap_or_default(),
            keywords: keywords.remove(&id).unwrap_or_default(),
            history: history.remove(&id).unwrap_or_default(),
            snapshots: snapshots.remove(&id).unwrap_or_default(),
            image,
            original,
        });
        Ok(())
    })?;
    done.fetch_add(1, Ordering::Relaxed);
    out.photos = photos;
    drop_copies_of(&mut out, &orphans);
    if let Some(why) = develop_error {
        let missing = out.photos.iter().filter(|p| p.settings.is_empty()).count();
        out.edits_not_imported = missing;
        if missing == 0 {
            out.warnings.push(format!("develop settings: the table ends in an unreadable part ({why}); every photo's edits were read before it"));
        } else {
            let total = out.photos.len();
            let warning = format!(
                "Lightroom edits were not imported for {missing} of {total} photos: the catalog's develop settings could not be read ({why}). \
                 Their ratings, keywords and collections still import. Repair the catalog in Lightroom Classic (File > Optimize Catalog) \
                 and import it again with updateExisting to bring in the edits."
            );
            out.warnings.insert(0, warning);
        }
    }
    refuse_non_media(&mut out);
    ensure_read_active(cancel)?;
    reader.db.ensure_unchanged()?;
    let wal_after = std::fs::metadata(&wal_path).ok().map(|m| (m.len(), m.modified().ok()));
    if wal_before != wal_after {
        return Err("catalog changed during reading; close Lightroom and retry".into());
    }
    Ok(out)
}

/// Drop the virtual copies whose master was skipped, adding them to the skipped images: a copy has
/// no original of its own.
fn drop_copies_of(data: &mut CatalogImport, masters: &HashSet<i64>) {
    if masters.is_empty() {
        return;
    }
    let mut skipped = Vec::new();
    data.photos.retain(|p| {
        let master = number(&p.image, "masterImage");
        // 0 marks a master, never a copy
        if master == 0 || !masters.contains(&master) {
            return true;
        }
        skipped.push(SkippedImage { image: p.source_id, reason: SkipReason::MasterSkipped });
        false
    });
    data.skipped.extend(skipped);
}

/// Drop the catalog's images whose original is not a file type LightCraft reads, saying so in the
/// import's warnings.
///
/// The catalog decides these paths, and a record it leaves behind is one click from the system
/// opener (Photo ▸ Edit In ▸ Open Original in Other Editor). A record for `payload.exe`,
/// `start.lnk` or `Evil.app` would make a `.lrcat` someone was sent a way to have a program run
/// (issue #2135), so such an image never becomes a photo: not imported, not relinkable, not
/// openable. A virtual copy of a refused original goes with it — it has no original of its own.
///
/// Called where the catalog is read and again where its photos become records, so neither route
/// can keep one; the second pass finds nothing left to say.
pub(crate) fn refuse_non_media(data: &mut CatalogImport) {
    // masters first: a virtual copy may come before its master in the table
    let refused: HashSet<i64> = data
        .photos
        .iter()
        .filter(|p| number(&p.image, "masterImage") == 0 && !crate::open_path::is_media(Path::new(&p.path)))
        .map(|p| p.source_id)
        .collect();
    if refused.is_empty() {
        return;
    }
    let mut warnings = Vec::new();
    data.photos.retain(|p| {
        let master = number(&p.image, "masterImage");
        // `masterImage` is 0 for a master, so a refused id of 0 (a malformed catalog) must not
        // take every master of the catalog with it
        if master != 0 {
            if !refused.contains(&master) {
                return true;
            }
            warnings.push(format!("image {}: not imported; its original was not imported", p.source_id));
            return false;
        }
        if !refused.contains(&p.source_id) {
            return true;
        }
        warnings.push(format!("image {}: not imported; {} isn't one of the file types LightCraft reads", p.source_id, p.path));
        false
    });
    data.warnings.extend(warnings);
}

fn ensure_read_active(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) { Err("Lightroom import cancelled".into()) } else { Ok(()) }
}

fn mapped_settings(
    text: &str,
    target: crate::crs::Target,
    aspect: f64,
    orientation: Orientation,
    stored_aspect: f64,
) -> Result<(Value, Vec<String>), String> {
    let mut root = crate::preset_import::parse_lua(text)?;
    let deferred = strip_lua_sentinels(&mut root);
    let (mut props, values) = crate::preset_import::lua_settings_props(&root)?;
    // A catalog's develop settings carry the crop edges and angle but no `HasCrop` (XMP has it):
    // any crop field means the photo is cropped.
    const CROP: [&str; 5] = ["crs:CropLeft", "crs:CropTop", "crs:CropRight", "crs:CropBottom", "crs:CropAngle"];
    if !props.contains_key("crs:HasCrop") && CROP.iter().any(|k| props.contains_key(*k)) {
        props.insert("crs:HasCrop".into(), vec!["True".into()]);
    }
    let (mut partial, mut unknown) = crate::crs::to_partial_report(&props, Some(&values), target, aspect);
    catalog_crop(&mut partial, orientation, stored_aspect);
    if deferred > 0 {
        unknown.push("Deferred Lightroom adjustments (-999999)".into());
    }
    Ok((partial, unknown))
}

/// Lightroom's `Adobe_images.orientation`: where the stored image's top-left and top-right corners
/// land in the displayed photo, naming the displayed corners A, B, C, D clockwise from the top left
/// (`AB` upright, `BC` a quarter turn clockwise, `DA` a quarter turn counter-clockwise, `BA` mirrored).
fn lightroom_orientation(code: &str) -> Option<Orientation> {
    const ALL: [Orientation; 8] = [
        Orientation::Normal,
        Orientation::FlipH,
        Orientation::Rotate180,
        Orientation::FlipV,
        Orientation::Transpose,
        Orientation::Rotate90,
        Orientation::Transverse,
        Orientation::Rotate270,
    ];
    let corner = |o: Orientation, x: f64, y: f64| match o.map(x, y, 1.0, 1.0) {
        (u, v) if u < 0.5 && v < 0.5 => 'A',
        (_, v) if v < 0.5 => 'B',
        (u, _) if u >= 0.5 => 'C',
        _ => 'D',
    };
    ALL.into_iter().find(|&o| code.chars().eq([corner(o, 0.0, 0.0), corner(o, 1.0, 0.0)]))
}

/// Lightroom's crop in LightCraft's frame. `crs` gives the crop's top-left (`CropLeft`, `CropTop`)
/// and bottom-right (`CropRight`, `CropBottom`) corners on the stored image (before orientation and
/// straightening), normalized to its size, plus a straighten angle that turns the other way to
/// LightCraft's. LightCraft's crop is a rectangle in the oriented image rotated by `angle` about its
/// centre, normalized to the oriented image's size, so the corners go through the orientation and
/// then the rotation; `stored_aspect` (width / height of the stored image) matters once the
/// rectangle turns. On a real catalog this reproduced the frame aspect of all 223 straightened crops
/// in Lightroom's exports within 1%.
fn catalog_crop(partial: &mut Value, orientation: Orientation, stored_aspect: f64) {
    let Some(geometry) = partial.pointer_mut("/crop/geometry").and_then(Value::as_object_mut) else { return };
    let edge = |k: &str| geometry.get("rect").and_then(|r| r.get(k)).and_then(Value::as_f64);
    let (Some(x0), Some(y0), Some(x1), Some(y1)) = (edge("x0"), edge("y0"), edge("x1"), edge("y1")) else { return };
    let lightroom_angle = geometry.get("angle").and_then(Value::as_f64).unwrap_or(0.0);
    if orientation == Orientation::Normal && lightroom_angle == 0.0 {
        return;
    }
    let w = if stored_aspect.is_finite() && stored_aspect > 0.0 { stored_aspect } else { 1.0 };
    let (ow, oh) = if orientation.swaps_axes() { (1.0, w) } else { (w, 1.0) };
    // a mirror turns the straighten the other way too
    let angle = if orientation.to_parts().0 { lightroom_angle } else { -lightroom_angle };
    let to_rotated = Affine::rotate_about(angle.to_radians(), Point::new(ow / 2.0, oh / 2.0));
    let corner = |x: f64, y: f64| {
        let (u, v) = orientation.map(x * w, y, w, 1.0);
        to_rotated.apply(Point::new(u, v))
    };
    let r = Rect::from_points(corner(x0, y0), corner(x1, y1));
    geometry.insert("rect".into(), json!({"x0": r.x0 / ow, "y0": r.y0 / oh, "x1": r.x1 / ow, "y1": r.y1 / oh}));
    geometry.insert("angle".into(), json!(angle));
}

fn strip_lua_sentinels(value: &mut crate::preset_import::Lua) -> usize {
    use crate::preset_import::Lua;
    match value {
        Lua::Num(n) if *n == -999999.0 => {
            *value = Lua::Nil;
            1
        }
        Lua::Table(array, fields) => {
            let mut removed = array.iter_mut().map(strip_lua_sentinels).sum::<usize>();
            fields.retain_mut(|(_, value)| {
                removed += strip_lua_sentinels(value);
                !matches!(value, Lua::Nil)
            });
            removed
        }
        _ => 0,
    }
}

fn strip_xmp_sentinels(value: &mut Value) {
    match value {
        Value::Object(fields) => fields.retain(|_, value| {
            if value.as_f64() == Some(-999999.0) {
                return false;
            }
            strip_xmp_sentinels(value);
            true
        }),
        Value::Array(values) => {
            if values.iter().any(|v| v.as_f64() == Some(-999999.0)) {
                values.clear();
            } else {
                for v in values {
                    strip_xmp_sentinels(v);
                }
            }
        }
        _ => {}
    }
}

/// Import into the current library. Existing edited records are preserved unless explicitly
/// requested; missing originals remain catalogued for later relinking. The source catalog is read-only.
pub fn import(s: &mut crate::Session, path: &Path, update_existing: bool) -> crate::Result<Value> {
    let mut job = crate::lightroom_job::LightroomJob::new(s, path.to_path_buf(), update_existing)?;
    let cancel = AtomicBool::new(false);
    let prepared = job.prepare(&cancel)?;
    let completion = crate::lightroom_job::commit_prepared(s, prepared)?;
    if s.library.as_ref().is_some_and(|library| library.on_disk) {
        s.persist()?;
    }
    completion.finalization.finish()?;
    Ok(completion.report)
}

#[cfg(test)]
fn apply(s: &mut crate::Session, data: CatalogImport, update_existing: bool) -> crate::Result<Value> {
    let archive_dir = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join("Interop"));
    let index_path = archive_dir.as_ref().map(|p| p.join("lightroom-index.json"));
    let mut index = load_index(index_path.as_deref())?;
    let archive_path = archive_dir.as_ref().map(|dir| crate::lightroom_archive::store(dir, &data).map_err(error)).transpose()?;
    let undo0 = s.undo.len();
    let existing: HashMap<_, _> =
        s.catalog.photos().filter(|p| p.copy_of.is_none()).filter_map(|p| source_path(p).map(|path| (key(path), p.id))).collect();
    let paths: Vec<_> = data
        .photos
        .iter()
        .filter(|p| number(&p.image, "masterImage") == 0 && !existing.contains_key(&key(&p.path)) && p.original_is_file())
        .map(|p| p.path.clone())
        .collect();
    let missing_sources: HashSet<String> =
        data.photos.iter().filter(|p| number(&p.image, "masterImage") == 0).filter(|p| !p.original_is_file()).map(|p| key(&p.path)).collect();
    let files = crate::import::import(s, &paths, crate::import::ImportMode::Add)?;
    let applied =
        apply_prepared(s, data, ApplyPrepared { update_existing, files, index: &mut index, archive_path, missing_sources: &missing_sources, undo0 })?;
    if let Some(path) = index_path {
        s.persist()?;
        let bytes = serde_json::to_vec(&applied.index).map_err(|e| error(e.to_string()))?;
        lightcraft_catalog::safe_file::write_atomic(&path, &bytes).map_err(|e| error(e.to_string()))?;
    }
    Ok(applied.report)
}

/// Apply prepared normal-file results and Lightroom metadata in one owner-thread operation.
/// This function does no filesystem work; callers finish its returned index separately.
pub(crate) struct ApplyPrepared<'a> {
    pub(crate) update_existing: bool,
    pub(crate) files: crate::import::ImportReport,
    pub(crate) index: &'a mut ImportIndex,
    pub(crate) archive_path: Option<PathBuf>,
    pub(crate) missing_sources: &'a HashSet<String>,
    pub(crate) undo0: usize,
}

/// Turn the read catalog into library records in one undo step and return the import report
/// (added, missing, failed, warnings and skipped images).
pub(crate) fn apply_prepared(s: &mut crate::Session, mut data: CatalogImport, context: ApplyPrepared<'_>) -> crate::Result<AppliedImport> {
    let ApplyPrepared { update_existing, files, index, archive_path, missing_sources, undo0 } = context;
    // nothing but a file type LightCraft reads becomes a record, whichever route got here (#2135)
    refuse_non_media(&mut data);
    validate(&data).map_err(error)?;
    let mut warnings = data.warnings.clone();
    let duplicates: HashMap<_, _> = files.duplicates.iter().filter_map(|d| d.existing.map(|id| (key(&d.path), PhotoId(id)))).collect();
    let mut by_path: HashMap<_, _> =
        s.catalog.photos().filter(|p| p.copy_of.is_none()).filter_map(|p| source_path(p).map(|path| (key(path), p.id))).collect();
    let mut ids = HashMap::new();
    let mut ops = Vec::new();
    let now = (s.clock)();
    let mut missing = Vec::new();
    // Normal import counts probed originals; catalog import also adds offline records,
    // distinct originals with identical bytes, and virtual copies.
    let mut imported = files.imported.len();
    let mut promoted = HashSet::new();
    let mut unmapped = BTreeMap::new();
    let mut preserved = 0;
    // Masters precede their virtual copies.
    let mut photos: Vec<_> = data.photos.iter().collect();
    photos.sort_by_key(|p| number(&p.image, "masterImage") != 0);
    let copy_masters: HashSet<_> = data.photos.iter().map(|p| number(&p.image, "masterImage")).filter(|id| *id != 0).collect();
    let refused: HashSet<_> = data.photos.iter().filter(|p| matches!(p.original, Original::Refused(_))).map(|p| p.source_id).collect();
    let mut planned: HashMap<PhotoId, Photo> = HashMap::new();
    for src in photos {
        let master = number(&src.image, "masterImage");
        if master == 0 && missing_sources.contains(&key(&src.path)) {
            missing.push(src.path.clone());
        }
        // A refused path must not become a library record the app would look at later; its
        // virtual copies go with it.
        if refused.contains(&src.source_id) || refused.contains(&master) {
            continue;
        }
        let indexed = index
            .photos
            .get(&identity(&data, src))
            .copied()
            .map(PhotoId)
            .filter(|id| s.catalog.photo(*id).is_some_and(|p| source_path(p).is_some_and(|path| key(path) == key(&src.path))));
        let existing_id = indexed.or_else(|| if master == 0 { by_path.get(&key(&src.path)).copied() } else { None });
        let mut p = if let Some(id) = existing_id.and_then(|id| s.catalog.photo(id)) {
            (**id).clone()
        } else if master != 0 {
            let master_id = ids.get(&master).copied().ok_or_else(|| error(format!("virtual copy {}: missing master", src.source_id)))?;
            let mut p = planned
                .get(&master_id)
                .cloned()
                .or_else(|| s.catalog.photo(master_id).map(|p| (**p).clone()))
                .ok_or_else(|| error("virtual copy master is unavailable"))?;
            p.id = s.catalog.alloc_photo_id();
            p.copy_of = Some(master_id);
            p.copy_name = Some(text(&src.image, "copyName").to_string());
            p
        } else if let Some(duplicate) = duplicates.get(&key(&src.path)).and_then(|id| s.catalog.photo(*id)) {
            // Catalogs can contain distinct originals with identical bytes and independent edits.
            let mut p = (**duplicate).clone();
            p.id = s.catalog.alloc_photo_id();
            p.source = Source::File { path: src.path.clone() };
            p.file_name = Path::new(&src.path).file_name().map(|n| n.to_string_lossy().into()).unwrap_or_else(|| src.path.clone());
            p
        } else {
            let id = s.catalog.alloc_photo_id();
            let name = Path::new(&src.path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| src.path.clone());
            let mut p = Photo::new(
                id,
                Source::File { path: src.path.clone() },
                &name,
                text(&src.image, "fileFormat"),
                number(&src.image, "fileWidth").max(0) as u32,
                number(&src.image, "fileHeight").max(0) as u32,
                &now,
            );
            if ["RAW", "DNG", "ARW", "CR2", "CR3", "NEF", "NRW", "RAF", "ORF", "RW2", "PEF"].contains(&p.format.to_uppercase().as_str()) {
                p.kind = lightcraft_catalog::MediaKind::Raw;
            }
            p
        };
        ids.insert(src.source_id, p.id);
        index.photos.insert(identity(&data, src), p.id.0);
        let keep_develop = existing_id.is_some() && p.edited.is_some() && !update_existing;
        let previous_develop = (p.develop.clone(), p.edited.clone());
        if keep_develop {
            preserved += 1;
        }
        if !src.xmp.is_empty() {
            match crate::sidecar::parse_sidecar(&src.xmp, crate::crs::Target::for_photo(&p)) {
                Ok(mut sc) => {
                    if let Some(crate::sidecar::DevelopPatch::Partial(partial)) = &mut sc.develop {
                        strip_xmp_sentinels(partial);
                    }
                    crate::sidecar::merge_into(&mut p, &sc, &now);
                }
                Err(e) => warnings.push(format!("image {} XMP: {e}", src.source_id)),
            }
        }
        p.rating = number(&src.image, "rating").clamp(0, 5) as u8;
        p.flag = match number(&src.image, "pick") {
            1 => Flag::Pick,
            -1 => Flag::Reject,
            _ => Flag::None,
        };
        p.label = s.catalog.label_from_name(text(&src.image, "colorLabels"));
        if !text(&src.image, "captureTime").is_empty() {
            p.captured = Some(text(&src.image, "captureTime").into());
        }
        if !src.keywords.is_empty() {
            p.meta.keywords = src.keywords.clone();
        }
        if !src.settings.is_empty() && !keep_develop {
            let orientation = lightroom_orientation(text(&src.image, "orientation")).unwrap_or_default();
            let stored_aspect = number(&src.image, "fileWidth") as f64 / number(&src.image, "fileHeight") as f64;
            match mapped_settings(
                &src.settings,
                crate::crs::Target::for_photo(&p),
                p.width.max(1) as f64 / p.height.max(1) as f64,
                orientation,
                stored_aspect,
            ) {
                Ok((partial, unknown)) => {
                    p.develop = Arc::new(lightcraft_develop::apply_partial(&p.develop, &partial, 1.0));
                    p.edited = Some(now.clone());
                    if !unknown.is_empty() {
                        unmapped.insert(src.source_id, unknown);
                    }
                }
                Err(e) => warnings.push(format!("image {} settings: {e}", src.source_id)),
            }
        }
        if keep_develop {
            (p.develop, p.edited) = previous_develop;
        }
        // Missing files bypass normal-file preparation, including its Local promotion.
        // Import still joins the library, and a virtual copy must inherit that membership.
        let was_local = p.local;
        p.local = false;
        let photo_id = p.id;
        // Only virtual-copy masters need a second owned photo while operations are staged.
        // Keeping every photo here doubled the new-record working set on large catalogs.
        if copy_masters.contains(&src.source_id) {
            planned.insert(photo_id, p.clone());
        }
        if s.catalog.photo(p.id).is_some() {
            if was_local && promoted.insert(p.id) {
                imported += 1;
                ops.push(Op::SetLocal { id: p.id, local: false });
            }
            ops.extend([
                Op::SetRating { id: p.id, rating: p.rating },
                Op::SetFlag { id: p.id, flag: p.flag },
                Op::SetLabel { id: p.id, label: p.label },
                Op::SetMeta { id: p.id, meta: Box::new(p.meta.clone()) },
                Op::SetCaptured { id: p.id, captured: p.captured.clone() },
            ]);
            ops.push(Op::SetDevelop { id: p.id, settings: p.develop.clone(), label: "Lightroom catalog".into(), edited: p.edited.clone() });
        } else {
            imported += 1;
            ops.push(Op::AddPhoto { photo: Box::new(p) });
        }
        by_path.entry(key(&src.path)).or_insert(photo_id);
    }
    let mut albums = HashMap::new();
    for collection in &data.collections {
        if number(collection, "systemOnly") != 0 {
            continue;
        }
        let collection_key = format!("{}:{}", key(&data.source), number(collection, "id_local"));
        let id = index
            .collections
            .get(&collection_key)
            .copied()
            .map(lightcraft_catalog::AlbumId)
            .filter(|id| s.catalog.album(*id).is_some())
            .unwrap_or_else(|| s.catalog.alloc_album_id());
        index.collections.insert(collection_key, id.0);
        albums.insert(number(collection, "id_local"), id);
    }
    let parents: HashMap<_, _> = data.collections.iter().map(|r| (number(r, "id_local"), number(r, "parent"))).collect();
    let mut grouped_members: HashMap<i64, Vec<&Row>> = HashMap::new();
    for member in &data.members {
        grouped_members.entry(number(member, "collection")).or_default().push(member);
    }
    for members in grouped_members.values_mut() {
        members.sort_by(|a, b| text(a, "positionInCollection").cmp(text(b, "positionInCollection")));
    }
    let mut ordered: Vec<_> = data.collections.iter().collect();
    ordered.sort_by_key(|r| {
        let mut depth = 0;
        let mut at = number(r, "parent");
        while at != 0 && depth < 64 {
            depth += 1;
            at = parents.get(&at).copied().unwrap_or(0);
        }
        depth
    });
    for collection in ordered {
        let Some(id) = albums.get(&number(collection, "id_local")).copied() else { continue };
        let mut album = Album::new(id, text(collection, "name"));
        album.parent = albums.get(&number(collection, "parent")).copied();
        album.folder = text(collection, "creationId").contains("group");
        if text(collection, "creationId").contains("smart") {
            warnings.push(format!("{}: smart rules preserved in archive; imported current membership", album.name));
        }
        if !album.folder {
            album.photos = grouped_members
                .get(&number(collection, "id_local"))
                .into_iter()
                .flatten()
                .filter_map(|r| ids.get(&number(r, "image")).copied())
                .collect();
        }
        if s.catalog.album(id).is_some() {
            // Reimport preserves the user's album name/hierarchy, merges only new membership.
            if let Some(old) = s.catalog.album(id).filter(|a| !a.folder && !a.is_smart()) {
                let mut photos = old.photos.clone();
                let mut seen: HashSet<_> = photos.iter().copied().collect();
                for photo in album.photos {
                    if seen.insert(photo) {
                        photos.push(photo);
                    }
                }
                ops.push(Op::SetAlbumPhotos { id, photos });
            }
        } else {
            ops.push(Op::AddAlbum { album });
        }
    }
    if !ops.is_empty() {
        s.commit("Import Lightroom Catalog", Op::Batch { ops })?;
    }
    s.merge_undo(s.undo.len().saturating_sub(undo0), "Import Lightroom Catalog");
    Ok(AppliedImport {
        report: json!({"source":data.source,"photos":ids.len(),"imported":imported,"library":s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.to_string_lossy()),"collections":albums.len(),"editsNotImported":data.edits_not_imported,"preservedExistingEdits":preserved,"missing":missing,"failed":files.failed,"warnings":warnings,"skipped":data.skipped,"unmapped":unmapped,"archive":archive_path,"mapping":ids}),
        index: index.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_geom::CropGeometry;
    fn row(v: Value) -> Row {
        serde_json::from_value(v).unwrap()
    }
    fn sample() -> CatalogImport {
        let mut photo = CatalogPhoto {
            source_id: 1,
            uuid: "test-master".into(),
            path: "/missing/test.ARW".into(),
            image: row(json!({"id_local":1,"rating":5,"pick":1,"fileFormat":"ARW","fileWidth":20,"fileHeight":10})),
            settings: "s = { Exposure2012 = 1.25, Contrast2012 = 17, CameraProfile = \"Example\" }".into(),
            xmp: String::new(),
            keywords: vec!["Travel|Goa".into()],
            history: vec![row(json!({"text":"s = { Exposure2012 = 0.5 }"}))],
            snapshots: Vec::new(),
            original: Original::Local,
        };
        let mut copy = photo.clone();
        copy.source_id = 2;
        copy.uuid = "test-copy".into();
        copy.image = row(json!({"id_local":2,"masterImage":1,"copyName":"Mono","rating":2,"pick":-1}));
        copy.settings = "s = { ConvertToGrayscale = true, Exposure2012 = -0.5 }".into();
        photo.image.insert("captureTime".into(), json!("2024-01-16T12:34:56"));
        CatalogImport {
            source: "test.lrcat".into(),
            photos: vec![copy, photo],
            collections: vec![
                row(json!({"id_local":9,"name":"Travel","creationId":"collection.group"})),
                row(json!({"id_local":10,"parent":9,"name":"Keepers","creationId":"collection"})),
            ],
            members: vec![
                row(json!({"collection":10,"image":2,"positionInCollection":"a"})),
                row(json!({"collection":10,"image":1,"positionInCollection":"b"})),
            ],
            collection_content: Vec::new(),
            warnings: Vec::new(),
            edits_not_imported: 0,
            skipped: Vec::new(),
        }
    }
    /// A crafted `.lrcat` that names a program as a photo's original gets no record for it, so
    /// Photo ▸ Edit In ▸ Open Original has nothing to hand the system opener (#2135). Its virtual
    /// copies go with it; the catalog's real photos still import.
    #[test]
    fn a_catalog_original_that_is_not_a_photo_file_is_not_imported() {
        let mut data = sample();
        let mut exe = data.photos[1].clone();
        exe.source_id = 3;
        exe.uuid = "payload".into();
        exe.path = "/Users/sky/Downloads/payload.exe".into();
        exe.image = row(json!({"id_local":3,"fileFormat":"ARW","fileWidth":20,"fileHeight":10}));
        let mut exe_copy = exe.clone();
        exe_copy.source_id = 4;
        exe_copy.uuid = "payload-copy".into();
        exe_copy.path = "/Users/sky/Downloads/payload.exe".into();
        exe_copy.image = row(json!({"id_local":4,"masterImage":3,"copyName":"Mono"}));
        // the copy comes before its master, as a catalog's table order may have it
        data.photos.insert(0, exe_copy);
        data.photos.push(exe);
        for bad in [r"C:\Users\sky\start.lnk", "/Applications/Evil.app", "/Users/sky/go.command", "/Users/sky/run.bat"] {
            let mut one = data.clone();
            one.photos.retain(|p| p.source_id < 3);
            let mut planted = one.photos[1].clone();
            planted.source_id = 5;
            planted.uuid = "planted".into();
            planted.path = bad.into();
            planted.image = row(json!({"id_local":5,"fileFormat":"ARW"}));
            one.photos.push(planted);
            let mut s = crate::Session::new();
            let r = apply(&mut s, one, false).unwrap();
            assert_eq!(r["photos"], 2, "{bad} is not catalogued: {r}");
            assert!(s.catalog.photos().all(|p| !matches!(&p.source, Source::File { path } if path == bad)), "{bad} became a library record");
        }
        let mut s = crate::Session::new();
        let r = apply(&mut s, data, false).unwrap();
        assert_eq!(r["photos"], 2, "only the real master and its copy: {r}");
        assert!(s.catalog.photos().all(|p| !matches!(&p.source, Source::File { path } if path.ends_with(".exe"))), "the program was catalogued");
        let warnings = r["warnings"].to_string();
        assert!(warnings.contains("payload.exe") && warnings.contains("file types LightCraft reads"), "{warnings}");
        assert!(warnings.contains("image 4: not imported"), "the virtual copy of the refused original goes with it: {warnings}");
    }

    #[test]
    fn offline_local_master_and_copy_join_library_with_one_undo_step() {
        let mut s = crate::Session::new();
        let data = sample();
        let master_id = s.catalog.alloc_photo_id();
        let mut master = Photo::new(master_id, Source::File { path: "/missing/test.ARW".into() }, "test.ARW", "ARW", 20, 10, "before");
        master.local = true;
        s.commit("Browse folder", Op::AddPhoto { photo: Box::new(master) }).unwrap();
        s.undo.clear();
        let before = s.catalog.photo(master_id).unwrap().clone();
        let mut index = ImportIndex::default();
        let missing_sources = [key("/missing/test.ARW")].into_iter().collect();
        let mut import = |s: &mut crate::Session| {
            let undo0 = s.undo.len();
            apply_prepared(
                s,
                data.clone(),
                ApplyPrepared {
                    update_existing: false,
                    files: Default::default(),
                    index: &mut index,
                    archive_path: None,
                    missing_sources: &missing_sources,
                    undo0,
                },
            )
            .unwrap()
            .report
        };
        let first = import(&mut s);
        assert_eq!(first["mapping"]["1"], master_id.0);
        assert_eq!(first["imported"], 2, "the promoted master and new copy both join All Photos");
        assert_eq!(first["missing"], json!(["/missing/test.ARW"]));
        assert_eq!(s.catalog.len(), 2);
        assert!(s.catalog.photos().all(|photo| !photo.local));
        assert_eq!(s.undo.len(), 1);
        let second = import(&mut s);
        assert_eq!(second["imported"], 0);
        assert_eq!(second["mapping"], first["mapping"]);
        s.undo_step().unwrap();
        s.undo_step().unwrap();
        assert_eq!(s.catalog.len(), 1);
        assert_eq!(s.catalog.photo(master_id), Some(&before));
        assert!(s.catalog.albums().next().is_none());
        let third = import(&mut s);
        assert_eq!(third["imported"], 2);
        assert!(s.catalog.photos().all(|photo| !photo.local));
    }

    #[test]
    fn imports_copies_collections_ratings_and_mapped_edits_in_one_undo_step() {
        let mut s = crate::Session::new();
        let data = sample();
        let r = apply(&mut s, data.clone(), false).unwrap();
        assert_eq!(r["photos"], 2);
        assert_eq!(r["imported"], 2, "the offline master and virtual copy both count as added");
        assert_eq!(r["missing"].as_array().unwrap().len(), 1);
        let master = s.catalog.photo(PhotoId(r["mapping"]["1"].as_u64().unwrap())).unwrap();
        assert_eq!(master.rating, 5);
        assert_eq!(master.flag, Flag::Pick);
        assert_eq!(master.meta.keywords, vec!["Travel|Goa"]);
        assert_eq!(master.develop.light.exposure, 1.25);
        let copy = s.catalog.photos().find(|p| p.copy_of.is_some()).unwrap();
        assert_eq!(copy.copy_of, Some(master.id));
        assert_eq!(copy.rating, 2);
        assert_eq!(copy.flag, Flag::Reject);
        assert_eq!(copy.develop.light.exposure, -0.5);
        let album = s.catalog.albums().find(|a| a.name == "Keepers").unwrap();
        assert_eq!(album.photos, vec![copy.id, master.id]);
        assert!(album.parent.is_some());
        assert!(r["unmapped"]["1"].as_array().unwrap().iter().any(|v| v.as_str() == Some("CameraProfile")));
        assert_eq!(s.undo.len(), 1);
        s.undo_step().unwrap();
        assert!(s.catalog.is_empty());
        assert!(s.catalog.albums().next().is_none());
        assert!(data.photos[1].settings.contains("CameraProfile")); // unmapped source stays intact
    }
    #[test]
    fn preserves_existing_edits_and_rejects_invalid_structure_before_mutation() {
        let mut s = crate::Session::new();
        let data = sample();
        apply(&mut s, data.clone(), false).unwrap();
        let id = s.catalog.photos().find(|p| p.copy_of.is_none()).unwrap().id;
        s.set_develop(id, lightcraft_develop::DevelopSettings::default(), "Personal edit").unwrap();
        let r = apply(&mut s, data.clone(), false).unwrap();
        assert!(r["preservedExistingEdits"].as_u64().unwrap() > 0);
        assert_eq!(s.catalog.photo(id).unwrap().develop.light.exposure, 0.0);
        let before = s.catalog.to_snapshot();
        let mut cyclic = data;
        cyclic.collections[0].insert("parent".into(), json!(10));
        assert!(apply(&mut s, cyclic, false).is_err());
        assert_eq!(s.catalog.to_snapshot(), before);
        assert!(
            mapped_settings(&format!("{}0{}", "{".repeat(1000), "}".repeat(1000)), crate::crs::Target::RawAbsolute, 1.0, Orientation::Normal, 1.5)
                .is_err()
        );
    }
    #[test]
    fn compressed_xmp_is_bounded_and_settings_are_data_only() {
        let packet = b"<x:xmpmeta>example</x:xmpmeta>";
        let mut encoded = (packet.len() as u32).to_be_bytes().to_vec();
        encoded.extend(miniz_oxide::deflate::compress_to_vec_zlib(packet, 6));
        assert_eq!(decode_xmp_bytes(&encoded).unwrap(), String::from_utf8_lossy(packet));
        encoded[0] = 0xff;
        assert!(decode_xmp_bytes(&encoded).is_err());
        let (partial, _) = mapped_settings(
            "s = { Exposure2012 = 0.75, ToneCurvePV2012 = {0,0,128,150,255,255} }",
            crate::crs::Target::RawAbsolute,
            1.5,
            Orientation::Normal,
            1.5,
        )
        .unwrap();
        assert_eq!(partial["light"]["exposure"], 0.75);
    }

    #[test]
    fn custom_white_balance_on_relative_wb_raws_is_not_read_as_kelvin() {
        // Issue #510: a catalog keeps no as-shot white for Custom photos, so on a raw developed
        // relative to its as-shot look the edit stays As Shot and is reported; a DNG keeps the Kelvin
        let settings =
            "s = { WhiteBalance = \"Custom\", Temperature = 3578, Tint = -5, CustomTemperature = 3578.95, CustomTint = -5, Exposure2012 = 0.5 }";
        let (partial, unknown) = mapped_settings(settings, crate::crs::Target::RawRelative, 1.5, Orientation::Normal, 1.5).unwrap();
        assert_eq!(partial["wb"], json!({"mode": "asShot"}), "{partial}");
        assert_eq!(partial["light"]["exposure"], 0.5);
        assert!(unknown.iter().any(|k| k.starts_with("Temperature, Tint")), "{unknown:?}");
        let (partial, unknown) = mapped_settings(settings, crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, 1.5).unwrap();
        assert_eq!(
            (partial["wb"]["mode"].as_str(), partial["wb"]["temp"].as_f64(), partial["wb"]["tint"].as_f64()),
            (Some("custom"), Some(3578.0), Some(-5.0))
        );
        assert!(!unknown.iter().any(|k| k.starts_with("Temperature")), "{unknown:?}");
    }

    #[test]
    fn catalog_crops_without_has_crop_are_imported() {
        // Lightroom Classic catalogs store crop edges and angle but never `HasCrop`
        let (partial, unknown) = mapped_settings(
            "s = { CropConstrainAspectRatio = true, CropLeft = 0.042702, CropRight = 0.957298, CropTop = 0.1 }",
            crate::crs::Target::RawAbsolute,
            1.5,
            Orientation::Normal,
            1.5,
        )
        .unwrap();
        let crop = &partial["crop"]["geometry"];
        assert_eq!(crop["rect"]["x0"], 0.042702);
        assert_eq!(crop["rect"]["y0"], 0.1);
        assert_eq!(crop["rect"]["x1"], 0.957298);
        assert_eq!(crop["rect"]["y1"], 1.0);
        assert_eq!(crop["angle"], 0.0);
        assert!(!unknown.iter().any(|k| k.starts_with("CropLeft")), "{unknown:?}");
        // an angle alone still means a crop (Lightroom's angle turns the other way to LightCraft's)
        let (partial, unknown) = mapped_settings("s = { CropAngle = -1.5 }", crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, 1.5).unwrap();
        assert_eq!(partial["crop"]["geometry"]["angle"], 1.5);
        assert!(!unknown.iter().any(|k| k.starts_with("CropAngle")), "{unknown:?}");
        // no crop fields: the crop stays untouched
        let (partial, _) = mapped_settings("s = { Exposure2012 = 0.5 }", crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, 1.5).unwrap();
        assert!(partial.get("crop").is_none());
    }

    #[test]
    fn lightroom_orientation_codes() {
        // the three seen in a real catalog, checked against each file's EXIF orientation (1, 6, 8)
        assert_eq!(lightroom_orientation("AB"), Some(Orientation::Normal));
        assert_eq!(lightroom_orientation("BC"), Some(Orientation::Rotate90));
        assert_eq!(lightroom_orientation("DA"), Some(Orientation::Rotate270));
        assert_eq!(lightroom_orientation("CD"), Some(Orientation::Rotate180));
        assert_eq!(lightroom_orientation("BA"), Some(Orientation::FlipH));
        assert_eq!(lightroom_orientation("DC"), Some(Orientation::FlipV));
        assert_eq!(lightroom_orientation("AD"), Some(Orientation::Transpose));
        assert_eq!(lightroom_orientation("CB"), Some(Orientation::Transverse));
        for code in ["", "A", "ABC", "AC", "ab", "XY"] {
            assert_eq!(lightroom_orientation(code), None, "{code:?}");
        }
    }

    #[test]
    fn catalog_crops_follow_the_photo_orientation() {
        let crop = |o: Orientation| {
            let settings = "s = { CropLeft = 0.1, CropRight = 0.6, CropTop = 0.2, CropBottom = 0.9 }";
            let (partial, _) = mapped_settings(settings, crate::crs::Target::RawAbsolute, 1.5, o, 1.5).unwrap();
            let g = &partial["crop"]["geometry"];
            let r = &g["rect"];
            [r["x0"].as_f64(), r["y0"].as_f64(), r["x1"].as_f64(), r["y1"].as_f64(), g["angle"].as_f64()].map(|v| (v.unwrap() * 1e9).round() / 1e9)
        };
        // (x, y) of the stored image → (1 − y, x) a quarter turn clockwise, (y, 1 − x) counter-clockwise
        assert_eq!(crop(Orientation::Normal), [0.1, 0.2, 0.6, 0.9, 0.0]);
        assert_eq!(crop(Orientation::Rotate90), [0.1, 0.1, 0.8, 0.6, 0.0]);
        assert_eq!(crop(Orientation::Rotate270), [0.2, 0.4, 0.9, 0.9, 0.0]);
        assert_eq!(crop(Orientation::Rotate180), [0.4, 0.1, 0.9, 0.8, 0.0]);
        assert_eq!(crop(Orientation::FlipH), [0.4, 0.2, 0.9, 0.9, 0.0]);
        // a mirror turns the straighten the other way
        let angle = |o: Orientation| {
            let (partial, _) = mapped_settings("s = { CropAngle = 2 }", crate::crs::Target::RawAbsolute, 1.5, o, 1.5).unwrap();
            partial["crop"]["geometry"]["angle"].as_f64().unwrap()
        };
        assert_eq!(angle(Orientation::Rotate90), -2.0);
        assert_eq!(angle(Orientation::FlipH), 2.0);
        // no crop: nothing to orient
        let (partial, _) = mapped_settings("s = { Exposure2012 = 0.5 }", crate::crs::Target::RawAbsolute, 1.5, Orientation::Rotate90, 1.5).unwrap();
        assert!(partial.get("crop").is_none());
    }

    /// The `crs` corners and angle Lightroom would store for LightCraft crop `g` on a stored image
    /// of aspect `w` (height 1) shown with orientation `o`.
    fn lightroom_crop(g: CropGeometry, o: Orientation, w: f64) -> String {
        let (ow, oh) = if o.swaps_axes() { (1.0, w) } else { (w, 1.0) };
        let to_source = Affine::rotate_about(-g.angle.to_radians(), Point::new(ow / 2.0, oh / 2.0));
        let stored = |x: f64, y: f64| {
            let p = to_source.apply(Point::new(x * ow, y * oh));
            o.inverse().map(p.x, p.y, ow, oh)
        };
        // the rectangle's corners that land top-left and bottom-right on the stored image
        let corners = [stored(g.rect.x0, g.rect.y0), stored(g.rect.x1, g.rect.y0), stored(g.rect.x1, g.rect.y1), stored(g.rect.x0, g.rect.y1)];
        let (tl, br) = (0..4).map(|i| (corners[i], corners[(i + 2) % 4])).find(|(a, b)| a.0 < b.0 && a.1 < b.1).unwrap();
        let angle = if o.to_parts().0 { g.angle } else { -g.angle };
        format!("s = {{ CropLeft = {}, CropTop = {}, CropRight = {}, CropBottom = {}, CropAngle = {angle} }}", tl.0 / w, tl.1, br.0 / w, br.1)
    }

    #[test]
    fn straightened_catalog_crops_round_trip() {
        let g = CropGeometry { rect: Rect::new(0.2, 0.15, 0.7, 0.8), angle: 6.5 };
        for o in [Orientation::Normal, Orientation::Rotate90, Orientation::Rotate270, Orientation::Rotate180, Orientation::FlipH] {
            for angle in [6.5, -6.5] {
                let g = CropGeometry { angle, ..g };
                let (partial, _) = mapped_settings(&lightroom_crop(g, o, 1.5), crate::crs::Target::RawAbsolute, 1.5, o, 1.5).unwrap();
                let got: CropGeometry = serde_json::from_value(partial["crop"]["geometry"].clone()).unwrap();
                let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
                assert!(
                    close(got.angle, g.angle)
                        && [(got.rect.x0, g.rect.x0), (got.rect.y0, g.rect.y0), (got.rect.x1, g.rect.x1), (got.rect.y1, g.rect.y1)]
                            .iter()
                            .all(|&(a, b)| close(a, b)),
                    "{o:?} {angle}: {got:?}"
                );
            }
        }
    }

    #[test]
    fn straightened_catalog_crops_match_lightroom_exports() {
        // Settings and pixel sizes from a real Lightroom Classic catalog (9504 × 6336 stored) and the
        // JPEGs Lightroom exported from it at full size.
        for (settings, o, export) in [
            (
                "s = { CropAngle = -9.34444, CropBottom = 0.802034, CropLeft = 0.05499, CropRight = 0.94501, CropTop = 0.197966 }",
                Orientation::Normal,
                (7725.0, 5150.0),
            ),
            (
                "s = { CropAngle = -1.48964, CropBottom = 0.962457, CropLeft = 0.010429, CropRight = 0.989571, CropTop = 0.037543 }",
                Orientation::Rotate90,
                (6100.0, 9150.0),
            ),
        ] {
            let (partial, _) = mapped_settings(settings, crate::crs::Target::RawAbsolute, 1.5, o, 9504.0 / 6336.0).unwrap();
            let r: Rect = serde_json::from_value(partial["crop"]["geometry"]["rect"].clone()).unwrap();
            let (ow, oh) = if o.swaps_axes() { (6336.0, 9504.0) } else { (9504.0, 6336.0) };
            let (w, h) = (r.width() * ow, r.height() * oh);
            assert!((w - export.0).abs() <= 3.0 && (h - export.1).abs() <= 3.0, "{o:?}: {w:.1} × {h:.1}, Lightroom exported {export:?}");
        }
    }

    #[test]
    fn imported_crops_use_the_catalog_orientation() {
        let mut data = sample();
        data.photos.truncate(1);
        data.photos[0].image.remove("masterImage");
        data.photos[0].image.insert("orientation".into(), json!("BC"));
        data.photos[0].settings = "s = { CropLeft = 0.1, CropRight = 0.6, CropTop = 0.2, CropBottom = 0.9 }".into();
        data.photos[0].xmp = String::new();
        let mut s = crate::Session::new();
        apply(&mut s, data, false).unwrap();
        let g = s.catalog.photos().next().unwrap().develop.crop.geometry;
        let r = [g.rect.x0, g.rect.y0, g.rect.x1, g.rect.y1].map(|v| (v * 1e9).round() / 1e9);
        assert_eq!(r, [0.1, 0.1, 0.8, 0.6]);
    }

    #[test]
    fn deferred_auto_tone_sentinels_never_become_real_slider_values() {
        let mut data = sample();
        data.photos.truncate(1);
        data.photos[0].image.remove("masterImage");
        data.photos[0].settings =
            "s = { AutoTone = true, Exposure2012 = -999999, Contrast2012 = -999999, Shadows2012 = 65, Temperature = 6150 }".into();
        data.photos[0].xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="-999999" crs:Contrast2012="-999999" crs:Blacks2012="-22"/></rdf:RDF></x:xmpmeta>"#.into();
        let mut s = crate::Session::new();
        let report = apply(&mut s, data, false).unwrap();
        let p = s.catalog.photos().next().unwrap();
        assert_eq!(p.develop.light.exposure, 0.0);
        assert_eq!(p.develop.light.contrast, 0.0);
        assert_eq!(p.develop.light.shadows, 65.0);
        assert_eq!(p.develop.light.blacks, -22.0);
        assert!(report["unmapped"]["2"].as_array().unwrap().iter().any(|v| v.as_str().is_some_and(|v| v.contains("Deferred"))));
        let (partial, _) =
            mapped_settings("s = { Exposure2012 = -5, Contrast2012 = -100 }", crate::crs::Target::RawAbsolute, 1.0, Orientation::Normal, 1.5)
                .unwrap();
        assert_eq!(partial["light"]["exposure"], -5.0);
        assert_eq!(partial["light"]["contrast"], -100.0);
    }

    #[test]
    fn wal_checksums_cover_commits_without_materializing_historical_databases() {
        let mut main = vec![0; 512];
        main[16..18].copy_from_slice(&512u16.to_be_bytes());
        assert_eq!(wal_checksum(&[1, 0, 0, 0, 2, 0, 0, 0], false, (0, 0)), (1, 3));
        for big in [false, true] {
            let mut wal = vec![0; 32];
            wal[0..4].copy_from_slice(&(if big { 0x377f0683u32 } else { 0x377f0682u32 }).to_be_bytes());
            wal[4..8].copy_from_slice(&3_007_000u32.to_be_bytes());
            wal[8..12].copy_from_slice(&512u32.to_be_bytes());
            wal[16..20].copy_from_slice(&123u32.to_be_bytes());
            let mut sum = wal_checksum(&wal[..24], big, (0, 0));
            wal[24..28].copy_from_slice(&sum.0.to_be_bytes());
            wal[28..32].copy_from_slice(&sum.1.to_be_bytes());
            for _ in 0..128 {
                let mut frame = vec![0; 536];
                frame[0..4].copy_from_slice(&1u32.to_be_bytes());
                frame[4..8].copy_from_slice(&1u32.to_be_bytes());
                frame[8..12].copy_from_slice(&123u32.to_be_bytes());
                sum = wal_checksum(&frame[..8], big, sum);
                sum = wal_checksum(&frame[24..], big, sum);
                frame[16..20].copy_from_slice(&sum.0.to_be_bytes());
                frame[20..24].copy_from_slice(&sum.1.to_be_bytes());
                wal.extend(frame);
            }
            assert_eq!(committed_wal_len(&main, &wal).unwrap(), wal.len());
            let end = wal.len();
            wal[end - 1] ^= 1;
            assert_eq!(committed_wal_len(&main, &wal).unwrap(), end - 536);
            wal[24] ^= 1;
            assert!(committed_wal_len(&main, &wal).is_err());
            main[17] = 0;
            assert!(committed_wal_len(&main, &wal).is_err());
            main[16..18].copy_from_slice(&512u16.to_be_bytes());
        }
    }

    #[test]
    fn persistent_reimport_keeps_copy_and_collection_ids_and_capture_time() {
        let dir = std::env::temp_dir().join(format!("lightcraft-lrcat-reimport-{}", std::process::id()));
        let mut s = crate::Session::new();
        s.open_library(&dir, false).unwrap();
        let mut data = sample();
        data.collections.reverse(); // Parent must be committed before its child regardless of row order.
        let first = apply(&mut s, data.clone(), false).unwrap();
        s.close_library().unwrap();
        s.open_library(&dir, false).unwrap();
        let id = PhotoId(first["mapping"]["1"].as_u64().unwrap());
        s.commit("Changed time", Op::SetCaptured { id, captured: None }).unwrap();
        let second = apply(&mut s, data, true).unwrap();
        assert_eq!(first["mapping"], second["mapping"]);
        assert_eq!(first["imported"], 2);
        assert_eq!(second["imported"], 0, "reimport updates records instead of adding them");
        assert_eq!(second["missing"].as_array().unwrap().len(), 1, "offline originals are reported on reimport too");
        assert_eq!(second["library"], dir.to_string_lossy().as_ref());
        assert_eq!(s.catalog.len(), 2);
        assert_eq!(s.catalog.albums().count(), 2);
        assert_eq!(s.catalog.photo(id).unwrap().captured.as_deref(), Some("2024-01-16T12:34:56"));
        s.close_library().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Construct a small SQLite fixture from public file-format records, never private media.
    fn varint(mut n: u64) -> Vec<u8> {
        let mut out = vec![(n & 127) as u8];
        n >>= 7;
        while n > 0 {
            out.push((n & 127) as u8 | 128);
            n >>= 7;
        }
        out.reverse();
        out
    }
    fn record(values: &[SqlValue]) -> Vec<u8> {
        let mut header = Vec::new();
        let mut body = Vec::new();
        for value in values {
            let serial = match value {
                SqlValue::Null => 0,
                SqlValue::Integer(n) => {
                    body.extend(n.to_be_bytes());
                    6
                }
                SqlValue::Text(s) => {
                    body.extend(s.as_bytes());
                    13 + 2 * s.len() as u64
                }
                _ => unreachable!(),
            };
            header.extend(varint(serial));
        }
        let mut out = varint(1 + header.len() as u64);
        out.extend(header);
        out.extend(body);
        out
    }
    fn leaf(page: &mut [u8], offset: usize, rows: &[Vec<SqlValue>]) {
        page[offset] = 13;
        page[offset + 3..offset + 5].copy_from_slice(&(rows.len() as u16).to_be_bytes());
        let mut end = page.len();
        for (i, values) in rows.iter().enumerate() {
            let payload = record(values);
            let mut cell = varint(payload.len() as u64);
            cell.extend(varint(i as u64 + 1));
            cell.extend(payload);
            end -= cell.len();
            page[end..end + cell.len()].copy_from_slice(&cell);
            page[offset + 8 + i * 2..offset + 10 + i * 2].copy_from_slice(&(end as u16).to_be_bytes());
        }
        page[offset + 5..offset + 7].copy_from_slice(&(end as u16).to_be_bytes());
    }
    /// A one-row-per-table SQLite file: `(name, columns, row)` each.
    fn native_catalog(tables: &[(&str, &str, Vec<SqlValue>)]) -> Vec<u8> {
        native_catalog_with_last_table_page(tables, None)
    }
    /// [`native_catalog`] whose last table claims `last_page` as its root page.
    fn native_catalog_with_last_table_page(tables: &[(&str, &str, Vec<SqlValue>)], last_page: Option<i64>) -> Vec<u8> {
        catalog_pages(tables, 2048, last_page)
    }

    /// One page per table (`page_size` bytes each) after the schema page.
    fn catalog_pages(tables: &[(&str, &str, Vec<SqlValue>)], page_size: usize, last_page: Option<i64>) -> Vec<u8> {
        let tables: Vec<_> = tables.iter().map(|(name, columns, row)| (*name, *columns, vec![row.clone()])).collect();
        catalog_pages_rows(&tables, page_size, last_page)
    }
    /// A SQLite file with one leaf page per table: `(name, columns, rows)` each. `last_page` overrides the last
    /// table's root page.
    fn native_catalog_rows(tables: &[(&str, &str, Vec<Vec<SqlValue>>)], last_page: Option<i64>) -> Vec<u8> {
        catalog_pages_rows(tables, 2048, last_page)
    }

    /// [`native_catalog_rows`] with `page_size`-byte pages.
    fn catalog_pages_rows(tables: &[(&str, &str, Vec<Vec<SqlValue>>)], page_size: usize, last_page: Option<i64>) -> Vec<u8> {
        use SqlValue::{Integer as I, Text as T};
        let pages = tables.len() + 1;
        let mut bytes = vec![0u8; pages * page_size];
        bytes[..16].copy_from_slice(b"SQLite format 3\0");
        // the header stores 65,536 as 1
        bytes[16..18].copy_from_slice(&u16::try_from(page_size).unwrap_or(1).to_be_bytes());
        bytes[18] = 1;
        bytes[19] = 1;
        bytes[21] = 64;
        bytes[22] = 32;
        bytes[23] = 32;
        bytes[28..32].copy_from_slice(&(pages as u32).to_be_bytes());
        bytes[44..48].copy_from_slice(&4u32.to_be_bytes());
        bytes[56..60].copy_from_slice(&1u32.to_be_bytes());
        let mut schema = Vec::new();
        for (i, (name, columns, rows)) in tables.iter().enumerate() {
            schema.push(vec![
                T("table".into()),
                T((*name).into()),
                T((*name).into()),
                I(if i + 1 == tables.len() { last_page.unwrap_or(i as i64 + 2) } else { i as i64 + 2 }),
                T(format!("CREATE TABLE {name} ({columns})")),
            ]);
            leaf(&mut bytes[(i + 1) * page_size..(i + 2) * page_size], 0, rows);
        }
        leaf(&mut bytes[..page_size], 100, &schema);
        bytes
    }

    fn photo_tables(root: (&str, &str)) -> Vec<(&'static str, &'static str, Vec<SqlValue>)> {
        use SqlValue::{Integer as I, Null as N, Text as T};
        vec![
            (
                "AgLibraryRootFolder",
                "id_local INTEGER PRIMARY KEY, absolutePath, name, relativePathFromCatalog",
                vec![N, T(root.0.into()), T("Photos".into()), T(root.1.into())],
            ),
            ("AgLibraryFolder", "id_local INTEGER PRIMARY KEY, rootFolder, pathFromRoot", vec![N, I(1), T("Photos/".into())]),
            ("AgLibraryFile", "id_local INTEGER PRIMARY KEY, folder, idx_filename", vec![N, I(1), T("test.ARW".into())]),
            (
                "Adobe_images",
                "id_local INTEGER PRIMARY KEY, id_global, rootFile, fileFormat, fileWidth, fileHeight, rating, pick",
                vec![N, T("fixture-image".into()), I(1), T("ARW".into()), I(20), I(10), I(5), I(1)],
            ),
        ]
    }

    #[test]
    fn reads_native_sqlite_tables_without_lightroom_or_external_processes() {
        let bytes = native_catalog(&photo_tables(("/catalog/", "")));
        let path = std::env::temp_dir().join(format!("lightcraft-native-catalog-{}.lrcat", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let catalog = read(&path).unwrap();
        assert_eq!(catalog.photos.len(), 1);
        assert_eq!(catalog.photos[0].uuid, "fixture-image");
        assert_eq!(number(&catalog.photos[0].image, "rating"), 5);
        assert!(catalog.photos[0].path.ends_with("test.ARW"));
        // Put the required image table beyond the old 1 GiB cutoff without allocating it.
        // A main-file-sized allocation would defeat the purpose of the page-backed reader.
        // The large fixture relies on Unix sparse-file behavior; small parity runs everywhere.
        #[cfg(unix)]
        {
            let far_offset = 1u64 << 30;
            let bytes = native_catalog_with_last_table_page(&photo_tables(("/catalog/", "")), Some((far_offset / 2048 + 1) as i64));
            use std::io::{Seek, SeekFrom, Write};
            let mut sparse = std::fs::OpenOptions::new().write(true).truncate(true).open(&path).unwrap();
            sparse.write_all(&bytes).unwrap();
            sparse.set_len(far_offset + 2048).unwrap();
            sparse.seek(SeekFrom::Start(far_offset)).unwrap();
            sparse.write_all(&bytes[4 * 2048..5 * 2048]).unwrap();
            drop(sparse);
            let large = read(&path).unwrap();
            assert_eq!(serde_json::to_value(&large.photos).unwrap(), serde_json::to_value(&catalog.photos).unwrap());
            let mut session = crate::Session::new();
            session.execute("library.importLightroom", &json!({"path": path.to_string_lossy()})).unwrap();
            assert_eq!(session.catalog.len(), 1);
            assert_eq!(session.catalog.photos().next().unwrap().rating, 5);
            session.execute("edit.undo", &json!({})).unwrap();
            assert!(session.catalog.is_empty());
        }
        std::fs::write(&path, b"bad catalog").unwrap();
        assert!(read(&path).is_err());
        std::fs::remove_file(path).unwrap();
    }

    /// Reads `tables` as a catalog file and returns what the reader made of it.
    fn read_tables(tables: &[(&str, &str, Vec<Vec<SqlValue>>)], name: &str) -> CatalogImport {
        let path = std::env::temp_dir().join(format!("lightcraft-{name}-{}.lrcat", std::process::id()));
        std::fs::write(&path, native_catalog_rows(tables, None)).unwrap();
        let catalog = read(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        catalog
    }

    /// `tables` with each row as a one-row table, for [`native_catalog_rows`].
    fn one_row(tables: Vec<(&'static str, &'static str, Vec<SqlValue>)>) -> Vec<(&'static str, &'static str, Vec<Vec<SqlValue>>)> {
        tables.into_iter().map(|(name, columns, row)| (name, columns, vec![row])).collect()
    }

    #[test]
    fn images_with_incomplete_records_are_skipped_not_fatal() {
        use SqlValue::{Integer as I, Text as T};
        // (table, column, value): the record that breaks, and the reason the import gives
        let cases = [
            (3, 2, I(2), SkipReason::MissingFile),                // Adobe_images.rootFile → no such file
            (2, 1, I(2), SkipReason::MissingFolder),              // AgLibraryFile.folder → no such folder
            (1, 1, I(2), SkipReason::MissingRoot),                // AgLibraryFolder.rootFolder → no such root
            (2, 2, T(String::new()), SkipReason::IncompletePath), // AgLibraryFile.idx_filename empty
        ];
        for (table, column, value, reason) in cases {
            let mut tables = one_row(photo_tables(("/catalog/", "")));
            tables[table].2[0][column] = value;
            let catalog = read_tables(&tables, &format!("skipped-{reason:?}"));
            assert!(catalog.photos.is_empty(), "{reason:?}");
            assert_eq!(catalog.skipped, [SkippedImage { image: 1, reason }]);
            assert!(catalog.warnings.is_empty(), "{reason:?}: {:?}", catalog.warnings);
        }
    }

    #[test]
    fn a_virtual_copy_of_a_skipped_master_is_skipped_too() {
        use SqlValue::{Integer as I, Null as N, Text as T};
        let mut tables = one_row(photo_tables(("/catalog/", "")));
        tables[3] = (
            "Adobe_images",
            "id_local INTEGER PRIMARY KEY, id_global, rootFile, masterImage, fileFormat, fileWidth, fileHeight, rating, pick",
            vec![
                // the master's file record is gone, so it is skipped …
                vec![N, T("master".into()), I(2), N, T("ARW".into()), I(20), I(10), I(5), I(1)],
                // … and its virtual copy goes with it, although its own file record is there
                vec![N, T("copy".into()), I(1), I(1), T("ARW".into()), I(20), I(10), I(3), I(0)],
            ],
        );
        let catalog = read_tables(&tables, "skipped-copy");
        assert!(catalog.photos.is_empty());
        assert_eq!(
            catalog.skipped,
            [SkippedImage { image: 1, reason: SkipReason::MissingFile }, SkippedImage { image: 2, reason: SkipReason::MasterSkipped }]
        );
        validate(&catalog).unwrap();
    }

    #[test]
    fn skipped_images_are_in_the_import_report() {
        let mut data = sample();
        data.photos.retain(|p| p.source_id != 1);
        drop_copies_of(&mut data, &HashSet::from([1]));
        data.skipped.insert(0, SkippedImage { image: 1, reason: SkipReason::MissingFile });
        let mut s = crate::Session::new();
        let report = apply(&mut s, data, false).unwrap();
        assert_eq!(report["photos"], 0);
        assert_eq!(report["skipped"], json!([{"image": 1, "reason": "missingFile"}, {"image": 2, "reason": "masterSkipped"}]));
        assert_eq!(report["warnings"], json!([]));
    }

    #[test]
    fn preparation_skips_missing_original_probes_but_keeps_catalog_records() {
        let dir = std::env::temp_dir().join(format!("lightcraft-lrcat-offline-probes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.lrcat");
        let root = dir.join("offline");
        let tables = photo_tables((root.to_str().unwrap(), ""));
        std::fs::write(&source, native_catalog(&tables)).unwrap();
        let mut s = crate::Session::new();
        let probes = Arc::new(AtomicUsize::new(0));
        let calls = probes.clone();
        s.media.file_probe = Some(Arc::new(move |_| {
            calls.fetch_add(1, Ordering::Relaxed);
            Err("a known-missing original must not be probed".into())
        }));
        let mut job = crate::lightroom_job::LightroomJob::new(&mut s, source, false).unwrap();
        let prepared = job.prepare(&AtomicBool::new(false)).unwrap();
        assert_eq!(probes.load(Ordering::Relaxed), 0);
        let result = crate::lightroom_job::commit_prepared(&mut s, prepared).unwrap();
        assert_eq!(result.report["imported"], 1);
        assert_eq!(result.report["failed"], json!([]));
        assert_eq!(result.report["missing"].as_array().unwrap().len(), 1);
        let photo = s.catalog.photos().next().unwrap();
        assert_eq!(photo.rating, 5);
        assert_eq!((photo.width, photo.height), (20, 10));
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(s.catalog.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn probed_original_is_counted_once_and_reimport_adds_nothing() {
        let dir = std::env::temp_dir().join(format!("lightcraft-lrcat-present-probes-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Photos")).unwrap();
        let source = dir.join("source.lrcat");
        std::fs::write(dir.join("Photos/test.ARW"), b"original").unwrap();
        std::fs::write(&source, native_catalog(&photo_tables((dir.to_str().unwrap(), "")))).unwrap();
        let mut s = crate::Session::new();
        let probes = Arc::new(AtomicUsize::new(0));
        let calls = probes.clone();
        s.media.file_probe = Some(Arc::new(move |_| {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(crate::media::ProbeInfo { format: "ARW".into(), width: 20, height: 10, ..Default::default() })
        }));
        for expected in [1, 0] {
            let mut job = crate::lightroom_job::LightroomJob::new(&mut s, source.clone(), false).unwrap();
            let prepared = job.prepare(&AtomicBool::new(false)).unwrap();
            let completion = crate::lightroom_job::commit_prepared(&mut s, prepared).unwrap();
            assert_eq!(completion.report["imported"], expected);
            assert_eq!(completion.report["missing"], json!([]));
            assert_eq!(completion.report["failed"], json!([]));
            assert_eq!(s.catalog.len(), 1);
        }
        assert_eq!(probes.load(Ordering::Relaxed), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_root_missing_here_is_found_relative_to_the_catalog() {
        let dir = std::env::temp_dir().join(format!("lightcraft-relocated-catalog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Lightroom")).unwrap();
        std::fs::create_dir_all(dir.join("RAW")).unwrap();
        let path = dir.join("Lightroom").join("Lightroom.lrcat");
        std::fs::write(&path, native_catalog(&photo_tables(("C:/Users/me/Project/RAW/", "../RAW/")))).unwrap();
        let catalog = read(&path).unwrap();
        assert_eq!(Path::new(&catalog.photos[0].path), dir.join("RAW").join("Photos").join("test.ARW"));
        assert!(catalog.warnings.iter().any(|w| w.contains("relative to the catalog")), "{:?}", catalog.warnings);
        // a relative path that leads nowhere keeps the recorded one
        std::fs::write(&path, native_catalog(&photo_tables(("C:/Users/me/Project/RAW/", "../Elsewhere/")))).unwrap();
        let catalog = read(&path).unwrap();
        assert!(Path::new(&catalog.photos[0].path).starts_with("C:/Users/me/Project/RAW/"), "{}", catalog.photos[0].path);
        assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
        std::fs::remove_dir_all(dir).unwrap();
    }

    const DEVELOP_COLUMNS: &str = "id_local INTEGER PRIMARY KEY, image, text, digest";

    /// Issue #1234: a develop-settings table that exists but can't be read must not look like a
    /// clean import. The photo still arrives, counted in `editsNotImported` with a leading warning.
    #[test]
    fn unreadable_develop_settings_are_reported_not_hidden() {
        use SqlValue::{Integer as I, Null as N, Text as T};
        let mut tables = photo_tables(("/catalog/", ""));
        tables.push(("Adobe_imageDevelopSettings", DEVELOP_COLUMNS, vec![N, I(1), T("s = { Exposure2012 = 1.5 }".into()), N]));
        let dir = std::env::temp_dir().join(format!("lightcraft-develop-unreadable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("catalog.lrcat");
        let good = native_catalog(&tables);
        std::fs::write(&path, &good).unwrap();
        let catalog = read(&path).unwrap();
        assert_eq!((catalog.edits_not_imported, catalog.photos[0].settings.as_str()), (0, "s = { Exposure2012 = 1.5 }"));
        let mut session = crate::Session::new();
        let report = session.execute("library.importLightroom", &json!({"path": path.to_string_lossy()})).unwrap();
        assert_eq!(report["editsNotImported"], 0);
        assert_eq!(session.catalog.photos().next().unwrap().develop.light.exposure, 1.5);
        // the develop table's page (the fifth table, page 6) is no B-tree page any more
        let mut bad = good;
        bad[5 * 2048] = 0x07;
        std::fs::write(&path, &bad).unwrap();
        let catalog = read(&path).unwrap();
        assert_eq!(catalog.edits_not_imported, 1);
        assert!(catalog.warnings[0].starts_with("Lightroom edits were not imported for 1 of 1 photos"), "{:?}", catalog.warnings);
        let mut session = crate::Session::new();
        let report = session.execute("library.importLightroom", &json!({"path": path.to_string_lossy()})).unwrap();
        assert_eq!(report["editsNotImported"], 1);
        assert_eq!(session.catalog.len(), 1);
        assert_eq!(session.catalog.photos().next().unwrap().rating, 5);
        let inspected = session.execute("library.inspectLightroom", &json!({"path": path.to_string_lossy()})).unwrap();
        assert_eq!(inspected["editsNotImported"], 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Issue #1234: a develop-settings table past the old 256 MiB per-table cap still imports its
    /// edits. Five 60 MiB rows (a big zero `digest` blob each, stored in sparse overflow pages)
    /// make a 300 MiB table in a file that takes about 20 MiB of disk.
    #[cfg(unix)]
    #[test]
    fn develop_settings_past_the_old_table_cap_import_their_edits() {
        use SqlValue::{Integer as I, Null as N, Text as T};
        use std::io::{Seek, SeekFrom, Write};
        const PAGE: usize = 65536;
        const ROWS: usize = 5;
        // SQLite file format §1.6: a payload of P bytes keeps K = M + (P − M) mod (U − 4) bytes in its
        // cell (M = (U − 12) × 32 / 255 − 23); choose P so K = M and the rows share one leaf page.
        let min_local = (PAGE - 12) * 32 / 255 - 23;
        let per_page = PAGE - 4;
        let overflow_pages = 960;
        let payload_len = min_local + overflow_pages * per_page;
        let settings = "s = { Exposure2012 = 1.25 }";
        // record: header, then id (NULL alias), image (8-byte int), text, then the zero blob
        let prefix = |image: i64, blob: usize| {
            let mut header = vec![0];
            header.push(6);
            header.extend(varint(13 + 2 * settings.len() as u64));
            header.extend(varint(12 + 2 * blob as u64));
            let mut out = varint(1 + header.len() as u64);
            out.extend(header);
            out.extend(image.to_be_bytes());
            out.extend(settings.as_bytes());
            out
        };
        let blob = payload_len - prefix(0, payload_len).len();
        assert_eq!(prefix(0, blob).len() + blob, payload_len);
        assert!(payload_len * ROWS > 256 << 20 && payload_len <= 64 << 20);

        // pages 1–5 are the schema and the four photo tables; 6 is the develop leaf
        let mut tables = photo_tables(("/catalog/", ""));
        tables.push(("Adobe_imageDevelopSettings", DEVELOP_COLUMNS, vec![N, I(1), T(String::new()), N]));
        let mut file = catalog_pages(&tables, PAGE, None);
        file[5 * PAGE..].fill(0);
        // the develop leaf: ROWS cells, the last for image 1 (the catalog's photo) so that it lies
        // past the old cap, each with its own overflow chain after page 6
        let leaf = 5 * PAGE;
        file[leaf] = 0x0d;
        file[leaf + 3..leaf + 5].copy_from_slice(&(ROWS as u16).to_be_bytes());
        let mut end = PAGE;
        let mut chains = Vec::new();
        for row in 0..ROWS {
            let first = 7 + row * overflow_pages;
            let mut record = prefix((ROWS - row) as i64, blob);
            record.resize(min_local, 0);
            let mut cell = varint(payload_len as u64);
            cell.extend(varint(row as u64 + 1));
            cell.extend(record);
            cell.extend((first as u32).to_be_bytes());
            end -= cell.len();
            file[leaf + end..leaf + end + cell.len()].copy_from_slice(&cell);
            file[leaf + 8 + 2 * row..leaf + 10 + 2 * row].copy_from_slice(&(end as u16).to_be_bytes());
            chains.push(first);
        }
        let pages = 6 + ROWS * overflow_pages;

        let dir = std::env::temp_dir().join(format!("lightcraft-develop-large-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("catalog.lrcat");
        let mut out = std::fs::File::create(&path).unwrap();
        out.write_all(&file).unwrap();
        out.set_len((pages * PAGE) as u64).unwrap();
        // only the overflow pages' next-page numbers are written; their payload stays a hole
        for first in chains {
            for page in first..first + overflow_pages - 1 {
                out.seek(SeekFrom::Start(((page - 1) * PAGE) as u64)).unwrap();
                out.write_all(&(page as u32 + 1).to_be_bytes()).unwrap();
            }
        }
        drop(out);
        let catalog = read(&path).unwrap();
        assert_eq!(catalog.edits_not_imported, 0);
        assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
        assert_eq!(catalog.photos[0].settings, settings);
        let mut session = crate::Session::new();
        let report = session.execute("library.importLightroom", &json!({"path": path.to_string_lossy()})).unwrap();
        assert_eq!(report["editsNotImported"], 0);
        assert_eq!(session.catalog.photos().next().unwrap().develop.light.exposure, 1.25);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// History and snapshots are streamed and kept up to a budget; past it they are counted, not
    /// held, and the import goes on.
    #[test]
    fn history_past_its_budget_is_counted_not_kept() {
        use SqlValue::{Integer as I, Null as N, Text as T};
        let mut tables = photo_tables(("/catalog/", ""));
        tables.push((
            "Adobe_libraryImageDevelopHistoryStep",
            "id_local INTEGER PRIMARY KEY, image, name, text",
            vec![N, I(1), T("Crop".into()), T("s = {}".into())],
        ));
        let path = std::env::temp_dir().join(format!("lightcraft-history-budget-{}.lrcat", std::process::id()));
        std::fs::write(&path, native_catalog(&tables)).unwrap();
        let catalog = read(&path).unwrap();
        assert_eq!(catalog.photos[0].history.len(), 1);
        assert_eq!(text(&catalog.photos[0].history[0], "name"), "Crop");
        let main = FileSource::open(&path).unwrap();
        let db = Database::open_file_with_wal(main, &[]).unwrap();
        let cancel = AtomicBool::new(false);
        let reader = Reader { tables: db.live_tables().unwrap(), db, cancel: &cancel };
        let mut warnings = Vec::new();
        let mut budget = 10;
        let kept = reader.grouped_rows("Adobe_libraryImageDevelopHistoryStep", &mut warnings, &mut budget).unwrap();
        assert!(kept.is_empty());
        assert!(warnings[0].contains("1 more are not archived"), "{warnings:?}");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn originals_on_other_computers_are_never_looked_at_or_added() {
        let dir = std::env::temp_dir().join(format!("lightcraft-lrcat-remote-roots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Photos")).unwrap();
        std::fs::write(dir.join("Photos/test.ARW"), b"original").unwrap();
        let source = dir.join("source.lrcat");
        let mut roots = vec![r"\\192.0.2.1\share\".to_string(), "file://192.0.2.1/s/".to_string()];
        if cfg!(unix) {
            // `//<local folder>/` is a local folder to Linux, but a network path to the resolver.
            roots.push(format!("/{}/", dir.display()));
        }
        for root in &roots {
            std::fs::write(&source, native_catalog(&photo_tables((root, "")))).unwrap();
            let catalog = read(&source).unwrap();
            assert!(matches!(catalog.photos[0].original, Original::Refused(_)), "{root}: {:?}", catalog.photos[0]);
            assert!(catalog.warnings.iter().any(|w| w.contains("not opened")), "{root}: {:?}", catalog.warnings);
            let mut s = crate::Session::new();
            let probes = Arc::new(AtomicUsize::new(0));
            let calls = probes.clone();
            s.media.file_probe = Some(Arc::new(move |_| {
                calls.fetch_add(1, Ordering::Relaxed);
                Err("a refused original must not be probed".into())
            }));
            let report = import(&mut s, &source, false).unwrap();
            assert_eq!(probes.load(Ordering::Relaxed), 0, "{root}");
            assert_eq!(report["missing"].as_array().map(Vec::len), Some(1), "{root}: {report}");
            assert!(s.catalog.is_empty(), "{root}");
        }
        // An absolute folder path would replace the root.
        let mut tables = photo_tables((dir.to_str().unwrap(), ""));
        tables[1].2[2] = SqlValue::Text(format!("{}/Photos/", dir.display()));
        std::fs::write(&source, native_catalog(&tables)).unwrap();
        assert!(matches!(read(&source).unwrap().photos[0].original, Original::Refused(_)));
        // A root beside the catalog is followed.
        std::fs::write(&source, native_catalog(&photo_tables((dir.to_str().unwrap(), "")))).unwrap();
        let catalog = read(&source).unwrap();
        assert_eq!(catalog.photos[0].original, Original::Local);
        assert!(catalog.photos[0].original_is_file());
        // A root that moved is found through its path relative to the catalog.
        std::fs::write(&source, native_catalog(&photo_tables((r"\\192.0.2.1\share\", "./")))).unwrap();
        let catalog = read(&source).unwrap();
        assert!(catalog.photos[0].original_is_file(), "{:?}", catalog.photos[0]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn oversized_optional_table_warns_and_is_skipped() {
        let mut main = vec![0u8; 2 * 512];
        main[..16].copy_from_slice(b"SQLite format 3\0");
        main[16..18].copy_from_slice(&512u16.to_be_bytes());
        main[18] = 1;
        main[19] = 1;
        main[21] = 64;
        main[22] = 32;
        main[23] = 32;
        main[28..32].copy_from_slice(&2u32.to_be_bytes());
        main[44..48].copy_from_slice(&4u32.to_be_bytes());
        main[56..60].copy_from_slice(&1u32.to_be_bytes());
        main[100] = 0x0d;
        let page = &mut main[512..];
        page[0] = 0x0d;
        page[3..5].copy_from_slice(&1u16.to_be_bytes());
        let mut cell = varint((64 * 1024 * 1024 + 1) as u64);
        cell.extend(varint(1));
        let start = 500 - cell.len();
        page[start..start + cell.len()].copy_from_slice(&cell);
        page[8..10].copy_from_slice(&u16::try_from(start).unwrap_or(0).to_be_bytes());
        page[5..7].copy_from_slice(&u16::try_from(start).unwrap_or(0).to_be_bytes());
        let db = Database::open_with_wal(main, &[]).unwrap();
        let cancel = AtomicBool::new(false);
        let reader = Reader {
            db,
            tables: vec![LiveTable { name: "optional_history".into(), rootpage: 2, column_names: Some(vec!["payload".into()]) }],
            cancel: &cancel,
        };
        let mut warnings = Vec::new();
        let rows = reader.optional_rows("optional_history", &mut warnings).unwrap();
        assert!(rows.is_empty());
        assert!(warnings.iter().any(|warning| warning.contains("optional_history")));
    }
}
