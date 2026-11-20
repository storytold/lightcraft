//! Lightroom Classic catalog backups (issue #2592): `Backups/YYYY-MM-DD HHMM/<name>.zip`, which
//! holds the catalog `<name>.lrcat` (and, since Classic 12, a small `<name>.lrcat-data/` folder that
//! the import does not need). Someone who has left Lightroom may have nothing else: the import takes
//! the zip as it is.
//!
//! The catalog entry is inflated by streaming into a private folder ([`Extracted`]), read from
//! there like any `.lrcat`, and the folder is removed when the [`Extracted`] guard is dropped (after
//! the read, on an error and on cancel alike). The zip itself is only read. The folder is made under
//! the open library's `Interop/` when there is one (the library's disk, not a RAM-backed `/tmp`),
//! else under [`std::env::temp_dir`]; a folder left by a crash (`lc-lrbackup-*`, older than a day)
//! is removed by the next extraction there.
//!
//! ZIP/ZIP64 parsing, metadata bounds, size-cap enforcement, and CRC-32 verification are handled
//! by [`craft_zip::SeekableArchive`].

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use craft_zip::{SeekableArchive, SeekableLimits};

/// The largest catalog a backup may unpack to: the SQLite reader's cap on a database file.
pub const MAX_CATALOG_BYTES: u64 = 16 << 30;
/// The most central directory entries looked at (a Lightroom backup has one to four).
const MAX_ENTRIES: u64 = 4096;
/// The largest central directory read (names of `MAX_ENTRIES` entries fit easily).
const MAX_DIRECTORY_BYTES: u64 = 4 << 20;
/// Prefix of the private extraction folders, so leftovers can be found.
const PREFIX: &str = "lc-lrbackup-";
/// Leftover folders older than this are removed (one still being read is never this old).
const STALE_SECS: u64 = 24 * 60 * 60;
/// Bytes read from the zip and written to the catalog per step.
const CHUNK: usize = 1 << 20;

/// Whether `path` names a Lightroom catalog backup: a `.zip` file (any case).
pub fn is_backup(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

/// Where the catalog in a backup lived, for resolving the paths it records relative to itself, and
/// whether that is known: `<catalog folder>/Backups/<date>/<name>.zip` (Lightroom's default
/// layout) → `<catalog folder>/<name>.lrcat`, known; a zip anywhere else (a backup folder of the
/// person's choosing, a zip moved away) → the `.lrcat` next to it, a guess that is only used to
/// resolve paths, never to recognise the catalog.
pub fn original_catalog(zip: &Path, entry: &str) -> (PathBuf, bool) {
    let name = Path::new(entry).file_name().map_or_else(|| PathBuf::from("catalog.lrcat"), PathBuf::from);
    let dated = zip.parent();
    let backups = dated.and_then(Path::parent);
    match backups {
        Some(b) if b.file_name().is_some_and(|n| n.eq_ignore_ascii_case("backups")) => (b.parent().unwrap_or(b).join(name), true),
        _ => (dated.map_or_else(|| name.clone(), |d| d.join(&name)), false),
    }
}

/// A catalog unpacked from a backup; its private folder is removed when this is dropped.
#[derive(Debug)]
pub struct Extracted {
    dir: PathBuf,
    /// The unpacked `.lrcat`.
    pub catalog: PathBuf,
    /// The catalog's name inside the zip.
    pub entry: String,
}

impl Drop for Extracted {
    /// Removes the private folder and the catalog in it; a failure is logged, not raised (drop can't fail).
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("Lightroom backup: could not remove {}: {e}", self.dir.display());
        }
    }
}

/// Opens `path` as a bounded seekable ZIP archive without buffering payloads in memory.
fn open_archive(path: &Path) -> Result<SeekableArchive<File>, String> {
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let records =
        craft_zip::lenient::file::directory_record_names(&mut file, MAX_DIRECTORY_BYTES, MAX_ENTRIES as usize).map_err(|e| e.to_string())?;
    let mut lrcat_records = 0usize;
    for record in &records {
        let lower = String::from_utf8_lossy(&record.name).replace('\\', "/").to_lowercase();
        if lower.ends_with(".lrcat") && !lower.split('/').any(|p| p == "__macosx" || p.starts_with("._")) {
            lrcat_records += 1;
            if lrcat_records > 1 {
                return Err("this zip holds more than one Lightroom catalog (.lrcat); unzip it and import one".into());
            }
        }
    }
    SeekableArchive::open(
        file,
        SeekableLimits { max_entries: MAX_ENTRIES, max_metadata_bytes: MAX_DIRECTORY_BYTES + 65_535 + 128, max_total_bytes: MAX_CATALOG_BYTES },
    )
    .map_err(|e| e.to_string())
}

/// The backup's catalog entry: `(index, normalized_name, declared_size)` for the one `*.lrcat`
/// file in the zip, outside macOS resource forks.
fn catalog_entry(archive: &mut SeekableArchive<File>) -> Result<(usize, String, u64), String> {
    let mut found: Option<(usize, String, u64)> = None;
    for index in 0..archive.len() {
        let Some(name) = archive.entry_name(index).map_err(|e| e.to_string())? else { continue };
        let normalized = name.replace('\\', "/");
        let lower = normalized.to_lowercase();
        if lower.ends_with(".lrcat") && !lower.split('/').any(|p| p == "__macosx" || p.starts_with("._")) {
            if found.is_some() {
                return Err("this zip holds more than one Lightroom catalog (.lrcat); unzip it and import one".into());
            }
            let (size, _) = archive.entry_sizes(index).map_err(|e| e.to_string())?.ok_or_else(|| "damaged zip".to_string())?;
            found = Some((index, normalized, size));
        }
    }
    found.ok_or_else(|| "this zip holds no Lightroom catalog (.lrcat)".into())
}

/// Whether the zip at `path` holds exactly one Lightroom catalog, by its central directory only
/// (nothing is unpacked). Any read error means no.
pub fn holds_catalog(path: &Path) -> bool {
    open_archive(path).ok().is_some_and(|mut archive| catalog_entry(&mut archive).is_ok())
}

/// Remove `lc-lrbackup-*` folders in `root` left by an earlier run that ended without cleaning up.
fn sweep(root: &Path) {
    let Ok(dir) = std::fs::read_dir(root) else { return };
    for entry in dir.take(10_000).flatten() {
        if !entry.file_name().to_str().is_some_and(|n| n.starts_with(PREFIX)) {
            continue;
        }
        // only a real folder of ours: `DirEntry::metadata` doesn't follow a link (Unix), and a link named
        // like one is skipped explicitly rather than relying on `remove_dir_all` removing just the link
        let Ok(meta) = entry.metadata() else { continue };
        let stale = meta.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > STALE_SECS);
        if meta.is_dir()
            && !meta.file_type().is_symlink()
            && stale
            && let Err(e) = std::fs::remove_dir_all(entry.path())
        {
            log::warn!("Lightroom backup: could not remove the leftover {}: {e}", entry.path().display());
        }
    }
}

/// Refuse a known shortage; an unavailable space reading is left to normal write-error handling.
fn check_space(root: &Path, needed: u64, available: Option<u64>) -> Result<(), String> {
    if let Some(available) = available
        && needed > available
    {
        return Err(format!("not enough free space in {} to unpack this catalog ({} bytes needed, {} available)", root.display(), needed, available));
    }
    Ok(())
}

/// Unpack the catalog of the backup `zip` into a new private folder under `root`, checking its
/// size and CRC-32. Stops with an error when `cancel` is set; the folder is removed then too.
pub fn extract(zip: &Path, root: &Path, cancel: &AtomicBool) -> Result<Extracted, String> {
    extract_inner(zip, root, cancel, || {})
}

/// Core extraction loop; `on_chunk` runs after each written chunk (a no-op in production, used
/// in tests to trigger mid-stream cancellation deterministically).
fn extract_inner(zip: &Path, root: &Path, cancel: &AtomicBool, mut on_chunk: impl FnMut()) -> Result<Extracted, String> {
    let mut archive = open_archive(zip)?;
    let (index, entry_name, entry_size) = catalog_entry(&mut archive)?;
    if entry_size > MAX_CATALOG_BYTES {
        return Err(format!("the catalog in this backup unpacks to {} GB, more than LightCraft reads", entry_size >> 30));
    }
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    check_space(root, entry_size, crate::disk_space::read(root).map(|space| space.available))?;
    sweep(root);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let dir = root.join(format!("{PREFIX}{}-{stamp}", std::process::id()));
    // owner-only from the start on Unix (a catalog holds the person's paths and metadata)
    #[cfg(unix)]
    let builder = {
        let mut builder = std::fs::DirBuilder::new();
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder
    };
    #[cfg(not(unix))]
    let builder = std::fs::DirBuilder::new();
    builder.create(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // a fixed name, not the zip's: an entry named `NUL.lrcat` or `a:b.lrcat` would be a device or an
    // alternate data stream on Windows. The guard exists before the first byte is written: every
    // early return below cleans up.
    let out = Extracted { catalog: dir.join("catalog.lrcat"), dir, entry: entry_name };
    let mut reader = archive
        .open_entry_index(index, MAX_CATALOG_BYTES)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "damaged zip (missing catalog entry)".to_string())?;
    // created new: a file or a link already in its place is an error, never written through
    let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&out.catalog).map_err(|e| format!("{}: {e}", out.catalog.display()))?;
    let mut writer = std::io::BufWriter::with_capacity(CHUNK, file);
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let n = reader.read(&mut buf).map_err(|e| format!("reading {}: {e}", zip.display()))?;
        if n == 0 {
            break;
        }
        writer.write_all(buf.get(..n).unwrap_or_default()).map_err(|e| format!("writing {}: {e}", out.catalog.display()))?;
        on_chunk();
    }
    writer.flush().map_err(|e| format!("writing {}: {e}", out.catalog.display()))?;
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    #[test]
    fn free_space_check_refuses_known_shortage() {
        let root = std::path::Path::new("library");
        assert!(super::check_space(root, 11, Some(10)).unwrap_err().contains("not enough free space"));
        assert!(super::check_space(root, 10, Some(10)).is_ok());
        assert!(super::check_space(root, 11, None).is_ok());
    }
    use super::*;

    /// A zip of `files` (name, bytes), each stored or deflated; `zip64` writes ZIP64 records and
    /// extra fields (with the 32-bit fields set to 0xFFFFFFFF) as large archives do.
    pub(crate) fn zip(files: &[(&str, &[u8])], deflate: bool, zip64: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in files {
            let body = if deflate { miniz_oxide::deflate::compress_to_vec(data, 6) } else { data.to_vec() };
            let crc = crc32fast::hash(data);
            let local = out.len() as u64;
            let method: u16 = if deflate { 8 } else { 0 };
            let big = |v: u64| if zip64 { 0xFFFF_FFFFu32 } else { v as u32 };
            let mut extra = Vec::new();
            if zip64 {
                extra.extend_from_slice(&1u16.to_le_bytes());
                extra.extend_from_slice(&24u16.to_le_bytes());
                extra.extend_from_slice(&(data.len() as u64).to_le_bytes());
                extra.extend_from_slice(&(body.len() as u64).to_le_bytes());
                extra.extend_from_slice(&local.to_le_bytes());
            }
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&[20, 0, 0, 0]);
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&big(body.len() as u64).to_le_bytes());
            out.extend_from_slice(&big(data.len() as u64).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&body);
            central.extend_from_slice(b"PK\x01\x02");
            central.extend_from_slice(&[45, 0, 45, 0, 0, 0]);
            central.extend_from_slice(&method.to_le_bytes());
            central.extend_from_slice(&[0; 4]);
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&big(body.len() as u64).to_le_bytes());
            central.extend_from_slice(&big(data.len() as u64).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            // comment length, disk number, internal attributes, external attributes
            central.extend_from_slice(&[0; 6]);
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&big(local).to_le_bytes());
            central.extend_from_slice(name.as_bytes());
            central.extend_from_slice(&extra);
        }
        let dir_at = out.len() as u64;
        out.extend_from_slice(&central);
        let count = files.len() as u64;
        if zip64 {
            let rec_at = out.len() as u64;
            out.extend_from_slice(b"PK\x06\x06");
            out.extend_from_slice(&44u64.to_le_bytes());
            out.extend_from_slice(&[45, 0, 45, 0]);
            out.extend_from_slice(&[0; 8]);
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&(central.len() as u64).to_le_bytes());
            out.extend_from_slice(&dir_at.to_le_bytes());
            out.extend_from_slice(b"PK\x06\x07");
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&rec_at.to_le_bytes());
            out.extend_from_slice(&1u32.to_le_bytes());
        }
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0; 4]);
        let c16 = if zip64 { 0xFFFF } else { count as u16 };
        out.extend_from_slice(&c16.to_le_bytes());
        out.extend_from_slice(&c16.to_le_bytes());
        out.extend_from_slice(&(if zip64 { 0xFFFF_FFFF } else { central.len() as u32 }).to_le_bytes());
        out.extend_from_slice(&(if zip64 { 0xFFFF_FFFF } else { dir_at as u32 }).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    /// A fresh, empty folder for one test.
    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lc-lrbackup-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The private extraction folders still in `root`.
    fn leftovers(root: &Path) -> Vec<String> {
        let Ok(dir) = std::fs::read_dir(root) else { return Vec::new() };
        dir.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with(PREFIX)).collect()
    }

    /// Stored, deflated and ZIP64 backups give back the catalog byte for byte, under the fixed name, and leave nothing.
    #[test]
    fn stored_deflated_and_zip64_backups_unpack_their_catalog_and_clean_up() {
        let catalog: Vec<u8> = (0..300_000u32).flat_map(|i| (i % 251).to_le_bytes()).collect();
        for (deflate, zip64) in [(false, false), (true, false), (true, true), (false, true)] {
            let dir = temp(&format!("ok-{deflate}-{zip64}"));
            let path = dir.join("Cat.zip");
            let files: [(&str, &[u8]); 3] = [("Cat.lrcat", &catalog), ("Cat.lrcat-data/CURRENT", b"MANIFEST-1\n"), ("__MACOSX/._Cat.lrcat", b"x")];
            std::fs::write(&path, zip(&files, deflate, zip64)).unwrap();
            let root = dir.join("tmp");
            let got = extract(&path, &root, &AtomicBool::new(false)).unwrap();
            assert_eq!(std::fs::read(&got.catalog).unwrap(), catalog, "deflate {deflate} zip64 {zip64}");
            assert_eq!(got.entry, "Cat.lrcat");
            assert_eq!(got.catalog.file_name().unwrap(), "catalog.lrcat", "a fixed name, never the zip's");
            drop(got);
            assert!(leftovers(&root).is_empty(), "the private folder is removed");
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    /// Not a zip, empty, no or two catalogs, cut short, bad CRC, or cancelled: an error, and no folder left.
    #[test]
    fn damaged_or_wrong_zips_are_errors_and_leave_nothing_behind() {
        let dir = temp("bad");
        let root = dir.join("tmp");
        let catalog = vec![7u8; 100_000];
        let good = zip(&[("Cat.lrcat", &catalog)], true, false);
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("not a zip", b"SQLite format 3\0".to_vec()),
            ("empty", Vec::new()),
            ("no catalog", zip(&[("notes.txt", b"hi")], false, false)),
            ("two catalogs", zip(&[("A.lrcat", b"a"), ("B.lrcat", b"b")], false, false)),
            ("cut short", good.get(..good.len() / 2).unwrap().to_vec()),
            ("bad crc", {
                let mut z = good.clone();
                // flip one byte of the deflated data (after the 30-byte local header and the name)
                z[30 + 9 + 10] ^= 0xFF;
                z
            }),
        ];
        for (what, bytes) in cases {
            let path = dir.join("x.zip");
            std::fs::write(&path, &bytes).unwrap();
            assert!(extract(&path, &root, &AtomicBool::new(false)).is_err(), "{what} must fail");
            if root.exists() {
                assert!(leftovers(&root).is_empty(), "{what} left a folder");
            }
        }
        std::fs::write(dir.join("x.zip"), &good).unwrap();
        assert!(extract(&dir.join("x.zip"), &root, &AtomicBool::new(true)).is_err(), "cancel stops");
        assert!(leftovers(&root).is_empty(), "cancel cleans up");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Setting the cancel flag mid-stream stops extraction cleanly and removes the partial temp folder.
    #[test]
    fn mid_stream_cancellation_stops_extraction_and_cleans_up() {
        let dir = temp("cancel-mid-stream");
        let root = dir.join("tmp");
        let path = dir.join("Cat.zip");
        // Larger than two CHUNK reads so the loop must iterate multiple times.
        let catalog = vec![0x5Au8; CHUNK * 2 + 123];
        std::fs::write(&path, zip(&[("Cat.lrcat", &catalog)], false, false)).unwrap();

        let cancel = AtomicBool::new(false);
        let mut chunks = 0usize;
        let err = extract_inner(&path, &root, &cancel, || {
            chunks += 1;
            if chunks == 1 {
                assert_eq!(leftovers(&root).len(), 1, "temp folder must exist while extracting");
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap_err();

        assert_eq!(err, "cancelled");
        assert_eq!(chunks, 1, "extraction must stop before reading remaining chunks");
        assert!(leftovers(&root).is_empty(), "mid-stream cancel must clean up temp files");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A ZIP64 size of 1 TB is refused before anything is written.
    #[test]
    fn a_zip_that_lies_about_the_catalog_size_is_refused() {
        let dir = temp("size");
        let mut z = zip(&[("Cat.lrcat", &[1u8; 1000])], true, true);
        // the ZIP64 extra field's uncompressed size: 1 TB
        let at = z.windows(4).rposition(|w| w == [1, 0, 24, 0]).unwrap() + 4;
        z[at..at + 8].copy_from_slice(&(1u64 << 40).to_le_bytes());
        std::fs::write(dir.join("x.zip"), &z).unwrap();
        let err = extract(&dir.join("x.zip"), &dir.join("tmp"), &AtomicBool::new(false)).unwrap_err();
        assert!(err.contains("more than LightCraft reads"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A day-old leftover folder is removed; a recent one (another import still reading) is kept.
    #[cfg(unix)]
    #[test]
    fn stale_leftovers_are_swept_and_recent_ones_kept() {
        let root = temp("sweep");
        let old = root.join(format!("{PREFIX}1-1"));
        let recent = root.join(format!("{PREFIX}2-2"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&recent).unwrap();
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(STALE_SECS + 60);
        File::open(&old).unwrap().set_modified(past).unwrap();
        sweep(&root);
        assert!(!old.exists() && recent.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Paths relative to the catalog resolve from where Lightroom kept it, not from the zip.
    #[test]
    fn the_original_catalog_is_two_folders_above_a_dated_backup() {
        let zip = Path::new("/p/Lightroom/Backups/2023-11-07 0900/Cat-v13.zip");
        assert_eq!(original_catalog(zip, "Cat-v13.lrcat"), (PathBuf::from("/p/Lightroom/Cat-v13.lrcat"), true));
        // a backup folder of the person's choosing: a guess for paths, not an identity
        assert_eq!(original_catalog(Path::new("/elsewhere/Cat.zip"), "sub/Cat.lrcat"), (PathBuf::from("/elsewhere/Cat.lrcat"), false));
        let chosen = Path::new("/Volumes/Ext/LR Backups/2026-10-22 1200/Cat.zip");
        assert!(!original_catalog(chosen, "Cat.lrcat").1);
        assert!(is_backup(Path::new("a/B.ZIP")) && !is_backup(Path::new("a/B.lrcat")));
    }

    /// Folder detection counts a zip only when its directory lists exactly one catalog.
    #[test]
    fn only_a_zip_with_one_catalog_holds_one() {
        let dir = temp("holds");
        let cases: [(&str, Vec<u8>, bool); 5] = [
            ("one.zip", zip(&[("Cat.lrcat", b"x"), ("Cat.lrcat-data/CURRENT", b"y")], true, false), true),
            ("none.zip", zip(&[("photo.jpg", b"x")], false, false), false),
            ("two.zip", zip(&[("A.lrcat", b"a"), ("B.lrcat", b"b")], false, false), false),
            ("dup.zip", zip(&[("Cat.lrcat", b"a"), ("Cat.lrcat", b"b")], false, false), false),
            ("junk.zip", b"PK".to_vec(), false),
        ];
        for (name, bytes, want) in cases {
            std::fs::write(dir.join(name), bytes).unwrap();
            assert_eq!(holds_catalog(&dir.join(name)), want, "{name}");
        }
        assert!(extract(&dir.join("dup.zip"), &dir.join("out"), &AtomicBool::new(false)).unwrap_err().contains("more than one Lightroom catalog"));
        assert!(!holds_catalog(&dir.join("absent.zip")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
