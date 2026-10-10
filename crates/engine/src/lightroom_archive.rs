//! Bounded, deduplicated persistence for Lightroom import source data.
//!
//! Archives use a deterministic JSON representation wrapped in zlib.  The JSON
//! writer streams through a bounded compressor, so an import cannot first build
//! an unbounded archive buffer.  Blob columns are omitted by the catalog reader;
//! decoded XMP,
//! settings, history, snapshots, identities, paths, collections & membership
//! remain available for recovery.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use miniz_oxide::deflate::{
    core::{CompressorOxide, create_comp_flags_from_zip_params},
    stream::deflate,
};
use miniz_oxide::{MZFlush, MZStatus};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, SerializeStruct, Serializer};
use serde_json::Value;

use crate::lightroom_catalog::{CatalogImport, CatalogPhoto};

/// Maximum size of one complete archive on disk, including format header.
pub const MAX_ARCHIVE_BYTES: u64 = 32 * 1024 * 1024;
/// Maximum total size retained in managed archives.
pub const MAX_TOTAL_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
/// Maximum number of managed archives retained.
pub const MAX_MANAGED_ARCHIVES: usize = 8;

const FORMAT_VERSION: u8 = 1;
const HEADER: &[u8] = b"LC-LRARCH\0\x01Z";
const NAME_PREFIX: &str = "lightroom-import-";
const NAME_SUFFIX: &str = ".lca";
const MAX_SCAN: usize = 1024;
const IO_BUFFER: usize = 16 * 1024;

/// Store compact source data below `directory` and return its managed archive.
///
/// The operation is safe to call before catalog mutation.  It rejects a
/// symlink directory, writes only `lightroom-import-<number>.lca`, compares the
/// complete bounded byte stream with recent managed archives, and retains at
/// most [`MAX_MANAGED_ARCHIVES`] and [`MAX_TOTAL_ARCHIVE_BYTES`].  Legacy
/// `lightroom-import-*.json` files are deliberately ignored and preserved for
/// an explicit caller warning.
pub fn store(directory: &Path, data: &CatalogImport) -> Result<PathBuf, String> {
    prepare_directory(directory)?;
    let mut managed = managed_files(directory)?;
    managed.sort_by_key(|entry| entry.index);

    let temporary = temporary_path(directory)?;
    write_archive(&temporary, data).inspect_err(|_| {
        let _ = remove_managed_file(&temporary);
    })?;

    let recent = managed.iter().rev().take(MAX_MANAGED_ARCHIVES);
    for entry in recent {
        let same = match same_bounded(&temporary, &entry.path) {
            Ok(same) => same,
            Err(error) => {
                let _ = remove_managed_file(&temporary);
                return Err(error.to_string());
            }
        };
        if same {
            remove_managed_file(&temporary).map_err(|e| e.to_string())?;
            let matched = entry.path.clone();
            let mut all = managed.clone();
            prune_existing(directory, &mut all, Some(&matched))?;
            return Ok(matched);
        }
    }

    let index = managed.last().map_or(0, |entry| entry.index.saturating_add(1));
    if managed.last().is_some_and(|entry| entry.index == u64::MAX) {
        let _ = remove_managed_file(&temporary);
        return Err("managed Lightroom archive index is exhausted".into());
    }
    let destination = match next_destination(directory, index) {
        Ok(path) => path,
        Err(error) => {
            let _ = remove_managed_file(&temporary);
            return Err(error);
        }
    };
    let size = match fs::metadata(&temporary) {
        Ok(meta) => meta.len(),
        Err(error) => {
            let _ = remove_managed_file(&temporary);
            return Err(error.to_string());
        }
    };
    let new_entry = Managed { index, path: destination.clone(), size };
    let mut after = managed;
    after.push(new_entry);
    after.sort_by_key(|entry| entry.index);
    let retirees = match retirement_set(&after) {
        Ok(retirees) => retirees,
        Err(error) => {
            let _ = remove_managed_file(&temporary);
            return Err(error);
        }
    };
    let quarantined = match quarantine(&retirees) {
        Ok(quarantined) => quarantined,
        Err(error) => {
            let _ = remove_managed_file(&temporary);
            return Err(error);
        }
    };

    if let Err(error) = publish_new(&temporary, &destination) {
        rollback_quarantine(&quarantined);
        let _ = remove_managed_file(&temporary);
        return Err(error.to_string());
    }
    if let Err(error) = remove_managed_file(&temporary) {
        return Err(format!("archive published but temporary cleanup failed: {error}"));
    }
    for (old, held) in quarantined {
        if let Err(error) = remove_managed_file(&held) {
            return Err(format!("archive published but could not retire {}: {error}", old.display()));
        }
    }
    Ok(destination)
}

/// Store source data when possible. An archive that reaches its bounded size is
/// omitted with a warning so catalog import can still complete.
pub fn store_best_effort(directory: &Path, data: &CatalogImport) -> Result<Option<PathBuf>, String> {
    match store(directory, data) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.contains("Lightroom archive exceeds 32 MiB") => Ok(None),
        Err(error) => Err(error),
    }
}

#[derive(Clone, Debug)]
struct Managed {
    index: u64,
    path: PathBuf,
    size: u64,
}

fn prepare_directory(directory: &Path) -> Result<(), String> {
    match fs::symlink_metadata(directory) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return Err(format!("archive directory is not a real directory: {}", directory.display()));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(directory).map_err(|e| e.to_string())?;
            let meta = fs::symlink_metadata(directory).map_err(|e| e.to_string())?;
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return Err(format!("archive directory is not a real directory: {}", directory.display()));
            }
        }
        Err(error) => return Err(error.to_string()),
    }
    Ok(())
}

fn managed_files(directory: &Path) -> Result<Vec<Managed>, String> {
    let mut files = Vec::new();
    for (seen, result) in fs::read_dir(directory).map_err(|e| e.to_string())?.enumerate() {
        if seen >= MAX_SCAN {
            return Err(format!("too many entries in archive directory (limit {MAX_SCAN})"));
        }
        let entry = result.map_err(|e| e.to_string())?;
        let Some(index) = parse_managed_name(&entry.file_name()) else { continue };
        let meta = fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            continue;
        }
        files.push(Managed { index, path: entry.path(), size: meta.len() });
    }
    Ok(files)
}

fn parse_managed_name(name: &std::ffi::OsStr) -> Option<u64> {
    let name = name.to_str()?;
    let digits = name.strip_prefix(NAME_PREFIX)?.strip_suffix(NAME_SUFFIX)?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn temporary_path(directory: &Path) -> Result<PathBuf, String> {
    let pid = std::process::id();
    for slot in 0..MAX_SCAN {
        let path = directory.join(format!(".lightroom-import-{pid}-{slot}.tmp"));
        match fs::symlink_metadata(&path) {
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(path),
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("no safe temporary archive name available".into())
}

fn next_destination(directory: &Path, mut index: u64) -> Result<PathBuf, String> {
    for _ in 0..MAX_SCAN {
        let path = directory.join(format!("{NAME_PREFIX}{index}{NAME_SUFFIX}"));
        match fs::symlink_metadata(&path) {
            Ok(_) => index = index.checked_add(1).ok_or("managed archive index is exhausted")?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(path),
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("no safe managed archive name available".into())
}

fn write_archive(path: &Path, data: &CatalogImport) -> Result<(), String> {
    dac_catalog::safe_file::write_atomic_with(path, &mut |raw| {
        let mut bounded = BoundedWriter { inner: raw, total: 0 };
        bounded.write_all(HEADER)?;
        let mut compressor = ZlibWriter::new(&mut bounded);
        serde_json::to_writer(&mut compressor, &ArchiveRef(data)).map_err(json_io)?;
        compressor.finish()?;
        Ok(())
    })
    .map(|_| ())
    .map_err(|error| format!("could not write Lightroom archive: {error}"))
}

fn json_io(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

struct BoundedWriter<'a> {
    inner: &'a mut dyn Write,
    total: u64,
}

impl Write for BoundedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self.total.checked_add(bytes.len() as u64).ok_or_else(limit_error)?;
        if next > MAX_ARCHIVE_BYTES {
            return Err(limit_error());
        }
        self.inner.write_all(bytes)?;
        self.total = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn limit_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Lightroom archive exceeds 32 MiB")
}

struct ZlibWriter<'a, 'b> {
    sink: &'a mut BoundedWriter<'b>,
    compressor: CompressorOxide,
    plain: u64,
}

impl<'a, 'b> ZlibWriter<'a, 'b> {
    fn new(sink: &'a mut BoundedWriter<'b>) -> ZlibWriter<'a, 'b> {
        let flags = create_comp_flags_from_zip_params(6, 15, 0);
        ZlibWriter { sink, compressor: CompressorOxide::new(flags), plain: 0 }
    }

    fn run(&mut self, input: &[u8], flush: MZFlush) -> io::Result<()> {
        let mut rest = input;
        loop {
            let mut output = [0u8; IO_BUFFER];
            let result = deflate(&mut self.compressor, rest, &mut output, flush);
            self.sink.write_all(&output[..result.bytes_written])?;
            match result.status {
                Ok(MZStatus::StreamEnd) => return Ok(()),
                Ok(MZStatus::Ok) => {
                    let consumed = result.bytes_consumed;
                    if consumed > rest.len() {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid compressor progress"));
                    }
                    rest = &rest[consumed..];
                    if rest.is_empty() && flush == MZFlush::None {
                        return Ok(());
                    }
                    if consumed == 0 && result.bytes_written == 0 {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "compressor made no progress"));
                    }
                }
                Ok(status) => {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unsupported compressor status: {status:?}")));
                }
                Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("compression failed: {error:?}"))),
            }
        }
    }

    fn finish(&mut self) -> io::Result<()> {
        self.run(&[], MZFlush::Finish)
    }
}

impl Write for ZlibWriter<'_, '_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let next = self.plain.checked_add(bytes.len() as u64).ok_or_else(limit_error)?;
        if next > MAX_ARCHIVE_BYTES {
            return Err(limit_error());
        }
        self.run(bytes, MZFlush::None)?;
        self.plain = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn same_bounded(left: &Path, right: &Path) -> io::Result<bool> {
    let left_meta = fs::symlink_metadata(left)?;
    let right_meta = fs::symlink_metadata(right)?;
    if left_meta.file_type().is_symlink() || right_meta.file_type().is_symlink() || !left_meta.is_file() || !right_meta.is_file() {
        return Ok(false);
    }
    let left_len = left_meta.len();
    let right_len = right_meta.len();
    if left_len != right_len || left_len > MAX_ARCHIVE_BYTES {
        return Ok(false);
    }
    let mut left = File::open(left)?;
    let mut right = File::open(right)?;
    let mut left_buf = [0u8; IO_BUFFER];
    let mut right_buf = [0u8; IO_BUFFER];
    loop {
        let left_n = left.read(&mut left_buf)?;
        let right_n = right.read(&mut right_buf)?;
        if left_n != right_n {
            return Ok(false);
        }
        if left_n == 0 {
            return Ok(true);
        }
        if left_buf[..left_n] != right_buf[..right_n] {
            return Ok(false);
        }
    }
}

fn retirement_set(entries: &[Managed]) -> Result<Vec<Managed>, String> {
    retirement_set_protected(entries, None)
}

fn retirement_set_protected(entries: &[Managed], protected: Option<&Path>) -> Result<Vec<Managed>, String> {
    let mut ordered = entries.to_vec();
    ordered.sort_by_key(|entry| entry.index);
    let mut total = ordered.iter().try_fold(0u64, |sum, entry| sum.checked_add(entry.size)).ok_or("archive size overflow")?;
    let mut retirees = Vec::new();
    while ordered.len() > MAX_MANAGED_ARCHIVES || total > MAX_TOTAL_ARCHIVE_BYTES {
        let Some(position) = ordered.iter().position(|entry| protected.is_none_or(|path| entry.path != path)) else {
            return Err("cannot satisfy archive retention limit".into());
        };
        let oldest = ordered.remove(position);
        total = total.saturating_sub(oldest.size);
        retirees.push(oldest);
    }
    Ok(retirees)
}

fn prune_existing(directory: &Path, entries: &mut [Managed], protected: Option<&Path>) -> Result<(), String> {
    let retirees = retirement_set_protected(entries, protected)?;
    let quarantined = quarantine(&retirees)?;
    for (old, held) in quarantined {
        if let Err(error) = remove_managed_file(&held) {
            return Err(format!("could not retire {} from {}: {error}", old.display(), directory.display()));
        }
    }
    Ok(())
}

fn quarantine(entries: &[Managed]) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    let mut moved = Vec::new();
    let pid = std::process::id();
    for (slot, entry) in entries.iter().enumerate() {
        let held = entry.path.with_file_name(format!(".lightroom-retire-{pid}-{slot}.tmp"));
        match fs::symlink_metadata(&held) {
            Ok(_) => {
                rollback_quarantine(&moved);
                return Err(format!("retention temporary path already exists: {}", held.display()));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                rollback_quarantine(&moved);
                return Err(error.to_string());
            }
        }
        let meta = fs::symlink_metadata(&entry.path).map_err(|error| {
            rollback_quarantine(&moved);
            error.to_string()
        })?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            rollback_quarantine(&moved);
            return Err(format!("managed archive changed into non-file: {}", entry.path.display()));
        }
        if let Err(error) = fs::rename(&entry.path, &held) {
            rollback_quarantine(&moved);
            return Err(error.to_string());
        }
        moved.push((entry.path.clone(), held));
    }
    Ok(moved)
}

fn rollback_quarantine(moved: &[(PathBuf, PathBuf)]) {
    for (original, held) in moved.iter().rev() {
        let _ = fs::rename(held, original);
    }
}

fn publish_new(temporary: &Path, destination: &Path) -> io::Result<()> {
    match fs::hard_link(temporary, destination) {
        Ok(()) => Ok(()),
        Err(error) => Err(io::Error::new(error.kind(), format!("cannot atomically publish archive: {error}"))),
    }
}

fn remove_managed_file(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "refusing to remove non-regular archive path"));
    }
    fs::remove_file(path)
}

struct ArchiveRef<'a>(&'a CatalogImport);

impl Serialize for ArchiveRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_struct("CatalogImportArchive", 7)?;
        out.serialize_field("version", &FORMAT_VERSION)?;
        out.serialize_field("source", &self.0.source)?;
        out.serialize_field("photos", &PhotoSeq(&self.0.photos))?;
        out.serialize_field("collections", &RowSeq(&self.0.collections))?;
        out.serialize_field("members", &RowSeq(&self.0.members))?;
        out.serialize_field("collection_content", &RowSeq(&self.0.collection_content))?;
        out.serialize_field("warnings", &self.0.warnings)?;
        out.end()
    }
}

struct PhotoSeq<'a>(&'a [CatalogPhoto]);
impl Serialize for PhotoSeq<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_seq(Some(self.0.len()))?;
        for photo in self.0 {
            out.serialize_element(&PhotoRef(photo))?;
        }
        out.end()
    }
}

struct PhotoRef<'a>(&'a CatalogPhoto);
impl Serialize for PhotoRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let p = self.0;
        let mut out = serializer.serialize_struct("CatalogPhotoArchive", 8)?;
        out.serialize_field("source_id", &p.source_id)?;
        out.serialize_field("uuid", &p.uuid)?;
        out.serialize_field("path", &p.path)?;
        out.serialize_field("image", &RowRef(&p.image))?;
        out.serialize_field("settings", &p.settings)?;
        out.serialize_field("xmp", &p.xmp)?;
        out.serialize_field("keywords", &p.keywords)?;
        out.serialize_field("history", &RowSeq(&p.history))?;
        out.serialize_field("snapshots", &RowSeq(&p.snapshots))?;
        out.end()
    }
}

struct RowSeq<'a>(&'a [BTreeMap<String, Value>]);
impl Serialize for RowSeq<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_seq(Some(self.0.len()))?;
        for row in self.0 {
            out.serialize_element(&RowRef(row))?;
        }
        out.end()
    }
}

struct RowRef<'a>(&'a BTreeMap<String, Value>);
impl Serialize for RowRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_map(None)?;
        for (key, value) in self.0 {
            out.serialize_entry(key, value)?;
        }
        out.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lightroom_catalog::{CatalogImport, CatalogPhoto};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn tempdir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("app-lr-archive-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn data(source: String) -> CatalogImport {
        CatalogImport {
            source,
            photos: Vec::new(),
            collections: Vec::new(),
            members: Vec::new(),
            collection_content: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn rich_data() -> CatalogImport {
        let photo = CatalogPhoto {
            source_id: 7,
            uuid: "uuid-7".into(),
            path: "/photos/one.raw".into(),
            image: BTreeMap::from([("rating".into(), serde_json::json!(5)), ("opaque_blob".into(), serde_json::json!([1, 2, 3, 4]))]),
            settings: "s = { Exposure2012 = 0.75 }".into(),
            xmp: "<xmp>decoded</xmp>".into(),
            keywords: vec!["Trips|Goa".into()],
            history: vec![BTreeMap::from([("text".into(), serde_json::json!("step")), ("curve".into(), serde_json::json!([0, 128, 255]))])],
            snapshots: vec![BTreeMap::from([("name".into(), serde_json::json!("before"))])],
        };
        CatalogImport {
            source: "source.lrcat".into(),
            photos: vec![photo],
            collections: vec![BTreeMap::from([("name".into(), serde_json::json!("Keepers"))])],
            members: vec![BTreeMap::from([("image".into(), serde_json::json!(7))])],
            collection_content: vec![BTreeMap::from([("rule".into(), serde_json::json!("rating > 3"))])],
            warnings: vec!["one warning".into()],
        }
    }

    fn managed_count(path: &Path) -> usize {
        fs::read_dir(path).unwrap().filter_map(Result::ok).filter(|entry| parse_managed_name(&entry.file_name()).is_some()).count()
    }

    #[test]
    fn deduplicates_identical_deterministic_archives() {
        let path = tempdir();
        let input = data("catalog.lrcat".into());
        let first = store(&path, &input).unwrap();
        let second = store(&path, &input).unwrap();
        assert_eq!(first, second);
        assert_eq!(managed_count(&path), 1);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn dedup_returns_archive_that_retention_keeps() {
        let path = tempdir();
        let input = data("catalog.lrcat".into());
        let first = store(&path, &input).unwrap();
        for index in 1..MAX_MANAGED_ARCHIVES {
            let filler = path.join(format!("lightroom-import-{index}.lca"));
            File::create(filler).unwrap().set_len(19 * 1024 * 1024).unwrap();
        }
        let reused = store(&path, &input).unwrap();
        assert_eq!(reused, first);
        assert!(reused.is_file());
        assert!(managed_count(&path) <= MAX_MANAGED_ARCHIVES);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn caps_managed_archive_count_and_preserves_unrelated_files() {
        let path = tempdir();
        let human = path.join("notes.txt");
        let legacy = path.join("lightroom-import-0.json");
        fs::write(&human, b"keep").unwrap();
        fs::write(&legacy, b"legacy").unwrap();
        for index in 0..(MAX_MANAGED_ARCHIVES + 3) {
            let input = data(format!("catalog-{index}.lrcat"));
            store(&path, &input).unwrap();
        }
        assert!(managed_count(&path) <= MAX_MANAGED_ARCHIVES);
        assert_eq!(fs::read(&human).unwrap(), b"keep");
        assert_eq!(fs::read(&legacy).unwrap(), b"legacy");
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn rejects_oversize_before_creating_archive() {
        let path = tempdir();
        let input = data("x".repeat((MAX_ARCHIVE_BYTES as usize) + 1));
        assert!(store(&path, &input).is_err());
        assert_eq!(managed_count(&path), 0);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn large_catalog_history_is_best_effort_and_does_not_abort_import() {
        let path = tempdir();
        let mut input = data("catalog.lrcat".into());
        input.photos = (0..5_000)
            .map(|index| CatalogPhoto {
                source_id: index,
                uuid: format!("photo-{index}"),
                path: format!("/photos/{index}.raw"),
                image: BTreeMap::new(),
                settings: String::new(),
                xmp: String::new(),
                keywords: Vec::new(),
                history: vec![BTreeMap::from([("text".into(), serde_json::json!(format!("history-{index}-{}", "x".repeat(8 * 1024))))])],
                snapshots: Vec::new(),
            })
            .collect();
        assert_eq!(store_best_effort(&path, &input).unwrap(), None);
        assert_eq!(managed_count(&path), 0);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn archive_roundtrips_zlib_payload_and_preserves_numeric_arrays() {
        let path = tempdir();
        let archive = store(&path, &rich_data()).unwrap();
        let bytes = fs::read(archive).unwrap();
        assert!(bytes.starts_with(HEADER));
        let json = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&bytes[HEADER.len()..], MAX_ARCHIVE_BYTES as usize).unwrap();
        let decoded: Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(decoded["version"], serde_json::json!(FORMAT_VERSION));
        assert_eq!(decoded["source"], serde_json::json!("source.lrcat"));
        assert_eq!(decoded["photos"].as_array().unwrap().len(), 1);
        assert_eq!(decoded["photos"][0]["source_id"], serde_json::json!(7));
        assert_eq!(decoded["photos"][0]["uuid"], serde_json::json!("uuid-7"));
        assert_eq!(decoded["photos"][0]["path"], serde_json::json!("/photos/one.raw"));
        assert_eq!(decoded["photos"][0]["settings"], serde_json::json!("s = { Exposure2012 = 0.75 }"));
        assert_eq!(decoded["photos"][0]["xmp"], serde_json::json!("<xmp>decoded</xmp>"));
        assert_eq!(decoded["photos"][0]["keywords"], serde_json::json!(["Trips|Goa"]));
        assert_eq!(decoded["photos"][0]["image"]["rating"], serde_json::json!(5));
        assert_eq!(decoded["photos"][0]["image"]["opaque_blob"], serde_json::json!([1, 2, 3, 4]));
        assert_eq!(decoded["photos"][0]["history"].as_array().unwrap().len(), 1);
        assert_eq!(decoded["photos"][0]["history"][0]["curve"], serde_json::json!([0, 128, 255]));
        assert_eq!(decoded["photos"][0]["snapshots"].as_array().unwrap().len(), 1);
        assert_eq!(decoded["collections"].as_array().unwrap().len(), 1);
        assert_eq!(decoded["collections"][0]["name"], serde_json::json!("Keepers"));
        assert_eq!(decoded["members"].as_array().unwrap().len(), 1);
        assert_eq!(decoded["members"][0]["image"], serde_json::json!(7));
        assert_eq!(decoded["collection_content"].as_array().unwrap().len(), 1);
        assert_eq!(decoded["warnings"], serde_json::json!(["one warning"]));
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn retention_enforces_aggregate_byte_cap_with_small_synthetic_entries() {
        let mut entries: Vec<_> = (0..MAX_MANAGED_ARCHIVES as u64)
            .map(|index| Managed { index, path: PathBuf::from(format!("lightroom-import-{index}.lca")), size: 20 * 1024 * 1024 })
            .collect();
        entries.push(Managed { index: 8, path: PathBuf::from("lightroom-import-8.lca"), size: 20 * 1024 * 1024 });
        let retirees = retirement_set(&entries).unwrap();
        assert_eq!(retirees.len(), 3);
        assert_eq!(retirees.iter().map(|entry| entry.index).collect::<Vec<_>>(), vec![0, 1, 2]);
    }

    #[test]
    fn retention_failure_preserves_old_archive() {
        let path = tempdir();
        let old = path.join("lightroom-import-0.lca");
        let held = path.join(format!(".lightroom-retire-{}-0.tmp", std::process::id()));
        fs::write(&old, b"old archive").unwrap();
        fs::write(&held, b"occupied").unwrap();
        let entry = Managed { index: 0, path: old.clone(), size: 11 };
        assert!(quarantine(&[entry]).is_err());
        assert_eq!(fs::read(&old).unwrap(), b"old archive");
        fs::remove_dir_all(path).unwrap();
    }
}
