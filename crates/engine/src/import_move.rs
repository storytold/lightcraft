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
//! 2. Only after the catalog records pointing at the destinations were committed and saved
//!    does [`finish`] remove each source — after checking once more that its destination is
//!    still names the same live file, or verifying its bytes and rechecking both names. A source that can't be removed (a read-only card, a
//!    locked file) stays and is reported; the photo is then imported from the copy.
//!
//! A `IMG_1.xmp` sidecar is shared by `IMG_1.CR3` and `IMG_1.JPG`: it is copied along with each,
//! and removed from the source only once no photo with that name is left beside it.
//!
//! (Not `std::fs::rename`: it would remove the source before the catalog knows the new place,
//! and on Unix it silently replaces an existing destination.)

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// A photo put in place by [`place`]; its source is untouched.
#[derive(Clone, Debug)]
pub(crate) struct Placed {
    pub src: PathBuf,
    pub dst: PathBuf,
    pub sidecars: Vec<PlacedSidecar>,
    pub hash: Option<lightcraft_preview::Hash128>,
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
    /// Each copy takes a little longer and notes the thread it ran on ([`copy_threads`]), to see
    /// an import's copies run side by side.
    Slow,
    Grow,
    SwapAfterVerify,
}

#[cfg(test)]
thread_local! {
    static FAULT: std::cell::Cell<Fault> = const { std::cell::Cell::new(Fault::None) };
    static GROW_WRITER: std::cell::RefCell<Option<File>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn read_bytes() -> u64 {
    crate::originals::read_bytes()
}

#[cfg(test)]
pub(crate) struct GrowthWriter(Option<File>);
#[cfg(test)]
impl Drop for GrowthWriter {
    fn drop(&mut self) {
        GROW_WRITER.with(|writer| writer.replace(self.0.take()));
    }
}
#[cfg(test)]
pub(crate) fn grow_using(file: File) -> GrowthWriter {
    GrowthWriter(GROW_WRITER.with(|writer| writer.replace(Some(file))))
}
fn grow_source(src: &Path) -> io::Result<()> {
    #[cfg(test)]
    if let Some(result) = GROW_WRITER.with(|writer| writer.borrow_mut().as_mut().map(|file| file.write_all(b"more"))) {
        return result;
    }
    OpenOptions::new().append(true).open(src)?.write_all(b"more")
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
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    let mut dst = None;
    let mut hash = None;
    for i in 0..100_000u32 {
        let d = if i == 0 { dir.join(name) } else { dir.join(format!("{stem}-{i}{ext}")) };
        match link_or_copy(src, &d) {
            Ok(copied) => {
                dst = Some(d);
                hash = copied;
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("move {}: {e} (the original is kept)", src.display())),
        }
    }
    let Some(dst) = dst else { return Err(format!("move {}: no free name in {}", src.display(), dir.display())) };
    let mut placed = Placed { src: src.to_path_buf(), dst, sidecars: Vec::new(), hash };
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
            link_or_copy(&sc, &sc_dst).map(|_| true).map_err(|e| format!("move {}: {e} (the original is kept)", sc.display()))
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
pub(crate) fn copy_new(
    src: &Path,
    dir: &Path,
    name: &str,
    expect: Option<lightcraft_preview::Hash128>,
) -> Result<(PathBuf, lightcraft_preview::Hash128), String> {
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    for i in 0..100_000u32 {
        let d = if i == 0 { dir.join(name) } else { dir.join(format!("{stem}-{i}{ext}")) };
        match copy_hashed(src, &d, expect) {
            Ok(hash) => return Ok((d, hash)),
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
pub(crate) fn finish(p: &Placed) -> Result<Vec<(String, String)>, String> {
    let _binding = crate::originals::standalone_binding();
    let verified = (|| -> io::Result<(File, File, fs::Metadata, fs::Metadata)> {
        if !fs::symlink_metadata(&p.src)?.is_file() || !fs::symlink_metadata(&p.dst)?.is_file() {
            return Err(io::Error::other("the source or destination is not a regular file"));
        }
        let (mut src, mut dst) = (File::open(&p.src)?, File::open(&p.dst)?);
        let (sm, dm) = (src.metadata()?, dst.metadata()?);
        if !sm.is_file()
            || !dm.is_file()
            || sm.len() != dm.len()
            || (!same_handle_identity(&sm, &dm) && !same_open_bytes(&p.src, &p.dst, &mut src, &mut dst)?)
        {
            return Err(io::Error::other("the destination differs from the source"));
        }
        if !same_file_state(&sm, &src.metadata()?) || !same_file_state(&dm, &dst.metadata()?) {
            return Err(io::Error::other("a file changed while it was verified"));
        }
        Ok((src, dst, sm, dm))
    })();
    let Ok((_src, _dst, src_meta, dst_meta)) = verified else {
        return Err(format!("{} is missing or changed: the original is kept", p.dst.display()));
    };
    #[cfg(test)]
    if injected(Fault::SwapAfterVerify) {
        fs::remove_file(&p.dst).map_err(|e| e.to_string())?;
        fs::write(&p.dst, b"replaced destination").map_err(|e| e.to_string())?;
    }
    if injected(Fault::FailRemove) {
        return Err("could not remove the original (injected): it stays where it was".into());
    }
    if !current_name_matches(&p.src, &_src, &src_meta) || !current_name_matches(&p.dst, &_dst, &dst_meta) {
        return Err("a source or destination name changed after verification: the original is kept".into());
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

/// `dst` = `src` without touching `src`: a hard link, else a verified copy. Never overwrites
/// (`AlreadyExists` when `dst` is taken).
fn link_or_copy(src: &Path, dst: &Path) -> io::Result<Option<lightcraft_preview::Hash128>> {
    let force_copy = injected(Fault::NoLink) || injected(Fault::FailWrite) || injected(Fault::Corrupt);
    if !force_copy && !is_symlink(src) {
        match fs::hard_link(src, dst) {
            Ok(()) => {
                let (a, b) = (fs::metadata(src)?, fs::metadata(dst)?);
                if a.len() == b.len() {
                    return Ok(None);
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
    copy_verified(src, dst, None).map(Some)
}

/// Copy into a new file, sync it to disk and verify it: against `expect` (the probe's hash of the
/// source) when given, else byte for byte against the source. On any failure the partial copy is
/// removed.
pub(crate) fn copy_verified(src: &Path, dst: &Path, expect: Option<lightcraft_preview::Hash128>) -> io::Result<lightcraft_preview::Hash128> {
    copy_checked(src, dst, |_| match expect {
        Some(h) if hash_file(dst)? != h => Err(io::Error::other(CHANGED_SINCE_READ)),
        Some(_) => Ok(()),
        None if !same_bytes(src, dst)? => Err(io::Error::other("the copy differs from the original")),
        None => Ok(()),
    })
}

const CHANGED_SINCE_READ: &str = "the copy differs from the original as it was read for the import (a bad copy, or the file changed meanwhile)";

pub(crate) fn copy_hashed(src: &Path, dst: &Path, expect: Option<lightcraft_preview::Hash128>) -> io::Result<lightcraft_preview::Hash128> {
    copy_checked(src, dst, |copied| {
        if expect.is_some_and(|h| h != copied) {
            return Err(io::Error::other(CHANGED_SINCE_READ));
        }
        if hash_file(dst)? != copied {
            return Err(io::Error::other("the copy differs from the original"));
        }
        Ok(())
    })
}

fn copy_checked(
    src: &Path,
    dst: &Path,
    verify: impl FnOnce(lightcraft_preview::Hash128) -> io::Result<()>,
) -> io::Result<lightcraft_preview::Hash128> {
    let _binding = crate::originals::standalone_binding();
    let before = fs::metadata(src)?;
    let mut out = OpenOptions::new().write(true).create_new(true).open(dst)?;
    let r = (|| {
        if injected(Fault::FailWrite) {
            out.write_all(b"partial")?;
            return Err(io::Error::other("write failed (injected)"));
        }
        let (copied, len) = stream_copy(&mut out, src)?;
        #[cfg(test)]
        if injected(Fault::Slow) {
            COPY_THREADS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(std::thread::current().id());
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        if injected(Fault::Corrupt) {
            out.write_all(b"!")?;
        }
        out.sync_all()?;
        let after = fs::metadata(src)?;
        if len != before.len() || after.len() != before.len() || after.modified().ok() != before.modified().ok() {
            return Err(io::Error::other("the original changed while it was copied"));
        }
        if let Ok(m) = after.modified() {
            let _ = out.set_modified(m);
        }
        Ok(copied)
    })();
    drop(out);
    let r = r.and_then(|copied| {
        verify(copied)?;
        Ok(copied)
    });
    if r.is_err() {
        let _ = fs::remove_file(dst);
        if let Some(reader) = crate::originals::bound_reader() {
            if let Some(path) = src.to_str() {
                reader.discard_candidate(path);
            }
            if let Some(path) = dst.to_str() {
                reader.discard_candidate(path);
            }
        }
    }
    r
}

fn stream_copy(out: &mut File, src: &Path) -> io::Result<(lightcraft_preview::Hash128, u64)> {
    struct CopyWriter<'a> {
        out: &'a mut File,
        src: &'a Path,
        wrote: bool,
    }
    impl Write for CopyWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let n = self.out.write(bytes)?;
            if n > 0 && !self.wrote {
                self.wrote = true;
                if injected(Fault::Grow) {
                    grow_source(self.src)?;
                }
            }
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.out.flush()
        }
    }
    let path = src.to_str().ok_or_else(|| io::Error::other("original path is not UTF-8"))?;
    let fp = crate::originals::copy_whole(path, &mut CopyWriter { out, src, wrote: false })?;
    Ok((fp.hash, fp.len()))
}

fn hash_file(path: &Path) -> io::Result<lightcraft_preview::Hash128> {
    crate::originals::hash_whole(path.to_str().ok_or_else(|| io::Error::other("original path is not UTF-8"))?).map(|fp| fp.hash)
}

/// Do two files hold the same bytes?
pub(crate) fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    let _binding = crate::originals::standalone_binding();
    if fs::metadata(a)?.len() != fs::metadata(b)?.len() {
        return Ok(false);
    }
    let (mut fa, mut fb) = (File::open(a)?, File::open(b)?);
    same_open_bytes(a, b, &mut fa, &mut fb)
}

fn current_name_matches(path: &Path, held: &File, before: &fs::Metadata) -> bool {
    if !fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && same_file_state(before, &m)) {
        return false;
    }
    #[cfg(windows)]
    {
        let Ok(current) = File::open(path) else { return false };
        lightcraft_file_identity_sys::identity(held).ok().is_some_and(|id| lightcraft_file_identity_sys::identity(&current).ok() == Some(id))
    }
    #[cfg(not(windows))]
    {
        let _ = held;
        true
    }
}

fn same_handle_identity(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() != 0 && a.ino() != 0 && a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        false
    }
}
fn same_file_state(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    let same = a.is_file() && b.is_file() && a.len() == b.len() && a.modified().ok().zip(b.modified().ok()).is_some_and(|(a, b)| a == b);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        same && same_handle_identity(a, b) && a.ctime() == b.ctime() && a.ctime_nsec() == b.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        same
    }
}
fn same_open_bytes(a: &Path, b: &Path, fa: &mut File, fb: &mut File) -> io::Result<bool> {
    let a = a.to_str().ok_or_else(|| io::Error::other("original path is not UTF-8"))?;
    let b = b.to_str().ok_or_else(|| io::Error::other("original path is not UTF-8"))?;
    crate::originals::compare_open(a, b, fa, fb)
}
