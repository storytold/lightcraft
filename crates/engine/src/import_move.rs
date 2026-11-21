//! Import → **Move**: the file transfer behind [`crate::import::ImportMode::Move`].
//!
//! Moving deletes the user's originals, so it is done in two phases and every step that could
//! lose data comes last:
//!
//! 1. [`place`] puts the photo (and its XMP sidecars) at the destination **without touching the
//!    source**: a hard link when source and destination share a volume (instant, no extra space,
//!    never overwrites), else a copy that is written to a new file (never overwriting), synced to
//!    disk and compared byte for byte with the original. A name that is taken gets -1, -2…
//!    Anything that fails is undone ([`rollback`]): the partial destination is removed and the
//!    source stays as it was.
//! 2. Only after the catalog records pointing at the destinations were committed and saved do the
//!    destination folders' entries go to disk ([`sync_folders`] — once per folder for the batch, so
//!    a batch into one folder flushes it once) and then [`finish`] remove each source —
//!    after checking once more that its destination is still there with the same bytes. A file's own
//!    sync does not cover the folder entry naming it, so a crash between removing an original and
//!    that entry reaching disk would keep the deletion and lose the photo (issue #1501). A source
//!    whose folder could not be flushed, or that can't be removed (a read-only card, a locked file),
//!    stays and is reported; the photo is then imported from the copy.
//!
//! A `IMG_1.xmp` sidecar is shared by `IMG_1.CR3` and `IMG_1.JPG`: it is copied along with each,
//! and removed from the source only once no photo with that name is left beside it.
//!
//! (Not `std::fs::rename`: it would remove the source before the catalog knows the new place,
//! and on Unix it silently replaces an existing destination.)

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// A photo put in place by [`place`]; its source is untouched.
#[derive(Clone, Debug)]
pub(crate) struct Placed {
    pub src: PathBuf,
    pub dst: PathBuf,
    pub sidecars: Vec<PlacedSidecar>,
}

#[derive(Clone, Debug)]
pub(crate) struct PlacedSidecar {
    pub src: PathBuf,
    pub dst: PathBuf,
    /// `false`: an identical sidecar was already there (the other half of a raw + JPEG pair).
    pub created: bool,
    /// Named after the photo's stem (`IMG_1.xmp`), so possibly shared with a sibling photo.
    pub stem: bool,
}

/// Test-only fault injection (per thread), to exercise the failure paths.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum Fault {
    #[default]
    None,
    /// Hard links fail, as across volumes: the copy path runs.
    NoLink,
    /// Writing the copy fails half-way (implies `NoLink`).
    FailWrite,
    /// The copy comes out different from the original, caught by the verification (implies `NoLink`).
    Corrupt,
    /// Removing the source fails (as on a read-only card).
    FailRemove,
    /// Syncing the destination's folder to disk fails.
    FailDirSync,
    /// Syncing fails for the folder whose path ends like this, and for no other (a mixed batch).
    FailDirSyncIn(&'static str),
    /// Syncing a folder is refused, as on a platform or file system without one (Windows needs a
    /// flush the standard library cannot ask for): nothing is flushed, and nothing failed.
    RefuseDirSync,
    /// Each copy takes a little longer and notes the thread it ran on ([`copy_threads`]), to see
    /// an import's copies run side by side.
    Slow,
}

#[cfg(test)]
thread_local! {
    static FAULT: std::cell::Cell<Fault> = const { std::cell::Cell::new(Fault::None) };
}

/// Inject a fault for the calling thread's next moves (tests only).
#[cfg(test)]
pub(crate) fn inject(f: Fault) {
    FAULT.with(|c| c.set(f));
}

/// The calling thread's injected fault (tests only), for the workers it starts to inherit.
#[cfg(test)]
pub(crate) fn injected_fault() -> Fault {
    FAULT.with(|c| c.get())
}

/// The threads copies made under [`Fault::Slow`] ran on (tests only).
#[cfg(test)]
static COPY_THREADS: std::sync::Mutex<Vec<std::thread::ThreadId>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(crate) fn copy_threads() -> Vec<std::thread::ThreadId> {
    COPY_THREADS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

fn injected(f: Fault) -> bool {
    #[cfg(test)]
    {
        FAULT.with(|c| c.get()) == f
    }
    #[cfg(not(test))]
    {
        let _ = f;
        false
    }
}

/// Is `path` inside `dir` (as written, or once symlinks and `..` are resolved)?
pub(crate) fn inside(path: &Path, dir: &Path) -> bool {
    if path.starts_with(dir) {
        return true;
    }
    match (fs::canonicalize(path), fs::canonicalize(dir)) {
        (Ok(p), Ok(d)) => p.starts_with(d),
        _ => false,
    }
}

pub(crate) fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// The XMP sidecars beside `src`, both naming conventions (`IMG_1.xmp`, `IMG_1.CR3.xmp`; the
/// `.xmp` extension in any case): (path, named after the stem).
pub(crate) fn sidecars_of(src: &Path) -> Vec<(PathBuf, bool)> {
    let (Some(dir), Some(name)) = (src.parent(), src.file_name().map(|n| n.to_string_lossy().to_string())) else { return Vec::new() };
    let stem = src.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<(PathBuf, bool)> = rd
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            let (base, ext) = n.rsplit_once('.')?;
            if !ext.eq_ignore_ascii_case("xmp") || !e.path().is_file() {
                return None;
            }
            // `IMG_1.cr3.xmp` names `IMG_1.CR3` too: some apps lower-case the photo's extension
            let full = base == name
                || base.rsplit_once('.').is_some_and(|(s, x)| s == stem && name.rsplit_once('.').is_some_and(|(_, y)| x.eq_ignore_ascii_case(y)));
            if full {
                Some((e.path(), false))
            } else if base == stem {
                Some((e.path(), true))
            } else {
                None
            }
        })
        .collect();
    out.sort();
    out
}

/// Put `src` into `dir` as `name` (-1, -2… when taken) with its sidecars, leaving the source
/// untouched. On failure nothing is left at the destination.
pub(crate) fn place(src: &Path, dir: &Path, name: &str) -> Result<Placed, String> {
    create_dir_durable(dir).map_err(|e| format!("could not make {} durable ({e}): the original is kept", dir.display()))?;
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    let mut dst = None;
    for i in 0..100_000u32 {
        let d = if i == 0 { dir.join(name) } else { dir.join(format!("{stem}-{i}{ext}")) };
        match link_or_copy(src, &d) {
            Ok(()) => {
                dst = Some(d);
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("move {}: {e} (the original is kept)", src.display())),
        }
    }
    let Some(dst) = dst else { return Err(format!("move {}: no free name in {}", src.display(), dir.display())) };
    let mut placed = Placed { src: src.to_path_buf(), dst, sidecars: Vec::new() };
    for (sc, stem_named) in sidecars_of(src) {
        let sc_ext = sc.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_else(|| "xmp".into());
        let sc_dst =
            if stem_named { placed.dst.with_extension(&sc_ext) } else { PathBuf::from(format!("{}.{sc_ext}", placed.dst.to_string_lossy())) };
        let r = if sc_dst.exists() {
            match same_bytes(&sc, &sc_dst) {
                Ok(true) => Ok(false),
                Ok(false) => Err(format!("a different sidecar is already at {} (the original is kept)", sc_dst.display())),
                Err(e) => Err(format!("{}: {e} (the original is kept)", sc_dst.display())),
            }
        } else {
            link_or_copy(&sc, &sc_dst).map(|()| true).map_err(|e| format!("move {}: {e} (the original is kept)", sc.display()))
        };
        match r {
            Ok(created) => placed.sidecars.push(PlacedSidecar { src: sc, dst: sc_dst, created, stem: stem_named }),
            Err(e) => {
                rollback(&placed);
                return Err(e);
            }
        }
    }
    Ok(placed)
}

/// Import → **Copy**: copy `src` into `dir` as `name` (-1, -2… when taken) with the same checks as
/// a move's copy — a new file (never overwriting one, even one that appears meanwhile), synced to
/// disk and verified; a bad copy is removed and reported, so a card is never wiped on the strength
/// of it. Returns the new path.
///
/// `expect` is the content hash the import's probe computed from its full read of the source
/// ([`lightcraft_preview::hash_bytes`]): the copy is read back and checked against it, which spares
/// a third read of the card (issue #134). It also catches a source that changed since the probe
/// (whose recorded metadata and hash would be stale). Without one the copy is compared byte for
/// byte with the source.
pub(crate) fn copy_new(src: &Path, dir: &Path, name: &str, expect: Option<lightcraft_preview::Hash128>) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    for i in 0..100_000u32 {
        let d = if i == 0 { dir.join(name) } else { dir.join(format!("{stem}-{i}{ext}")) };
        match copy_verified(src, &d, expect) {
            Ok(()) => return Ok(d),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("copy {}: {e} (nothing was imported from it; the original is untouched)", src.display())),
        }
    }
    Err(format!("copy {}: no free name in {}", src.display(), dir.display()))
}

/// Undo a [`place`]: remove what it created at the destination (the sources were never touched).
pub(crate) fn rollback(p: &Placed) {
    for sc in p.sidecars.iter().filter(|s| s.created) {
        let _ = fs::remove_file(&sc.dst);
    }
    let _ = fs::remove_file(&p.dst);
}

/// Remove a placed photo's source (its catalog record is committed and saved): only when the
/// destination is still there with the same bytes. Then its sidecars, unless another photo still
/// uses a shared one. `Err` = the source was kept (why); `Ok` lists sidecars kept (path, why).
///
/// The destination's folder must already be on disk — [`sync_folders`] puts it there, once per
/// folder for a whole batch (issue #1501). This does not sync it, so each committed batch
/// flushes a destination folder once rather than once per photo.
pub(crate) fn finish(p: &Placed) -> Result<Vec<(String, String)>, String> {
    // compare the bytes now: either file may have changed since the copy was verified
    let same = match (fs::metadata(&p.src), fs::metadata(&p.dst)) {
        (Ok(s), Ok(d)) => s.is_file() && d.is_file() && same_bytes(&p.src, &p.dst).unwrap_or(false),
        _ => false,
    };
    if !same {
        return Err(format!("{} is missing or changed: the original is kept", p.dst.display()));
    }
    if injected(Fault::FailRemove) {
        return Err("could not remove the original (injected): it stays where it was".into());
    }
    fs::remove_file(&p.src).map_err(|e| format!("could not remove the original ({e}): it stays where it was"))?;
    let mut kept = Vec::new();
    for sc in &p.sidecars {
        let path = sc.src.to_string_lossy().to_string();
        if !same_bytes(&sc.src, &sc.dst).unwrap_or(false) {
            kept.push((path, format!("{} is missing or changed: the sidecar is kept", sc.dst.display())));
            continue;
        }
        if sc.stem
            && let Some(other) = sibling_photo(&sc.src)
        {
            kept.push((path, format!("also the sidecar of {other}: kept beside it")));
            continue;
        }
        if let Err(e) = fs::remove_file(&sc.src) {
            kept.push((path, format!("could not remove the sidecar ({e})")));
        }
    }
    Ok(kept)
}

/// A photo (by extension) still beside a stem-named sidecar, sharing its stem.
pub(crate) fn sibling_photo(sidecar: &Path) -> Option<String> {
    let dir = sidecar.parent()?;
    let stem = sidecar.file_stem()?.to_string_lossy().to_string();
    fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find_map(|f| {
        let ok = f.file_stem().is_some_and(|s| s.to_string_lossy() == stem) && crate::import::is_supported(&f);
        ok.then(|| f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())
    })
}

/// The folder `path` is named in (the current one for a bare file name).
pub(crate) fn folder_of(path: &Path) -> &Path {
    path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

/// Flush the destination directory before removing an original. Unsupported directory flushing is
/// a compatibility exception (including our std-only non-Unix path), not a durability guarantee;
/// log it rather than claim success. Permission, path and real I/O errors keep the original. Both
/// new-folder creation and the final batch barrier use this policy (issue #1501).
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    use lightcraft_catalog::safe_file::{FolderSync, sync_dir_checked};
    if injected(Fault::RefuseDirSync) {
        no_folder_flush(dir, &io::Error::new(io::ErrorKind::Unsupported, "folder sync refused (injected)"))?;
    } else {
        match sync_dir_checked(dir)? {
            FolderSync::Durable => {}
            FolderSync::NoFolderSync(why) => no_folder_flush(dir, &why)?,
        }
    }
    // (the tests' injected failure comes after the real flush, so a test that counts flushes counts
    // the flush the file system really made)
    if dir_sync_fails(dir) {
        return Err(io::Error::other("folder sync failed (injected)"));
    }
    Ok(())
}

/// Does the calling thread's injected fault fail the folder sync for `dir`?
fn dir_sync_fails(dir: &Path) -> bool {
    #[cfg(test)]
    {
        match FAULT.with(std::cell::Cell::get) {
            Fault::FailDirSync => true,
            Fault::FailDirSyncIn(tag) => dir.to_string_lossy().ends_with(tag),
            _ => false,
        }
    }
    #[cfg(not(test))]
    {
        let _ = dir;
        false
    }
}

thread_local! {
    /// The folders whose missing flush has already been reported on this thread (a batch runs on
    /// one): a bulk move into a share that refuses them says so once per folder, not once per photo.
    /// Capped, because a session that has said it for a great many folders has said enough.
    static TOLD_NO_FOLDER_FLUSH: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// There is no way to flush a folder here: nothing failed, nothing more can be done, and the caller
/// goes on — saying so in the log rather than passing it off as a flush that worked. Once per folder
/// per thread, so a 1000-photo Move is one line and not a thousand.
fn no_folder_flush(dir: &Path, why: &io::Error) -> io::Result<()> {
    let first = TOLD_NO_FOLDER_FLUSH.with(|told| {
        let Ok(mut told) = told.try_borrow_mut() else { return false };
        if told.iter().any(|d| d == dir) || told.len() >= 256 {
            return false;
        }
        told.push(dir.to_path_buf());
        true
    });
    if first {
        log::debug!(
            "import: {}: no way to flush a folder's entries on this file system ({why}): the new name is as durable as it makes it",
            dir.display()
        );
    }
    Ok(())
}

/// Flush every distinct destination folder before the removal loop. Path components coalesce
/// trailing separators and `./`; canonicalization also coalesces symlink aliases. Resolve each
/// distinct spelling once, and open the canonical key that is used for deduplication.
///
/// Return failures under the original spelling, so every affected photo is kept even if a later
/// lookup fails or a symlink changes. Resolution failures also fail closed. This is not a defense
/// against arbitrary concurrent path replacement; `finish` still rechecks the copy's bytes.
pub(crate) fn sync_folders(placed: &[Placed]) -> BTreeMap<PathBuf, String> {
    let spellings: std::collections::BTreeSet<&Path> = placed.iter().map(|p| folder_of(&p.dst)).collect();
    let mut synced: BTreeMap<PathBuf, Result<(), String>> = BTreeMap::new();
    let mut failed = BTreeMap::new();
    for dir in spellings {
        let outcome = match fs::canonicalize(dir) {
            Ok(key) => synced.entry(key).or_insert_with_key(|key| sync_dir(key).map_err(|e| e.to_string())).clone(),
            Err(e) => Err(format!("could not resolve the destination folder: {e}")),
        };
        if let Err(why) = outcome {
            failed.insert(dir.to_path_buf(), why);
        }
    }
    failed
}

/// [`fs::create_dir_all`], with every folder it creates made durable: each new folder's entry is in its parent, so
/// the parent is synced too.
fn create_dir_durable(dir: &Path) -> io::Result<()> {
    let mut missing: Vec<&Path> = Vec::new();
    let mut at = Some(dir);
    while let Some(d) = at.filter(|d| !d.as_os_str().is_empty() && !d.exists()) {
        missing.push(d);
        at = d.parent();
    }
    fs::create_dir_all(dir)?;
    for d in &missing {
        // (a relative top folder's parent is the current one)
        if let Err(e) = sync_dir(folder_of(d)) {
            // nothing is left at the destination on failure: the folders made here go again, deepest first
            for d in &missing {
                let _ = fs::remove_dir(d);
            }
            return Err(e);
        }
    }
    Ok(())
}

/// `dst` = `src` without touching `src`: a hard link, else a verified copy. Never overwrites
/// (`AlreadyExists` when `dst` is taken).
fn link_or_copy(src: &Path, dst: &Path) -> io::Result<()> {
    let force_copy = injected(Fault::NoLink) || injected(Fault::FailWrite) || injected(Fault::Corrupt);
    if !force_copy && !is_symlink(src) {
        match fs::hard_link(src, dst) {
            Ok(()) => {
                let (a, b) = (fs::metadata(src)?, fs::metadata(dst)?);
                if a.len() == b.len() {
                    return Ok(());
                }
                let _ = fs::remove_file(dst);
                return Err(io::Error::other("the link doesn't match the original"));
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(e),
            // another volume, or no hard links there (FAT/exFAT cards, some shares): copy
            Err(_) => {}
        }
    }
    // Move keeps the byte-for-byte comparison: the source is deleted on the strength of it
    copy_verified(src, dst, None)
}

/// Copy into a new file, sync it to disk and verify it: against `expect` (the probe's hash of the
/// source) when given, else byte for byte against the source. On any failure the partial copy is
/// removed.
pub(crate) fn copy_verified(src: &Path, dst: &Path, expect: Option<lightcraft_preview::Hash128>) -> io::Result<()> {
    let mut out = OpenOptions::new().write(true).create_new(true).open(dst)?;
    let r = (|| {
        let mut inp = File::open(src)?;
        if injected(Fault::FailWrite) {
            out.write_all(b"partial")?;
            return Err(io::Error::other("write failed (injected)"));
        }
        io::copy(&mut inp, &mut out)?;
        #[cfg(test)]
        if injected(Fault::Slow) {
            COPY_THREADS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(std::thread::current().id());
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        if injected(Fault::Corrupt) {
            out.write_all(b"!")?;
        }
        out.sync_all()?;
        if let Ok(m) = fs::metadata(src).and_then(|m| m.modified()) {
            let _ = out.set_modified(m);
        }
        match expect {
            Some(h) if hash_file(dst)? != h => {
                Err(io::Error::other("the copy differs from the original as it was read for the import (a bad copy, or the file changed meanwhile)"))
            }
            Some(_) => Ok(()),
            None if !same_bytes(src, dst)? => Err(io::Error::other("the copy differs from the original")),
            None => Ok(()),
        }
    })();
    drop(out);
    if r.is_err() {
        let _ = fs::remove_file(dst);
    }
    r
}

/// [`lightcraft_preview::hash_bytes`] of a file's content, read in chunks.
fn hash_file(path: &Path) -> io::Result<lightcraft_preview::Hash128> {
    let mut f = File::open(path)?;
    let mut h = lightcraft_preview::Hasher128::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = fill(&mut f, &mut buf)?;
        if n == 0 {
            return Ok(h.finish());
        }
        h.update(buf.get(..n).unwrap_or(&[]));
    }
}

/// Do two files hold the same bytes?
pub(crate) fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    if fs::metadata(a)?.len() != fs::metadata(b)?.len() {
        return Ok(false);
    }
    let (mut fa, mut fb) = (File::open(a)?, File::open(b)?);
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let (na, nb) = (fill(&mut fa, &mut ba)?, fill(&mut fb, &mut bb)?);
        if na != nb || ba.get(..na) != bb.get(..nb) {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

/// Read until `buf` is full or the file ends.
fn fill(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while let Some(rest) = buf.get_mut(n..).filter(|r| !r.is_empty()) {
        match f.read(rest) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}
