//! Direct, read-only Lightroom Classic catalog import. SQLite pages are read in Rust;
//! Lightroom, Python and a C SQLite runtime are not needed. Original paths stay in place.
//! Raw settings are archived before mutation; mapped edits are approximations, not Adobe renders.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use crate::lightroom_sqlite::{Database, FileSource, LiveTable, Value as SqlValue};
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

struct Reader {
    db: Database,
    tables: Vec<LiveTable>,
}
impl Reader {
    fn table(&self, name: &str, required: bool) -> Result<Vec<Row>, String> {
        let Some(t) = self.tables.iter().find(|t| t.name == name) else {
            return if required { Err(format!("unsupported Lightroom catalog: missing {name}")) } else { Ok(Vec::new()) };
        };
        let columns = t.column_names.as_ref().ok_or_else(|| format!("cannot read {name} columns"))?;
        let rows = self.db.read_table(t.rootpage, columns.len()).map_err(|e| format!("{name}: {e:?}"))?;
        if rows.len() > 1_000_000 {
            return Err(format!("{name}: more than one million records"));
        }
        Ok(rows
            .into_iter()
            .map(|r| {
                columns
                    .iter()
                    .cloned()
                    .zip(r.values.into_iter().map(|v| match v {
                        SqlValue::Null => Some(Value::Null),
                        SqlValue::Integer(n) => Some(json!(n)),
                        SqlValue::Real(n) => Some(json!(n)),
                        SqlValue::Text(s) => Some(json!(s)),
                        // Opaque Lightroom payloads never become JSON byte arrays.
                        SqlValue::Blob(_) => None,
                    }))
                    .filter_map(|(column, value)| value.map(|value| (column, value)))
                    .collect()
            })
            .collect())
    }

    fn optional_table(&self, name: &str, warnings: &mut Vec<String>) -> Result<Vec<Row>, String> {
        match self.table(name, false) {
            Ok(rows) => Ok(rows),
            Err(error) => {
                warnings.push(format!("skipping optional Lightroom table {name}: {error}"));
                Ok(Vec::new())
            }
        }
    }

    fn xmp_table(&self) -> Result<(Vec<Row>, Vec<String>), String> {
        let Some(t) = self.tables.iter().find(|t| t.name == "Adobe_AdditionalMetadata") else { return Ok((Vec::new(), Vec::new())) };
        let columns = t.column_names.as_ref().ok_or_else(|| "cannot read Adobe_AdditionalMetadata columns".to_string())?;
        let rows = self.db.read_table(t.rootpage, columns.len()).map_err(|e| format!("Adobe_AdditionalMetadata: {e:?}"))?;
        if rows.len() > 1_000_000 {
            return Err("Adobe_AdditionalMetadata: more than one million records".into());
        }
        let mut warnings = Vec::new();
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let image = row
                .values
                .iter()
                .zip(columns)
                .find_map(|(value, column)| {
                    (column == "image").then_some(match value {
                        SqlValue::Integer(n) => *n,
                        _ => 0,
                    })
                })
                .unwrap_or(0);
            let mut mapped = Row::new();
            for (column, value) in columns.iter().zip(row.values) {
                let value = match value {
                    SqlValue::Null => Some(Value::Null),
                    SqlValue::Integer(n) => Some(json!(n)),
                    SqlValue::Real(n) => Some(json!(n)),
                    SqlValue::Text(s) => Some(json!(s)),
                    SqlValue::Blob(bytes) if column == "xmp" => match decode_xmp_bytes(&bytes) {
                        Ok(packet) => Some(Value::String(packet)),
                        Err(error) => {
                            warnings.push(format!("image {image}: {error}"));
                            None
                        }
                    },
                    SqlValue::Blob(_) => None,
                };
                if let Some(value) = value {
                    mapped.insert(column.clone(), value);
                }
            }
            out.push(mapped);
        }
        Ok((out, warnings))
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

fn xmp(v: Option<&Value>) -> Result<String, String> {
    match v {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(Value::Array(a)) if !a.is_empty() => {
            let b: Vec<u8> = a.iter().filter_map(|n| n.as_u64().and_then(|n| u8::try_from(n).ok())).collect();
            let size = b.get(..4).and_then(|s| <[u8; 4]>::try_from(s).ok()).map(u32::from_be_bytes).ok_or("truncated XMP header")?;
            if size > 16 << 20 {
                return Err("XMP packet exceeds 16 MiB".into());
            }
            let compressed = b.get(4..).ok_or("truncated XMP")?;
            let decoded = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(compressed, 16 << 20).map_err(|_| "invalid compressed XMP")?;
            if decoded.len() != size as usize {
                return Err("XMP packet length mismatch".into());
            }
            String::from_utf8(decoded).map_err(|e| e.to_string())
        }
        _ => Ok(String::new()),
    }
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
    let reader = Reader { tables: db.live_tables()?, db };
    let mut warnings = Vec::new();
    let table = |name: &str, required: bool| -> Result<Vec<Row>, String> {
        ensure_read_active(cancel)?;
        let rows = reader.table(name, required)?;
        done.fetch_add(1, Ordering::Relaxed);
        Ok(rows)
    };
    let optional_table = |name: &str, warnings: &mut Vec<String>| -> Result<Vec<Row>, String> {
        ensure_read_active(cancel)?;
        let rows = reader.optional_table(name, warnings)?;
        done.fetch_add(1, Ordering::Relaxed);
        Ok(rows)
    };
    let roots: HashMap<_, _> = table("AgLibraryRootFolder", true)?.into_iter().map(|r| (number(&r, "id_local"), r)).collect();
    let folders: HashMap<_, _> = table("AgLibraryFolder", true)?.into_iter().map(|r| (number(&r, "id_local"), r)).collect();
    let files: HashMap<_, _> = table("AgLibraryFile", true)?.into_iter().map(|r| (number(&r, "id_local"), r)).collect();
    let develops: HashMap<_, _> = optional_table("Adobe_imageDevelopSettings", &mut warnings)?
        .into_iter()
        .map(|r| (number(&r, "image"), text(&r, "text").to_string()))
        .collect();
    ensure_read_active(cancel)?;
    let (metadata_rows, metadata_warnings) = match reader.xmp_table() {
        Ok(result) => result,
        Err(error) => {
            warnings.push(format!("skipping optional Lightroom table Adobe_AdditionalMetadata: {error}"));
            (Vec::new(), Vec::new())
        }
    };
    warnings.extend(metadata_warnings);
    done.fetch_add(1, Ordering::Relaxed);
    let metadata: HashMap<_, _> = metadata_rows.into_iter().map(|r| (number(&r, "image"), r)).collect();
    let keyword_rows: HashMap<_, _> = optional_table("AgLibraryKeyword", &mut warnings)?.into_iter().map(|r| (number(&r, "id_local"), r)).collect();
    let mut keywords: HashMap<i64, Vec<String>> = HashMap::new();
    for r in optional_table("AgLibraryKeywordImage", &mut warnings)? {
        ensure_read_active(cancel)?;
        let mut parts = Vec::new();
        let mut at = number(&r, "tag");
        let mut seen = HashSet::new();
        while at != 0 {
            if parts.len() >= 64 || !seen.insert(at) {
                return Err("cyclic or overly deep keyword hierarchy".into());
            }
            let Some(k) = keyword_rows.get(&at) else { break };
            let name = text(k, "name");
            if !name.is_empty() {
                parts.push(name.to_string());
            }
            at = number(k, "parent");
        }
        parts.reverse();
        if !parts.is_empty() {
            keywords.entry(number(&r, "image")).or_default().push(parts.join("|"));
        }
    }
    let group = |rows: Vec<Row>| {
        let mut out: HashMap<i64, Vec<Row>> = HashMap::new();
        for r in rows {
            out.entry(number(&r, "image")).or_default().push(r);
        }
        out
    };
    let mut root_paths = HashMap::new();
    let mut refused_roots = HashSet::new();
    let mut root_ids: Vec<_> = roots.keys().copied().collect();
    root_ids.sort_unstable();
    for (id, root) in root_ids.iter().filter_map(|id| Some((*id, roots.get(id)?))) {
        let absolute = text(root, "absolutePath");
        let resolved = match relocated_root(absolute, text(root, "relativePathFromCatalog"), path) {
            Some(found) => {
                let found = found.to_string_lossy().to_string();
                warnings.push(format!("root folder {}: {absolute} not found; using {found} (relative to the catalog)", text(root, "name")));
                found
            }
            None => absolute.to_string(),
        };
        if let Err(crate::doc_path::NotFollowed::Refused(why)) = crate::doc_path::resolve(&resolved, Some(path)) {
            warnings
                .push(format!("root folder {}: {resolved} is not opened: {why}; its photos are listed as missing and not added", text(root, "name")));
            refused_roots.insert(id);
        }
        root_paths.insert(id, resolved);
    }
    let mut history = group(optional_table("Adobe_libraryImageDevelopHistoryStep", &mut warnings)?);
    let mut snapshots = group(optional_table("Adobe_libraryImageDevelopSnapshot", &mut warnings)?);
    let mut out = CatalogImport {
        source: path.to_string_lossy().into(),
        photos: Vec::new(),
        collections: optional_table("AgLibraryCollection", &mut warnings)?,
        members: optional_table("AgLibraryCollectionImage", &mut warnings)?,
        collection_content: optional_table("AgLibraryCollectionContent", &mut warnings)?,
        warnings,
        skipped: Vec::new(),
    };
    // images whose file, folder or root record is gone, or whose path is empty (Lightroom leaves
    // these behind and ignores them); they and their virtual copies are skipped and reported
    // instead of failing the import
    let mut orphans = HashSet::new();
    for image in table("Adobe_images", true)? {
        ensure_read_active(cancel)?;
        let id = number(&image, "id_local");
        let record = files.get(&number(&image, "rootFile")).ok_or(SkipReason::MissingFile).and_then(|file| {
            let folder = folders.get(&number(file, "folder")).ok_or(SkipReason::MissingFolder)?;
            let root = roots.get(&number(folder, "rootFolder")).ok_or(SkipReason::MissingRoot)?;
            Ok((file, folder, root))
        });
        let (file, folder, root) = match record {
            Ok(found) => found,
            Err(reason) => {
                out.skipped.push(SkippedImage { image: id, reason });
                orphans.insert(id);
                continue;
            }
        };
        let root_id = number(root, "id_local");
        let root_path = root_paths.get(&root_id).map_or("", String::as_str);
        let folder_path = text(folder, "pathFromRoot");
        let name = text(file, "idx_filename");
        if root_path.is_empty() || name.is_empty() {
            out.skipped.push(SkippedImage { image: id, reason: SkipReason::IncompletePath });
            orphans.insert(id);
            continue;
        }
        let (photo_path, original) = original_path(root_path, folder_path, name, path);
        if let Original::Refused(why) = &original
            && !refused_roots.contains(&root_id)
        {
            out.warnings.push(format!("image {id}: {photo_path} is not opened: {why}; listed as missing and not added"));
        }
        let packet = match xmp(metadata.get(&id).and_then(|r| r.get("xmp"))) {
            Ok(s) => s,
            Err(e) => {
                out.warnings.push(format!("image {id}: {e}"));
                String::new()
            }
        };
        out.photos.push(CatalogPhoto {
            source_id: id,
            uuid: text(&image, "id_global").into(),
            path: photo_path,
            image,
            settings: develops.get(&id).cloned().unwrap_or_default(),
            xmp: packet,
            keywords: keywords.remove(&id).unwrap_or_default(),
            history: history.remove(&id).unwrap_or_default(),
            snapshots: snapshots.remove(&id).unwrap_or_default(),
            original,
        });
    }
    drop_copies_of(&mut out, &orphans);
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
        report: json!({"source":data.source,"photos":ids.len(),"imported":imported,"library":s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.to_string_lossy()),"collections":albums.len(),"preservedExistingEdits":preserved,"missing":missing,"failed":files.failed,"warnings":warnings,"skipped":data.skipped,"unmapped":unmapped,"archive":archive_path,"mapping":ids}),
        index: index.clone(),
    })
}

#[cfg(test)]
mod tests;
