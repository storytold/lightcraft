//! Rendered thumbnails on disk: `<dir>/<2 hex>/<32 hex>.lcthumb`, bounded in bytes.
//!
//! The cache is disposable: files are written via temp + rename (readers never see partial
//! files) but not fsynced; unreadable files are deleted and re-rendered. When the total size
//! exceeds the budget, the least recently used files (by modification time) are removed down to
//! 80 % of the budget.
//!
//! **The cache is nobody's pictures** (issues #610, #1066). A library is often in the person's
//! Pictures folder, which photo galleries and backups look through, so:
//! - its files are JPEG data under a name that is no picture's ([`EXT`]). They used to be
//!   `.jpg`, and every thumbnail (one per photo, size and state of its edits) showed up in the
//!   system's gallery as a photo of the person's. Files from then are taken over under the new
//!   name: one that is asked for at once, the rest when the cache is tidied
//!   ([`DiskCache::take_over_old`], which the engine does once per folder in the background).
//!   Nothing is rendered again for it.
//! - reading writes nothing. A file's date was brought forward on every read, so every launch
//!   made every thumbnail in view look new; now only one that was not used for [`STALE`] is
//!   touched. What was read in this session is remembered in memory, and a full cache lets it
//!   go last.
//! - a folder that holds nothing but the cache is tagged as one ([`TAG`]): when the cache
//!   makes it, and when it is tidied.
//!
//! An older build that is run again writes `.jpg` files once more; in a folder that was tidied
//! already those are taken over one by one as they are asked for, and the rest go as the cache
//! fills.
//!
//! The cache only ever lists, counts, renames or deletes files of its own name shape
//! (`<2 hex>/<32 hex>.lcthumb`, the `.jpg` of before, and the `<32 hex>.tmp…` temp files of
//! [`DiskCache::put`], see [`is_cache_file`]): the directory may be shared with other files
//! (e.g. a library opened on a photo folder that already has a `thumbs/` folder), and those are
//! never touched (issue #98). A shard directory that is a symlink or junction is not followed
//! (issue #1504).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use lightcraft_raster::Rgba8;

use crate::Hash128;

const QUALITY: u8 = 90;

/// What a cached thumbnail's file is named after its key: not a picture's ending, so that
/// nothing that looks for the person's photos takes the cache for them.
pub const EXT: &str = "lcthumb";
/// What it was named before (JPEG data then as now).
const OLD_EXT: &str = "jpg";

/// How long a thumbnail goes unused before a read brings its date forward (the date is what a
/// full cache lets go by, oldest first). Reads within it write nothing.
pub const STALE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// The file that says the cache has looked through its folder for thumbnails under the name
/// of before and taken them over ([`DiskCache::take_over_old`]): written after a whole pass,
/// whatever else the folder holds, so that it is done once.
const TIDIED: &str = ".lightcraft-thumbs";

/// Files an operating system leaves in a folder that was looked at: nobody's, and no reason
/// to take a folder for more than a cache.
const DROPPINGS: [&str; 3] = [".DS_Store", "desktop.ini", "Thumbs.db"];

/// The file that says a folder is a cache, for backup programs and the like (the Cache
/// Directory Tagging Specification: the first line is the signature).
pub const TAG: &str = "CACHEDIR.TAG";
const TAG_TEXT: &str = "Signature: 8a477f597d28d172789f06886806bc55\n\
# This folder holds LightCraft's thumbnail cache. It is safe to delete: LightCraft renders what it needs again.\n\
# See https://bford.info/cachedir/\n";

pub struct DiskCache {
    dir: PathBuf,
    /// Persisted by the library together with changes to the proxy source folder.
    namespace: u64,
    /// Bytes the cache may hold (changed in place by [`DiskCache::set_budget`]).
    budget: AtomicU64,
    /// Bytes on disk (`None` until scanned, and again after a clear).
    total: Mutex<Option<u64>>,
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    pub writes: AtomicU64,
    /// The thumbnails read in this session: a full cache lets them go last ([`Self::prune`]).
    read: Mutex<std::collections::HashSet<u128>>,
}

impl DiskCache {
    pub fn new(dir: &Path, budget: u64) -> DiskCache {
        Self::with_namespace(dir, budget, 0)
    }

    /// Select a persistent cache namespace without scanning or changing any files. Old
    /// namespaces share this directory and its budget; normal pruning removes their files.
    pub fn with_namespace(dir: &Path, budget: u64, namespace: u64) -> DiskCache {
        DiskCache {
            dir: dir.to_path_buf(),
            namespace,
            budget: AtomicU64::new(budget),
            total: Mutex::new(None),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            writes: AtomicU64::new(0),
            read: Mutex::new(Default::default()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn namespace(&self) -> u64 {
        self.namespace
    }

    /// Bytes the cache may hold.
    pub fn budget(&self) -> u64 {
        self.budget.load(Ordering::Relaxed)
    }

    /// Change the budget in place: the files stay valid (they are keyed by content), so a resize
    /// keeps them; a smaller budget is enforced by the next write, like any other overflow.
    pub fn set_budget(&self, bytes: u64) {
        self.budget.store(bytes, Ordering::Relaxed);
    }

    fn path(&self, key: Hash128) -> PathBuf {
        self.named(key, EXT)
    }

    fn named(&self, key: Hash128, ext: &str) -> PathBuf {
        // Namespace zero preserves existing libraries' cache keys. Every committed folder
        // switch gets a new value, even when returning to a previously used folder.
        let key = if self.namespace == 0 {
            key
        } else {
            crate::Hasher128::new().str("thumbnail namespace").u64(self.namespace).update(&key.0.to_le_bytes()).finish()
        };
        let hex = key.to_string();
        // (32 hex digits: the first two are there)
        self.dir.join(hex.get(..2).unwrap_or("00")).join(format!("{hex}.{ext}"))
    }

    /// The file's bytes: under its name, or under the name it had before ([`OLD_EXT`]), which
    /// it then no longer has.
    fn read(&self, key: Hash128) -> Option<(PathBuf, Vec<u8>)> {
        let p = self.path(key);
        // a shard that is a symlink (or junction) leads outside the cache: nothing is read
        // through it, under either name
        if !p.parent().is_some_and(is_real_dir) {
            return None;
        }
        if let Ok(bytes) = std::fs::read(&p) {
            return Some((p, bytes));
        }
        let old = self.named(key, OLD_EXT);
        let Ok(bytes) = std::fs::read(&old) else {
            // (taken over by the tidying between the two looks: it is under the new name now)
            return std::fs::read(&p).ok().map(|bytes| (p, bytes));
        };
        // (taken over; if that can't be done it is read where it is, and tried again next time)
        match std::fs::rename(&old, &p) {
            Ok(()) => Some((p, bytes)),
            Err(_) => Some((old, bytes)),
        }
    }

    pub fn get(&self, key: Hash128) -> Option<Rgba8> {
        let Some((p, bytes)) = self.read(key) else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        match decode_jpeg(&bytes) {
            Some(img) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                // Recency for pruning, without a write for every read: it is remembered here for
                // this session, and only a file that was not used for a long while has its date
                // brought forward (for the sessions after).
                self.read.lock().unwrap_or_else(|e| e.into_inner()).insert(key.0);
                let now = std::time::SystemTime::now();
                let unused = std::fs::metadata(&p).and_then(|m| m.modified()).ok().and_then(|at| now.duration_since(at).ok());
                if unused.is_some_and(|d| d > STALE)
                    && let Ok(f) = std::fs::File::options().write(true).open(&p)
                {
                    let _ = f.set_modified(now);
                }
                Some(img)
            }
            None => {
                log::warn!("preview cache: dropping unreadable {}", p.display());
                let _ = std::fs::remove_file(&p);
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    pub fn put(&self, key: Hash128, img: &Rgba8) {
        let Some(bytes) = encode_jpeg(img) else { return };
        let p = self.path(key);
        let Some(parent) = p.parent() else { return };
        // a folder the cache makes for itself says that it is a cache; one that was there
        // already may hold somebody's files, and is not declared one
        let made = !self.dir.exists();
        if std::fs::create_dir_all(parent).is_err() || !is_real_dir(parent) {
            return;
        }
        if made {
            let _ = std::fs::write(self.dir.join(TAG), TAG_TEXT);
        }
        // Serialize the file replacement and its size accounting. A repeated render of
        // the same key replaces its JPEG; it must not count as another cache entry.
        let mut total = self.total.lock().unwrap_or_else(|e| e.into_inner());
        let used = total.get_or_insert_with(|| self.scan().iter().map(|f| f.1).sum());
        let old_len = std::fs::metadata(&p).map_or(0, |m| m.len());
        let tmp = p.with_extension(format!("tmp{:?}", std::thread::current().id()).replace(['(', ')'], ""));
        if std::fs::write(&tmp, &bytes).is_err() || std::fs::rename(&tmp, &p).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        self.writes.fetch_add(1, Ordering::Relaxed);
        *used = used.saturating_sub(old_len).saturating_add(bytes.len() as u64);
        let over = *used > self.budget();
        drop(total);
        if over {
            self.prune();
        }
    }

    /// All cache files (only names of the cache's own shape, see [`is_cache_file`]):
    /// (path, size, modified).
    fn scan(&self) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
        self.scan_until(&|| false).unwrap_or_default()
    }

    /// [`Self::scan`], given up (`None`) when `stop` says so, which is asked for each shard.
    fn scan_until(&self, stop: &dyn Fn() -> bool) -> Option<Vec<(PathBuf, u64, std::time::SystemTime)>> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return Some(out) };
        for sub in rd.flatten() {
            if stop() {
                return None;
            }
            let sub_name = sub.file_name();
            let Some(sub_name) = sub_name.to_str() else { continue };
            if !is_shard_name(sub_name) {
                continue;
            }
            // a shard that is a symlink (or junction) leads outside the cache: never walk it
            if !is_real_dir(&sub.path()) {
                continue;
            }
            let Ok(files) = std::fs::read_dir(sub.path()) else { continue };
            for f in files.flatten() {
                let name = f.file_name();
                let Some(name) = name.to_str() else { continue };
                if !is_cache_file(sub_name, name) {
                    continue;
                }
                // `DirEntry::metadata` does not follow symlinks: a link is never a cache file
                if let Ok(m) = f.metadata()
                    && m.is_file()
                {
                    out.push((f.path(), m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH)));
                }
            }
        }
        Some(out)
    }

    /// Whether the folder may hold thumbnails under the name they had before ([`OLD_EXT`]): it
    /// is there, and neither made by a cache that names them as now (which tags it, [`TAG`])
    /// nor looked through by one ([`TIDIED`]). Cheap: asked when a library opens.
    pub fn needs_tidying(&self) -> bool {
        self.dir.is_dir() && !self.dir.join(TAG).exists() && !self.dir.join(TIDIED).exists()
    }

    /// Take over the thumbnails cached under the name of before: each `<32 hex>.jpg` of the
    /// cache's own shape is renamed (or removed, when the thumbnail is there under the new name
    /// too). After a whole pass the folder is marked as looked through, so that this is done
    /// once, and tagged as a cache if it holds nothing but the cache. Returns how many were
    /// renamed. `stop` is asked for each shard listed and each file renamed; `progress` is told
    /// (done, of) as the old files are gone through.
    ///
    /// This walks the whole folder and renames file by file (a cache of 2 GB is some 100 000
    /// files): it is for a background thread, never for the interface.
    pub fn take_over_old(&self, stop: &dyn Fn() -> bool, progress: &dyn Fn(usize, usize)) -> usize {
        // (listed in full before anything is renamed: a folder that changes while it is listed
        // lists some of its files twice)
        let Some(listed) = self.scan_until(stop) else { return 0 };
        let old: Vec<(PathBuf, u64)> = listed.into_iter().filter(|f| f.0.extension().is_some_and(|ext| ext == OLD_EXT)).map(|f| (f.0, f.1)).collect();
        let (mut taken, mut removed, mut blocked) = (0usize, 0u64, false);
        for (done, (path, len)) in old.iter().enumerate() {
            if stop() {
                self.forget(removed);
                return taken;
            }
            progress(done, old.len());
            let new = path.with_extension(EXT);
            match std::fs::symlink_metadata(&new) {
                // there under both names: one and the same thumbnail
                Ok(m) if m.is_file() => {
                    if std::fs::remove_file(path).is_ok() {
                        removed = removed.saturating_add(*len);
                    }
                }
                // something that is no file has the name: not the cache's to move aside
                Ok(_) => blocked = true,
                Err(_) => match std::fs::rename(path, &new) {
                    Ok(()) => taken += 1,
                    Err(_) => blocked = true,
                },
            }
        }
        progress(old.len(), old.len());
        self.forget(removed);
        // done, also when the folder holds other things; not when something could not be taken
        // over (a folder that can't be written: it is tried again at the next open)
        if !blocked {
            let _ = std::fs::write(self.dir.join(TIDIED), b"");
            if self.holds_only_the_cache() {
                let _ = std::fs::write(self.dir.join(TAG), TAG_TEXT);
            }
        }
        taken
    }

    /// Take `bytes` that are no longer on disk off the total, when it is known.
    fn forget(&self, bytes: u64) {
        if let Some(total) = self.total.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            *total = total.saturating_sub(bytes);
        }
    }

    /// Whether everything in the folder is the cache's: its own marks, and shard folders that
    /// hold only its own files under the name they have now. What an operating system leaves in
    /// a folder that was looked at ([`DROPPINGS`]) does not count against it. (A folder with
    /// anything else in it may be somebody's, issue #98.)
    fn holds_only_the_cache(&self) -> bool {
        let dropping = |name: &str| DROPPINGS.contains(&name) || name.starts_with("._");
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return false };
        entries.flatten().all(|e| {
            let name = e.file_name();
            let Some(name) = name.to_str() else { return false };
            if name == TAG || name == TIDIED || dropping(name) {
                return true;
            }
            // (a shard that is a symlink or junction leads somewhere else: not only the cache's)
            if !is_shard_name(name) || !is_real_dir(&e.path()) {
                return false;
            }
            let Ok(inside) = std::fs::read_dir(e.path()) else { return false };
            inside.flatten().all(|f| {
                // a link among the files is not a file the cache wrote
                let link = f.file_type().is_ok_and(|t| t.is_symlink());
                !link && f.file_name().to_str().is_some_and(|f| dropping(f) || (is_cache_file(name, f) && !f.ends_with(&format!(".{OLD_EXT}"))))
            })
        })
    }

    /// Remove least recently used files until the cache is at most 80 % of its budget: by the
    /// date of each file, the ones read in this session last.
    pub fn prune(&self) {
        let mut files = self.scan();
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        let target = self.budget() / 5 * 4;
        let read = self.read.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let in_use = |p: &Path| key_of(p).is_some_and(|k| read.contains(&k));
        files.sort_by_cached_key(|f| (in_use(&f.0), f.2));
        let mut gone = Vec::new();
        for (p, len, _) in files {
            if total <= target {
                break;
            }
            if std::fs::remove_file(&p).is_ok() {
                total = total.saturating_sub(len);
                gone.extend(key_of(&p));
            }
        }
        // (what is gone is no longer "read this session": the set holds only what is there)
        if !gone.is_empty() {
            let mut read = self.read.lock().unwrap_or_else(|e| e.into_inner());
            for key in gone {
                read.remove(&key);
            }
        }
        *self.total.lock().unwrap_or_else(|e| e.into_inner()) = Some(total);
    }

    /// Bytes on disk (scans if not yet known).
    pub fn size(&self) -> u64 {
        let mut t = self.total.lock().unwrap_or_else(|e| e.into_inner());
        *t.get_or_insert_with(|| self.scan().iter().map(|f| f.1).sum())
    }

    pub fn clear(&self) {
        let files = self.scan();
        #[cfg(test)]
        crate::tests::during_clear(self);
        for (p, _, _) in files {
            let _ = std::fs::remove_file(p);
        }
        // A thumbnail written while the files go (not in the listing) outlives the clear, so the
        // size is unknown, not zero: the next check scans the nearly empty folder. A writer
        // updates the total under the same lock as its file replacement, so none can put back
        // a total from before this.
        *self.total.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Whether `p` is a directory itself, not a symlink or Windows reparse point to one.
fn is_real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

/// The key a cache file is of, by its name.
fn key_of(path: &Path) -> Option<u128> {
    path.file_stem().and_then(|s| s.to_str()).and_then(|hex| u128::from_str_radix(hex, 16).ok())
}

fn is_lower_hex(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A shard directory name: two lowercase hex digits.
fn is_shard_name(s: &str) -> bool {
    s.len() == 2 && is_lower_hex(s)
}

/// Whether `name` inside shard directory `shard` is a file this cache wrote: `<32 hex>.lcthumb`
/// (whose first two digits are the shard), the `<32 hex>.jpg` it wrote before, or a
/// `<32 hex>.tmp<alphanumeric>` temp file of [`DiskCache::put`]. Anything else in the cache
/// directory belongs to someone else.
pub fn is_cache_file(shard: &str, name: &str) -> bool {
    if !is_shard_name(shard) {
        return false;
    }
    let Some((stem, ext)) = name.split_once('.') else { return false };
    if stem.len() != 32 || !is_lower_hex(stem) || !stem.starts_with(shard) {
        return false;
    }
    ext == EXT || ext == OLD_EXT || ext.strip_prefix("tmp").is_some_and(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Encode a thumbnail as the cache stores it (JPEG, RGB, 4:2:0).
pub fn encode_jpeg(img: &Rgba8) -> Option<Vec<u8>> {
    if img.width == 0 || img.height == 0 || img.width > u16::MAX as usize || img.height > u16::MAX as usize {
        return None;
    }
    let mut rgb = Vec::with_capacity(img.width * img.height * 3);
    for p in &img.data {
        rgb.extend_from_slice(&p[..3]);
    }
    let mut out = Vec::new();
    let mut enc = jpeg_encoder::Encoder::new(&mut out, QUALITY);
    enc.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_2_0);
    enc.encode(&rgb, img.width as u16, img.height as u16, jpeg_encoder::ColorType::Rgb).ok()?;
    Some(out)
}

/// Decode a cached thumbnail (see [`encode_jpeg`]).
pub fn decode_jpeg(bytes: &[u8]) -> Option<Rgba8> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
    let px = d.decode().ok()?;
    let info = d.info()?;
    let (w, h) = (info.width as usize, info.height as usize);
    if px.len() < w * h * 3 {
        return None;
    }
    let data = px.as_chunks::<3>().0.iter().take(w * h).map(|c| [c[0], c[1], c[2], 255]).collect();
    Some(Rgba8 { width: w, height: h, data })
}
