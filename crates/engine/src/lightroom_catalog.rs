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

pub(crate) type Row = BTreeMap<String, Value>;
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
pub(crate) fn key(path: &str) -> String {
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

fn identity(source: &str, photo: &CatalogPhoto) -> String {
    if photo.uuid.is_empty() { format!("{}:{}", key(source), photo.source_id) } else { photo.uuid.clone() }
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

/// A video the catalog lists with the format `VIDEO` and a video extension. Videos aren't image
/// types, but they become video records (never opened by the system opener), so they pass.
fn is_catalog_video(p: &CatalogPhoto) -> bool {
    const EXTENSIONS: [&str; 12] = ["mp4", "mov", "m4v", "avi", "mts", "m2ts", "3gp", "mkv", "webm", "wmv", "mpg", "mpeg"];
    text(&p.image, "fileFormat").eq_ignore_ascii_case("VIDEO")
        && Path::new(&p.path).extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
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
        .filter(|p| number(&p.image, "masterImage") == 0 && !crate::open_path::is_media(Path::new(&p.path)) && !is_catalog_video(p))
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
    user: Orientation,
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
    // Mask edges are fractions of the stored image, like the crop's, so radii go to long-edge units
    // with its aspect: `aspect` is the photo's in LightCraft, which is the shown one for a photo
    // already in the library and only stands in when the catalog has no file size.
    let mask_aspect = if stored_aspect.is_finite() && stored_aspect > 0.0 { stored_aspect } else { aspect };
    let (mut partial, mut unknown) = crate::crs::to_partial_report(&props, Some(&values), target, mask_aspect);
    catalog_crop(&mut partial, orientation, stored_aspect);
    catalog_masks(&mut partial, in_photo_frame(orientation, user));
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

/// The catalog's orientation (the stored image to the photo as Lightroom shows it), then the
/// photo's own Rotate Left / Right and Flip in LightCraft (`user`), which an update of a photo
/// already in the library keeps: the frame LightCraft draws masks in.
fn in_photo_frame(catalog: Orientation, user: Orientation) -> Orientation {
    let (flip, turns) = user.to_parts();
    let o = if flip { catalog.flipped_h() } else { catalog };
    (0..turns).fold(o, |o, _| o.rotated(true))
}

/// Lightroom's masks in LightCraft's frame. Like the crop's corners, their positions are on the
/// stored image (before orientation), normalized to its size; LightCraft's are on the oriented
/// image. So the points go through the orientation, a radial's radii (in long-edge units, from the
/// stored image's aspect) swap when the axes do, and a mirror turns its angle the other way. A
/// quarter turn keeps the angle: the ellipse turned 90° is the same ellipse with its radii swapped.
fn catalog_masks(partial: &mut Value, orientation: Orientation) {
    if orientation == Orientation::Normal {
        return;
    }
    let point = |p: Option<&mut Value>| {
        let Some(p) = p.and_then(Value::as_object_mut) else { return };
        let (Some(x), Some(y)) = (p.get("x").and_then(Value::as_f64), p.get("y").and_then(Value::as_f64)) else { return };
        let (u, v) = orientation.map(x, y, 1.0, 1.0);
        p.insert("x".into(), json!(u));
        p.insert("y".into(), json!(v));
    };
    let Some(masks) = partial.get_mut("masks").and_then(Value::as_array_mut) else { return };
    let components = masks.iter_mut().filter_map(|m| m.get_mut("components").and_then(Value::as_array_mut)).flatten();
    for shape in components.filter_map(|c| c.get_mut("shape").and_then(Value::as_object_mut)) {
        point(shape.get_mut("start"));
        point(shape.get_mut("end"));
        point(shape.get_mut("center"));
        let strokes = shape.get_mut("strokes").and_then(Value::as_array_mut).into_iter().flatten();
        for points in strokes.filter_map(|s| s.get_mut("points").and_then(Value::as_array_mut)) {
            points.iter_mut().for_each(|p| point(Some(p)));
        }
        if shape.get("kind").and_then(Value::as_str) == Some("radial") {
            if orientation.swaps_axes()
                && let (Some(rx), Some(ry)) = (shape.get("rx").cloned(), shape.get("ry").cloned())
            {
                shape.insert("rx".into(), ry);
                shape.insert("ry".into(), rx);
            }
            if orientation.to_parts().0
                && let Some(angle) = shape.get("angle").and_then(Value::as_f64)
            {
                shape.insert("angle".into(), json!(-angle));
            }
        }
    }
}

/// The catalog's XMP masks in LightCraft's frame, as [`catalog_masks`] puts the develop row's: they
/// are on the same stored image, and they stay when the row has no masks of its own. The packet's
/// radials come fitted to the sidecar default aspect and `sidecar::merge_into` re-fits them to the
/// photo's size, so they are fitted to the stored image to be turned (a quarter turn swaps the
/// radii) and handed back in the default aspect, for that re-fit to undo.
fn catalog_xmp_masks(partial: &mut Value, orientation: Orientation, stored_aspect: f64, merged: f64) {
    use crate::crs_masks::{DEFAULT_ASPECT, refit_radials};
    let fit = if stored_aspect.is_finite() && stored_aspect > 0.0 { stored_aspect } else { merged };
    refit_radials(partial, DEFAULT_ASPECT, fit);
    catalog_masks(partial, orientation);
    refit_radials(partial, merged, DEFAULT_ASPECT);
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
/// Photos are added batch by batch ([`crate::lightroom_job`]); one undo step reverses the import.
pub fn import(s: &mut crate::Session, path: &Path, update_existing: bool) -> crate::Result<Value> {
    let mut job = crate::lightroom_job::LightroomJob::new(s, path.to_path_buf(), update_existing)?;
    let cancel = AtomicBool::new(false);
    let start = job.start(&cancel)?;
    let mut import = crate::lightroom_job::LightroomImport::begin(s, start)?;
    let mut added = || -> crate::Result<()> {
        while let Some(batch) = job.next_batch(&cancel) {
            let index = import.commit(s, batch?)?;
            // each batch is saved: a crash keeps what was added, and importing again continues
            // (with the index after a batch of virtual copies, which are found again through it)
            if s.library.as_ref().is_some_and(|library| library.on_disk) {
                s.persist()?;
                if let Some(index) = index {
                    index.finish()?;
                }
            }
        }
        Ok(())
    };
    if let Err(e) = added() {
        // what was added stays (one undo step), and so does its identity for a later import
        if let Ok(partial) = import.finish(s, false) {
            let _ = partial.finalization.finish();
        }
        return Err(e);
    }
    let completion = import.finish(s, true)?;
    if s.library.as_ref().is_some_and(|library| library.on_disk) {
        s.persist()?;
    }
    completion.finalization.finish()?;
    Ok(completion.report)
}

const RAW_FORMATS: [&str; 11] = ["RAW", "DNG", "ARW", "CR2", "CR3", "NEF", "NRW", "RAF", "ORF", "RW2", "PEF"];

/// The photo a catalog record becomes when its original couldn't be read (relinked later).
pub(crate) fn catalog_photo(src: &CatalogPhoto, id: PhotoId, now: &str) -> Photo {
    let name = Path::new(&src.path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| src.path.clone());
    let mut p = Photo::new(
        id,
        Source::File { path: src.path.clone() },
        &name,
        text(&src.image, "fileFormat"),
        number(&src.image, "fileWidth").max(0) as u32,
        number(&src.image, "fileHeight").max(0) as u32,
        now,
    );
    if RAW_FORMATS.contains(&p.format.to_uppercase().as_str()) {
        p.kind = lightcraft_catalog::MediaKind::Raw;
    }
    p
}

/// What mapping a catalog record's XMP and develop settings depends on: the photo's target, size
/// and its own Rotate / Flip in LightCraft (masks are turned into the frame it draws them in).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shape {
    target: crate::crs::Target,
    width: u32,
    height: u32,
    user: Orientation,
}

impl Shape {
    pub(crate) fn of(p: &Photo) -> Self {
        Shape { target: crate::crs::Target::for_photo(p), width: p.width, height: p.height, user: p.develop.orientation }
    }

    /// The aspect the develop settings are mapped with.
    fn aspect(self) -> f64 {
        self.width.max(1) as f64 / self.height.max(1) as f64
    }

    /// The aspect `sidecar::merge_into` re-fits the XMP packet's radials to.
    fn merged_aspect(self) -> f64 {
        if self.width > 0 && self.height > 0 { self.width as f64 / self.height as f64 } else { crate::crs_masks::DEFAULT_ASPECT }
    }
}

/// A catalog record with the work that needs no library done (on a worker): its XMP packet parsed
/// and its develop settings mapped, for the shape its photo is expected to have. The owner thread
/// maps again only when the photo turns out different.
pub(crate) struct MappedPhoto {
    pub(crate) src: CatalogPhoto,
    sidecar: Option<(Shape, std::result::Result<crate::sidecar::SidecarData, String>)>,
    settings: Option<(Shape, std::result::Result<(Value, Vec<String>), String>)>,
}

impl MappedPhoto {
    pub(crate) fn new(src: CatalogPhoto, shape: Shape) -> Self {
        let mut m = MappedPhoto { src, sidecar: None, settings: None };
        m.sidecar = m.parse_sidecar(shape).map(|r| (shape, r));
        m.settings = m.map_settings(shape).map(|r| (shape, r));
        m
    }

    /// The catalog's orientation and the stored image's aspect (the frame its masks are in).
    fn stored_frame(&self) -> (Orientation, f64) {
        let orientation = lightroom_orientation(text(&self.src.image, "orientation")).unwrap_or_default();
        (orientation, number(&self.src.image, "fileWidth") as f64 / number(&self.src.image, "fileHeight") as f64)
    }

    fn parse_sidecar(&self, shape: Shape) -> Option<std::result::Result<crate::sidecar::SidecarData, String>> {
        let (orientation, stored_aspect) = self.stored_frame();
        (!self.src.xmp.is_empty()).then(|| {
            crate::sidecar::parse_sidecar(&self.src.xmp, shape.target).map(|mut sc| {
                if let Some(crate::sidecar::DevelopPatch::Partial(partial)) = &mut sc.develop {
                    strip_xmp_sentinels(partial);
                    catalog_xmp_masks(partial, in_photo_frame(orientation, shape.user), stored_aspect, shape.merged_aspect());
                }
                sc
            })
        })
    }

    fn map_settings(&self, shape: Shape) -> Option<std::result::Result<(Value, Vec<String>), String>> {
        let (orientation, stored_aspect) = self.stored_frame();
        (!self.src.settings.is_empty())
            .then(|| mapped_settings(&self.src.settings, shape.target, shape.aspect(), orientation, stored_aspect, shape.user))
    }

    /// The parsed XMP packet, for `shape`.
    fn sidecar_for(&mut self, shape: Shape) -> Option<std::result::Result<crate::sidecar::SidecarData, String>> {
        match self.sidecar.take() {
            Some((s, r)) if s == shape => Some(r),
            _ => self.parse_sidecar(shape),
        }
    }

    /// The mapped develop settings, for `shape`.
    fn settings_for(&mut self, shape: Shape) -> Option<std::result::Result<(Value, Vec<String>), String>> {
        match self.settings.take() {
            Some((s, r)) if s == shape => Some(r),
            _ => self.map_settings(shape),
        }
    }
}

/// The new record for a catalog photo whose file has the same bytes as the library photo
/// `duplicate`: catalogs can contain distinct originals with identical bytes and independent edits.
/// It takes only what the bytes determine from that photo, never that photo's own state: not its
/// Recently Deleted (#2589), virtual copy, edits, history, versions, ratings, capture time or the
/// metadata a person writes. Its metadata and capture time are what its own file gives it (`file`),
/// as a file import reads them (copyright, creator…); the catalog's capture time, when it has one,
/// applies on top.
fn content_duplicate(s: &mut crate::Session, path: &str, duplicate: &Photo, file: Option<&crate::import::FileFacts>, now: &str) -> Photo {
    let name = Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
    let mut p = Photo::new(
        s.catalog.alloc_photo_id(),
        Source::File { path: path.to_string() },
        &name,
        &duplicate.format,
        duplicate.width,
        duplicate.height,
        now,
    );
    p.kind = duplicate.kind;
    p.file_size = duplicate.file_size;
    p.duration = duplicate.duration;
    p.as_shot_wb = duplicate.as_shot_wb;
    p.content_hash = duplicate.content_hash.clone();
    p.embedded_lens = duplicate.embedded_lens;
    p.preview_only = duplicate.preview_only.clone();
    p.preview_checked = duplicate.preview_checked.clone();
    if let Some(file) = file {
        p.meta = file.meta.clone();
        p.captured = file.captured.clone();
    } else {
        // (the file wasn't read: only the exposure description, which no command edits; not GPS,
        // which placing a photo on the map does, nor the capture time, which Edit Capture Time does)
        let m = &duplicate.meta;
        p.meta.camera = m.camera.clone();
        p.meta.lens = m.lens.clone();
        p.meta.focal_mm = m.focal_mm;
        p.meta.aperture = m.aperture;
        p.meta.shutter = m.shutter.clone();
        p.meta.iso = m.iso;
        p.meta.focus = m.focus.clone();
    }
    // the settings a file import gives it (as-shot white balance, embedded lens corrections, the
    // default preset); the catalog's settings apply on top
    crate::import::apply_import_defaults(s, &mut p);
    p
}

/// Adding a Lightroom import to the library on the owner thread: batch by batch
/// ([`LightroomApply::batch`]), then its collections ([`LightroomApply::finish`]). No filesystem work.
pub(crate) struct LightroomApply {
    source: String,
    update_existing: bool,
    pub(crate) index: ImportIndex,
    now: String,
    /// Source photo id → its photo in the library.
    ids: HashMap<i64, PhotoId>,
    /// Photos in the library (and added so far) by path.
    by_path: HashMap<String, PhotoId>,
    warnings: Vec<String>,
    unmapped: BTreeMap<i64, Vec<String>>,
    missing: Vec<String>,
    preserved: usize,
    /// Photos added: the originals the file import added, offline records, virtual copies and
    /// Local records that joined the library.
    imported: usize,
    /// Local records this import brought into the library.
    promoted: HashSet<PhotoId>,
    /// Source photos whose original path is refused ([`Original::Refused`]): left out, with their
    /// virtual copies.
    refused: HashSet<i64>,
    failed: Vec<(String, String)>,
    /// The catalog's images the reader skipped (incomplete records), for the report.
    pub(crate) skipped: Vec<SkippedImage>,
    /// Library photos in Recently Deleted that a catalog photo matched: they stay there (only an
    /// import of the files themselves brings a photo back, see `OnDeleted`), and the report says so.
    in_trash: HashSet<PhotoId>,
}

impl LightroomApply {
    pub(crate) fn new(s: &crate::Session, source: String, update_existing: bool, index: ImportIndex, warnings: Vec<String>, now: String) -> Self {
        let by_path = s.catalog.photos().filter(|p| p.copy_of.is_none()).filter_map(|p| source_path(p).map(|path| (key(path), p.id))).collect();
        LightroomApply {
            source,
            update_existing,
            index,
            now,
            ids: HashMap::new(),
            by_path,
            warnings,
            unmapped: BTreeMap::new(),
            missing: Vec::new(),
            preserved: 0,
            imported: 0,
            promoted: HashSet::new(),
            refused: HashSet::new(),
            failed: Vec::new(),
            skipped: Vec::new(),
            in_trash: HashSet::new(),
        }
    }

    /// Photos added so far.
    pub(crate) fn len(&self) -> usize {
        self.ids.len()
    }

    /// The photo a catalog record becomes before its own metadata and settings: the library's
    /// (`existing`), a new virtual copy of its master, a new photo for a content duplicate of a
    /// library photo (`duplicate`, with what its own file says), or one from the catalog record.
    fn base_photo(
        &mut self,
        s: &mut crate::Session,
        src: &CatalogPhoto,
        existing: Option<PhotoId>,
        planned: &HashMap<PhotoId, Photo>,
        duplicate: Option<(PhotoId, Option<&crate::import::FileFacts>)>,
    ) -> crate::Result<Photo> {
        let master = number(&src.image, "masterImage");
        let mut p = if let Some(id) = existing.and_then(|id| s.catalog.photo(id)) {
            if id.deleted {
                self.in_trash.insert(id.id);
            }
            (**id).clone()
        } else if master != 0 {
            let master_id = self.ids.get(&master).copied().ok_or_else(|| error(format!("virtual copy {}: missing master", src.source_id)))?;
            let mut p = planned
                .get(&master_id)
                .cloned()
                .or_else(|| s.catalog.photo(master_id).map(|p| (**p).clone()))
                .ok_or_else(|| error("virtual copy master is unavailable"))?;
            p.id = s.catalog.alloc_photo_id();
            p.copy_of = Some(master_id);
            p.copy_name = Some(text(&src.image, "copyName").to_string());
            // a new copy of a master in Recently Deleted is imported, not trashed with it
            p.deleted = false;
            p
        } else if let Some((duplicate, file)) = duplicate.and_then(|(id, file)| s.catalog.photo(id).cloned().map(|p| (p, file))) {
            content_duplicate(s, &src.path, &duplicate, file, &self.now)
        } else {
            catalog_photo(src, s.catalog.alloc_photo_id(), &self.now)
        };
        // Offline videos bypass file probing; Lightroom's format still identifies their kind.
        if text(&src.image, "fileFormat").eq_ignore_ascii_case("VIDEO") {
            p.kind = lightcraft_catalog::MediaKind::Video;
        }
        Ok(p)
    }

    /// Add one batch of catalog photos (masters ahead of their virtual copies), whose originals
    /// `files` (the import of the batch's files, just committed) added; one undo step.
    pub(crate) fn batch(
        &mut self,
        s: &mut crate::Session,
        mut photos: Vec<MappedPhoto>,
        files: crate::import::ImportReport,
        missing_sources: &HashSet<String>,
    ) -> crate::Result<()> {
        for id in &files.imported {
            if let Some(p) = s.catalog.photo(PhotoId(*id)).filter(|p| p.copy_of.is_none())
                && let Some(path) = source_path(p)
            {
                self.by_path.insert(key(path), p.id);
            }
        }
        self.imported += files.imported.len();
        self.failed.extend(files.failed);
        let duplicates: HashMap<_, _> =
            files.duplicates.iter().filter_map(|d| d.existing.map(|id| (key(&d.path), (PhotoId(id), d.file.as_deref())))).collect();
        let mut ops = Vec::new();
        let now = self.now.clone();
        photos.sort_by_key(|m| number(&m.src.image, "masterImage") != 0);
        // Only a master whose virtual copy is in this batch needs a second owned photo while the
        // batch is staged (a copy in a later batch finds its master in the library).
        let copy_masters: HashSet<i64> = photos.iter().map(|m| number(&m.src.image, "masterImage")).filter(|id| *id != 0).collect();
        let mut planned: HashMap<PhotoId, Photo> = HashMap::new();
        for mut m in photos {
            let src = &m.src;
            let master = number(&src.image, "masterImage");
            // nothing but a file type LightCraft reads becomes a record (#2135); the catalog's
            // reader already left these out, so this finds one only on a route around it
            if master == 0 && !crate::open_path::is_media(Path::new(&src.path)) && !is_catalog_video(src) {
                self.warnings.push(format!("image {}: not imported; {} isn't one of the file types LightCraft reads", src.source_id, src.path));
                self.refused.insert(src.source_id);
                continue;
            }
            if master == 0 && missing_sources.contains(&key(&src.path)) {
                self.missing.push(src.path.clone());
            }
            // A refused path must not become a library record the app would look at later; its
            // virtual copies (in this batch or a later one) go with it. (`masterImage` is 0 for a
            // master: a refused id 0 in a malformed catalog must not take every master with it.)
            if matches!(src.original, Original::Refused(_)) || (master != 0 && self.refused.contains(&master)) {
                self.refused.insert(src.source_id);
                continue;
            }
            let indexed = self
                .index
                .photos
                .get(&identity(&self.source, src))
                .copied()
                .map(PhotoId)
                .filter(|id| s.catalog.photo(*id).is_some_and(|p| source_path(p).is_some_and(|path| key(path) == key(&src.path))));
            let existing_id = indexed.or_else(|| if master == 0 { self.by_path.get(&key(&src.path)).copied() } else { None });
            let duplicate = if master == 0 { duplicates.get(&key(&src.path)).copied() } else { None };
            let mut p = self.base_photo(s, src, existing_id, &planned, duplicate)?;
            self.ids.insert(src.source_id, p.id);
            self.index.photos.insert(identity(&self.source, src), p.id.0);
            let keep_develop = existing_id.is_some() && p.edited.is_some() && !self.update_existing;
            let previous_develop = (p.develop.clone(), p.edited.clone());
            if keep_develop {
                self.preserved += 1;
            }
            let source_id = m.src.source_id;
            match m.sidecar_for(Shape::of(&p)) {
                Some(Ok(sc)) => {
                    crate::sidecar::merge_into(&mut p, &sc, &now);
                }
                Some(Err(e)) => self.warnings.push(format!("image {source_id} XMP: {e}")),
                None => {}
            }
            let src = &m.src;
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
            let path = src.path.clone();
            if !keep_develop {
                match m.settings_for(Shape::of(&p)) {
                    Some(Ok((partial, unknown))) => {
                        p.develop = Arc::new(lightcraft_develop::apply_partial(&p.develop, &partial, 1.0));
                        p.edited = Some(now.clone());
                        if !unknown.is_empty() {
                            self.unmapped.insert(source_id, unknown);
                        }
                    }
                    Some(Err(e)) => self.warnings.push(format!("image {source_id} settings: {e}")),
                    None => {}
                }
            }
            if keep_develop {
                (p.develop, p.edited) = previous_develop;
            }
            // A missing original isn't read, so its Local record isn't promoted by the file import:
            // the catalog import still joins the library, and a virtual copy inherits that.
            let was_local = p.local;
            p.local = false;
            if let Some(old) = s.catalog.photo(p.id) {
                if old.kind != p.kind {
                    // Repair older imports with existing reversible operations. Removing just the
                    // record leaves album/stack membership intact; the enclosing batch is atomic.
                    let mut repaired = (**old).clone();
                    repaired.kind = p.kind;
                    ops.extend([Op::RemovePhoto { id: p.id }, Op::AddPhoto { photo: Box::new(repaired) }]);
                }
                if was_local && self.promoted.insert(p.id) {
                    self.imported += 1;
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
                self.imported += 1;
                ops.push(Op::AddPhoto { photo: Box::new(p.clone()) });
            }
            self.by_path.entry(key(&path)).or_insert(p.id);
            if copy_masters.contains(&source_id) {
                planned.insert(p.id, p);
            }
        }
        if !ops.is_empty() {
            s.commit("Import Lightroom Catalog", Op::Batch { ops })?;
        }
        Ok(())
    }

    /// Finish: the collections become albums (only when every photo was added: `complete`), and
    /// the report. (The import's undo steps are one: [`ImportSteps`].)
    pub(crate) fn finish(
        &mut self,
        s: &mut crate::Session,
        collections: &[Row],
        members: &[Row],
        complete: bool,
        archive: Option<&Path>,
    ) -> crate::Result<Value> {
        let mut albums = HashMap::new();
        let mut ops = Vec::new();
        if complete {
            for collection in collections {
                if number(collection, "systemOnly") != 0 {
                    continue;
                }
                let collection_key = format!("{}:{}", key(&self.source), number(collection, "id_local"));
                let id = self
                    .index
                    .collections
                    .get(&collection_key)
                    .copied()
                    .map(lightcraft_catalog::AlbumId)
                    .filter(|id| s.catalog.album(*id).is_some())
                    .unwrap_or_else(|| s.catalog.alloc_album_id());
                self.index.collections.insert(collection_key, id.0);
                albums.insert(number(collection, "id_local"), id);
            }
            let parents: HashMap<_, _> = collections.iter().map(|r| (number(r, "id_local"), number(r, "parent"))).collect();
            let mut grouped_members: HashMap<i64, Vec<&Row>> = HashMap::new();
            for member in members {
                grouped_members.entry(number(member, "collection")).or_default().push(member);
            }
            for members in grouped_members.values_mut() {
                members.sort_by(|a, b| text(a, "positionInCollection").cmp(text(b, "positionInCollection")));
            }
            let mut ordered: Vec<_> = collections.iter().collect();
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
                    self.warnings.push(format!("{}: smart rules preserved in archive; imported current membership", album.name));
                }
                if !album.folder {
                    album.photos = grouped_members
                        .get(&number(collection, "id_local"))
                        .into_iter()
                        .flatten()
                        .filter_map(|r| self.ids.get(&number(r, "image")).copied())
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
        }
        if !ops.is_empty() {
            s.commit("Import Lightroom Catalog", Op::Batch { ops })?;
        }
        let mut report = json!({
            "source": self.source,
            "photos": self.ids.len(),
            "imported": self.imported,
            "library": s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.to_string_lossy()),
            "collections": albums.len(),
            "preservedExistingEdits": self.preserved,
            "inRecentlyDeleted": self.in_trash.len(),
            "missing": self.missing,
            "failed": self.failed,
            "warnings": self.warnings,
            "skipped": self.skipped,
            "unmapped": self.unmapped,
            "archive": archive,
            "mapping": self.ids,
        });
        if !complete {
            report["cancelled"] = json!(true);
        }
        Ok(report)
    }
}

/// An import's undo steps as one. The steps a batch takes fold into the import's step while that
/// is still on top of the undo stack; a step taken in between (an edit made while the import runs,
/// or an undo) stays apart, and the import goes on in a new step. Counted by commits, not by the
/// stack's length, which stops growing at its cap.
#[derive(Debug, Default)]
pub(crate) struct ImportSteps {
    /// The undo stack's length and the session's commits right after the import's last step.
    top: Option<(usize, u64)>,
}

impl ImportSteps {
    /// Run `f`, folding the undo steps it takes into the import's.
    pub(crate) fn run<T>(&mut self, s: &mut crate::Session, f: impl FnOnce(&mut crate::Session) -> crate::Result<T>) -> crate::Result<T> {
        let on_top = self.top == Some((s.undo.len(), s.commits()));
        let commits0 = s.commits();
        let result = f(s);
        let taken = usize::try_from(s.commits().wrapping_sub(commits0)).unwrap_or(usize::MAX);
        if taken > 0 {
            s.merge_undo(taken.saturating_add(usize::from(on_top)).min(s.undo.len()), "Import Lightroom Catalog");
            self.top = Some((s.undo.len(), s.commits()));
        }
        result
    }
}

#[cfg(test)]
fn apply(s: &mut crate::Session, mut data: CatalogImport, update_existing: bool) -> crate::Result<Value> {
    // nothing but a file type LightCraft reads becomes a record, whichever route got here (#2135)
    refuse_non_media(&mut data);
    validate(&data).map_err(error)?;
    let archive_dir = s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.join("Interop"));
    let index_path = archive_dir.as_ref().map(|p| p.join("lightroom-index.json"));
    let index = load_index(index_path.as_deref())?;
    let archive_path = archive_dir.as_ref().map(|dir| crate::lightroom_archive::store(dir, &data).map_err(error)).transpose()?;
    let mut steps = ImportSteps::default();
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
    let files = steps.run(s, |s| crate::import::import(s, &paths, crate::import::ImportMode::Add))?;
    let now = (s.clock)();
    let mut apply = LightroomApply::new(s, data.source.clone(), update_existing, index, data.warnings.clone(), now.clone());
    apply.skipped = data.skipped.clone();
    let mut probe = Photo::new(PhotoId(0), Source::File { path: String::new() }, "", "", 0, 0, &now);
    let photos = data
        .photos
        .into_iter()
        .map(|src| {
            probe.format = text(&src.image, "fileFormat").to_string();
            MappedPhoto::new(src, Shape::of(&probe))
        })
        .collect();
    steps.run(s, |s| apply.batch(s, photos, files, &missing_sources))?;
    let report = steps.run(s, |s| apply.finish(s, &data.collections, &data.members, true, archive_path.as_deref()))?;
    if let Some(path) = index_path {
        s.persist()?;
        let bytes = serde_json::to_vec(&apply.index).map_err(|e| error(e.to_string()))?;
        lightcraft_catalog::safe_file::write_atomic(&path, &bytes).map_err(|e| error(e.to_string()))?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests;
