//! Delete from Disk (`photo.deleteFromDisk`): the photos' files go to the system trash
//! (the macOS Trash, the Windows Recycle Bin, the freedesktop trash on Linux and the BSDs) and the
//! photos leave the library.
//!
//! **Never a permanent delete.** A file is only ever *moved to the trash*, through
//! [`Session::trash`] (installed by [`Session::with_fs`]); without it (the web build, a session
//! without a file system) the command refuses, and nothing changes.
//!
//! What goes with a photo:
//! - its virtual copies (they share its file and would be left without one);
//! - its XMP sidecar: `<file>.xmp`, and `<stem>.xmp` unless another file in the folder shares the
//!   stem (`IMG_1.CR3` beside `IMG_1.JPG`: the stem sidecar may be the JPEG's too, so it stays).
//!
//! What stays:
//! - a file another photo still uses (a virtual copy deleted on its own): the photo only leaves
//!   the library, like Lightroom Classic's Remove;
//! - a photo whose file isn't there (a disk that isn't connected) or couldn't be moved to the
//!   trash: it stays in the library and the report says why, so no photo leaves the library
//!   while its file is still somewhere on disk.
//!
//! Callers outside the UI (CLI, MCP, the control channel) must pass `confirm: true`: the command
//! moves files, so it never runs on an id alone. The desktop app asks in its Delete dialog first.
//!
//! The trashing holds an activity row ("Deleting from disk", with progress; cancellable between
//! files: what was moved by then leaves the library, the rest stays).
//!
//! One undo step brings the catalog records back, pointing at their old paths: put the files back
//! from the trash and the photos find them again, with their edits.
//!
//! Sources: own design; the behaviour (Delete from Disk next to Remove) follows Lightroom
//! Classic's delete dialog as observed in the running application.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lcc_catalog::{Catalog, PhotoId, Source};
use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

/// Moves one file to the system trash (never deletes it outright). `Err` carries a message for
/// people.
pub type TrashFn = Arc<dyn Fn(&Path) -> std::result::Result<(), String> + Send + Sync>;

/// The system trash (the `trash` crate: Trash, Recycle Bin, freedesktop trash).
#[cfg(not(target_arch = "wasm32"))]
pub fn system_trash() -> TrashFn {
    Arc::new(|path: &Path| trash::delete(path).map_err(|e| e.to_string()))
}

/// One file to move to the trash and the photos that leave the library with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Group {
    pub photos: Vec<PhotoId>,
    pub original: PathBuf,
}

/// What Delete from Disk would do with a set of photos, from the catalog alone (no file is
/// touched: the sidecars are looked up while trashing, see [`sidecars_of`]).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub groups: Vec<Group>,
    /// Photos that leave the library while their file stays (other photos use it).
    pub keep_file: Vec<(PhotoId, String)>,
    /// Photos with no file on disk (demo scenes): they stay.
    pub no_file: Vec<PhotoId>,
}

/// The photos `targets` (unknown ids are ignored) plus their virtual copies, grouped by file.
pub(crate) fn plan(cat: &Catalog, targets: &[PhotoId]) -> Plan {
    let mut leaving: BTreeSet<PhotoId> = targets.iter().copied().filter(|id| cat.photo(*id).is_some()).collect();
    // virtual copies go with their master: they share its file
    let copies: Vec<PhotoId> = cat.photos().filter(|p| p.copy_of.is_some_and(|m| leaving.contains(&m))).map(|p| p.id).collect();
    leaving.extend(copies);

    let mut plan = Plan::default();
    let mut by_path: BTreeMap<String, Vec<PhotoId>> = BTreeMap::new();
    for id in &leaving {
        match cat.photo(*id).map(|p| &p.source) {
            Some(Source::File { path }) if !path.is_empty() => by_path.entry(path.clone()).or_default().push(*id),
            _ => plan.no_file.push(*id),
        }
    }
    // files a photo that stays (in the library, in Recently Deleted or seen in Local) still uses
    let shared: HashSet<&str> = cat
        .photos()
        .filter(|p| !leaving.contains(&p.id))
        .filter_map(|p| match &p.source {
            Source::File { path } if by_path.contains_key(path) => Some(path.as_str()),
            _ => None,
        })
        .collect();
    for (path, photos) in &by_path {
        if shared.contains(path.as_str()) {
            plan.keep_file.extend(photos.iter().map(|id| (*id, path.clone())));
            continue;
        }
        plan.groups.push(Group { photos: photos.clone(), original: PathBuf::from(path) });
    }
    plan
}

/// The originals of `plan` that go to the trash (the set [`sidecars_of`] takes).
fn originals(plan: &Plan) -> HashSet<PathBuf> {
    plan.groups.iter().map(|g| g.original.clone()).collect()
}

fn same_path(a: &Path, b: &Path) -> bool {
    // one file under two spellings on a case-insensitive volume (macOS, Windows): `test.xmp`
    // and `test.XMP` would otherwise be listed, and trashed, as two sidecars
    use crate::rename::MoveFs;
    crate::rename::RealFs.same_file(a, b)
}

/// The XMP sidecars of `original` that go with it: `<file>.xmp` always, `<stem>.xmp` only when no
/// other file in the folder (besides `leaving`, the originals going too) shares its stem. Only
/// existing files are listed.
pub(crate) fn sidecars_of(original: &Path, leaving: &HashSet<PathBuf>) -> Vec<PathBuf> {
    let (Some(name), Some(dir)) = (original.file_name(), original.parent()) else { return Vec::new() };
    let listing = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
    // list the folder rather than probe `.xmp` / `.XMP` / `.Xmp`: on a case-insensitive volume every
    // probe finds the one file, while the listing names each file once, as it is spelled on disk
    let Ok(entries) = std::fs::read_dir(listing) else { return Vec::new() };
    let names: Vec<String> = entries.flatten().filter(|e| e.path().is_file()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    let (whole, stem) = sidecar_names(&name.to_string_lossy(), &names);
    let mut v: Vec<PathBuf> = whole.into_iter().map(|n| dir.join(n)).collect();
    if !stem.is_empty() && !stem_shared(original, leaving) {
        v.extend(stem.into_iter().map(|n| dir.join(n)));
    }
    v
}

/// The entries of `names` (one folder's file names, as listed) that are `original`'s sidecars,
/// compared without case: `<original>.xmp` and `<stem>.xmp`. Each listed file appears at most once.
fn sidecar_names<'a>(original: &str, names: &'a [String]) -> (Vec<&'a str>, Vec<&'a str>) {
    let lower = original.to_lowercase();
    let whole = format!("{lower}.xmp");
    let stem = Path::new(original).file_stem().map(|s| format!("{}.xmp", s.to_string_lossy().to_lowercase()));
    let (mut w, mut st) = (Vec::new(), Vec::new());
    for n in names {
        let l = n.to_lowercase();
        if l == whole {
            w.push(n.as_str());
        } else if stem.as_deref() == Some(l.as_str()) && l != lower {
            st.push(n.as_str());
        }
    }
    (w, st)
}

/// Another file in `original`'s folder, not a sidecar and not among `leaving`, has the same stem
/// (compared without case). A folder that can't be listed counts as shared: the sidecar stays.
fn stem_shared(original: &Path, leaving: &HashSet<PathBuf>) -> bool {
    let Some(stem) = original.file_stem().map(|s| s.to_string_lossy().to_lowercase()) else { return true };
    let dir = match original.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let Ok(entries) = std::fs::read_dir(dir) else { return true };
    entries.flatten().map(|e| e.path()).any(|p| {
        !same_path(&p, original)
            && !leaving.iter().any(|l| same_path(l, &p))
            && p.file_stem().is_some_and(|s| s.to_string_lossy().to_lowercase() == stem)
            && !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
    })
}

/// `photo.deleteFromDisk`: move the targets' files to the trash and take the photos out of the
/// library (one undo step). See the module docs.
pub(crate) fn delete_from_disk(s: &mut Session, p: &Value) -> Result<Value> {
    if p.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Err(crate::cmd::bad("photo.deleteFromDisk", "moves the photos' files to the system trash: pass confirm: true to go ahead"));
    }
    let Some(trash) = s.trash.clone() else {
        return Err(EngineError::Other("Delete from Disk isn't available here: this build can't move files to the system trash".into()));
    };
    let targets = s.targets(p)?;
    if targets.is_empty() {
        return Err(crate::cmd::bad("photo.deleteFromDisk", "no photos to delete"));
    }
    let plan = plan(&s.catalog, &targets);
    let leaving = originals(&plan);
    let shown = |path: &Path| path.to_string_lossy().to_string();
    let (mut remove, mut trashed, mut kept, mut failed) = (Vec::new(), Vec::new(), Vec::new(), Vec::<(String, String)>::new());
    let task = s.activity.start("deleteFromDisk", "Deleting from disk", crate::activity::Cancel::Yes);
    let total = u64::try_from(plan.groups.len()).unwrap_or(u64::MAX);
    let mut cancelled = 0usize;
    for (i, g) in plan.groups.iter().enumerate() {
        if task.is_cancelled() {
            cancelled = cancelled.saturating_add(1);
            continue;
        }
        task.progress(u64::try_from(i).unwrap_or(u64::MAX), total);
        if let Some(name) = g.original.file_name() {
            task.detail(&name.to_string_lossy());
        }
        // never remove a photo whose file is still somewhere: an unplugged disk, a folder
        match std::fs::symlink_metadata(&g.original) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                failed.push((shown(&g.original), "the file isn't there (is its disk connected?)".into()));
                continue;
            }
            Err(e) => {
                failed.push((shown(&g.original), e.to_string()));
                continue;
            }
            Ok(m) if m.is_dir() => {
                failed.push((shown(&g.original), "this is a folder, not a photo's file".into()));
                continue;
            }
            Ok(_) => {}
        }
        // looked up before the original goes, so the folder still shows which stems are shared
        let sidecars = sidecars_of(&g.original, &leaving);
        if let Err(e) = trash(&g.original) {
            failed.push((shown(&g.original), e));
            continue;
        }
        trashed.push(shown(&g.original));
        remove.extend(g.photos.iter().copied());
        for sc in &sidecars {
            // (on a case-insensitive disk `a.xmp` and `a.XMP` are one file, gone with the first)
            if !sc.is_file() {
                continue;
            }
            match trash(sc) {
                Ok(()) => trashed.push(shown(sc)),
                Err(e) => failed.push((shown(sc), e)),
            }
        }
    }
    drop(task);
    for (id, path) in &plan.keep_file {
        remove.push(*id);
        kept.push(path.clone());
    }
    for id in &plan.no_file {
        let name = s.catalog.photo(*id).map(|p| p.file_name.clone()).unwrap_or_default();
        failed.push((name, "not a file on disk".into()));
    }
    if remove.is_empty() {
        let why = if cancelled > 0 {
            "cancelled".to_string()
        } else {
            failed.first().map(|(path, e)| format!("{path}: {e}")).unwrap_or_else(|| "no photos to delete".into())
        };
        return Err(EngineError::Other(format!("Nothing was moved to the system trash. {why}")));
    }
    let op = s.catalog.delete_photos_permanently_ops(&remove);
    s.commit("Delete from Disk", op)?;
    kept.dedup();
    Ok(json!({"deleted": remove.len(), "trashed": trashed, "kept": kept, "failed": failed, "cancelled": cancelled}))
}

/// The photos among `targets` (with their virtual copies) whose file Delete from Disk would move
/// to the trash, and how many files that is (originals only): what the confirmation shows. Works
/// from the catalog alone, so it stays quick for thousands of photos.
pub fn preview(cat: &Catalog, targets: &[PhotoId]) -> (usize, usize) {
    let plan = plan(cat, targets);
    let photos = plan.groups.iter().map(|g| g.photos.len()).sum::<usize>() + plan.keep_file.len();
    (photos, plan.groups.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn add_test_photo(s: &mut Session, path: &Path) -> PhotoId {
        let id = s.catalog.alloc_photo_id();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let op = lcc_catalog::Op::AddPhoto {
            photo: Box::new(lcc_catalog::Photo::new(
                id,
                lcc_catalog::Source::File { path: path.to_string_lossy().to_string() },
                &name,
                "JPEG",
                100,
                100,
                "2026-01-01",
            )),
        };
        s.commit("Add test photo", op).unwrap();
        id
    }

    fn add_virtual_copy(s: &mut Session, master_id: PhotoId) -> PhotoId {
        let copy_id = s.catalog.alloc_photo_id();
        let master = s.catalog.photo(master_id).unwrap();
        let mut copy = (**master).clone();
        copy.id = copy_id;
        copy.copy_of = Some(master_id);
        copy.copy_name = Some("Copy 1".into());
        let op = lcc_catalog::Op::AddPhoto { photo: Box::new(copy) };
        s.commit("Add copy", op).unwrap();
        copy_id
    }

    struct TempDir {
        path: PathBuf,
    }
    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("lcc-test-delete-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn plan_single_file_and_preview() {
        let tmp = TempDir::new("single");
        let file = tmp.path.join("img.jpg");
        std::fs::write(&file, b"test").unwrap();

        let mut s = Session::new();
        let id = add_test_photo(&mut s, &file);

        let p = plan(&s.catalog, &[id]);
        assert_eq!(p.groups.len(), 1);
        assert_eq!(p.groups[0].photos, vec![id]);
        assert_eq!(p.groups[0].original, file);
        assert!(sidecars_of(&file, &originals(&p)).is_empty());
        assert!(p.keep_file.is_empty());
        assert!(p.no_file.is_empty());

        assert_eq!(preview(&s.catalog, &[id]), (1, 1));
    }

    #[test]
    fn plan_sidecars_shared_stem() {
        let tmp = TempDir::new("sidecars");
        let cr3 = tmp.path.join("test.cr3");
        let cr3_xmp = tmp.path.join("test.cr3.xmp");
        let stem_xmp = tmp.path.join("test.xmp");
        let jpg = tmp.path.join("test.jpg");

        std::fs::write(&cr3, b"raw").unwrap();
        std::fs::write(&cr3_xmp, b"xmp").unwrap();
        std::fs::write(&stem_xmp, b"xmp").unwrap();

        let mut s = Session::new();
        let id_cr3 = add_test_photo(&mut s, &cr3);

        // Alone without test.jpg: both test.cr3.xmp and test.xmp are sidecars
        let p = plan(&s.catalog, &[id_cr3]);
        assert_eq!(sidecars_of(&cr3, &originals(&p)).len(), 2);

        // Now add test.jpg on disk and in catalog
        std::fs::write(&jpg, b"jpeg").unwrap();
        let id_jpg = add_test_photo(&mut s, &jpg);

        // Only deleting CR3: test.xmp is shared with test.jpg, so only test.cr3.xmp goes
        let p_cr3_only = plan(&s.catalog, &[id_cr3]);
        assert_eq!(sidecars_of(&cr3, &originals(&p_cr3_only)), vec![cr3_xmp.clone()]);

        // Deleting both CR3 and JPG: test.xmp is included because all owners leave
        let p_both = plan(&s.catalog, &[id_cr3, id_jpg]);
        let all_sidecars: Vec<PathBuf> = p_both.groups.iter().flat_map(|g| sidecars_of(&g.original, &originals(&p_both))).collect();
        assert!(all_sidecars.contains(&cr3_xmp));
        assert!(all_sidecars.contains(&stem_xmp));
    }

    /// Issue #159: a case-insensitive volume lists one sidecar once (probing `.xmp`, `.XMP`,
    /// `.Xmp` found it three times); a case-sensitive folder holding two spellings lists both.
    #[test]
    fn sidecar_names_follow_the_listing() {
        let one = vec!["test.cr3".to_string(), "test.cr3.XMP".to_string(), "Test.xmp".to_string(), "other.xmp".to_string()];
        assert_eq!(sidecar_names("test.cr3", &one), (vec!["test.cr3.XMP"], vec!["Test.xmp"]));
        let two = vec!["a.jpg".to_string(), "a.jpg.xmp".to_string(), "a.jpg.XMP".to_string()];
        assert_eq!(sidecar_names("a.jpg", &two).0.len(), 2);
        // an original that is itself an .xmp is never its own sidecar
        assert_eq!(sidecar_names("x.xmp", &["x.xmp".to_string()]), (vec![], vec![]));
    }

    #[test]
    fn sidecars_listed_once_on_disk() {
        let tmp = TempDir::new("once");
        let raw = tmp.path.join("img.cr3");
        std::fs::write(&raw, b"raw").unwrap();
        std::fs::write(tmp.path.join("img.cr3.xmp"), b"x").unwrap();
        assert_eq!(sidecars_of(&raw, &HashSet::new()), vec![tmp.path.join("img.cr3.xmp")]);
    }

    #[test]
    fn plan_virtual_copies() {
        let tmp = TempDir::new("vc");
        let file = tmp.path.join("img.jpg");
        std::fs::write(&file, b"test").unwrap();

        let mut s = Session::new();
        let master = add_test_photo(&mut s, &file);
        let copy = add_virtual_copy(&mut s, master);

        // Deleting master deletes both, groups 1 file
        let p_master = plan(&s.catalog, &[master]);
        assert_eq!(p_master.groups.len(), 1);
        assert_eq!(p_master.groups[0].photos.len(), 2);
        assert_eq!(preview(&s.catalog, &[master]), (2, 1));

        // Deleting copy alone leaves file on disk
        let p_copy = plan(&s.catalog, &[copy]);
        assert_eq!(p_copy.groups.len(), 0);
        assert_eq!(p_copy.keep_file, vec![(copy, file.to_string_lossy().to_string())]);
        assert_eq!(preview(&s.catalog, &[copy]), (1, 0));
    }

    #[test]
    fn delete_from_disk_command_trashes_and_undo_restores() {
        let tmp = TempDir::new("cmd");
        let file = tmp.path.join("photo.jpg");
        let xmp = tmp.path.join("photo.jpg.xmp");
        std::fs::write(&file, b"photo").unwrap();
        std::fs::write(&xmp, b"xmp").unwrap();

        let mut s = Session::new();
        let trashed = Arc::new(Mutex::new(Vec::new()));
        let t_clone = trashed.clone();
        s.trash = Some(Arc::new(move |p: &Path| {
            t_clone.lock().unwrap().push(p.to_path_buf());
            Ok(())
        }));

        let id = add_test_photo(&mut s, &file);
        s.selection = crate::Selection::single(id);

        let r = s.execute("photo.deleteFromDisk", &json!({"ids": [id.0], "confirm": true})).unwrap();
        assert_eq!(r["deleted"], 1);

        let trashed_paths = trashed.lock().unwrap().clone();
        assert!(trashed_paths.contains(&file));
        assert!(trashed_paths.contains(&xmp));
        assert!(s.catalog.photo(id).is_none(), "photo permanently removed from catalog");

        // Undo brings catalog record back
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(s.catalog.photo(id).is_some(), "undo restored catalog record");
    }

    #[test]
    fn delete_from_disk_virtual_copy_keeps_file() {
        let tmp = TempDir::new("vc-del");
        let file = tmp.path.join("photo.jpg");
        std::fs::write(&file, b"photo").unwrap();

        let mut s = Session::new();
        let trashed = Arc::new(Mutex::new(Vec::new()));
        let t_clone = trashed.clone();
        s.trash = Some(Arc::new(move |p: &Path| {
            t_clone.lock().unwrap().push(p.to_path_buf());
            Ok(())
        }));

        let master = add_test_photo(&mut s, &file);
        let copy = add_virtual_copy(&mut s, master);
        s.selection = crate::Selection::single(copy);

        let r = s.execute("photo.deleteFromDisk", &json!({"ids": [copy.0], "confirm": true})).unwrap();
        assert_eq!(r["deleted"], 1);
        assert!(trashed.lock().unwrap().is_empty(), "file was NOT trashed");
        assert!(s.catalog.photo(copy).is_none(), "copy was removed");
        assert!(s.catalog.photo(master).is_some(), "master remains");
    }

    #[test]
    fn delete_from_disk_missing_file_or_no_trash_fails_safely() {
        let mut s = Session::new();
        let id = s.catalog.alloc_photo_id();
        let op = lcc_catalog::Op::AddPhoto {
            photo: Box::new(lcc_catalog::Photo::new(
                id,
                lcc_catalog::Source::File { path: "/nonexistent/photo.jpg".into() },
                "photo.jpg",
                "JPEG",
                100,
                100,
                "2026-01-01",
            )),
        };
        s.commit("Add photo", op).unwrap();
        s.selection = crate::Selection::single(id);

        // Without trash
        let err = s.execute("photo.deleteFromDisk", &json!({"ids": [id.0], "confirm": true})).unwrap_err();
        assert!(err.to_string().contains("Delete from Disk isn't available here"));

        // With trash, but file doesn't exist
        s.trash = Some(Arc::new(|_| Ok(())));
        let err = s.execute("photo.deleteFromDisk", &json!({"ids": [id.0], "confirm": true})).unwrap_err();
        assert!(err.to_string().contains("Nothing was moved"));
        assert!(s.catalog.photo(id).is_some(), "photo stayed in library");
    }

    /// CLI, MCP and the control channel can't move files to the trash without saying so.
    #[test]
    fn delete_from_disk_requires_confirm() {
        let tmp = TempDir::new("confirm");
        let file = tmp.path.join("photo.jpg");
        std::fs::write(&file, b"photo").unwrap();
        let mut s = Session::new();
        let trashed = Arc::new(Mutex::new(Vec::new()));
        let t_clone = trashed.clone();
        s.trash = Some(Arc::new(move |p: &Path| {
            t_clone.lock().unwrap().push(p.to_path_buf());
            Ok(())
        }));
        let id = add_test_photo(&mut s, &file);
        s.selection = crate::Selection::single(id);
        for p in [json!({"ids": [id.0]}), json!({"ids": [id.0], "confirm": false}), json!({"ids": [id.0], "confirm": "yes"})] {
            assert!(s.execute("photo.deleteFromDisk", &p).is_err(), "{p}");
        }
        assert!(trashed.lock().unwrap().is_empty(), "nothing trashed without confirm");
        assert!(s.catalog.photo(id).is_some());
    }

    /// The trashing shows in the activity stack while it runs and leaves no row behind.
    #[test]
    fn delete_from_disk_shows_in_the_activity_stack() {
        let tmp = TempDir::new("activity");
        let file = tmp.path.join("photo.jpg");
        std::fs::write(&file, b"photo").unwrap();
        let mut s = Session::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (activity, seen2) = (s.activity.clone(), seen.clone());
        s.trash = Some(Arc::new(move |_: &Path| {
            seen2.lock().unwrap().extend(activity.list().into_iter().map(|t| t.kind));
            Ok(())
        }));
        let id = add_test_photo(&mut s, &file);
        s.selection = crate::Selection::single(id);
        s.execute("photo.deleteFromDisk", &json!({"ids": [id.0], "confirm": true})).unwrap();
        assert!(seen.lock().unwrap().contains(&"deleteFromDisk"), "a row while trashing");
        assert!(s.activity.list().is_empty(), "no row afterwards");
    }

    /// Cancelling from the stack stops between files: what was moved leaves the library, the rest stays.
    #[test]
    fn delete_from_disk_cancel_keeps_the_rest() {
        let tmp = TempDir::new("cancel");
        let (a, b) = (tmp.path.join("a.jpg"), tmp.path.join("b.jpg"));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let mut s = Session::new();
        let activity = s.activity.clone();
        s.trash = Some(Arc::new(move |_: &Path| {
            activity.cancel_all();
            Ok(())
        }));
        let (ia, ib) = (add_test_photo(&mut s, &a), add_test_photo(&mut s, &b));
        s.selection = crate::Selection::single(ia);
        let r = s.execute("photo.deleteFromDisk", &json!({"ids": [ia.0, ib.0], "confirm": true})).unwrap();
        assert_eq!(r["deleted"], 1);
        assert_eq!(r["cancelled"], 1);
        assert_eq!([ia, ib].iter().filter(|id| s.catalog.photo(**id).is_some()).count(), 1);
    }
}
