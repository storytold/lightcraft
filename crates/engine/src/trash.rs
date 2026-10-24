//! The system Trash: where `photo.trashFiles` moves a photo's file, so that taking a photo out
//! of the library for good can take its file along without destroying it. Nothing else in
//! LightCraft removes a photo's file.
//!
//! [`Bin`] is the Trash as the engine uses it; [`SystemBin`] is the real one:
//! - macOS: the Finder's Trash (`NSFileManager`), which says where the file went, so undo can
//!   put it back.
//! - Linux and the other Unixes: the freedesktop.org Trash ([`xdg`]), likewise.
//! - Windows and the web: not yet. Asking is refused, and no file is touched.
//!
//! A disk without a Trash (many network shares) answers [`BinError::NoTrash`]: the file stays,
//! and only an explicit second question may erase it ([`Bin::erase`]).

use std::path::{Path, PathBuf};

/// Whether this build has a Trash to move files to (not on Windows or in the browser yet).
pub fn available() -> bool {
    cfg!(all(unix, not(target_arch = "wasm32")))
}

/// How many photos one `photo.trashFiles` call may name.
pub const MAX_PHOTOS: usize = 1_000_000;

/// Why `n` photos are too many for one `photo.trashFiles` call, if they are: said before the
/// person is asked, and again by the command itself.
pub fn too_many(n: usize) -> Option<String> {
    (n > MAX_PHOTOS).then(|| format!("more than {MAX_PHOTOS} photos in one call"))
}

/// Why a file did not go to the Trash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BinError {
    /// The disk the file is on has no Trash: it can only be erased, which is asked separately.
    NoTrash,
    /// Anything else, as a sentence for the person (the file is where it was).
    Failed(String),
}

/// Whether a folder's disk has a Trash, as far as the system says before anything is moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrashHere {
    Yes,
    /// No Trash: its files can only be erased. A hint, confirmed by one real attempt per folder
    /// before anything there is erased.
    No,
    /// Not known: each file is tried.
    Unknown,
}

/// A Trash. Implementations move, never copy-and-forget: after `trash` returned `Ok` the file is
/// in the Trash and not at `path`; after an error it is at `path`.
pub trait Bin: Send + Sync {
    /// Move the file at `path` to the Trash. `Ok(Some(at))`: where it is now, when the Trash
    /// tells (then [`Bin::put_back`] can undo it); `Ok(None)`: in the Trash, somewhere.
    fn trash(&self, path: &Path) -> Result<Option<PathBuf>, BinError>;

    /// Move a trashed file from `at` back to `to`, never replacing what is at `to` now.
    fn put_back(&self, at: &Path, to: &Path) -> std::io::Result<()> {
        use crate::rename::MoveFs;
        if let Some(dir) = to.parent() {
            // the folder may have been removed since, when it was left empty
            std::fs::create_dir_all(dir)?;
        }
        crate::rename::RealFs.rename_no_replace(at, to)
    }

    /// Erase the file at `path` for good: only for a file on a disk without a Trash, after the
    /// person was asked about exactly that.
    fn erase(&self, path: &Path) -> std::io::Result<()> {
        std::fs::remove_file(path)
    }

    /// Whether this Trash can take files at all here (the system's can't on Windows or in the
    /// browser yet: asking is then refused before any photo leaves the library).
    fn works_here(&self) -> bool {
        true
    }

    /// Whether the disk folder `folder` is on has a Trash, asked before any file there moves (so
    /// the person can be asked about erasing first). [`TrashHere::No`] is a hint: before
    /// anything in that folder is erased, one file is still offered to [`Bin::trash`].
    /// [`TrashHere::Unknown`]: try each file.
    fn trash_here(&self, _folder: &Path) -> TrashHere {
        TrashHere::Unknown
    }
}

/// The system's Trash.
pub struct SystemBin;

impl Bin for SystemBin {
    fn trash(&self, path: &Path) -> Result<Option<PathBuf>, BinError> {
        system_trash(path)
    }

    fn works_here(&self) -> bool {
        available()
    }

    fn trash_here(&self, folder: &Path) -> TrashHere {
        system_trash_here(folder)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    fn put_back(&self, at: &Path, to: &Path) -> std::io::Result<()> {
        use crate::rename::MoveFs;
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir)?;
        }
        crate::rename::RealFs.rename_no_replace(at, to)?;
        // the Trash's note about it goes with it
        if let Some(info) = xdg::info_of(at) {
            let _ = std::fs::remove_file(info);
        }
        Ok(())
    }
}

/// A file a step moved out of its place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trashed {
    /// Where it was.
    pub from: String,
    pub now: Whereabouts,
}

/// What became of a file `photo.trashFiles` took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Whereabouts {
    /// In the Trash, at this path: undo moves it back.
    At(String),
    /// In the Trash, the system did not say where: the person puts it back from there.
    InTrash,
    /// Erased (a disk without a Trash): it is not coming back.
    Erased,
    /// At its place: a redo could not move it to the Trash again. The next redo tries again.
    InPlace,
}

/// The file work of an undo or redo step of `photo.trashFiles`, done with its catalog op.
#[derive(Clone, Debug)]
// (`photo.trashFiles`, the next pull request, is its caller; the tests use it until then)
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum TrashStep {
    /// Undo: put these files back where they were.
    PutBack(Vec<Trashed>),
    /// Redo: move `back` (the files undo put back) to the Trash again. `still_out` did not come
    /// back at that undo (still in the Trash, or erased): the next undo tries them again.
    Again { back: Vec<String>, still_out: Vec<Trashed> },
}

/// "1 file" / "3 files".
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn files(n: usize) -> String {
    if n == 1 { "1 file".into() } else { format!("{n} files") }
}

#[cfg_attr(not(test), allow(dead_code))]
impl TrashStep {
    /// Do the step's file work with `bin`. Returns the step that reverses it and, when not
    /// everything could be done, a sentence saying what was not. It never fails as a whole: the
    /// catalog step goes ahead, and a photo whose file did not come back shows as missing.
    pub(crate) fn run(&self, bin: &dyn Bin) -> (TrashStep, Option<String>) {
        match self {
            TrashStep::PutBack(list) => {
                let (mut back, mut still_out) = (Vec::new(), Vec::new());
                let (mut failed, mut elsewhere, mut erased) = (0usize, 0usize, 0usize);
                let mut why = None;
                for t in list {
                    match &t.now {
                        Whereabouts::At(at) => match bin.put_back(Path::new(at), Path::new(&t.from)) {
                            Ok(()) => {
                                back.push(t.from.clone());
                                continue;
                            }
                            Err(e) => {
                                failed += 1;
                                why.get_or_insert(e.to_string());
                            }
                        },
                        Whereabouts::InTrash => elsewhere += 1,
                        Whereabouts::Erased => erased += 1,
                        // it never left: nothing to put back, and the next redo takes it again
                        Whereabouts::InPlace => {
                            back.push(t.from.clone());
                            continue;
                        }
                    }
                    still_out.push(t.clone());
                }
                let mut notes = Vec::new();
                if failed > 0 {
                    notes.push(format!("{} could not be put back from the Trash ({})", files(failed), why.unwrap_or_default()));
                }
                if elsewhere > 0 {
                    let (are, them) = if elsewhere == 1 { ("1 file is".to_string(), "it") } else { (format!("{elsewhere} files are"), "them") };
                    notes.push(format!("{are} still in the Trash: put {them} back from there"));
                }
                if erased > 0 {
                    let (were, are) = if erased == 1 { ("1 file was".to_string(), "is") } else { (format!("{erased} files were"), "are") };
                    notes.push(format!("{were} erased and {are} not coming back"));
                }
                (TrashStep::Again { back, still_out }, (!notes.is_empty()).then(|| notes.join("; ")))
            }
            TrashStep::Again { back, still_out } => {
                let (mut list, mut failed) = (still_out.clone(), 0usize);
                for from in back {
                    match bin.trash(Path::new(from)) {
                        Ok(at) => list.push(Trashed {
                            from: from.clone(),
                            now: at.map_or(Whereabouts::InTrash, |p| Whereabouts::At(p.to_string_lossy().to_string())),
                        }),
                        // (never erased without being asked: the file stays where it is, and
                        // is not lost sight of)
                        Err(_) => {
                            failed += 1;
                            list.push(Trashed { from: from.clone(), now: Whereabouts::InPlace });
                        }
                    }
                }
                let note = (failed > 0).then(|| {
                    let (stay, it) = if failed == 1 { ("stays", "it was") } else { ("stay", "they were") };
                    format!("{} could not be moved to the Trash and {stay} where {it}", files(failed))
                });
                (TrashStep::PutBack(list), note)
            }
        }
    }
}

/// `NSFeatureUnsupportedError` (in `NSCocoaErrorDomain`): what a volume without a Trash answers.
#[cfg(target_os = "macos")]
const FEATURE_UNSUPPORTED: isize = 3328;

#[cfg(target_os = "macos")]
fn system_trash(path: &Path) -> Result<Option<PathBuf>, BinError> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};
    let Some(text) = path.to_str() else { return Err(BinError::Failed(format!("{}: not a name the Trash can take", path.display()))) };
    // (a pool of its own: this runs on worker threads, which have none)
    objc2::rc::autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(text));
        let mut at = None;
        match NSFileManager::defaultManager().trashItemAtURL_resultingItemURL_error(&url, Some(&mut at)) {
            Ok(()) => Ok(at.and_then(|u| u.path()).map(|p| PathBuf::from(p.to_string()))),
            // (only that one answer means "no Trash": anything else leaves the file not erasable)
            Err(e) if e.code() == FEATURE_UNSUPPORTED && e.domain().to_string() == "NSCocoaErrorDomain" => Err(BinError::NoTrash),
            Err(e) => Err(BinError::Failed(e.localizedDescription().to_string())),
        }
    })
}

/// The Finder's Trash for a file in `folder`, without making one: a volume without a Trash
/// answers what `trashItemAtURL` does there (measured on SMB shares, 2026-10-20).
#[cfg(target_os = "macos")]
fn system_trash_here(folder: &Path) -> TrashHere {
    use objc2_foundation::{NSFileManager, NSSearchPathDirectory, NSSearchPathDomainMask, NSString, NSURL};
    let Some(text) = folder.to_str() else { return TrashHere::Unknown };
    objc2::rc::autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(text));
        match NSFileManager::defaultManager().URLForDirectory_inDomain_appropriateForURL_create_error(
            NSSearchPathDirectory::TrashDirectory,
            NSSearchPathDomainMask::UserDomainMask,
            Some(&url),
            false,
        ) {
            Ok(_) => TrashHere::Yes,
            Err(e) if e.code() == FEATURE_UNSUPPORTED && e.domain().to_string() == "NSCocoaErrorDomain" => TrashHere::No,
            Err(_) => TrashHere::Unknown,
        }
    })
}

#[cfg(all(unix, not(target_os = "macos"), not(target_arch = "wasm32")))]
fn system_trash(path: &Path) -> Result<Option<PathBuf>, BinError> {
    let Some(places) = xdg::Places::of_this_user() else {
        return Err(BinError::Failed("there is no home folder to keep a Trash in (HOME is not set): nothing was changed".into()));
    };
    xdg::trash(path, &places).map(Some)
}

#[cfg(all(unix, not(target_os = "macos"), not(target_arch = "wasm32")))]
fn system_trash_here(folder: &Path) -> TrashHere {
    match xdg::Places::of_this_user() {
        Some(places) => xdg::trash_here(folder, &places),
        None => TrashHere::Unknown,
    }
}

#[cfg(any(not(unix), target_arch = "wasm32"))]
fn system_trash_here(_folder: &Path) -> TrashHere {
    TrashHere::Unknown
}

#[cfg(not(any(unix, target_arch = "wasm32")))]
fn system_trash(_path: &Path) -> Result<Option<PathBuf>, BinError> {
    Err(BinError::Failed("moving files to the Recycle Bin is not available on Windows yet: nothing was changed".into()))
}

#[cfg(target_arch = "wasm32")]
fn system_trash(_path: &Path) -> Result<Option<PathBuf>, BinError> {
    Err(BinError::Failed("there is no Trash in the browser: nothing was changed".into()))
}

/// The freedesktop.org Trash (specification 1.0), in plain file operations: a file goes to
/// `<trash>/files/<name>` beside a `<trash>/info/<name>.trashinfo` that records where it was and
/// when, which is what file managers read to list it and put it back.
///
/// The home Trash (`$XDG_DATA_HOME/Trash`) takes files of the disk it is on. A file on another
/// disk goes to that disk's own `.Trash-<uid>` at its top, made when missing; where that can't
/// be made (a read-only disk, or one that can't hold a folder private to the user, as some network
/// shares and FAT-like disks, or whose top folder others could rename our folders in: writable by
/// group or others without the sticky bit, or owned by neither the user nor root), the disk has
/// no Trash. A folder of that name that is there but
/// unsafe (a link, not the user's, open to others) is a failure to put right, never "no Trash",
/// even on a disk that could not hold a private folder: only a folder just made is taken for that.
/// The same on a disk whose top folder others could rename entries in: with no `.Trash-<uid>`
/// there it has no Trash, with anything at that name (even a private folder of the user's) it is
/// a failure, as the folder can't be trusted there and erasing would lose what is in it.
#[cfg(unix)]
pub mod xdg {
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};

    use super::BinError;

    /// Where this user's Trash is, and who they are.
    #[derive(Clone, Debug)]
    pub struct Places {
        /// `$XDG_DATA_HOME/Trash` (`~/.local/share/Trash`).
        pub home_trash: PathBuf,
        pub uid: u32,
        /// The time written to `DeletionDate` (`YYYY-MM-DDThh:mm:ss`, local time, as the
        /// specification asks).
        pub now: String,
    }

    impl Places {
        /// `None` without a home folder to keep the Trash in (`HOME` unset or not absolute).
        pub fn of_this_user() -> Option<Places> {
            let home = std::env::var_os("HOME").map(PathBuf::from).filter(|h| h.is_absolute())?;
            let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(".local/share"));
            let uid = rustix::process::geteuid().as_raw();
            let now = deletion_date(chrono::Local::now());
            Some(Places { home_trash: data.join("Trash"), uid, now })
        }
    }

    /// `DeletionDate` text for an instant: its wall-clock time in its own zone, with no zone written.
    pub(super) fn deletion_date<Tz: chrono::TimeZone>(at: chrono::DateTime<Tz>) -> String {
        at.naive_local().format("%Y-%m-%dT%H:%M:%S").to_string()
    }

    /// The device a path is on (`None`: it can't be asked).
    /// (Of what the path leads to: a folder reached through a link is on the disk it is on.)
    fn device(p: &Path) -> Option<u64> {
        std::fs::metadata(p).ok().map(|m| m.dev())
    }

    /// A Trash folder at the top of another disk could not be made. Only a disk that takes no
    /// such folder at all (read-only, or no folders of that kind) has no Trash. A full disk, a
    /// folder that is not ours to write to or any other trouble is a failure: something to put
    /// right, and the file is then never offered for erasing.
    fn no_place(path: &Path, e: std::io::Error) -> BinError {
        use std::io::ErrorKind as K;
        match e.kind() {
            K::ReadOnlyFilesystem | K::Unsupported => BinError::NoTrash,
            _ => BinError::Failed(format!("{}: the Trash on its disk can't be written to: {e}", path.display())),
        }
    }

    /// What a disk's own Trash folders must be: the top one (`.Trash-<uid>`) private to us, the
    /// two in it (`files/`, `info/`) ours (what is in them is then reached only through the
    /// private one).
    #[derive(Clone, Copy, PartialEq)]
    enum Need {
        Private,
        Ours,
    }

    /// Opens a folder without following a link and without waiting on anything that is not a
    /// folder (a FIFO would block a plain open): a link or a non-folder is an error at once.
    fn open_folder(dir: &Path) -> std::io::Result<std::fs::File> {
        use rustix::fs::{Mode, OFlags, open};
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        open(dir, flags, Mode::empty()).map(std::fs::File::from).map_err(std::io::Error::from)
    }

    fn unsafe_dir(dir: &Path, why: &str) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("{} {why}", dir.display()))
    }

    /// Refuses a Trash folder of a disk that is not what it must be (`need`) for `uid`: a symlink
    /// (it could lead anywhere), anything but a folder, one owned by someone else or, for the
    /// private one, one that group or others can enter or read (the `info/` notes say where
    /// files were). On a disk everybody writes to, anyone could otherwise plant such a folder
    /// first.
    ///
    /// The folder is opened without following a link or waiting on a non-folder, and judged by
    /// that handle (`fstat`), so what is judged is what was opened. The writes that follow go by
    /// path again: a folder swapped for a link between the check and them is not caught. The
    /// check is complete only where nobody else can rename our folders (a top that
    /// `top_allows_replacement` accepts: sticky or not writable, owned by us or root); the later
    /// writes still go by path.
    fn check(dir: &Path, uid: u32, need: Need) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        // (the path's own `lstat` first: a plain answer for a link or a file)
        let at_path = std::fs::symlink_metadata(dir)?;
        if at_path.file_type().is_symlink() {
            return Err(unsafe_dir(dir, "is a symbolic link: remove it so a Trash folder can be made"));
        }
        if !at_path.is_dir() {
            return Err(unsafe_dir(dir, "is not a folder: remove it so a Trash folder can be made"));
        }
        let handle = match open_folder(dir) {
            Ok(f) => f.metadata()?,
            // (swapped for a link or a file since the `lstat`)
            Err(e)
                if [rustix::io::Errno::LOOP, rustix::io::Errno::NOTDIR, rustix::io::Errno::MLINK]
                    .iter()
                    .any(|n| e.raw_os_error() == Some(n.raw_os_error())) =>
            {
                return Err(unsafe_dir(dir, "changed while it was checked"));
            }
            Err(e) => return Err(e),
        };
        if handle.uid() != uid {
            return Err(unsafe_dir(dir, "belongs to someone else: remove it, or ask its owner to; it is never taken for \"no Trash\""));
        }
        if need == Need::Private && handle.permissions().mode() & 0o077 != 0 {
            return Err(unsafe_dir(
                dir,
                &format!(
                    "can be read or entered by others: make it private with `chmod 700 {0}` (or remove it, if it holds nothing you need)",
                    dir.display()
                ),
            ));
        }
        Ok(())
    }

    /// The folders of the home Trash, with mode 700 for each one made now (they hold the original
    /// paths; the specification sets no mode for the home Trash, GLib and KDE make it 0700, and a
    /// missing parent such as `~/.local/share` is made 0700 too, as the XDG base directory
    /// specification asks); one that was there is never changed. Through links as before: the home
    /// Trash may be one.
    fn private_dirs(dir: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
    }

    /// One folder of a disk's own Trash: made with mode 700 when missing (the parent first, one
    /// level at a time: never through a link), then checked.
    ///
    /// A disk that can't hold a private folder of ours (it ignores the mode or shows another
    /// owner: some network shares and FAT-like disks, mounted with a fixed mode and owner) makes
    /// the folder just made fail the check. Making it private afterwards is tried; when that
    /// fails too, the folder is taken away again and the disk has no Trash (`Unsupported`),
    /// every time and the same way. A folder that was already there is never loosened or
    /// removed, and never taken for a disk without a Trash, whatever the disk: it is refused,
    /// with how to put it right (including removing it).
    fn make(dir: &Path, uid: u32, need: Need) -> std::io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let made = match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(e),
        };
        match check(dir, uid, need) {
            Err(e) if made && need == Need::Private && e.kind() == std::io::ErrorKind::PermissionDenied => {
                // (only a folder the disk shows as ours is changed: never one it shows as another's)
                let fixed = open_folder(dir)
                    .and_then(|f| if f.metadata()?.uid() == uid { f.set_permissions(std::fs::Permissions::from_mode(0o700)) } else { Err(e) })
                    .and_then(|()| check(dir, uid, need));
                if fixed.is_ok() {
                    return Ok(());
                }
                let _ = std::fs::remove_dir(dir);
                Err(std::io::Error::new(std::io::ErrorKind::Unsupported, format!("{} can't be made private on this disk", dir.display())))
            }
            other => other,
        }
    }

    #[cfg(test)]
    pub(super) fn make_for_test(dir: &Path, uid: u32, private: bool) -> std::io::Result<()> {
        make(dir, uid, if private { Need::Private } else { Need::Ours })
    }

    #[cfg(test)]
    pub(super) fn disk_trash_is_ours(dir: &Path, uid: u32) -> bool {
        check(dir, uid, Need::Private).is_ok()
    }

    /// Whether someone else could replace our Trash folders by renaming or removing them in the
    /// disk's top folder `top`: it can be written to by group or others, has no sticky bit (which
    /// keeps a user from touching another's entries), whoever owns it: another local user with
    /// write and search on a world-writable top of ours can rename our folders too, so a share
    /// mounted with a fixed 0777 mode has no Trash (the safe side). Or it is owned by neither us
    /// nor root: its owner can rename entries even under a sticky bit. The checks of the Trash
    /// folders are only as good as this: the writes that follow go by path.
    fn top_allows_replacement(top: &Path, uid: u32) -> std::io::Result<bool> {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(top)?;
        Ok(replaceable_by_others(m.permissions().mode(), m.uid(), uid))
    }

    /// [`top_allows_replacement`] on what a folder shows: its mode and its owner, for user `uid`.
    pub(super) fn replaceable_by_others(mode: u32, owner: u32, uid: u32) -> bool {
        (mode & 0o022 != 0 && mode & 0o1000 == 0) || (owner != uid && owner != 0)
    }

    #[cfg(test)]
    pub(super) fn top_allows_replacement_for_test(top: &Path, uid: u32) -> std::io::Result<bool> {
        top_allows_replacement(top, uid)
    }

    /// The top folder of the disk `dir` is on: the highest folder above it still on that device.
    fn top_of_disk(dir: &Path) -> Option<PathBuf> {
        let dev = device(dir)?;
        let mut top = dir.to_path_buf();
        // (bounded: a path has no more levels than this)
        for _ in 0..4096 {
            match top.parent() {
                Some(up) if device(up) == Some(dev) => top = up.to_path_buf(),
                _ => break,
            }
        }
        Some(top)
    }

    /// `p` with its links resolved (itself when that fails: it is then not found either).
    fn real(p: &Path) -> PathBuf {
        p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
    }

    /// A path as `Path=` wants it: percent-encoded, `/` kept.
    fn encoded(p: &Path) -> String {
        use std::os::unix::ffi::OsStrExt;
        let mut out = String::new();
        for b in p.as_os_str().as_bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(*b as char),
                b => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }

    /// Move `path` to the Trash; returns where it is now.
    pub fn trash(path: &Path, places: &Places) -> Result<PathBuf, BinError> {
        trash_in(path, None, places)
    }

    /// [`trash`] for a file on a disk whose top folder is `top`, whichever disk the home Trash is
    /// on: the way to reach the disk's own Trash (and the top-folder rules before it) without a
    /// second disk.
    #[cfg(test)]
    pub(super) fn trash_at_top(path: &Path, top: &Path, places: &Places) -> Result<PathBuf, BinError> {
        trash_in(path, Some(top), places)
    }

    /// What a disk whose top folder lets others rename our folders says when asked for a Trash:
    /// "no Trash" only when it has no `.Trash-<uid>` at all (nothing there is then lost by
    /// erasing). A folder, link or file of that name that is there is something to put right,
    /// never "no Trash": the file is not offered for erasing.
    fn replaceable_top(top: &Path, uid: u32, path: &Path) -> BinError {
        let bin = top.join(format!(".Trash-{uid}"));
        match std::fs::symlink_metadata(&bin) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BinError::NoTrash,
            Ok(_) => BinError::Failed(format!(
                "{}: {} is there, but the top folder of its disk lets others rename it, so it can't be trusted: the top folder must be owned by you or root, and be sticky or not writable by others (the files in {} are kept)",
                path.display(),
                bin.display(),
                bin.display()
            )),
            Err(e) => BinError::Failed(format!(
                "{}: {} can't be read ({e}), and the top folder of its disk lets others rename it",
                path.display(),
                bin.display()
            )),
        }
    }

    /// `top`: the top folder of the file's disk when the caller says (tests); else found, and
    /// the home Trash is used for a file on its disk.
    fn trash_in(path: &Path, top: Option<&Path>, places: &Places) -> Result<PathBuf, BinError> {
        let failed = |what: &str, e: std::io::Error| BinError::Failed(format!("{}: {what}: {e}", path.display()));
        let path = std::path::absolute(path).map_err(|e| failed("not a path", e))?;
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(BinError::Failed(format!("{}: not a file", path.display())));
        };
        // (the folder with its links resolved, as the disk's top and `Path=` are found from it:
        // the file itself is not resolved, so a link is moved as a link)
        let dir = dir.canonicalize().map_err(|e| failed("its folder can't be read", e))?;
        let (dir, path) = (dir.as_path(), dir.join(name));
        let top = top.map(real);
        let top = top.as_deref();
        let Some(dev) = device(dir) else { return Err(BinError::Failed(format!("{}: its folder can't be read", path.display()))) };
        // the home Trash for files of its disk, else the disk's own
        // (a home Trash that can't be made or read is a failure, never "no Trash": the disk
        // the home folder is on has one, it only needs putting right)
        private_dirs(&places.home_trash).map_err(|e| failed("the Trash can't be made", e))?;
        let Some(home_dev) = device(&places.home_trash) else {
            return Err(BinError::Failed(format!("{}: the Trash can't be read", path.display())));
        };
        let home = top.is_none() && home_dev == dev;
        let (bin, relative_to) = if home {
            (places.home_trash.clone(), None)
        } else {
            let top = match top {
                Some(t) => t.to_path_buf(),
                None => top_of_disk(dir).ok_or_else(|| BinError::Failed(format!("{}: its disk can't be found", path.display())))?,
            };
            // (a top that others can replace our folders in holds no safe Trash; a top that
            // can't be read is a failure: only that answer can lead to "no Trash")
            if top_allows_replacement(&top, places.uid).map_err(|e| failed("the top folder of its disk can't be read", e))? {
                return Err(replaceable_top(&top, places.uid, &path));
            }
            (top.join(format!(".Trash-{}", places.uid)), Some(top))
        };
        let (files, info) = (bin.join("files"), bin.join("info"));
        if home {
            private_dirs(&files).and_then(|()| private_dirs(&info)).map_err(|e| failed("the Trash can't be written to", e))?;
        } else {
            // (a disk's own Trash is on a disk others may write to: only a private one of ours)
            make(&bin, places.uid, Need::Private)
                .and_then(|()| make(&files, places.uid, Need::Ours))
                .and_then(|()| make(&info, places.uid, Need::Ours))
                .map_err(|e| no_place(&path, e))?;
        }
        // in a disk's own Trash the path is written from the disk's top
        let original = relative_to.as_deref().and_then(|top| path.strip_prefix(top).ok()).unwrap_or(&path);
        let record = format!("[Trash Info]\nPath={}\nDeletionDate={}\n", encoded(original), places.now);
        // a name of its own: the info file is made first and never replaces one (the
        // specification's way of reserving the name)
        let base = name.to_string_lossy().to_string();
        for n in 0..10_000u32 {
            let unique = if n == 0 { base.clone() } else { format!("{base}.{n}") };
            let (info_file, to) = (info.join(format!("{unique}.trashinfo")), files.join(&unique));
            // (mode 600: the note holds an original path)
            use std::os::unix::fs::OpenOptionsExt;
            match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&info_file) {
                Ok(mut f) => {
                    use std::io::Write;
                    // (a file of that name left in the Trash without its note: the next name)
                    if std::fs::symlink_metadata(&to).is_ok() {
                        drop(f);
                        let _ = std::fs::remove_file(&info_file);
                        continue;
                    }
                    let moved = f.write_all(record.as_bytes()).and_then(|()| f.sync_all()).and_then(|()| std::fs::rename(&path, &to));
                    return match moved {
                        Ok(()) => Ok(to),
                        Err(e) => {
                            // nothing went: the reservation goes too
                            let _ = std::fs::remove_file(&info_file);
                            Err(failed("can't be moved to the Trash", e))
                        }
                    };
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                // (a full disk, a name too long…: a failure, never "no Trash")
                Err(e) => return Err(failed("the Trash can't take a note about it", e)),
            }
        }
        Err(BinError::Failed(format!("{}: the Trash holds too many files of that name", path.display())))
    }

    /// Whether `dir`'s disk has a Trash, without making one: the home Trash's disk does (it is
    /// made when first needed), another disk when its `.Trash-<uid>` is there. Otherwise not
    /// known: [`trash`] tries to make one, and only its answer says "no Trash". (Never `No`
    /// from here, so a top that others can replace our folders in, with or without a
    /// `.Trash-<uid>` on it, is `Unknown`, and [`trash`] says which of "no Trash" and a failure.)
    pub fn trash_here(dir: &Path, places: &Places) -> super::TrashHere {
        trash_here_in(dir, None, places)
    }

    /// [`trash_here`] with the top folder of the disk given (tests) as `top`, else found.
    fn trash_here_in(dir: &Path, top: Option<&Path>, places: &Places) -> super::TrashHere {
        use super::TrashHere;
        // (its links resolved: the disk's top is found from the folder the disk has)
        let (dir, top) = (real(dir), top.map(real));
        let (dir, top) = (dir.as_path(), top.as_deref());
        let Some(dev) = device(dir) else { return TrashHere::Unknown };
        // (the home Trash may not be made yet: the first folder above it that is there)
        let home_dev = places.home_trash.ancestors().take(4096).find_map(device);
        if top.is_none() && home_dev == Some(dev) {
            return TrashHere::Yes;
        }
        match top.map(Path::to_path_buf).or_else(|| top_of_disk(dir)) {
            // (a link or a folder that isn't ours and private is no Trash: `trash` reports why)
            Some(top)
                if !top_allows_replacement(&top, places.uid).unwrap_or(true)
                    && check(&top.join(format!(".Trash-{}", places.uid)), places.uid, Need::Private).is_ok() =>
            {
                TrashHere::Yes
            }
            _ => TrashHere::Unknown,
        }
    }

    #[cfg(test)]
    pub(super) fn trash_here_at_top(dir: &Path, top: &Path, places: &Places) -> super::TrashHere {
        trash_here_in(dir, Some(top), places)
    }

    /// The `.trashinfo` of a file in a Trash (`<trash>/files/<name>`), to remove once the file
    /// was put back.
    pub fn info_of(at: &Path) -> Option<PathBuf> {
        let (files, name) = (at.parent()?, at.file_name()?);
        (files.file_name()? == "files").then(|| files.parent().map(|bin| bin.join("info").join(format!("{}.trashinfo", name.to_string_lossy()))))?
    }
}

#[cfg(all(test, unix))]
mod tests {
    /// The limit is checked before anyone is asked, with the command's own words.
    #[test]
    fn a_list_over_the_limit_is_refused_with_the_commands_words() {
        assert_eq!(too_many(MAX_PHOTOS), None);
        assert_eq!(too_many(MAX_PHOTOS + 1).as_deref(), Some("more than 1000000 photos in one call"));
    }

    use super::xdg::{Places, deletion_date, info_of, trash};
    use super::*;

    /// A scratch folder that goes away with the test.
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `DeletionDate` is the wall-clock time of the user's own time zone, with no zone written
    /// (freedesktop Trash specification): the same instant reads differently in Tokyo and in UTC.
    #[test]
    fn the_deletion_date_is_local_time_without_a_zone() {
        use chrono::{FixedOffset, TimeZone};
        let tokyo = FixedOffset::east_opt(9 * 3600).unwrap().with_ymd_and_hms(2026, 10, 11, 21, 5, 9).unwrap();
        assert_eq!(deletion_date(tokyo), "2026-10-11T21:05:09");
        let la = FixedOffset::west_opt(7 * 3600).unwrap().with_ymd_and_hms(2026, 10, 11, 5, 5, 9).unwrap();
        assert_eq!(deletion_date(la), "2026-10-11T05:05:09");
    }

    fn scratch(tag: &str) -> (Scratch, Places) {
        let dir = std::env::temp_dir().join(format!("lc-xdg-trash-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("photos")).unwrap();
        let places = Places { home_trash: dir.join("data/Trash"), uid: 1000, now: "2026-10-11T12:00:00".into() };
        (Scratch(dir), places)
    }

    /// A file goes to `files/`, with an info file that says where it was and when; a second file
    /// of the same name gets a name of its own, and nothing in the Trash is replaced.
    #[test]
    fn a_file_goes_to_the_freedesktop_trash_with_its_info() {
        let (dir, places) = scratch("basic");
        let photo = dir.0.join("photos/my photo é.jpg");
        std::fs::write(&photo, "one").unwrap();
        let at = trash(&photo, &places).unwrap();
        assert_eq!(at, places.home_trash.join("files/my photo é.jpg"));
        assert!(!photo.exists());
        assert_eq!(std::fs::read_to_string(&at).unwrap(), "one");
        let info = std::fs::read_to_string(info_of(&at).unwrap()).unwrap();
        let mut lines = info.lines();
        assert_eq!(lines.next(), Some("[Trash Info]"));
        let path = lines.next().unwrap();
        assert!(path.starts_with("Path=/") && path.ends_with("/photos/my%20photo%20%C3%A9.jpg"), "{path}");
        assert_eq!(lines.next(), Some("DeletionDate=2026-10-11T12:00:00"));
        // the same name again
        std::fs::write(&photo, "two").unwrap();
        let second = trash(&photo, &places).unwrap();
        assert_eq!(second, places.home_trash.join("files/my photo é.jpg.1"));
        assert_eq!(std::fs::read_to_string(&at).unwrap(), "one", "the first is untouched");
        assert!(info_of(&second).unwrap().exists());
    }

    /// A file that is not there is not trashed, and leaves nothing behind in the Trash.
    #[test]
    fn a_file_that_can_t_be_moved_leaves_no_info_behind() {
        let (dir, places) = scratch("gone");
        let e = trash(&dir.0.join("photos/none.jpg"), &places).unwrap_err();
        assert!(matches!(e, BinError::Failed(_)), "{e:?}");
        let left: Vec<_> = std::fs::read_dir(places.home_trash.join("info")).unwrap().flatten().collect();
        assert!(left.is_empty(), "{left:?}");
    }

    /// What goes into the home Trash is private (the specification sets no mode; GLib and KDE make
    /// the folders 0700): the folders new (0700) and each note about a file (0600), since they hold
    /// original paths. A folder that was there is never changed.
    #[test]
    fn a_new_home_trash_and_its_notes_are_private_and_an_old_one_is_left_alone() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let (dir, places) = scratch("home-private");
        let photo = dir.0.join("photos/a.jpg");
        std::fs::write(&photo, "x").unwrap();
        let at = trash(&photo, &places).unwrap();
        for d in [places.home_trash.clone(), places.home_trash.join("files"), places.home_trash.join("info")] {
            assert_eq!(mode(&d), 0o700, "{}", d.display());
        }
        assert_eq!(mode(&info_of(&at).unwrap()), 0o600);
        // folders that were there, shared with others on purpose, keep their mode
        let (dir, places) = scratch("home-existing");
        for d in [places.home_trash.clone(), places.home_trash.join("files"), places.home_trash.join("info")] {
            std::fs::create_dir_all(&d).unwrap();
            std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let photo = dir.0.join("photos/b.jpg");
        std::fs::write(&photo, "x").unwrap();
        trash(&photo, &places).unwrap();
        for d in [places.home_trash.clone(), places.home_trash.join("files"), places.home_trash.join("info")] {
            assert_eq!(mode(&d), 0o755, "{}", d.display());
        }
    }

    /// A home Trash that can't be written to is a failure to put right, never "no Trash": the
    /// file is then not offered for erasing.
    #[test]
    fn a_home_trash_that_can_t_be_written_is_a_failure_not_a_missing_trash() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, places) = scratch("readonly-home");
        let photo = dir.0.join("photos/a.jpg");
        std::fs::write(&photo, "x").unwrap();
        // the Trash folder is there but takes nothing
        std::fs::create_dir_all(&places.home_trash).unwrap();
        std::fs::set_permissions(&places.home_trash, std::fs::Permissions::from_mode(0o555)).unwrap();
        // restored even if the test panics, so the scratch folder can be removed
        struct Restore(std::path::PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
            }
        }
        let _restore = Restore(places.home_trash.clone());
        // running as root ignores permissions: nothing to check there
        if std::fs::write(places.home_trash.join("probe"), "x").is_ok() {
            return;
        }
        let e = trash(&photo, &places);
        assert!(matches!(e, Err(BinError::Failed(_))), "{e:?}");
        assert!(photo.exists());
        // nor one that can't be made at all
        let nowhere = Places { home_trash: dir.0.join("photos/a.jpg/Trash"), ..places };
        let e = trash(&photo, &nowhere);
        assert!(matches!(e, Err(BinError::Failed(_))), "{e:?}");
        assert!(photo.exists());
    }

    /// A folder on the disk of the home Trash has a Trash, whether or not it was made yet; the
    /// question makes nothing.
    #[test]
    fn a_folder_on_the_home_trash_s_disk_has_a_trash() {
        let (dir, places) = scratch("here");
        assert_eq!(super::xdg::trash_here(&dir.0.join("photos"), &places), TrashHere::Yes);
        assert!(!places.home_trash.exists(), "nothing was made");
        assert_eq!(super::xdg::trash_here(&dir.0.join("not there"), &places), TrashHere::Unknown);
    }

    /// A disk's own Trash is made with mode 700, `files/` and `info/` too, and is then fine.
    #[test]
    fn a_disk_s_own_trash_is_made_private() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let (dir, _) = scratch("private-new");
        let bin = dir.0.join(".Trash-1000");
        let uid = std::fs::metadata(&dir.0).unwrap().uid();
        // (the parent first, as `trash` makes them)
        for d in [bin.clone(), bin.join("files"), bin.join("info")] {
            super::xdg::make_for_test(&d, uid, true).unwrap();
            assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700, "{}", d.display());
        }
    }

    /// A disk's top that others could replace our Trash folders in holds no Trash: writable by
    /// group or others without sticky (whoever owns it), or owned by neither us nor root. Sticky
    /// or not writable, owned by us or root, is fine.
    #[test]
    fn a_top_others_can_replace_our_trash_in_holds_no_trash() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let (dir, _) = scratch("top");
        let top = dir.0.join("top");
        std::fs::create_dir(&top).unwrap();
        let ours = std::fs::metadata(&top).unwrap().uid();
        let on_disk = |mode| {
            std::fs::set_permissions(&top, std::fs::Permissions::from_mode(mode)).unwrap();
            super::xdg::top_allows_replacement_for_test(&top, ours).unwrap()
        };
        assert!(on_disk(0o777) && on_disk(0o775) && on_disk(0o757));
        assert!(!on_disk(0o1777) && !on_disk(0o755));
        // (owners are given: a foreign one or root needs no root to be tried)
        let (me, other) = (1000, 1001);
        let replaceable = |mode, owner| super::xdg::replaceable_by_others(mode, owner, me);
        assert!(replaceable(0o777, me), "world-writable, ours");
        assert!(!replaceable(0o1777, me), "sticky, ours");
        assert!(!replaceable(0o1777, 0), "sticky, root's");
        assert!(!replaceable(0o755, 0), "not writable, root's");
        assert!(replaceable(0o777, 0), "root's, but no sticky");
        assert!(replaceable(0o1777, other), "sticky, but owned by another user");
        assert!(replaceable(0o755, other), "not writable, but owned by another user");
    }

    /// A symlink, a plain file, a folder others can enter and a folder of someone else are all
    /// refused, and `trash_here` does not call them a Trash.
    #[test]
    fn an_unsafe_disk_trash_is_refused() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
        let (dir, _) = scratch("private-bad");
        let uid = std::fs::metadata(&dir.0).unwrap().uid();
        let refused =
            |p: &std::path::Path| matches!(super::xdg::make_for_test(p, uid, true), Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied);
        // a link, to a good folder and to nothing
        let real = dir.0.join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = dir.0.join("link");
        symlink(&real, &link).unwrap();
        assert!(refused(&link));
        let dangling = dir.0.join("dangling");
        symlink(dir.0.join("nowhere"), &dangling).unwrap();
        assert!(super::xdg::make_for_test(&dangling, uid, true).is_err());
        // a file
        let file = dir.0.join("file");
        std::fs::write(&file, "x").unwrap();
        assert!(refused(&file));
        // a folder others can read, or enter
        for mode in [0o755, 0o750, 0o705] {
            let open = dir.0.join(format!("open-{mode:o}"));
            std::fs::create_dir(&open).unwrap();
            std::fs::set_permissions(&open, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(refused(&open), "{mode:o}");
        }
        // someone else's (the owner is compared, so another uid stands in for another user)
        assert!(matches!(super::xdg::make_for_test(&real, uid.wrapping_add(1), true), Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied));
        // the one that is fine
        super::xdg::make_for_test(&real, uid, true).unwrap();
    }

    /// A folder others can read or enter is still fine inside our private Trash (`files/`, `info/`
    /// of older tools), but a link there is not; and a FIFO where the Trash should be is refused
    /// at once, not waited on.
    #[test]
    fn the_folders_in_a_private_trash_need_only_be_ours_and_a_fifo_never_hangs() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
        let (dir, _) = scratch("private-children");
        let uid = std::fs::metadata(&dir.0).unwrap().uid();
        let open = dir.0.join("files");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::xdg::make_for_test(&open, uid, false).unwrap();
        let link = dir.0.join("info");
        symlink(&open, &link).unwrap();
        assert!(matches!(super::xdg::make_for_test(&link, uid, false), Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied));
        // the error says how to put a loose folder right
        let loose = super::xdg::make_for_test(&open, uid, true).unwrap_err().to_string();
        assert!(loose.contains("chmod 700"), "{loose}");
        // a FIFO: answered, not waited on (a hang fails the test after 10 s)
        let fifo = dir.0.join(".Trash-1000");
        assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        let (tx, rx) = std::sync::mpsc::channel();
        let (f, u) = (fifo.clone(), uid);
        std::thread::spawn(move || {
            let _ = tx.send((super::xdg::make_for_test(&f, u, true).is_err(), super::xdg::disk_trash_is_ours(&f, u)));
        });
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(10)).expect("a FIFO hung the check"), (true, false));
    }

    /// A disk that can't hold a folder of ours (it shows another owner: a share mounted with a
    /// fixed one) has no Trash, the same way every time, and no folder is left; a folder that
    /// was there already, with that other owner, is refused instead.
    #[test]
    fn a_disk_that_can_t_hold_a_private_folder_has_no_trash() {
        use std::os::unix::fs::MetadataExt;
        let (dir, _) = scratch("private-unsupported");
        let uid = std::fs::metadata(&dir.0).unwrap().uid();
        let bin = dir.0.join(".Trash-1000");
        for _ in 0..2 {
            let e = super::xdg::make_for_test(&bin, uid.wrapping_add(1), true).unwrap_err();
            assert_eq!(e.kind(), std::io::ErrorKind::Unsupported, "{e}");
            assert!(!bin.exists(), "nothing is left behind");
        }
        // the same folder, there before: refused, and kept
        std::fs::create_dir(&bin).unwrap();
        let e = super::xdg::make_for_test(&bin, uid.wrapping_add(1), true).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied, "{e}");
        assert!(bin.exists());
        assert!(e.to_string().contains("remove it") && e.to_string().contains("never taken for"), "{e}");
    }

    /// A disk's top folder that others could rename our folders in (0777, no sticky bit) with no
    /// `.Trash-<uid>` there: "no Trash", and nothing is made. With one there (a folder, a link
    /// or a file), never "no Trash": the file would be erased under `noTrash: "delete"` while a
    /// Trash folder exists. The dialog's answer is never `No` for it, and `Yes` only for a safe
    /// top with a private Trash.
    #[test]
    fn a_replaceable_top_is_no_trash_only_when_it_has_no_trash_folder() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
        let (dir, places) = scratch("replaceable-top");
        let uid = std::fs::metadata(&dir.0).unwrap().uid();
        let places = Places { uid, ..places };
        let top = dir.0.join("top");
        std::fs::create_dir_all(top.join("photos")).unwrap();
        std::fs::set_permissions(&top, std::fs::Permissions::from_mode(0o777)).unwrap();
        let bin = top.join(format!(".Trash-{uid}"));
        let photo = top.join("photos/a.jpg");
        std::fs::write(&photo, "x").unwrap();
        let (here, at_top) = (top.join("photos"), top.as_path());

        // (a) no folder of that name: no Trash, and none is made
        assert!(matches!(super::xdg::trash_at_top(&photo, at_top, &places), Err(BinError::NoTrash)));
        assert!(photo.exists() && std::fs::symlink_metadata(&bin).is_err(), "nothing was changed or made");
        assert_eq!(super::xdg::trash_here_at_top(&here, at_top, &places), TrashHere::Unknown);

        // (b) a folder of that name: a failure that says what to do, the file stays
        std::fs::create_dir(&bin).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        let e = super::xdg::trash_at_top(&photo, at_top, &places).unwrap_err();
        assert!(matches!(&e, BinError::Failed(m) if m.contains("lets others rename")), "{e:?}");
        assert!(photo.exists());
        assert_eq!(super::xdg::trash_here_at_top(&here, at_top, &places), TrashHere::Unknown, "not a usable Trash");
        std::fs::remove_dir(&bin).unwrap();

        // (c) a link of that name: a failure, and what it leads to is untouched
        let target = dir.0.join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        symlink(&target, &bin).unwrap();
        let e = super::xdg::trash_at_top(&photo, at_top, &places).unwrap_err();
        assert!(matches!(e, BinError::Failed(_)), "{e:?}");
        assert!(photo.exists());
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0, "the link's target is untouched");
        assert_eq!(super::xdg::trash_here_at_top(&here, at_top, &places), TrashHere::Unknown);
        // (a file too)
        std::fs::remove_file(&bin).unwrap();
        std::fs::write(&bin, "").unwrap();
        assert!(matches!(super::xdg::trash_at_top(&photo, at_top, &places), Err(BinError::Failed(_))));
        assert!(photo.exists());
        // (a dangling link too)
        std::fs::remove_file(&bin).unwrap();
        symlink(dir.0.join("nowhere"), &bin).unwrap();
        assert!(matches!(super::xdg::trash_at_top(&photo, at_top, &places), Err(BinError::Failed(_))));
        assert!(photo.exists());
        std::fs::remove_file(&bin).unwrap();

        // (control) the same top made safe (sticky) takes a private Trash and the file goes in it
        std::fs::set_permissions(&top, std::fs::Permissions::from_mode(0o1777)).unwrap();
        let at = super::xdg::trash_at_top(&photo, at_top, &places).unwrap();
        // (the top is found with its links resolved: /var is /private/var on macOS)
        assert!(at.starts_with(bin.canonicalize().unwrap()) && !photo.exists());
        std::fs::write(&photo, "y").unwrap();
        assert_eq!(super::xdg::trash_here_at_top(&here, at_top, &places), TrashHere::Yes);
    }

    /// The user is the process's effective user, not whoever owns `$HOME`.
    #[test]
    fn the_user_is_the_process_s_own() {
        use std::os::unix::fs::MetadataExt;
        let (dir, _) = scratch("uid");
        let probe = dir.0.join("probe");
        std::fs::write(&probe, "x").unwrap();
        let places = super::xdg::Places::of_this_user().expect("HOME is set");
        assert_eq!(places.uid, std::fs::metadata(&probe).unwrap().uid());
    }

    /// `trash_here` takes a private `.Trash-<uid>` only (the same check): a link is no Trash.
    #[test]
    fn a_disk_trash_that_is_a_link_is_not_ours() {
        use std::os::unix::fs::symlink;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let (dir, places) = scratch("private-here");
        let uid = std::fs::metadata(&dir.0).unwrap().uid();
        let real = dir.0.join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = dir.0.join(format!(".Trash-{}", places.uid));
        symlink(&real, &link).unwrap();
        assert!(!super::xdg::disk_trash_is_ours(&link, uid));
        assert!(super::xdg::disk_trash_is_ours(&real, uid));
    }

    /// Putting a file back never replaces what is at its place now.
    #[test]
    fn putting_back_never_replaces() {
        let (dir, places) = scratch("back");
        let photo = dir.0.join("photos/a.jpg");
        std::fs::write(&photo, "old").unwrap();
        let at = trash(&photo, &places).unwrap();
        std::fs::write(&photo, "new").unwrap();
        assert!(SystemBin.put_back(&at, &photo).is_err());
        assert_eq!(std::fs::read_to_string(&photo).unwrap(), "new");
        std::fs::remove_file(&photo).unwrap();
        SystemBin.put_back(&at, &photo).unwrap();
        assert_eq!(std::fs::read_to_string(&photo).unwrap(), "old");
    }

    /// The real Trash of this Mac (run by hand: `cargo test -p lightcraft-engine -- --ignored
    /// the_system_trash`): a file goes there and comes back, so nothing is left in it.
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn the_system_trash_takes_a_file_and_says_where_it_went() {
        let dir = std::env::temp_dir().join(format!("lc-system-trash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("lightcraft-trash-test.jpg");
        std::fs::write(&photo, "x").unwrap();
        let at = SystemBin.trash(&photo).unwrap().expect("the Finder's Trash says where");
        assert!(!photo.exists() && at.exists(), "{at:?}");
        SystemBin.put_back(&at, &photo).unwrap();
        assert!(photo.exists() && !at.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The Finder says a folder of the startup disk has a Trash (run by hand, like the test above).
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn the_system_trash_is_there_for_a_folder_of_the_startup_disk() {
        assert_eq!(SystemBin.trash_here(&std::env::temp_dir()), TrashHere::Yes);
    }

    /// A folder reached through a link is judged, and written in the note, as the folder it is.
    #[test]
    fn a_folder_reached_through_a_link_is_noted_by_its_real_path() {
        let (dir, places) = scratch("link");
        let real = dir.0.join("photos").canonicalize().unwrap();
        let link = dir.0.join("shortcut");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let photo = link.join("a b.jpg");
        std::fs::write(&photo, "x").unwrap();
        let at = trash(&photo, &places).unwrap();
        let info = std::fs::read_to_string(info_of(&at).unwrap()).unwrap();
        let path = info.lines().nth(1).unwrap();
        assert_eq!(path, format!("Path={}/a%20b.jpg", real.display()), "{path}");
        assert!(!real.join("a b.jpg").exists());
    }

    /// A Trash of the tests' own: answers as told, and records what it was asked to do.
    #[derive(Default)]
    struct FakeBin {
        /// `trash` answers `Err(Failed)` for these paths, `Err(NoTrash)` for `no_trash`.
        refuse: Vec<&'static str>,
        no_trash: Vec<&'static str>,
        /// `put_back` fails from these places.
        stuck: Vec<&'static str>,
        calls: std::sync::Mutex<Vec<String>>,
    }
    impl Bin for FakeBin {
        fn trash(&self, path: &Path) -> Result<Option<PathBuf>, BinError> {
            let p = path.to_string_lossy().to_string();
            self.calls.lock().unwrap().push(format!("trash {p}"));
            if self.no_trash.contains(&p.as_str()) {
                return Err(BinError::NoTrash);
            }
            if self.refuse.contains(&p.as_str()) {
                return Err(BinError::Failed("no".into()));
            }
            Ok(Some(PathBuf::from(format!("/bin{p}"))))
        }
        fn put_back(&self, at: &Path, to: &Path) -> std::io::Result<()> {
            let at = at.to_string_lossy().to_string();
            self.calls.lock().unwrap().push(format!("put_back {at} {}", to.display()));
            if self.stuck.contains(&at.as_str()) { Err(std::io::Error::other("busy")) } else { Ok(()) }
        }
        fn erase(&self, path: &Path) -> std::io::Result<()> {
            self.calls.lock().unwrap().push(format!("erase {}", path.display()));
            Ok(())
        }
    }

    fn trashed(from: &str, now: Whereabouts) -> Trashed {
        Trashed { from: from.into(), now }
    }

    /// Undo puts back what the Trash says where it is, says what stayed out and why, and redo
    /// takes the same files again; nothing is ever erased.
    #[test]
    fn a_put_back_and_the_redo_after_it_come_back_to_the_same_files() {
        let bin = FakeBin { stuck: vec!["/bin/b"], ..Default::default() };
        let step = TrashStep::PutBack(vec![
            trashed("/a", Whereabouts::At("/bin/a".into())),
            trashed("/b", Whereabouts::At("/bin/b".into())),
            trashed("/c", Whereabouts::InTrash),
            trashed("/d", Whereabouts::Erased),
            trashed("/e", Whereabouts::InPlace),
        ]);
        let (redo, note) = step.run(&bin);
        assert_eq!(
            note.as_deref(),
            Some(
                "1 file could not be put back from the Trash (busy); 1 file is still in the Trash: put it back from there; 1 file was erased and is not coming back"
            )
        );
        let TrashStep::Again { back, still_out } = &redo else { panic!("{redo:?}") };
        assert_eq!(back, &["/a".to_string(), "/e".to_string()], "what is in place again, in order");
        assert_eq!(
            still_out,
            &[trashed("/b", Whereabouts::At("/bin/b".into())), trashed("/c", Whereabouts::InTrash), trashed("/d", Whereabouts::Erased)]
        );
        let (undo, note) = redo.run(&bin);
        assert_eq!(note, None);
        let TrashStep::PutBack(list) = &undo else { panic!("{undo:?}") };
        assert_eq!(
            list,
            &[
                trashed("/b", Whereabouts::At("/bin/b".into())),
                trashed("/c", Whereabouts::InTrash),
                trashed("/d", Whereabouts::Erased),
                trashed("/a", Whereabouts::At("/bin/a".into())),
                trashed("/e", Whereabouts::At("/bin/e".into())),
            ]
        );
        assert!(bin.calls.lock().unwrap().iter().all(|c| !c.starts_with("erase")), "{:?}", bin.calls.lock().unwrap());
    }

    /// A redo whose file can't go to the Trash again leaves it where it is, whatever the reason
    /// (a disk without a Trash included), says so, and keeps it for the next redo. It is not erased.
    #[test]
    fn a_redo_that_can_t_trash_a_file_keeps_it_in_place_and_never_erases_it() {
        let bin = FakeBin { refuse: vec!["/a"], no_trash: vec!["/b"], ..Default::default() };
        let step = TrashStep::Again { back: vec!["/a".into(), "/b".into(), "/c".into()], still_out: vec![trashed("/d", Whereabouts::InTrash)] };
        let (undo, note) = step.run(&bin);
        assert_eq!(note.as_deref(), Some("2 files could not be moved to the Trash and stay where they were"));
        let TrashStep::PutBack(list) = &undo else { panic!("{undo:?}") };
        assert_eq!(
            list,
            &[
                trashed("/d", Whereabouts::InTrash),
                trashed("/a", Whereabouts::InPlace),
                trashed("/b", Whereabouts::InPlace),
                trashed("/c", Whereabouts::At("/bin/c".into())),
            ]
        );
        assert_eq!(*bin.calls.lock().unwrap(), ["trash /a", "trash /b", "trash /c"]);
        let one = FakeBin { refuse: vec!["/a"], ..Default::default() };
        let (_, note) = TrashStep::Again { back: vec!["/a".into()], still_out: vec![] }.run(&one);
        assert_eq!(note.as_deref(), Some("1 file could not be moved to the Trash and stays where it was"));
    }
}
