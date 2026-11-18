//! Crash-safe writes of user-visible files: exports, renders, DNG conversions, smart previews and
//! XMP sidecars all go through here (the catalog's own files use [`crate::store::FsStore`]).
//!
//! **Write policy** (issue #134): what can't be recreated — originals, XMP sidecars, the catalog,
//! settings, DNGs that replace raws, smart previews (the only stand-in while an original is
//! offline), an edit copy that becomes a library photo — is written **durably**: synced to disk
//! before it counts as written. What can always be made again from the originals — exports,
//! renders, screenshots, merge previews — is written **atomically** only: a half-written file
//! never replaces a good one, but there is no per-file sync (on a USB drive or a NAS that sync
//! dominates a large export).
//!
//! - [`write_atomic`] replaces a file so that after a crash, a full disk or an unplugged drive
//!   either the old or the complete new content is there — never a truncated mix. The content
//!   goes to a new temp file in the same folder (unique name, `create_new`), is synced to disk and
//!   renamed over the target; then the folder is synced (POSIX). Any failure removes the temp
//!   file and leaves the target as it was.
//! - [`write_atomic_nosync`] is the same temp file + rename without the two syncs, for
//!   recreatable outputs only: a failed write still removes the temp file and leaves the target as
//!   it was, but after a power cut the new file may be lost (on some file systems left empty) —
//!   it is then simply exported again.
//! - [`write_new`] / [`write_new_unique`] write a file that must not exist yet: the same temp file
//!   and sync, plus a read-back check, and the file then appears under its final name **without
//!   replacing anything** (a hard link, which fails on a taken name; where the volume has no hard
//!   links, an exclusive create that claims the name, then a rename over that placeholder).
//! - [`same_file`] tells whether two paths name the same file on disk (device + inode on Unix,
//!   volume serial number + file index on Windows,
//!   resolved paths elsewhere), for "is this target an original?" checks.
//! - [`open_regular`] / [`open_or_create_regular`] open a library file in place (to read, append
//!   or cut it), never through a link planted at its name, nor a FIFO or a device (issue #2552).
//!   The replacing writes above are safe already: a rename replaces a link, it doesn't follow it.
//! - [`open_existing`], [`open_or_create`] and [`read_no_follow`] open a file in place (to append,
//!   cut or read it) **never through a link** planted at its name, nor a FIFO or a device: a
//!   library or photo folder may be shared, synced or unpacked from an archive, and a link there
//!   would have the write land in, or the read copy, a file elsewhere (issue #2122). The replacing
//!   writes above are safe already: a rename replaces a link, it doesn't follow it.
//! - [`sync_file`] flushes a file to disk, with a plain `fsync` where the volume refuses the full
//!   flush (an SMB share mounted by macOS, issue #1407). Every durable write goes through it: the
//!   ones here, the catalog's and the engine's own (import copies, moves, sidecar backups).

use std::cell::Cell;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

thread_local! {
    static FAIL_AFTER: Cell<Option<u64>> = const { Cell::new(None) };
    static SYNCS: Cell<u64> = const { Cell::new(0) };
    static NO_FULL_SYNC: Cell<bool> = const { Cell::new(false) };
    static PLAIN_FSYNCS: Cell<u64> = const { Cell::new(0) };
}

/// How many file and folder syncs this module made **on the calling thread** (for tests: a
/// durable write path syncs, a recreatable output doesn't).
#[doc(hidden)]
pub fn syncs_on_this_thread() -> u64 {
    SYNCS.with(Cell::get)
}

fn counted_sync(f: &File) -> io::Result<()> {
    SYNCS.with(|c| c.set(c.get() + 1));
    sync_file(f, false)
}

/// Flush a file to disk: `sync_all`, or `sync_data` when `data_only`.
///
/// On macOS both are `F_FULLFSYNC`, which an SMB share refuses with ENOTSUP (issue #1407) while a
/// plain `fsync` works there: that is the fallback. If it fails too, the write fails.
pub fn sync_file(f: &File, data_only: bool) -> io::Result<()> {
    let r = if NO_FULL_SYNC.with(Cell::get) {
        Err(enotsup())
    } else if data_only {
        f.sync_data()
    } else {
        f.sync_all()
    };
    match r {
        Err(e) if flush_unsupported(&e) => {
            PLAIN_FSYNCS.with(|c| c.set(c.get() + 1));
            lcc_sysmem::fsync(f)
        }
        r => r,
    }
}

/// The volume can't flush a file to disk (ENOTSUP on macOS, `Unsupported` elsewhere).
fn flush_unsupported(e: &io::Error) -> bool {
    const ENOTSUP: i32 = 45;
    e.kind() == io::ErrorKind::Unsupported || (cfg!(target_vendor = "apple") && e.raw_os_error() == Some(ENOTSUP))
}

/// What a volume without a full flush answers (macOS: ENOTSUP, os error 45, from `F_FULLFSYNC`).
fn enotsup() -> io::Error {
    if cfg!(target_vendor = "apple") { io::Error::from_raw_os_error(45) } else { io::Error::from(io::ErrorKind::Unsupported) }
}

/// How many plain `fsync` fallbacks [`sync_file`] made **on the calling thread** (for tests).
#[doc(hidden)]
pub fn plain_fsyncs_on_this_thread() -> u64 {
    PLAIN_FSYNCS.with(Cell::get)
}

/// For tests (in any crate): while the guard lives, every full flush [`sync_file`] asks for on
/// the calling thread fails as on an SMB share mounted by macOS (issue #1407).
#[doc(hidden)]
pub fn full_sync_unsupported() -> NoFullSync {
    NO_FULL_SYNC.with(|c| c.set(true));
    NoFullSync(())
}

/// Ends a [`full_sync_unsupported`] when dropped.
#[doc(hidden)]
pub struct NoFullSync(());

impl Drop for NoFullSync {
    fn drop(&mut self) {
        NO_FULL_SYNC.with(|c| c.set(false));
    }
}

/// Fault injection for tests (in any crate): while the returned guard lives, every write made by
/// this module **on the calling thread** fails once `bytes` bytes of a file were written — as a
/// full disk or an unplugged drive would.
#[doc(hidden)]
pub fn fail_writes_after(bytes: u64) -> FaultGuard {
    FAIL_AFTER.with(|c| c.set(Some(bytes)));
    FaultGuard(())
}

/// Whether a write of `len` bytes made outside this module (the catalog store's own file, which
/// redb writes) fails under an injected [`fail_writes_after`] limit on the calling thread: when
/// `len` alone exceeds the limit (nothing is counted across such writes).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn injected_fault(len: u64) -> bool {
    FAIL_AFTER.with(Cell::get).is_some_and(|limit| len > limit)
}

/// Ends a [`fail_writes_after`] when dropped.
#[doc(hidden)]
pub struct FaultGuard(());

impl Drop for FaultGuard {
    fn drop(&mut self) {
        FAIL_AFTER.with(|c| c.set(None));
    }
}

/// Counts what is written; fails past an injected limit.
struct Limited<W> {
    inner: W,
    n: u64,
    limit: Option<u64>,
}

impl<W: Write> Write for Limited<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let buf = match self.limit {
            Some(l) => {
                let room = usize::try_from(l.saturating_sub(self.n)).unwrap_or(usize::MAX);
                if room == 0 && !buf.is_empty() {
                    return Err(io::Error::other("write failed (injected fault)"));
                }
                buf.get(..room.min(buf.len())).unwrap_or(buf)
            }
            None => buf,
        };
        let k = self.inner.write(buf)?;
        self.n += k as u64;
        Ok(k)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn parent_of(path: &Path) -> &Path {
    path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

/// Make a rename or a new name in `dir` durable (POSIX). Opening a directory fails on Windows,
/// where NTFS journals the rename itself, so errors are ignored.
pub fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = counted_sync(&d);
    }
}

/// A new, empty temp file next to `path` (hidden, unique in this process and folder).
fn create_temp(path: &Path) -> io::Result<(PathBuf, File)> {
    static N: AtomicU64 = AtomicU64::new(0);
    if path.file_name().is_none() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is not a file path", path.display())));
    }
    let dir = parent_of(path);
    for _ in 0..1000 {
        let tmp = dir.join(format!(".{}-{}-{}.tmp", lcc_brand::BINARY, std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => return Ok((tmp, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("no free temp file name in {}", dir.display())))
}

/// Write a temp file next to `path` with what `fill` writes, synced to disk when `sync`. On
/// failure the temp file is removed. Returns its path and length.
fn write_temp(path: &Path, sync: bool, fill: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>) -> io::Result<(PathBuf, u64)> {
    let (tmp, file) = create_temp(path)?;
    let r = (|| {
        let limit = FAIL_AFTER.with(Cell::get);
        let mut w = Limited { inner: io::BufWriter::with_capacity(1 << 20, file), n: 0, limit };
        fill(&mut w)?;
        w.flush()?;
        let n = w.n;
        let f = w.inner.into_inner().map_err(|e| e.into_error())?;
        if sync {
            counted_sync(&f)?;
        }
        Ok(n)
    })();
    match r {
        Ok(n) => Ok((tmp, n)),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Replace (or create) `path` with `data` atomically and durably; see the module docs. The
/// folder must exist.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    write_atomic_with(path, &mut |w| w.write_all(data)).map(|_| ())
}

/// [`write_atomic`] with streamed content: `fill` writes it (an error aborts and leaves the old
/// file). Returns the length written.
pub fn write_atomic_with(path: &Path, fill: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>) -> io::Result<u64> {
    replace_with(path, true, fill)
}

/// Replace (or create) `path` with `data` atomically but **without syncing** it to disk — only for
/// outputs that can always be recreated (exports, renders, screenshots); see the module docs.
/// Anything that can't be made again goes through [`write_atomic`].
pub fn write_atomic_nosync(path: &Path, data: &[u8]) -> io::Result<()> {
    replace_with(path, false, &mut |w| w.write_all(data)).map(|_| ())
}

/// Temp file (synced when `sync`) renamed over `path`, then the folder synced when `sync`.
fn replace_with(path: &Path, sync: bool, fill: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>) -> io::Result<u64> {
    let real = replace_target(path)?;
    let path = real.as_path();
    let (tmp, n) = write_temp(path, sync, fill)?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if sync {
        sync_dir(parent_of(path));
    }
    Ok(n)
}

/// The file a replacement of `path` must land on. A symbolic link is written **through**: the
/// file it points to is replaced (next to it, so the rename stays on one volume) and the link
/// itself is kept, never clobbered with a regular file. A link that leads nowhere, or to
/// something that isn't a regular file, is refused.
fn replace_target(path: &Path) -> io::Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            let real = fs::canonicalize(path).map_err(|e| {
                io::Error::new(e.kind(), format!("{} is a symbolic link that leads nowhere usable ({e}); not replacing it", path.display()))
            })?;
            if !fs::metadata(&real).is_ok_and(|m| m.is_file()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is a symbolic link to {}, which is not a regular file; not replacing it", path.display(), real.display()),
                ));
            }
            Ok(real)
        }
        Ok(m) if m.is_dir() => Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is a folder, not a file", path.display()))),
        _ => Ok(path.to_path_buf()),
    }
}

/// Write `data` as the new file `path`: never replaces an existing file (`AlreadyExists`), and
/// the file only appears once it is complete, synced and read back identical.
pub fn write_new(path: &Path, data: &[u8]) -> io::Result<()> {
    write_new_unique(&mut std::iter::once(path.to_path_buf()), data).map(|_| ())
}

/// [`write_new`] under the first free name of `candidates` (all in the same folder, e.g.
/// `IMG_1.dng`, `IMG_1-2.dng`, …): the file is written once and then given the first name that
/// is not taken. Returns that name; `AlreadyExists` when every candidate is taken.
pub fn write_new_unique(candidates: &mut dyn Iterator<Item = PathBuf>, data: &[u8]) -> io::Result<PathBuf> {
    let mut candidates = candidates.peekable();
    let Some(first) = candidates.peek().cloned() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no file name to write"));
    };
    let (tmp, _) = write_temp(&first, true, &mut |w| w.write_all(data))?;
    let r = (|| {
        // Kept (issue #134 point 4): these files replace raws (Convert / Copy as DNG) or become
        // library photos (merges). On a local disk the read usually comes from the cache, but on a
        // network volume it can go back to the server, and next to encoding a DNG it costs little.
        if !same_content(&tmp, data)? {
            return Err(io::Error::other("the written file reads back different (failing drive or connection?)"));
        }
        for path in candidates.by_ref().take(100_000) {
            match publish_new(&tmp, &path) {
                Ok(()) => {
                    sync_dir(parent_of(&path));
                    return Ok(path);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("{}: no free file name", first.display())))
    })();
    // after a hard link the temp name is a second name of the new file; after a rename it's gone
    let _ = fs::remove_file(&tmp);
    r
}

/// Give the complete temp file `tmp` the name `path`, unless that is taken (`AlreadyExists`): a hard
/// link, or where the volume has none (FAT/exFAT, some shares) an exclusive placeholder renamed over.
/// After a link `tmp` is a second name of the file, after a rename it is gone.
pub fn publish_new(tmp: &Path, path: &Path) -> io::Result<()> {
    match fs::hard_link(tmp, path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
        // no hard links on this volume (FAT/exFAT, some shares)
        Err(_) => publish_new_by_rename(tmp, path, &|| {}),
    }
}

/// Fallback of [`publish_new`]: reserve the name with an exclusive create (atomic, so a file that
/// appears at the same time is never replaced), then rename the complete temp file over our own
/// empty placeholder. `before_claim` runs just before the name is claimed (a test hook).
fn publish_new_by_rename(tmp: &Path, path: &Path, before_claim: &dyn Fn()) -> io::Result<()> {
    before_claim();
    OpenOptions::new().write(true).create_new(true).open(path)?;
    if let Err(e) = fs::rename(tmp, path) {
        let _ = fs::remove_file(path);
        return Err(e);
    }
    Ok(())
}

/// Does the file at `path` hold exactly `data`?
fn same_content(path: &Path, data: &[u8]) -> io::Result<bool> {
    use std::io::Read;
    let mut f = File::open(path)?;
    if f.metadata()?.len() != data.len() as u64 {
        return Ok(false);
    }
    let mut buf = vec![0u8; 1 << 20];
    let mut at = 0usize;
    loop {
        let k = match f.read(&mut buf) {
            Ok(0) => return Ok(at == data.len()),
            Ok(k) => k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if data.get(at..at + k) != buf.get(..k) {
            return Ok(false);
        }
        at += k;
    }
}

/// The error for a name that is a link, or not a regular file (a FIFO, a device, a folder).
fn not_plain(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, format!("{} is a link or not a regular file: not opened", path.display()))
}

/// Windows: open a reparse point (a symbolic link or junction) itself rather than its target, so
/// a link swapped in after the check is opened as the link and refused
/// (`FILE_FLAG_OPEN_REPARSE_POINT`).
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// Open the existing regular file `path` with `opts`, never through a link at that name: a link,
/// a FIFO (whose open would block) or a device is refused (`InvalidInput`), and a missing file is
/// `NotFound`. Between the check and the open a link could be swapped in; the opened file is
/// checked to be the one looked at (same device and inode on Unix; on Windows the link itself is
/// opened, and refused).
pub fn open_existing(path: &Path, opts: &OpenOptions) -> io::Result<File> {
    let before = fs::symlink_metadata(path)?;
    if !before.file_type().is_file() {
        return Err(not_plain(path));
    }
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut opts = opts.clone();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        opts.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let f = opts.open(path)?;
    let after = f.metadata()?;
    #[cfg(unix)]
    let same = {
        use std::os::unix::fs::MetadataExt;
        after.dev() == before.dev() && after.ino() == before.ino()
    };
    #[cfg(not(unix))]
    let same = after.file_type().is_file();
    if !same {
        return Err(not_plain(path));
    }
    Ok(f)
}

/// [`open_existing`], or when nothing is at `path` a new file created exclusively (`create_new`),
/// so a link planted there meanwhile is refused rather than followed.
pub fn open_or_create(path: &Path, opts: &OpenOptions) -> io::Result<File> {
    match open_existing(path, opts) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let mut create = opts.clone();
            create.create_new(true);
            match create.open(path) {
                // created by someone else since: open that, by the same rules
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => open_existing(path, opts),
                r => r,
            }
        }
        r => r,
    }
}

/// The content of the regular file `path`, never read through a link ([`open_existing`]).
pub fn read_no_follow(path: &Path) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let mut b = Vec::new();
    open_existing(path, OpenOptions::new().read(true))?.read_to_end(&mut b)?;
    Ok(b)
}

/// [`read_no_follow`], at most `max` bytes: a file bigger than that, or one that grows past it
/// while it is read, is refused rather than read to its end.
pub fn read_no_follow_max(path: &Path, max: u64) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let f = open_existing(path, OpenOptions::new().read(true))?;
    let mut b = Vec::new();
    f.take(max.saturating_add(1)).read_to_end(&mut b)?;
    if b.len() as u64 > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{} is more than the {max} bytes allowed", path.display())));
    }
    Ok(b)
}

/// Do `a` and `b` name the same existing file (through links, `..`, case-insensitive names)?
pub fn same_file(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (fs::metadata(a), fs::metadata(b)) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ma.dev() == mb.dev() && ma.ino() == mb.ino()
    }
    #[cfg(not(unix))]
    {
        if ma.len() != mb.len() {
            return false;
        }
        // A hard link has its own path but shares the volume serial number and file index with
        // its original. A volume that reports file index 0 is compared by resolved path below.
        // On ReFS two files can share the 64-bit file index, so the lengths are compared first.
        #[cfg(windows)]
        {
            if file_ids(a, b).is_ok_and(|(ia, ib)| same_file_id(ia, ib)) {
                return true;
            }
        }
        match (fs::canonicalize(a), fs::canonicalize(b)) {
            (Ok(x), Ok(y)) => x == y || x.to_string_lossy().to_lowercase() == y.to_string_lossy().to_lowercase(),
            _ => false,
        }
    }
}

/// The volume serial number and file index of `a` and `b`, read while both files are open.
#[cfg(windows)]
fn file_ids(a: &Path, b: &Path) -> io::Result<((u64, u64), (u64, u64))> {
    use winapi_util::{Handle, file::information};
    let (ha, hb) = (Handle::from_path_any(a)?, Handle::from_path_any(b)?);
    let (ia, ib) = (information(&ha)?, information(&hb)?);
    Ok(((ia.volume_serial_number(), ia.file_index()), (ib.volume_serial_number(), ib.file_index())))
}

/// Whether two (volume serial number, file index) pairs name one file. A file index of 0 is not
/// an identity and matches nothing.
#[cfg(any(windows, test))]
fn same_file_id(a: (u64, u64), b: (u64, u64)) -> bool {
    a.1 != 0 && b.1 != 0 && a == b
}

/// Open the regular file `path` in place with `opts`, never through a link at that name: a library
/// folder may come from a share, a sync or an archive, and a link planted there would have a write
/// land in, or a read take, a file elsewhere (issue #2552). A link, a FIFO (whose open would block),
/// a device or a folder at the name is refused with a message saying what to do; nothing there is
/// `None`. `opts` must not create or truncate: [`open_or_create_regular`] makes a missing file.
///
/// On Unix the opened file is checked to be the one looked at (same device and inode) before
/// anything is read or written through it, so a link swapped in meanwhile is refused too.
pub fn open_regular(path: &Path, opts: &OpenOptions) -> io::Result<Option<File>> {
    let before = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !before.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is a link or not a regular file; the app won't read or write a catalog file through one. Remove it or put the real file there.",
                path.display()
            ),
        ));
    }
    let f = match opts.open(path) {
        Ok(f) => f,
        // removed since it was looked at
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let after = f.metadata()?;
        if (after.dev(), after.ino()) != (before.dev(), before.ino()) {
            return Err(io::Error::other(format!("{} changed while it was being opened; try again", path.display())));
        }
    }
    Ok(Some(f))
}

/// [`open_regular`], or, when nothing is at `path`, a new file made exclusively (`create_new`),
/// which neither follows a link planted there meanwhile nor creates its target. `true` when this
/// call made the file.
pub fn open_or_create_regular(path: &Path, opts: &OpenOptions) -> io::Result<(File, bool)> {
    if let Some(f) = open_regular(path, opts)? {
        return Ok((f, false));
    }
    match opts.clone().create_new(true).open(path) {
        Ok(f) => Ok((f, true)),
        // made by another program since: opened by the same rules
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => open_regular(path, opts)?.map(|f| (f, false)).ok_or(e),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_no_follow_max_refuses_what_is_over_the_cap_or_not_a_plain_file() {
        let d = temp("read-max");
        let p = d.join("a");
        fs::write(&p, b"abcd").unwrap();
        assert_eq!(read_no_follow_max(&p, 4).unwrap(), b"abcd");
        assert!(read_no_follow_max(&p, 3).is_err());
        assert!(read_no_follow_max(&d, 100).is_err(), "a folder");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/dev/zero", d.join("z")).unwrap();
            assert!(read_no_follow_max(&d.join("z"), 1 << 20).is_err(), "a link to an endless source");
        }
        let _ = fs::remove_dir_all(&d);
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-safe-file-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        v.sort();
        v
    }

    /// A sidecar or a DNG on an SMB share mounted by macOS (issue #1407), where the full flush
    /// fails with ENOTSUP: the file and its folder are flushed with a plain `fsync` instead, and
    /// any other failure is still one.
    #[test]
    fn durable_writes_fall_back_to_fsync_without_a_full_flush() {
        let d = temp("no-full-flush");
        let before = plain_fsyncs_on_this_thread();
        {
            let _smb = full_sync_unsupported();
            write_atomic(&d.join("DSC_0001.xmp"), b"<x/>").unwrap();
        }
        // (Windows can't open a folder to flush it: NTFS journals the rename, `sync_dir`)
        let expected = if cfg!(windows) { 1 } else { 2 };
        assert_eq!(plain_fsyncs_on_this_thread() - before, expected, "the file and its folder");
        assert_eq!(names(&d), vec!["DSC_0001.xmp"]);
        assert!(flush_unsupported(&enotsup()));
        assert!(!flush_unsupported(&io::Error::from(io::ErrorKind::StorageFull)));
        assert!(!flush_unsupported(&io::Error::from_raw_os_error(5)), "an I/O error is still one");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn fallback_publish_never_replaces_a_file_that_appears_at_the_same_time() {
        let d = temp("nolink-race");
        let tmp = d.join(".tmp-new");
        let dest = d.join("IMG_1.dng");
        fs::write(&tmp, b"new").unwrap();
        let e = publish_new_by_rename(&tmp, &dest, &|| fs::write(&dest, b"other writer").unwrap()).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&dest).unwrap(), b"other writer");
        fs::remove_file(&dest).unwrap();
        publish_new_by_rename(&tmp, &dest, &|| {}).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"new");
        assert!(!tmp.exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_goes_through_a_symlink_and_keeps_it() {
        let d = temp("symlink");
        let real_dir = d.join("real");
        fs::create_dir_all(&real_dir).unwrap();
        let real = real_dir.join("settings.json");
        fs::write(&real, b"old").unwrap();
        let link = d.join("settings.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write_atomic(&link, b"new").unwrap();
        write_atomic_nosync(&link, b"newer").unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link was replaced by a file");
        assert_eq!(fs::read(&real).unwrap(), b"newer");
        // no temp files left beside either name
        assert_eq!(names(&d), vec!["real", "settings.json"]);
        assert_eq!(names(&real_dir), vec!["settings.json"]);
        // a dangling link and a link to a folder are refused, and left as they were
        let dangling = d.join("dangling");
        std::os::unix::fs::symlink(d.join("missing"), &dangling).unwrap();
        assert!(write_atomic(&dangling, b"x").is_err());
        assert!(fs::symlink_metadata(&dangling).unwrap().file_type().is_symlink());
        assert!(!d.join("missing").exists());
        let to_dir = d.join("to-dir");
        std::os::unix::fs::symlink(&real_dir, &to_dir).unwrap();
        assert!(write_atomic(&to_dir, b"x").is_err());
        assert!(fs::symlink_metadata(&to_dir).unwrap().file_type().is_symlink());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let d = temp("replace");
        let p = d.join("out.jpg");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two two");
        assert_eq!(names(&d), vec!["out.jpg"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failure_mid_write_keeps_the_old_file() {
        let d = temp("midwrite");
        let p = d.join("photo.jpg");
        fs::write(&p, b"the original bytes").unwrap();
        {
            let _f = fail_writes_after(4);
            let e = write_atomic(&p, &[7u8; 10_000]).unwrap_err();
            assert!(e.to_string().contains("injected"), "{e}");
            assert!(write_new(&d.join("new.dng"), &[1u8; 100]).is_err());
        }
        assert_eq!(fs::read(&p).unwrap(), b"the original bytes", "the target is untouched");
        assert_eq!(names(&d), vec!["photo.jpg"], "no temp file and no partial new file");
        // a fill error aborts the same way
        let r = write_atomic_with(&p, &mut |w| {
            w.write_all(b"half")?;
            Err(io::Error::other("encoder failed"))
        });
        assert!(r.is_err());
        assert_eq!(fs::read(&p).unwrap(), b"the original bytes");
        assert_eq!(names(&d), vec!["photo.jpg"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn recreatable_outputs_are_atomic_without_a_sync() {
        let d = temp("nosync");
        let p = d.join("export.jpg");
        let before = syncs_on_this_thread();
        write_atomic_nosync(&p, b"one").unwrap();
        write_atomic_nosync(&p, b"two two").unwrap();
        assert_eq!(syncs_on_this_thread(), before, "no file or folder sync");
        assert_eq!(fs::read(&p).unwrap(), b"two two");
        assert_eq!(names(&d), vec!["export.jpg"], "no temp file left");
        // still atomic: a failure part-way leaves the previous file and no temp file
        {
            let _f = fail_writes_after(3);
            assert!(write_atomic_nosync(&p, &[9u8; 10_000]).is_err());
        }
        assert_eq!(fs::read(&p).unwrap(), b"two two");
        assert_eq!(names(&d), vec!["export.jpg"]);
        // the durable writers do sync (the file; the folder too where it can be opened)
        let before = syncs_on_this_thread();
        write_atomic(&p, b"three").unwrap();
        let mid = syncs_on_this_thread();
        assert!(mid > before);
        write_new(&d.join("new.dng"), b"dng").unwrap();
        assert!(syncs_on_this_thread() > mid);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn new_files_never_replace_anything() {
        let d = temp("new");
        let p = d.join("a.dng");
        fs::write(&p, b"keep").unwrap();
        assert_eq!(write_new(&p, b"other").unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&p).unwrap(), b"keep");
        let mut c = (1..).map(|k| if k == 1 { d.join("a.dng") } else { d.join(format!("a-{k}.dng")) });
        let got = write_new_unique(&mut c, b"fresh").unwrap();
        assert_eq!(got, d.join("a-2.dng"));
        assert_eq!(fs::read(&got).unwrap(), b"fresh");
        assert_eq!(names(&d), vec!["a-2.dng", "a.dng"], "the temp file is gone");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn same_file_sees_a_hard_link_under_another_name() {
        let d = temp("same-link");
        let p = d.join("IMG_1.jpg");
        fs::write(&p, b"original").unwrap();
        let linked = d.join("linked.png");
        match fs::hard_link(&p, &linked) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => panic!("{}: {e}", linked.display()),
            // no hard links on this volume (FAT/exFAT, some shares), as in `publish_new`
            Err(e) => {
                let _ = fs::remove_dir_all(&d);
                lcc_testkit::skip!(System, "no hard links in {}: {e}", d.display());
                return;
            }
        }
        fs::write(d.join("copy.png"), b"original").unwrap();
        assert!(same_file(&p, &linked), "a hard link is the same file");
        assert!(same_file(&linked, &p));
        assert!(!same_file(&p, &d.join("copy.png")), "a copy with the same content and length is another file");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn same_file_sees_through_other_spellings() {
        let d = temp("same");
        let p = d.join("x.jpg");
        fs::write(&p, b"x").unwrap();
        fs::write(d.join("y.jpg"), b"x").unwrap();
        fs::create_dir_all(d.join("sub")).unwrap();
        assert!(same_file(&p, &d.join("sub/../x.jpg")));
        assert!(!same_file(&p, &d.join("y.jpg")));
        assert!(!same_file(&p, &d.join("missing.jpg")));
        let _ = fs::remove_dir_all(&d);
    }

    /// What can't be a link (Windows test runs can't make one): a folder at the name is refused, a
    /// missing file is `NotFound`, and `open_or_create` makes the file once and then opens it.
    #[test]
    fn opening_in_place_takes_regular_files_only() {
        let d = temp("in-place");
        let f = d.join("catalog.log");
        assert_eq!(read_no_follow(&f).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(open_existing(&f, OpenOptions::new().append(true)).unwrap_err().kind(), io::ErrorKind::NotFound);
        open_or_create(&f, OpenOptions::new().append(true)).unwrap().write_all(b"a").unwrap();
        open_or_create(&f, OpenOptions::new().append(true)).unwrap().write_all(b"b").unwrap();
        assert_eq!(read_no_follow(&f).unwrap(), b"ab");
        fs::create_dir(d.join("sub")).unwrap();
        for r in [read_no_follow(&d.join("sub")).map(|_| ()), open_or_create(&d.join("sub"), OpenOptions::new().append(true)).map(|_| ())] {
            assert_eq!(r.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        }
        let _ = fs::remove_dir_all(&d);
    }

    /// Links planted in a library folder are never written, cut or read through (issue #2122):
    /// the lock owner note, the lock itself, `catalog.log` (appended to and cut back by the
    /// journal) and `prefs.json` (read, and copied aside when damaged). Each target keeps its bytes.
    #[cfg(unix)]
    #[test]
    fn links_in_a_library_folder_are_never_followed() {
        use crate::store::{FsStore, Store};
        use std::os::unix::fs::symlink;
        let d = temp("library-links");
        let outside = temp("library-links-outside");
        let victim = |name: &str| {
            let v = outside.join(name);
            fs::write(
                &v, b"keep
",
            )
            .unwrap();
            v
        };
        let owner = victim("owner");
        symlink(&owner, d.join(crate::lock::OWNER)).unwrap();
        {
            let lock = crate::lock::LibraryLock::acquire(&d, "test").unwrap();
            assert!(lock.held());
            assert!(!fs::symlink_metadata(d.join(crate::lock::OWNER)).unwrap().file_type().is_symlink(), "the link was replaced by the note");
        }
        assert_eq!(
            fs::read(&owner).unwrap(),
            b"keep
"
        );

        // a lock file linked elsewhere is not created or locked there: the library opens unprotected
        let d2 = temp("library-links-lock");
        let missing = outside.join("created-by-lock");
        symlink(&missing, d2.join(crate::lock::LOCK)).unwrap();
        assert!(!crate::lock::LibraryLock::acquire(&d2, "test").unwrap().held());
        assert!(!missing.exists(), "nothing was created through the link");

        let log = victim("log");
        symlink(&log, d.join("catalog.log")).unwrap();
        let prefs = victim("prefs");
        symlink(&prefs, d.join("prefs.json")).unwrap();
        let mut store = FsStore::open(&d).unwrap();
        assert!(store.truncate("catalog.log", 0).is_err());
        assert!(
            store
                .append(
                    "catalog.log",
                    b"[1]
"
                )
                .is_err()
        );
        assert!(store.read("catalog.log").is_err());
        assert!(store.read("prefs.json").is_err(), "refused: a damaged prefs.json is copied aside");
        assert_eq!(
            (fs::read(&log).unwrap(), fs::read(&prefs).unwrap()),
            (
                b"keep
"
                .to_vec(),
                b"keep
"
                .to_vec()
            )
        );
        // the journal refuses the library rather than cutting the log's target back
        assert!(crate::journal::Journal::open(Box::new(FsStore::open(&d).unwrap())).is_err());
        assert_eq!(
            fs::read(&log).unwrap(),
            b"keep
"
        );
        for dir in [&d, &d2, &outside] {
            let _ = fs::remove_dir_all(dir);
        }
    }

    /// Issue #2552: only a regular file is opened in place. A link (even to a regular file), a
    /// folder, a FIFO or a device at the name is refused without being opened, and a missing file
    /// is `None` until `open_or_create_regular` makes it.
    #[cfg(unix)]
    #[test]
    fn only_regular_files_are_opened_in_place() {
        let d = temp("regular");
        let p = d.join("catalog.log");
        assert!(open_regular(&p, OpenOptions::new().read(true)).unwrap().is_none());
        assert!(open_or_create_regular(&p, OpenOptions::new().append(true)).unwrap().1, "a missing file is made");
        assert!(!open_or_create_regular(&p, OpenOptions::new().append(true)).unwrap().1, "an existing one is opened");
        assert!(open_regular(&p, OpenOptions::new().read(true)).unwrap().is_some());
        std::os::unix::fs::symlink(&p, d.join("link.log")).unwrap();
        fs::create_dir_all(d.join("folder.log")).unwrap();
        let fifo = d.join("fifo.log");
        assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        for refused in [d.join("link.log"), d.join("folder.log"), fifo, PathBuf::from("/dev/null")] {
            let e = open_regular(&refused, OpenOptions::new().read(true)).unwrap_err();
            assert!(e.to_string().contains("is a link or not a regular file"), "{}: {e}", refused.display());
            assert!(open_or_create_regular(&refused, OpenOptions::new().append(true)).is_err(), "{}", refused.display());
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_zero_file_index_is_not_a_file_identity() {
        assert!(same_file_id((7, 42), (7, 42)));
        assert!(!same_file_id((7, 42), (7, 43)));
        assert!(!same_file_id((7, 42), (8, 42)));
        assert!(!same_file_id((7, 0), (7, 0)), "two files on a volume that reports index 0 are not the same file");
        assert!(!same_file_id((0, 0), (0, 0)));
    }
}
