//! Direct, read-only Lightroom Classic catalog import. SQLite pages are read in Rust;
//! Lightroom, Python and a C SQLite runtime are not needed. Original paths stay in place.
//! Raw settings are archived before mutation; mapped edits are approximations, not Adobe renders.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
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
use lcc_catalog::{Album, Flag, Op, Photo, PhotoId, Source};
use lcc_geom::{Affine, Orientation, Point, Rect};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) type Row = BTreeMap<String, Value>;
const COMMAND: &str = "library.importLightroom";

fn error(why: impl Into<String>) -> crate::EngineError {
    crate::EngineError::BadParams { cmd: COMMAND.into(), msg: why.into() }
}

fn text<'a>(r: &'a Row, k: &str) -> &'a str {
    r.get(k).and_then(Value::as_str).unwrap_or("")
}
/// Whether a collection row is a smart collection: its `creationId` names one (`com.adobe.ag.library.smart_collection`).
/// The import's smart albums and the collections a Collection rule can match both go by this one test.
fn is_smart_collection(r: &Row) -> bool {
    text(r, "creationId").contains("smart")
}
/// Whether a collection row is a collection set: its `creationId` names one (`com.adobe.ag.library.group`). The
/// import's folders and the collections a Collection rule can match both go by this one test.
fn is_collection_set(r: &Row) -> bool {
    text(r, "creationId").contains("group")
}
/// Whether a collection row is a saved print (`com.adobe.ag.print`; an unsaved one is `….print.unsaved`).
fn is_saved_print(r: &Row) -> bool {
    text(r, "creationId") == "com.adobe.ag.print"
}
/// Seconds from the Unix epoch to 2001-01-01T00:00:00Z, where Lightroom's times start.
const LIGHTROOM_EPOCH: i64 = 978_307_200;
/// When Lightroom last changed the image (`Adobe_images.touchTime`: seconds since 2001-01-01T00:00:00Z, also for
/// metadata changes), as the library writes times. `None` when it is missing, not a number, not after 2001 or past
/// the years a date holds.
fn touch_time(image: &Row) -> Option<String> {
    let secs = image.get("touchTime").and_then(Value::as_f64).filter(|t| t.is_finite() && *t > 0.0)?;
    // (a cast saturates: a huge time fails the checked add or the year check)
    lcc_catalog::dates::local((secs.floor() as i64).checked_add(LIGHTROOM_EPOCH)?)
}
fn number(r: &Row, k: &str) -> i64 {
    r.get(k).and_then(Value::as_f64).filter(|n| n.is_finite()).unwrap_or(0.0) as i64
}

pub(crate) fn number_for_job(r: &Row, k: &str) -> i64 {
    number(r, k)
}
/// The photo a side-table row (develop settings, XMP, keywords, history, snapshots, collection members) belongs to:
/// its `image` as a whole number, or `None` when that is missing, NULL, text or a fraction. [`number`] would read
/// those as 0, giving the row to a photo whose `id_local` is 0, and 2.5 as 2, giving it to photo 2.
fn image_of(r: &Row) -> Option<i64> {
    id_of(r, "image")
}
/// The record a row's `k` column names: its value as a whole number, or `None` when that is missing, NULL, text, a
/// fraction or out of `i64` range. [`number`] reads a missing, NULL or text value as 0, naming the record whose
/// `id_local` is 0, and truncates a fraction (1.5 names record 1).
fn id_of(r: &Row, k: &str) -> Option<i64> {
    let n = r.get(k)?.as_number()?;
    let f = n.as_f64()?;
    // An integer goes through f64 as [`number`] takes the `id_local` of the record it names, so both get the same key
    // past 2^53 too (read exactly, the row would miss its rounded record and be lost without a warning).
    if n.is_i64() {
        return Some(f as i64);
    }
    // (2^63 is the first value past i64::MAX; a cast would saturate onto a real id)
    (f.fract() == 0.0 && f >= i64::MIN as f64 && f < 9_223_372_036_854_775_808.0).then_some(f as i64)
}
/// `rows` of `table` with the photo each belongs to ([`image_of`]). Rows that name no photo are left out, with one
/// warning for the table.
fn by_image(table: &str, rows: Vec<Row>, warnings: &mut Vec<String>) -> Vec<(i64, Row)> {
    let total = rows.len();
    let keyed: Vec<_> = rows.into_iter().filter_map(|r| Some((image_of(&r)?, r))).collect();
    warn_unkeyed(table, total - keyed.len(), "skipped", warnings);
    keyed
}
/// One warning for the `count` rows of `table` whose `image` names no photo; `outcome` says what became of them.
fn warn_unkeyed(table: &str, count: usize, outcome: &str, warnings: &mut Vec<String>) {
    match count {
        0 => {}
        1 => warnings.push(format!("{table}: 1 record {outcome}; its image is not a photo id")),
        n => warnings.push(format!("{table}: {n} records {outcome}; their image is not a photo id")),
    }
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

pub(crate) struct Reader {
    db: Database,
}
/// The longest XMP packet kept for one image (what a compressed one may expand to). The bound is per
/// image, never a running total, so the size of a catalog alone cannot drop a packet (upstream #3674).
const MAX_XMP_PACKET: usize = 16 << 20;
/// Keyword paths kept for the photos of one catalog, all together (a million photos at ~1 KB of paths each still fit).
const MAX_KEYWORD_BYTES: usize = 1 << 30;
/// Most bytes of history-step or snapshot settings decoded from one table. They are archived, and an archive holds at
/// most [`crate::lightroom_archive::MAX_ARCHIVE_BYTES`] of JSON, so this keeps all a catalog can archive while a
/// hostile one can't make the reader allocate without bound (each row alone may decode to 16 MiB).
const MAX_DECODED_SETTINGS: usize = crate::lightroom_archive::MAX_ARCHIVE_BYTES as usize;
/// Set (to `true`) on a history-step or snapshot row read past [`MAX_DECODED_SETTINGS`], whose settings were left
/// undecoded. Not a Lightroom column: it tells the archive's reader why the row has no `text`, and keeps the row out
/// of the per-image "could not be read" count, since the table's one warning already covers it.
const OVER_DECODE_BUDGET: &str = "_overDecodeBudget";

/// The column of table `name` that Lightroom Classic 13 and later store as compressed text (see [`decode_compressed`]),
/// and what a warning calls one and several of its rows.
fn compressed_settings(name: &str) -> Option<(&'static str, &'static str, &'static str)> {
    match name {
        "Adobe_libraryImageDevelopHistoryStep" => Some(("text", "history step", "history steps")),
        "Adobe_libraryImageDevelopSnapshot" => Some(("text", "snapshot", "snapshots")),
        _ => None,
    }
}

impl Reader {
    /// Stream `name`'s rows to `f`, one at a time: the table is never collected whole, so a catalog's large develop,
    /// history and metadata tables cost only what `f` keeps, and its pages are read from the file one at a time. A
    /// missing table yields no rows unless `required`. `Blob` columns never become JSON; they reach `f` separately,
    /// as (column, bytes), and are dropped after it. `kept` says that `f` keeps every row, which bounds the walk to a
    /// million rows and the reader's table payload budget.
    pub(crate) fn each_row(
        &self,
        name: &str,
        required: bool,
        kept: bool,
        cancel: &AtomicBool,
        mut f: impl FnMut(Row, Vec<(&str, Vec<u8>)>) -> Result<(), String>,
    ) -> Result<(), String> {
        let Some(t) = self.db.live_table(name, cancel)? else {
            return if required { Err(format!("unsupported Lightroom catalog: missing {name}")) } else { Ok(()) };
        };
        let columns = t.column_names.as_ref().ok_or_else(|| format!("cannot read {name} columns"))?;
        let mut sink = |r: crate::lightroom_sqlite::Record| {
            ensure_read_active(cancel)?;
            let mut row = Row::new();
            let mut blobs = Vec::new();
            for (column, value) in columns.iter().zip(r.values) {
                let value = match value {
                    SqlValue::Null => Value::Null,
                    SqlValue::Integer(n) => json!(n),
                    SqlValue::Real(n) => json!(n),
                    SqlValue::Text(s) => json!(s),
                    SqlValue::Blob(bytes) => {
                        blobs.push((column.as_str(), bytes));
                        continue;
                    }
                };
                row.insert(column.clone(), value);
            }
            f(row, blobs)
        };
        let walked = if kept {
            self.db.for_each_kept_row_with_cancel(t.rootpage, columns.len(), cancel, &mut sink)
        } else {
            self.db.for_each_row_with_cancel(t.rootpage, columns.len(), cancel, &mut sink)
        };
        walked.map_err(|e| format!("{name}: {e}"))
    }

    pub(crate) fn table(&self, name: &str, required: bool, cancel: &AtomicBool) -> Result<Vec<Row>, String> {
        let mut out = Vec::new();
        self.each_row(name, required, true, cancel, |row, _| {
            out.try_reserve(1).map_err(|_| format!("{name}: unable to reserve bounded rows"))?;
            out.push(row);
            Ok(())
        })?;
        Ok(out)
    }

    /// An optional table's read failed: cancellation and a failing or changed source file fail the import (a partial
    /// read of a catalog that is being written must not import); anything else skips the table with a warning.
    fn skip_optional(&self, name: &str, error: String, warnings: &mut Vec<String>, cancel: &AtomicBool) -> Result<(), String> {
        ensure_read_active(cancel)?;
        if self.db.has_observed_source_failure() {
            return Err(error);
        }
        warnings.push(format!("skipping optional Lightroom table {name}: {error}"));
        Ok(())
    }

    fn optional_table(&self, name: &str, warnings: &mut Vec<String>, cancel: &AtomicBool) -> Result<Vec<Row>, String> {
        match self.table(name, false, cancel) {
            Ok(rows) => Ok(rows),
            Err(error) => {
                self.skip_optional(name, error, warnings, cancel)?;
                Ok(Vec::new())
            }
        }
    }

    /// An optional per-image table (history steps, snapshots), streamed into rows grouped by their `image` column.
    fn grouped(&self, name: &str, warnings: &mut Vec<String>, cancel: &AtomicBool) -> Result<HashMap<i64, Vec<Row>>, String> {
        self.grouped_within(name, MAX_DECODED_SETTINGS, warnings, cancel)
    }

    /// [`Self::grouped`], with the table's compressed settings column ([`compressed_settings`]) decoded within
    /// `budget` bytes for the whole table: each value is charged its declared length before it is inflated, even one
    /// that then fails to decode. A value that can't be decoded is left out of its row with a warning naming the
    /// image, and those past the budget with one warning for all. Other BLOB columns are always left out.
    fn grouped_within(&self, name: &str, budget: usize, warnings: &mut Vec<String>, cancel: &AtomicBool) -> Result<HashMap<i64, Vec<Row>>, String> {
        let compressed = compressed_settings(name);
        let mut out: HashMap<i64, Vec<Row>> = HashMap::new();
        let mut decoding = Vec::new();
        let mut left = budget;
        let mut over_budget = 0usize;
        let mut unkeyed = 0usize;
        let read = self.each_row(name, false, false, cancel, |mut r, blobs| {
            // a row that names no photo is left out (one warning for the table), not given to photo 0
            let Some(image) = image_of(&r) else {
                unkeyed += 1;
                return Ok(());
            };
            if let Some((settings, what, _)) = compressed {
                for (column, bytes) in blobs.into_iter().filter(|(column, _)| *column == settings) {
                    // charged at its declared length before it is inflated (which never goes past that length), so
                    // a row that then fails to decode counts too; past the budget, the rest aren't inflated at all
                    let declared = compressed_len(&bytes);
                    if declared.is_some_and(|n| over_budget > 0 || n > left) {
                        over_budget += 1;
                        r.insert(OVER_DECODE_BUDGET.to_string(), Value::Bool(true));
                        continue;
                    }
                    left -= declared.unwrap_or(0);
                    match decode_compressed(&bytes, "settings") {
                        Ok(text) => {
                            r.insert(column.to_string(), Value::String(text));
                        }
                        Err(why) => decoding.push(format!("image {image}: {what} {}: {why}", number(&r, "id_local"))),
                    }
                }
            }
            out.entry(image).or_default().push(r);
            Ok(())
        });
        if let Err(error) = read {
            self.skip_optional(name, error, warnings, cancel)?;
            out.clear();
            return Ok(out);
        }
        warn_unkeyed(name, unkeyed, "skipped", warnings);
        warnings.extend(decoding);
        if let (Some((_, one, many)), 1..) = (compressed, over_budget) {
            let what = if over_budget == 1 { one } else { many };
            warnings.push(format!("{over_budget} {what} read without their settings: more than {} MiB of settings in {name}", budget >> 20));
        }
        Ok(out)
    }

    /// Fill `out` with the as-shot white of each image in `wanted`, from the last of its history steps that has one.
    /// Streamed, a row at a time and none kept: only a wanted image's step is decoded, and each is bounded on its own.
    fn as_shot_whites(&self, wanted: &HashSet<i64>, cancel: &AtomicBool, out: &mut HashMap<i64, (f64, f64)>) -> Result<(), String> {
        self.each_row("Adobe_libraryImageDevelopHistoryStep", false, false, cancel, |r, blobs| {
            let Some(image) = image_of(&r) else { return Ok(()) };
            if !wanted.contains(&image) {
                return Ok(());
            }
            let white = match blobs.into_iter().find(|(column, _)| *column == "text") {
                Some((_, bytes)) => decode_compressed(&bytes, "settings").ok().as_deref().and_then(step_as_shot_white),
                None => step_as_shot_white(text(&r, "text")),
            };
            if let Some(white) = white {
                out.insert(image, white);
            }
            Ok(())
        })
    }

    /// `Adobe_imageDevelopSettings`, streamed into image id -> settings text. The photos' edits come from here, so a
    /// failure to read it fails the import rather than quietly bringing every photo in unedited.
    fn develop_settings(&self, cancel: &AtomicBool, warnings: &mut Vec<String>) -> Result<HashMap<i64, String>, String> {
        let mut out = HashMap::new();
        let mut unkeyed = 0usize;
        let read = self.each_row("Adobe_imageDevelopSettings", false, false, cancel, |r, _| {
            let Some(image) = image_of(&r) else {
                unkeyed += 1;
                return Ok(());
            };
            let settings = text(&r, "text");
            if !settings.is_empty() {
                out.insert(image, settings.to_string());
            }
            Ok(())
        });
        warn_unkeyed("Adobe_imageDevelopSettings", unkeyed, "skipped", warnings);
        read.map_err(|e| {
            if cancel.load(Ordering::Relaxed) {
                e
            } else {
                format!("the Lightroom develop settings could not be read, so no photo would get its edits: {e}")
            }
        })?;
        Ok(out)
    }

    /// `Adobe_AdditionalMetadata`, streamed into image id -> row with its (decompressed) `xmp` packet.
    fn xmp_table(&self, cancel: &AtomicBool) -> Result<(HashMap<i64, Row>, Vec<String>), String> {
        self.xmp_table_within(cancel, MAX_XMP_PACKET)
    }

    /// [`Self::xmp_table`] with each packet held to `max_packet` decoded bytes: a packet past it is left off
    /// its row and named in the warnings; the bound is per image, so the size of the catalog alone never
    /// drops a packet (upstream #3674).
    fn xmp_table_within(&self, cancel: &AtomicBool, max_packet: usize) -> Result<(HashMap<i64, Row>, Vec<String>), String> {
        let mut warnings = Vec::new();
        let mut out = HashMap::new();
        let mut unkeyed = 0usize;
        let read = self.each_row("Adobe_AdditionalMetadata", false, false, cancel, |mut r, blobs| {
            let Some(image) = image_of(&r) else {
                unkeyed += 1;
                return Ok(());
            };
            let blobs_had_xmp = blobs.iter().any(|(column, _)| *column == "xmp");
            for (column, bytes) in blobs {
                if column != "xmp" {
                    continue;
                }
                // the declared size (the decode checks it) against what is left, before inflating
                let declared = bytes.get(..4).and_then(|s| <[u8; 4]>::try_from(s).ok()).map_or(0, |s| u32::from_be_bytes(s) as usize);
                let over = |n: usize| n > max_packet;
                let decoded = if over(declared) { Ok(None) } else { decode_compressed(&bytes, "XMP").map(Some) };
                match decoded {
                    Ok(Some(packet)) if !over(packet.len()) => {
                        r.insert(column.to_string(), Value::String(packet));
                    }
                    Ok(_) => warnings.push(format!("image {image}: the XMP packet exceeds {} MiB; it is left out", max_packet >> 20)),
                    Err(error) => warnings.push(format!("image {image}: {error}")),
                }
            }
            // a text packet is held to the same bound as a decoded one
            if let Some(len) = r.get("xmp").and_then(Value::as_str).map(str::len)
                && !blobs_had_xmp
                && len > max_packet
            {
                r.remove("xmp");
                warnings.push(format!("image {image}: the XMP packet exceeds {} MiB; it is left out", max_packet >> 20));
            }
            out.insert(image, r);
            Ok(())
        });
        warn_unkeyed("Adobe_AdditionalMetadata", unkeyed, "skipped", &mut warnings);
        read?;
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
    /// Lightroom's as-shot `(Temperature, Tint)` for a Custom white balance, from its history: what the Kelvin in
    /// `settings` is measured from (upstream #3906; [`custom_as_shot`]). Not archived.
    #[serde(skip)]
    pub as_shot_white: Option<(f64, f64)>,
    /// A Capture One variant's develop settings ([`crate::capture_one_develop`]), in place of `settings`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_one: Option<Box<crate::capture_one_develop::CaptureOneDevelop>>,
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
    /// `AgLibraryFolderStackImage` rows (`stack`, `image`, `position`, `collapsed`).
    #[serde(default)]
    pub stacks: Vec<Row>,
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
    /// Its `AgLibraryFile` record is gone, or its `rootFile` is not a whole number.
    MissingFile,
    /// Its file's `AgLibraryFolder` record is gone, or the file's `folder` is not a whole number.
    MissingFolder,
    /// Its folder's `AgLibraryRootFolder` record is gone, or the folder's `rootFolder` is not a whole number.
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

/// The decoded length [`decode_compressed`] reads from `bytes`' header, when it has one of at most 16 MiB.
fn compressed_len(bytes: &[u8]) -> Option<usize> {
    let size = bytes.get(..4).and_then(|s| <[u8; 4]>::try_from(s).ok()).map(u32::from_be_bytes)?;
    usize::try_from(size).ok().filter(|size| *size <= 16 << 20)
}

/// Lightroom's compressed text (`Adobe_AdditionalMetadata.xmp`, and history-step and snapshot settings since Lightroom
/// Classic 13): the decoded length as 4 big-endian bytes, then a zlib stream, at most 16 MiB decoded. Errors name it
/// `what`.
fn decode_compressed(bytes: &[u8], what: &str) -> Result<String, String> {
    if bytes.len() < 4 {
        return Err(format!("truncated {what} header"));
    }
    let size = compressed_len(bytes).ok_or_else(|| format!("{what} longer than 16 MiB"))?;
    // inflated no further than its declared length: a stream that goes on is invalid, at no more cost than a valid one
    let decoded = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(bytes.get(4..).ok_or_else(|| format!("truncated {what}"))?, size)
        .map_err(|_| format!("invalid compressed {what}"))?;
    if decoded.len() != size {
        return Err(format!("{what} length mismatch"));
    }
    String::from_utf8(decoded).map_err(|e| format!("{what} not UTF-8: {e}"))
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

pub(crate) struct SourceGuard {
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

    pub(crate) fn verify(&self) -> Result<(), String> {
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
    // A Capture One catalog takes the same road from here on (the same model, job and report).
    if crate::capture_one_catalog::is_capture_one(path) {
        return crate::capture_one_catalog::read_with_progress(path, cancel, total, done);
    }
    // 14 catalog tables plus one validation stage; ImportJob adds one unit per original path.
    total.store(15, Ordering::Relaxed);
    done.store(0, Ordering::Relaxed);
    let (reader, source_guard) = open_catalog(path, cancel)?;
    read_lightroom(path, reader, source_guard, cancel, done)
}

/// Open a catalog's SQLite file (and its committed `-wal` pages) read-only, with a guard that tells
/// whether either file changed while it was read.
pub(crate) fn open_catalog(path: &Path, cancel: &AtomicBool) -> Result<(Reader, SourceGuard), String> {
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
    Ok((Reader { db }, source_guard))
}

fn read_lightroom(path: &Path, reader: Reader, source_guard: SourceGuard, cancel: &AtomicBool, done: &AtomicUsize) -> Result<CatalogImport, String> {
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
    ensure_read_active(cancel)?;
    let develops = reader.develop_settings(cancel, &mut warnings)?;
    done.fetch_add(1, Ordering::Relaxed);
    ensure_read_active(cancel)?;
    let (metadata, metadata_warnings) = match reader.xmp_table(cancel) {
        Ok(result) => result,
        Err(error) => {
            ensure_read_active(cancel)?;
            if reader.db.has_observed_source_failure() {
                return Err(error);
            }
            warnings.push(format!("skipping optional Lightroom table Adobe_AdditionalMetadata: {error}"));
            (HashMap::new(), Vec::new())
        }
    };
    warnings.extend(metadata_warnings);
    done.fetch_add(1, Ordering::Relaxed);
    let keyword_rows: HashMap<_, _> = optional_table("AgLibraryKeyword", &mut warnings)?.into_iter().map(|r| (number(&r, "id_local"), r)).collect();
    let mut keywords: HashMap<i64, Vec<String>> = HashMap::new();
    // each keyword's path is resolved once, however many photos carry it
    let mut paths: HashMap<i64, Option<String>> = HashMap::new();
    let mut keyword_bytes = 0usize;
    let keyword_images = optional_table("AgLibraryKeywordImage", &mut warnings)?;
    for (image, r) in by_image("AgLibraryKeywordImage", keyword_images, &mut warnings) {
        ensure_read_active(cancel)?;
        let tag = number(&r, "tag");
        let path = match paths.get(&tag) {
            Some(p) => p.clone(),
            None => {
                let mut parts = Vec::new();
                let mut at = tag;
                let mut seen = HashSet::new();
                while at != 0 {
                    // every ancestor visited counts towards the depth, named or not
                    if seen.len() >= 64 || !seen.insert(at) {
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
                let p = (!parts.is_empty()).then(|| parts.join("|"));
                paths.insert(tag, p.clone());
                p
            }
        };
        if let Some(p) = path {
            keyword_bytes = keyword_bytes.saturating_add(p.len());
            if keyword_bytes > MAX_KEYWORD_BYTES {
                return Err("the catalog's keyword assignments together exceed 1 GiB".into());
            }
            keywords.entry(image).or_default().push(p);
        }
    }
    let mut root_paths = HashMap::new();
    let mut refused_roots = HashSet::new();
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
        if let Err(crate::doc_path::NotFollowed::Refused(why)) = crate::doc_path::resolve(&resolved, Some(path)) {
            warnings
                .push(format!("root folder {}: {resolved} is not opened: {why}; its photos are listed as missing and not added", text(root, "name")));
            refused_roots.insert(id);
        }
        root_paths.insert(id, resolved);
    }
    ensure_read_active(cancel)?;
    let mut history = reader.grouped("Adobe_libraryImageDevelopHistoryStep", &mut warnings, cancel)?;
    done.fetch_add(1, Ordering::Relaxed);
    ensure_read_active(cancel)?;
    let mut snapshots = reader.grouped("Adobe_libraryImageDevelopSnapshot", &mut warnings, cancel)?;
    done.fetch_add(1, Ordering::Relaxed);
    let as_shot_whites = custom_as_shot(&reader, &develops, cancel, &mut warnings);
    ensure_read_active(cancel)?;
    let collections = optional_table("AgLibraryCollection", &mut warnings)?;
    // members stay as read (the archive keeps them); the ones that name no photo join no album
    let members = optional_table("AgLibraryCollectionImage", &mut warnings)?;
    let unkeyed = members.iter().filter(|m| image_of(m).is_none()).count();
    warn_unkeyed("AgLibraryCollectionImage", unkeyed, "added to no album", &mut warnings);
    let mut out = CatalogImport {
        source: path.to_string_lossy().into(),
        photos: Vec::new(),
        collections,
        members,
        collection_content: optional_table("AgLibraryCollectionContent", &mut warnings)?,
        stacks: optional_table("AgLibraryFolderStackImage", &mut warnings)?,
        warnings,
        skipped: Vec::new(),
    };
    // An image row whose file, folder or root record is gone (Lightroom leaves such rows behind, and its integrity
    // check passes them) can't be located: it is left out with a warning, and the rest of the catalog imports
    // (issue #1588). The first few are named, then only counted.
    let mut left_out = 0usize;
    let mut leave_out = |warnings: &mut Vec<String>, why: String| {
        left_out += 1;
        if left_out <= LEFT_OUT_NAMED {
            warnings.push(format!("{why}; left out"));
        }
    };
    // images whose file, folder or root record is gone, or whose path is empty (Lightroom leaves
    // these behind and ignores them); they and their virtual copies are skipped and reported
    // instead of failing the import
    let mut orphans = HashSet::new();
    for image in table("Adobe_images", true)? {
        ensure_read_active(cancel)?;
        let id = number(&image, "id_local");
        // read strictly: a NULL, text or fractional reference names no record, not record 0 (or 1 for 1.5), whose
        // path would merge this image into another photo
        let record = id_of(&image, "rootFile").and_then(|f| files.get(&f)).ok_or(SkipReason::MissingFile).and_then(|file| {
            let folder = id_of(file, "folder").and_then(|f| folders.get(&f)).ok_or(SkipReason::MissingFolder)?;
            let root = id_of(folder, "rootFolder").and_then(|r| roots.get(&r)).ok_or(SkipReason::MissingRoot)?;
            Ok((file, folder, root))
        });
        let (file, folder, root) = match record {
            Ok(found) => found,
            Err(reason) => {
                let what = match reason {
                    SkipReason::MissingFile => "file",
                    SkipReason::MissingFolder => "folder",
                    _ => "root",
                };
                leave_out(&mut out.warnings, format!("image {id}: missing {what} record"));
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
            leave_out(&mut out.warnings, format!("image {id}: incomplete original path"));
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
            as_shot_white: as_shot_whites.get(&id).copied(),
            capture_one: None,
        });
    }
    if left_out > LEFT_OUT_NAMED {
        out.warnings.push(format!("{left_out} images left out in all: their original's file, folder or root record is missing or incomplete"));
    }
    drop_copies_of(&mut out, &orphans);
    refuse_non_media(&mut out);
    ensure_read_active(cancel)?;
    source_guard.verify()?;
    Ok(out)
}

/// How many of the images left out of an import (no file, folder or root record) the warnings name one by one.
const LEFT_OUT_NAMED: usize = 20;
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

/// Drop the catalog's images whose original is not a file type the app reads, saying so in the
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
        warnings.push(format!("image {}: not imported; {} isn't one of the file types the app reads", p.source_id, p.path));
        false
    });
    data.warnings.extend(warnings);
}

pub(crate) fn ensure_read_active(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) { Err("Lightroom import cancelled".into()) } else { Ok(()) }
}

/// Lightroom's history steps (each a full settings table) → History entries, oldest first, each
/// mapped onto `base` like the photo's current settings. A step that can't be read is skipped and
/// counted. When the newest step doesn't match `current` (Lightroom records the state after the
/// last step separately), a final "Lightroom catalog" entry holds it, so History ends where the
/// photo is.
fn history_steps(
    rows: &[Row],
    base: &Arc<lcc_develop::DevelopSettings>,
    current: &Arc<lcc_develop::DevelopSettings>,
    map: impl Fn(&str) -> Result<(Value, Vec<String>), String>,
) -> (Vec<lcc_catalog::HistoryStep>, usize) {
    let rows: Vec<&Row> = rows.iter().filter(|r| !r.contains_key(OVER_DECODE_BUDGET)).collect();
    let mut ordered: Vec<&Row> = rows.iter().copied().filter(|r| !text(r, "text").is_empty()).collect();
    let skipped_empty = rows.len() - ordered.len();
    let when = |r: &Row| r.get("dateCreated").and_then(Value::as_f64).filter(|n| n.is_finite()).unwrap_or(0.0);
    ordered.sort_by(|a, b| when(a).total_cmp(&when(b)).then(number(a, "id_local").cmp(&number(b, "id_local"))));
    let mut skipped = skipped_empty;
    let mut steps: Vec<lcc_catalog::HistoryStep> = Vec::new();
    for row in ordered {
        let Ok((partial, _)) = map(text(row, "text")) else {
            skipped += 1;
            continue;
        };
        let label = match text(row, "name").trim() {
            "" => "Lightroom step".to_string(),
            n => n.chars().take(120).collect(),
        };
        let mut settings = lcc_develop::apply_partial(base, &partial, 1.0);
        crate::crs::keep_chosen_dcp(base, &mut settings);
        steps.push(lcc_catalog::HistoryStep { label, settings: Arc::new(settings), batch: None });
    }
    if !steps.is_empty() && steps.last().is_some_and(|l| l.settings != *current) {
        steps.push(lcc_catalog::HistoryStep { label: "Lightroom catalog".into(), settings: current.clone(), batch: None });
    }
    if steps.len() > lcc_catalog::HISTORY_LIMIT {
        let n = steps.len() - lcc_catalog::HISTORY_LIMIT;
        steps.drain(..n);
    }
    (steps, skipped)
}

/// Lightroom's snapshots (named full settings tables) → Versions, in Lightroom's order (by
/// `id_local`), each mapped onto `base` like the photo's current settings. A snapshot that can't
/// be read is skipped and counted; an unnamed one is called "Lightroom snapshot".
fn snapshot_versions(
    rows: &[Row],
    base: &Arc<lcc_develop::DevelopSettings>,
    now: &str,
    map: impl Fn(&str) -> Result<(Value, Vec<String>), String>,
) -> (Vec<lcc_catalog::Version>, usize) {
    let mut ordered: Vec<&Row> = rows.iter().filter(|r| !r.contains_key(OVER_DECODE_BUDGET)).collect();
    ordered.sort_by_key(|r| number(r, "id_local"));
    let mut skipped = 0;
    let mut versions = Vec::new();
    for row in ordered {
        let settings = text(row, "text");
        let Some(Ok((partial, _))) = (!settings.is_empty()).then(|| map(settings)) else {
            skipped += 1;
            continue;
        };
        let name = match text(row, "name").trim() {
            "" => "Lightroom snapshot".to_string(),
            n => n.chars().take(120).collect(),
        };
        let mut settings = lcc_develop::apply_partial(base, &partial, 1.0);
        crate::crs::keep_chosen_dcp(base, &mut settings);
        versions.push(lcc_catalog::Version { name, created: now.to_string(), settings: Arc::new(settings), auto: false });
    }
    (versions, skipped)
}

fn mapped_settings(
    text: &str,
    target: crate::crs::Target,
    aspect: f64,
    orientation: Orientation,
    stored_aspect: f64,
) -> Result<(Value, Vec<String>), String> {
    mapped_settings_from(text, None, target, aspect, orientation, stored_aspect)
}

/// [`mapped_settings`] with `as_shot`, the white a Custom Kelvin is measured from, when the catalog's history gave one.
fn mapped_settings_from(
    text: &str,
    as_shot: Option<(f64, f64)>,
    target: crate::crs::Target,
    aspect: f64,
    orientation: Orientation,
    stored_aspect: f64,
) -> Result<(Value, Vec<String>), String> {
    let mut root = crate::preset_import::parse_lua(text)?;
    let deferred = strip_lua_sentinels(&mut root);
    // A catalog's develop settings carry no `HasCrop`: `crs::to_partial` takes any crop field as a crop.
    let (mut props, values) = crate::preset_import::lua_settings_props(&root)?;
    if let Some((temperature, tint)) = as_shot {
        props.entry("crs:AsShotTemperature".into()).or_insert_with(|| vec![temperature.to_string()]);
        props.entry("crs:AsShotTint".into()).or_insert_with(|| vec![tint.to_string()]);
    }
    // Lightroom's spots and mask shapes are on the stored image, before the photo's orientation: on a turned photo
    // they are read in that frame (spot sizes and radials fitted to it), then turned into the app's
    let turned = orientation != Orientation::Normal && stored_aspect.is_finite() && stored_aspect > 0.0;
    let frame = if turned { stored_aspect } else { aspect };
    let (mut partial, mut unknown) = crate::crs::to_partial_report(&props, Some(&values), target, frame);
    if turned {
        crate::crs_masks::orient_geometry(&mut partial, orientation, stored_aspect);
    }
    catalog_crop(&mut partial, orientation, stored_aspect);
    if deferred > 0 {
        unknown.push("Deferred Lightroom adjustments (-999999)".into());
    }
    Ok((partial, unknown))
}

/// The `(Temperature, Tint)` of a history step whose white balance is As Shot. Lightroom writes them there as it read
/// them from the file, and keeps them nowhere else once the photo has a Custom white balance. `None` for any other
/// step, and for a photo's first steps, which say As Shot without the values.
fn step_as_shot_white(text: &str) -> Option<(f64, f64)> {
    let props = settings_with_white_balance(text, "As Shot")?;
    let number = |k: &str| props.get(k)?.first()?.parse::<f64>().ok().filter(|v| v.is_finite());
    Some((number("crs:Temperature").filter(|t| *t > 0.0)?, number("crs:Tint")?))
}

/// The parsed develop settings in `text` when their white balance is `mode`; `None` for another mode or text that
/// does not parse. The substring test only skips parsing text that cannot match.
fn settings_with_white_balance(text: &str, mode: &str) -> Option<crate::crs::Props> {
    if !text.contains(mode) {
        return None;
    }
    let root = crate::preset_import::parse_lua(text).ok()?;
    let (props, _) = crate::preset_import::lua_settings_props(&root).ok()?;
    (props.get("crs:WhiteBalance")?.first()?.as_str() == mode).then_some(props)
}

/// The as-shot white of each photo with a Custom white balance (issue #510: without it a raw developed relative to
/// its as-shot look can't take the Kelvin). Optional: what was read before a failure is kept, with a warning.
fn custom_as_shot(reader: &Reader, develops: &HashMap<i64, String>, cancel: &AtomicBool, warnings: &mut Vec<String>) -> HashMap<i64, (f64, f64)> {
    let wanted: HashSet<i64> = develops.iter().filter(|(_, t)| settings_with_white_balance(t, "Custom").is_some()).map(|(id, _)| *id).collect();
    let mut found = HashMap::new();
    if !wanted.is_empty()
        && let Err(error) = reader.as_shot_whites(&wanted, cancel, &mut found)
        && !cancel.load(Ordering::Relaxed)
    {
        warnings.push(format!("as-shot white balance from Lightroom history: {error}"));
    }
    found
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

/// Lightroom's crop in the app's frame. `crs` gives the crop's top-left (`CropLeft`, `CropTop`)
/// and bottom-right (`CropRight`, `CropBottom`) corners on the stored image (before orientation and
/// straightening), normalized to its size, plus a straighten angle that turns the other way to
/// The app's. The app's crop is a rectangle in the oriented image rotated by `angle` about its
/// centre, normalized to the oriented image's size, so the corners go through the orientation and
/// then the rotation; `stored_aspect` (width / height of the stored image) matters once the
/// rectangle turns. On a real catalog this reproduced the frame aspect of all 223 straightened crops
/// in Lightroom's exports within 1%.
pub(crate) fn catalog_crop(partial: &mut Value, orientation: Orientation, stored_aspect: f64) {
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
    let archive_path = archive_dir
        .as_ref()
        .map(|dir| crate::lightroom_archive::store(dir, &data).map(|stored| stored.path).map_err(|e| error(e.to_string())))
        .transpose()?;
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
        lcc_catalog::safe_file::write_atomic(&path, &bytes).map_err(|e| error(e.to_string()))?;
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

/// The catalog's collections with each id once, in catalog order. A row repeated as it is drops quietly; a different
/// collection with an id an earlier one has is left out with a warning naming both, since two albums can't have one id
/// (#1588: one bad Lightroom record costs that record, not the import). The photos listed under the id go to the first.
fn first_of_each_collection<'a>(data: &'a CatalogImport, warnings: &mut Vec<String>) -> Vec<&'a Row> {
    let mut first: HashMap<i64, &Row> = HashMap::new();
    let mut rows = Vec::new();
    for collection in &data.collections {
        let id = number(collection, "id_local");
        let Some(kept) = first.get(&id) else {
            first.insert(id, collection);
            rows.push(collection);
            continue;
        };
        if *kept != collection {
            let photos = if data.members.iter().any(|m| id_of(m, "collection") == Some(id)) {
                format!("; the photos listed under that id go to “{}”", text(kept, "name"))
            } else {
                String::new()
            };
            warnings.push(format!(
                "collection “{}”: not imported, it has the id of collection “{}”{photos}",
                text(collection, "name"),
                text(kept, "name")
            ));
        }
    }
    rows
}

/// Turn the read catalog into library records in one undo step and return the import report
/// (added, missing, failed, warnings and skipped images).
pub(crate) fn apply_prepared(s: &mut crate::Session, mut data: CatalogImport, context: ApplyPrepared<'_>) -> crate::Result<AppliedImport> {
    let ApplyPrepared { update_existing, files, index, archive_path, missing_sources, undo0 } = context;
    // nothing but a file type the app reads becomes a record, whichever route got here (#2135)
    refuse_non_media(&mut data);
    validate(&data).map_err(error)?;
    let mut warnings = data.warnings.clone();
    // Records this import has just added, or promoted from Local untouched since browsing, hold
    // no edits of the library's own, even when adding or browsing read develop settings from the
    // file's XMP: the catalog's settings apply to them.
    let changed: HashSet<_> = files.promoted_changed.iter().collect();
    let added: HashSet<_> = files.imported.iter().filter(|id| !changed.contains(id)).copied().map(PhotoId).collect();
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
    // Capture One white balances applied on the relative scale (`crate::capture_one_develop`): approximate
    let mut capture_one_wb_approximate = 0usize;
    let mut preserved = 0;
    let mut history_imported = 0usize;
    let mut snapshots_imported = 0usize;
    // images dated at the import, or left undated, because their Lightroom edit time can't be read
    let mut untimed = Vec::new();
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
        // virtual copies go with it. (`masterImage` is 0 for a master, so a refused id of 0, from
        // a malformed catalog, must not refuse every master.)
        if refused.contains(&src.source_id) || (master != 0 && refused.contains(&master)) {
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
                p.kind = lcc_catalog::MediaKind::Raw;
            }
            p
        };
        // Lightroom gives any photo a Copy Name, not only a virtual copy (some use it as a short title).
        let copy_name = text(&src.image, "copyName");
        p.copy_name = if master != 0 { Some(copy_name.to_string()) } else { Some(copy_name.to_string()).filter(|n| !n.is_empty()) };
        // Offline videos bypass file probing; Lightroom's format still identifies their kind.
        if text(&src.image, "fileFormat").eq_ignore_ascii_case("VIDEO") {
            p.kind = lcc_catalog::MediaKind::Video;
        }
        ids.insert(src.source_id, p.id);
        index.photos.insert(identity(&data, src), p.id.0);
        // A record this import added, or a Local one untouched since browsing (an offline original
        // never reaches the add step, so it is judged here), holds no edits of the library's own.
        let fresh = existing_id.is_some_and(|id| added.contains(&id) || s.catalog.photo(id).is_some_and(|q| q.untouched_local()));
        // Every photo Lightroom has carries its edit date (below), so a date alone doesn't say the library edited it:
        // its settings or its History do.
        let library_edits = existing_id.is_some() && !fresh && p.edited.is_some() && (p.is_edited() || !p.history.is_empty());
        let keep_develop = library_edits && !update_existing;
        let touched = touch_time(&src.image);
        // dated from the catalog's develop settings (below); otherwise from `touched` alone
        let mut dated = false;
        let previous_develop = (p.develop.clone(), p.edited.clone());
        if keep_develop {
            preserved += 1;
        }
        let orientation = lightroom_orientation(text(&src.image, "orientation")).unwrap_or_default();
        let stored_aspect = number(&src.image, "fileWidth") as f64 / number(&src.image, "fileHeight") as f64;
        // Lightroom leaves some settings at zero (Clarity, Texture) out of its develop text, so the
        // catalog's settings start from the camera's defaults, not from what the file's XMP (or a
        // default import preset) gave the photo. With no develop text, or one that can't be read,
        // there is nothing to replace them with.
        if fresh
            && !src.settings.is_empty()
            && mapped_settings(
                &src.settings,
                crate::crs::Target::for_photo(&p),
                p.width.max(1) as f64 / p.height.max(1) as f64,
                orientation,
                stored_aspect,
            )
            .is_ok()
        {
            p.develop = Arc::new(p.camera_defaults());
        }
        // a Capture One variant's settings (partial: what it uses) start from the camera's defaults too
        if fresh && src.settings.is_empty() && src.capture_one.as_deref().is_some_and(|c| !c.is_empty()) {
            p.develop = Arc::new(p.camera_defaults());
        }
        if !src.xmp.is_empty() {
            match crate::sidecar::parse_sidecar(&src.xmp, crate::crs::Target::for_photo(&p)) {
                Ok(mut sc) => {
                    if let Some(crate::sidecar::DevelopPatch::Partial(partial)) = &mut sc.develop {
                        strip_xmp_sentinels(partial);
                        // its crop, spots and masks are on the stored image, as in the develop text
                        let photo_aspect = f64::from(p.width) / f64::from(p.height);
                        crate::sidecar::orient_lightroom_patch(partial, orientation, stored_aspect, photo_aspect);
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
        fill_camera(&mut p.meta, &src.image);
        // A capture time the library can't hold would fail the whole batch at `SetCaptured`, and an
        // impossible date (2026-02-30) would roll over to another day in date grouping: it is left
        // out with a warning (the catalog's value stays in the import archive). A year or a month
        // alone (`1983`, `1983-04`: a scanned print's date, from its XMP) is kept as stated.
        let captured = text(&src.image, "captureTime");
        let is_date = |c: &str| lcc_catalog::dates::normalize_iso(&lcc_catalog::dates::capture_start(c)).is_some();
        if is_date(captured) {
            p.captured = Some(captured.into());
        } else if !captured.is_empty() {
            warnings.push(format!("image {}: capture time “{captured}” is not a date; imported without it", src.source_id));
        }
        // a new record (a virtual copy, a duplicate) doesn't inherit a time an earlier import stored unchecked
        if s.catalog.photo(p.id).is_none()
            && let Some(inherited) = p.captured.take_if(|c| !is_date(c))
        {
            warnings.push(format!("image {}: inherited capture time “{inherited}” is not a date; imported without it", src.source_id));
        }
        if !src.keywords.is_empty() {
            p.meta.keywords = src.keywords.clone();
        }
        if !src.settings.is_empty() && !keep_develop {
            let target = crate::crs::Target::for_photo(&p);
            let aspect = p.width.max(1) as f64 / p.height.max(1) as f64;
            let base = p.develop.clone();
            match mapped_settings_from(&src.settings, src.as_shot_white, target, aspect, orientation, stored_aspect) {
                Ok((partial, unknown)) => {
                    let mut develop = lcc_develop::apply_partial(&base, &partial, 1.0);
                    crate::crs::keep_chosen_dcp(&base, &mut develop);
                    p.develop = Arc::new(develop);
                    if touched.is_none() {
                        untimed.push(src.source_id);
                    }
                    p.edited = Some(touched.clone().unwrap_or_else(|| now.clone()));
                    dated = true;
                    if !unknown.is_empty() {
                        unmapped.insert(src.source_id, unknown);
                    }
                }
                Err(e) => warnings.push(format!("image {} settings: {e}", src.source_id)),
            }
            let (steps, skipped) = history_steps(&src.history, &base, &p.develop, |t| mapped_settings(t, target, aspect, orientation, stored_aspect));
            if skipped > 0 {
                warnings.push(format!("image {}: {skipped} Lightroom history step(s) could not be read; left out of History", src.source_id));
            }
            if !steps.is_empty() {
                history_imported += steps.len();
                p.history = steps;
            }
            let (versions, skipped) =
                snapshot_versions(&src.snapshots, &base, &now, |t| mapped_settings(t, target, aspect, orientation, stored_aspect));
            if skipped > 0 {
                warnings.push(format!("image {}: {skipped} Lightroom snapshot(s) could not be read; left out of Versions", src.source_id));
            }
            if !versions.is_empty() {
                snapshots_imported += versions.len();
                p.versions = versions;
            }
        } else if let Some(c1) = src.capture_one.as_deref()
            && !keep_develop
        {
            // what the variant uses that isn't imported is reported for it, settings applied or not
            let target = crate::crs::Target::for_photo(&p);
            let (partial, unknown) = crate::capture_one_develop::partial(c1, target, p.as_shot_wb);
            if target == crate::crs::Target::RawRelative && partial.get("wb").is_some() {
                capture_one_wb_approximate = capture_one_wb_approximate.saturating_add(1);
            }
            if partial.as_object().is_some_and(|o| !o.is_empty()) {
                p.develop = Arc::new(lcc_develop::apply_partial(&p.develop, &partial, 1.0));
                if touched.is_none() {
                    untimed.push(src.source_id);
                }
                p.edited = Some(touched.clone().unwrap_or_else(|| now.clone()));
                dated = true;
            }
            if !unknown.is_empty() {
                unmapped.insert(src.source_id, unknown);
            }
        }
        // Lightroom dates every photo at its last edit, settings or not (or settings that can't be read); one edited
        // in the library keeps its own date.
        if !dated && !library_edits {
            match &touched {
                Some(t) => p.edited = Some(t.clone()),
                None => untimed.push(src.source_id),
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
        if let Some(old) = s.catalog.photo(p.id) {
            if old.kind != p.kind || old.copy_name != p.copy_name {
                // Repair older imports, and bring a changed Copy Name across (no op sets one), with existing
                // reversible operations. Removing just the record leaves album/stack membership intact; the
                // enclosing batch is atomic. The record's cached previews go with it, so they render again.
                let mut repaired = (**old).clone();
                repaired.kind = p.kind;
                repaired.copy_name = p.copy_name.clone();
                ops.extend([Op::RemovePhoto { id: p.id }, Op::AddPhoto { photo: Box::new(repaired) }]);
            }
            if was_local && promoted.insert(p.id) {
                imported += 1;
                ops.push(Op::SetLocal { id: p.id, local: false });
            }
            ops.extend([
                Op::SetRating { id: p.id, rating: p.rating },
                Op::SetFlag { id: p.id, flag: p.flag },
                Op::SetLabel { id: p.id, label: p.label },
                Op::SetMeta { id: p.id, meta: Box::new(p.meta.clone()) },
            ]);
            // A record an earlier import gave a time that isn't a date keeps it: `SetCaptured` refuses
            // such a time, so setting it would fail the batch, and replacing it would leave an undo
            // that can't put it back.
            let settable = |c: Option<&str>| c.is_none_or(lcc_catalog::dates::is_capture_time);
            let stored = s.catalog.photo(p.id).and_then(|old| old.captured.clone());
            if settable(stored.as_deref()) && settable(p.captured.as_deref()) {
                ops.push(Op::SetCaptured { id: p.id, captured: p.captured.clone() });
            } else if let (Some(stored), true) = (&stored, p.captured != stored) {
                warnings.push(format!(
                    "image {}: the library's capture time “{stored}” is not a date, so Lightroom's was not applied (undoing the import could not restore it)",
                    src.source_id
                ));
            }
            ops.push(Op::SetDevelop { id: p.id, settings: p.develop.clone(), label: "Lightroom catalog".into(), edited: p.edited.clone() });
            if !keep_develop && !src.history.is_empty() {
                ops.push(Op::SetHistory { id: p.id, history: p.history.clone() });
            }
            if !keep_develop && !src.snapshots.is_empty() {
                ops.push(Op::SetVersions { id: p.id, versions: p.versions.clone() });
            }
        } else {
            imported += 1;
            ops.push(Op::AddPhoto { photo: Box::new(p) });
        }
        by_path.entry(key(&src.path)).or_insert(photo_id);
    }
    if capture_one_wb_approximate > 0 {
        warnings.push(format!(
            "Capture One White Balance: approximate on {capture_one_wb_approximate} photo(s) — their raws are edited on the \
             relative white balance scale, where Capture One's change can only be approximated"
        ));
    }
    if let Some(first) = untimed.first() {
        let named: Vec<String> = untimed.iter().take(LEFT_OUT_NAMED).map(i64::to_string).collect();
        let more = untimed.len() - named.len();
        let more = if more > 0 { format!(" and {more} more") } else { String::new() };
        let which = if untimed.len() == 1 { format!("image {first}") } else { format!("images {}{more}", named.join(", ")) };
        warnings.push(format!(
            "{which}: Lightroom's last edit time is missing or not a date; photos with develop settings are dated at the import, the others keep any edit date they already had"
        ));
    }
    let rows = first_of_each_collection(&data, &mut warnings);
    let mut albums = HashMap::new();
    // Two collections never share an album. An index that names one album for two gives the second a new one; the
    // counter saturates, so a damaged one hands out an id that's taken, and that stops the import instead.
    let mut assigned = HashSet::new();
    for &collection in &rows {
        if number(collection, "systemOnly") != 0 {
            continue;
        }
        let collection_key = format!("{}:{}", key(&data.source), number(collection, "id_local"));
        let indexed = index
            .collections
            .get(&collection_key)
            .copied()
            .map(lcc_catalog::AlbumId)
            .filter(|id| s.catalog.album(*id).is_some() && !assigned.contains(id));
        let id = match indexed {
            Some(id) => id,
            None => {
                let id = s.catalog.alloc_album_id();
                if s.catalog.album(id).is_some() || assigned.contains(&id) {
                    return Err(error(format!(
                        "collection “{}”: the library has no unused album id left for it (its album numbering is exhausted or damaged)",
                        text(collection, "name")
                    )));
                }
                id
            }
        };
        assigned.insert(id);
        index.collections.insert(collection_key, id.0);
        albums.insert(number(collection, "id_local"), id);
    }
    let parents: HashMap<_, _> = rows.iter().map(|r| (number(r, "id_local"), number(r, "parent"))).collect();
    let collections: HashMap<_, _> = rows.iter().map(|r| (number(r, "id_local"), *r)).collect();
    // Collections whose albums are folders: a collection set, or an earlier import's album when it is a folder.
    let sets: HashSet<i64> = albums
        .iter()
        .filter(|(lr, id)| match s.catalog.album(**id) {
            Some(album) => album.folder,
            None => collections.get(*lr).is_some_and(|r| is_collection_set(r)),
        })
        .map(|(lr, _)| *lr)
        .collect();
    let mut added_folders = HashSet::new();
    let mut grouped_members: HashMap<i64, Vec<&Row>> = HashMap::new();
    // read strictly: a NULL, text or fractional `collection` names no collection, not collection 0 (or 1 for 1.5)
    for member in &data.members {
        if let Some(collection) = id_of(member, "collection") {
            grouped_members.entry(collection).or_default().push(member);
        }
    }
    match data.members.iter().filter(|m| id_of(m, "collection").is_none()).count() {
        0 => {}
        1 => warnings.push("AgLibraryCollectionImage: 1 record added to no album; its collection is not a collection id".into()),
        n => warnings.push(format!("AgLibraryCollectionImage: {n} records added to no album; their collection is not a collection id")),
    }
    for members in grouped_members.values_mut() {
        members.sort_by(|a, b| text(a, "positionInCollection").cmp(text(b, "positionInCollection")));
    }
    let smart_content: HashMap<i64, &str> = data
        .collection_content
        .iter()
        .filter(|r| text(r, "owningModule") == "ag.library.smart_collection")
        .map(|r| (number(r, "collection"), text(r, "content")))
        .collect();
    let print_content: HashMap<i64, &str> = data
        .collection_content
        .iter()
        .filter(|r| text(r, "owningModule") == "com.adobe.ag.print")
        .map(|r| (number(r, "collection"), text(r, "content")))
        .collect();
    let mut saved_prints = 0usize;
    // a saved print's layout as a Saved Print, or why it stays a plain collection (the original is in the archive)
    let mut print_layout = |collection: &Row, name: &str, members: &[PhotoId], warnings: &mut Vec<String>| {
        let found = print_content.get(&number(collection, "id_local")).ok_or_else(|| "the catalog has no layout for it".to_string());
        match found.and_then(|c| crate::lightroom_print::saved_print(c, &|image| ids.get(&image).copied(), members)) {
            Ok(print) => {
                saved_prints += 1;
                warnings.extend(print.notes.into_iter().map(|n| format!("{name}: saved print: {n}")));
                Some(lcc_catalog::Creation { kind: "print".into(), document: print.document })
            }
            Err(why) => {
                warnings.push(format!("{name}: its print layout isn't imported: {why}; imported as a plain collection (layout kept in archive)"));
                None
            }
        }
    };
    // Lightroom's Collection rule matches ordinary collections only: never a set, nor a smart collection (so a smart
    // collection never matches itself, nor one whose rules match it back).
    let named_collections: Vec<(&str, lcc_catalog::AlbumId)> = data
        .collections
        .iter()
        .filter(|r| !is_collection_set(r) && !is_smart_collection(r))
        .filter_map(|r| Some((text(r, "name"), albums.get(&number(r, "id_local")).copied()?)))
        .collect();
    let (mut smart_albums, mut static_smart, mut smart_kept) = (0usize, 0usize, 0usize);
    // the albums each smart album added in this batch tests, which the library doesn't know yet
    let mut queued_smart: HashMap<lcc_catalog::AlbumId, Vec<lcc_catalog::AlbumId>> = HashMap::new();
    let mut ordered = rows.clone();
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
        album.folder = is_collection_set(collection);
        let is_smart = is_smart_collection(collection);
        let rules = if is_smart {
            Some(match smart_content.get(&number(collection, "id_local")) {
                Some(c) => crate::lightroom_smart::smart_rules(c, &named_collections),
                None => Err("smart rules missing".into()),
            })
        } else {
            None
        };
        let members: Vec<PhotoId> =
            grouped_members.get(&number(collection, "id_local")).into_iter().flatten().filter_map(|r| ids.get(&image_of(r)?).copied()).collect();
        // Reimport branches on the existing album's kind: its name/hierarchy and (for a smart album)
        // its possibly user-edited rules are kept; a regular album only gains new members.
        let existing = s.catalog.album(id).map(|a| (a.folder, a.is_smart(), a.photos.clone()));
        // an earlier import's plain album of a saved print gains its layout; one that has a layout keeps it
        let gains_layout = is_saved_print(collection) && s.catalog.album(id).is_some_and(|a| !a.folder && !a.is_smart() && a.creation.is_none());
        match existing {
            Some((true, _, _)) => {}
            Some((false, true, _)) => {
                if is_smart {
                    smart_albums += 1;
                    smart_kept += 1;
                }
            }
            Some((false, false, old)) => {
                if let Some(rules) = &rules {
                    static_smart += 1;
                    match rules {
                        Ok(_) => warnings.push(format!(
                            "{}: an earlier import created a static album; it stays static (delete it and reimport to get a smart album)",
                            album.name
                        )),
                        Err(why) => warnings.push(format!("{}: {why}; imported current membership (rules kept in archive)", album.name)),
                    }
                }
                let mut photos = old;
                let mut seen: HashSet<_> = photos.iter().copied().collect();
                for photo in members {
                    if seen.insert(photo) {
                        photos.push(photo);
                    }
                }
                if gains_layout && let Some(creation) = print_layout(collection, &album.name, &photos, &mut warnings) {
                    ops.push(Op::SetAlbumCreation { id, creation: Some(creation) });
                }
                ops.push(Op::SetAlbumPhotos { id, photos });
            }
            None => {
                // `Catalog::apply_new` refuses an album that would include itself, and that would roll back the whole
                // batch: such a collection keeps its current membership instead. The rules test ordinary collections
                // only, so only a library album that already tests this new id (a damaged library) can close a loop.
                let rules = rules.map(|r| {
                    r.and_then(|rules| {
                        let tested = rules.albums_tested();
                        if leads_to(s, &queued_smart, tested.clone(), id) {
                            Err("its rules would make it include itself through the albums they test".into())
                        } else {
                            queued_smart.insert(id, tested);
                            Ok(rules)
                        }
                    })
                });
                match rules {
                    Some(Ok(rules)) => {
                        album.smart = Some(Box::new(lcc_catalog::Filter { rule_set: Some(rules), ..Default::default() }));
                        smart_albums += 1;
                    }
                    Some(Err(why)) => {
                        warnings.push(format!("{}: {why}; imported current membership (rules kept in archive)", album.name));
                        static_smart += 1;
                    }
                    None => {}
                }
                if !album.folder && !album.is_smart() {
                    album.photos = members;
                    if is_saved_print(collection) {
                        album.creation = print_layout(collection, &album.name, &album.photos, &mut warnings);
                    }
                }
                let (set, moved) = collection_parent(collection, &collections, &sets);
                album.parent = set.and_then(|set| albums.get(&set).copied());
                // `AddAlbum` rejects a parent that isn't a folder yet, and that would roll back the whole batch: every
                // album and every catalog-only photo. `collection_parent` already prevents it; this keeps a bug there
                // (or a cycle `validate` missed) from doing so.
                if let Some(parent) = album.parent
                    && !added_folders.contains(&parent)
                    && !s.catalog.album(parent).is_some_and(|a| a.folder)
                {
                    album.parent = None;
                    warnings.push(format!(
                        "{}: its collection set could not be created first; imported at the top level (Lightroom collection {})",
                        album.name,
                        number(collection, "id_local")
                    ));
                } else if moved {
                    let lr_parent = number(collection, "parent");
                    let why = match collections.get(&lr_parent) {
                        None => "its parent collection isn't in the catalog".to_string(),
                        Some(r) => match albums.get(&lr_parent).and_then(|id| s.catalog.album(*id)) {
                            // a set imported earlier whose album is no longer a folder
                            Some(a) if is_collection_set(r) => {
                                format!("its collection set “{}” is the album “{}” in this library, which isn't a folder", text(r, "name"), a.name)
                            }
                            _ => format!("Lightroom keeps it inside “{}”, which isn't a collection set", text(r, "name")),
                        },
                    };
                    // the set's name in this library: the user may have renamed it since an earlier import
                    let to = match (album.parent.and_then(|id| s.catalog.album(id)), set.and_then(|set| collections.get(&set))) {
                        (Some(a), _) => format!("into “{}”", a.name),
                        (None, Some(r)) => format!("into “{}”", text(r, "name")),
                        (None, None) => "at the top level".to_string(),
                    };
                    // the Lightroom id tells apart collections that share a name (#2165)
                    warnings.push(format!("{}: {why}; imported {to} (Lightroom collection {})", album.name, number(collection, "id_local")));
                }
                if album.folder {
                    added_folders.insert(id);
                }
                ops.push(Op::AddAlbum { album });
            }
        }
    }
    let stacks = stack_ops(s, &data, &ids, &mut ops, &mut warnings);
    // the undo step names the program the catalog comes from
    let step =
        if crate::capture_one_catalog::is_capture_one(Path::new(&data.source)) { "Import Capture One Catalog" } else { "Import Lightroom Catalog" };
    if !ops.is_empty() {
        s.commit(step, Op::Batch { ops })?;
    }
    s.merge_undo(s.undo.len().saturating_sub(undo0), step);
    Ok(AppliedImport {
        report: json!({"source":data.source,"photos":ids.len(),"imported":imported,"library":s.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.to_string_lossy()),"collections":albums.len(),"smartAlbums":smart_albums,"smartAsStatic":static_smart,"smartKept":smart_kept,"savedPrints":saved_prints,"stacks":stacks,"historySteps":history_imported,"snapshots":snapshots_imported,"preservedExistingEdits":preserved,"missing":missing,"failed":files.failed,"warnings":warnings,"skipped":data.skipped,"unmapped":unmapped,"archive":archive_path,"mapping":ids}),
        index: index.clone(),
    })
}

/// The camera and lens a catalog row names (`cameraMake`, `cameraModel`, `lens`: the Capture One
/// read has them; Lightroom's rows don't), for what the photo's file didn't give: an offline
/// original is never read, a file that was read keeps its own.
fn fill_camera(m: &mut lcc_catalog::Meta, row: &Row) {
    let mut filled = false;
    for (key, field) in [("cameraMake", &mut m.camera_make), ("cameraModel", &mut m.camera_model), ("lens", &mut m.lens)] {
        let value = text(row, key).trim();
        if field.is_empty() && !value.is_empty() {
            *field = value.to_string();
            filled |= key != "lens";
        }
    }
    if filled && m.camera.is_empty() {
        m.camera = lcc_catalog::camera_label(&m.camera_make, &m.camera_model);
    }
}

/// Whether albums `from` test album `to`, directly or through the smart albums they test: those of the library, and
/// `queued`, the smart albums this import adds (each with the albums it tests). Loops already there end the search.
fn leads_to(
    s: &crate::Session,
    queued: &HashMap<lcc_catalog::AlbumId, Vec<lcc_catalog::AlbumId>>,
    from: Vec<lcc_catalog::AlbumId>,
    to: lcc_catalog::AlbumId,
) -> bool {
    let mut seen = HashSet::new();
    let mut next = from;
    while let Some(a) = next.pop() {
        if a == to {
            return true;
        }
        if !seen.insert(a) {
            continue;
        }
        match queued.get(&a) {
            Some(tested) => next.extend(tested.iter().copied()),
            None => next.extend(s.catalog.album(a).and_then(|al| al.smart.as_deref()).map(lcc_catalog::Filter::albums_tested).unwrap_or_default()),
        }
    }
    false
}

/// Lightroom folder stacks → stacks, top (position 1) first. Photos already stacked in the library
/// keep their stack; a stack that would have fewer than two photos is skipped.
fn stack_ops(s: &mut crate::Session, data: &CatalogImport, ids: &HashMap<i64, PhotoId>, ops: &mut Vec<Op>, warnings: &mut Vec<String>) -> usize {
    let mut groups: BTreeMap<i64, Vec<(f64, PhotoId, bool)>> = BTreeMap::new();
    for r in &data.stacks {
        let Some(photo) = image_of(r).and_then(|image| ids.get(&image).copied()) else { continue };
        let position = r.get("position").and_then(Value::as_f64).filter(|p| p.is_finite()).unwrap_or(f64::MAX);
        groups.entry(number(r, "stack")).or_default().push((position, photo, number(r, "collapsed") != 0));
    }
    let mut taken: HashSet<PhotoId> = HashSet::new();
    let mut made = 0;
    for (stack, mut members) in groups {
        members.sort_by(|a, b| a.0.total_cmp(&b.0));
        let collapsed = members.iter().any(|m| m.2);
        let stacked = members.iter().any(|m| s.catalog.stack_of(m.1).is_some());
        let photos: Vec<PhotoId> = members.into_iter().map(|m| m.1).filter(|p| s.catalog.stack_of(*p).is_none() && taken.insert(*p)).collect();
        if stacked && photos.len() < 2 {
            continue; // already stacked by an earlier import
        }
        if photos.len() < 2 {
            warnings.push(format!("stack {stack}: fewer than two photos left to stack; skipped"));
            continue;
        }
        let id = s.catalog.alloc_stack_id();
        ops.push(Op::AddStack { stack: lcc_catalog::Stack { id, photos, collapsed } });
        made += 1;
    }
    made
}

/// The collection set a Lightroom collection's album goes into (`None`: the top level), and whether that isn't its
/// Lightroom parent. An imported album nests only in a folder, but Lightroom also keeps a print, book, slideshow or
/// web gallery (or, in an odd catalog, a collection) inside a plain or smart collection, and a parent can be
/// system-only (not imported) or gone from the catalog. Such an album joins the nearest collection set above it.
fn collection_parent(collection: &Row, collections: &HashMap<i64, &Row>, sets: &HashSet<i64>) -> (Option<i64>, bool) {
    let mut at = number(collection, "parent");
    let mut seen = HashSet::from([number(collection, "id_local")]);
    while at != 0 && !sets.contains(&at) {
        // `validate` rejects cycles and deep chains; stop at the top level if one got here anyway.
        if seen.len() > 64 || !seen.insert(at) {
            return (None, true);
        }
        at = collections.get(&at).map_or(0, |r| number(r, "parent"));
    }
    ((at != 0).then_some(at), at != number(collection, "parent"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A catalog's camera and lens fill only what the file left empty, and a Lightroom row (which
    /// names none) changes nothing.
    #[test]
    fn a_catalogs_camera_fills_only_what_the_file_left_empty() {
        let row: Row = serde_json::from_value(json!({"cameraMake": "EXAMPLE CORP", "cameraModel": " EXAMPLE X1 ", "lens": "EXAMPLE 50mm"})).unwrap();
        let mut offline = lcc_catalog::Meta::default();
        fill_camera(&mut offline, &row);
        assert_eq!(
            (offline.camera_make.as_str(), offline.camera_model.as_str(), offline.lens.as_str()),
            ("EXAMPLE CORP", "EXAMPLE X1", "EXAMPLE 50mm")
        );
        assert_eq!(offline.camera, lcc_catalog::camera_label("EXAMPLE CORP", "EXAMPLE X1"));
        let mut read = lcc_catalog::Meta {
            camera: "OTHER OT-2".into(),
            camera_make: "OTHER".into(),
            camera_model: "OT-2".into(),
            lens: "Other 35mm".into(),
            ..Default::default()
        };
        let before = read.clone();
        fill_camera(&mut read, &row);
        assert_eq!(read, before, "the file's own camera wins");
        let mut lightroom = lcc_catalog::Meta::default();
        fill_camera(&mut lightroom, &Row::new());
        assert_eq!(lightroom, lcc_catalog::Meta::default());
    }
    use lcc_geom::CropGeometry;

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

    /// A reference names a record only by a whole number: never record 0 for a missing, NULL or text value, nor
    /// record 1 for 1.5, nor a saturated id for a value past the `i64` range.
    #[test]
    fn a_reference_names_a_record_only_by_a_whole_number() {
        let r = row(json!({
            "int": 7, "zero": 0, "negative": -3, "real": 7.0, "fraction": 7.5, "text": "7", "null": null,
            "past_f64": 9_007_199_254_740_993_i64, "edge": 9_223_372_036_854_775_808.0, "too_big": 1e19, "too_small": -1e19
        }));
        for (key, want) in [
            ("int", Some(7)),
            ("zero", Some(0)),
            ("negative", Some(-3)),
            ("real", Some(7)),
            ("fraction", None),
            ("text", None),
            ("null", None),
            ("missing", None),
            ("past_f64", Some(number(&r, "past_f64"))),
            ("edge", None),
            ("too_big", None),
            ("too_small", None),
        ] {
            assert_eq!(id_of(&r, key), want, "{key}");
        }
    }
    fn sample() -> CatalogImport {
        let mut photo = CatalogPhoto {
            source_id: 1,
            uuid: "test-master".into(),
            path: "/missing/test.ARW".into(),
            image: row(json!({"id_local":1,"rating":5,"pick":1,"fileFormat":"ARW","fileWidth":20,"fileHeight":10,"touchTime":814_504_925.0})),
            settings: "s = { Exposure2012 = 1.25, Contrast2012 = 17, CameraProfile = \"Example\" }".into(),
            xmp: String::new(),
            keywords: vec!["Travel|Goa".into()],
            history: vec![row(json!({"text":"s = { Exposure2012 = 0.5 }"}))],
            snapshots: Vec::new(),
            original: Original::Local,
            as_shot_white: None,
            capture_one: None,
        };
        let mut copy = photo.clone();
        copy.source_id = 2;
        copy.uuid = "test-copy".into();
        copy.image = row(json!({"id_local":2,"masterImage":1,"copyName":"Mono","rating":2,"pick":-1,"touchTime":814_504_925.0}));
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
            stacks: Vec::new(),
            warnings: Vec::new(),
            skipped: Vec::new(),
        }
    }
    /// Every flat `crs:` develop field named in Adobe's public Camera Raw XMP documentation (no
    /// structured fields: masks, spots, curves and looks have their own tests). Coverage report:
    /// the ones a catalog import maps, and the ones it drops because the app has no equivalent.
    const PUBLIC_CRS_FIELDS: &[&str] = &[
        "Exposure2012",
        "Contrast2012",
        "Highlights2012",
        "Shadows2012",
        "Whites2012",
        "Blacks2012",
        "Texture",
        "Clarity2012",
        "Dehaze",
        "Vibrance",
        "Saturation",
        "WhiteBalance",
        "Temperature",
        "Tint",
        "IncrementalTemperature",
        "IncrementalTint",
        "ConvertToGrayscale",
        "HDREditMode",
        "SDRBrightness",
        "SDRContrast",
        "SDRHighlights",
        "SDRShadows",
        "SDRWhites",
        "SDRClarity",
        "SDRBlend",
        "HDRMaxValue",
        "ParametricShadows",
        "ParametricDarks",
        "ParametricLights",
        "ParametricHighlights",
        "ParametricShadowSplit",
        "ParametricMidtoneSplit",
        "ParametricHighlightSplit",
        "CurveRefineSaturation",
        "HueAdjustmentRed",
        "SaturationAdjustmentOrange",
        "LuminanceAdjustmentBlue",
        "GrayMixerMagenta",
        "SplitToningShadowHue",
        "SplitToningShadowSaturation",
        "SplitToningHighlightHue",
        "SplitToningHighlightSaturation",
        "SplitToningBalance",
        "ColorGradeShadowLum",
        "ColorGradeHighlightLum",
        "ColorGradeMidtoneHue",
        "ColorGradeMidtoneSat",
        "ColorGradeMidtoneLum",
        "ColorGradeGlobalHue",
        "ColorGradeGlobalSat",
        "ColorGradeGlobalLum",
        "ColorGradeBlending",
        "Sharpness",
        "SharpenRadius",
        "SharpenDetail",
        "SharpenEdgeMasking",
        "LuminanceSmoothing",
        "LuminanceNoiseReductionDetail",
        "LuminanceNoiseReductionContrast",
        "ColorNoiseReduction",
        "ColorNoiseReductionDetail",
        "ColorNoiseReductionSmoothness",
        "PostCropVignetteAmount",
        "PostCropVignetteMidpoint",
        "PostCropVignetteRoundness",
        "PostCropVignetteFeather",
        "PostCropVignetteHighlightContrast",
        "PostCropVignetteStyle",
        "GrainAmount",
        "GrainSize",
        "GrainFrequency",
        "GrainSeed",
        "LensProfileEnable",
        "LensProfileDistortionScale",
        "LensProfileVignettingScale",
        "AutoLateralCA",
        "ChromaticAberrationR",
        "ChromaticAberrationB",
        "LensManualDistortionAmount",
        "VignetteAmount",
        "VignetteMidpoint",
        "DefringePurpleAmount",
        "DefringePurpleHueLo",
        "DefringePurpleHueHi",
        "DefringeGreenAmount",
        "DefringeGreenHueLo",
        "DefringeGreenHueHi",
        "Defringe",
        "ShadowTint",
        "RedHue",
        "RedSaturation",
        "GreenHue",
        "GreenSaturation",
        "BlueHue",
        "BlueSaturation",
        "PerspectiveVertical",
        "PerspectiveHorizontal",
        "PerspectiveRotate",
        "PerspectiveScale",
        "PerspectiveAspect",
        "PerspectiveX",
        "PerspectiveY",
        "PerspectiveUpright",
        "UprightFocalLength35mm",
        "UprightCenterMode",
        "UprightPreview",
        "CropLeft",
        "CropTop",
        "CropRight",
        "CropBottom",
        "CropAngle",
        "AutoWhiteVersion",
        "LensProfileSetup",
    ];

    #[test]
    fn develop_field_coverage_over_public_crs_fields() {
        let value = |k: &str| match k {
            "CropLeft" | "CropTop" => "0.1",
            "CropRight" | "CropBottom" => "0.9",
            _ => "10",
        };
        let body: Vec<String> = PUBLIC_CRS_FIELDS.iter().map(|k| format!("{k} = {}", value(k))).collect();
        let settings = format!("s = {{ {} }}", body.join(", "));
        let (_, mut dropped) = mapped_settings(&settings, crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, 1.5).unwrap();
        dropped.sort();
        let mapped = PUBLIC_CRS_FIELDS.len() - dropped.len();
        eprintln!("crs coverage: {mapped} of {} public fields map; dropped: {dropped:?}", PUBLIC_CRS_FIELDS.len());
        // No equivalent in the app: Lightroom's algorithm / tool bookkeeping (auto white version,
        // Upright's preview, centre mode and focal length: ours computes its own) and the pre-2012
        // single Defringe switch. Reported as unmapped and kept in the archive. (Lens profile setup
        // is bookkeeping the import ignores, `crs::IGNORED`: not reported.)
        let expected = ["AutoWhiteVersion", "Defringe", "UprightCenterMode", "UprightFocalLength35mm", "UprightPreview"];
        assert_eq!(dropped, expected, "{mapped} mapped");
    }

    #[test]
    fn lightroom_snapshots_become_versions() {
        let mut s = crate::Session::new();
        let mut data = sample();
        data.photos[1].snapshots = vec![
            row(json!({"id_local":8,"name":"Bright","text":"s = { Exposure2012 = 2 }"})),
            row(json!({"id_local":5,"name":"","text":"s = { Contrast2012 = 30 }"})),
            row(json!({"id_local":9,"name":"Bad","text":"nope {"})),
            row(json!({"id_local":10,"name":"Empty"})),
        ];
        let r = apply(&mut s, data, false).unwrap();
        let master = s.catalog.photo(PhotoId(r["mapping"]["1"].as_u64().unwrap())).unwrap();
        let names: Vec<_> = master.versions.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["Lightroom snapshot", "Bright"]);
        assert_eq!(master.versions[0].settings.light.contrast, 30.0);
        assert_eq!(master.versions[1].settings.light.exposure, 2.0);
        assert!(r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("2 Lightroom snapshot")));
        assert_eq!(r["snapshots"], 2);
    }

    #[test]
    fn lightroom_history_steps_become_history_in_order() {
        let mut s = crate::Session::new();
        let mut data = sample();
        data.photos[1].history = vec![
            row(
                json!({"id_local":3,"name":"Contrast","dateCreated":20.0,"text":"s = { Exposure2012 = 1.25, Contrast2012 = 17, CameraProfile = \"Example\" }"}),
            ),
            row(json!({"id_local":2,"name":"Exposure","dateCreated":10.0,"text":"s = { Exposure2012 = 1.25 }"})),
            row(json!({"id_local":4,"name":"Broken","dateCreated":15.0,"text":"not lua {"})),
        ];
        let r = apply(&mut s, data, false).unwrap();
        let master = s.catalog.photo(PhotoId(r["mapping"]["1"].as_u64().unwrap())).unwrap();
        let labels: Vec<_> = master.history.iter().map(|h| h.label.as_str()).collect();
        // the newest step matches the current settings: no extra entry
        assert_eq!(labels, ["Exposure", "Contrast"]);
        assert_eq!(master.history[0].settings.light.exposure, 1.25);
        assert_eq!(master.history[0].settings.light.contrast, 0.0);
        assert_eq!(*master.history[1].settings, *master.develop);
        assert!(r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("1 Lightroom history step")));
        assert_eq!(r["historySteps"], 4, "2 master steps + the copy's 1 step and its final state");
    }

    /// The history steps of a catalog file written with `steps`, read with a settings budget of 1 MiB.
    fn steps_within_one_mib(tag: &str, steps: Vec<Vec<crate::lightroom_fixture::Field>>) -> (Vec<Row>, Vec<String>) {
        use crate::lightroom_fixture::{self as fixture, Table};
        let dir = fixture::dir(tag);
        let catalog = dir.join("test.lrcat");
        fixture::write(&catalog, &[Table { name: "Adobe_libraryImageDevelopHistoryStep", columns: fixture::HISTORY_COLUMNS, rows: steps }]);
        let reader = Reader { db: Database::open_with_wal(std::fs::read(&catalog).unwrap(), &[]).unwrap() };
        let _ = std::fs::remove_dir_all(dir);
        let mut warnings = Vec::new();
        let grouped = reader.grouped_within("Adobe_libraryImageDevelopHistoryStep", 1 << 20, &mut warnings, &AtomicBool::new(false)).unwrap();
        let mut rows: Vec<Row> = grouped.into_values().flatten().collect();
        rows.sort_by_key(|r| number(r, "id_local"));
        (rows, warnings)
    }

    /// A catalog whose settings decode to more than the budget keeps every row, decodes the settings that fit, and
    /// says once how many it left out, instead of allocating whatever its BLOBs inflate to.
    #[test]
    fn settings_past_the_decode_budget_are_left_out_with_one_warning() {
        use crate::lightroom_fixture::{self as fixture, Field};
        let big = "x".repeat(512 << 10);
        let steps = (1..=4).map(|id| fixture::history_step(id, 1, id as f64, "Step", Field::Blob(fixture::compressed(&big)))).collect();
        let (rows, warnings) = steps_within_one_mib("history-budget", steps);
        assert_eq!(rows.len(), 4, "every step is still read");
        assert_eq!(rows.iter().filter(|r| r.contains_key("text")).count(), 2, "the two steps within 1 MiB keep their settings");
        assert_eq!(warnings, ["2 history steps read without their settings: more than 1 MiB of settings in Adobe_libraryImageDevelopHistoryStep"]);
    }

    /// Rows read past the budget get no History entry or version, and aren't counted again in each image's "could not
    /// be read" warning: the table's one warning covers them.
    #[test]
    fn rows_past_the_decode_budget_are_not_counted_as_unreadable() {
        use crate::lightroom_fixture::{self as fixture, Field};
        let big = "x".repeat(512 << 10);
        let steps = (1..=4).map(|id| fixture::history_step(id, 1, id as f64, "Step", Field::Blob(fixture::compressed(&big)))).collect();
        let (rows, _) = steps_within_one_mib("history-budget-count", steps);
        let base = Arc::new(lcc_develop::DevelopSettings::default());
        let (history, skipped) = history_steps(&rows, &base, &base, |_| Ok((json!({}), Vec::new())));
        assert_eq!((history.len(), skipped), (2, 0), "the two steps within the budget import; the rest aren't unreadable");
        let (versions, skipped) = snapshot_versions(&rows, &base, "now", |_| Ok((json!({}), Vec::new())));
        assert_eq!((versions.len(), skipped), (2, 0), "the same for snapshots");
    }

    /// A stream that inflates past the length its header declares is stopped there, not inflated up to 16 MiB first.
    #[test]
    fn a_stream_longer_than_its_declared_length_is_stopped_at_that_length() {
        let mut bytes = 10u32.to_be_bytes().to_vec();
        bytes.extend(miniz_oxide::deflate::compress_to_vec_zlib(&[b'x'; 1024], 6));
        assert_eq!(decode_compressed(&bytes, "settings"), Err("invalid compressed settings".to_string()));
        let mut exact = 1024u32.to_be_bytes().to_vec();
        exact.extend(miniz_oxide::deflate::compress_to_vec_zlib(&[b'x'; 1024], 6));
        assert_eq!(decode_compressed(&exact, "settings").map(|text| text.len()), Ok(1024), "exactly its length decodes");
    }

    /// Settings that fail to decode still cost their declared length, so a catalog of damaged rows can't make the
    /// reader inflate without bound: past the budget, the rest are left out without being inflated.
    #[test]
    fn settings_that_fail_to_decode_still_count_against_the_budget() {
        use crate::lightroom_fixture::{self as fixture, Field};
        let mut damaged = (512u32 << 10).to_be_bytes().to_vec();
        damaged.extend([1, 2, 3]);
        let steps = (1..=4).map(|id| fixture::history_step(id, 1, id as f64, "Step", Field::Blob(damaged.clone()))).collect();
        let (rows, warnings) = steps_within_one_mib("history-budget-bad", steps);
        assert_eq!(rows.len(), 4, "every step is still read");
        assert_eq!(
            warnings,
            [
                "image 1: history step 1: invalid compressed settings",
                "image 1: history step 2: invalid compressed settings",
                "2 history steps read without their settings: more than 1 MiB of settings in Adobe_libraryImageDevelopHistoryStep",
            ],
            "the two damaged steps within 1 MiB are tried; the other two aren't"
        );
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
        assert!(warnings.contains("payload.exe") && warnings.contains("file types the app reads"), "{warnings}");
        assert!(warnings.contains("image 4: not imported"), "the virtual copy of the refused original goes with it: {warnings}");
    }

    /// `sample()` (or its master alone) plus a photo `id` whose original is refused, and the
    /// import's report.
    fn import_with_refused(with_copy: bool, id: i64) -> (crate::Session, Value) {
        let mut data = sample();
        if !with_copy {
            data.photos.retain(|p| number(&p.image, "masterImage") == 0);
            data.members.retain(|m| number(m, "image") != 2);
        }
        let mut far = data.photos[data.photos.len() - 1].clone();
        far.source_id = id;
        far.uuid = format!("image-{id}");
        far.path = "//server/share/far.ARW".into();
        far.image = row(json!({"id_local": id, "fileFormat": "ARW"}));
        far.original = Original::Refused("it is on another computer".into());
        data.photos.push(far);
        let mut s = crate::Session::new();
        let r = apply(&mut s, data, false).unwrap_or_else(|e| panic!("with_copy {with_copy}, refused id {id}: {e:?}"));
        (s, r)
    }

    /// The sample's master (rating 5) has its catalog record, its virtual copy (rating 2) has one when
    /// `with_copy`, and no record is the refused photo's.
    fn assert_only_the_sample_imported(s: &crate::Session, r: &Value, with_copy: bool) {
        let mut mapped: Vec<_> = r["mapping"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        mapped.sort();
        let kept: &[&str] = if with_copy { &["1", "2"] } else { &["1"] };
        assert_eq!(mapped, kept, "the sample's master (and its copy) are imported: {r}");
        assert_eq!(s.catalog.len(), kept.len(), "{r}");
        let master = s.catalog.photos().find(|p| p.copy_of.is_none()).unwrap_or_else(|| panic!("no master record: {r}"));
        assert!(matches!(&master.source, Source::File { path } if path == "/missing/test.ARW"), "{:?}", master.source);
        assert_eq!(master.rating, 5, "the master's catalog data arrived");
        let copies: Vec<_> = s.catalog.photos().filter(|p| p.copy_of == Some(master.id)).map(|p| (p.copy_name.clone(), p.rating)).collect();
        let expected = if with_copy { vec![(Some("Mono".to_string()), 2)] } else { Vec::new() };
        assert_eq!(copies, expected, "{r}");
        assert!(
            s.catalog.photos().all(|p| !matches!(&p.source, Source::File { path } if path.ends_with("far.ARW"))),
            "the refused photo became a record"
        );
    }

    /// A refused photo whose id reads as 0 (a malformed catalog) refuses only itself: `masterImage`
    /// is 0 for every master, so every master was skipped with it and got no catalog record.
    #[test]
    fn a_refused_photo_with_id_zero_refuses_only_itself() {
        let (s, r) = import_with_refused(false, 0);
        assert_only_the_sample_imported(&s, &r, false);
        assert!(r["missing"].as_array().is_some_and(|m| m.contains(&json!("//server/share/far.ARW"))), "the refused photo is listed as missing: {r}");
        // the same with a virtual copy: its master was skipped, so the copy failed the import with
        // "virtual copy 2: missing master"
        let (s, r) = import_with_refused(true, 0);
        assert_only_the_sample_imported(&s, &r, true);
    }

    /// A virtual copy of a refused original has no original of its own, so it is refused with it,
    /// even when the catalog gives the copy a path of its own that isn't refused.
    #[test]
    fn a_virtual_copy_of_a_refused_original_is_refused_with_it() {
        let mut data = sample();
        let mut far = data.photos[1].clone();
        far.source_id = 3;
        far.uuid = "image-3".into();
        far.path = "//server/share/far.ARW".into();
        far.image = row(json!({"id_local": 3, "fileFormat": "ARW"}));
        far.original = Original::Refused("it is on another computer".into());
        let mut far_copy = far.clone();
        far_copy.source_id = 4;
        far_copy.uuid = "image-4".into();
        far_copy.image = row(json!({"id_local": 4, "masterImage": 3, "copyName": "Far copy"}));
        far_copy.path = "/missing/far-copy.ARW".into();
        far_copy.original = Original::Local;
        // the copy comes before its master, as a catalog's table order may have it
        data.photos.insert(0, far_copy);
        data.photos.push(far);
        let mut s = crate::Session::new();
        let r = apply(&mut s, data, false).unwrap_or_else(|e| panic!("{e:?}"));
        assert_only_the_sample_imported(&s, &r, true);
        assert!(s.catalog.photos().all(|p| p.copy_name.as_deref() != Some("Far copy")), "the refused original's copy became a record: {r}");
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
        // Lightroom's profile name picks the camera's calibrated look of that name
        assert_eq!(master.develop.profile.camera, "cal:example");
        assert!(!r["unmapped"]["1"].as_array().is_some_and(|a| a.iter().any(|v| v.as_str() == Some("CameraProfile"))));
        assert_eq!(s.undo.len(), 1);
        s.undo_step().unwrap();
        assert!(s.catalog.is_empty());
        assert!(s.catalog.albums().next().is_none());
        assert!(data.photos[1].settings.contains("CameraProfile")); // the source stays intact
    }
    #[test]
    fn smart_collections_become_smart_albums_and_stacks_are_kept() {
        let mut s = crate::Session::new();
        let mut data = sample();
        data.collections.push(row(json!({"id_local":11,"name":"Best","creationId":"com.adobe.ag.library.smart_collection"})));
        data.collections.push(row(json!({"id_local":12,"name":"Odd","creationId":"com.adobe.ag.library.smart_collection"})));
        data.members.push(row(json!({"collection":12,"image":1})));
        data.collection_content = vec![
            row(json!({"collection":11,"owningModule":"ag.library.smart_collection",
                "content":"s = { { criteria = \"rating\", operation = \">=\", value = 4 }, combine = \"intersect\" }"})),
            row(json!({"collection":12,"owningModule":"ag.library.smart_collection",
                "content":"s = { { criteria = \"metadataStatus\", operation = \"==\", value = \"x\" } }"})),
        ];
        data.stacks =
            vec![row(json!({"stack":5,"image":1,"position":2.0,"collapsed":1})), row(json!({"stack":5,"image":2,"position":1.0,"collapsed":1}))];
        let r = apply(&mut s, data.clone(), false).unwrap();
        assert_eq!(r["smartAlbums"], 1);
        assert_eq!(r["smartAsStatic"], 1);
        assert_eq!(r["stacks"], 1);
        let best = s.catalog.albums().find(|a| a.name == "Best").unwrap();
        assert!(best.is_smart());
        let odd = s.catalog.albums().find(|a| a.name == "Odd").unwrap();
        assert!(!odd.is_smart() && odd.photos.len() == 1);
        let copy = s.catalog.photos().find(|p| p.copy_of.is_some()).unwrap().id;
        let stack = s.catalog.stacks().next().unwrap();
        assert_eq!(stack.photos.first(), Some(&copy));
        assert!(stack.collapsed);
        // Reimport neither duplicates the stack nor warns about it.
        let r = apply(&mut s, data, false).unwrap();
        assert_eq!(r["stacks"], 0);
        assert_eq!(s.catalog.stacks().count(), 1);
        assert!(!r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().is_some_and(|w| w.starts_with("stack"))), "{}", r["warnings"]);
    }
    /// A member whose `collection` is missing, NULL, text or a fraction names no collection: it joins no album, with one
    /// warning, and does not join collection 0 or the truncated id.
    #[test]
    fn a_collection_member_whose_collection_is_not_an_id_joins_no_album() {
        let mut s = crate::Session::new();
        let mut data = sample();
        data.collections.push(row(json!({"id_local":0,"name":"Zero","creationId":"com.adobe.ag.library.collection"})));
        data.members.push(row(json!({"image":1,"positionInCollection":"a"})));
        data.members.push(row(json!({"collection":null,"image":1,"positionInCollection":"b"})));
        data.members.push(row(json!({"collection":"10","image":1,"positionInCollection":"c"})));
        data.members.push(row(json!({"collection":10.5,"image":1,"positionInCollection":"d"})));
        let r = apply(&mut s, data, false).unwrap();
        assert!(album_named(&s, "Zero").photos.is_empty());
        assert_eq!(album_named(&s, "Keepers").photos.len(), 2);
        let w: Vec<_> =
            r["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).filter(|w| w.starts_with("AgLibraryCollectionImage")).collect();
        assert_eq!(w.len(), 1, "{w:?}");
    }
    #[test]
    fn creation_saved_inside_a_collection_joins_the_nearest_set() {
        // Lightroom Classic can save a print or book inside a plain collection; the app's albums only nest in folders.
        let mut s = crate::Session::new();
        let mut data = sample();
        data.collections.push(row(json!({"id_local":11,"parent":10,"name":"Print","creationId":"com.adobe.ag.print"})));
        data.members.push(row(json!({"collection":11,"image":1,"positionInCollection":"a"})));
        let r = apply(&mut s, data, false).unwrap();
        assert_eq!(r["collections"], 3);
        let travel = s.catalog.albums().find(|a| a.name == "Travel").unwrap().id;
        let print = s.catalog.albums().find(|a| a.name == "Print").unwrap();
        assert_eq!(print.parent, Some(travel));
        assert_eq!(print.photos.len(), 1);
        assert!(r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().is_some_and(|w| w.contains("Print"))));
    }
    /// A saved print's layout as the catalog stores it: a Custom Package with two cells holding images 1 and 2,
    /// and a third naming an image the catalog doesn't import.
    const SAVED_PRINT: &str = r#"s = {
	_engineIdentifier = "com.adobe.lightroom.print.engine.package.custom",
	_engineSettings = {
		["com.adobe.lightroom.print.engine.package.custom"] = {
			autorotate = true,
			innerStrokeBorder = true,
			innerStrokeColor = AgColor( 0, 0, 0, 1 ),
			innerStrokeWidth = 0.2,
			numLayoutPages = 1,
			pages = {
				{
					cells = {
						{ bounds = AgMutableRect( 0, 0, 300, 200 ), imageID = 2, page = 1, },
						{ bounds = AgMutableRect( 250, 150, 612, 792 ), imageID = 1, page = 1, },
						{ bounds = AgMutableRect( 0, 500, 200, 792 ), imageID = 77, page = 1, },
					},
				},
			},
			zoomToFillFrame = true,
		},
	},
	_globalSettings = { printDestination = "jpeg", printingFileResolution = 300, mediaType = "matte" },
	_pageDimensions_x = 612,
	_pageDimensions_y = 792,
}"#;
    fn saved_print_sample(content: Option<&str>) -> CatalogImport {
        let mut data = sample();
        data.collections.push(row(json!({"id_local":11,"name":"Collage","creationId":"com.adobe.ag.print"})));
        data.members.push(row(json!({"collection":11,"image":1,"positionInCollection":"---0"})));
        data.members.push(row(json!({"collection":11,"image":2,"positionInCollection":"---1"})));
        if let Some(content) = content {
            data.collection_content.push(row(json!({"collection":11,"owningModule":"com.adobe.ag.print","content":content})));
            data.collection_content
                .push(row(json!({"collection":11,"owningModule":"com.adobe.ag.library.sortType","content":"positionInCollection"})));
        }
        data
    }
    #[test]
    fn a_saved_print_imports_as_a_saved_print_with_its_layout() {
        let mut s = crate::Session::new();
        let r = apply(&mut s, saved_print_sample(Some(SAVED_PRINT)), false).unwrap();
        assert_eq!(r["savedPrints"], 1);
        let collage = album_named(&s, "Collage");
        assert_eq!(collage.photos.len(), 2);
        let creation = collage.creation.as_ref().expect("a Saved Print");
        assert_eq!(creation.kind, "print");
        // what Print ▸ Saved Prints opens: the cells where Lightroom put them, each with its photo
        let doc = lcc_layout::Document::from_json(&creation.document).unwrap();
        let cells: Vec<_> = doc.pages[0].cells.iter().filter_map(|c| Some((c.rect, c.as_photo()?.photo.clone()))).collect();
        let photo_of = |source: i64| r["mapping"][source.to_string()].as_u64().map(|id| id.to_string());
        assert_eq!(cells.len(), 3);
        assert_eq!(cells[0].1, photo_of(2));
        assert_eq!(cells[1].1, photo_of(1));
        assert!((cells[1].0.x - 250.0).abs() < 1e-3 && (cells[1].0.h - 642.0).abs() < 1e-3);
        assert_eq!(cells[2].1, None, "a cell whose image isn't imported stays empty");
        let settings = lcc_print::PrintSettings::from_json(&doc.settings.unwrap().to_string()).unwrap();
        assert!(matches!(settings.layout, lcc_print::LayoutStyle::CustomPackage { .. }));
        assert_eq!(settings.destination, lcc_print::Destination::Jpeg);
        let w = warnings_naming(&r, "Collage");
        assert!(w.len() == 1 && w[0].contains("wasn't imported"), "{w:?}");
        // listed with the other Saved Prints
        assert!(crate::creations::creations_of(&s, None).iter().any(|c| c.name == "Collage"));
    }
    #[test]
    fn a_saved_print_whose_layout_cant_be_read_imports_as_a_plain_collection() {
        for content in [None, Some("s = { _engineIdentifier = \"com.adobe.lightroom.print.engine.unknown\" }"), Some("s = {")] {
            let mut s = crate::Session::new();
            let r = apply(&mut s, saved_print_sample(content), false).unwrap();
            assert_eq!(r["savedPrints"], 0);
            let collage = album_named(&s, "Collage");
            assert!(collage.creation.is_none() && collage.photos.len() == 2);
            let w = warnings_naming(&r, "Collage");
            assert!(w.len() == 1 && w[0].contains("imported as a plain collection"), "{content:?}: {w:?}");
        }
    }
    /// A catalog imported before saved prints carried their layout: importing it again turns the plain album into the
    /// Saved Print, and a later import keeps the layout the user may have changed since.
    #[test]
    fn reimport_gives_an_earlier_plain_album_its_print_layout_once() {
        let dir = std::env::temp_dir().join(format!("app-lrcat-saved-print-reimport-{}", std::process::id()));
        let mut s = crate::Session::new();
        s.open_library(&dir, false).unwrap();
        apply(&mut s, saved_print_sample(None), false).unwrap();
        assert!(album_named(&s, "Collage").creation.is_none());
        let albums = s.catalog.albums().count();
        let r = apply(&mut s, saved_print_sample(Some(SAVED_PRINT)), false).unwrap();
        assert_eq!(r["savedPrints"], 1);
        assert_eq!(s.catalog.albums().count(), albums, "no second album");
        let collage = album_named(&s, "Collage");
        let id = collage.id;
        assert!(collage.creation.as_ref().is_some_and(|c| c.kind == "print"));
        let edited = lcc_catalog::Creation { kind: "print".into(), document: collage.creation.as_ref().unwrap().document.replace("612", "613") };
        s.commit("edit", Op::SetAlbumCreation { id, creation: Some(edited.clone()) }).unwrap();
        let r = apply(&mut s, saved_print_sample(Some(SAVED_PRINT)), false).unwrap();
        assert_eq!(r["savedPrints"], 0);
        assert_eq!(album_named(&s, "Collage").creation.as_ref(), Some(&edited));
        s.close_library().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
    /// The import report's warnings about `album`.
    fn warnings_naming(r: &Value, album: &str) -> Vec<String> {
        r["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).filter(|w| w.starts_with(&format!("{album}:"))).map(String::from).collect()
    }
    /// [`warnings_naming`] without the one a saved print gets when (as in these fixtures) the catalog has no layout
    /// for it.
    fn placement_warnings(r: &Value, album: &str) -> Vec<String> {
        warnings_naming(r, album).into_iter().filter(|w| !w.contains("print layout isn't imported")).collect()
    }
    fn album_named<'a>(s: &'a crate::Session, name: &str) -> &'a lcc_catalog::Album {
        s.catalog.albums().find(|a| a.name == name).unwrap_or_else(|| panic!("no album {name}"))
    }
    /// An imported album nests only in a folder. A collection Lightroom keeps inside a plain collection used to fail the
    /// whole batch with "parent must be an existing folder", taking every album with it (#2165, #2532).
    #[test]
    fn a_collection_inside_a_plain_collection_joins_the_set_above_it() {
        let mut s = crate::Session::new();
        let mut data = sample();
        data.collections.push(row(json!({"id_local":11,"parent":10,"name":"Picks","creationId":"com.adobe.ag.library.collection"})));
        data.members.push(row(json!({"collection":11,"image":1,"positionInCollection":"a"})));
        let r = apply(&mut s, data, false).unwrap();
        assert_eq!(r["collections"], 3);
        assert_eq!(s.catalog.photos().count(), 2);
        let travel = album_named(&s, "Travel").id;
        assert_eq!(album_named(&s, "Keepers").parent, Some(travel));
        assert_eq!(album_named(&s, "Keepers").photos.len(), 2);
        assert_eq!(album_named(&s, "Picks").parent, Some(travel));
        assert_eq!(album_named(&s, "Picks").photos.len(), 1);
        let w = warnings_naming(&r, "Picks");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("Keepers") && w[0].contains("into “Travel”"), "{w:?}");
        assert!(warnings_naming(&r, "Keepers").is_empty());
    }
    /// Lightroom Classic saves a print, book, slideshow or web gallery inside a collection; with no set above it, the
    /// creation lands at the top level.
    #[test]
    fn a_creation_saved_inside_a_top_level_collection_lands_at_the_top_level() {
        let mut s = crate::Session::new();
        let mut data = sample();
        data.collections.push(row(json!({"id_local":12,"name":"Loose","creationId":"com.adobe.ag.library.collection"})));
        data.collections.push(row(json!({"id_local":13,"parent":12,"name":"Poster","creationId":"com.adobe.ag.print"})));
        data.collections.push(row(json!({"id_local":14,"parent":10,"name":"Album Book","creationId":"com.adobe.ag.layout.book"})));
        data.members.push(row(json!({"collection":13,"image":1,"positionInCollection":"a"})));
        let r = apply(&mut s, data, false).unwrap();
        assert_eq!(r["collections"], 5);
        let poster = album_named(&s, "Poster");
        assert_eq!((poster.parent, poster.folder, poster.photos.len()), (None, false, 1));
        assert_eq!(album_named(&s, "Album Book").parent, Some(album_named(&s, "Travel").id));
        let w = placement_warnings(&r, "Poster");
        assert!(w.len() == 1 && w[0].contains("Loose") && w[0].contains("at the top level"), "{w:?}");
        assert_eq!(warnings_naming(&r, "Album Book").len(), 1);
        assert_eq!(album_named(&s, "Keepers").photos.len(), 2);
    }
    /// A parent that's gone from the catalog or system-only (Quick Collection, an unsaved print…) isn't imported; the
    /// album lands at the top level, and the report says why it isn't where Lightroom had it.
    #[test]
    fn a_collection_whose_parent_is_missing_or_system_only_lands_at_the_top_level_with_a_warning() {
        let mut s = crate::Session::new();
        let mut data = sample();
        data.collections.push(row(json!({"id_local":11,"parent":99,"name":"Orphan","creationId":"com.adobe.ag.library.collection"})));
        data.collections.push(row(json!({"id_local":5,"name":"quick collection","creationId":"com.adobe.ag.library.collection","systemOnly":1})));
        data.collections.push(row(json!({"id_local":12,"parent":5,"name":"Stray","creationId":"com.adobe.ag.print"})));
        let r = apply(&mut s, data, false).unwrap();
        assert_eq!(r["collections"], 4);
        assert_eq!(album_named(&s, "Orphan").parent, None);
        assert_eq!(album_named(&s, "Stray").parent, None);
        assert!(s.catalog.albums().all(|a| a.name != "quick collection"));
        let w = warnings_naming(&r, "Orphan");
        assert!(w.len() == 1 && w[0].contains("isn't in the catalog"), "{w:?}");
        let w = placement_warnings(&r, "Stray");
        assert!(w.len() == 1 && w[0].contains("quick collection") && w[0].contains("at the top level"), "{w:?}");
        assert_eq!(album_named(&s, "Keepers").parent, Some(album_named(&s, "Travel").id));
    }
    /// Imports `data` keeping `index` between imports, as a library on disk does.
    fn import_indexed(s: &mut crate::Session, data: CatalogImport, index: &mut ImportIndex) -> crate::Result<Value> {
        let undo0 = s.undo.len();
        let missing_sources = HashSet::new();
        let context =
            ApplyPrepared { update_existing: false, files: Default::default(), index, archive_path: None, missing_sources: &missing_sources, undo0 };
        apply_prepared(s, data, context).map(|applied| applied.report)
    }
    /// The sample with “Picks”, a collection Lightroom keeps inside the plain collection “Keepers”.
    fn with_picks() -> CatalogImport {
        let mut data = sample();
        data.collections.push(row(json!({"id_local":11,"parent":10,"name":"Picks","creationId":"com.adobe.ag.library.collection"})));
        data.members.push(row(json!({"collection":11,"image":1,"positionInCollection":"a"})));
        data
    }
    /// Importing the catalog again finds the moved album where the first import put it, adds nothing twice and doesn't
    /// warn again: the album is already in the library.
    #[test]
    fn a_relocated_collection_keeps_its_place_on_reimport_without_duplicates_or_a_second_warning() {
        let mut s = crate::Session::new();
        let mut index = ImportIndex::default();
        let first = import_indexed(&mut s, with_picks(), &mut index).unwrap();
        assert_eq!(warnings_naming(&first, "Picks").len(), 1);
        let second = import_indexed(&mut s, with_picks(), &mut index).unwrap();
        assert_eq!(second["collections"], 3);
        assert_eq!(s.catalog.albums().count(), 3);
        assert_eq!(album_named(&s, "Picks").parent, Some(album_named(&s, "Travel").id));
        assert_eq!(album_named(&s, "Picks").photos.len(), 1);
        assert_eq!(album_named(&s, "Keepers").photos.len(), 2);
        assert!(warnings_naming(&second, "Picks").is_empty(), "{second}");
    }
    /// Albums the user moved after an import stay where the user put them when the catalog is imported again.
    #[test]
    fn an_album_the_user_moved_keeps_its_place_on_reimport() {
        let mut s = crate::Session::new();
        let mut index = ImportIndex::default();
        import_indexed(&mut s, with_picks(), &mut index).unwrap();
        for name in ["Picks", "Keepers"] {
            let id = album_named(&s, name).id;
            s.commit("Move Album", Op::MoveAlbum { id, parent: None }).unwrap();
        }
        let r = import_indexed(&mut s, with_picks(), &mut index).unwrap();
        assert_eq!(s.catalog.albums().count(), 3);
        assert_eq!(album_named(&s, "Picks").parent, None);
        assert_eq!(album_named(&s, "Keepers").parent, None);
        assert!(warnings_naming(&r, "Picks").is_empty() && warnings_naming(&r, "Keepers").is_empty(), "{r}");
    }
    /// A collection new since the last import goes into the set's album, and the warning names that album as the
    /// library calls it now, not by its Lightroom name.
    #[test]
    fn a_relocation_warning_names_the_set_as_the_user_renamed_it() {
        let mut s = crate::Session::new();
        let mut index = ImportIndex::default();
        import_indexed(&mut s, sample(), &mut index).unwrap();
        let travel = album_named(&s, "Travel").id;
        s.commit("Rename Album", Op::RenameAlbum { id: travel, name: "Trips".into() }).unwrap();
        let r = import_indexed(&mut s, with_picks(), &mut index).unwrap();
        assert_eq!(album_named(&s, "Picks").parent, Some(travel));
        let w = warnings_naming(&r, "Picks");
        assert!(w.len() == 1 && w[0].contains("inside “Keepers”") && w[0].ends_with("imported into “Trips” (Lightroom collection 11)"), "{w:?}");
    }
    /// The album an earlier import made for a Lightroom set may no longer be a folder (here: an index that names a plain
    /// album). A collection in that set lands at the top level, and the warning says why without calling the set
    /// something it isn't.
    #[test]
    fn a_set_whose_album_is_not_a_folder_puts_its_collections_at_the_top_level() {
        let mut s = crate::Session::new();
        let plain = s.catalog.alloc_album_id();
        s.commit("New Album", Op::AddAlbum { album: Album::new(plain, "Travel picks") }).unwrap();
        let mut index = ImportIndex::default();
        index.collections.insert(format!("{}:9", key("test.lrcat")), plain.0);
        let r = import_indexed(&mut s, sample(), &mut index).unwrap();
        assert_eq!(s.catalog.albums().count(), 2);
        assert!(!s.catalog.album(plain).unwrap().folder);
        let keepers = album_named(&s, "Keepers");
        assert_eq!((keepers.parent, keepers.photos.len()), (None, 2));
        let w = warnings_naming(&r, "Keepers");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("“Travel” is the album “Travel picks”") && w[0].contains("isn't a folder"), "{w:?}");
        assert!(!w[0].contains("isn't a collection set") && w[0].ends_with("at the top level (Lightroom collection 10)"), "{w:?}");
    }
    /// Only the outermost set inside a plain collection moves; the sets and creations inside it keep their places.
    #[test]
    fn sets_nested_under_a_plain_collection_keep_their_own_nesting() {
        let mut s = crate::Session::new();
        let mut data = sample();
        data.collections.push(row(json!({"id_local":12,"name":"Loose","creationId":"com.adobe.ag.library.collection"})));
        data.collections.push(row(json!({"id_local":13,"parent":12,"name":"Outer","creationId":"com.adobe.ag.library.group"})));
        data.collections.push(row(json!({"id_local":14,"parent":13,"name":"Inner","creationId":"com.adobe.ag.library.group"})));
        data.collections.push(row(json!({"id_local":15,"parent":14,"name":"Poster","creationId":"com.adobe.ag.print"})));
        data.members.push(row(json!({"collection":15,"image":1,"positionInCollection":"a"})));
        let r = apply(&mut s, data, false).unwrap();
        assert_eq!(r["collections"], 6);
        let outer = album_named(&s, "Outer");
        assert_eq!((outer.parent, outer.folder), (None, true));
        let inner = album_named(&s, "Inner");
        assert_eq!((inner.parent, inner.folder), (Some(outer.id), true));
        assert_eq!(album_named(&s, "Poster").parent, Some(inner.id));
        assert_eq!(album_named(&s, "Poster").photos.len(), 1);
        let w = warnings_naming(&r, "Outer");
        assert!(w.len() == 1 && w[0].contains("inside “Loose”") && w[0].ends_with("at the top level (Lightroom collection 13)"), "{w:?}");
        assert!(warnings_naming(&r, "Inner").is_empty() && placement_warnings(&r, "Poster").is_empty(), "{r}");
    }
    /// The album counter saturates: one at its end hands every collection the same id. The second collection used to be
    /// dropped without a word while the report counted it; the import now stops and names it.
    #[test]
    fn an_exhausted_album_counter_stops_the_import_naming_the_collection() {
        let mut s = crate::Session::new();
        s.commit("New Album", Op::AddAlbum { album: Album::new(lcc_catalog::AlbumId(u64::MAX - 1), "Last") }).unwrap();
        let err = apply(&mut s, sample(), false).unwrap_err().to_string();
        assert!(err.contains("“Keepers”") && err.contains("no unused album id"), "{err}");
        assert_eq!(s.catalog.albums().count(), 1);
        assert!(s.catalog.is_empty());
    }
    /// A collection listed twice imports once. A different collection under the same Lightroom id used to stop the import;
    /// like any bad record it now costs only itself (#1588): it is left out with a warning naming both, and its photos go
    /// to the first.
    #[test]
    fn a_collection_listed_twice_imports_once_and_a_conflicting_one_is_skipped_with_a_warning() {
        let mut s = crate::Session::new();
        let mut data = sample();
        data.collections.push(data.collections[1].clone());
        let r = apply(&mut s, data, false).unwrap();
        assert_eq!(r["collections"], 2);
        assert_eq!(s.catalog.albums().filter(|a| a.name == "Keepers").count(), 1);
        assert_eq!(album_named(&s, "Keepers").photos.len(), 2);

        let mut s = crate::Session::new();
        let mut data = sample();
        // shallower than the first, so a depth-ordered pass would meet it first
        data.collections.push(row(json!({"id_local":10,"parent":0,"name":"Rejects","creationId":"collection"})));
        let r = apply(&mut s, data, false).unwrap();
        assert_eq!(r["photos"], 2, "{r}");
        assert_eq!(r["collections"], 2, "{r}");
        assert!(s.catalog.albums().all(|a| a.name != "Rejects"));
        let keepers = album_named(&s, "Keepers");
        assert_eq!(keepers.photos.len(), 2);
        assert!(keepers.parent.is_some(), "the first row's place is kept");
        assert_eq!(
            r["warnings"],
            json!(["collection “Rejects”: not imported, it has the id of collection “Keepers”; the photos listed under that id go to “Keepers”"])
        );
        assert_eq!(s.undo.len(), 1);
    }
    /// A damaged index that names one existing album for two collections gives the second an album of its own rather
    /// than merging its photos into the first's.
    #[test]
    fn an_index_naming_one_album_for_two_collections_gives_the_second_its_own() {
        let mut s = crate::Session::new();
        let old = s.catalog.alloc_album_id();
        s.commit("New Album", Op::AddAlbum { album: Album::new(old, "Old") }).unwrap();
        let mut index = ImportIndex::default();
        for lr in [10, 11] {
            index.collections.insert(format!("{}:{lr}", key("test.lrcat")), old.0);
        }
        let r = import_indexed(&mut s, with_picks(), &mut index).unwrap();
        assert_eq!(r["collections"], 3);
        assert_eq!(s.catalog.album(old).unwrap().photos.len(), 2, "Keepers merges into the album the index names");
        let picks = album_named(&s, "Picks");
        assert_ne!(picks.id, old);
        assert_eq!(picks.photos.len(), 1);
    }
    #[test]
    fn preserves_existing_edits_and_rejects_invalid_structure_before_mutation() {
        let mut s = crate::Session::new();
        let data = sample();
        apply(&mut s, data.clone(), false).unwrap();
        let id = s.catalog.photos().find(|p| p.copy_of.is_none()).unwrap().id;
        s.set_develop(id, lcc_develop::DevelopSettings::default(), "Personal edit").unwrap();
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

    /// Upstream #3906: a Custom white balance is shifted from the as-shot white the photo's history still holds, as
    /// its text or compressed step; steps without the values, damaged steps and other photos' steps give none.
    #[test]
    fn a_custom_white_balance_is_shifted_from_the_as_shot_white_in_its_history() {
        use crate::lightroom_fixture::{self as fixture, Table};
        let custom = "s = { WhiteBalance = \"Custom\", Temperature = 3578, Tint = -5, Exposure2012 = 0.5 }";
        let as_shot = "s = { WhiteBalance = \"As Shot\", Temperature = 3650, Tint = -2 }";
        let bare = "s = { WhiteBalance = \"As Shot\" }";
        let damaged = "s = { WhiteBalance = \"As Shot\", Temperature = 0, Tint = 1";
        type Case = (&'static str, Vec<Vec<fixture::Field>>, Option<(f64, f64)>);
        let cases: Vec<Case> = vec![
            (
                "text",
                vec![fixture::history_step(1, 1, 1.0, "Import", fixture::text(bare)), fixture::history_step(2, 1, 2.0, "WB", fixture::text(as_shot))],
                Some((3650.0, -2.0)),
            ),
            ("blob", vec![fixture::history_step(1, 1, 1.0, "WB", fixture::Field::Blob(fixture::compressed(as_shot)))], Some((3650.0, -2.0))),
            ("bare", vec![fixture::history_step(1, 1, 1.0, "Import", fixture::text(bare))], None),
            (
                "damaged",
                vec![
                    fixture::history_step(1, 1, 1.0, "WB", fixture::Field::Blob(vec![0xff; 9])),
                    fixture::history_step(2, 1, 2.0, "WB", fixture::text(damaged)),
                    fixture::history_step(3, 2, 3.0, "Other photo", fixture::text(as_shot)),
                ],
                None,
            ),
        ];
        for (tag, steps, expected) in cases {
            let dir = fixture::dir(&format!("as-shot-white-{tag}"));
            let mut tables = fixture::one_photo(&dir.join("originals"), custom);
            tables.push(Table { name: "Adobe_libraryImageDevelopHistoryStep", columns: fixture::HISTORY_COLUMNS, rows: steps });
            let catalog = dir.join("test.lrcat");
            fixture::write(&catalog, &tables);
            let imported = read(&catalog).unwrap();
            assert_eq!(imported.photos[0].as_shot_white, expected, "{tag}: {:?}", imported.warnings);
            let (partial, unknown) =
                mapped_settings_from(custom, imported.photos[0].as_shot_white, crate::crs::Target::RawRelative, 1.5, Orientation::Normal, 1.5)
                    .unwrap();
            if expected.is_some() {
                assert_eq!(partial["wb"]["mode"], "custom", "{partial}");
                assert!(partial["wb"]["temp"].as_f64().is_some_and(|t| t.is_finite()), "{partial}");
                assert!(!unknown.iter().any(|k| k.starts_with("Temperature")), "{unknown:?}");
            } else {
                assert_eq!(partial["wb"], json!({"mode": "asShot"}), "{partial}");
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
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
        // an angle alone still means a crop (Lightroom's angle turns the other way to the app's)
        let (partial, unknown) = mapped_settings("s = { CropAngle = -1.5 }", crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, 1.5).unwrap();
        assert_eq!(partial["crop"]["geometry"]["angle"], 1.5);
        assert!(!unknown.iter().any(|k| k.starts_with("CropAngle")), "{unknown:?}");
        // no crop fields: the crop stays untouched
        let (partial, _) = mapped_settings("s = { Exposure2012 = 0.5 }", crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, 1.5).unwrap();
        assert!(partial.get("crop").is_none());
    }

    #[test]
    fn catalog_with_profile_corrections_off_turns_off_the_embedded_ones() {
        // Lightroom Classic renders such a photo without the raw's built-in lens corrections
        for target in [crate::crs::Target::RawAbsolute, crate::crs::Target::RawRelative] {
            let (partial, _) = mapped_settings("s = { LensProfileEnable = 0 }", target, 1.5, Orientation::Normal, 1.5).unwrap();
            assert_eq!(partial["optics"]["lens_profile"], json!(false), "{target:?}: {partial}");
            let (partial, _) = mapped_settings("s = { LensProfileEnable = 1 }", target, 1.5, Orientation::Normal, 1.5).unwrap();
            assert_eq!(partial["optics"]["lens_profile"], json!(true), "{target:?}: {partial}");
        }
        // a catalog entry without the switch leaves the photo's own default alone
        let (partial, _) = mapped_settings("s = { Exposure2012 = 0.5 }", crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, 1.5).unwrap();
        assert!(partial.pointer("/optics/lens_profile").is_none(), "{partial}");
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

    /// Lightroom stores a catalog photo's spots, masks and brush strokes on the stored image; a raw it shows turned
    /// (portrait from a landscape sensor) has them land where Lightroom shows them, at the sizes it shows.
    #[test]
    fn catalog_spots_and_masks_on_a_turned_raw_land_where_lightroom_shows_them() {
        let settings = r#"s = {
            RetouchAreas = { { SpotType = "heal", SourceState = "sourceSetExplicitly", SourceX = 0.6, OffsetY = 0.9,
                Masks = { { What = "Mask/Ellipse", X = 0.69, Y = 0.9, SizeX = 0.02 } } } },
            CircularGradientBasedCorrections = { { What = "Correction", CorrectionAmount = 1, LocalExposure2012 = 0.25,
                CorrectionMasks = { { What = "Mask/CircularGradient", Top = 0.2, Left = 0.1, Bottom = 0.4, Right = 0.3,
                    Angle = 10, Feather = 50, MaskValue = 1 } } } },
            PaintBasedCorrections = { { What = "Correction", LocalExposure2012 = 0.25, CorrectionMasks = {
                { What = "Mask/Paint", Radius = 0.05, CenterWeight = 0, Flow = 1, MaskValue = 1, Dabs = { "d 0.69 0.9" } } } } } }"#;
        let near = |v: &serde_json::Value, x: f64, y: f64| (v["x"].as_f64().unwrap() - x).abs() < 1e-9 && (v["y"].as_f64().unwrap() - y).abs() < 1e-9;
        // a 3:2 sensor, shown a quarter turn counter-clockwise (Lightroom's "DA"): (x, y) → (y, 1 − x)
        let (turned, _) = mapped_settings(settings, crate::crs::Target::RawAbsolute, 2.0 / 3.0, Orientation::Rotate270, 1.5).unwrap();
        let spot = &turned["spots"][0];
        assert!(near(&spot["points"][0], 0.9, 0.31), "{spot}");
        assert!(near(&spot["source_offset"], 0.0, 0.09), "the source turns with it: {spot}");
        assert!((spot["size"].as_f64().unwrap() - 0.02).abs() < 1e-9, "a spot keeps its size on the long edge: {spot}");
        let radial = &turned["masks"][0]["components"][0]["shape"];
        let brush = &turned["masks"][1]["components"][0]["shape"];
        let (radial, brush) = if radial["kind"] == "radial" { (radial, brush) } else { (brush, radial) };
        assert!(near(&radial["center"], 0.3, 0.8), "{radial}");
        // 0.2 of the sensor's width (its long edge) wide → 0.2 of the shown height tall
        let (rx, ry) = (radial["rx"].as_f64().unwrap(), radial["ry"].as_f64().unwrap());
        assert!((rx - 0.2 / 3.0).abs() < 1e-9 && (ry - 0.1).abs() < 1e-9, "{radial}");
        assert!(near(&brush["strokes"][0]["points"][0], 0.9, 0.31), "{brush}");
        // an upright photo keeps them as stored
        let (upright, _) = mapped_settings(settings, crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, 1.5).unwrap();
        assert!(near(&upright["spots"][0]["points"][0], 0.69, 0.9));
        assert_eq!(upright["masks"].as_array().map(Vec::len), Some(2));
    }

    /// The `crs` corners and angle Lightroom would store for the app crop `g` on a stored image
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

    /// The XMP a catalog keeps beside a photo's develop text holds its spots and masks on the stored image too: on a
    /// photo Lightroom shows turned they land where it shows them.
    #[test]
    fn catalog_xmp_spots_and_masks_follow_the_catalog_orientation() {
        let mut data = sample();
        data.photos.truncate(1);
        data.photos[0].image.remove("masterImage");
        data.photos[0].image.insert("orientation".into(), json!("DA"));
        data.photos[0].image.insert("fileWidth".into(), json!(3000));
        data.photos[0].image.insert("fileHeight".into(), json!(2000));
        data.photos[0].settings = "s = { Exposure2012 = 0.5 }".into();
        data.photos[0].xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
 <crs:RetouchAreas><rdf:Seq><rdf:li><rdf:Description crs:SpotType="clone" crs:SourceState="sourceSetExplicitly" crs:SourceX="0.6" crs:OffsetY="0.9">
  <crs:Masks><rdf:Seq><rdf:li crs:What="Mask/Ellipse" crs:X="0.69" crs:Y="0.9" crs:SizeX="0.02"/></rdf:Seq></crs:Masks>
 </rdf:Description></rdf:li></rdf:Seq></crs:RetouchAreas>
 <crs:CircularGradientBasedCorrections><rdf:Seq><rdf:li><rdf:Description crs:What="Correction" crs:CorrectionAmount="1" crs:LocalExposure2012="0.25">
  <crs:CorrectionMasks><rdf:Seq><rdf:li crs:What="Mask/CircularGradient" crs:Top="0.2" crs:Left="0.1" crs:Bottom="0.4" crs:Right="0.3" crs:Angle="0" crs:Feather="50" crs:MaskValue="1"/></rdf:Seq></crs:CorrectionMasks>
 </rdf:Description></rdf:li></rdf:Seq></crs:CircularGradientBasedCorrections>
</rdf:Description></rdf:RDF></x:xmpmeta>"#
            .into();
        let mut s = crate::Session::new();
        apply(&mut s, data, false).unwrap();
        let p = s.catalog.photos().next().unwrap();
        let near = |a: lcc_geom::Point, x: f64, y: f64| (a.x - x).abs() < 1e-6 && (a.y - y).abs() < 1e-6;
        let spot = p.develop.spots.first().expect("the spot came in");
        // (x, y) of the stored image → (y, 1 − x) shown a quarter turn counter-clockwise
        assert!(near(spot.points[0], 0.9, 0.31), "{spot:?}");
        let offset = spot.source_offset.expect("an explicit source");
        assert!(near(offset, 0.0, 0.09), "the source turns with it: {offset:?}");
        let comp = p.develop.masks.first().and_then(|m| m.components.first()).expect("the radial came in");
        let lcc_develop::MaskShape::Radial { center, .. } = &comp.shape else { panic!("not radial: {:?}", comp.shape) };
        assert!(near(*center, 0.3, 0.8), "{center:?}");
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

    fn smart_sample(content: &str) -> CatalogImport {
        let mut data = sample();
        data.collections.push(row(json!({"id_local":11,"name":"Best","creationId":"com.adobe.ag.library.smart_collection"})));
        data.members.push(row(json!({"collection":11,"image":1})));
        data.collection_content = vec![row(json!({"collection":11,"owningModule":"ag.library.smart_collection","content":content}))];
        data
    }
    const SUPPORTED: &str = "s = { { criteria = \"rating\", operation = \">=\", value = 4 }, combine = \"intersect\" }";
    const UNSUPPORTED: &str = "s = { { criteria = \"metadataStatus\", operation = \"==\", value = \"x\" } }";
    #[test]
    fn reimport_keeps_an_earlier_static_album_static_and_merges_its_membership() {
        let dir = std::env::temp_dir().join(format!("app-lrcat-reimport_keeps_an_earlie-{}", std::process::id()));
        let mut s = crate::Session::new();
        s.open_library(&dir, false).unwrap();
        let r = apply(&mut s, smart_sample(UNSUPPORTED), false).unwrap();
        assert_eq!((r["smartAlbums"].as_u64(), r["smartAsStatic"].as_u64()), (Some(0), Some(1)));
        let albums = s.catalog.albums().count();
        let mut data = smart_sample(SUPPORTED);
        data.members.push(row(json!({"collection":11,"image":2})));
        let r = apply(&mut s, data.clone(), false).unwrap();
        assert_eq!((r["smartAlbums"].as_u64(), r["smartAsStatic"].as_u64(), r["smartKept"].as_u64()), (Some(0), Some(1), Some(0)));
        assert!(
            r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().is_some_and(|w| w.starts_with("Best:") && w.contains("stays static"))),
            "{}",
            r["warnings"]
        );
        let best = s.catalog.albums().find(|a| a.name == "Best").unwrap();
        assert!(!best.is_smart());
        assert_eq!(best.photos.len(), 2, "new membership merged");
        // Repeating the import creates no duplicates.
        apply(&mut s, data, false).unwrap();
        assert_eq!(s.catalog.albums().count(), albums);
        assert_eq!(s.catalog.albums().find(|a| a.name == "Best").unwrap().photos.len(), 2);
        s.close_library().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn reimport_preserves_user_edited_smart_rules() {
        let dir = std::env::temp_dir().join(format!("app-lrcat-reimport_preserves_user_-{}", std::process::id()));
        let mut s = crate::Session::new();
        s.open_library(&dir, false).unwrap();
        let r = apply(&mut s, smart_sample(SUPPORTED), false).unwrap();
        assert_eq!(r["smartAlbums"], 1);
        let albums = s.catalog.albums().count();
        let id = s.catalog.albums().find(|a| a.name == "Best").unwrap().id;
        let edited = lcc_catalog::Filter { rating: 2, ..Default::default() };
        s.commit("Edit rules", Op::SetAlbumRules { id, rules: Box::new(edited.clone()) }).unwrap();
        let r = apply(&mut s, smart_sample(SUPPORTED), false).unwrap();
        assert_eq!((r["smartAlbums"].as_u64(), r["smartAsStatic"].as_u64(), r["smartKept"].as_u64()), (Some(1), Some(0), Some(1)));
        let best = s.catalog.album(id).unwrap();
        assert_eq!(best.smart.as_deref().map(|f| f.rating), Some(2));
        assert_eq!(s.catalog.albums().count(), albums);
        s.close_library().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
    /// A smart collection with a Collection rule, as Lightroom stores it.
    fn smart_collection(data: &mut CatalogImport, id: i64, name: &str, op: &str, value: &str) {
        data.collections.push(row(json!({"id_local":id,"name":name,"creationId":"com.adobe.ag.library.smart_collection"})));
        let content = format!("s = {{ {{ criteria = \"collection\", operation = \"{op}\", value = \"{value}\" }}, combine = \"intersect\" }}");
        data.collection_content.push(row(json!({"collection":id,"owningModule":"ag.library.smart_collection","content":content})));
    }
    #[test]
    fn collection_rules_match_ordinary_collections_only() {
        let mut s = crate::Session::new();
        let mut data = sample();
        data.collections.push(row(json!({"id_local":13,"name":"Trip 2024","creationId":"com.adobe.ag.library.collection"})));
        data.members.push(row(json!({"collection":13,"image":1})));
        // Each matches its own name and the other's ("collection" holds both letters); of the ordinary collections,
        // "e" matches Keepers and "i" Trip 2024 (Travel is a set).
        smart_collection(&mut data, 20, "In a collection 'e'", "all", "e");
        smart_collection(&mut data, 21, "In a collection 'i'", "all", "i");
        // matches Trip 2024 and its own name
        smart_collection(&mut data, 22, "Trip picks", "beginsWith", "trip");
        let r = apply(&mut s, data, false).expect("the catalog imports");
        assert_eq!((r["smartAlbums"].as_u64(), r["smartAsStatic"].as_u64()), (Some(3), Some(0)), "{}", r["warnings"]);
        assert_eq!(s.catalog.photos().count(), 2);
        let (keepers, trip) = (album_named(&s, "Keepers").id, album_named(&s, "Trip 2024").id);
        for (name, tests) in [("In a collection 'e'", keepers), ("In a collection 'i'", trip), ("Trip picks", trip)] {
            let album = album_named(&s, name);
            let tested = album.smart.as_deref().map(lcc_catalog::Filter::albums_tested);
            assert_eq!(tested, Some(vec![tests]), "“{name}” is a smart album testing only the ordinary collection its rule matches");
        }
        // Trip picks finds Trip 2024's photo, as in Lightroom
        let picks = album_named(&s, "Trip picks").id;
        assert_eq!(s.catalog.album_photos(picks), s.catalog.album_photos(trip));
        assert_eq!(s.catalog.album_photos(trip).len(), 1);
    }
    #[test]
    fn a_collection_rule_matching_only_smart_collections_matches_nothing() {
        let mut s = crate::Session::new();
        let mut data = sample();
        smart_collection(&mut data, 20, "In a collection 'e'", "all", "e");
        smart_collection(&mut data, 21, "Smart ones", "beginsWith", "in a");
        data.members.push(row(json!({"collection":21,"image":1})));
        let r = apply(&mut s, data, false).expect("the catalog imports");
        assert_eq!((r["smartAlbums"].as_u64(), r["smartAsStatic"].as_u64()), (Some(1), Some(1)), "{}", r["warnings"]);
        assert!(
            r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().is_some_and(|w| w.starts_with("Smart ones:")
                && w.contains("matches no imported collection")
                && w.contains("imported current membership"))),
            "{}",
            r["warnings"]
        );
        let smart_ones = album_named(&s, "Smart ones");
        assert!(!smart_ones.is_smart() && smart_ones.photos.len() == 1, "falls back to a static album with its current members");
    }
    #[test]
    fn a_smart_collection_that_would_include_itself_falls_back_alone() {
        let dir = std::env::temp_dir().join(format!("app-lrcat-a_smart_collection_that-{}", std::process::id()));
        let mut s = crate::Session::new();
        s.open_library(&dir, false).unwrap();
        let mut data = sample();
        smart_collection(&mut data, 11, "Best", "beginsWith", "keep");
        data.members.push(row(json!({"collection":11,"image":1})));
        let r = apply(&mut s, data.clone(), false).unwrap();
        assert_eq!(r["smartAlbums"], 1);
        // A damaged library: Best's album is deleted, and Keepers' album is a smart album testing the id the reimport
        // gives Best, which tests Keepers.
        let best = album_named(&s, "Best").id;
        s.commit("Delete", Op::RemoveAlbum { id: best }).unwrap();
        let next = lcc_catalog::AlbumId(s.catalog.alloc_album_id().0 + 1);
        let mut keepers = album_named(&s, "Keepers").clone();
        let rules = lcc_catalog::RuleSet {
            mode: lcc_catalog::Match::All,
            rules: vec![lcc_catalog::Rule::Field { field: "album".into(), op: "is".into(), value: json!(next) }],
        };
        keepers.smart = Some(Box::new(lcc_catalog::Filter { rule_set: Some(rules), ..Default::default() }));
        keepers.photos.clear();
        s.catalog.apply(Op::RemoveAlbum { id: keepers.id }).unwrap();
        s.catalog.apply(Op::AddAlbum { album: keepers }).unwrap();
        let albums = s.catalog.albums().count();
        let r = apply(&mut s, data, false).expect("the rest of the catalog imports");
        assert_eq!((r["smartAlbums"].as_u64(), r["smartAsStatic"].as_u64()), (Some(0), Some(1)), "{}", r["warnings"]);
        assert!(
            r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().is_some_and(|w| w.starts_with("Best:") && w.contains("include itself"))),
            "{}",
            r["warnings"]
        );
        let best = album_named(&s, "Best");
        assert_eq!(best.id, next);
        assert!(!best.is_smart() && best.photos.len() == 1, "Best keeps its members as a static album");
        assert_eq!(s.catalog.albums().count(), albums + 1);
        assert_eq!(s.catalog.photos().count(), 2);
        s.close_library().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn a_loop_through_a_smart_collection_of_the_same_import_falls_back_alone() {
        let dir = std::env::temp_dir().join(format!("app-lrcat-a_loop_through_a_smart-{}", std::process::id()));
        let mut s = crate::Session::new();
        s.open_library(&dir, false).unwrap();
        let mut data = sample();
        data.collections.push(row(json!({"id_local":13,"name":"Trip 2024","creationId":"com.adobe.ag.library.collection"})));
        apply(&mut s, data.clone(), false).unwrap();
        // A damaged library: the albums of Keepers and Trip 2024 are smart albums testing the ids the next import gives
        // Best and Picks. Best tests Keepers (→ Picks), and Picks tests Trip 2024 (→ Best), added just before it.
        smart_collection(&mut data, 11, "Best", "beginsWith", "keep");
        smart_collection(&mut data, 12, "Picks", "beginsWith", "trip");
        data.members.push(row(json!({"collection":12,"image":1})));
        let first = s.catalog.alloc_album_id().0 + 1;
        let (best, picks) = (lcc_catalog::AlbumId(first), lcc_catalog::AlbumId(first + 1));
        for (name, tests) in [("Keepers", picks), ("Trip 2024", best)] {
            let mut album = album_named(&s, name).clone();
            let rules = lcc_catalog::RuleSet {
                mode: lcc_catalog::Match::All,
                rules: vec![lcc_catalog::Rule::Field { field: "album".into(), op: "is".into(), value: json!(tests) }],
            };
            album.smart = Some(Box::new(lcc_catalog::Filter { rule_set: Some(rules), ..Default::default() }));
            album.photos.clear();
            s.catalog.apply(Op::RemoveAlbum { id: album.id }).unwrap();
            s.catalog.apply(Op::AddAlbum { album }).unwrap();
        }
        let r = apply(&mut s, data, false).expect("the rest of the catalog imports");
        assert_eq!((r["smartAlbums"].as_u64(), r["smartAsStatic"].as_u64()), (Some(1), Some(1)), "{}", r["warnings"]);
        assert_eq!((album_named(&s, "Best").id, album_named(&s, "Picks").id), (best, picks));
        assert!(album_named(&s, "Best").is_smart());
        let picks = album_named(&s, "Picks");
        assert!(!picks.is_smart() && picks.photos.len() == 1, "Picks keeps its members as a static album");
        assert!(
            r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().is_some_and(|w| w.starts_with("Picks:") && w.contains("include itself"))),
            "{}",
            r["warnings"]
        );
        s.close_library().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn persistent_reimport_keeps_copy_and_collection_ids_and_capture_time() {
        let dir = std::env::temp_dir().join(format!("app-lrcat-reimport-{}", std::process::id()));
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
                SqlValue::Real(n) => {
                    body.extend(n.to_be_bytes());
                    7
                }
                SqlValue::Blob(b) => {
                    body.extend(b);
                    12 + 2 * b.len() as u64
                }
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
        native_catalog_with_last_table_page(tables, None)
    }
    /// [`native_catalog`] whose last table claims `last_page` as its root page.
    fn native_catalog_with_last_table_page(tables: &[(&str, &str, Vec<SqlValue>)], last_page: Option<i64>) -> Vec<u8> {
        let tables: Vec<_> = tables.iter().map(|(name, columns, row)| (*name, *columns, vec![row.clone()])).collect();
        native_catalog_rows(&tables, last_page)
    }
    /// A SQLite file with one leaf page per table: `(name, columns, rows)` each. `last_page` overrides the last
    /// table's root page.
    pub(crate) fn native_catalog_rows(tables: &[(&str, &str, Vec<Vec<SqlValue>>)], last_page: Option<i64>) -> Vec<u8> {
        use SqlValue::{Integer as I, Text as T};
        let pages = tables.len() + 1;
        let mut bytes = vec![0u8; pages * 2048];
        bytes[..16].copy_from_slice(b"SQLite format 3\0");
        bytes[16..18].copy_from_slice(&2048u16.to_be_bytes());
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
            leaf(&mut bytes[(i + 1) * 2048..(i + 2) * 2048], 0, rows);
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
        let scratch = lcc_brand::env_os("TEST_SCRATCH").or_else(|| std::env::var_os("TMPDIR")).map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let scratch = scratch.join("app-lrcat-fixtures");
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
            // fsutil's own answer is in the system's language; the file attribute isn't
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x200;
                let attributes = std::fs::metadata(&path).expect("read synthetic fixture attributes").file_attributes();
                assert!(attributes & FILE_ATTRIBUTE_SPARSE_FILE != 0, "synthetic fixture is not sparse (attributes {attributes:#x})");
            }
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

        SparseCatalogFixture { path, keep: large && lcc_brand::env_is_set("KEEP_SPARSE_FIXTURE") }
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
    fn native_video_import_is_visible_in_video_filter() {
        let dir = std::env::temp_dir().join(format!("app-lrcat-video-filter-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("videos.lrcat");
        let mut tables = photo_tables((dir.to_str().unwrap(), ""));
        tables[2].2[2] = SqlValue::Text("clip.MOV".into());
        tables[3].2[3] = SqlValue::Text("VIDEO".into());
        std::fs::write(&path, native_catalog(&tables)).unwrap();
        let mut s = crate::Session::new();
        let imported = s.execute("library.importLightroom", &json!({"path": path})).unwrap();
        let id = PhotoId(imported["mapping"]["1"].as_u64().unwrap());
        assert_eq!(s.catalog.photo(id).unwrap().kind, lcc_catalog::MediaKind::Video);
        s.execute("library.filter", &json!({"kind": "video"})).unwrap();
        assert_eq!(s.visible(), &[id]);
        s.execute("library.filter", &json!({"kind": "image"})).unwrap();
        assert!(s.visible().is_empty());
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(s.catalog.is_empty());
        s.execute("edit.redo", &json!({})).unwrap();
        s.execute("library.filter", &json!({"kind": "video"})).unwrap();
        assert_eq!(s.visible(), &[id]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn native_video_reimport_repairs_kind_without_losing_edits_or_memberships() {
        let dir = std::env::temp_dir().join(format!("app-lrcat-video-reimport-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("videos.lrcat");
        let mut tables = photo_tables((dir.to_str().unwrap(), ""));
        tables[2].2[2] = SqlValue::Text("clip.MOV".into());
        tables[3].2[3] = SqlValue::Text("VIDEO".into());
        std::fs::write(&path, native_catalog(&tables)).unwrap();
        let mut s = crate::Session::new();
        let id = s.catalog.alloc_photo_id();
        let mut photo =
            Photo::new(id, Source::File { path: dir.join("Photos/clip.MOV").to_string_lossy().into() }, "clip.MOV", "VIDEO", 1920, 1080, "before");
        photo.rating = 5;
        photo.flag = Flag::Pick;
        photo.file_size = 1234;
        photo.content_hash = Some("existing-content".into());
        s.commit("Previous import", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        let mut settings = lcc_develop::DevelopSettings::default();
        settings.light.exposure = 0.75;
        s.set_develop(id, settings, "Personal edit").unwrap();
        let peer_id = s.catalog.alloc_photo_id();
        let peer = Photo::new(peer_id, Source::File { path: "/missing/peer.jpg".into() }, "peer.jpg", "JPEG", 20, 10, "before");
        s.commit("Peer", Op::AddPhoto { photo: Box::new(peer) }).unwrap();
        let mut album = Album::new(s.catalog.alloc_album_id(), "Personal album");
        album.photos = vec![id, peer_id];
        s.commit("Album", Op::AddAlbum { album: album.clone() }).unwrap();
        let stack = lcc_catalog::Stack { id: s.catalog.alloc_stack_id(), photos: vec![id, peer_id], collapsed: false };
        s.commit("Stack", Op::AddStack { stack: stack.clone() }).unwrap();
        let before = s.catalog.to_snapshot();
        let mut expected = (**s.catalog.photo(id).unwrap()).clone();
        expected.kind = lcc_catalog::MediaKind::Video;
        s.execute("library.filter", &json!({"kind": "video"})).unwrap();
        assert!(s.visible().is_empty());
        let result = s.execute("library.importLightroom", &json!({"path": path})).unwrap();
        assert_eq!(result["mapping"]["1"], id.0);
        assert_eq!(result["imported"], 0);
        assert_eq!(result["preservedExistingEdits"], 1);
        assert_eq!(s.catalog.photo(id).unwrap().as_ref(), &expected);
        assert_eq!(s.visible(), &[id]);
        let after = s.catalog.to_snapshot();
        assert_eq!(s.catalog.album(album.id), Some(&album));
        assert_eq!(s.catalog.stack(stack.id), Some(&stack));
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.catalog.to_snapshot(), before);
        assert!(s.visible().is_empty());
        s.execute("edit.redo", &json!({})).unwrap();
        assert_eq!(s.catalog.to_snapshot(), after);
        assert_eq!(s.visible(), &[id]);
        std::fs::remove_dir_all(dir).unwrap();
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
        let expected_path = lcc_brand::env_os("TEST_SCRATCH")
            .or_else(|| std::env::var_os("TMPDIR"))
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("app-lrcat-fixtures")
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
    fn observed_changed_optional_wal_frames_fail_public_read() {
        for table in ["Adobe_imageDevelopSettings", "Adobe_AdditionalMetadata"] {
            let result = read_after_indexed_optional_frame_changes(table);
            assert!(result.is_err(), "{table}: an indexed source-frame change in an optional table must fail the whole read, not become a warning");
        }
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
        let path = std::env::temp_dir().join(format!("app-native-catalog-{}.lrcat", std::process::id()));
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
        let path = std::env::temp_dir().join(format!("app-{name}-{}.lrcat", std::process::id()));
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
            // the fork also names each left-out image in the warnings (issue #1588)
            assert_eq!(catalog.warnings.len(), 1, "{reason:?}: {:?}", catalog.warnings);
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
        let dir = std::env::temp_dir().join(format!("lcc-lrcat-offline-probes-{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("lcc-lrcat-present-probes-{}", std::process::id()));
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

    /// An image row whose file record is gone (Lightroom keeps such rows, and its integrity check passes them) is left
    /// out with a warning instead of failing the whole import (issue #1588).
    #[test]
    fn an_image_without_a_file_record_is_left_out() {
        use SqlValue::{Integer as I, Null as N, Text as T};
        let mut tables: Vec<(&str, &str, Vec<Vec<SqlValue>>)> =
            photo_tables(("/catalog/", "")).into_iter().map(|(n, c, row)| (n, c, vec![row])).collect();
        let images = tables.iter_mut().find(|t| t.0 == "Adobe_images").unwrap();
        // a second image whose rootFile (99) has no AgLibraryFile row
        images.2.push(vec![N, T("orphan-image".into()), I(99), T("ARW".into()), I(20), I(10), I(0), I(0)]);
        let path = std::env::temp_dir().join(format!("app-orphan-catalog-{}.lrcat", std::process::id()));
        std::fs::write(&path, native_catalog_rows(&tables, None)).unwrap();
        let catalog = read(&path).unwrap();
        assert_eq!(catalog.photos.len(), 1, "the image that can be located is imported");
        assert_eq!(catalog.photos[0].uuid, "fixture-image");
        assert!(catalog.warnings.iter().any(|w| w.contains("image 2: missing file record; left out")), "{:?}", catalog.warnings);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_root_missing_here_is_found_relative_to_the_catalog() {
        let dir = std::env::temp_dir().join(format!("app-relocated-catalog-{}", std::process::id()));
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
    /// Upstream #3674: the XMP bound is per packet: many small ones all stay however many there are, and one
    /// past the bound is left off its row, named, while the others keep theirs.
    #[test]
    fn xmp_is_bounded_per_packet_not_as_a_running_total() {
        use SqlValue::{Integer as I, Null as N, Text as T};
        let rows = vec![
            vec![N, I(1), T("<x:xmpmeta/>".into())],
            vec![N, I(2), T("<x:xmpmeta/>".into())],
            vec![N, I(3), T("<x:xmpmeta>padding</x:xmpmeta>".into())],
        ];
        let bytes = native_catalog_rows(&[("Adobe_AdditionalMetadata", "id_local INTEGER PRIMARY KEY, image, xmp", rows)], None);
        let reader = Reader { db: Database::open_with_wal(bytes, &[]).unwrap() };
        let cancel = AtomicBool::new(false);
        let (rows, warnings) = reader.xmp_table_within(&cancel, 12).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.values().filter(|r| r.contains_key("xmp")).count(), 2);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("exceed"), "{warnings:?}");
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
    fn originals_on_network_shares_are_followed_wherever_the_catalog_is() {
        // Fork difference (README → Network drives and shares): upstream refuses an original on a
        // share other than the catalog's own; the fork follows it, so a catalog on this computer with
        // its originals on a NAS imports them (missing while the NAS is unreachable, as here).
        let dir = std::env::temp_dir().join(format!("app-lrcat-remote-roots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Photos")).unwrap();
        std::fs::write(dir.join("Photos/test.ARW"), b"original").unwrap();
        let source = dir.join("source.lrcat");
        for root in [r"\192.0.2.1\share\", "file://192.0.2.1/s/"] {
            std::fs::write(&source, native_catalog(&photo_tables((root, "")))).unwrap();
            let catalog = read(&source).unwrap();
            // followed (Windows), or not a path on this computer (a UNC path read on Linux or macOS): never refused
            assert!(!matches!(catalog.photos[0].original, Original::Refused(_)), "{root}: {:?}", catalog.photos[0]);
            assert!(!catalog.photos[0].original_is_file(), "{root}: the share is unreachable here");
            assert!(!catalog.warnings.iter().any(|w| w.contains("not opened")), "{root}: {:?}", catalog.warnings);
            let mut s = crate::Session::new();
            let report = import(&mut s, &source, false).unwrap();
            assert_eq!(report["missing"].as_array().map(Vec::len), Some(1), "{root}: {report}");
            assert!(!s.catalog.is_empty(), "{root}: the photo is catalogued, as missing until the share is reachable");
        }
        if cfg!(unix) {
            // `//<local folder>/` is that local folder on Linux and macOS: followed and found
            std::fs::write(&source, native_catalog(&photo_tables((&format!("/{}/", dir.display()), "")))).unwrap();
            assert!(read(&source).unwrap().photos[0].original_is_file());
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

    /// A Lightroom catalog's Apple ProRAW edit keeps the look's parameters as a nested table
    /// (`Look = { Name = "Apple ProRaw", Parameters = { ProfileGainTableMap = 100 } }`): the raw's own
    /// local tone mapping comes over, so the photo renders as bright as in Lightroom.
    #[test]
    fn apple_proraw_look_turns_on_the_camera_local_tone_mapping() {
        let settings = |gain: u32| {
            format!(
                "s = {{ CameraProfile = \"Apple Embedded Color Profile\", Exposure2012 = 0.4, Contrast2012 = 10, Vibrance = 30, \
                 Look = {{ Name = \"Apple ProRaw\", Amount = 1, UUID = \"D866CAE82BD24C3F896B6762BDDA6C50\", \
                 Parameters = {{ Version = \"18.5.1\", ProcessVersion = \"15.4\", ConvertToGrayscale = false, ProfileGainTableMap = {gain} }} }} }}"
            )
        };
        let map = |s: &str| mapped_settings(s, crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, 1.5).unwrap();
        let (partial, _) = map(&settings(100));
        assert_eq!(partial["profile"]["camera_local_tone"], true, "{partial}");
        assert_eq!(partial["light"]["exposure"], 0.4);
        let (partial, _) = map(&settings(0));
        assert_eq!(partial["profile"]["camera_local_tone"], false, "{partial}");
        // an edit without the look leaves the option alone
        let (partial, _) = map("s = { Exposure2012 = 0.4 }");
        assert!(partial.pointer("/profile/camera_local_tone").is_none(), "{partial}");
    }

    /// Issue #1234: develop settings are streamed into the photos, and a develop table that can't be read fails the
    /// import with a clear error instead of a warning that every photo arrives unedited.
    #[test]
    fn develop_settings_reach_the_photos_and_an_unreadable_table_fails_the_import() {
        use SqlValue::{Integer as I, Null as N, Text as T};
        let mut tables = photo_tables(("/catalog/", ""));
        tables.push(("Adobe_imageDevelopSettings", "id_local INTEGER PRIMARY KEY, image, text", vec![N, I(1), T("s = { Exposure2012 = 1 }".into())]));
        let path = std::env::temp_dir().join(format!("app-develop-catalog-{}.lrcat", std::process::id()));
        let mut bytes = native_catalog(&tables);
        std::fs::write(&path, &bytes).unwrap();
        let catalog = read(&path).unwrap();
        assert_eq!(catalog.photos[0].settings, "s = { Exposure2012 = 1 }");
        // The develop table's page (the fifth table, page 6) turns into an index page this reader refuses.
        bytes[5 * 2048] = 0x0a;
        std::fs::write(&path, &bytes).unwrap();
        let error = read(&path).unwrap_err();
        assert!(error.contains("develop settings could not be read"), "{error}");
        std::fs::remove_file(path).unwrap();
    }

    /// Every shape of `Adobe_images.captureTime`, read from a catalog file and imported: a year or a month alone (a
    /// scanned print's date; Lightroom also has it in the photo's XMP as `photoshop:DateCreated`) is kept as stated,
    /// as is every full time; what isn't a date is left out with one warning naming the image, its value stays in the
    /// import archive, and nothing panics.
    #[test]
    fn every_capture_time_shape_imports_as_stated_or_is_left_out() {
        use crate::lightroom_fixture::{self as fixture, Field, Table, text as t};
        // (captureTime, what the library keeps; None = left out with a warning, or nothing to keep)
        let shapes: &[(&str, Option<&str>)] = &[
            ("1983", Some("1983")),
            ("1983-04", Some("1983-04")),
            ("1983-04-12", Some("1983-04-12")),
            ("2024-01-16T12:34:56", Some("2024-01-16T12:34:56")),
            ("2024-01-16T12:34:56.12", Some("2024-01-16T12:34:56.12")),
            ("2024-01-16T12:34:56.123", Some("2024-01-16T12:34:56.123")),
            ("2024-01-16T12:34:56.123456", Some("2024-01-16T12:34:56.123456")),
            ("2024-01-16T12:34", Some("2024-01-16T12:34")),
            ("2024-01-16T12:34:56-07:00", Some("2024-01-16T12:34:56-07:00")),
            ("2024-01-16T12:34:56Z", Some("2024-01-16T12:34:56Z")),
            // a date with text after it reads and is kept, as before: Edit Capture Time counts it `unreadable`
            ("2024-01-16T12:34:56junk", Some("2024-01-16T12:34:56junk")),
            ("", None),
            ("1983-13", None),
            ("0000", None),
            ("2026-02-30", None),
            ("garbage", None),
            ("１９８３", None),
            ("1983-０4", None),
        ];
        let partial = |c: &str| c == "1983" || c == "1983-04";
        // Lightroom's `Adobe_AdditionalMetadata.xmp` for a file that states only `photoshop:DateCreated`
        let xmp = |date: &str| {
            fixture::compressed(&format!(
                "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
                 <rdf:Description rdf:about=\"\" xmlns:photoshop=\"http://ns.adobe.com/photoshop/1.0/\" \
                 photoshop:DateCreated=\"{date}\"/></rdf:RDF></x:xmpmeta>"
            ))
        };
        let n = |i: usize| i64::try_from(i + 1).unwrap();
        let tables = vec![
            Table {
                name: "AgLibraryRootFolder",
                columns: "id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, absolutePath UNIQUE NOT NULL DEFAULT '', name NOT NULL DEFAULT '', relativePathFromCatalog",
                rows: vec![vec![Field::Integer(1), t("root-1"), t("/missing/scans/"), t("scans"), Field::Null]],
            },
            Table {
                name: "AgLibraryFolder",
                columns: "id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, pathFromRoot NOT NULL DEFAULT '', rootFolder INTEGER NOT NULL DEFAULT 0",
                rows: vec![vec![Field::Integer(1), t("folder-1"), t("Photos/"), Field::Integer(1)]],
            },
            Table {
                name: "AgLibraryFile",
                columns: "id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, baseName NOT NULL DEFAULT '', extension NOT NULL DEFAULT '', folder INTEGER NOT NULL DEFAULT 0, idx_filename NOT NULL DEFAULT ''",
                rows: (0..shapes.len())
                    .map(|i| {
                        vec![
                            Field::Integer(n(i)),
                            t(&format!("file-{i}")),
                            t(&format!("scan{i}")),
                            t("jpg"),
                            Field::Integer(1),
                            t(&format!("scan{i}.jpg")),
                        ]
                    })
                    .collect(),
            },
            Table {
                name: "Adobe_images",
                columns: "id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, captureTime, fileFormat NOT NULL DEFAULT 'unset', fileHeight, fileWidth, rootFile INTEGER NOT NULL DEFAULT 0",
                rows: shapes
                    .iter()
                    .enumerate()
                    .map(|(i, (c, _))| {
                        let captured = if c.is_empty() { Field::Null } else { t(c) };
                        vec![
                            Field::Integer(n(i)),
                            t(&format!("image-{i}")),
                            captured,
                            t("JPG"),
                            Field::Integer(10),
                            Field::Integer(20),
                            Field::Integer(n(i)),
                        ]
                    })
                    .collect(),
            },
            Table {
                name: "Adobe_AdditionalMetadata",
                columns: "id_local INTEGER PRIMARY KEY, image INTEGER, xmp",
                rows: shapes
                    .iter()
                    .enumerate()
                    .filter(|(_, (c, _))| partial(c))
                    .map(|(i, (c, _))| vec![Field::Integer(n(i)), Field::Integer(n(i)), Field::Blob(xmp(c))])
                    .collect(),
            },
        ];
        let dir = fixture::dir("capture-time-shapes");
        let path = dir.join("shapes.lrcat");
        fixture::write(&path, &tables);
        let catalog = read(&path).unwrap();
        assert_eq!(catalog.photos.len(), shapes.len(), "{:?}", catalog.skipped);
        // the catalog's own value stays in the import archive, whatever the library keeps
        let archived = fixture::read_archive(&crate::lightroom_archive::store(&dir.join("Interop"), &catalog).unwrap().path);
        for photo in archived["photos"].as_array().unwrap() {
            let i = usize::try_from(photo["source_id"].as_i64().unwrap() - 1).unwrap();
            let (c, _) = shapes[i];
            let want = if c.is_empty() { Value::Null } else { json!(c) };
            assert_eq!(photo["image"]["captureTime"], want, "archived captureTime of image {}", i + 1);
        }
        let mut s = crate::Session::new();
        let r = apply(&mut s, catalog, false).unwrap();
        let mut warnings = Vec::new();
        for (i, (c, kept)) in shapes.iter().enumerate() {
            let id = PhotoId(r["mapping"][(i + 1).to_string()].as_u64().unwrap_or_else(|| panic!("image {} not imported: {r}", i + 1)));
            assert_eq!(s.catalog.photo(id).unwrap().captured.as_deref(), *kept, "captureTime “{c}”: {r}");
            if kept.is_none() && !c.is_empty() {
                warnings.push(format!("image {}: capture time “{c}” is not a date; imported without it", i + 1));
            }
        }
        let got: Vec<&str> = r["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).filter(|w| w.contains("capture time")).collect();
        assert_eq!(got, warnings, "one warning for each time that isn't a date, none for a partial one: {r}");
        // a reimport over the same records sets the same times again, partial ones included
        let again = apply(&mut s, read(&path).unwrap(), false).unwrap();
        assert_eq!(again["mapping"], r["mapping"]);
        for (i, (c, kept)) in shapes.iter().enumerate() {
            let id = PhotoId(r["mapping"][(i + 1).to_string()].as_u64().unwrap());
            assert_eq!(s.catalog.photo(id).unwrap().captured.as_deref(), *kept, "reimported captureTime “{c}”");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The reported failure: photos whose originals are on disk are added as files first and get Lightroom's capture
    /// time through `SetCaptured`, so one year-only date used to fail the whole import ("not a date: 1983"). Now every
    /// photo imports: a partial date as stated, one that isn't a date without it.
    #[test]
    fn a_partial_capture_time_on_a_file_on_disk_doesnt_fail_the_import() {
        use crate::lightroom_fixture::{self as fixture, Field, Table, text as t};
        let shapes = ["1983", "1983-04", "2024-01-16T12:34:56", "garbage"];
        let dir = fixture::dir("capture-time-on-disk");
        let originals = dir.join("originals");
        std::fs::create_dir_all(originals.join("Photos")).unwrap();
        for i in 0..shapes.len() {
            crate::tests_xmp::write_png(&originals.join("Photos").join(format!("scan{i}.png")), u8::try_from(i).unwrap());
        }
        let n = |i: usize| i64::try_from(i + 1).unwrap();
        let tables = vec![
            Table {
                name: "AgLibraryRootFolder",
                columns: "id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, absolutePath UNIQUE NOT NULL DEFAULT '', name NOT NULL DEFAULT '', relativePathFromCatalog",
                rows: vec![vec![Field::Integer(1), t("root-1"), Field::Text(format!("{}/", originals.display())), t("originals"), Field::Null]],
            },
            Table {
                name: "AgLibraryFolder",
                columns: "id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, pathFromRoot NOT NULL DEFAULT '', rootFolder INTEGER NOT NULL DEFAULT 0",
                rows: vec![vec![Field::Integer(1), t("folder-1"), t("Photos/"), Field::Integer(1)]],
            },
            Table {
                name: "AgLibraryFile",
                columns: "id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, baseName NOT NULL DEFAULT '', extension NOT NULL DEFAULT '', folder INTEGER NOT NULL DEFAULT 0, idx_filename NOT NULL DEFAULT ''",
                rows: (0..shapes.len())
                    .map(|i| {
                        vec![
                            Field::Integer(n(i)),
                            t(&format!("file-{i}")),
                            t(&format!("scan{i}")),
                            t("png"),
                            Field::Integer(1),
                            t(&format!("scan{i}.png")),
                        ]
                    })
                    .collect(),
            },
            Table {
                name: "Adobe_images",
                columns: "id_local INTEGER PRIMARY KEY, id_global UNIQUE NOT NULL, captureTime, fileFormat NOT NULL DEFAULT 'unset', fileHeight, fileWidth, rootFile INTEGER NOT NULL DEFAULT 0",
                rows: shapes
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        vec![
                            Field::Integer(n(i)),
                            t(&format!("image-{i}")),
                            t(c),
                            t("PNG"),
                            Field::Integer(8),
                            Field::Integer(8),
                            Field::Integer(n(i)),
                        ]
                    })
                    .collect(),
            },
        ];
        let path = dir.join("on-disk.lrcat");
        fixture::write(&path, &tables);
        let mut s = crate::Session::new().with_fs();
        let r = apply(&mut s, read(&path).unwrap(), false).unwrap();
        assert_eq!(r["missing"], json!([]), "the originals are on disk: {r}");
        let kept: Vec<_> = (0..shapes.len())
            .map(|i| {
                let id = PhotoId(r["mapping"][(i + 1).to_string()].as_u64().unwrap_or_else(|| panic!("image {} not imported: {r}", i + 1)));
                s.catalog.photo(id).unwrap().captured.clone()
            })
            .collect();
        assert_eq!(kept[..3], [Some("1983".into()), Some("1983-04".into()), Some("2024-01-16T12:34:56".into())], "{r}");
        assert_ne!(kept[3].as_deref(), Some("garbage"), "{r}");
        assert!(r["warnings"].to_string().contains("image 4: capture time “garbage” is not a date"), "{r}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A record an earlier version stored with a capture time that isn't a date keeps it on a reimport, with a warning,
    /// rather than take Lightroom's (undoing the import could not put it back); a new virtual copy doesn't inherit it.
    #[test]
    fn a_stored_capture_time_that_is_not_a_date_is_kept_and_not_inherited() {
        let mut s = crate::Session::new();
        let r = apply(&mut s, sample(), false).unwrap();
        let master = PhotoId(r["mapping"]["1"].as_u64().unwrap());
        let mut broken = (**s.catalog.photo(master).unwrap()).clone();
        broken.captured = Some("garbage".into());
        // as an earlier version stored it, past the check `SetCaptured` makes
        s.commit("Earlier import", Op::Batch { ops: vec![Op::RemovePhoto { id: master }, Op::AddPhoto { photo: Box::new(broken) }] }).unwrap();
        // Lightroom's own time isn't applied over it
        let mut data = sample();
        data.photos.retain(|p| p.source_id == 1);
        let again = apply(&mut s, data, false).unwrap();
        assert_eq!(s.catalog.photo(master).unwrap().captured.as_deref(), Some("garbage"), "{again}");
        assert!(again["warnings"].to_string().contains("image 1: the library's capture time “garbage” is not a date"), "{again}");
        // a copy new this time starts from the stored master (whose time this catalog doesn't state) but not its time
        let mut data = sample();
        data.photos.iter_mut().for_each(|p| drop(p.image.remove("captureTime")));
        for p in &mut data.photos {
            if p.source_id == 2 {
                p.source_id = 3;
                p.uuid = "test-copy-2".into();
                p.image.insert("id_local".into(), json!(3));
            }
        }
        let again = apply(&mut s, data, false).unwrap();
        assert_eq!(s.catalog.photo(master).unwrap().captured.as_deref(), Some("garbage"), "{again}");
        let copy = PhotoId(again["mapping"]["3"].as_u64().unwrap());
        assert_eq!(s.catalog.photo(copy).unwrap().captured, None, "{again}");
        assert!(again["warnings"].to_string().contains("image 3: inherited capture time “garbage” is not a date"), "{again}");
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.catalog.photo(master).unwrap().captured.as_deref(), Some("garbage"), "undo leaves it as it was");
    }

    /// Settings read from a file's XMP while this import adds it are not edits the library had: the
    /// catalog's settings and history win, for embedded XMP and sidecars alike, without
    /// `updateExisting`, and a setting the catalog leaves out (Lightroom omits those at zero) is
    /// taken neither from the file nor from a default import preset.
    #[test]
    fn a_photo_this_import_adds_takes_the_catalogs_settings_over_its_files_xmp() {
        let dir = crate::tests_xmp::temp_dir("lightroom-embedded-edits");
        let saved = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="+1.00" crs:Dehaze="0" crs:Clarity2012="+20"/></rdf:RDF></x:xmpmeta>"#;
        let dng = dir.join("embedded.dng");
        std::fs::write(&dng, crate::tests_xmp::synthetic_dng_with(Some(saved), Default::default())).unwrap();
        let raw = dir.join("sidecar.dng");
        std::fs::write(&raw, crate::tests_xmp::synthetic_dng_with(None, Default::default())).unwrap();
        std::fs::write(dir.join("sidecar.xmp"), saved).unwrap();
        let photo = |id: i64, path: &Path| CatalogPhoto {
            source_id: id,
            uuid: format!("photo-{id}"),
            path: path.to_string_lossy().into(),
            image: row(json!({"id_local":id,"fileFormat":"DNG","fileWidth":32,"fileHeight":24})),
            settings: "s = { Exposure2012 = 1, Dehaze = -50 }".into(),
            xmp: String::new(),
            keywords: Vec::new(),
            history: vec![
                row(json!({"id_local":id * 10,"dateCreated":1.0,"name":"Exposure","text":"s = { Exposure2012 = 1 }"})),
                row(json!({"id_local":id * 10 + 1,"dateCreated":2.0,"name":"Dehaze","text":"s = { Exposure2012 = 1, Dehaze = -50 }"})),
            ],
            snapshots: Vec::new(),
            original: Original::Local,
            as_shot_white: None,
            capture_one: None,
        };
        let data = CatalogImport {
            source: "test.lrcat".into(),
            photos: vec![photo(1, &dng), photo(2, &raw)],
            collections: Vec::new(),
            members: Vec::new(),
            collection_content: Vec::new(),
            stacks: Vec::new(),
            warnings: Vec::new(),
            skipped: Vec::new(),
        };
        let mut s = crate::Session::new().with_fs();
        // a default import preset sets a value the catalog leaves out too
        let added = s.add_presets(vec![lcc_develop::Preset {
            id: "look".into(),
            name: "Look".into(),
            group: "g".into(),
            settings: json!({"effects": {"texture": 12.0}}),
            favorite: false,
            builtin: false,
        }]);
        s.import_defaults.raw_preset = added.first().cloned();
        let r = apply(&mut s, data, false).unwrap();
        assert_eq!(r["imported"], 2, "{r}");
        assert_eq!(r["preservedExistingEdits"], 0, "the library held nothing before the import: {r}");
        assert_eq!(r["historySteps"], 4, "each photo's two Lightroom steps are imported: {r}");
        for p in s.catalog.photos() {
            assert_eq!(p.develop.light.exposure, 1.0, "{}: the file's saved exposure and the catalog's agree", p.file_name);
            assert_eq!(p.develop.effects.dehaze, -50.0, "{}: the catalog's later edit is kept, not the file's", p.file_name);
            assert_eq!(p.develop.effects.clarity, 0.0, "{}: a setting the catalog leaves out is not taken from the file", p.file_name);
            assert_eq!(p.develop.effects.texture, 0.0, "{}: nor from a default import preset", p.file_name);
            let labels: Vec<_> = p.history.iter().map(|h| h.label.as_str()).collect();
            assert_eq!(labels, ["Exposure", "Dehaze"], "{}: the photo's Lightroom history is imported", p.file_name);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A catalog row whose develop text can't be read leaves a new photo with what its file's XMP and
    /// the default import preset gave it, not the camera's defaults (and shows that both reach it).
    #[test]
    fn unreadable_catalog_settings_leave_a_new_photos_file_settings() {
        let dir = crate::tests_xmp::temp_dir("lightroom-unreadable-settings");
        let saved = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Clarity2012="+20"/></rdf:RDF></x:xmpmeta>"#;
        let dng = dir.join("embedded.dng");
        std::fs::write(&dng, crate::tests_xmp::synthetic_dng_with(Some(saved), Default::default())).unwrap();
        let data = CatalogImport {
            source: "test.lrcat".into(),
            photos: vec![CatalogPhoto {
                source_id: 1,
                uuid: "photo-1".into(),
                path: dng.to_string_lossy().into(),
                image: row(json!({"id_local":1,"fileFormat":"DNG","fileWidth":32,"fileHeight":24})),
                settings: format!("s = {}0{}", "{".repeat(1000), "}".repeat(1000)),
                xmp: String::new(),
                keywords: Vec::new(),
                history: Vec::new(),
                snapshots: Vec::new(),
                original: Original::Local,
                as_shot_white: None,
                capture_one: None,
            }],
            collections: Vec::new(),
            members: Vec::new(),
            collection_content: Vec::new(),
            stacks: Vec::new(),
            warnings: Vec::new(),
            skipped: Vec::new(),
        };
        let mut s = crate::Session::new().with_fs();
        let added = s.add_presets(vec![lcc_develop::Preset {
            id: "look".into(),
            name: "Look".into(),
            group: "g".into(),
            settings: json!({"effects": {"texture": 12.0}}),
            favorite: false,
            builtin: false,
        }]);
        s.import_defaults.raw_preset = added.first().cloned();
        let r = apply(&mut s, data, false).unwrap();
        assert_eq!(r["imported"], 1, "{r}");
        assert!(
            r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().is_some_and(|w| w.contains("settings"))),
            "the unreadable settings are reported: {r}"
        );
        let p = s.catalog.photos().next().unwrap();
        assert_eq!(p.develop.effects.clarity, 20.0, "the file's XMP is kept when the catalog's settings can't be read");
        assert_eq!(p.develop.effects.texture, 12.0, "and so is the default import preset");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A browsed (Local) photo joins the library through the import's add step, or, when its original
    /// is offline, through the catalog import alone. Settings browsing read from its file are not the
    /// library's edits and give way to the catalog's settings and history; a change made after
    /// browsing is kept, as for any other existing record.
    #[test]
    fn a_browsed_photo_keeps_only_edits_made_after_browsing() {
        use crate::lightroom_fixture::{self as fixture, Table};
        for (changed, offline) in [(true, false), (false, false), (true, true), (false, true)] {
            let dir = fixture::dir(&format!("browsed-edits-{changed}-{offline}"));
            let originals = dir.join("originals");
            std::fs::create_dir_all(originals.join("Photos")).unwrap();
            let original = originals.join("Photos/test.ARW");
            if !offline {
                std::fs::write(&original, b"original").unwrap();
            }
            let mut tables = fixture::one_photo(&originals, "s = { Dehaze = -50 }");
            tables.push(Table {
                name: "Adobe_libraryImageDevelopHistoryStep",
                columns: fixture::HISTORY_COLUMNS,
                rows: vec![fixture::history_step(2, 1, 10.0, "Dehaze", fixture::text("s = { Dehaze = -50 }"))],
            });
            let source = dir.join("source.lrcat");
            fixture::write(&source, &tables);
            let mut s = crate::Session::new();
            s.media.file_probe =
                Some(Arc::new(|_| Ok(crate::media::ProbeInfo { format: "ARW".into(), width: 20, height: 10, ..Default::default() })));
            let id = s.catalog.alloc_photo_id();
            let mut browsed = Photo::new(id, Source::File { path: original.to_string_lossy().into() }, "test.ARW", "ARW", 20, 10, "before");
            browsed.local = true;
            // as browsing a raw whose file's XMP holds develop settings leaves it
            let mut from_file = (*browsed.develop).clone();
            from_file.effects.dehaze = 10.0;
            // a setting the catalog's develop text leaves out (Lightroom omits those at zero)
            from_file.effects.clarity = 15.0;
            browsed.develop = Arc::new(from_file);
            browsed.edited = Some("before".into());
            browsed.set_local_baseline();
            s.commit("Browse folder", Op::AddPhoto { photo: Box::new(browsed) }).unwrap();
            if changed {
                let mut edit = (*s.catalog.photo(id).unwrap().develop).clone();
                edit.effects.dehaze = 25.0;
                s.set_develop(id, edit, "Personal edit").unwrap();
            }
            let mut job = crate::lightroom_job::LightroomJob::new(&mut s, source, false).unwrap();
            let prepared = job.prepare(&AtomicBool::new(false)).unwrap();
            let report = crate::lightroom_job::commit_prepared(&mut s, prepared).unwrap().report;
            assert_eq!(report["imported"], 1, "the browsed photo joins the library (offline: {offline}): {report}");
            let photo = s.catalog.photo(id).unwrap();
            assert!(!photo.local);
            let catalog_step = photo.history.iter().any(|h| h.label == "Dehaze");
            if changed {
                assert_eq!(report["preservedExistingEdits"], 1, "the edit came before the import (offline: {offline}): {report}");
                assert_eq!(photo.develop.effects.dehaze, 25.0, "the library's edit is kept (offline: {offline})");
                assert_eq!(photo.develop.effects.clarity, 15.0, "the kept settings are kept whole (offline: {offline})");
                assert!(!catalog_step, "the library's history is kept too (offline: {offline}): {:?}", photo.history);
            } else {
                assert_eq!(report["preservedExistingEdits"], 0, "browsing read the file, nobody edited it (offline: {offline}): {report}");
                assert_eq!(photo.develop.effects.dehaze, -50.0, "the catalog's settings replace the file's (offline: {offline})");
                assert_eq!(photo.develop.effects.clarity, 0.0, "a setting the catalog leaves out is not the file's (offline: {offline})");
                assert!(catalog_step, "the catalog's history is imported (offline: {offline}): {:?}", photo.history);
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    /// 2026-09-29T12:00:00Z and 2020-05-01T08:00:00Z, as Lightroom's `touchTime` (seconds since 2001-01-01T00:00:00Z)
    /// and as Unix seconds. Smart albums are judged a day after `RECENT` ([`smart_album_photos`]).
    const RECENT: (f64, i64) = (812_376_000.0, 1_790_683_200);
    const OLD: (f64, i64) = (610_012_800.0, 1_588_320_000);
    /// An imported photo's edit date, as Unix seconds.
    fn edit_instant(s: &crate::Session, id: PhotoId) -> Option<i64> {
        s.catalog.photo(id).unwrap().edited.as_deref().and_then(lcc_catalog::dates::instant)
    }
    /// A photo Lightroom holds no develop settings for.
    fn plain_photo(source_id: i64, image: Value) -> CatalogPhoto {
        let mut row = row(json!({"id_local":source_id,"fileFormat":"JPEG","fileWidth":20,"fileHeight":10}));
        if let Value::Object(extra) = image {
            row.extend(extra);
        }
        CatalogPhoto {
            source_id,
            uuid: format!("plain-{source_id}"),
            path: format!("/missing/plain-{source_id}.jpg"),
            image: row,
            settings: String::new(),
            xmp: String::new(),
            keywords: Vec::new(),
            history: Vec::new(),
            snapshots: Vec::new(),
            original: Original::Local,
            as_shot_white: None,
            capture_one: None,
        }
    }
    /// `sample()` (a virtual copy, then its master) with a Copy Name on the master, Lightroom's edit times, a photo
    /// with no develop settings (image 3), and smart collections that test both.
    fn copy_name_and_edit_date_sample() -> CatalogImport {
        let mut data = sample();
        data.photos[0].image.insert("touchTime".into(), json!(OLD.0));
        data.photos[1].image.insert("copyName".into(), json!("Web title"));
        data.photos[1].image.insert("touchTime".into(), json!(RECENT.0));
        data.photos.push(plain_photo(3, json!({"touchTime": RECENT.0})));
        data.collections.push(row(json!({"id_local":11,"name":"Has Copy Name","creationId":"com.adobe.ag.library.smart_collection"})));
        data.collections.push(row(json!({"id_local":12,"name":"Recently modified","creationId":"com.adobe.ag.library.smart_collection"})));
        data.collection_content = vec![
            row(json!({"collection":11,"owningModule":"ag.library.smart_collection",
                "content":"s = { { criteria = \"copyname\", operation = \"notEmpty\" } }"})),
            row(json!({"collection":12,"owningModule":"ag.library.smart_collection",
                "content":"s = { { criteria = \"touchTime\", operation = \"inLast\", value = 2, value_units = \"days\" } }"})),
        ];
        data
    }
    /// A smart album's photos, judged a day after `RECENT`.
    fn smart_album_photos(s: &crate::Session, name: &str) -> Vec<PhotoId> {
        let album = s.catalog.albums().find(|a| a.name == name).unwrap();
        assert!(album.is_smart(), "{name} is a smart album");
        lcc_catalog::rules::set_now(lcc_catalog::dates::local(RECENT.1 + 86_400));
        let photos = s.catalog.album_photos(album.id);
        lcc_catalog::rules::set_now(None);
        photos
    }

    #[test]
    fn copy_names_come_across_for_every_photo() {
        let mut s = crate::Session::new();
        let r = apply(&mut s, copy_name_and_edit_date_sample(), false).unwrap();
        let master = PhotoId(r["mapping"]["1"].as_u64().unwrap());
        let copy = PhotoId(r["mapping"]["2"].as_u64().unwrap());
        let plain = PhotoId(r["mapping"]["3"].as_u64().unwrap());
        let m = s.catalog.photo(master).unwrap();
        assert_eq!((m.copy_of, m.copy_name.as_deref()), (None, Some("Web title")), "a photo that isn't a virtual copy keeps its Copy Name");
        let c = s.catalog.photo(copy).unwrap();
        assert_eq!((c.copy_of, c.copy_name.as_deref()), (Some(master), Some("Mono")), "a virtual copy keeps its Copy Name");
        assert_eq!(s.catalog.photo(plain).unwrap().copy_name, None, "a photo with no Copy Name gets none");
        let named = smart_album_photos(&s, "Has Copy Name");
        assert!(named.contains(&master) && named.contains(&copy) && !named.contains(&plain), "{named:?}");
        assert_eq!(s.execute("photo.inspect", &json!({"id": master.0})).unwrap()["copy_name"], "Web title");
    }

    #[test]
    fn each_photo_is_dated_at_its_last_edit_in_lightroom() {
        let mut s = crate::Session::new();
        let r = apply(&mut s, copy_name_and_edit_date_sample(), false).unwrap();
        let id = |n: &str| PhotoId(r["mapping"][n].as_u64().unwrap());
        let (master, copy, plain) = (id("1"), id("2"), id("3"));
        assert_eq!(edit_instant(&s, master), Some(RECENT.1), "a photo with develop settings: Lightroom's edit time, not the import's");
        assert_eq!(edit_instant(&s, copy), Some(OLD.1));
        assert_eq!(edit_instant(&s, plain), Some(RECENT.1), "a photo with no develop settings is dated too");
        assert!(!s.catalog.photo(plain).unwrap().is_edited(), "a date doesn't make a photo edited");
        let recent = smart_album_photos(&s, "Recently modified");
        assert!(recent.contains(&master) && recent.contains(&plain) && !recent.contains(&copy), "{recent:?}");
        // an edit in the app dates the photo again (the test clock)
        let mut settings = (*s.catalog.photo(plain).unwrap().develop).clone();
        settings.light.exposure = 0.5;
        s.set_develop(plain, settings, "Personal edit").unwrap();
        assert_eq!(s.catalog.photo(plain).unwrap().edited.as_deref(), Some("2026-09-30T12:00:00"));
    }

    #[test]
    fn a_missing_or_broken_lightroom_edit_time_never_fails_the_import() {
        let mut s = crate::Session::new();
        let mut data = sample();
        // the master has develop settings and no `touchTime`; the plain photos have none and a broken one
        let broken = [json!(0), json!(-5.0), json!("yesterday"), json!(1e15), json!(1e300), Value::Null];
        for (n, t) in broken.iter().enumerate() {
            data.photos.push(plain_photo(3 + n as i64, json!({"touchTime": t})));
        }
        data.photos.push(plain_photo(20, json!({})));
        data.photos[0].image.insert("touchTime".into(), json!(OLD.0));
        data.photos[1].image.remove("touchTime");
        let r = apply(&mut s, data, false).unwrap();
        let id = |n: i64| PhotoId(r["mapping"][n.to_string()].as_u64().unwrap());
        assert_eq!(s.catalog.photo(id(1)).unwrap().edited.as_deref(), Some("2026-09-30T12:00:00"), "with settings: dated at the import");
        for n in (3..3 + broken.len() as i64).chain([20]) {
            assert_eq!(s.catalog.photo(id(n)).unwrap().edited, None, "image {n} without settings: no edit date");
        }
        assert_eq!(edit_instant(&s, id(2)), Some(OLD.1));
        let warnings: Vec<&str> = r["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).filter(|w| w.contains("edit time")).collect();
        assert_eq!(warnings.len(), 1, "one warning: {warnings:?}");
        let named = warnings[0].strip_prefix("images ").and_then(|w| w.split(':').next()).unwrap_or_default();
        assert_eq!(named, "1, 3, 4, 5, 6, 7, 8, 20", "the warning names each image whose time can't be read: {}", warnings[0]);
    }

    #[test]
    fn a_photo_whose_settings_cant_be_read_is_still_dated_from_lightroom() {
        let mut s = crate::Session::new();
        let mut data = copy_name_and_edit_date_sample();
        data.photos[2].settings = format!("{}0{}", "{".repeat(1000), "}".repeat(1000));
        let r = apply(&mut s, data, false).unwrap();
        let plain = PhotoId(r["mapping"]["3"].as_u64().unwrap());
        assert!(r["warnings"].as_array().unwrap().iter().any(|w| w.as_str().is_some_and(|w| w.starts_with("image 3 settings"))), "{r}");
        assert_eq!(edit_instant(&s, plain), Some(RECENT.1));
    }

    #[test]
    fn reimporting_brings_a_changed_copy_name_and_edit_time_across() {
        let mut s = crate::Session::new();
        let mut data = copy_name_and_edit_date_sample();
        data.photos[1].image.insert("copyName".into(), json!("Old title"));
        data.photos[1].image.insert("touchTime".into(), json!(OLD.0));
        let r = apply(&mut s, data, false).unwrap();
        let master = PhotoId(r["mapping"]["1"].as_u64().unwrap());
        let album = s.catalog.albums().find(|a| a.name == "Keepers").unwrap().id;
        assert!(s.catalog.album_photos(album).contains(&master));
        let before = (**s.catalog.photo(master).unwrap()).clone();
        let r = apply(&mut s, copy_name_and_edit_date_sample(), true).unwrap();
        assert_eq!(r["mapping"]["1"], master.0, "the same record");
        assert_eq!(s.catalog.photo(master).unwrap().copy_name.as_deref(), Some("Web title"));
        assert_eq!(edit_instant(&s, master), Some(RECENT.1));
        assert!(s.catalog.album_photos(album).contains(&master), "the photo stays in its album");
        s.undo_step().unwrap();
        assert_eq!(s.catalog.photo(master).unwrap().as_ref(), &before, "one undo step puts the earlier name and date back");
        assert!(s.catalog.album_photos(album).contains(&master));
    }

    #[test]
    fn a_photo_imported_without_settings_takes_lightrooms_later_ones() {
        let mut s = crate::Session::new();
        let data = copy_name_and_edit_date_sample();
        let r = apply(&mut s, data.clone(), false).unwrap();
        let plain = PhotoId(r["mapping"]["3"].as_u64().unwrap());
        assert!(s.catalog.photo(plain).unwrap().edited.is_some(), "dated, though unedited");
        // edited in Lightroom since, imported again without updating existing photos
        let mut later = data;
        later.photos[2].settings = "s = { Exposure2012 = 1.5 }".into();
        let r = apply(&mut s, later, false).unwrap();
        assert_eq!(s.catalog.photo(plain).unwrap().develop.light.exposure, 1.5, "Lightroom's settings apply");
        // (this test's imports keep no index, so the virtual copy comes in again as a new photo)
        assert_eq!(r["preservedExistingEdits"], 1, "only the master, imported with settings, keeps them: {r}");
    }

    /// A side-table row whose `image` is missing, NULL, text or not a whole number names no photo. It used to read as
    /// photo 0 (or 2.5 as photo 2), so a catalog with a photo whose `id_local` is 0 gave that photo other rows' keywords,
    /// develop settings, XMP, history, snapshots and album membership. Such rows are left out with a warning per table;
    /// the rows that name photo 0 still go to it. Every table is checked before the test fails, so each one shows.
    #[test]
    fn side_table_rows_without_a_photo_id_go_to_no_photo() {
        use SqlValue::{Integer as I, Null as N, Real as R, Text as T};
        let dir = std::env::temp_dir().join(format!("lrcat-side-image-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("side.lrcat");
        let mut tables = one_row(photo_tables(("/catalog/", "")));
        tables[2].2.push(vec![I(2), I(1), T("two.ARW".into())]);
        tables[3].2 = vec![
            vec![I(0), T("zero".into()), I(1), T("ARW".into()), I(20), I(10), I(1), I(0)],
            vec![I(2), T("two".into()), I(2), T("ARW".into()), I(20), I(10), I(2), I(0)],
        ];
        let xmp = |title: &str| {
            T(format!(
                "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
                 <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title><rdf:Alt>\
                 <rdf:li xml:lang=\"x-default\">{title}</rdf:li></rdf:Alt></dc:title></rdf:Description></rdf:RDF></x:xmpmeta>"
            ))
        };
        // each table: photo 0's own row first, then rows that name no photo (a later row for the same key used to win)
        tables.extend([
            (
                "Adobe_imageDevelopSettings",
                "id_local INTEGER PRIMARY KEY, image, text",
                vec![vec![N, I(0), T("s = { Exposure2012 = 0.5 }".into())], vec![N, N, T("s = { Exposure2012 = 1.75 }".into())]],
            ),
            (
                "Adobe_AdditionalMetadata",
                "id_local INTEGER PRIMARY KEY, image, xmp",
                vec![vec![N, I(0), xmp("Own title")], vec![N, T("x".into()), xmp("Stray title")]],
            ),
            (
                "AgLibraryKeyword",
                "id_local INTEGER PRIMARY KEY, name",
                ["Own", "Two", "StrayNull", "StrayText", "StrayReal"].iter().map(|k| vec![N, T((*k).into())]).collect(),
            ),
            (
                "AgLibraryKeywordImage",
                "id_local INTEGER PRIMARY KEY, image, tag",
                vec![vec![N, I(0), I(1)], vec![N, I(2), I(2)], vec![N, N, I(3)], vec![N, T("abc".into()), I(4)], vec![N, R(2.5), I(5)]],
            ),
            (
                "Adobe_libraryImageDevelopHistoryStep",
                "id_local INTEGER PRIMARY KEY, image, name",
                vec![vec![N, I(0), T("Own step".into())], vec![N, N, T("Stray step".into())]],
            ),
            (
                "Adobe_libraryImageDevelopSnapshot",
                "id_local INTEGER PRIMARY KEY, image, name",
                vec![vec![N, I(0), T("Own snapshot".into())], vec![N, N, T("Stray snapshot".into())]],
            ),
            ("AgLibraryCollection", "id_local INTEGER PRIMARY KEY, name", vec![vec![N, T("Own album".into())], vec![N, T("Stray album".into())]]),
            ("AgLibraryCollectionImage", "id_local INTEGER PRIMARY KEY, collection, image", vec![vec![N, I(1), I(0)], vec![N, I(2), N]]),
        ]);
        std::fs::write(&path, native_catalog_rows(&tables, None)).unwrap();
        let catalog = read(&path).unwrap();
        let photo = |id: i64| catalog.photos.iter().find(|p| p.source_id == id).unwrap();
        let names = |rows: &[Row]| rows.iter().map(|r| text(r, "name").to_string()).collect::<Vec<_>>();
        let mut wrong = Vec::new();
        let mut check = |what: &str, ok: bool, got: String| {
            if !ok {
                wrong.push(format!("{what}: {got}"));
            }
        };
        check("photo 0 keywords", photo(0).keywords == ["Own"], format!("{:?}", photo(0).keywords));
        check("photo 0 develop", photo(0).settings == "s = { Exposure2012 = 0.5 }", photo(0).settings.clone());
        check("photo 0 XMP", photo(0).xmp.contains("Own title") && !photo(0).xmp.contains("Stray"), photo(0).xmp.clone());
        check("photo 0 history", names(&photo(0).history) == ["Own step"], format!("{:?}", names(&photo(0).history)));
        check("photo 0 snapshots", names(&photo(0).snapshots) == ["Own snapshot"], format!("{:?}", names(&photo(0).snapshots)));
        check("photo 2 keywords", photo(2).keywords == ["Two"], format!("{:?}", photo(2).keywords));
        let expected = [
            "Adobe_imageDevelopSettings: 1 record skipped; its image is not a photo id",
            "Adobe_AdditionalMetadata: 1 record skipped; its image is not a photo id",
            "AgLibraryKeywordImage: 3 records skipped; their image is not a photo id",
            "Adobe_libraryImageDevelopHistoryStep: 1 record skipped; its image is not a photo id",
            "Adobe_libraryImageDevelopSnapshot: 1 record skipped; its image is not a photo id",
            "AgLibraryCollectionImage: 1 record added to no album; its image is not a photo id",
        ];
        check("warnings", catalog.warnings == expected, format!("{:?}", catalog.warnings));
        let mut s = crate::Session::new();
        let report = s.execute("library.importLightroom", &json!({"path": path})).unwrap();
        let zero = PhotoId(report["mapping"]["0"].as_u64().unwrap());
        check("Own album", album_named(&s, "Own album").photos == [zero], format!("{:?}", album_named(&s, "Own album").photos));
        check("Stray album", album_named(&s, "Stray album").photos.is_empty(), format!("{:?}", album_named(&s, "Stray album").photos));
        let keywords = &s.catalog.photo(zero).unwrap().meta.keywords;
        check("imported photo 0 keywords", *keywords == ["Own"], format!("{keywords:?}"));
        std::fs::remove_dir_all(dir).unwrap();
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// A photo's `id_local` is read through f64 ([`number`]), so past 2^53 it is rounded. Its side-table rows are rounded
    /// the same way and still find it, with no warning, as before this check existed.
    #[test]
    fn side_table_rows_find_a_photo_whose_id_is_past_f64_precision() {
        use SqlValue::{Integer as I, Null as N, Text as T};
        let dir = std::env::temp_dir().join(format!("lrcat-large-image-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("large.lrcat");
        for id in [9_007_199_254_740_993_i64, 1_152_921_504_606_846_977, i64::MAX] {
            let mut tables = one_row(photo_tables(("/catalog/", "")));
            tables[3].2 = vec![vec![I(id), T("big".into()), I(1), T("ARW".into()), I(20), I(10), I(1), I(0)]];
            tables.extend([
                (
                    "Adobe_imageDevelopSettings",
                    "id_local INTEGER PRIMARY KEY, image, text",
                    vec![vec![N, I(id), T("s = { Exposure2012 = 0.5 }".into())]],
                ),
                ("AgLibraryKeyword", "id_local INTEGER PRIMARY KEY, name", vec![vec![N, T("Own".into())]]),
                ("AgLibraryKeywordImage", "id_local INTEGER PRIMARY KEY, image, tag", vec![vec![N, I(id), I(1)]]),
                ("Adobe_libraryImageDevelopSnapshot", "id_local INTEGER PRIMARY KEY, image, name", vec![vec![N, I(id), T("Own snapshot".into())]]),
                ("AgLibraryCollection", "id_local INTEGER PRIMARY KEY, name", vec![vec![N, T("Own album".into())]]),
                ("AgLibraryCollectionImage", "id_local INTEGER PRIMARY KEY, collection, image", vec![vec![N, I(1), I(id)]]),
            ]);
            std::fs::write(&path, native_catalog_rows(&tables, None)).unwrap();
            let catalog = read(&path).unwrap();
            let photo = &catalog.photos[0];
            assert_eq!(
                (photo.keywords.as_slice(), photo.settings.as_str(), photo.snapshots.len()),
                (&["Own".to_string()][..], "s = { Exposure2012 = 0.5 }", 1),
                "{id}"
            );
            assert!(catalog.warnings.is_empty(), "{id}: {:?}", catalog.warnings);
            let mut s = crate::Session::new();
            s.execute("library.importLightroom", &json!({"path": path})).unwrap();
            assert_eq!(album_named(&s, "Own album").photos.len(), 1, "{id}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An XMP packet that can't be decoded is reported under its photo's id; a row that names no photo is left out with the
    /// table's warning, not reported as photo 0's.
    #[test]
    fn an_undecodable_xmp_packet_is_reported_under_its_own_image() {
        use SqlValue::{Blob as B, Integer as I, Null as N};
        let mut tables = one_row(photo_tables(("/catalog/", "")));
        tables.push((
            "Adobe_AdditionalMetadata",
            "id_local INTEGER PRIMARY KEY, image, xmp",
            vec![vec![N, I(1), B(b"bad".to_vec())], vec![N, N, B(b"bad".to_vec())]],
        ));
        let catalog = read_tables(&tables, "xmp-label");
        assert_eq!(catalog.warnings, ["image 1: truncated XMP header", "Adobe_AdditionalMetadata: 1 record skipped; its image is not a photo id"]);
    }

    #[test]
    fn a_side_table_row_names_a_photo_only_by_a_whole_number() {
        let image = |v: Value| image_of(&row(json!({"image": v})));
        assert_eq!(image(json!(2)), Some(2));
        assert_eq!(image(json!(0)), Some(0));
        assert_eq!(image(json!(2.0)), Some(2));
        assert_eq!(image(json!(-3)), Some(-3));
        // as `number` reads a photo's `id_local`: rounded through f64, saturating at i64::MAX
        assert_eq!(image(json!(9_007_199_254_740_993_i64)), Some(9_007_199_254_740_992));
        assert_eq!(image(json!(i64::MAX)), Some(i64::MAX));
        for bad in [json!(null), json!(2.5), json!("2"), json!(1e19), json!(-1e19), json!(u64::MAX)] {
            assert_eq!(image(bad.clone()), None, "{bad}");
        }
        assert_eq!(image_of(&row(json!({"tag": 1}))), None);
    }
}
