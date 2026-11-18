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

use lcc_raster::Rgba8;

use crate::Hash128;

const QUALITY: u8 = 90;

/// What a cached thumbnail's file is named after its key: not a picture's ending, so that
/// nothing that looks for the person's photos takes the cache for them.
pub const EXT: &str = "lcthumb";
/// What it was named before (JPEG data then as now).
const OLD_EXT: &str = "jpg";
/// What a small record kept beside the thumbnails is named: not a picture but an analysis
/// result, such as a photo's eyes ([`DiskCache::put_data`]).
pub const DATA_EXT: &str = "lcdata";

/// How long a thumbnail goes unused before a read brings its date forward (the date is what a
/// full cache lets go by, oldest first). Reads within it write nothing.
pub const STALE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// The file that says the cache has looked through its folder for thumbnails under the name
/// of before and taken them over ([`DiskCache::take_over_old`]): written after a whole pass,
/// whatever else the folder holds, so that it is done once.
const TIDIED: &str = crate::legacy::TIDIED;

/// Files an operating system leaves in a folder that was looked at: nobody's, and no reason
/// to take a folder for more than a cache.
const DROPPINGS: [&str; 3] = [".DS_Store", "desktop.ini", "Thumbs.db"];

/// The file that says a folder is a cache, for backup programs and the like (the Cache
/// Directory Tagging Specification: the first line is the signature).
pub const TAG: &str = "CACHEDIR.TAG";
/// The text of the [`TAG`] file.
fn tag_text() -> String {
    let app = lcc_brand::DISPLAY_NAME;
    format!(
        "Signature: 8a477f597d28d172789f06886806bc55\n\
# This folder holds {app}'s thumbnail cache. It is safe to delete: {app} renders what it needs again.\n\
# See https://bford.info/cachedir/\n"
    )
}

/// Write `bytes` as a new file at `path`: whatever is at that name already (a leftover, or a
/// link) is removed first, the link itself and never what it points to, and the file is then
/// created exclusively, so a write never follows a link planted in the cache folder (issue #2122).
fn write_fresh(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if std::fs::symlink_metadata(path).is_ok() {
        std::fs::remove_file(path)?;
    }
    std::fs::OpenOptions::new().write(true).create_new(true).open(path)?.write_all(bytes)
}

/// Most bytes of one cache entry read into memory (a thumbnail is a few hundred KB).
const MAX_ENTRY_BYTES: u64 = 64 << 20;

/// The entry at `path`: the OPENED file must be a regular file within [`MAX_ENTRY_BYTES`], so a link planted in
/// the cache to an endless source or a FIFO is not read to its end (upstream #3728).
fn read_regular(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let f = std::fs::File::open(path)?;
    if !f.metadata()?.is_file() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"));
    }
    let mut b = Vec::new();
    f.take(MAX_ENTRY_BYTES.saturating_add(1)).read_to_end(&mut b)?;
    if u64::try_from(b.len()).is_ok_and(|n| n > MAX_ENTRY_BYTES) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "cache entry too large"));
    }
    Ok(b)
}

pub struct DiskCache {
    dir: PathBuf,
    /// Bytes the cache may hold (changed in place by [`DiskCache::set_budget`]).
    budget: AtomicU64,
    /// Bytes on disk (`None` until first scanned). Held across the first scan of the folder and each
    /// file's write: never taken on the interface's thread, which reads [`Self::known_size`]. Only
    /// this file's methods take it, so that rule lives here.
    total: Mutex<Option<u64>>,
    /// A copy of `total` that never waits ([`UNKNOWN`] until first scanned): the interface shows
    /// the cache's size every frame and must not wait for a scan or a write.
    known: AtomicU64,
    /// A background count for [`Self::known_size`] is running ([`Self::claim_count`],
    /// [`crate::PreviewCache::disk_size_now_with`]).
    sizing: std::sync::atomic::AtomicBool,
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    pub writes: AtomicU64,
    /// The thumbnails read in this session: a full cache lets them go last ([`Self::prune`]).
    read: Mutex<std::collections::HashSet<u128>>,
    /// Pictures a full cache lets go only after everything else ([`Self::keep_last`]): the edited
    /// photos' latest previews, the only pictures of their edits (a photo as shot has its camera JPEG).
    last: Mutex<std::collections::HashSet<u128>>,
    /// The set of [`Self::keep_last`] is being worked out (in the background, issue #979): a full
    /// cache prunes nothing meanwhile, so the pictures to keep last are not let go before they are known.
    last_pending: std::sync::atomic::AtomicBool,
}

impl DiskCache {
    pub fn new(dir: &Path, budget: u64) -> DiskCache {
        DiskCache {
            dir: dir.to_path_buf(),
            budget: AtomicU64::new(budget),
            total: Mutex::new(None),
            known: AtomicU64::new(UNKNOWN),
            sizing: Default::default(),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            writes: AtomicU64::new(0),
            read: Mutex::new(Default::default()),
            last: Mutex::new(Default::default()),
            last_pending: Default::default(),
        }
    }

    /// Let the pictures under `keys` go last when the cache is full (after the ones read in this
    /// session too): the edited photos' latest previews (a full cache pruned the rendered edits and
    /// the loupe had nothing but the unedited camera JPEG). Added to the set (keys added one by one
    /// with [`Self::keep_last_one`] meanwhile stay), and the set is known from now on
    /// ([`Self::keep_last_pending`]).
    pub fn keep_last(&self, keys: impl IntoIterator<Item = Hash128>) {
        self.last.lock().unwrap_or_else(|e| e.into_inner()).extend(keys.into_iter().map(|k| k.0));
        self.last_pending.store(false, Ordering::Release);
    }

    /// Say that [`Self::keep_last`]'s set is being worked out: until it is given (or
    /// [`Self::keep_last_unknown`] says it won't be), a full cache prunes nothing.
    pub fn keep_last_pending(&self) {
        self.last_pending.store(true, Ordering::Release);
    }

    /// The set [`Self::keep_last_pending`] waited for won't come (the work was stopped): the cache
    /// prunes again as it fills, with the keys it has.
    pub fn keep_last_unknown(&self) {
        self.last_pending.store(false, Ordering::Release);
    }

    /// Whether [`Self::keep_last`]'s set is still being worked out.
    pub fn is_keep_last_pending(&self) -> bool {
        self.last_pending.load(Ordering::Acquire)
    }

    /// Whether `key` is among the pictures a full cache lets go last ([`Self::keep_last`]).
    pub fn is_kept_last(&self, key: Hash128) -> bool {
        self.last.lock().unwrap_or_else(|e| e.into_inner()).contains(&key.0)
    }

    /// Add one key to [`Self::keep_last`]'s set.
    pub fn keep_last_one(&self, key: Hash128) {
        self.last.lock().unwrap_or_else(|e| e.into_inner()).insert(key.0);
    }

    pub fn dir(&self) -> &Path {
        &self.dir
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
        if let Ok(bytes) = read_regular(&p) {
            return Some((p, bytes));
        }
        let old = self.named(key, OLD_EXT);
        let Ok(bytes) = read_regular(&old) else {
            // (taken over by the tidying between the two looks: it is under the new name now)
            return read_regular(&p).ok().map(|bytes| (p, bytes));
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

    /// The size (width, height) of the picture kept under `key`, read from the start of its file
    /// only (no decode, no hit counted, nothing written): whether a preview is there and big
    /// enough, without loading it.
    pub fn dims(&self, key: Hash128) -> Option<(usize, usize)> {
        use std::io::Read;
        let p = self.path(key);
        if !p.parent().is_some_and(is_real_dir) {
            return None;
        }
        let f = std::fs::File::open(&p).or_else(|_| std::fs::File::open(self.named(key, OLD_EXT))).ok()?;
        let mut head = Vec::with_capacity(HEADER_BYTES as usize);
        f.take(HEADER_BYTES).read_to_end(&mut head).ok()?;
        jpeg_dims(&head)
    }

    pub fn put(&self, key: Hash128, img: &Rgba8) {
        // what `get` would refuse to read back isn't written
        if img.width.max(img.height) > MAX_EDGE {
            return;
        }
        let Some(bytes) = encode_jpeg(img) else { return };
        self.write(self.path(key), &bytes);
    }

    /// The record kept under `key` with [`Self::put_data`] (`None`: there is none). A read
    /// counts as a use, like a thumbnail's. What the bytes mean is the caller's: one it can't
    /// read it drops with [`Self::remove_data`].
    pub fn get_data(&self, key: Hash128) -> Option<Vec<u8>> {
        let p = self.named(key, DATA_EXT);
        if !p.parent().is_some_and(is_real_dir) {
            return None;
        }
        let Ok(bytes) = read_regular(&p) else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        self.hits.fetch_add(1, Ordering::Relaxed);
        self.read.lock().unwrap_or_else(|e| e.into_inner()).insert(key.0);
        let now = std::time::SystemTime::now();
        let unused = std::fs::metadata(&p).and_then(|m| m.modified()).ok().and_then(|at| now.duration_since(at).ok());
        if unused.is_some_and(|d| d > STALE)
            && let Ok(f) = std::fs::File::options().write(true).open(&p)
        {
            let _ = f.set_modified(now);
        }
        Some(bytes)
    }

    /// Keep `bytes` under `key` (written via temp + rename, counted in the cache's size and let
    /// go like a thumbnail when the cache is full).
    pub fn put_data(&self, key: Hash128, bytes: &[u8]) {
        self.write(self.named(key, DATA_EXT), bytes);
    }

    /// Drop the record under `key` (one that could not be read).
    pub fn remove_data(&self, key: Hash128) {
        let p = self.named(key, DATA_EXT);
        if !p.parent().is_some_and(is_real_dir) {
            return;
        }
        let Ok(m) = std::fs::symlink_metadata(&p) else { return };
        if !m.is_file() || std::fs::remove_file(&p).is_err() {
            return;
        }
        let mut t = self.total.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(total) = t.as_mut() {
            *total = total.saturating_sub(m.len());
            self.known.store(*total, Ordering::Relaxed);
        }
    }

    /// Write `bytes` to the cache file `p` (via temp + rename) and trim the cache when that takes
    /// it over its budget.
    fn write(&self, p: PathBuf, bytes: &[u8]) {
        let Some(parent) = p.parent() else { return };
        // a folder the cache makes for itself says that it is a cache; one that was there
        // already may hold somebody's files, and is not declared one
        let made = !self.dir.exists();
        if std::fs::create_dir_all(parent).is_err() || !is_real_dir(parent) {
            return;
        }
        if made {
            let _ = write_fresh(&self.dir.join(TAG), tag_text().as_bytes());
        }
        // Serialize the file replacement and its size accounting. A repeated render of
        // the same key replaces its JPEG; it must not count as another cache entry.
        let mut total = self.total.lock().unwrap_or_else(|e| e.into_inner());
        let used = total.get_or_insert_with(|| self.scan().iter().map(|f| f.1).sum());
        let old_len = std::fs::metadata(&p).map_or(0, |m| m.len());
        let tmp = p.with_extension(format!("tmp{:?}", std::thread::current().id()).replace(['(', ')'], ""));
        if write_fresh(&tmp, bytes).is_err() || std::fs::rename(&tmp, &p).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        self.writes.fetch_add(1, Ordering::Relaxed);
        *used = used.saturating_sub(old_len).saturating_add(bytes.len() as u64);
        self.known.store(*used, Ordering::Relaxed);
        let over = *used > self.budget();
        drop(total);
        // (not while the pictures to keep last are being worked out: the first write after prunes)
        if over && !self.is_keep_last_pending() {
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
            let _ = write_fresh(&self.dir.join(TIDIED), b"");
            if self.holds_only_the_cache() {
                let _ = write_fresh(&self.dir.join(TAG), tag_text().as_bytes());
            }
        }
        taken
    }

    /// Take `bytes` that are no longer on disk off the total, when it is known.
    fn forget(&self, bytes: u64) {
        if let Some(total) = self.total.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            *total = total.saturating_sub(bytes);
            self.known.store(*total, Ordering::Relaxed);
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
    /// date of each file, the ones read in this session later, the ones to keep last
    /// ([`Self::keep_last`]) last of all.
    pub fn prune(&self) {
        let mut files = self.scan();
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        let target = self.budget() / 5 * 4;
        let read = self.read.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let last = self.last.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let in_use = |p: &Path| key_of(p).is_some_and(|k| read.contains(&k));
        let kept = |p: &Path| key_of(p).is_some_and(|k| last.contains(&k));
        files.sort_by_cached_key(|f| (kept(&f.0), in_use(&f.0), f.2));
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
        // published under the lock, so a write that lands meanwhile can't be overtaken by this older count
        let mut published = self.total.lock().unwrap_or_else(|e| e.into_inner());
        *published = Some(total);
        self.known.store(total, Ordering::Relaxed);
        drop(published);
    }

    /// Bytes on disk when they are known without a scan. Never waits (a scan or a write in progress
    /// on another thread): safe on the interface's thread.
    pub fn known_size(&self) -> Option<u64> {
        Some(self.known.load(Ordering::Relaxed)).filter(|b| *b != UNKNOWN)
    }

    /// Claim the one background count of the size: false while another runs. The claimant calls
    /// [`Self::size`] off the interface's thread, then [`Self::count_ended`].
    pub(crate) fn claim_count(&self) -> bool {
        !self.sizing.swap(true, Ordering::AcqRel)
    }

    /// The background count claimed by [`Self::claim_count`] ended (or never started).
    pub(crate) fn count_ended(&self) {
        self.sizing.store(false, Ordering::Release);
    }

    /// The size's lock, held as a write or the first scan of the folder holds it (tests only).
    #[cfg(test)]
    pub(crate) fn hold_count(&self) -> std::sync::MutexGuard<'_, Option<u64>> {
        self.total.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Bytes on disk (scans if not yet known: a walk of the whole folder, for a background thread).
    pub fn size(&self) -> u64 {
        let mut t = self.total.lock().unwrap_or_else(|e| e.into_inner());
        let b = *t.get_or_insert_with(|| self.scan().iter().map(|f| f.1).sum());
        self.known.store(b, Ordering::Relaxed);
        b
    }

    /// When the file of `key` was last written or read (`None`: not on disk).
    pub fn modified(&self, key: Hash128) -> Option<std::time::SystemTime> {
        std::fs::metadata(self.path(key)).ok().filter(|m| m.is_file()).and_then(|m| m.modified().ok())
    }

    /// Delete the file of `key`; returns the bytes freed (0: it was not there).
    pub fn remove(&self, key: Hash128) -> u64 {
        let p = self.path(key);
        let Ok(m) = std::fs::symlink_metadata(&p) else { return 0 };
        if !m.is_file() || std::fs::remove_file(&p).is_err() {
            return 0;
        }
        let mut t = self.total.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(total) = t.as_mut() {
            *total = total.saturating_sub(m.len());
            self.known.store(*total, Ordering::Relaxed);
        }
        m.len()
    }

    pub fn clear(&self) {
        for (p, _, _) in self.scan() {
            let _ = std::fs::remove_file(p);
        }
        let mut published = self.total.lock().unwrap_or_else(|e| e.into_inner());
        *published = Some(0);
        self.known.store(0, Ordering::Relaxed);
        drop(published);
    }
}

/// [`DiskCache::known`] before the first scan.
const UNKNOWN: u64 = u64::MAX;

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
/// (whose first two digits are the shard), the `<32 hex>.jpg` it wrote before, a record
/// `<32 hex>.lcdata` ([`DiskCache::put_data`]), or a
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
    ext == EXT
        || ext == OLD_EXT
        || ext == DATA_EXT
        || ext.strip_prefix("tmp").is_some_and(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_alphanumeric()))
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

/// Longest side of an image the cache keeps: the loupe's largest render (`LOUPE_EDGE_CEILING`).
/// [`decode_jpeg`] refuses larger ones from their header, so a file planted in the cache folder
/// costs at most 200 MB to read, not the decoder's default 16384² (issue #2140).
pub const MAX_EDGE: usize = 8192;

/// How much of a cached file [`DiskCache::dims`] reads: the JPEG headers this cache writes come
/// first and are a few hundred bytes.
const HEADER_BYTES: u64 = 64 << 10;

/// A JPEG's (width, height) from its headers (`None`: not a JPEG, or cut short before its frame header).
pub fn jpeg_dims(bytes: &[u8]) -> Option<(usize, usize)> {
    use zune_core::bytestream::ZCursor;
    let mut d = zune_jpeg::JpegDecoder::new(ZCursor::new(bytes));
    d.decode_headers().ok()?;
    let info = d.info()?;
    let (w, h) = (info.width as usize, info.height as usize);
    (w > 0 && h > 0).then_some((w, h))
}

/// Decode a cached thumbnail (see [`encode_jpeg`]): `None` for anything else, larger than
/// [`MAX_EDGE`] (from the header, before its pixels are allocated), or that panics the decoder.
pub fn decode_jpeg(bytes: &[u8]) -> Option<Rgba8> {
    std::panic::catch_unwind(|| decode_jpeg_unguarded(bytes)).ok().flatten()
}

fn decode_jpeg_unguarded(bytes: &[u8]) -> Option<Rgba8> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let opts = DecoderOptions::default().set_max_width(MAX_EDGE).set_max_height(MAX_EDGE).jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
    d.decode_headers().ok()?;
    let info = d.info()?;
    if usize::from(info.width).max(usize::from(info.height)) > MAX_EDGE {
        return None;
    }
    let px = d.decode().ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    if px.len() < w * h * 3 {
        return None;
    }
    let data = px.as_chunks::<3>().0.iter().take(w * h).map(|c| [c[0], c[1], c[2], 255]).collect();
    Some(Rgba8 { width: w, height: h, data })
}
