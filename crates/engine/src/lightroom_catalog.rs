//! Direct, read-only Lightroom Classic catalog import. SQLite pages are read in Rust;
//! Lightroom, Python and a C SQLite runtime are not needed. Original paths stay in place.
//! Raw settings are archived before mutation; mapped edits are approximations, not Adobe renders.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::SystemTime;

#[cfg(test)]
static AFTER_DATABASE_INIT_HOOK: std::sync::Mutex<Vec<(PathBuf, fn(&Path))>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
use crate::lightroom_sqlite::wal_checksum;
use crate::lightroom_sqlite::{Database, Value as SqlValue};
use lightcraft_catalog::{Album, Flag, Op, Photo, PhotoId, Source};
use lightcraft_geom::{Affine, Orientation, Point, Rect};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

type Row = BTreeMap<String, Value>;
const COMMAND: &str = "library.importLightroom";

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
}
impl Reader {
    fn table(&self, name: &str, required: bool, cancel: &AtomicBool) -> Result<Vec<Row>, String> {
        let Some(t) = self.db.live_table(name, cancel)? else {
            return if required { Err(format!("unsupported Lightroom catalog: missing {name}")) } else { Ok(Vec::new()) };
        };
        let columns = t.column_names.as_ref().ok_or_else(|| format!("cannot read {name} columns"))?;
        let rows = self.db.read_table_with_cancel(t.rootpage, columns.len(), cancel).map_err(|e| format!("{name}: {e:?}"))?;
        if rows.len() > 1_000_000 {
            return Err(format!("{name}: more than one million records"));
        }
        let mut mapped_rows = Vec::new();
        mapped_rows.try_reserve_exact(rows.len()).map_err(|_| format!("{name}: unable to reserve bounded rows"))?;
        for row in rows {
            ensure_read_active(cancel)?;
            mapped_rows.push(
                columns
                    .iter()
                    .cloned()
                    .zip(row.values.into_iter().map(|value| match value {
                        SqlValue::Null => Some(Value::Null),
                        SqlValue::Integer(n) => Some(json!(n)),
                        SqlValue::Real(n) => Some(json!(n)),
                        SqlValue::Text(s) => Some(json!(s)),
                        // Opaque Lightroom payloads never become JSON byte arrays.
                        SqlValue::Blob(_) => None,
                    }))
                    .filter_map(|(column, value)| value.map(|value| (column, value)))
                    .collect(),
            );
        }
        Ok(mapped_rows)
    }

    fn optional_table(&self, name: &str, warnings: &mut Vec<String>, cancel: &AtomicBool) -> Result<Vec<Row>, String> {
        match self.table(name, false, cancel) {
            Ok(rows) => Ok(rows),
            Err(error) => {
                ensure_read_active(cancel)?;
                if self.db.has_observed_source_failure() {
                    return Err(error);
                }
                warnings.push(format!("skipping optional Lightroom table {name}: {error}"));
                Ok(Vec::new())
            }
        }
    }

    fn xmp_table(&self, cancel: &AtomicBool) -> Result<(Vec<Row>, Vec<String>), String> {
        let Some(t) = self.db.live_table("Adobe_AdditionalMetadata", cancel)? else { return Ok((Vec::new(), Vec::new())) };
        let columns = t.column_names.as_ref().ok_or_else(|| "cannot read Adobe_AdditionalMetadata columns".to_string())?;
        let rows = self.db.read_table_with_cancel(t.rootpage, columns.len(), cancel).map_err(|e| format!("Adobe_AdditionalMetadata: {e:?}"))?;
        if rows.len() > 1_000_000 {
            return Err("Adobe_AdditionalMetadata: more than one million records".into());
        }
        let mut warnings = Vec::new();
        let mut out = Vec::new();
        out.try_reserve_exact(rows.len()).map_err(|_| "Adobe_AdditionalMetadata: unable to reserve bounded rows")?;
        for row in rows {
            ensure_read_active(cancel)?;
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
                ensure_read_active(cancel)?;
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogImport {
    pub source: String,
    pub photos: Vec<CatalogPhoto>,
    pub collections: Vec<Row>,
    pub members: Vec<Row>,
    pub collection_content: Vec<Row>,
    pub warnings: Vec<String>,
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

#[cfg(test)]
fn be_word(bytes: &[u8], at: usize) -> Result<u32, String> {
    let word = bytes.get(at..at + 4).and_then(|s| <[u8; 4]>::try_from(s).ok()).ok_or("truncated WAL")?;
    Ok(u32::from_be_bytes(word))
}

// SQLite file format §4: validate the live checksum chain in constant space, then overlay
// only its last committed prefix. The forensic timeline materializes EVERY historical
// database snapshot; it must never be used to validate a large, actively used catalog.
#[cfg(test)]
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileStamp {
    len: u64,
    modified: SystemTime,
    is_file: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileSnapshot {
    stamp: FileStamp,
    prefix: Vec<u8>,
}

struct GuardedFile {
    path: PathBuf,
    handle: File,
    initial: FileSnapshot,
    path_stamp: FileStamp,
    prefix_limit: usize,
}

impl GuardedFile {
    fn capture(path: &Path, source: &File, prefix_limit: usize) -> Result<Self, String> {
        let path_stamp = file_stamp(&std::fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?)?;
        let initial = file_snapshot(source, prefix_limit)?;
        if path_stamp != initial.stamp {
            return Err("catalog changed during reading; close Lightroom and retry".into());
        }
        let handle = source.try_clone().map_err(|error| format!("{}: clone read-only source: {error}", path.display()))?;
        Ok(Self { path: path.to_path_buf(), handle, initial, path_stamp, prefix_limit })
    }

    fn verify(&self) -> Result<(), String> {
        let current = file_snapshot(&self.handle, self.prefix_limit)?;
        let path_stamp = file_stamp(&std::fs::metadata(&self.path).map_err(|error| format!("{}: {error}", self.path.display()))?)?;
        if current != self.initial || path_stamp != self.path_stamp || path_stamp != current.stamp {
            return Err("catalog changed during reading; close Lightroom and retry".into());
        }
        Ok(())
    }
}

struct SourceGuard {
    main: GuardedFile,
    wal_path: PathBuf,
    wal: Option<GuardedFile>,
}

impl SourceGuard {
    fn capture(main_path: &Path, main: &File, wal_path: &Path, wal: Option<&File>) -> Result<Self, String> {
        let main = GuardedFile::capture(main_path, main, 100)?;
        let wal_path_stamp = optional_path_stamp(wal_path)?;
        let wal = match (wal, wal_path_stamp) {
            (Some(file), Some(_)) => Some(GuardedFile::capture(wal_path, file, 32)?),
            (None, None) => None,
            _ => return Err("catalog changed during reading; close Lightroom and retry".into()),
        };
        Ok(Self { main, wal_path: wal_path.to_path_buf(), wal })
    }

    fn main_len(&self) -> u64 {
        self.main.initial.stamp.len
    }

    fn wal_len(&self) -> Option<u64> {
        self.wal.as_ref().map(|file| file.initial.stamp.len)
    }

    fn verify(&self) -> Result<(), String> {
        self.main.verify()?;
        match (&self.wal, optional_path_stamp(&self.wal_path)?) {
            (Some(file), Some(_)) => file.verify(),
            (None, None) => Ok(()),
            _ => Err("catalog changed during reading; close Lightroom and retry".into()),
        }
    }
}

fn file_stamp(metadata: &Metadata) -> Result<FileStamp, String> {
    Ok(FileStamp {
        len: metadata.len(),
        modified: metadata.modified().map_err(|error| format!("read source modification time: {error}"))?,
        is_file: metadata.is_file(),
    })
}

fn optional_path_stamp(path: &Path) -> Result<Option<FileStamp>, String> {
    match std::fs::metadata(path) {
        Ok(metadata) => file_stamp(&metadata).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn file_snapshot(file: &File, prefix_limit: usize) -> Result<FileSnapshot, String> {
    let metadata = file.metadata().map_err(|error| format!("source metadata: {error}"))?;
    let stamp = file_stamp(&metadata)?;
    if !stamp.is_file {
        return Err("catalog source is not a regular file".into());
    }
    let prefix_len = usize::try_from(stamp.len.min(prefix_limit as u64)).map_err(|_| "source header length does not fit host")?;
    let mut prefix = vec![0; prefix_len];
    let mut reader = file.try_clone().map_err(|error| format!("clone read-only source: {error}"))?;
    reader.seek(SeekFrom::Start(0)).map_err(|error| format!("seek source header: {error}"))?;
    reader.read_exact(&mut prefix).map_err(|error| format!("read source header: {error}"))?;
    Ok(FileSnapshot { stamp, prefix })
}

/// Where a root folder is when its `absolutePath` isn't a folder here (the catalog came from
/// another computer): `relativePathFromCatalog`, which Lightroom records for roots on the
/// catalog's volume, resolved against the catalog's folder, if that is a folder.
fn relocated_root(absolute: &str, relative: &str, catalog: &Path) -> Option<PathBuf> {
    if relative.is_empty() || !Path::new(relative).is_relative() || Path::new(absolute).is_dir() {
        return None;
    }
    let dir = std::path::absolute(catalog).ok()?.parent()?.join(relative);
    let mut out = PathBuf::new();
    for part in dir.components() {
        match part {
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            part => out.push(part),
        }
    }
    out.is_dir().then_some(out)
}

/// Inspect `.lrcat` plus committed WAL pages without modifying either source file.
pub fn read(path: &Path) -> Result<CatalogImport, String> {
    let cancel = AtomicBool::new(false);
    let total = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    read_with_progress(path, &cancel, &total, &done)
}

pub fn read_with_progress(path: &Path, cancel: &AtomicBool, total: &AtomicUsize, done: &AtomicUsize) -> Result<CatalogImport, String> {
    // 13 catalog tables plus one validation stage; ImportJob adds one unit per original path.
    total.store(14, Ordering::Relaxed);
    done.store(0, Ordering::Relaxed);
    ensure_read_active(cancel)?;
    let wal_path = PathBuf::from(format!("{}-wal", path.display()));
    let main_file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let wal_file = match File::open(&wal_path) {
        Ok(file) => Some(file),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", wal_path.display())),
    };
    let source_guard = SourceGuard::capture(path, &main_file, &wal_path, wal_file.as_ref())?;
    let main_len = source_guard.main_len();
    let wal_file = match (wal_file, source_guard.wal_len()) {
        (Some(file), Some(len)) => Some((file, len)),
        (None, None) => None,
        _ => return Err("catalog changed during reading; close Lightroom and retry".into()),
    };
    ensure_read_active(cancel)?;
    let db = Database::open_file_with_wal(main_file, main_len, wal_file, cancel).map_err(|e| format!("invalid SQLite catalog: {e:?}"))?;
    source_guard.verify()?;
    #[cfg(test)]
    {
        let mut hooks = AFTER_DATABASE_INIT_HOOK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = hooks.iter().position(|(target, _)| target == path) {
            let (_, hook) = hooks.remove(index);
            hook(path);
        }
    }
    let reader = Reader { db };
    let mut warnings = Vec::new();
    let table = |name: &str, required: bool| -> Result<Vec<Row>, String> {
        ensure_read_active(cancel)?;
        let rows = reader.table(name, required, cancel)?;
        done.fetch_add(1, Ordering::Relaxed);
        Ok(rows)
    };
    let optional_table = |name: &str, warnings: &mut Vec<String>| -> Result<Vec<Row>, String> {
        ensure_read_active(cancel)?;
        let rows = reader.optional_table(name, warnings, cancel)?;
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
    let (metadata_rows, metadata_warnings) = match reader.xmp_table(cancel) {
        Ok(result) => result,
        Err(error) => {
            ensure_read_active(cancel)?;
            if reader.db.has_observed_source_failure() {
                return Err(error);
            }
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
    let mut root_ids: Vec<_> = roots.keys().copied().collect();
    root_ids.sort_unstable();
    for (id, root) in root_ids.iter().filter_map(|id| Some((*id, roots.get(id)?))) {
        ensure_read_active(cancel)?;
        let absolute = text(root, "absolutePath");
        let resolved = match relocated_root(absolute, text(root, "relativePathFromCatalog"), path) {
            Some(found) => {
                let found = found.to_string_lossy().to_string();
                warnings.push(format!("root folder {}: {absolute} not found; using {found} (relative to the catalog)", text(root, "name")));
                found
            }
            None => absolute.to_string(),
        };
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
    };
    for image in table("Adobe_images", true)? {
        ensure_read_active(cancel)?;
        let id = number(&image, "id_local");
        let file = files.get(&number(&image, "rootFile")).ok_or_else(|| format!("image {id}: missing file record"))?;
        let folder = folders.get(&number(file, "folder")).ok_or_else(|| format!("image {id}: missing folder record"))?;
        let root = roots.get(&number(folder, "rootFolder")).ok_or_else(|| format!("image {id}: missing root record"))?;
        let root_path = root_paths.get(&number(root, "id_local")).map_or("", String::as_str);
        let folder_path = text(folder, "pathFromRoot");
        let name = text(file, "idx_filename");
        if root_path.is_empty() || name.is_empty() {
            return Err(format!("image {id}: incomplete original path"));
        }
        let path = Path::new(root_path).join(folder_path).join(name).to_string_lossy().to_string();
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
            path,
            image,
            settings: develops.get(&id).cloned().unwrap_or_default(),
            xmp: packet,
            keywords: keywords.remove(&id).unwrap_or_default(),
            history: history.remove(&id).unwrap_or_default(),
            snapshots: snapshots.remove(&id).unwrap_or_default(),
        });
    }
    ensure_read_active(cancel)?;
    source_guard.verify()?;
    Ok(out)
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
        .filter(|p| number(&p.image, "masterImage") == 0 && !existing.contains_key(&key(&p.path)) && Path::new(&p.path).is_file())
        .map(|p| p.path.clone())
        .collect();
    let missing_sources: HashSet<String> =
        data.photos.iter().filter(|p| number(&p.image, "masterImage") == 0).filter(|p| !Path::new(&p.path).is_file()).map(|p| key(&p.path)).collect();
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

pub(crate) fn apply_prepared(s: &mut crate::Session, data: CatalogImport, context: ApplyPrepared<'_>) -> crate::Result<AppliedImport> {
    let ApplyPrepared { update_existing, files, index, archive_path, missing_sources, undo0 } = context;
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
    let mut planned: HashMap<PhotoId, Photo> = HashMap::new();
    for src in photos {
        let master = number(&src.image, "masterImage");
        if master == 0 && missing_sources.contains(&key(&src.path)) {
            missing.push(src.path.clone());
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
        report: json!({"source":data.source,"photos":ids.len(),"imported":imported,"library":s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.to_string_lossy()),"collections":albums.len(),"preservedExistingEdits":preserved,"missing":missing,"failed":files.failed,"warnings":warnings,"unmapped":unmapped,"archive":archive_path,"mapping":ids}),
        index: index.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_geom::CropGeometry;

    static CANCEL_DURING_PAGE_TEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static HIGH_PAGE_READS_TEST: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn cancel_after_keyword_root_page(page_id: u32) {
        use std::sync::atomic::Ordering;
        if (262_157..=262_159).contains(&page_id) {
            HIGH_PAGE_READS_TEST.fetch_add(1, Ordering::Relaxed);
            if page_id == 262_159 {
                CANCEL_DURING_PAGE_TEST.store(true, Ordering::Relaxed);
            }
        }
    }

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
        }
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
        assert_eq!(xmp(Some(&json!(encoded))).unwrap(), String::from_utf8_lossy(packet));
        encoded[0] = 0xff;
        assert!(xmp(Some(&json!(encoded))).is_err());
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

    fn leaf_one(page: &mut [u8], offset: usize, rowid: u64, values: &[SqlValue]) {
        let payload = record(values);
        let mut cell = varint(payload.len() as u64);
        cell.extend(varint(rowid));
        cell.extend(payload);
        let end = page.len() - cell.len();
        page[offset] = 13;
        page[offset + 3..offset + 5].copy_from_slice(&1u16.to_be_bytes());
        page[end..end + cell.len()].copy_from_slice(&cell);
        page[offset + 5..offset + 7].copy_from_slice(&(end as u16).to_be_bytes());
        page[offset + 8..offset + 10].copy_from_slice(&(end as u16).to_be_bytes());
    }

    /// A one-row-per-table SQLite file: `(name, columns, row)` each.
    fn native_catalog(tables: &[(&str, &str, Vec<SqlValue>)]) -> Vec<u8> {
        use SqlValue::{Integer as I, Text as T};
        let mut bytes = vec![0u8; 5 * 2048];
        bytes[..16].copy_from_slice(b"SQLite format 3\0");
        bytes[16..18].copy_from_slice(&2048u16.to_be_bytes());
        bytes[18] = 1;
        bytes[19] = 1;
        bytes[21] = 64;
        bytes[22] = 32;
        bytes[23] = 32;
        bytes[28..32].copy_from_slice(&5u32.to_be_bytes());
        bytes[44..48].copy_from_slice(&4u32.to_be_bytes());
        bytes[56..60].copy_from_slice(&1u32.to_be_bytes());
        let mut schema = Vec::new();
        for (i, (name, columns, row)) in tables.iter().enumerate() {
            schema.push(vec![
                T("table".into()),
                T((*name).into()),
                T((*name).into()),
                I(i as i64 + 2),
                T(format!("CREATE TABLE {name} ({columns})")),
            ]);
            leaf(&mut bytes[(i + 1) * 2048..(i + 2) * 2048], 0, std::slice::from_ref(row));
        }
        leaf(&mut bytes[..2048], 100, &schema);
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

    struct SparseCatalogFixture {
        path: PathBuf,
        keep: bool,
    }

    impl Drop for SparseCatalogFixture {
        fn drop(&mut self) {
            if self.keep {
                return;
            }
            for suffix in ["", "-wal", "-shm", "-journal"] {
                let path = PathBuf::from(format!("{}{suffix}", self.path.display()));
                let _ = std::fs::remove_file(path);
            }
        }
    }

    fn sparse_catalog_fixture(large: bool) -> SparseCatalogFixture {
        sparse_catalog_fixture_with_optional_wal_table(large, None)
    }

    fn sparse_catalog_fixture_with_optional_wal_table(large: bool, optional_wal_table: Option<&str>) -> SparseCatalogFixture {
        use std::io::{Seek, SeekFrom, Write};

        fn write_page(file: &mut std::fs::File, page_id: u32, page: &[u8], page_size: u64) {
            let offset = u64::from(page_id - 1).checked_mul(page_size).expect("fixture page offset");
            file.seek(SeekFrom::Start(offset)).expect("seek fixture page");
            file.write_all(page).expect("write fixture page");
        }

        static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);
        const PAGE_SIZE: usize = 4096;
        const LOCK_BYTE_PAGE: u32 = 262_145;
        let page_count = if large {
            262_160
        } else if optional_wal_table.is_some() {
            7
        } else {
            6
        };
        let scratch =
            std::env::var_os("LIGHTCRAFT_TEST_SCRATCH").or_else(|| std::env::var_os("TMPDIR")).map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let scratch = scratch.join("lightcraft-lrcat-fixtures");
        std::fs::create_dir_all(&scratch).expect("create fixture scratch directory");
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let path = scratch.join(format!("synthetic-{}-{sequence}.lrcat", std::process::id()));
        let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).expect("create new synthetic catalog");
        drop(file);

        if large && cfg!(windows) {
            let set_sparse = std::process::Command::new("C:/Windows/System32/fsutil.exe")
                .args(["sparse", "setflag"])
                .arg(&path)
                .output()
                .expect("run fsutil sparse setflag on new synthetic fixture");
            assert!(set_sparse.status.success(), "fsutil sparse setflag failed: {}", String::from_utf8_lossy(&set_sparse.stderr));
            let query_sparse = std::process::Command::new("C:/Windows/System32/fsutil.exe")
                .args(["sparse", "queryflag"])
                .arg(&path)
                .output()
                .expect("verify sparse flag on synthetic fixture");
            assert!(query_sparse.status.success(), "fsutil sparse queryflag failed: {}", String::from_utf8_lossy(&query_sparse.stderr));
            let query = format!("{}{}", String::from_utf8_lossy(&query_sparse.stdout), String::from_utf8_lossy(&query_sparse.stderr));
            assert!(query.to_ascii_lowercase().contains("sparse"), "fsutil did not confirm a sparse fixture: {query}");
        }

        let mut file = std::fs::OpenOptions::new().write(true).open(&path).expect("open synthetic catalog");
        let length = u64::from(page_count).checked_mul(PAGE_SIZE as u64).expect("fixture length");
        file.set_len(length).expect("extend synthetic catalog sparsely");

        let tables = [
            (
                "AgLibraryRootFolder",
                "id_local INTEGER PRIMARY KEY, absolutePath",
                vec![SqlValue::Null, SqlValue::Text(scratch.join("synthetic-originals").to_string_lossy().into_owned())],
            ),
            (
                "AgLibraryFolder",
                "id_local INTEGER PRIMARY KEY, rootFolder, pathFromRoot",
                vec![SqlValue::Null, SqlValue::Integer(1), SqlValue::Text("Photos".into())],
            ),
            (
                "AgLibraryFile",
                "id_local INTEGER PRIMARY KEY, folder, idx_filename",
                vec![SqlValue::Null, SqlValue::Integer(1), SqlValue::Text("test.ARW".into())],
            ),
            (
                "Adobe_images",
                "id_local INTEGER PRIMARY KEY, id_global, rootFile, fileFormat, fileWidth, fileHeight, rating, pick",
                vec![
                    SqlValue::Null,
                    SqlValue::Text("synthetic-large-catalog-uuid".into()),
                    SqlValue::Integer(1),
                    SqlValue::Text("ARW".into()),
                    SqlValue::Integer(6000),
                    SqlValue::Integer(4000),
                    SqlValue::Integer(5),
                    SqlValue::Integer(1),
                ],
            ),
        ];
        let mut schema = Vec::new();
        for (index, (name, columns, row)) in tables.iter().enumerate() {
            let rootpage = match index {
                3 if large => page_count,
                3 => 5,
                _ => index as u32 + 2,
            };
            schema.push(vec![
                SqlValue::Text("table".into()),
                SqlValue::Text((*name).into()),
                SqlValue::Text((*name).into()),
                SqlValue::Integer(i64::from(rootpage)),
                SqlValue::Text(format!("CREATE TABLE {name} ({columns})")),
            ]);
            let mut page = vec![0u8; PAGE_SIZE];
            leaf(&mut page, 0, std::slice::from_ref(row));
            write_page(&mut file, rootpage, &page, PAGE_SIZE as u64);
        }

        let keyword_root = if large { page_count - 1 } else { 6 };
        schema.push(vec![
            SqlValue::Text("table".into()),
            SqlValue::Text("AgLibraryKeyword".into()),
            SqlValue::Text("AgLibraryKeyword".into()),
            SqlValue::Integer(i64::from(keyword_root)),
            SqlValue::Text("CREATE TABLE AgLibraryKeyword (id_local INTEGER PRIMARY KEY, name, parent)".into()),
        ]);
        if let Some(name) = optional_wal_table {
            let (columns, _) = match name {
                "Adobe_imageDevelopSettings" => ("image INTEGER, text", vec![SqlValue::Integer(1), SqlValue::Text("s = {}".into())]),
                "Adobe_AdditionalMetadata" => ("image INTEGER, xmp BLOB", vec![SqlValue::Integer(1), SqlValue::Null]),
                _ => panic!("unsupported synthetic optional table {name}"),
            };
            schema.push(vec![
                SqlValue::Text("table".into()),
                SqlValue::Text(name.into()),
                SqlValue::Text(name.into()),
                SqlValue::Integer(i64::from(page_count)),
                SqlValue::Text(format!("CREATE TABLE {name} ({columns})")),
            ]);
        }
        let keyword_rows = [
            vec![SqlValue::Null, SqlValue::Text("Synthetic one".into()), SqlValue::Null],
            vec![SqlValue::Null, SqlValue::Text("Synthetic two".into()), SqlValue::Integer(1)],
        ];
        if large {
            let left_page_id = page_count - 2;
            let right_page_id = page_count - 3;
            let mut interior = vec![0u8; PAGE_SIZE];
            interior[0] = 0x05;
            interior[3..5].copy_from_slice(&1u16.to_be_bytes());
            let cell_at = PAGE_SIZE - 5;
            interior[5..7].copy_from_slice(&(cell_at as u16).to_be_bytes());
            interior[8..12].copy_from_slice(&right_page_id.to_be_bytes());
            interior[12..14].copy_from_slice(&(cell_at as u16).to_be_bytes());
            interior[cell_at..cell_at + 4].copy_from_slice(&left_page_id.to_be_bytes());
            interior[cell_at + 4] = 1;
            write_page(&mut file, keyword_root, &interior, PAGE_SIZE as u64);
            let mut left_page = vec![0u8; PAGE_SIZE];
            leaf_one(&mut left_page, 0, 1, &keyword_rows[0]);
            write_page(&mut file, left_page_id, &left_page, PAGE_SIZE as u64);
            let mut right_page = vec![0u8; PAGE_SIZE];
            leaf_one(&mut right_page, 0, 2, &keyword_rows[1]);
            write_page(&mut file, right_page_id, &right_page, PAGE_SIZE as u64);
        } else {
            let mut page = vec![0u8; PAGE_SIZE];
            leaf(&mut page, 0, &keyword_rows);
            write_page(&mut file, keyword_root, &page, PAGE_SIZE as u64);
        }

        let mut free_count = 0u32;
        for page_id in 5..page_count {
            if large && page_id != LOCK_BYTE_PAGE && page_id < page_count - 3 {
                free_count += 1;
            }
        }
        let first_trunk: u32 = if free_count == 0 { 0 } else { 5 };
        let mut free_pages = (5..page_count).filter(|page_id| large && *page_id != LOCK_BYTE_PAGE && *page_id < page_count - 3);
        let mut trunk = free_pages.next();
        while let Some(trunk_page) = trunk {
            let mut leaves = Vec::with_capacity(1018);
            for _ in 0..1018 {
                let Some(page_id) = free_pages.next() else { break };
                leaves.push(page_id);
            }
            let next_trunk = free_pages.next();
            let mut page = vec![0u8; PAGE_SIZE];
            page[..4].copy_from_slice(&next_trunk.unwrap_or(0).to_be_bytes());
            page[4..8].copy_from_slice(&(leaves.len() as u32).to_be_bytes());
            for (index, page_id) in leaves.iter().enumerate() {
                let at = 8 + index * 4;
                page[at..at + 4].copy_from_slice(&page_id.to_be_bytes());
            }
            write_page(&mut file, trunk_page, &page, PAGE_SIZE as u64);
            trunk = next_trunk;
        }

        let mut header_page = vec![0u8; PAGE_SIZE];
        header_page[..16].copy_from_slice(b"SQLite format 3\0");
        header_page[16..18].copy_from_slice(&(PAGE_SIZE as u16).to_be_bytes());
        header_page[18] = 1;
        header_page[19] = 1;
        header_page[21] = 64;
        header_page[22] = 32;
        header_page[23] = 32;
        header_page[24..28].copy_from_slice(&1u32.to_be_bytes());
        header_page[28..32].copy_from_slice(&page_count.to_be_bytes());
        header_page[32..36].copy_from_slice(&first_trunk.to_be_bytes());
        header_page[36..40].copy_from_slice(&free_count.to_be_bytes());
        header_page[40..44].copy_from_slice(&1u32.to_be_bytes());
        header_page[44..48].copy_from_slice(&4u32.to_be_bytes());
        header_page[56..60].copy_from_slice(&1u32.to_be_bytes());
        header_page[92..96].copy_from_slice(&1u32.to_be_bytes());
        leaf(&mut header_page, 100, &schema);
        write_page(&mut file, 1, &header_page, PAGE_SIZE as u64);
        file.sync_all().expect("flush synthetic catalog");
        drop(file);

        SparseCatalogFixture { path, keep: large && std::env::var_os("LIGHTCRAFT_KEEP_SPARSE_FIXTURE").is_some() }
    }

    fn optional_wal_fixture(table_name: &str) -> SparseCatalogFixture {
        use std::io::Write;

        const PAGE_SIZE: usize = 4096;
        const PAGE_ID: u32 = 7;
        const SALT: (u32, u32) = (0x1234_5678, 0x9abc_def0);

        let fixture = sparse_catalog_fixture_with_optional_wal_table(false, Some(table_name));
        let row = match table_name {
            "Adobe_imageDevelopSettings" => vec![SqlValue::Integer(1), SqlValue::Text("s = {}".into())],
            "Adobe_AdditionalMetadata" => vec![SqlValue::Integer(1), SqlValue::Null],
            _ => panic!("unsupported synthetic optional table {table_name}"),
        };
        let mut page = vec![0u8; PAGE_SIZE];
        leaf_one(&mut page, 0, 1, &row);

        let mut wal = vec![0u8; 32];
        wal[..4].copy_from_slice(&0x377f0682u32.to_be_bytes());
        wal[4..8].copy_from_slice(&3_007_000u32.to_be_bytes());
        wal[8..12].copy_from_slice(&(PAGE_SIZE as u32).to_be_bytes());
        wal[16..20].copy_from_slice(&SALT.0.to_be_bytes());
        wal[20..24].copy_from_slice(&SALT.1.to_be_bytes());
        let mut checksum = wal_checksum(&wal[..24], false, (0, 0));
        wal[24..28].copy_from_slice(&checksum.0.to_be_bytes());
        wal[28..32].copy_from_slice(&checksum.1.to_be_bytes());

        let mut frame = [0u8; 24];
        frame[..4].copy_from_slice(&PAGE_ID.to_be_bytes());
        frame[4..8].copy_from_slice(&PAGE_ID.to_be_bytes());
        frame[8..12].copy_from_slice(&SALT.0.to_be_bytes());
        frame[12..16].copy_from_slice(&SALT.1.to_be_bytes());
        checksum = wal_checksum(&frame[..8], false, checksum);
        checksum = wal_checksum(&page, false, checksum);
        frame[16..20].copy_from_slice(&checksum.0.to_be_bytes());
        frame[20..24].copy_from_slice(&checksum.1.to_be_bytes());
        wal.extend(frame);
        wal.extend(page);

        let wal_path = PathBuf::from(format!("{}-wal", fixture.path.display()));
        let mut writer = std::fs::OpenOptions::new().write(true).create_new(true).open(&wal_path).expect("create synthetic optional-table WAL");
        writer.write_all(&wal).expect("write synthetic optional-table WAL");
        writer.sync_all().expect("flush synthetic optional-table WAL");
        fixture
    }

    fn mutate_optional_wal_frame_page_id(path: &Path) {
        use std::io::{Seek, SeekFrom, Write};

        let wal_path = PathBuf::from(format!("{}-wal", path.display()));
        let mut wal = std::fs::OpenOptions::new().read(true).write(true).open(wal_path).expect("open synthetic WAL for indexed-frame mutation");
        let modified = wal.metadata().expect("read synthetic WAL timestamp").modified().expect("synthetic WAL modification time");
        wal.seek(SeekFrom::Start(32)).expect("seek indexed synthetic WAL frame");
        wal.write_all(&8u32.to_be_bytes()).expect("change indexed synthetic WAL page id");
        wal.sync_all().expect("flush indexed synthetic WAL mutation");
        wal.set_times(std::fs::FileTimes::new().set_modified(modified)).expect("restore synthetic WAL modification time");
        wal.sync_all().expect("flush restored synthetic WAL timestamp");
    }

    fn truncate_optional_wal_after_database_init(path: &Path) {
        let wal_path = PathBuf::from(format!("{}-wal", path.display()));
        let wal = std::fs::OpenOptions::new().write(true).open(wal_path).expect("open synthetic WAL for injected source-read failure");
        wal.set_len(32).expect("truncate synthetic WAL after its index was built");
    }

    fn remove_wal_after_database_init(path: &Path) {
        std::fs::remove_file(format!("{}-wal", path.display())).expect("delete synthetic WAL after database initialization");
    }

    fn reset_wal_after_database_init(path: &Path) {
        use std::io::{Seek, SeekFrom, Write};
        let wal_path = PathBuf::from(format!("{}-wal", path.display()));
        let mut wal = std::fs::OpenOptions::new().write(true).open(wal_path).expect("open synthetic WAL for reset");
        let modified = wal.metadata().expect("read WAL timestamp before reset").modified().expect("WAL modification timestamp");
        wal.seek(SeekFrom::Start(16)).expect("seek WAL salt fields");
        wal.write_all(&[0u8; 8]).expect("reset WAL salts");
        wal.sync_all().expect("flush WAL reset");
        wal.set_times(std::fs::FileTimes::new().set_modified(modified)).expect("restore WAL timestamp after reset");
        wal.sync_all().expect("flush restored WAL timestamp");
    }

    fn grow_wal_after_database_init(path: &Path) {
        use std::io::Write;
        let wal_path = PathBuf::from(format!("{}-wal", path.display()));
        let mut wal = std::fs::OpenOptions::new().append(true).open(wal_path).expect("open synthetic WAL for growth");
        wal.write_all(&[0x5a]).expect("append synthetic WAL tail");
        wal.sync_all().expect("flush WAL growth");
    }

    fn truncate_wal_after_database_init(path: &Path) {
        let wal_path = PathBuf::from(format!("{}-wal", path.display()));
        std::fs::OpenOptions::new()
            .write(true)
            .open(wal_path)
            .expect("open synthetic WAL for truncation")
            .set_len(32)
            .expect("truncate synthetic WAL");
    }

    fn replace_file_with_distinct_stamp(path: &Path, changed_offset: usize) {
        use std::io::Write;
        let mut bytes = std::fs::read(path).expect("read synthetic source before replacement");
        bytes[changed_offset] ^= 0x5a;
        let replacement = PathBuf::from(format!("{}.replacement", path.display()));
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&replacement).expect("create replacement synthetic source");
        file.write_all(&bytes).expect("write replacement synthetic source");
        file.sync_all().expect("flush replacement synthetic source");
        file.set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
            .expect("stamp replacement synthetic source distinctly");
        drop(file);
        std::fs::remove_file(path).expect("remove old synthetic source path");
        std::fs::rename(&replacement, path).expect("install replacement synthetic source");
    }

    fn replace_main_after_database_init(path: &Path) {
        replace_file_with_distinct_stamp(path, 24);
    }

    fn replace_wal_after_database_init(path: &Path) {
        replace_file_with_distinct_stamp(&PathBuf::from(format!("{}-wal", path.display())), 16);
    }

    fn remove_main_after_database_init(path: &Path) {
        std::fs::remove_file(path).expect("delete synthetic main after database initialization");
    }

    fn assert_source_stamps_and_headers_unchanged(
        path: &Path,
        main_stamp: &std::fs::Metadata,
        wal_stamp: &std::fs::Metadata,
        main_header: &[u8; 100],
        wal_header: &[u8; 32],
    ) {
        use std::io::Read;

        let after_main = std::fs::metadata(path).expect("synthetic main metadata after observed WAL failure");
        let wal_path = PathBuf::from(format!("{}-wal", path.display()));
        let after_wal = std::fs::metadata(&wal_path).expect("synthetic WAL metadata after observed frame failure");
        assert_eq!(main_stamp.len(), after_main.len());
        assert_eq!(main_stamp.modified().ok(), after_main.modified().ok());
        assert_eq!(wal_stamp.len(), after_wal.len());
        assert_eq!(wal_stamp.modified().ok(), after_wal.modified().ok());
        let mut main_prefix = [0u8; 100];
        std::fs::File::open(path).expect("open synthetic main header").read_exact(&mut main_prefix).expect("read synthetic main header");
        assert_eq!(main_header, &main_prefix);
        let mut wal_prefix = [0u8; 32];
        std::fs::File::open(wal_path).expect("open synthetic WAL header").read_exact(&mut wal_prefix).expect("read synthetic WAL header");
        assert_eq!(wal_header, &wal_prefix);
    }

    fn read_after_indexed_optional_frame_changes(table_name: &str) -> Result<CatalogImport, String> {
        use std::io::Read;

        let fixture = optional_wal_fixture(table_name);
        let wal_path = PathBuf::from(format!("{}-wal", fixture.path.display()));
        let main_stamp = std::fs::metadata(&fixture.path).expect("synthetic main file stamp");
        let wal_stamp = std::fs::metadata(&wal_path).expect("synthetic WAL file stamp");
        let mut main_header = [0u8; 100];
        std::fs::File::open(&fixture.path)
            .expect("open synthetic main header before read")
            .read_exact(&mut main_header)
            .expect("read synthetic main header before read");
        let mut wal_header = [0u8; 32];
        std::fs::File::open(&wal_path)
            .expect("open synthetic WAL header before read")
            .read_exact(&mut wal_header)
            .expect("read synthetic WAL header before read");
        AFTER_DATABASE_INIT_HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((fixture.path.clone(), mutate_optional_wal_frame_page_id));
        let result = read(&fixture.path);
        assert_source_stamps_and_headers_unchanged(&fixture.path, &main_stamp, &wal_stamp, &main_header, &wal_header);
        result
    }

    #[test]
    fn cancellation_stops_catalog_btree_traversal_between_pages() {
        use std::sync::atomic::Ordering;

        let fixture = sparse_catalog_fixture(true);
        CANCEL_DURING_PAGE_TEST.store(false, Ordering::Relaxed);
        HIGH_PAGE_READS_TEST.store(0, Ordering::Relaxed);
        crate::lightroom_sqlite::set_page_read_hook_for_test(Some(cancel_after_keyword_root_page));
        let result = read_with_progress(&fixture.path, &CANCEL_DURING_PAGE_TEST, &AtomicUsize::new(0), &AtomicUsize::new(0));
        crate::lightroom_sqlite::set_page_read_hook_for_test(None);

        let error = result.expect_err("cancellation must stop catalog page traversal");
        assert!(error.contains("cancelled"), "unexpected cancellation error: {error}");
        assert_eq!(HIGH_PAGE_READS_TEST.load(Ordering::Relaxed), 1, "must stop before reading child/overflow pages after cancellation");
    }

    #[test]
    fn reads_large_sparse_catalog() {
        use std::io::{Read, Seek, SeekFrom};

        const LARGE_PAGE: u32 = 262_160;
        const PAGE_SIZE: usize = 4096;
        let small = sparse_catalog_fixture(false);
        let large = sparse_catalog_fixture(true);
        let expected_path = std::env::var_os("LIGHTCRAFT_TEST_SCRATCH")
            .or_else(|| std::env::var_os("TMPDIR"))
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("lightcraft-lrcat-fixtures")
            .join("synthetic-originals")
            .join("Photos")
            .join("test.ARW");
        let small_catalog = read(&small.path).expect("small control catalog parses");
        assert_eq!(small_catalog.photos.len(), 1);
        assert_eq!(small_catalog.photos[0].source_id, 1);
        assert_eq!(small_catalog.photos[0].uuid, "synthetic-large-catalog-uuid");
        assert_eq!(number(&small_catalog.photos[0].image, "rating"), 5);
        assert_eq!(number(&small_catalog.photos[0].image, "pick"), 1);
        assert_eq!(small_catalog.photos[0].path, expected_path.to_string_lossy());

        let high_page_offset = u64::from(LARGE_PAGE - 1) * PAGE_SIZE as u64;
        assert!(high_page_offset > 1u64 << 30, "selected image-table page must be beyond 1 GiB");
        let before = std::fs::metadata(&large.path).expect("large fixture metadata");
        assert_eq!(before.len(), 1_073_807_360);
        let high_page_offsets =
            [LARGE_PAGE - 1, LARGE_PAGE - 2, LARGE_PAGE - 3, LARGE_PAGE - 4].map(|page_offset| u64::from(page_offset) * PAGE_SIZE as u64);
        assert!(high_page_offsets.iter().all(|offset| *offset > 1u64 << 30), "table pages must be beyond 1 GiB");
        let mut before_pages = Vec::new();
        let mut source = std::fs::File::open(&large.path).expect("open large fixture read-only");
        for offset in high_page_offsets {
            let mut page = vec![0u8; PAGE_SIZE];
            source.seek(SeekFrom::Start(offset)).expect("seek high table page");
            source.read_exact(&mut page).expect("read high table page before inspection");
            before_pages.push(page);
        }
        drop(source);
        let companion_paths: Vec<_> =
            ["-wal", "-shm", "-journal"].iter().map(|suffix| PathBuf::from(format!("{}{suffix}", large.path.display()))).collect();
        assert!(companion_paths.iter().all(|path| !path.exists()), "fixture must start without SQLite companions");
        if large.keep {
            eprintln!("KEEP_SYNTHETIC_FIXTURE={}", large.path.display());
        }

        let catalog = read(&large.path).unwrap_or_else(|error| panic!("large sparse catalog should parse: {error}"));
        assert_eq!(catalog.photos.len(), 1);
        assert_eq!(catalog.photos[0].source_id, small_catalog.photos[0].source_id);
        assert_eq!(catalog.photos[0].uuid, small_catalog.photos[0].uuid);
        assert_eq!(catalog.photos[0].image, small_catalog.photos[0].image);
        assert_eq!(catalog.photos[0].path, small_catalog.photos[0].path);
        assert_eq!(catalog.photos[0].path, expected_path.to_string_lossy());

        let after = std::fs::metadata(&large.path).expect("large fixture metadata after inspection");
        assert_eq!(before.len(), after.len());
        assert_eq!(before.modified().ok(), after.modified().ok());
        let mut source = std::fs::File::open(&large.path).expect("reopen large fixture read-only");
        for (offset, before_page) in high_page_offsets.into_iter().zip(before_pages) {
            let mut after_page = vec![0u8; PAGE_SIZE];
            source.seek(SeekFrom::Start(offset)).expect("seek high table page after inspection");
            source.read_exact(&mut after_page).expect("read high table page after inspection");
            assert_eq!(before_page, after_page, "inspection must not mutate source pages");
        }
        assert!(companion_paths.iter().all(|path| !path.exists()), "inspection must not create WAL/SHM/journal companions");
    }

    #[test]
    fn detects_wal_creation_after_database_initialization() {
        fn create_wal(path: &Path) {
            let wal_path = PathBuf::from(format!("{}-wal", path.display()));
            std::fs::write(wal_path, b"synthetic WAL appeared during read").expect("create synthetic WAL during read");
        }

        let fixture = sparse_catalog_fixture(false);
        AFTER_DATABASE_INIT_HOOK.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((fixture.path.clone(), create_wal));
        let result = read(&fixture.path);
        assert!(result.is_err(), "reader must reject a WAL created while reading the main catalog");
        assert!(result.unwrap_err().contains("catalog changed during reading"));
    }

    #[test]
    fn detects_main_catalog_change_after_database_initialization() {
        fn mutate_header(path: &Path) {
            use std::io::{Seek, SeekFrom, Write};
            let mut file = std::fs::OpenOptions::new().write(true).open(path).expect("open synthetic catalog for mutation");
            file.seek(SeekFrom::Start(24)).expect("seek synthetic SQLite change counter");
            file.write_all(&2u32.to_be_bytes()).expect("mutate synthetic SQLite change counter");
            file.sync_all().expect("flush synthetic source mutation");
        }

        let fixture = sparse_catalog_fixture(false);
        AFTER_DATABASE_INIT_HOOK.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((fixture.path.clone(), mutate_header));
        let result = read(&fixture.path);
        assert!(result.is_err(), "reader must reject an observable source mutation during table reads");
        assert!(result.unwrap_err().contains("catalog changed during reading"));
    }

    #[test]
    fn observed_changed_optional_settings_wal_frame_fails_public_read() {
        let result = read_after_indexed_optional_frame_changes("Adobe_imageDevelopSettings");
        assert!(result.is_err(), "an indexed source-frame change in an optional table must fail the whole read, not become a warning");
    }

    #[test]
    fn observed_changed_optional_xmp_wal_frame_fails_public_read() {
        let result = read_after_indexed_optional_frame_changes("Adobe_AdditionalMetadata");
        assert!(result.is_err(), "an indexed source-frame change in optional XMP must fail the whole read, not become a warning");
    }

    #[test]
    fn observed_optional_source_failure_fails_prepare_before_catalog_or_archive_effects() {
        use std::io::Read;

        let fixture = optional_wal_fixture("Adobe_imageDevelopSettings");
        let wal_path = PathBuf::from(format!("{}-wal", fixture.path.display()));
        let main_stamp = std::fs::metadata(&fixture.path).expect("synthetic main file stamp");
        let wal_stamp = std::fs::metadata(&wal_path).expect("synthetic WAL file stamp");
        let mut main_header = [0u8; 100];
        std::fs::File::open(&fixture.path)
            .expect("open synthetic main header before prepare")
            .read_exact(&mut main_header)
            .expect("read synthetic main header before prepare");
        let mut wal_header = [0u8; 32];
        std::fs::File::open(&wal_path)
            .expect("open synthetic WAL header before prepare")
            .read_exact(&mut wal_header)
            .expect("read synthetic WAL header before prepare");
        AFTER_DATABASE_INIT_HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((fixture.path.clone(), mutate_optional_wal_frame_page_id));

        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock after epoch").as_nanos();
        let library = fixture.path.parent().expect("fixture parent").join(format!("disposable-failed-prepare-{}-{nonce}", std::process::id()));
        let mut session = crate::Session::new();
        session.open_library(&library, false).expect("open disposable synthetic library");
        let catalog_before = session.catalog.to_snapshot();
        let undo_before = session.undo.len();
        let interop = library.join("Interop");
        let index_path = interop.join("lightroom-index.json");
        let index_before = std::fs::read(&index_path).ok();
        let interop_existed_before = interop.exists();
        let mut interop_entries_before = if interop_existed_before {
            std::fs::read_dir(&interop)
                .expect("read initial synthetic Interop directory")
                .map(|entry| entry.expect("synthetic Interop entry").file_name())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        interop_entries_before.sort();

        let mut job =
            crate::lightroom_job::LightroomJob::new(&mut session, fixture.path.clone(), false).expect("create synthetic Lightroom import job");
        let result = job.prepare(&AtomicBool::new(false));
        let error = match result {
            Ok(_) => panic!("observed optional-source failure must abort preparation before staging"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("indexed WAL frame changed during reading"), "unexpected preparation error: {error}");
        assert_eq!(session.catalog.to_snapshot(), catalog_before, "failed prepare must not mutate catalog");
        assert_eq!(session.undo.len(), undo_before, "failed prepare must not add undo records");
        assert_eq!(interop.exists(), interop_existed_before, "failed prepare must not create archive storage");
        assert_eq!(std::fs::read(&index_path).ok(), index_before, "failed prepare must not write or alter the persistent import index");
        let mut interop_entries_after = if interop.exists() {
            std::fs::read_dir(&interop)
                .expect("read Interop directory after failed prepare")
                .map(|entry| entry.expect("synthetic Interop entry").file_name())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        interop_entries_after.sort();
        assert_eq!(interop_entries_after, interop_entries_before, "failed prepare must not create or change archives or the import index");
        assert_source_stamps_and_headers_unchanged(&fixture.path, &main_stamp, &wal_stamp, &main_header, &wal_header);
        session.close_library().expect("close disposable synthetic library");
        let _ = std::fs::remove_dir_all(library);
    }

    #[test]
    fn observed_optional_wal_read_error_fails_prepare_before_catalog_or_archive_effects() {
        let fixture = optional_wal_fixture("Adobe_imageDevelopSettings");
        AFTER_DATABASE_INIT_HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((fixture.path.clone(), truncate_optional_wal_after_database_init));
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock after epoch").as_nanos();
        let library = fixture.path.parent().expect("fixture parent").join(format!("disposable-failed-io-prepare-{}-{nonce}", std::process::id()));
        let mut session = crate::Session::new();
        session.open_library(&library, false).expect("open disposable synthetic library");
        let catalog_before = session.catalog.to_snapshot();
        let undo_before = session.undo.len();
        let interop = library.join("Interop");
        let entries_before = if interop.exists() {
            std::fs::read_dir(&interop)
                .expect("read initial synthetic Interop directory")
                .map(|entry| entry.expect("synthetic Interop entry").file_name())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        let mut job =
            crate::lightroom_job::LightroomJob::new(&mut session, fixture.path.clone(), false).expect("create synthetic Lightroom import job");
        let result = job.prepare(&AtomicBool::new(false));
        let error = match result {
            Ok(_) => panic!("observed optional WAL read error must abort preparation before staging"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("indexed WAL frame header"), "unexpected optional WAL I/O error: {error}");
        assert_eq!(session.catalog.to_snapshot(), catalog_before, "failed prepare must not mutate catalog");
        assert_eq!(session.undo.len(), undo_before, "failed prepare must not add undo records");
        let entries_after = if interop.exists() {
            std::fs::read_dir(&interop)
                .expect("read Interop directory after failed I/O prepare")
                .map(|entry| entry.expect("synthetic Interop entry").file_name())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        assert_eq!(entries_after, entries_before, "failed prepare must not stage an archive or index");
        assert!(!interop.join("lightroom-index.json").exists(), "failed prepare must not persist an import index");
        session.close_library().expect("close disposable synthetic library");
        let _ = std::fs::remove_dir_all(library);
    }

    #[test]
    fn rejects_supported_source_deletion_reset_growth_truncation_and_replacement() {
        let mutations: [(&str, fn(&Path), bool); 7] = [
            ("WAL deletion", remove_wal_after_database_init, true),
            ("WAL reset", reset_wal_after_database_init, true),
            ("WAL growth", grow_wal_after_database_init, true),
            ("WAL truncation", truncate_wal_after_database_init, true),
            ("WAL replacement", replace_wal_after_database_init, true),
            ("MAIN deletion", remove_main_after_database_init, false),
            ("MAIN replacement", replace_main_after_database_init, false),
        ];
        let mut accepted = Vec::new();
        for (label, mutation, has_wal) in mutations {
            let fixture = if has_wal { optional_wal_fixture("Adobe_imageDevelopSettings") } else { sparse_catalog_fixture(false) };
            AFTER_DATABASE_INIT_HOOK.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((fixture.path.clone(), mutation));
            if let Err(error) = read(&fixture.path) {
                assert!(!error.is_empty(), "{label} should fail with an actionable error");
            } else {
                accepted.push(label);
            }
        }
        assert!(accepted.is_empty(), "reader accepted source mutations after initialization: {accepted:?}");
    }

    #[test]
    fn missing_main_open_is_reported_without_creating_a_source() {
        let fixture = sparse_catalog_fixture(false);
        let missing = fixture.path.with_extension("missing-synthetic-source");
        let result = read(&missing);
        assert!(result.is_err(), "opening a nonexistent source must fail");
        assert!(!missing.exists(), "failed open must not create a source file");
    }

    #[test]
    fn reads_native_catalog_fixture_without_lightroom_or_external_processes() {
        let bytes = native_catalog(&photo_tables(("/catalog/", "")));
        let path = std::env::temp_dir().join(format!("lightcraft-native-catalog-{}.lrcat", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let catalog = read(&path).unwrap();
        assert_eq!(catalog.photos.len(), 1);
        assert_eq!(catalog.photos[0].uuid, "fixture-image");
        assert_eq!(number(&catalog.photos[0].image, "rating"), 5);
        assert!(catalog.photos[0].path.ends_with("test.ARW"));
        std::fs::write(&path, b"bad catalog").unwrap();
        assert!(read(&path).is_err());
        std::fs::remove_file(path).unwrap();
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
        assert!(catalog.photos[0].path.starts_with("C:/Users/me/Project/RAW/"), "{}", catalog.photos[0].path);
        assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn oversized_optional_table_warns_and_is_skipped_upstream_fixture() {
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
        let schema = vec![
            SqlValue::Text("table".into()),
            SqlValue::Text("optional_history".into()),
            SqlValue::Text("optional_history".into()),
            SqlValue::Integer(2),
            SqlValue::Text("CREATE TABLE optional_history (payload BLOB)".into()),
        ];
        leaf_one(&mut main[..512], 100, 1, &schema);
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
        let reader = Reader { db };
        let mut warnings = Vec::new();
        let cancel = AtomicBool::new(false);
        let rows = reader.optional_table("optional_history", &mut warnings, &cancel).unwrap();
        assert!(rows.is_empty());
        assert!(warnings.iter().any(|warning| warning.contains("optional_history")));
    }
    #[test]
    fn reads_native_sqlite_tables_without_lightroom_or_external_processes() {
        let fixture = sparse_catalog_fixture(false);
        let catalog = read(&fixture.path).expect("synthetic native SQLite catalog");
        assert_eq!(catalog.photos.len(), 1);
        assert_eq!(catalog.photos[0].uuid, "synthetic-large-catalog-uuid");
        assert_eq!(number(&catalog.photos[0].image, "rating"), 5);
        assert!(catalog.photos[0].path.ends_with("test.ARW"));
        std::fs::write(&fixture.path, b"bad catalog").expect("replace only the unique synthetic source with invalid bytes");
        assert!(read(&fixture.path).is_err());
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
        let sql = "CREATE TABLE optional_history (payload BLOB)";
        let schema = vec![
            SqlValue::Text("table".into()),
            SqlValue::Text("optional_history".into()),
            SqlValue::Text("optional_history".into()),
            SqlValue::Integer(2),
            SqlValue::Text(sql.into()),
        ];
        leaf_one(&mut main[..512], 100, 1, &schema);
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
        let reader = Reader { db };
        let mut warnings = Vec::new();
        let cancel = AtomicBool::new(false);
        let rows = reader.optional_table("optional_history", &mut warnings, &cancel).unwrap();
        assert!(rows.is_empty());
        assert!(warnings.iter().any(|warning| warning.contains("optional_history")));
    }
}
