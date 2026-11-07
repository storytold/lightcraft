//! The folders automation may read and write.
//!
//! `lightcraft-cli mcp --automation-read-root <dir> --automation-write-root <dir>` confines the
//! files an agent's commands reach. [`AutomationRoots`] holds the two folders. While they are in force, every path a command
//! reads goes through [`check_read`] and every path it writes through [`check_write`]:
//!
//! - The path is made absolute against the working directory, as the file system reads it. Its
//!   links are followed: for a file that doesn't exist yet, the links of its nearest existing
//!   folder, the rest of the path being plain names (`..` below a missing folder is refused, and so
//!   is a link that leads nowhere).
//! - The result must lie inside the root, compared by whole components (`/work2` isn't inside
//!   `/work`), exactly: on Windows what exists is resolved to its real case first.
//! - Read access is permitted inside either root; write access only inside the write root.
//!
//! Roots are in force on a thread inside [`confine`], and in the whole process after
//! [`confine_process`] (`lightcraft-cli mcp`), except inside [`unconfined`]. Without roots
//! nothing is checked.
//!
//! [`Session::execute`](crate::Session::execute) also checks the paths among a command's
//! parameters before it runs, and refuses a command that isn't classified: see [`commands`].
//!
//! A path is checked, then opened: a process that can change the folders in between (swapping in
//! a link) can still win that race. A file with more than one name is refused, because another
//! name may lie outside the roots, and a name count that cannot be read is refused too. A file
//! with one name is an ordinary file. The roots keep an agent to the files it was given; they don't
//! guard against other programs on the computer.

pub mod commands;

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};

pub use commands::check_command;

/// Read access or write access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// The folders automation may read and write; a root left out grants no access of its kind.
#[derive(Debug)]
pub struct AutomationRoots {
    read: Option<Root>,
    write: Option<Root>,
}

/// An existing filesystem root folder with its canonical path and display path.
#[derive(Debug)]
struct Root {
    /// The folder made absolute, links followed: what paths are compared with.
    resolved: PathBuf,
    /// The same without Windows' `\\?\` prefix, for people.
    shown: PathBuf,
}

impl Root {
    /// The folder at `path`, which must exist.
    fn open(path: &Path, access: Access) -> Result<Self, String> {
        reject_unsafe_prefix(path)?;
        let kind = match access {
            Access::Read => "read",
            Access::Write => "write",
        };
        let resolved = std::fs::canonicalize(path)
            .and_then(|p| if p.is_dir() { Ok(p) } else { Err(std::io::ErrorKind::NotADirectory.into()) })
            .map_err(|e| format!("cannot open the automation {kind} root `{}`: {e}", path.display()))?;
        Ok(Self { shown: plain(resolved.clone()), resolved })
    }
}

impl AutomationRoots {
    /// The roots `read` and `write`, which must be existing folders; `None` when neither is given
    /// (nothing is confined then).
    pub fn new(read: Option<&Path>, write: Option<&Path>) -> Result<Option<Arc<Self>>, String> {
        if read.is_none() && write.is_none() {
            return Ok(None);
        }
        let read = read.map(|p| Root::open(p, Access::Read)).transpose()?;
        let write = write.map(|p| Root::open(p, Access::Write)).transpose()?;
        Ok(Some(Arc::new(Self { read, write })))
    }

    /// The read root, absolute with its links followed.
    pub fn read_root(&self) -> Option<&Path> {
        self.read.as_ref().map(|r| r.shown.as_path())
    }

    /// The write root, absolute with its links followed.
    pub fn write_root(&self) -> Option<&Path> {
        self.write.as_ref().map(|r| r.shown.as_path())
    }

    /// May the file or folder at `path` be read (or written)? `Err` says why not.
    ///
    /// A file with more than one name is rejected: another name may lie outside the roots, and
    /// opening this one would read or change that file too. A name count that cannot be read is
    /// rejected for the same reason. A directory is judged only by where it is, because its link
    /// count includes `.`, `..` and child directories rather than extra names of the directory.
    pub fn check(&self, path: impl AsRef<Path>, access: Access) -> Result<(), String> {
        let path = path.as_ref();
        self.check_location(path, access)?;
        one_name(path)
    }

    /// May `path` and its known placed destination be written/removed without another name
    /// escaping the roots? A same-file pair is allowed only when those are its exactly two names.
    pub fn check_write_pair(&self, path: impl AsRef<Path>, placed: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        let placed = placed.as_ref();
        self.check_location(path, Access::Write)?;
        self.check_location(placed, Access::Write)?;
        let same = lightcraft_catalog::safe_file::same_file_strict(path, placed)
            .map_err(|e| format!("automation path rejected: cannot verify the placed file pair ({path:?}, {placed:?}): {e}"))?;
        if same {
            match lightcraft_catalog::safe_file::link_count(path) {
                Ok(2) => Ok(()),
                Ok(count) => Err(format!("automation path rejected: the placed file pair has {count} names, not two: {}", path.display())),
                Err(e) => Err(format!("automation path rejected: cannot verify the names of the placed file pair at {}: {e}", path.display())),
            }
        } else {
            one_name(path)?;
            one_name(placed)
        }
    }

    /// Check root membership and link-entry location without applying the multiple-name rule.
    fn check_location(&self, path: &Path, access: Access) -> Result<(), String> {
        let roots: Vec<&Root> = match access {
            Access::Read => self.read.iter().chain(self.write.iter()).collect(),
            Access::Write => self.write.iter().collect(),
        };
        if roots.is_empty() {
            return match access {
                Access::Read => Err(format!("automation filesystem access is not granted: read authority is absent: {}", path.display())),
                Access::Write => Err(format!("automation filesystem access is not granted: write authority is absent: {}", path.display())),
            };
        }
        let in_roots = |p: &Path| roots.iter().any(|r| inside(p, &r.resolved));
        let mut allowed = in_roots(&resolve(path)?);
        // Writing to a link replaces, renames or removes the link where it stands, whatever it points to.
        if allowed
            && access == Access::Write
            && let Some(folder) = folder_of_link(path)
        {
            allowed = in_roots(&resolve(folder)?);
        }
        if allowed {
            return Ok(());
        }
        let shown: Vec<String> = roots.iter().map(|r| r.shown.display().to_string()).collect();
        let kind = match access {
            Access::Read => "read",
            Access::Write => "write",
        };
        Err(format!("automation path rejected: outside the {kind} root {}: {}", shown.join(", "), path.display()))
    }

    /// May a new file be created inside this folder, following a folder link to its target?
    /// Unlike [`Self::check`] for a link being changed, only the destination folder matters.
    pub fn check_container(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        let Some(root) = &self.write else {
            return Err(format!("automation filesystem access is not granted: write authority is absent: {}", path.display()));
        };
        if inside(&resolve(path)?, &root.resolved) {
            // The destination is a folder; its link count is not an extra file name.
            return one_name(path);
        }
        Err(format!("automation path rejected: outside the write root {}: {}", root.shown.display(), path.display()))
    }
}

/// Where the roots are in force on a thread ([`confine`], [`unconfined`]).
#[derive(Clone, Debug)]
enum Scope {
    Confined(Arc<AutomationRoots>),
    Unconfined,
}

thread_local! {
    /// This thread's scope; `None`: the process's roots ([`confine_process`]).
    static SCOPE: RefCell<Option<Scope>> = const { RefCell::new(None) };
}

/// The roots in force in every thread outside [`confine`] and [`unconfined`].
static PROCESS: OnceLock<Arc<AutomationRoots>> = OnceLock::new();

/// Put `roots` in force in the whole process, for good. `false` when other roots were in force
/// already (they stay as they were).
pub fn confine_process(roots: Arc<AutomationRoots>) -> bool {
    PROCESS.set(roots.clone()).is_ok() || PROCESS.get().is_some_and(|r| Arc::ptr_eq(r, &roots))
}

/// Puts the scope it replaced back when dropped (also when `f` panics).
struct Restore(Option<Scope>);

impl Drop for Restore {
    /// Put the thread's previous scope back.
    fn drop(&mut self) {
        let previous = self.0.take();
        SCOPE.with(|s| *s.borrow_mut() = previous);
    }
}

/// Executes `f` within the given thread-local scope, restoring the previous scope on drop.
fn with_scope<T>(scope: Option<Scope>, f: impl FnOnce() -> T) -> T {
    let _restore = Restore(SCOPE.with(|s| s.replace(scope)));
    f()
}

/// Run `f` with `roots` in force on this thread (`None`: as things stand).
pub fn confine<T>(roots: Option<&Arc<AutomationRoots>>, f: impl FnOnce() -> T) -> T {
    match roots {
        Some(r) => with_scope(Some(Scope::Confined(r.clone())), f),
        None => f(),
    }
}

/// Run `f` with no roots in force on this thread: what the person at the keyboard does, and the
/// app's own folders.
pub fn unconfined<T>(f: impl FnOnce() -> T) -> T {
    with_scope(Some(Scope::Unconfined), f)
}

/// The roots in force on this thread, if any.
pub fn current() -> Option<Arc<AutomationRoots>> {
    match SCOPE.with(|s| s.borrow().clone()) {
        Some(Scope::Confined(r)) => Some(r),
        Some(Scope::Unconfined) => None,
        None => PROCESS.get().cloned(),
    }
}

/// `path` without Windows' `\\?\` prefix (which `canonicalize` adds), for people to read.
fn plain(path: PathBuf) -> PathBuf {
    if !cfg!(windows) {
        return path;
    }
    // (a name that isn't Unicode is shown with its prefix: replacing characters would show another name)
    let Some(text) = path.to_str() else {
        return path;
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path,
    }
}

/// May `path` be read here? Always, without roots in force.
pub fn check_read(path: impl AsRef<Path>) -> Result<(), String> {
    current().map_or(Ok(()), |r| r.check(path, Access::Read))
}

/// May `path` be written here? Always, without roots in force.
pub fn check_write(path: impl AsRef<Path>) -> Result<(), String> {
    current().map_or(Ok(()), |r| r.check(path, Access::Write))
}

/// May a new file be created inside this folder, following a folder link to its target?
pub fn check_write_container(path: impl AsRef<Path>) -> Result<(), String> {
    current().map_or(Ok(()), |r| r.check_container(path))
}

/// Check source removal when its only other name may be the move's verified destination.
pub fn check_write_pair(path: impl AsRef<Path>, placed: impl AsRef<Path>) -> Result<(), String> {
    current().map_or(Ok(()), |r| r.check_write_pair(path, placed))
}

/// Reject `path` when it is a file with another name, or when that count cannot be read.
///
/// `Ok(())` when the path has one name, names a directory, or does not exist yet. A directory is
/// not counted: its link count includes `.`, `..` and child directories.
fn one_name(path: &Path) -> Result<(), String> {
    match has_other_names(path) {
        Ok(false) => Ok(()),
        Ok(true) => Err(format!("automation path rejected: {} has more than one name, and another may lie outside the roots", path.display())),
        Err(e) => Err(format!("automation path rejected: cannot count the names of {}: {e}", path.display())),
    }
}

/// `Ok(true)` when `path` is an existing file with more than one directory entry.
///
/// `Ok(false)` when it has one name, names a directory, or does not exist yet. `Err` when the
/// count cannot be read, or comes back as zero: neither is a count a confinement check can trust,
/// so the caller refuses the path. Symbolic links are followed, so the count is the file a read
/// would open.
fn has_other_names(path: &Path) -> std::io::Result<bool> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if meta.is_dir() {
        return Ok(false);
    }
    // A real file has at least one name. Zero is not a count we can trust.
    match lightcraft_catalog::safe_file::link_count(path)? {
        1 => Ok(false),
        0 => Err(std::io::Error::other("the number of names was 0")),
        _ => Ok(true),
    }
}

/// The folder `path` stands in when its last name is a link (`a/link/` and `a/link/.` are that link too).
fn folder_of_link(path: &Path) -> Option<&Path> {
    let path = path.components().as_path();
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return None;
    }
    Some(path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new(".")))
}

/// `path` made absolute with its links followed; for a path that doesn't exist (yet), its nearest
/// existing folder's links followed and the rest appended.
fn resolve(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("automation path rejected: path is empty".into());
    }
    reject_unsafe_prefix(path)?;
    // Checked as given: making `…\NUL` absolute gives the device path `\\.\NUL` on Windows.
    if cfg!(windows) && path.components().any(|c| matches!(c, Component::Normal(n) if n.to_str().is_none_or(is_device_name))) {
        return Err(format!("automation path rejected: reserved device names are not allowed: {}", path.display()));
    }
    let absolute = std::path::absolute(path).map_err(|e| format!("automation path rejected: cannot resolve {}: {e}", path.display()))?;
    // The names below the nearest existing folder, last first.
    let mut missing: Vec<OsString> = vec![];
    let mut at = absolute.as_path();
    loop {
        match std::fs::canonicalize(at) {
            Ok(mut found) => {
                found.extend(missing.iter().rev());
                return Ok(found);
            }
            Err(e) => match std::fs::symlink_metadata(at) {
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(format!("automation path rejected: a link that leads nowhere: {}", path.display()));
                }
                // There, yet its real path can't be had (no permission): nothing to compare.
                Ok(_) => return Err(format!("automation path rejected: cannot resolve {}: {e}", path.display())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("automation path rejected: cannot resolve {}: {e}", path.display())),
            },
        }
        let parent = at.parent();
        match (at.components().next_back(), parent) {
            (Some(Component::Normal(name)), Some(parent)) => {
                missing.push(name.to_os_string());
                at = parent;
            }
            (Some(Component::ParentDir), _) => {
                return Err(format!("automation path rejected: parent traversal below a folder that doesn't exist: {}", path.display()));
            }
            // Down to the root (or drive) and it doesn't exist either.
            _ => {
                let e = std::io::Error::from(std::io::ErrorKind::NotFound);
                return Err(format!("automation path rejected: cannot resolve {}: {e}", path.display()));
            }
        }
    }
}

/// Refuse Windows network and device namespaces before path resolution can contact them.
fn reject_unsafe_prefix(path: &Path) -> Result<(), String> {
    #[cfg(not(windows))]
    let _ = path;
    #[cfg(windows)]
    if let Some(Component::Prefix(prefix)) = path.components().next()
        && matches!(
            prefix.kind(),
            std::path::Prefix::UNC(..) | std::path::Prefix::VerbatimUNC(..) | std::path::Prefix::DeviceNS(..) | std::path::Prefix::Device(..)
        )
    {
        return Err(format!("automation path rejected: network and device paths are not allowed: {}", path.display()));
    }
    Ok(())
}

/// Is `path` (resolved) `root` or inside it, component by component? Exactly, on Windows too: what
/// exists has its real case from [`resolve`], and a folder that doesn't exist yet may be a different
/// one from the root where a parent folder is case sensitive.
fn inside(path: &Path, root: &Path) -> bool {
    let mut path = path.components();
    root.components().all(|r| path.next() == Some(r))
}

/// A Windows device name (`CON`, `NUL`, `COM1`… also with an extension), which names no file.
fn is_device_name(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component).trim_end_matches(' ').to_ascii_uppercase();
    let numbered =
        |prefix: &str| stem.strip_prefix(prefix).is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"));
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$") || numbered("COM") || numbered("LPT")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name that differs in case is another name until the file system says otherwise.
    #[test]
    fn names_are_compared_exactly() {
        let root = Path::new("/data/Photos");
        assert!(inside(Path::new("/data/Photos/new/file"), root));
        assert!(!inside(Path::new("/data/PHOTOS/file"), root));
        assert!(!inside(Path::new("/DATA/Photos/file"), root));
        if cfg!(windows) {
            let root = Path::new(r"C:\data\Photos");
            assert!(inside(Path::new(r"c:\data\Photos\file"), root), "a drive letter is one drive in either case");
            assert!(!inside(Path::new(r"C:\data\PHOTOS\file"), root));
        }
    }

    /// In a case-sensitive folder a missing `PHOTOS` is not the root `Photos`, and creating it would leave the root.
    #[cfg(windows)]
    #[test]
    fn a_case_sensitive_folder_keeps_a_differently_cased_name_out() {
        let base = std::env::temp_dir().join(format!("lc-access-case-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let set = std::process::Command::new("fsutil").args(["file", "setCaseSensitiveInfo"]).arg(&base).arg("enable").output().unwrap();
        if !set.status.success() {
            println!("CASE-SENSITIVE-FOLDER-UNAVAILABLE: {set:?}");
            return;
        }
        println!("CASE-SENSITIVE-FOLDER-USED");
        std::fs::create_dir(base.join("Photos")).unwrap();
        let roots = AutomationRoots::new(None, Some(&base.join("Photos"))).unwrap().unwrap();
        assert_eq!(roots.check(base.join("Photos/new.txt"), Access::Write), Ok(()));
        let e = roots.check(base.join("PHOTOS/new.txt"), Access::Write).unwrap_err();
        assert!(e.contains("outside the write root"), "{e}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A junction is a link too: removing or renaming one acts on it where it stands.
    #[cfg(windows)]
    #[test]
    fn a_junction_is_written_where_it_stands() {
        let base = std::env::temp_dir().join(format!("lc-access-junction-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (root, outside) = (base.join("root"), base.join("outside"));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let junction = outside.join("backdir");
        let made = std::process::Command::new("cmd").arg("/c").arg("mklink").arg("/J").arg(&junction).arg(root.join("sub")).output().unwrap();
        assert!(made.status.success(), "{made:?}");
        let roots = AutomationRoots::new(Some(&root), Some(&root)).unwrap().unwrap();
        let e = roots.check(&junction, Access::Write).unwrap_err();
        assert!(e.contains("outside the write root"), "{e}");
        assert!(roots.check(&junction, Access::Read).is_ok(), "read follows it");
        assert!(roots.check(junction.join("new"), Access::Write).is_ok(), "a file under it really is in the root");
        let _ = std::fs::remove_dir(&junction);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Writing to a link replaces, renames or removes the link where it stands, whatever it points to.
    #[cfg(unix)]
    #[test]
    fn a_link_is_written_where_it_stands() {
        let base = std::env::temp_dir().join(format!("lc-access-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (root, outside) = (base.join("root"), base.join("outside"));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("file"), b"x").unwrap();
        let link = |target: &Path, at: &Path| std::os::unix::fs::symlink(target, at).unwrap();
        link(&root.join("file"), &outside.join("back"));
        link(&root.join("sub"), &outside.join("backdir"));
        link(&root, &outside.join("root-alias"));
        link(&root.join("file"), &root.join("near"));
        let roots = AutomationRoots::new(Some(&root), Some(&root)).unwrap().unwrap();

        assert!(roots.check_container(outside.join("root-alias")).is_ok(), "new files go into the resolved destination folder");
        assert!(
            roots.check(outside.join("root-alias"), Access::Write).unwrap_err().contains("outside the write root"),
            "changing the link itself is still refused"
        );

        for refused in ["outside/back", "outside/backdir", "outside/backdir/", "outside/backdir/."] {
            let e = roots.check(base.join(refused), Access::Write).unwrap_err();
            assert!(e.contains("outside the write root"), "{refused}: {e}");
            assert!(roots.check(base.join(refused), Access::Read).is_ok(), "{refused}: read follows the link");
        }
        // inside a linked folder a file really is in the root; a link in the root stands in the root
        for allowed in ["outside/backdir/new", "root/near", "root/file", "root/new", "root"] {
            assert!(roots.check(base.join(allowed), Access::Write).is_ok(), "{allowed}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A fresh folder holding `in` (read root), `out` (write root) and `outside`.
    fn folders(name: &str) -> (PathBuf, Arc<AutomationRoots>) {
        let base = std::env::temp_dir().join(format!("lc-file-access-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for d in ["in/sub", "out", "outside", "in2"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        std::fs::write(base.join("in/a.jpg"), "x").unwrap();
        std::fs::write(base.join("outside/secret.jpg"), "x").unwrap();
        let roots = AutomationRoots::new(Some(&base.join("in")), Some(&base.join("out"))).unwrap().unwrap();
        (base, roots)
    }

    /// A path as text.
    fn s(p: PathBuf) -> String {
        p.to_string_lossy().into_owned()
    }

    /// Paths inside the roots pass and others dont.
    #[test]
    fn paths_inside_the_roots_pass_and_others_dont() {
        let (base, roots) = folders("inside");
        assert_eq!(roots.check(s(base.join("in/a.jpg")), Access::Read), Ok(()));
        assert_eq!(roots.check(s(base.join("in")), Access::Read), Ok(()), "the root itself");
        assert_eq!(roots.check(s(base.join("in/sub/new.jpg")), Access::Read), Ok(()), "missing files are checked by their folder");
        assert_eq!(roots.check(s(base.join("out/new/deeper/x.png")), Access::Write), Ok(()), "and by their nearest folder");
        let e = roots.check(s(base.join("outside/secret.jpg")), Access::Read).unwrap_err();
        assert!(e.starts_with("automation path rejected: outside the read root"), "{e}");
        // Read and write access.
        let e = roots.check(s(base.join("in/a.jpg")), Access::Write).unwrap_err();
        assert!(e.contains("outside the write root"), "{e}");
        assert_eq!(roots.check(s(base.join("out/x.png")), Access::Read), Ok(()), "reads are permitted under write root");
        let _ = std::fs::remove_dir_all(base);
    }

    /// A sibling with the same prefix is outside.
    #[test]
    fn a_sibling_with_the_same_prefix_is_outside() {
        let (base, roots) = folders("prefix");
        std::fs::write(base.join("in2/b.jpg"), "x").unwrap();
        assert!(roots.check(s(base.join("in2/b.jpg")), Access::Read).is_err(), "in2 is not inside in");
        assert!(roots.check(format!("{}2/c.jpg", s(base.join("in"))), Access::Read).is_err());
        let _ = std::fs::remove_dir_all(base);
    }

    /// Parent traversal is resolved or refused.
    #[test]
    fn parent_traversal_is_resolved_or_refused() {
        let (base, roots) = folders("dotdot");
        let sep = std::path::MAIN_SEPARATOR;
        let up = format!("{}{sep}..{sep}outside{sep}secret.jpg", s(base.join("in")));
        assert!(roots.check(&up, Access::Read).unwrap_err().contains("outside the read root"), "`..` out of the root");
        let back = format!("{}{sep}..{sep}a.jpg", s(base.join("in/sub")));
        assert_eq!(roots.check(&back, Access::Read), Ok(()), "`..` that stays inside");
        let missing = format!("{}{sep}nope{sep}..{sep}..{sep}outside{sep}x.jpg", s(base.join("out")));
        let e = roots.check(&missing, Access::Write).unwrap_err();
        assert!(e.contains("parent traversal below a folder that doesn't exist") || e.contains("outside the write root"), "{e}");
        let _ = std::fs::remove_dir_all(base);
    }

    /// Relative paths resolve against the working directory.
    #[test]
    fn relative_paths_resolve_against_the_working_directory() {
        let (base, roots) = folders("relative");
        assert!(roots.check("a.jpg", Access::Read).unwrap_err().contains("outside the read root"));
        assert!(roots.check("", Access::Read).unwrap_err().contains("path is empty"));
        let _ = std::fs::remove_dir_all(base);
    }

    /// A missing root is an error and none is no confinement.
    #[test]
    fn a_missing_root_is_an_error_and_none_is_no_confinement() {
        let (base, _) = folders("missing");
        let e = AutomationRoots::new(Some(&base.join("nope")), None).unwrap_err();
        assert!(e.starts_with("cannot open the automation read root"), "{e}");
        let e = AutomationRoots::new(None, Some(&base.join("in/a.jpg"))).unwrap_err();
        assert!(
            e.starts_with("cannot open the automation write root")
                && e.ends_with(&std::io::Error::from(std::io::ErrorKind::NotADirectory).to_string()),
            "{e}"
        );
        assert!(AutomationRoots::new(Some(Path::new("")), None).unwrap_err().starts_with("cannot open the automation read root ``"));
        assert!(AutomationRoots::new(None, None).unwrap().is_none());
        // One root alone grants only its own access.
        let read_only = AutomationRoots::new(Some(&base.join("in")), None).unwrap().unwrap();
        let e = read_only.check(s(base.join("in/x.jpg")), Access::Write).unwrap_err();
        assert!(e.contains("write authority is absent"), "{e}");
        let _ = std::fs::remove_dir_all(base);
    }

    /// Links are followed before comparing.
    #[test]
    fn links_are_followed_before_comparing() {
        let (base, roots) = folders("links");
        #[cfg(unix)]
        let made = |target: &Path, link: &Path| std::os::unix::fs::symlink(target, link).is_ok();
        #[cfg(windows)]
        let made = |target: &Path, link: &Path| {
            let made =
                if target.is_dir() { std::os::windows::fs::symlink_dir(target, link) } else { std::os::windows::fs::symlink_file(target, link) };
            made.is_ok()
        };
        #[cfg(not(any(unix, windows)))]
        let made = |_: &Path, _: &Path| false;
        if !made(&base.join("outside/secret.jpg"), &base.join("in/leak.jpg")) {
            eprintln!("skipped: symbolic links can't be made here");
            let _ = std::fs::remove_dir_all(base);
            return;
        }
        assert!(roots.check(s(base.join("in/leak.jpg")), Access::Read).unwrap_err().contains("outside the read root"), "a file link out");
        assert!(made(&base.join("outside"), &base.join("out/away")));
        let e = roots.check(s(base.join("out/away/new.png")), Access::Write).unwrap_err();
        assert!(e.contains("outside the write root"), "a folder link out: {e}");
        assert!(made(&base.join("outside/gone.jpg"), &base.join("out/dangling.png")));
        let e = roots.check(s(base.join("out/dangling.png")), Access::Write).unwrap_err();
        assert!(e.contains("a link that leads nowhere"), "writing would create the file outside: {e}");
        // A link to a folder inside the root stays inside.
        assert!(made(&base.join("in/sub"), &base.join("in/alias")));
        assert_eq!(roots.check(s(base.join("in/alias/x.jpg")), Access::Read), Ok(()));
        // A root given through a link is the folder it leads to.
        assert!(made(&base.join("in"), &base.join("in-link")));
        let linked = AutomationRoots::new(Some(&base.join("in-link")), None).unwrap().unwrap();
        assert_eq!(linked.check(s(base.join("in/a.jpg")), Access::Read), Ok(()));
        let _ = std::fs::remove_dir_all(base);
    }

    /// On windows a path takes its real case and devices are refused.
    #[cfg(windows)]
    #[test]
    fn on_windows_a_path_takes_its_real_case_and_devices_are_refused() {
        let (base, roots) = folders("case");
        let upper = s(base.join("IN/a.jpg"));
        assert_eq!(roots.check(&upper, Access::Read), Ok(()), "{upper}");
        assert_eq!(roots.check(s(base.join("In/Sub/New.jpg")), Access::Read), Ok(()));
        let e = roots.check(s(base.join("out/NUL")), Access::Write).unwrap_err();
        assert!(e.contains("reserved device names"), "{e}");
        assert!(roots.check(s(base.join("out/com1.txt")), Access::Write).is_err());
        assert_eq!(roots.check(s(base.join("out/console.txt")), Access::Write), Ok(()));
        let _ = std::fs::remove_dir_all(base);
    }

    /// A file with another name is refused, inside the root or not: the other name may lie outside it.
    /// A file with one name, a missing name, and a directory that has child folders all pass.
    #[test]
    fn a_file_with_hard_links_is_refused() {
        let base = std::env::temp_dir().join(format!("lc-file-access-hard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (root, outside) = (base.join("root"), base.join("outside"));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let file = root.join("a.jpg");
        std::fs::write(&file, b"x").unwrap();
        let roots = AutomationRoots::new(Some(&root), Some(&root)).unwrap().unwrap();
        assert_eq!(roots.check(&file, Access::Read), Ok(()), "one name");
        assert_eq!(roots.check(&file, Access::Write), Ok(()));
        assert_eq!(roots.check(&root, Access::Read), Ok(()), "a directory's link count is not extra names");
        assert_eq!(roots.check(root.join("new.jpg"), Access::Write), Ok(()), "a name that does not exist yet");
        let other = outside.join("also.jpg");
        match std::fs::hard_link(&file, &other) {
            Ok(()) => {}
            Err(e) => {
                let _ = std::fs::remove_dir_all(&base);
                lightcraft_testkit::skip!(System, "no hard links in {}: {e}", base.display());
                return;
            }
        }
        for access in [Access::Read, Access::Write] {
            let e = roots.check(&file, access).unwrap_err();
            assert!(e.contains("more than one name"), "{access:?}: {e}");
        }
        std::fs::remove_file(&other).unwrap();
        assert_eq!(roots.check(&file, Access::Read), Ok(()), "the extra name is gone");
        // Both names are inside the root. The other one is still invisible to a path check.
        std::fs::hard_link(&file, root.join("b.jpg")).unwrap();
        let e = roots.check(&file, Access::Read).unwrap_err();
        assert!(e.contains("more than one name"), "{e}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Scopes nest and stay on their thread.
    #[test]
    fn scopes_nest_and_stay_on_their_thread() {
        let (base, roots) = folders("scopes");
        let outside = s(base.join("outside/secret.jpg"));
        assert_eq!(check_read(&outside), Ok(()), "nothing is confined by default");
        confine(Some(&roots), || {
            assert!(check_read(&outside).is_err());
            assert_eq!(unconfined(|| check_read(&outside)), Ok(()));
            assert!(check_read(&outside).is_err(), "restored after unconfined");
            let o = outside.clone();
            let there = std::thread::spawn(move || check_read(&o).is_ok()).join().unwrap();
            assert!(there, "another thread is not confined by this thread's scope");
        });
        assert_eq!(check_read(&outside), Ok(()), "restored after confine");
        let r = std::panic::catch_unwind(|| confine(Some(&roots), || panic!("boom")));
        assert!(r.is_err());
        assert_eq!(check_read(&outside), Ok(()), "restored after a panic");
        assert_eq!(confine(None, || check_read(&outside)), Ok(()));
        let _ = std::fs::remove_dir_all(base);
    }

    /// A name that isn't Unicode is shown as it is.
    #[cfg(windows)]
    #[test]
    fn a_name_that_is_not_unicode_keeps_its_prefix() {
        use std::os::windows::ffi::OsStringExt;
        let wide: Vec<u16> = r"\\?\C:\root\".encode_utf16().chain([0xD800]).collect();
        let path = PathBuf::from(std::ffi::OsString::from_wide(&wide));
        assert_eq!(plain(path.clone()), path);
    }

    /// Device names.
    #[test]
    fn device_names() {
        for d in ["CON", "nul", "Com1", "LPT9.txt", "aux.jpg", "NUL ", "COM¹", "lpt³.txt"] {
            assert!(is_device_name(d), "{d}");
        }
        for f in ["console", "com10", "COM0", "lpt", "a.jpg", "nul-x"] {
            assert!(!is_device_name(f), "{f}");
        }
    }

    /// Network and device prefixes are refused without trying to resolve them.
    #[cfg(windows)]
    #[test]
    fn network_and_device_prefixes_are_refused_before_resolution() {
        for path in [r"\\server\share\file", r"\\?\UNC\server\share\file", r"\\.\NUL"] {
            assert!(reject_unsafe_prefix(Path::new(path)).is_err(), "{path}");
        }
        assert!(reject_unsafe_prefix(Path::new(r"C:\photos\file.jpg")).is_ok());
    }
}
