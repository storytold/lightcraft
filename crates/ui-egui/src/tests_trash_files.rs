//! Move Files to Trash… in the app (see `lightcraft_engine::trash`): the one way out of the
//! library that takes a photo's file along, and it always asks first.
//!
//! The tests use a Trash of their own (a folder in a scratch directory).
//!
//! Scenarios:
//! - Given a photo selected, when Photo ▸ Move Files to Trash… is chosen, then a question says
//!   what will happen and nothing has happened yet; Cancel leaves photo and file; OK takes the
//!   photo out of the library and its file to the Trash, and says so; Undo brings both back.
//! - The same from a photo's menu in the grid, for a photo in the library and for one in
//!   Recently Deleted; from Recently Deleted all of them can go at once.
//! - With "Confirm before deleting" on, the Delete question offers Move Files to Trash… as a
//!   second answer, which leads to the same question.
//! - Given photos on a disk without a Trash, then after the first question a second one asks
//!   whether their files may be erased for good; Cancel there leaves them in the library with
//!   their files; OK erases them.
//! - A file that can't be moved stays with its photo, and a notice says why.
//! - Delete itself, with or without its question, still moves photos to Recently Deleted and
//!   touches no file.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_engine::trash::{Bin, BinError};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::panels::trash_files::TrashAsk;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-ui-trash-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        Scratch(dir)
    }
    fn file(&self, rel: &str) -> String {
        let p = self.0.join("photos").join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "x").unwrap();
        p.to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A Trash that is a folder. Paths containing `nas/` are on a disk without a Trash; paths
/// containing `stuck` can't be moved.
struct FolderBin {
    dir: PathBuf,
    next: Mutex<u32>,
}

impl Bin for FolderBin {
    fn trash(&self, path: &Path) -> Result<Option<PathBuf>, BinError> {
        let name = path.to_string_lossy().to_string();
        if name.contains("nas/") {
            return Err(BinError::NoTrash);
        }
        if name.contains("stuck") {
            return Err(BinError::Failed("the file is in use".into()));
        }
        let mut n = self.next.lock().unwrap();
        *n += 1;
        let to = self.dir.join(format!("{}-{}", *n, path.file_name().unwrap().to_string_lossy()));
        std::fs::rename(path, &to).map_err(|e| BinError::Failed(e.to_string()))?;
        Ok(Some(to))
    }
}

/// The app showing a library of the files `rel` (made in `dir`), the first one selected.
fn app(dir: &Scratch, rel: &[&str]) -> (Headless, Vec<u64>, Vec<String>) {
    let mut session = lightcraft_engine::Session::new();
    session.trash = Arc::new(FolderBin { dir: dir.0.join("bin"), next: Mutex::new(0) });
    let (mut ids, mut paths) = (Vec::new(), Vec::new());
    for r in rel {
        let path = dir.file(r);
        let id = session.catalog.alloc_photo_id();
        let name = Path::new(&path).file_name().unwrap().to_string_lossy().to_string();
        let p = Photo::new(id, Source::File { path: path.clone() }, &name, "JPEG", 60, 40, "2026-01-01T10:00:00");
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        ids.push(id.0);
        paths.push(path);
    }
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    ask(&mut h, "ui.set", json!({"view": "photoGrid"}));
    run(&mut h, "library.select", json!({"ids": [ids[0]]}));
    (h, ids, paths)
}

fn ask(h: &mut Headless, method: &str, params: Value) -> Value {
    let r = h.request(method, params.clone(), T);
    assert_eq!(r["ok"], true, "{method} {params}: {r}");
    h.settle(SETTLE);
    r
}

fn run(h: &mut Headless, command: &str, params: Value) -> Value {
    ask(h, "engine.execute", json!({"command": command, "params": params}))["result"].clone()
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn click(h: &mut Headless, id: &str) {
    ask(h, "ui.clickWidget", json!({"id": id}));
}

fn in_library(h: &Headless, id: u64) -> bool {
    h.app.session.catalog.photo(PhotoId(id)).is_some()
}

fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

/// Let the Erase question's button become available (by the app's own clock).
fn wait_for_erase(h: &mut Headless) {
    let Some(Dialog::TrashFiles(TrashAsk::Erase { since, .. })) = h.app.ui.dialog.clone() else { panic!("no Erase question: {:?}", h.app.ui.dialog) };
    let ready = since + crate::panels::trash_files::ERASE_SETTLE_SECONDS + 0.05;
    assert!(h.step_until(T, |h| h.app.now() >= ready), "the app's clock moves on");
    h.step();
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default()
}

#[test]
fn move_files_to_trash_asks_first_and_undo_brings_photo_and_file_back() {
    let dir = Scratch::new("ask");
    let (mut h, ids, paths) = app(&dir, &["a.jpg", "b.jpg"]);
    run(&mut h, "dialog.trashFiles", json!({}));
    assert_eq!(h.app.ui.dialog, Some(Dialog::TrashFiles(TrashAsk::Trash { ids: vec![ids[0]], emptying: false, since: None })));
    assert!(in_library(&h, ids[0]) && exists(&paths[0]), "asking does nothing");
    ask(&mut h, "ui.dialog.cancel", json!({}));
    assert!(in_library(&h, ids[0]) && exists(&paths[0]), "Cancel leaves photo and file");
    run(&mut h, "dialog.trashFiles", json!({}));
    click(&mut h, "button:dialogOk");
    assert!(!in_library(&h, ids[0]) && !exists(&paths[0]), "out of the library, the file in the Trash");
    assert!(in_library(&h, ids[1]) && exists(&paths[1]));
    assert_eq!(h.app.ui.dialog, None);
    assert!(toast(&h).contains("Trash"), "it says what it did: {:?}", toast(&h));
    run(&mut h, "edit.undo", json!({}));
    assert!(in_library(&h, ids[0]) && exists(&paths[0]), "Undo brings both back");
}

#[test]
fn a_photo_s_menu_in_the_grid_offers_it_in_the_library_and_in_recently_deleted() {
    let dir = Scratch::new("menu");
    let (mut h, ids, paths) = app(&dir, &["a.jpg", "b.jpg", "c.jpg"]);
    ask(&mut h, "ui.clickWidget", json!({"id": format!("thumb:{}", ids[0]), "button": "right"}));
    assert!(has(&h, "button:contextTrashFiles"), "beside Delete Photo");
    click(&mut h, "button:contextTrashFiles");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::TrashFiles(TrashAsk::Trash { .. }))));
    ask(&mut h, "ui.dialog.cancel", json!({}));
    // in Recently Deleted: one photo, then all that are left
    run(&mut h, "library.select", json!({"ids": [ids[0], ids[1], ids[2]]}));
    run(&mut h, "photo.delete", json!({}));
    assert!(paths.iter().all(|p| exists(p)), "Delete touches no file");
    run(&mut h, "library.source", json!({"kind": "recentlyDeleted"}));
    run(&mut h, "library.select", json!({"ids": [ids[0]]}));
    ask(&mut h, "ui.clickWidget", json!({"id": format!("thumb:{}", ids[0]), "button": "right"}));
    click(&mut h, "button:contextTrashFiles");
    click(&mut h, "button:dialogOk");
    assert!(!in_library(&h, ids[0]) && !exists(&paths[0]) && exists(&paths[1]));
    ask(&mut h, "ui.clickWidget", json!({"id": format!("thumb:{}", ids[1]), "button": "right"}));
    click(&mut h, "button:contextEmptyTrashFiles");
    assert_eq!(h.app.ui.dialog, Some(Dialog::TrashFiles(TrashAsk::Trash { ids: vec![ids[1], ids[2]], emptying: true, since: None })));
    let undo = h.app.session.undo.len();
    click(&mut h, "button:dialogOk");
    assert!(!exists(&paths[1]) && !exists(&paths[2]) && !in_library(&h, ids[2]));
    assert_eq!(h.app.session.undo.len(), undo + 1, "one step");
}

#[test]
fn the_delete_question_offers_move_files_to_trash_as_a_second_answer() {
    let dir = Scratch::new("confirm");
    let (mut h, ids, paths) = app(&dir, &["a.jpg", "b.jpg"]);
    h.app.ui.settings.confirm_delete = true;
    ask(&mut h, "ui.key", json!({"key": "delete"}));
    assert_eq!(h.app.ui.dialog, Some(Dialog::ConfirmDelete { count: 1 }));
    assert!(has(&h, "button:dialogTrashFiles"));
    click(&mut h, "button:dialogTrashFiles");
    assert!(
        matches!(&h.app.ui.dialog, Some(Dialog::TrashFiles(TrashAsk::Trash { ids: asked, since: Some(_), .. })) if *asked == vec![ids[0]]),
        "the other question: {:?}",
        h.app.ui.dialog
    );
    // a second click of a double-click does not answer it: its button waits a moment
    click(&mut h, "button:dialogOk");
    assert!(in_library(&h, ids[0]) && exists(&paths[0]), "still nothing done");
    ask(&mut h, "ui.dialog.cancel", json!({}));
    // Delete's own answer is what it was: to Recently Deleted, no file touched
    ask(&mut h, "ui.key", json!({"key": "delete"}));
    click(&mut h, "button:dialogOk");
    assert!(h.app.session.catalog.photo(PhotoId(ids[0])).unwrap().deleted && exists(&paths[0]));
    // …and without the question too
    h.app.ui.settings.confirm_delete = false;
    run(&mut h, "library.select", json!({"ids": [ids[1]]}));
    ask(&mut h, "ui.key", json!({"key": "delete"}));
    assert!(h.app.session.catalog.photo(PhotoId(ids[1])).unwrap().deleted && exists(&paths[1]));
    assert_eq!(h.app.ui.dialog, None);
}

#[test]
fn photos_on_a_disk_without_a_trash_get_a_second_question_before_anything_is_erased() {
    let dir = Scratch::new("far-disk");
    let (mut h, ids, paths) = app(&dir, &["a.jpg", "nas/b.jpg", "nas/c.jpg"]);
    run(&mut h, "library.select", json!({"ids": ids}));
    run(&mut h, "dialog.trashFiles", json!({}));
    click(&mut h, "button:dialogOk");
    assert!(!in_library(&h, ids[0]) && !exists(&paths[0]), "the one that could go went");
    assert!(
        matches!(&h.app.ui.dialog, Some(Dialog::TrashFiles(TrashAsk::Erase { ids: asked, .. })) if *asked == vec![ids[1], ids[2]]),
        "the others are asked about"
    );
    // a second click meant for the first question does not answer this one
    click(&mut h, "button:dialogOk");
    assert!(in_library(&h, ids[1]) && exists(&paths[1]), "its button waits a moment");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::TrashFiles(TrashAsk::Erase { .. }))));
    assert!(in_library(&h, ids[1]) && exists(&paths[1]) && exists(&paths[2]), "nothing of them is touched yet");
    ask(&mut h, "ui.dialog.cancel", json!({}));
    assert!(in_library(&h, ids[1]) && in_library(&h, ids[2]) && exists(&paths[1]) && exists(&paths[2]), "Cancel leaves them");
    // again, and this time yes
    run(&mut h, "library.select", json!({"ids": [ids[1], ids[2]]}));
    run(&mut h, "dialog.trashFiles", json!({}));
    click(&mut h, "button:dialogOk");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::TrashFiles(TrashAsk::Erase { .. }))));
    wait_for_erase(&mut h);
    click(&mut h, "button:dialogOk");
    assert!(!in_library(&h, ids[1]) && !in_library(&h, ids[2]) && !exists(&paths[1]) && !exists(&paths[2]));
    assert_eq!(h.app.ui.dialog, None);
    assert!(toast(&h).to_lowercase().contains("erased"), "{:?}", toast(&h));
}

#[test]
fn a_file_that_can_t_be_moved_stays_with_its_photo_and_a_notice_says_why() {
    let dir = Scratch::new("in-use");
    let (mut h, ids, paths) = app(&dir, &["a.jpg", "stuck.jpg"]);
    run(&mut h, "library.select", json!({"ids": ids}));
    run(&mut h, "dialog.trashFiles", json!({}));
    click(&mut h, "button:dialogOk");
    assert!(!in_library(&h, ids[0]) && in_library(&h, ids[1]) && exists(&paths[1]));
    assert!(toast(&h).contains("in use"), "{:?}", toast(&h));
    // nothing could go at all: the refusal is the notice
    run(&mut h, "library.select", json!({"ids": [ids[1]]}));
    run(&mut h, "dialog.trashFiles", json!({}));
    click(&mut h, "button:dialogOk");
    assert!(in_library(&h, ids[1]) && exists(&paths[1]));
    assert!(toast(&h).contains("in use"), "{:?}", toast(&h));
    assert_eq!(h.app.ui.dialog, None);
}

#[test]
fn nothing_to_move_is_not_asked_about() {
    let dir = Scratch::new("nothing");
    let (mut h, _, _) = app(&dir, &["a.jpg"]);
    // nothing in Recently Deleted
    let r = h.request("engine.execute", json!({"command": "dialog.trashFiles", "params": {"recentlyDeleted": true}}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(h.app.ui.dialog, None);
}

/// A key can open the question; no key can be given to the command behind it.
#[test]
fn no_shortcut_can_move_files_without_the_question() {
    let ids: Vec<&str> = crate::shortcuts::bindable().iter().map(|b| b.id).collect();
    assert!(ids.contains(&"dialog.trashFiles") && !ids.contains(&"photo.trashFiles"), "only the question");
}

/// Move Files to Trash…: in the nine languages LightCraft had when this was written, none of the words stay
/// English. (A string with no entry in a language's catalog is shown as written, so nothing else fails when
/// one is forgotten. A language added later is not asked for them here: it shows them in English until it
/// is translated.)
#[test]
fn the_words_of_moving_files_to_the_trash_are_in_every_language() {
    let words = [
        "Move Files to Trash",
        "Move Files to Trash…",
        "Move All Files in Recently Deleted to Trash…",
        "Moving files to the Trash is not available here yet",
        "Recently Deleted is empty",
        "No photos of the library are selected",
        "Erase Files",
        "Move to Trash",
        "Move this photo's file to the Trash?",
        "The photos leave the library, also Recently Deleted. Their files go to the system Trash, with their XMP sidecars.",
        "Undo brings the photos back and moves the files back, as long as they are still in the Trash.",
        "This photo is on a disk that has no Trash. Erase its file for good?",
        "This cannot be undone: the files are not kept anywhere.",
        "Only these files are erased; XMP sidecars beside them stay.",
        "Cancel leaves these photos in the library and their files where they are.",
    ];
    use crate::i18n::Locale::{De, Es, Fr, Ja, PtBr, Ru, Uk, ZhHans, ZhHant};
    for language in [De, Es, Fr, Ja, PtBr, Ru, Uk, ZhHans, ZhHant] {
        for word in words {
            assert_ne!(language.tr(word), word, "{language:?} has no entry for {word:?}");
        }
    }
}
