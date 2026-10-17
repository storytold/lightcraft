//! Moving photos' files to the Trash (`photo.trashFiles`): the one place where taking a photo
//! out of the library also touches its file. Delete, Delete Permanently, Empty Recently Deleted
//! and Remove Folder from Library never do.
//!
//! The tests use a Trash of their own (a folder in a scratch directory): none of them puts
//! anything in the real one.
//!
//! Scenarios:
//! - Given a photo with a file and an XMP sidecar, when its files are moved to the Trash, then
//!   the photo is out of the library (not in Recently Deleted either) and both files are in the
//!   Trash, as one undo step; undo puts the files back where they were and the photo back with
//!   its rating; redo moves them to the Trash again.
//! - Photos in Recently Deleted go the same way, one by one or all at once
//!   (`library.emptyRecentlyDeleted {files: "trash"}`).
//! - Delete, Delete Permanently, Empty Recently Deleted and Remove Folder from Library still
//!   leave every file where it is.
//! - Given two photos of the library that are one file (a copy), then the file stays while one
//!   of them is in the library and goes with the last.
//! - Given a raw and a JPEG that share one sidecar (`IMG.xmp`), then the sidecar stays with the
//!   one that stays.
//! - Given a disk without a Trash, then its photos stay in the library and are named in the
//!   answer; only `noTrash: "delete"` erases their files, and then undo brings the photos back
//!   as missing.
//! - A file that can't be moved to the Trash stays, with its photo, and the answer says why;
//!   the others go.
//! - A photo whose file is already gone leaves the library; nothing is moved.
//! - Undo when a file is no longer in the Trash, or when something else is now at its place:
//!   the photo comes back, nothing is overwritten, and the answer says what could not be put back.
//! - Photos only browsed in Local are not touched; photos without a file just leave the library.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::Session;
use crate::trash::{Bin, BinError};

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-trash-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("photos")).unwrap();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        Scratch(dir)
    }
    fn file(&self, rel: &str, contents: &str) -> String {
        let p = self.0.join("photos").join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, contents).unwrap();
        p.to_string_lossy().to_string()
    }
    fn at(&self, rel: &str) -> String {
        self.0.join("photos").join(rel).to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A Trash that is a folder: files moved to it get a numbered name. Paths containing `nas/` are
/// on a disk without a Trash; paths containing `locked` can't be moved.
struct FolderBin {
    dir: PathBuf,
    next: Mutex<u32>,
    /// While set, nothing can be moved to it.
    closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Bin for FolderBin {
    fn trash(&self, path: &Path) -> Result<Option<PathBuf>, BinError> {
        let name = path.to_string_lossy().to_string();
        if name.contains("nas/") {
            return Err(BinError::NoTrash);
        }
        if name.contains("locked") {
            return Err(BinError::Failed("the file is locked".into()));
        }
        if self.closed.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(BinError::Failed("the Trash is not there".into()));
        }
        let mut n = self.next.lock().unwrap();
        *n += 1;
        let to = self.dir.join(format!("{}-{}", *n, path.file_name().unwrap().to_string_lossy()));
        std::fs::rename(path, &to).map_err(|e| BinError::Failed(e.to_string()))?;
        Ok(Some(to))
    }
}

fn session(dir: &Scratch) -> Session {
    let mut s = Session::new();
    s.trash = Arc::new(FolderBin { dir: dir.0.join("bin"), next: Mutex::new(0), closed: Default::default() });
    s
}

fn add(s: &mut Session, path: &str) -> u64 {
    let id = s.catalog.alloc_photo_id();
    let name = Path::new(path).file_name().unwrap().to_string_lossy().to_string();
    let p = Photo::new(id, Source::File { path: path.into() }, &name, "JPEG", 60, 40, "2026-01-01T10:00:00");
    s.commit("Add Photo", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id.0
}

fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

fn in_bin(dir: &Scratch) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir.0.join("bin")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    v.sort();
    v
}

fn has(s: &Session, id: u64) -> bool {
    s.catalog.photo(PhotoId(id)).is_some()
}

/// (Photo commands want a selection to be enabled, whatever `ids` says.)
fn run(s: &mut Session, command: &str, ids: &[u64], more: Value) -> crate::Result<Value> {
    select(s, ids);
    let mut params = json!({"ids": ids});
    if let (Some(p), Some(m)) = (params.as_object_mut(), more.as_object()) {
        p.extend(m.clone());
    }
    s.execute(command, &params)
}

fn trash(s: &mut Session, ids: &[u64]) -> Value {
    run(s, "photo.trashFiles", ids, json!({})).unwrap()
}

fn select(s: &mut Session, ids: &[u64]) {
    s.selection.ids = ids.iter().map(|i| PhotoId(*i)).collect();
    s.selection.active = s.selection.ids.first().copied();
}

#[test]
fn a_photo_s_file_and_sidecar_go_to_the_trash_and_undo_puts_them_back() {
    let dir = Scratch::new("basic");
    let (a, b) = (dir.file("a.jpg", "A"), dir.file("b.jpg", "B"));
    let sidecar = dir.file("a.xmp", "<xmp/>");
    let mut s = session(&dir);
    let (ia, ib) = (add(&mut s, &a), add(&mut s, &b));
    select(&mut s, &[ia]);
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    let steps = s.undo.len();
    let r = trash(&mut s, &[ia]);
    assert_eq!((r["removed"].as_u64(), r["trashed"].as_u64()), (Some(1), Some(2)), "{r}");
    assert!(!has(&s, ia) && has(&s, ib), "out of the library, not in Recently Deleted");
    assert!(!exists(&a) && !exists(&sidecar) && exists(&b));
    assert_eq!(in_bin(&dir), vec!["1-a.jpg", "2-a.xmp"]);
    assert_eq!(s.undo.len(), steps + 1, "one step");
    assert_eq!(s.undo.last().unwrap().label, "Move Photo's Files to Trash");
    // undo: the files where they were, the photo as it was
    let undone = s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(undone["undone"], "Move Photo's Files to Trash");
    assert!(exists(&a) && exists(&sidecar) && in_bin(&dir).is_empty());
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "A");
    assert_eq!(s.catalog.photo(PhotoId(ia)).unwrap().rating, 4);
    assert!(!s.catalog.photo(PhotoId(ia)).unwrap().deleted);
    // redo: to the Trash again; undo once more brings them back again
    s.execute("edit.redo", &json!({})).unwrap();
    assert!(!has(&s, ia) && !exists(&a) && !exists(&sidecar));
    assert_eq!(in_bin(&dir).len(), 2);
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(has(&s, ia) && exists(&a) && exists(&sidecar));
}

#[test]
fn several_photos_go_as_one_step_and_the_selection_is_what_goes_without_ids() {
    let dir = Scratch::new("several");
    let paths: Vec<String> = (1..=3).map(|i| dir.file(&format!("p{i}.jpg"), "x")).collect();
    let mut s = session(&dir);
    let ids: Vec<u64> = paths.iter().map(|p| add(&mut s, p)).collect();
    select(&mut s, &[ids[0], ids[2]]);
    let steps = s.undo.len();
    let r = s.execute("photo.trashFiles", &json!({})).unwrap();
    assert_eq!(r["removed"], 2);
    assert!(!exists(&paths[0]) && exists(&paths[1]) && !exists(&paths[2]));
    assert_eq!(s.undo.len(), steps + 1);
    assert_eq!(s.undo.last().unwrap().label, "Move 2 Photos' Files to Trash");
    assert_eq!(s.selection.active, Some(PhotoId(ids[1])), "the photo left is the one selected");
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(paths.iter().all(|p| exists(p)));
}

#[test]
fn photos_in_recently_deleted_go_too_one_by_one_or_all_at_once() {
    let dir = Scratch::new("deleted");
    let paths: Vec<String> = (1..=3).map(|i| dir.file(&format!("p{i}.jpg"), "x")).collect();
    let mut s = session(&dir);
    let ids: Vec<u64> = paths.iter().map(|p| add(&mut s, p)).collect();
    run(&mut s, "photo.delete", &ids, json!({})).unwrap();
    assert!(paths.iter().all(|p| exists(p)), "Delete touches no file");
    trash(&mut s, &[ids[0]]);
    assert!(!has(&s, ids[0]) && !exists(&paths[0]) && exists(&paths[1]));
    let steps = s.undo.len();
    let r = s.execute("library.emptyRecentlyDeleted", &json!({"files": "trash"})).unwrap();
    assert_eq!((r["deleted"].as_u64(), r["trashed"].as_u64()), (Some(2), Some(2)), "{r}");
    assert!(!exists(&paths[1]) && !exists(&paths[2]));
    assert_eq!(s.undo.len(), steps + 1);
    // undo: back in Recently Deleted, files back
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(exists(&paths[1]) && s.catalog.photo(PhotoId(ids[1])).unwrap().deleted);
    assert!(s.execute("library.emptyRecentlyDeleted", &json!({"files": "shred"})).is_err());
}

#[test]
fn the_other_ways_out_of_the_library_still_leave_every_file() {
    let dir = Scratch::new("untouched");
    let paths: Vec<String> = (1..=4).map(|i| dir.file(&format!("d{i}/p.jpg"), "x")).collect();
    let mut s = session(&dir);
    let ids: Vec<u64> = paths.iter().map(|p| add(&mut s, p)).collect();
    run(&mut s, "photo.delete", &[ids[0], ids[1], ids[2]], json!({})).unwrap();
    run(&mut s, "photo.deletePermanently", &[ids[0]], json!({})).unwrap();
    s.execute("library.emptyRecentlyDeleted", &json!({})).unwrap();
    s.execute("library.removeFolder", &json!({"path": dir.at("d4")})).unwrap();
    assert!(paths.iter().all(|p| exists(p)), "no file was touched");
    assert!(in_bin(&dir).is_empty());
}

#[test]
fn a_file_two_photos_share_stays_until_the_last_of_them_goes() {
    let dir = Scratch::new("shared");
    let a = dir.file("a.jpg", "A");
    let mut s = session(&dir);
    let (one, two) = (add(&mut s, &a), add(&mut s, &a));
    let r = trash(&mut s, &[one]);
    assert_eq!((r["removed"].as_u64(), r["trashed"].as_u64(), r["shared"].as_u64()), (Some(1), Some(0), Some(1)), "{r}");
    assert!(!has(&s, one) && has(&s, two) && exists(&a), "the other photo still uses the file");
    let r = trash(&mut s, &[two]);
    assert_eq!(r["trashed"], 1);
    assert!(!exists(&a));
    // both at once: the file goes once
    s.execute("edit.undo", &json!({})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(has(&s, one) && has(&s, two) && exists(&a));
    let r = trash(&mut s, &[one, two]);
    assert_eq!((r["removed"].as_u64(), r["trashed"].as_u64()), (Some(2), Some(1)), "{r}");
    assert!(!exists(&a));
}

#[test]
fn a_sidecar_shared_by_a_raw_and_a_jpeg_stays_with_the_one_that_stays() {
    let dir = Scratch::new("pair");
    let (raw, jpg) = (dir.file("IMG_1.dng", "R"), dir.file("IMG_1.jpg", "J"));
    let (shared, own) = (dir.file("IMG_1.xmp", "<xmp/>"), dir.file("IMG_1.jpg.xmp", "<own/>"));
    let mut s = session(&dir);
    let (ir, ij) = (add(&mut s, &raw), add(&mut s, &jpg));
    trash(&mut s, &[ij]);
    assert!(!exists(&jpg) && !exists(&own), "the JPEG and the sidecar that is its alone");
    assert!(exists(&raw) && exists(&shared), "the raw keeps the sidecar they shared");
    trash(&mut s, &[ir]);
    assert!(!exists(&raw) && !exists(&shared), "with the last of them the sidecar goes");
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(exists(&raw) && exists(&shared));
}

#[test]
fn photos_on_a_disk_without_a_trash_stay_unless_their_files_may_be_erased() {
    let dir = Scratch::new("far-disk");
    let (near, far) = (dir.file("a.jpg", "A"), dir.file("nas/b.jpg", "B"));
    let far_sidecar = dir.file("nas/b.xmp", "<xmp/>");
    let mut s = session(&dir);
    let (ia, ib) = (add(&mut s, &near), add(&mut s, &far));
    let r = trash(&mut s, &[ia, ib]);
    assert_eq!(r["removed"], 1);
    assert_eq!(r["noTrash"], json!([ib]), "named, so the question can be asked about them");
    assert!(has(&s, ib) && exists(&far) && exists(&far_sidecar), "it stays, file and all");
    assert!(!has(&s, ia) && !exists(&near));
    // asked for by name only: erased for good
    assert!(run(&mut s, "photo.trashFiles", &[ib], json!({"noTrash": "shred"})).is_err());
    assert!(has(&s, ib) && exists(&far));
    let steps = s.undo.len();
    let r = run(&mut s, "photo.trashFiles", &[ib], json!({"noTrash": "delete"})).unwrap();
    assert_eq!((r["removed"].as_u64(), r["erased"].as_u64(), r["trashed"].as_u64()), (Some(1), Some(1), Some(0)), "{r}");
    assert!(!has(&s, ib) && !exists(&far));
    // only what was asked about is erased: the photo's own file. Its sidecar stays, and is counted
    assert!(exists(&far_sidecar));
    assert_eq!(r["sidecarsLeft"], 1, "{r}");
    assert_eq!(in_bin(&dir), vec!["1-a.jpg"], "nothing of it is in the Trash");
    assert_eq!(s.undo.len(), steps + 1);
    assert_eq!(s.undo.last().unwrap().label, "Erase Photo's Files");
    // undo brings the photo back (its edits and metadata are kept); the file stays gone
    let undone = s.execute("edit.undo", &json!({})).unwrap()["undone"].as_str().unwrap().to_string();
    assert!(has(&s, ib) && !exists(&far));
    assert!(undone.contains("erased"), "undo says the file is not coming back: {undone}");
}

#[test]
fn a_file_that_can_t_go_to_the_trash_stays_with_its_photo_and_the_answer_says_why() {
    let dir = Scratch::new("stuck");
    let (ok, locked) = (dir.file("a.jpg", "A"), dir.file("locked.jpg", "L"));
    let mut s = session(&dir);
    let (ia, il) = (add(&mut s, &ok), add(&mut s, &locked));
    let r = trash(&mut s, &[il, ia]);
    assert_eq!(r["removed"], 1);
    assert_eq!(r["failed"][0]["id"], il);
    assert!(r["failed"][0]["why"].as_str().unwrap().contains("locked"), "{r}");
    assert!(has(&s, il) && exists(&locked) && !has(&s, ia) && !exists(&ok));
    // nothing could go: an error that says why, and nothing recorded to undo
    let steps = s.undo.len();
    let e = run(&mut s, "photo.trashFiles", &[il], json!({})).unwrap_err().to_string();
    assert!(e.contains("locked"), "{e}");
    assert_eq!(s.undo.len(), steps);
    // …and erasing is never the answer to that: `noTrash` is for disks without a Trash only
    assert!(run(&mut s, "photo.trashFiles", &[il], json!({"noTrash": "delete"})).is_err());
    assert!(exists(&locked));
}

#[test]
fn a_photo_whose_file_is_gone_leaves_the_library_and_nothing_is_moved() {
    let dir = Scratch::new("missing");
    let mut s = session(&dir);
    let gone = add(&mut s, &dir.at("gone.jpg"));
    let r = trash(&mut s, &[gone]);
    assert_eq!((r["removed"].as_u64(), r["trashed"].as_u64(), r["missing"].as_u64()), (Some(1), Some(0), Some(1)), "{r}");
    assert!(!has(&s, gone) && in_bin(&dir).is_empty());
}

#[test]
fn undo_never_overwrites_and_says_what_could_not_be_put_back() {
    let dir = Scratch::new("undo");
    let (a, b, c) = (dir.file("a.jpg", "A"), dir.file("b.jpg", "B"), dir.file("c.jpg", "C"));
    let mut s = session(&dir);
    let ids = [add(&mut s, &a), add(&mut s, &b), add(&mut s, &c)];
    trash(&mut s, &ids);
    // the Trash was emptied of one, and something else is now where another was
    std::fs::remove_file(dir.0.join("bin/1-a.jpg")).unwrap();
    std::fs::write(&b, "NEW").unwrap();
    let undone = s.execute("edit.undo", &json!({})).unwrap()["undone"].as_str().unwrap().to_string();
    assert!(ids.iter().all(|id| has(&s, *id)), "the photos are back in the library");
    assert!(exists(&c) && !exists(&a));
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "NEW", "what is there now is not replaced");
    assert!(dir.0.join("bin/2-b.jpg").exists(), "and the trashed file is still in the Trash");
    assert!(undone.contains("2 files could not be put back"), "{undone}");
    // redo takes away what undo put back (and the photos); what never came back is not touched
    s.execute("edit.redo", &json!({})).unwrap();
    assert!(ids.iter().all(|id| !has(&s, *id)));
    assert!(!exists(&c));
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "NEW", "a file that is not the photo's own stays");
}

#[test]
fn browsed_photos_are_left_alone_and_photos_without_a_file_just_leave() {
    let dir = Scratch::new("local");
    let local_path = dir.file("local.jpg", "L");
    let mut s = session(&dir);
    let id = s.catalog.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: local_path.clone() }, "local.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    p.local = true;
    s.commit("Browse", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let e = run(&mut s, "photo.trashFiles", &[id.0], json!({})).unwrap_err().to_string();
    assert!(e.contains("My Photos"), "{e}");
    assert!(exists(&local_path) && has(&s, id.0));
    // a photo that is no file (a demo scene)
    let demo = s.catalog.alloc_photo_id();
    let p = Photo::new(demo, Source::Demo { scene: 1 }, "demo.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    s.commit("Add Photo", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let r = trash(&mut s, &[demo.0]);
    assert_eq!((r["removed"].as_u64(), r["trashed"].as_u64()), (Some(1), Some(0)));
    // nothing named, nothing selected; a photo that is not there
    select(&mut s, &[id.0]);
    let e = s.execute("photo.trashFiles", &json!({"ids": []})).unwrap_err().to_string();
    assert!(e.contains("no photos"), "{e}");
    let e = s.execute("photo.trashFiles", &json!({"ids": [4242]})).unwrap_err().to_string();
    assert!(e.contains("4242"), "{e}");
    assert!(s.execute("photo.trashFiles", &json!({"ids": "all"})).is_err());
    assert!(exists(&local_path));
}

// ---- What an independent review found

#[test]
fn a_sidecar_stays_while_anything_beside_it_or_in_the_library_may_use_it() {
    let dir = Scratch::new("sidecar-owner");
    // a video beside the JPEG, which the library does not import: the stem sidecar may be its
    let (jpg, _mov, shared) = (dir.file("clip/IMG_1.jpg", "J"), dir.file("clip/IMG_1.mov", "V"), dir.file("clip/IMG_1.xmp", "<xmp/>"));
    // a raw of the library whose file is not there now (its disk is away), beside another JPEG
    let (jpg2, shared2) = (dir.file("away/IMG_2.jpg", "J"), dir.file("away/IMG_2.xmp", "<xmp/>"));
    let mut s = session(&dir);
    let (ij, ij2) = (add(&mut s, &jpg), add(&mut s, &jpg2));
    let raw_gone = add(&mut s, &dir.at("away/IMG_2.dng"));
    let r = trash(&mut s, &[ij, ij2]);
    assert_eq!((r["removed"].as_u64(), r["trashed"].as_u64()), (Some(2), Some(2)), "the two JPEGs, no sidecar: {r}");
    assert!(exists(&shared), "the video's sidecar stays");
    assert!(exists(&shared2), "the sidecar of a photo that stays in the library stays, though its file is away");
    assert!(has(&s, raw_gone));
    assert_eq!(r["sidecarsLeft"], 2, "{r}");
}

#[test]
fn photos_whose_folder_can_t_be_reached_stay_in_the_library() {
    let dir = Scratch::new("offline");
    let near = dir.file("a.jpg", "A");
    let mut s = session(&dir);
    let (ia, far) = (add(&mut s, &near), add(&mut s, &dir.at("unplugged/b.jpg")));
    // the folder is not there (a disk that is away): nothing says the file is gone for good
    let r = trash(&mut s, &[ia, far]);
    assert_eq!((r["removed"].as_u64(), r["missing"].as_u64()), (Some(1), Some(0)), "{r}");
    assert!(has(&s, far), "it stays in the library");
    assert!(r["failed"][0]["why"].as_str().unwrap().contains("can't be reached"), "{r}");
    // a file gone from a folder that is there is missing, as before
    std::fs::create_dir_all(dir.0.join("photos/unplugged")).unwrap();
    let r = trash(&mut s, &[far]);
    assert_eq!((r["removed"].as_u64(), r["missing"].as_u64()), (Some(1), Some(1)), "{r}");
}

#[test]
fn a_path_that_is_a_folder_is_never_moved() {
    let dir = Scratch::new("folder-path");
    let inside = dir.file("album/keep.jpg", "K");
    let mut s = session(&dir);
    let id = add(&mut s, &dir.at("album"));
    let e = run(&mut s, "photo.trashFiles", &[id], json!({})).unwrap_err().to_string();
    assert!(e.contains("not a file"), "{e}");
    assert!(exists(&inside) && has(&s, id) && in_bin(&dir).is_empty());
}

#[test]
fn a_file_a_browsed_photo_or_another_spelling_of_its_path_still_uses_stays() {
    let dir = Scratch::new("same-file");
    let a = dir.file("a.jpg", "A");
    let mut s = session(&dir);
    let lib = add(&mut s, &a);
    // the same file again, browsed in Local, and named in the same call
    let local = s.catalog.alloc_photo_id();
    let mut p = Photo::new(local, Source::File { path: a.clone() }, "a.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    p.local = true;
    s.commit("Browse", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let r = run(&mut s, "photo.trashFiles", &[lib, local.0, local.0], json!({})).unwrap();
    assert_eq!(
        (r["removed"].as_u64(), r["trashed"].as_u64(), r["shared"].as_u64(), r["skipped"].as_u64()),
        (Some(1), Some(0), Some(1), Some(1)),
        "{r}"
    );
    assert!(exists(&a) && has(&s, local.0) && !has(&s, lib));
    // the same file reached through another folder name (a link to its folder) in a photo that
    // stays: one file, whatever the two paths say
    #[cfg(unix)]
    {
        let b = dir.file("real/b.jpg", "B");
        std::os::unix::fs::symlink(dir.0.join("photos/real"), dir.0.join("photos/link")).unwrap();
        let (one, _other) = (add(&mut s, &b), add(&mut s, &dir.at("link/b.jpg")));
        let r = trash(&mut s, &[one]);
        assert_eq!((r["trashed"].as_u64(), r["shared"].as_u64()), (Some(0), Some(1)), "{r}");
        assert!(exists(&b));
    }
}

#[test]
fn a_file_that_did_not_come_back_is_tried_again_at_the_next_undo() {
    let dir = Scratch::new("retry");
    let (a, b) = (dir.file("a.jpg", "A"), dir.file("b.jpg", "B"));
    let mut s = session(&dir);
    let ids = [add(&mut s, &a), add(&mut s, &b)];
    trash(&mut s, &ids);
    // something is in a's place for now
    std::fs::write(&a, "IN THE WAY").unwrap();
    let undone = s.execute("edit.undo", &json!({})).unwrap()["undone"].as_str().unwrap().to_string();
    assert!(undone.contains("1 file could not be put back"), "{undone}");
    assert!(exists(&b));
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "IN THE WAY", "redo leaves what is not the photo's");
    // the way is clear now: the next undo brings a back too
    std::fs::remove_file(&a).unwrap();
    let undone = s.execute("edit.undo", &json!({})).unwrap()["undone"].as_str().unwrap().to_string();
    assert_eq!(undone, "Move 2 Photos' Files to Trash", "nothing left to say");
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "A");
    assert!(exists(&b) && in_bin(&dir).is_empty());
}

#[test]
fn a_trash_step_is_never_folded_into_another_undo_step() {
    let dir = Scratch::new("merge");
    let a = dir.file("a.jpg", "A");
    let mut s = session(&dir);
    let id = add(&mut s, &a);
    let other = add(&mut s, &dir.file("b.jpg", "B"));
    trash(&mut s, &[id]);
    select(&mut s, &[other]);
    s.execute("photo.rate", &json!({"rating": 1})).unwrap();
    let steps = s.undo.len();
    // (what a background import does with the steps made while it ran)
    s.merge_undo(2, "Add Photos");
    assert_eq!(s.undo.len(), steps, "left as they were");
    s.execute("edit.undo", &json!({})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(has(&s, id) && exists(&a), "undo still brings the file back");
}

#[test]
fn redo_never_erases_and_duplicate_ids_are_one_photo() {
    let dir = Scratch::new("redo-far-disk");
    let far = dir.file("nas/b.jpg", "B");
    let mut s = session(&dir);
    let id = add(&mut s, &far);
    run(&mut s, "photo.trashFiles", &[id, id], json!({"noTrash": "delete"})).unwrap();
    assert!(!exists(&far));
    s.execute("edit.undo", &json!({})).unwrap();
    // the person put a file there again (from a backup)
    std::fs::write(&far, "RESTORED").unwrap();
    s.execute("edit.redo", &json!({})).unwrap();
    assert!(!has(&s, id));
    assert_eq!(std::fs::read_to_string(&far).unwrap(), "RESTORED", "redo touches no file it did not put back itself");
}

// ---- What the check of those fixes found

#[test]
fn a_sidecar_named_for_the_whole_file_stays_when_it_may_be_another_file_s() {
    let dir = Scratch::new("whole-name");
    // IMG_1.heic.xmp is IMG_1.heic's by its whole name and IMG_1.heic.jpg's by its stem
    let (heic, jpg, xmp) = (dir.file("a/IMG_1.heic", "H"), dir.file("a/IMG_1.heic.jpg", "J"), dir.file("a/IMG_1.heic.xmp", "<xmp/>"));
    let mut s = session(&dir);
    let (ih, ij) = (add(&mut s, &heic), add(&mut s, &jpg));
    let r = trash(&mut s, &[ih]);
    assert_eq!((r["trashed"].as_u64(), r["sidecarsLeft"].as_u64()), (Some(1), Some(1)), "{r}");
    assert!(exists(&xmp) && exists(&jpg) && !exists(&heic));
    s.execute("edit.undo", &json!({})).unwrap();
    // …and the other way round
    let r = trash(&mut s, &[ij]);
    assert_eq!((r["trashed"].as_u64(), r["sidecarsLeft"].as_u64()), (Some(1), Some(1)), "{r}");
    assert!(exists(&xmp) && exists(&heic) && !exists(&jpg));
    // with the last of them it goes
    let r = trash(&mut s, &[ih]);
    assert_eq!(r["trashed"], 2, "{r}");
    assert!(!exists(&xmp));
}

#[cfg(unix)]
#[test]
fn a_file_that_can_t_be_looked_at_is_not_taken_for_gone() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Scratch::new("no-access");
    let a = dir.file("closed/a.jpg", "A");
    let mut s = session(&dir);
    let id = add(&mut s, &a);
    let folder = dir.0.join("photos/closed");
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o000)).unwrap();
    let r = run(&mut s, "photo.trashFiles", &[id], json!({}));
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    let e = r.unwrap_err().to_string();
    assert!(e.contains("can't be read"), "{e}");
    assert!(has(&s, id) && exists(&a), "the photo stays: nothing says its file is gone");
}

#[test]
fn a_file_a_redo_could_not_take_is_taken_at_the_next_redo() {
    let dir = Scratch::new("redo-again");
    let a = dir.file("a.jpg", "A");
    let mut s = Session::new();
    let closed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    s.trash = Arc::new(FolderBin { dir: dir.0.join("bin"), next: Mutex::new(0), closed: closed.clone() });
    let id = add(&mut s, &a);
    trash(&mut s, &[id]);
    s.execute("edit.undo", &json!({})).unwrap();
    // the Trash can't take it at this redo: it says so, and the file stays
    closed.store(true, std::sync::atomic::Ordering::Relaxed);
    let redone = s.execute("edit.redo", &json!({})).unwrap()["redone"].as_str().unwrap().to_string();
    assert!(redone.contains("1 file could not be moved to the Trash"), "{redone}");
    assert!(exists(&a) && !has(&s, id));
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(exists(&a) && has(&s, id));
    // the next redo takes it, as the first one meant to
    closed.store(false, std::sync::atomic::Ordering::Relaxed);
    let redone = s.execute("edit.redo", &json!({})).unwrap()["redone"].as_str().unwrap().to_string();
    assert_eq!(redone, "Move Photo's Files to Trash");
    assert!(!exists(&a) && !has(&s, id));
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(exists(&a) && in_bin(&dir).is_empty());
}
