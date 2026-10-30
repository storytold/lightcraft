use super::sidecar::PacketVersion;
use super::{HashMap, HashSet, Path, PathBuf, SidecarNaming};

/// Every sidecar that may belong to `path` (both naming conventions, `.xmp` and `.XMP`).
pub(super) fn sidecars(path: &str) -> Vec<(PathBuf, SidecarNaming, bool)> {
    let mut v = Vec::new();
    for naming in [SidecarNaming::Stem, SidecarNaming::Full] {
        let p = crate::sidecar::sidecar_path(path, naming);
        v.push((p.with_extension("XMP"), naming, true));
        v.push((p, naming, false));
    }
    v
}

/// The file-system primitives renaming uses. [`RealFs`] is the disk; tests substitute a model of
/// a case-insensitive or case-sensitive volume (and inject failures).
pub(crate) trait MoveFs {
    /// Something (file, folder, link) answers to `p`.
    fn exists(&self, p: &Path) -> bool;
    fn is_file(&self, p: &Path) -> bool;
    /// `a` and `b` name the same file (e.g. `IMG.JPG` and `img.jpg` on a case-insensitive
    /// volume). Decided by file identity, never by comparing names: on a case-sensitive volume
    /// they are two different photos. When in doubt: `false`.
    fn same_file(&self, a: &Path, b: &Path) -> bool;
    /// Rename `a` to `b`, failing (never replacing) when `b` exists.
    fn rename_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()>;
    /// Copy `a` to a new file `b` (an independent copy, never a link), failing when `b` exists.
    fn copy_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()>;
    fn remove_file(&self, p: &Path) -> std::io::Result<()>;
    /// Another photo file beside the stem-named sidecar `sidecar` that shares its stem (e.g.
    /// `IMG_1.JPG` for `IMG_1.xmp`), which still uses that sidecar.
    fn stem_sibling(&self, sidecar: &Path) -> Option<String>;
    /// The folder lists an entry spelled exactly like `p` (letter case included). On a
    /// case-insensitive volume `a.XMP` "exists" when only `a.xmp` is there; this tells the two
    /// apart. When the folder can't be listed: `true`, so nothing is skipped.
    fn listed(&self, p: &Path) -> bool {
        let (Some(dir), Some(name)) = (p.parent(), p.file_name()) else { return true };
        let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
        match std::fs::read_dir(dir) {
            Ok(entries) => entries.flatten().any(|e| e.file_name() == name),
            Err(_) => true,
        }
    }
    /// The names in folder `dir`, or `None` when it can't be listed or holds more than `budget`
    /// entries (it stops reading at that many, so a huge folder is never collected).
    fn list_dir(&self, dir: &Path, budget: usize) -> Option<Vec<std::ffi::OsString>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(dir).ok()? {
            if names.len() == budget {
                return None;
            }
            names.push(entry.ok()?.file_name());
        }
        Some(names)
    }
    /// Content observed in a packet, independent of its pathname or inode. Model volumes
    /// override this; disk implementations stream a bounded buffer without retaining the XML.
    fn packet_version(&self, path: &Path) -> std::io::Result<PacketVersion> {
        use std::io::Read;
        let mut file = std::fs::File::open(path)?;
        let mut hash = lightcraft_preview::Hasher128::new();
        let mut len = 0u64;
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let n = match file.read(&mut buffer) {
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if n == 0 {
                return Ok(PacketVersion { len, hash: hash.finish() });
            }
            let bytes = buffer.get(..n).ok_or_else(|| std::io::Error::other("invalid sidecar read length"))?;
            len = len.checked_add(n as u64).ok_or_else(|| std::io::Error::other("sidecar is too large"))?;
            hash.update(bytes);
        }
    }
}

/// The real file system.
pub(crate) struct RealFs;

impl MoveFs for RealFs {
    fn exists(&self, p: &Path) -> bool {
        std::fs::symlink_metadata(p).is_ok()
    }

    fn is_file(&self, p: &Path) -> bool {
        p.is_file()
    }

    fn same_file(&self, a: &Path, b: &Path) -> bool {
        if a == b {
            return true;
        }
        let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else { return false };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ma.dev() == mb.dev() && ma.ino() == mb.ino()
        }
        #[cfg(not(unix))]
        {
            // no stable file id in std here: `a` and `b` are one file under two spellings when
            // they look alike and the folder doesn't list both (on a case-sensitive folder
            // holding both `IMG.JPG` and `img.jpg`, both entries are listed). Either may be the
            // listed one: the answer must not depend on the argument order.
            let (Some(dir), Some(name_a), Some(name_b)) = (b.parent(), a.file_name(), b.file_name()) else { return false };
            let respelled = name_a.to_string_lossy().to_lowercase() == name_b.to_string_lossy().to_lowercase();
            if !respelled || a.parent() != Some(dir) || ma.len() != mb.len() || ma.modified().ok() != mb.modified().ok() {
                return false;
            }
            let Ok(entries) = std::fs::read_dir(dir) else { return false };
            let (mut listed_a, mut listed_b) = (false, false);
            for e in entries.flatten() {
                let name = e.file_name();
                listed_a |= name == name_a;
                listed_b |= name == name_b;
            }
            !(listed_a && listed_b)
        }
    }

    fn rename_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        if self.exists(b) {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} already exists", b.display())));
        }
        // A hard link fails when `b` exists — even if it appeared since the check above — so
        // nothing is ever replaced. Volumes without hard links (FAT, some network shares) fall back
        // to a plain rename after the check.
        match std::fs::hard_link(a, b) {
            Ok(()) => std::fs::remove_file(a).inspect_err(|_| {
                // both names are the same file: dropping the new one loses nothing
                let _ = std::fs::remove_file(b);
            }),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(e),
            Err(_) => std::fs::rename(a, b)
                .or_else(|e| if e.kind() == std::io::ErrorKind::CrossesDevices { move_across_volumes(self, a, b) } else { Err(e) }),
        }
    }

    fn copy_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        if self.exists(b) {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} already exists", b.display())));
        }
        // Streamed (a large raw or video is never read into memory whole), into a hidden name
        // beside `b` that only takes `b`'s name once complete: a share that drops mid-copy, so
        // the partial file can't be removed either, leaves no truncated file under the photo's
        // name for a retry to take for the photo.
        let name = b.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        // a free name: one left by a copy the share cut off (it couldn't be removed then) doesn't
        // block the next
        let partial = (0..1000u32)
            .map(|k| b.with_file_name(format!(".{name}.lc-partial-{}-{k}", std::process::id())))
            .find(|p| !self.exists(p))
            .ok_or_else(|| std::io::Error::other(format!("no free temporary name beside {}", b.display())))?;
        let copied = (|| {
            let mut from = std::fs::File::open(a)?;
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&partial)?;
            std::io::copy(&mut from, &mut f)?;
            f.sync_all()
        })();
        let placed = copied.and_then(|()| self.rename_no_replace(&partial, b));
        if placed.is_err() {
            let _ = std::fs::remove_file(&partial);
        }
        placed
    }

    fn remove_file(&self, p: &Path) -> std::io::Result<()> {
        std::fs::remove_file(p)
    }

    fn stem_sibling(&self, sidecar: &Path) -> Option<String> {
        crate::import_move::sibling_photo(sidecar)
    }
}

/// A folder with this many photos leaving it is listed once, and its photos' sidecars are looked
/// for in that listing: asking the disk for four names per photo costs a round trip each on a
/// network share, found or not. Fewer photos are probed one by one.
pub(super) const LIST_FOLDER_AT: usize = 8;

/// Directory entries the folder listings of one move may collect in all: the walk's own bound
/// ([`crate::walk::MAX_ENTRIES`]). A folder past what is left of it isn't listed, and its photos
/// are probed one by one as before.
const LIST_ENTRIES_AT_MOST: usize = crate::walk::MAX_ENTRIES;

/// `inner`, with the question "is this sidecar a file?" answered from one listing of each folder
/// that many photos leave ([`LIST_FOLDER_AT`]) where the listing can vouch for a "no": the folder was
/// listed whole, and its names are all ASCII (a file system may treat other names alike in other
/// cases), and the name is a plain one ([`crate::sync::plain_name`], no device names). A name the
/// listing holds (in any case), a folder not listed or not listed whole, and every other question
/// go to `inner` as before. A sidecar a move or copy brings into a listed folder is added to its names.
pub(super) struct ListedSources<'a> {
    inner: &'a dyn MoveFs,
    /// folder → the `.xmp` names in it, lower-cased.
    pub(super) folders: std::cell::RefCell<HashMap<PathBuf, HashSet<String>>>,
}

impl<'a> ListedSources<'a> {
    pub(super) fn new(inner: &'a dyn MoveFs, moves: &[(String, String)]) -> Self {
        Self::with_budget(inner, moves, LIST_ENTRIES_AT_MOST)
    }

    /// As [`Self::new`], with at most `budget` directory entries listed over all folders.
    pub(super) fn with_budget(inner: &'a dyn MoveFs, moves: &[(String, String)], mut budget: usize) -> Self {
        let mut leaving: HashMap<&Path, usize> = HashMap::new();
        for (from, _) in moves {
            if let Some(dir) = Path::new(from).parent().filter(|d| !d.as_os_str().is_empty()) {
                *leaving.entry(dir).or_default() += 1;
            }
        }
        let mut folders = HashMap::new();
        for (dir, n) in leaving {
            if n < LIST_FOLDER_AT {
                continue;
            }
            let Some(names) = inner.list_dir(dir, budget) else { continue };
            // (an override that lists more than the budget it was given breaks the contract: not trusted)
            let Some(left) = budget.checked_sub(names.len()) else { continue };
            budget = left;
            // (any name that is no ASCII text: the listing can't say what another name's other case is)
            let xmp: Option<HashSet<String>> = names
                .iter()
                .map(|n| n.to_str().filter(|n| n.is_ascii()).map(str::to_ascii_lowercase))
                .collect::<Option<Vec<_>>>()
                .map(|v| v.into_iter().filter(|n| n.ends_with(".xmp")).collect());
            if let Some(xmp) = xmp {
                folders.insert(dir.to_path_buf(), xmp);
            }
        }
        ListedSources { inner, folders: std::cell::RefCell::new(folders) }
    }

    /// Whether the listing shows `p`, a sidecar name, is not there.
    fn surely_absent(&self, p: &Path) -> bool {
        let folders = self.folders.borrow();
        let (Some(xmp), Some(name)) = (p.parent().and_then(|d| folders.get(d)), p.file_name().and_then(|n| n.to_str())) else { return false };
        p.to_str().is_some_and(|s| s.ends_with(name))
            && name.len() > ".xmp".len()
            && name.to_ascii_lowercase().ends_with(".xmp")
            && crate::sync::plain_name(name)
            && !crate::sync::asks_the_disk(name)
            && !xmp.contains(&name.to_ascii_lowercase())
    }

    /// A file arrived at `p`: a `.xmp` joins its folder's names (one that leaves changes nothing:
    /// a name listed that is gone is asked about, and found missing, as before).
    fn arrived(&self, p: &Path) {
        let Some(dir) = p.parent() else { return };
        let mut folders = self.folders.borrow_mut();
        let Some(xmp) = folders.get_mut(dir) else { return };
        match p.file_name().and_then(|n| n.to_str()).filter(|n| n.is_ascii()) {
            Some(name) if name.to_ascii_lowercase().ends_with(".xmp") => {
                xmp.insert(name.to_ascii_lowercase());
            }
            Some(_) => {}
            None => {
                folders.remove(dir);
            }
        }
    }
}

impl MoveFs for ListedSources<'_> {
    fn exists(&self, p: &Path) -> bool {
        self.inner.exists(p)
    }
    fn is_file(&self, p: &Path) -> bool {
        !self.surely_absent(p) && self.inner.is_file(p)
    }
    fn same_file(&self, a: &Path, b: &Path) -> bool {
        self.inner.same_file(a, b)
    }
    fn rename_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        let moved = self.inner.rename_no_replace(a, b);
        if moved.is_ok() {
            self.arrived(b);
        }
        moved
    }
    fn copy_no_replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        let copied = self.inner.copy_no_replace(a, b);
        if copied.is_ok() {
            self.arrived(b);
        }
        copied
    }
    fn remove_file(&self, p: &Path) -> std::io::Result<()> {
        self.inner.remove_file(p)
    }
    fn stem_sibling(&self, sidecar: &Path) -> Option<String> {
        self.inner.stem_sibling(sidecar)
    }
    fn listed(&self, p: &Path) -> bool {
        self.inner.listed(p)
    }
    fn list_dir(&self, dir: &Path, budget: usize) -> Option<Vec<std::ffi::OsString>> {
        self.inner.list_dir(dir, budget)
    }
    fn packet_version(&self, path: &Path) -> std::io::Result<PacketVersion> {
        self.inner.packet_version(path)
    }
}

/// Move `a` to `b` on another volume: copy (never replacing), compare the copy with the original
/// byte for byte, and only then remove the original. A copy that doesn't match is removed.
fn move_across_volumes(fs: &RealFs, a: &Path, b: &Path) -> std::io::Result<()> {
    fs.copy_no_replace(a, b)?;
    if !same_contents(a, b).unwrap_or(false) {
        let _ = std::fs::remove_file(b);
        return Err(std::io::Error::other(format!("the copy at {} did not match the original; the original was kept", b.display())));
    }
    std::fs::remove_file(a).inspect_err(|_| {
        // the original stays: drop the copy so the file isn't in two places
        let _ = std::fs::remove_file(b);
    })
}

/// Whether files `a` and `b` hold the same bytes: lengths first, then 1 MiB chunks at a time, so
/// a large raw or video is never read into memory whole.
pub(super) fn same_contents(a: &Path, b: &Path) -> std::io::Result<bool> {
    const CHUNK: usize = 1 << 20;
    let (mut fa, mut fb) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let (mut ba, mut bb) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    loop {
        let n = read_full(&mut fa, &mut ba)?;
        let m = read_full(&mut fb, &mut bb)?;
        if n != m || ba.get(..n) != bb.get(..m) {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

/// Fill `buf` from `r` as far as the file goes; the number of bytes read (less than the buffer
/// only at the end of the file).
fn read_full(r: &mut impl std::io::Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while let Some(rest) = buf.get_mut(filled..).filter(|r| !r.is_empty()) {
        match r.read(rest) {
            Ok(0) => break,
            Ok(k) => filled = filled.saturating_add(k),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// `from` → `to` differs only in letter case and `to` is that very file (a case-insensitive
/// volume): the rename must go through a temporary name.
pub(super) fn is_respelling(fs: &dyn MoveFs, from: &Path, to: &Path) -> bool {
    from != to && folded_path(&from.to_string_lossy()) == folded_path(&to.to_string_lossy()) && fs.exists(to) && fs.same_file(from, to)
}

/// Windows accepts both separators; spelling differences must not hide file identity.
pub(super) fn folded_path(path: &str) -> String {
    if cfg!(windows) { path.replace('\\', "/").to_lowercase() } else { path.to_lowercase() }
}

/// Rename `a` to `b` without ever replacing a file; a change of letter case only goes through a
/// unique temporary name so case-insensitive volumes see a real change.
pub(super) fn move_one(fs: &dyn MoveFs, a: &Path, b: &Path) -> std::io::Result<()> {
    if !is_respelling(fs, a, b) {
        return fs.rename_no_replace(a, b);
    }
    let tmp = (0..1000u32)
        .map(|k| a.with_file_name(format!(".lc-rename-{}-{k}", std::process::id())))
        .find(|t| !fs.exists(t))
        .ok_or_else(|| std::io::Error::other("no free temporary name"))?;
    fs.rename_no_replace(a, &tmp)?;
    fs.rename_no_replace(&tmp, b).map_err(|e| match fs.rename_no_replace(&tmp, a) {
        Ok(()) => e,
        Err(e2) => std::io::Error::other(format!("{e}; it could not be given its old name back either ({e2}): it is now {}", tmp.display())),
    })
}
