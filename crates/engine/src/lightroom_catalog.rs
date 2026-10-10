//! Direct, read-only Lightroom Classic catalog import. SQLite pages are read in Rust;
//! Lightroom, Python and a C SQLite runtime are not needed. Original paths stay in place.
//! Raw settings are archived before mutation; mapped edits are approximations, not Adobe renders.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use crate::lightroom_sqlite::{Database, LiveTable, Value as SqlValue};
use dac_catalog::{Album, Flag, Op, Photo, PhotoId, Source};
use dac_geom::{Affine, Orientation, Point, Rect};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

type Row = BTreeMap<String, Value>;
const COMMAND: &str = "library.importLightroom";
const MAX_FILE: u64 = 1 << 30;

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

fn bytes(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if f.metadata().map_err(|e| e.to_string())?.len() > MAX_FILE {
        return Err("catalog or WAL exceeds 1 GiB import limit".into());
    }
    let mut out = Vec::new();
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
    let before = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let wal_path = PathBuf::from(format!("{}-wal", path.display()));
    let wal_before = std::fs::metadata(&wal_path).ok().map(|m| (m.len(), m.modified().ok()));
    let main = bytes(path)?;
    let wal = match std::fs::metadata(&wal_path) {
        Ok(_) => bytes(&wal_path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.to_string()),
    };
    let wal_len = committed_wal_len(&main, &wal)?;
    ensure_read_active(cancel)?;
    let db = Database::open_with_wal(main, &wal[..wal_len]).map_err(|e| format!("invalid SQLite catalog: {e:?}"))?;
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
        let root_path = text(root, "absolutePath");
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

/// Lightroom's crop in the app's frame. `crs` gives the crop's top-left (`CropLeft`, `CropTop`)
/// and bottom-right (`CropRight`, `CropBottom`) corners on the stored image (before orientation and
/// straightening), normalized to its size, plus a straighten angle that turns the other way to
/// The app's. The app's crop is a rectangle in the oriented image rotated by `angle` about its
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
        dac_catalog::safe_file::write_atomic(&path, &bytes).map_err(|e| error(e.to_string()))?;
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
    let mut unmapped = BTreeMap::new();
    let mut preserved = 0;
    // Masters precede their virtual copies.
    let mut photos: Vec<_> = data.photos.iter().collect();
    photos.sort_by_key(|p| number(&p.image, "masterImage") != 0);
    let mut planned: HashMap<PhotoId, Photo> = HashMap::new();
    for src in photos {
        let master = number(&src.image, "masterImage");
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
            if missing_sources.contains(&key(&src.path)) {
                missing.push(src.path.clone());
            }
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
                p.kind = dac_catalog::MediaKind::Raw;
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
                    p.develop = Arc::new(dac_develop::apply_partial(&p.develop, &partial, 1.0));
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
        if s.catalog.photo(p.id).is_some() {
            ops.extend([
                Op::SetRating { id: p.id, rating: p.rating },
                Op::SetFlag { id: p.id, flag: p.flag },
                Op::SetLabel { id: p.id, label: p.label },
                Op::SetMeta { id: p.id, meta: Box::new(p.meta.clone()) },
                Op::SetCaptured { id: p.id, captured: p.captured.clone() },
            ]);
            ops.push(Op::SetDevelop { id: p.id, settings: p.develop.clone(), label: "Lightroom catalog".into(), edited: p.edited.clone() });
        } else {
            ops.push(Op::AddPhoto { photo: Box::new(p.clone()) });
        }
        by_path.entry(key(&src.path)).or_insert(p.id);
        planned.insert(p.id, p);
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
            .map(dac_catalog::AlbumId)
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
        report: json!({"source":data.source,"photos":ids.len(),"imported":files.imported.len(),"collections":albums.len(),"preservedExistingEdits":preserved,"missing":missing,"failed":files.failed,"warnings":warnings,"unmapped":unmapped,"archive":archive_path,"mapping":ids}),
        index: index.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dac_geom::CropGeometry;
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
    fn imports_copies_collections_ratings_and_mapped_edits_in_one_undo_step() {
        let mut s = crate::Session::new();
        let data = sample();
        let r = apply(&mut s, data.clone(), false).unwrap();
        assert_eq!(r["photos"], 2);
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
        s.set_develop(id, dac_develop::DevelopSettings::default(), "Personal edit").unwrap();
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
        // an angle alone still means a crop (Lightroom's angle turns the other way to the app's)
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
    #[test]
    fn reads_native_sqlite_tables_without_lightroom_or_external_processes() {
        use SqlValue::{Integer as I, Null as N, Text as T};
        let tables = [
            ("AgLibraryRootFolder", "id_local INTEGER PRIMARY KEY, absolutePath", vec![N, T("/catalog/".into())]),
            ("AgLibraryFolder", "id_local INTEGER PRIMARY KEY, rootFolder, pathFromRoot", vec![N, I(1), T("Photos/".into())]),
            ("AgLibraryFile", "id_local INTEGER PRIMARY KEY, folder, idx_filename", vec![N, I(1), T("test.ARW".into())]),
            (
                "Adobe_images",
                "id_local INTEGER PRIMARY KEY, id_global, rootFile, fileFormat, fileWidth, fileHeight, rating, pick",
                vec![N, T("fixture-image".into()), I(1), T("ARW".into()), I(20), I(10), I(5), I(1)],
            ),
        ];
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
        let path = std::env::temp_dir().join(format!("app-native-catalog-{}.lrcat", std::process::id()));
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
        let reader =
            Reader { db, tables: vec![LiveTable { name: "optional_history".into(), rootpage: 2, column_names: Some(vec!["payload".into()]) }] };
        let mut warnings = Vec::new();
        let rows = reader.optional_table("optional_history", &mut warnings).unwrap();
        assert!(rows.is_empty());
        assert!(warnings.iter().any(|warning| warning.contains("optional_history")));
    }
}
