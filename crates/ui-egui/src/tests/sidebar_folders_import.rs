//! LR-BEHAV-EMPTY-STATES / LRC-LIB-LIBFOLDERS: a Folders section with nothing in it offers the two
//! ways to fill it (Import Photos…, Import from Folder…) as rows, and its header has a + with the
//! same two items, as Albums and Keywords have theirs, whether or not Folders is empty.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_engine::Session;
use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(30);

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-ui-folders-import-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
    fn path(&self, rel: &str) -> String {
        self.0.join(rel).to_string_lossy().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_png(path: &str) {
    let (w, h) = (24usize, 16usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 9) as u8, (i / w * 13) as u8, 7, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// The demo library was imported from no folder, so its Folders section is empty.
fn app(session: Session, services: Services) -> Headless {
    let app = LightcraftApp::new(session, Services { png: None, local_places: Some(Vec::new()), ..services });
    let mut h = Headless::new(app, [1400.0, 1400.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    h
}

/// A library with one photo from a folder, so Folders lists it.
fn imported_from_a_folder() -> Session {
    let mut s = Session::new();
    let p = Photo::new(PhotoId(1), Source::File { path: "/pics/trip/a.jpg".into() }, "a.jpg", "JPEG", 60, 40, "2026-01-01T00:00:00");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn click(h: &mut Headless, id: &str) {
    h.step();
    h.step();
    assert_eq!(h.request("ui.clickWidget", json!({"id": id}), T)["ok"], true, "{id}");
    for _ in 0..4 {
        h.step();
    }
}

/// A folder picker that counts its asks and answers with `answer`.
fn folder_picker(answer: Option<String>) -> (crate::services::PickFolder, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let seen = asked.clone();
    (
        Box::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            answer.clone()
        }),
        asked,
    )
}

/// A file picker that counts its asks and answers with `answer`.
fn file_picker(answer: Vec<String>) -> (crate::services::PickFiles, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let seen = asked.clone();
    (
        Box::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            answer.clone()
        }),
        asked,
    )
}

fn import_review_open(h: &Headless) -> bool {
    matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))
}

/// Given an empty Folders section on a host with file and folder pickers, when the sidebar is
/// drawn, then "No imported folders yet" is followed by an Import Photos… row and an Import from
/// Folder… row.
#[test]
fn an_empty_folders_section_offers_both_imports() {
    let (pick_folder, _) = folder_picker(None);
    let (pick_files, _) = file_picker(Vec::new());
    let h = app(Session::with_demo(), Services { pick_folder: Some(pick_folder), pick_files: Some(pick_files), ..Default::default() });
    let rect = |id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"));
    let (hint, photos, folder) = (rect("sidebarEmpty:folders"), rect("source:folders:import"), rect("source:folders:importFolder"));
    assert!(hint.bottom() <= photos.top() && photos.bottom() <= folder.top(), "{hint:?} {photos:?} {folder:?}");
    assert!(folder.bottom() <= rect("sidebarSection:keywords").top(), "the rows belong to Folders, above the next section");
}

/// Given a host with no folder picker (the web build), when Folders is empty, then only Import
/// Photos… is offered.
#[test]
fn without_a_folder_picker_only_import_photos_is_offered() {
    let (pick_files, _) = file_picker(Vec::new());
    let h = app(Session::with_demo(), Services { pick_files: Some(pick_files), ..Default::default() });
    assert!(has(&h, "source:folders:import"));
    assert!(!has(&h, "source:folders:importFolder"));
}

/// Given an empty Folders section, when Import Photos… is clicked, then the file picker is asked
/// (as File ▸ Import Photos… does) and the photos it answers with open the import review.
#[test]
fn the_import_photos_row_runs_file_add_photos() {
    let dir = Scratch::new("photos");
    let photo = dir.path("a.png");
    write_png(&photo);
    let (pick_files, asked) = file_picker(vec![photo]);
    let mut h = app(Session::with_demo().with_fs(), Services { pick_files: Some(pick_files), ..Default::default() });
    click(&mut h, "source:folders:import");
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert!(h.step_until(T, import_review_open), "the review opens: {:?}", h.app.ui.dialog.is_some());
}

/// Given an empty Folders section, when Import from Folder… is clicked, then the folder picker is
/// asked and the folder it answers with opens the import review.
#[test]
fn the_import_from_folder_row_runs_file_add_folder() {
    let dir = Scratch::new("folder");
    write_png(&dir.path("trip/a.png"));
    let (pick_folder, asked) = folder_picker(Some(dir.path("trip")));
    let mut h = app(Session::with_demo().with_fs(), Services { pick_folder: Some(pick_folder), ..Default::default() });
    click(&mut h, "source:folders:importFolder");
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert!(h.step_until(T, import_review_open), "the review opens: {:?}", h.app.ui.dialog.is_some());
}

/// Given a library with imported folders, when Folders is open, then it lists them with no Import
/// rows (they are for the empty section), and its header still has the +.
#[test]
fn a_folders_section_with_folders_keeps_the_plus_and_drops_the_rows() {
    let (pick_files, _) = file_picker(Vec::new());
    let h = app(imported_from_a_folder(), Services { pick_files: Some(pick_files), ..Default::default() });
    assert!(!has(&h, "sidebarEmpty:folders"), "Folders lists a folder");
    assert!(!has(&h, "source:folders:import") && !has(&h, "source:folders:importFolder"));
    assert!(has(&h, "icon:foldersImport"), "the header's +");
}

/// Given an empty Folders section, then its header has the + too, and it opens a menu with the
/// same two items; picking Import Photos… asks for the photos.
#[test]
fn the_folders_plus_menu_offers_the_same_two_imports() {
    let (pick_folder, _) = folder_picker(None);
    let (pick_files, asked) = file_picker(Vec::new());
    let mut h = app(Session::with_demo(), Services { pick_folder: Some(pick_folder), pick_files: Some(pick_files), ..Default::default() });
    assert!(has(&h, "icon:foldersImport"), "the header's +");
    click(&mut h, "icon:foldersImport");
    assert!(has(&h, "button:foldersImportPhotos") && has(&h, "button:foldersImportFolder"));
    click(&mut h, "button:foldersImportPhotos");
    assert_eq!(asked.load(Ordering::SeqCst), 1);
}

/// Given a host with no folder picker, the + menu has only Import Photos….
#[test]
fn the_folders_plus_menu_has_no_folder_item_without_a_folder_picker() {
    let (pick_files, _) = file_picker(Vec::new());
    let mut h = app(Session::with_demo(), Services { pick_files: Some(pick_files), ..Default::default() });
    click(&mut h, "icon:foldersImport");
    assert!(has(&h, "button:foldersImportPhotos"));
    assert!(!has(&h, "button:foldersImportFolder"));
}

/// Given a host with no dialog to pick files with (neither `pick_files` nor `picker`), when
/// Folders is empty, then Import Photos… is not offered, as a row or in the + menu, and a header
/// with nothing to open has no +; a host that has only a background `picker` still gets it.
#[test]
fn import_photos_is_not_offered_where_nothing_can_pick_files() {
    let h = app(Session::with_demo(), Services::default());
    assert!(has(&h, "sidebarEmpty:folders"), "Folders still says it is empty");
    assert!(!has(&h, "source:folders:import") && !has(&h, "source:folders:importFolder"), "no rows");
    assert!(!has(&h, "icon:foldersImport"), "no + that would open an empty menu");
    let picker: crate::pick::Picker = Box::new(|_| Err("no dialog in this test".into()));
    let h = app(Session::with_demo(), Services { picker: Some(picker), ..Default::default() });
    assert!(has(&h, "source:folders:import") && has(&h, "icon:foldersImport"), "a background picker is a way to pick files");
}

/// Given an empty Folders section, when Import from Folder… is picked in the + menu, then the
/// folder picker is asked and the folder it answers with opens the import review (the menu item
/// runs `file.addFolder`, not just the row).
#[test]
fn the_plus_menus_import_from_folder_item_runs_file_add_folder() {
    let dir = Scratch::new("menu-folder");
    write_png(&dir.path("trip/a.png"));
    let (pick_folder, asked) = folder_picker(Some(dir.path("trip")));
    let (pick_files, files_asked) = file_picker(Vec::new());
    let mut h = app(Session::with_demo().with_fs(), Services { pick_folder: Some(pick_folder), pick_files: Some(pick_files), ..Default::default() });
    click(&mut h, "icon:foldersImport");
    click(&mut h, "button:foldersImportFolder");
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert_eq!(files_asked.load(Ordering::SeqCst), 0, "not the photos command");
    assert!(h.step_until(T, import_review_open), "the review opens: {:?}", h.app.ui.dialog.is_some());
}

/// Given a Lightroom catalog import that is running, when Folders is empty, then Import from
/// Folder… is not offered (as a row or in the + menu) while Import Photos… still is, and picking
/// Import Photos… is refused with a toast that says why, with no file dialog and no import review.
#[test]
fn while_a_lightroom_import_runs_only_import_photos_is_offered_and_it_is_refused_with_a_toast() {
    let (pick_folder, folder_asked) = folder_picker(None);
    let (pick_files, files_asked) = file_picker(Vec::new());
    let mut h = app(Session::with_demo(), Services { pick_folder: Some(pick_folder), pick_files: Some(pick_files), ..Default::default() });
    assert!(has(&h, "source:folders:importFolder"), "offered before the Lightroom import starts");
    let task = crate::lightroom_import::stall_import(&mut h.app);
    h.step();
    h.step();
    assert!(!has(&h, "source:folders:importFolder"), "no folder row while a Lightroom import runs");
    assert!(has(&h, "source:folders:import"));
    click(&mut h, "icon:foldersImport");
    assert!(has(&h, "button:foldersImportPhotos") && !has(&h, "button:foldersImportFolder"), "the + menu agrees");
    click(&mut h, "button:foldersImportPhotos");
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains("Lightroom catalog import"), "the refusal is a toast: {toast:?}");
    assert_eq!((files_asked.load(Ordering::SeqCst), folder_asked.load(Ordering::SeqCst)), (0, 0), "no dialog was asked");
    assert!(!import_review_open(&h));
    drop(task);
}

/// Only Local's header explains itself on hover: the other sections' titles say what they hold.
#[test]
fn only_locals_header_has_a_hover_line() {
    use crate::sidebar::SidebarSection;
    let with_hint: Vec<_> = SidebarSection::ALL.into_iter().filter(|s| s.header_hint().is_some()).collect();
    assert_eq!(with_hint, [SidebarSection::Local]);
}
