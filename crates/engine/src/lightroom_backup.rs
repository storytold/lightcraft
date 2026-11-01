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
//! Written from the ZIP application note (PKWARE APPNOTE.TXT 6.3: end of central directory,
//! ZIP64 end record and locator, central directory headers, local headers, the ZIP64 extra field
//! 0x0001) and RFC 1951 (deflate, inflated by `miniz_oxide`); no third-party zip code was used.
//! Every size and offset comes from the file and is checked before use.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

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

/// One file in the zip's central directory.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    name: String,
    method: u16,
    crc: u32,
    compressed: u64,
    size: u64,
    local: u64,
}

/// The little-endian `u16` at `at` in `b`, or `None` past its end.
fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

/// The little-endian `u32` at `at` in `b`, or `None` past its end.
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// The little-endian `u64` at `at` in `b`, or `None` past its end.
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    let s = b.get(at..at.checked_add(8)?)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Some(u64::from_le_bytes(a))
}

/// `len` bytes of `f` from offset `at`; a length that doesn't fit memory or a short read is an error.
fn read_at(f: &mut File, at: u64, len: u64) -> Result<Vec<u8>, String> {
    let len = usize::try_from(len).map_err(|_| "zip record too large".to_string())?;
    f.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf).map_err(|e| format!("truncated zip: {e}"))?;
    Ok(buf)
}

/// The central directory's entries (offset and count from the end record, or its ZIP64 record).
fn entries(f: &mut File) -> Result<Vec<Entry>, String> {
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    // end of central directory: 22 bytes plus a comment of up to 64 KiB
    let tail_len = len.min(22 + 65_535);
    let tail_at = len - tail_len;
    let tail = read_at(f, tail_at, tail_len)?;
    let eocd = (0..tail.len().saturating_sub(21)).rev().find(|&i| tail.get(i..).is_some_and(|b| b.starts_with(b"PK\x05\x06")));
    let Some(eocd) = eocd else { return Err("not a zip file (no end of central directory)".into()) };
    let bad = || "damaged zip (end of central directory)".to_string();
    let mut count = u64::from(u16_at(&tail, eocd + 10).ok_or_else(bad)?);
    let mut dir_len = u64::from(u32_at(&tail, eocd + 12).ok_or_else(bad)?);
    let mut dir_at = u64::from(u32_at(&tail, eocd + 16).ok_or_else(bad)?);
    // ZIP64: a locator just before the end record points at the ZIP64 end record
    if (count == 0xFFFF || dir_len == 0xFFFF_FFFF || dir_at == 0xFFFF_FFFF)
        && let Some(loc) = eocd.checked_sub(20)
        && tail.get(loc..).is_some_and(|b| b.starts_with(b"PK\x06\x07"))
    {
        let rec_at = u64_at(&tail, loc + 8).ok_or_else(bad)?;
        let rec = read_at(f, rec_at, 56)?;
        if !rec.starts_with(b"PK\x06\x06") {
            return Err("damaged zip (ZIP64 end record)".into());
        }
        count = u64_at(&rec, 32).ok_or_else(bad)?;
        dir_len = u64_at(&rec, 40).ok_or_else(bad)?;
        dir_at = u64_at(&rec, 48).ok_or_else(bad)?;
    }
    if count > MAX_ENTRIES || dir_len > MAX_DIRECTORY_BYTES || dir_at.checked_add(dir_len).is_none_or(|end| end > len) {
        return Err("damaged zip (central directory out of range)".into());
    }
    let dir = read_at(f, dir_at, dir_len)?;
    let mut out = Vec::new();
    let mut at = 0usize;
    let bad = || "damaged zip (central directory)".to_string();
    for _ in 0..count {
        if !dir.get(at..).is_some_and(|b| b.starts_with(b"PK\x01\x02")) {
            return Err(bad());
        }
        let method = u16_at(&dir, at + 10).ok_or_else(bad)?;
        let crc = u32_at(&dir, at + 16).ok_or_else(bad)?;
        let mut compressed = u64::from(u32_at(&dir, at + 20).ok_or_else(bad)?);
        let mut size = u64::from(u32_at(&dir, at + 24).ok_or_else(bad)?);
        let name_len = usize::from(u16_at(&dir, at + 28).ok_or_else(bad)?);
        let extra_len = usize::from(u16_at(&dir, at + 30).ok_or_else(bad)?);
        let comment_len = usize::from(u16_at(&dir, at + 32).ok_or_else(bad)?);
        let mut local = u64::from(u32_at(&dir, at + 42).ok_or_else(bad)?);
        let name_at = at + 46;
        let name_end = name_at.checked_add(name_len).ok_or_else(bad)?;
        let name = String::from_utf8_lossy(dir.get(name_at..name_end).ok_or_else(bad)?).replace('\\', "/");
        let extra = dir.get(name_end..name_end.checked_add(extra_len).ok_or_else(bad)?).ok_or_else(bad)?;
        // ZIP64 extra field: the 64-bit values, in this order, of those set to 0xFFFFFFFF
        let mut e = 0usize;
        while e + 4 <= extra.len() {
            let (id, len) = (u16_at(extra, e).ok_or_else(bad)?, usize::from(u16_at(extra, e + 2).ok_or_else(bad)?));
            let body = extra.get(e + 4..e + 4 + len).ok_or_else(bad)?;
            if id == 0x0001 {
                let mut v = 0usize;
                for field in [&mut size, &mut compressed, &mut local] {
                    if *field == 0xFFFF_FFFF {
                        *field = u64_at(body, v).ok_or_else(bad)?;
                        v += 8;
                    }
                }
            }
            e += 4 + len;
        }
        at = name_end.checked_add(extra_len).and_then(|x| x.checked_add(comment_len)).ok_or_else(bad)?;
        out.push(Entry { name, method, crc, compressed, size, local });
    }
    Ok(out)
}

/// The backup's catalog entry: the one `*.lrcat` file in the zip, outside macOS resource forks.
fn catalog_entry(entries: &[Entry]) -> Result<&Entry, String> {
    let mut found = entries.iter().filter(|e| {
        let lower = e.name.to_lowercase();
        lower.ends_with(".lrcat") && !lower.split('/').any(|p| p == "__macosx" || p.starts_with("._"))
    });
    match (found.next(), found.next()) {
        (Some(e), None) => Ok(e),
        (None, _) => Err("this zip holds no Lightroom catalog (.lrcat)".into()),
        (Some(_), Some(_)) => Err("this zip holds more than one Lightroom catalog (.lrcat); unzip it and import one".into()),
    }
}

/// Whether the zip at `path` holds exactly one Lightroom catalog, by its central directory only
/// (nothing is unpacked). Any read error means no.
pub fn holds_catalog(path: &Path) -> bool {
    File::open(path).ok().and_then(|mut f| entries(&mut f).ok()).is_some_and(|all| catalog_entry(&all).is_ok())
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

/// Unpack the catalog of the backup `zip` into a new private folder under `root`, checking its
/// size and CRC-32. Stops with an error when `cancel` is set; the folder is removed then too.
pub fn extract(zip: &Path, root: &Path, cancel: &AtomicBool) -> Result<Extracted, String> {
    let mut f = File::open(zip).map_err(|e| format!("{}: {e}", zip.display()))?;
    let all = entries(&mut f)?;
    let entry = catalog_entry(&all)?.clone();
    if entry.size > MAX_CATALOG_BYTES {
        return Err(format!("the catalog in this backup unpacks to {} GB, more than LightCraft reads", entry.size >> 30));
    }
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    sweep(root);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let dir = root.join(format!("{PREFIX}{}-{stamp}", std::process::id()));
    let mut builder = std::fs::DirBuilder::new();
    // owner-only from the start on Unix (a catalog holds the person's paths and metadata)
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // a fixed name, not the zip's: an entry named `NUL.lrcat` or `a:b.lrcat` would be a device or an
    // alternate data stream on Windows. The guard exists before the first byte is written: every
    // early return below cleans up.
    let out = Extracted { catalog: dir.join("catalog.lrcat"), dir, entry: entry.name.clone() };
    let local = read_at(&mut f, entry.local, 30)?;
    if !local.starts_with(b"PK\x03\x04") {
        return Err("damaged zip (local header)".into());
    }
    let bad = || "damaged zip (local header)".to_string();
    let skip = u64::from(u16_at(&local, 26).ok_or_else(bad)?) + u64::from(u16_at(&local, 28).ok_or_else(bad)?);
    let data_at = entry.local.checked_add(30 + skip).ok_or_else(bad)?;
    f.seek(SeekFrom::Start(data_at)).map_err(|e| e.to_string())?;
    let mut input = f.take(entry.compressed);
    // created new: a file or a link already in its place is an error, never written through
    let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&out.catalog).map_err(|e| format!("{}: {e}", out.catalog.display()))?;
    let mut writer = std::io::BufWriter::with_capacity(CHUNK, file);
    let mut crc = crc32fast::Hasher::new();
    let mut written = 0u64;
    let mut write = |bytes: &[u8], written: &mut u64| -> Result<(), String> {
        *written = written.saturating_add(bytes.len() as u64);
        if *written > entry.size {
            return Err("damaged zip (the catalog is longer than the zip says)".into());
        }
        crc.update(bytes);
        writer.write_all(bytes).map_err(|e| format!("writing {}: {e}", out.catalog.display()))
    };
    let mut buf = vec![0u8; CHUNK];
    match entry.method {
        0 => loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let n = input.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            write(buf.get(..n).unwrap_or_default(), &mut written)?;
        },
        8 => {
            use miniz_oxide::inflate::stream::{InflateState, inflate};
            use miniz_oxide::{DataFormat, MZFlush, MZStatus};
            let mut state = InflateState::new_boxed(DataFormat::Raw);
            let mut out_buf = vec![0u8; CHUNK];
            let (mut start, mut end) = (0usize, 0usize);
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".into());
                }
                if start == end {
                    end = input.read(&mut buf).map_err(|e| e.to_string())?;
                    start = 0;
                }
                let flush = if end == 0 { MZFlush::Finish } else { MZFlush::None };
                let r = inflate(&mut state, buf.get(start..end).unwrap_or_default(), &mut out_buf, flush);
                start = start.saturating_add(r.bytes_consumed).min(end);
                write(out_buf.get(..r.bytes_written).unwrap_or_default(), &mut written)?;
                match r.status {
                    Ok(MZStatus::StreamEnd) => break,
                    Ok(_) => {}
                    // no progress with all input given: the stream is cut short
                    Err(miniz_oxide::MZError::Buf) if end == 0 && r.bytes_written == 0 => return Err("damaged zip (the catalog is cut short)".into()),
                    Err(miniz_oxide::MZError::Buf) => {}
                    Err(e) => return Err(format!("damaged zip (deflate: {e:?})")),
                }
                if r.bytes_consumed == 0 && r.bytes_written == 0 && start < end {
                    return Err("damaged zip (the catalog's data does not inflate)".into());
                }
            }
        }
        m => return Err(format!("this zip uses compression method {m}, which LightCraft does not read; unzip it and import the .lrcat")),
    }
    writer.flush().map_err(|e| format!("writing {}: {e}", out.catalog.display()))?;
    drop(writer);
    if written != entry.size {
        return Err("damaged zip (the catalog is shorter than the zip says)".into());
    }
    if crc.finalize() != entry.crc {
        return Err("damaged zip (the catalog's checksum does not match)".into());
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
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
        let cases: [(&str, Vec<u8>, bool); 4] = [
            ("one.zip", zip(&[("Cat.lrcat", b"x"), ("Cat.lrcat-data/CURRENT", b"y")], true, false), true),
            ("none.zip", zip(&[("photo.jpg", b"x")], false, false), false),
            ("two.zip", zip(&[("A.lrcat", b"a"), ("B.lrcat", b"b")], false, false), false),
            ("junk.zip", b"PK".to_vec(), false),
        ];
        for (name, bytes, want) in cases {
            std::fs::write(dir.join(name), bytes).unwrap();
            assert_eq!(holds_catalog(&dir.join(name)), want, "{name}");
        }
        assert!(!holds_catalog(&dir.join("absent.zip")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
