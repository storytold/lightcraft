//! Photos dragged from the grid onto a Folders row move there on disk: the row lights up, the
//! dragged label says where they will go, a closed folder held under the pointer opens, and a
//! release asks first (unless the person said not to), then moves the files (one undo step).
//! Photos that can't go there, or are there already, say so and stay where they are. A folder's
//! menu does the same with Move Selected Photos Here, grayed out until photos are selected.

use std::path::{Path, PathBuf};

use egui::{Pos2, Rect};
use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

/// Folders `trip` and `home` in a fresh temporary folder, with a photo file in each.
fn folders(tag: &str) -> (lcc_testkit::TempDir, PathBuf, PathBuf) {
    let root = lcc_testkit::TempDir::new(&format!("lc-folder-drop-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    let (trip, home) = (root.join("trip"), root.join("home"));
    for (dir, file) in [(&trip, "beach.png"), (&home, "garden.png")] {
        std::fs::create_dir_all(dir).unwrap();
        write_png(&dir.join(file), [90, 120, 160]);
    }
    (root, trip, home)
}

/// A small PNG at `path`, its picture told apart from the others' by `rgb`.
fn write_png(path: &Path, rgb: [u8; 3]) {
    let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[rgb[0], rgb[1], rgb[2], 255]; 64] };
    let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(path, png).unwrap();
}

/// The photos at `paths` in the catalog (`local`: browsed, not in the library), in the grid with
/// the sidebar out, tall enough for the Folders section.
fn app(paths: &[(PathBuf, bool)]) -> Headless {
    app_with(paths, Services { png: None, ..Default::default() })
}

/// [`app`] on a host with `services` (a folder picker, say).
fn app_with(paths: &[(PathBuf, bool)], services: Services) -> Headless {
    use lcc_catalog::{Op, Photo, Source};
    let mut session = lcc_engine::Session::new().with_fs();
    for (path, local) in paths {
        let id = session.catalog.alloc_photo_id();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let mut p = Photo::new(id, Source::File { path: path.to_string_lossy().into() }, &name, "PNG", 8, 8, "2026-01-01T10:00:00");
        p.local = *local;
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let mut app = LccApp::new(session, services);
    // browsed photos dragged onto the library's folders: the Library tab's own view of a browsed
    // folder (Settings ▸ Import ▸ Import as a tab off; on, browsing shows the Import tab)
    app.ui.settings.import_as_tab = false;
    let mut h = Headless::new(app, [1300.0, 1600.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true, "sidebar": ["folders"]}), std::time::Duration::from_secs(10));
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..6 {
        h.step();
    }
    h
}

fn widget(h: &Headless, id: &str) -> Option<Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

/// Fold the sidebar's Local section. On Windows the temp folder lies under the home folder, so
/// browsing it opens Local at every folder on the way (all of `AppData\Local`'s), which pushes
/// the Folders rows below the window and a drag never reaches them.
fn fold_local(h: &mut Headless) {
    if !h.app.ui.sidebar_section_collapsed("local") {
        h.app.ui.toggle_sidebar_section("local");
    }
    h.step();
    h.step();
}

/// `dir` as the folder tree spells it: the catalog's folder paths use `/` (`C:/Users/…` on Windows).
fn tree_path(dir: &Path) -> String {
    dir.to_string_lossy().replace('\\', "/")
}

fn folder_row(h: &Headless, dir: &Path) -> Option<Rect> {
    widget(h, &format!("source:libfolder:{}", tree_path(dir)))
}

fn id_of(h: &Headless, file: &str) -> u64 {
    h.app.session.catalog.photos().find(|p| p.file_name == file).map(|p| p.id.0).unwrap()
}

fn press(h: &mut Headless, at: Pos2) {
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}

/// Move the pointer, pressed, from `from` to `to` over a few frames.
fn glide(h: &mut Headless, from: Pos2, to: Pos2) {
    for i in 1..=8 {
        h.app.synthetic.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
        h.step();
    }
}

fn release(h: &mut Headless, at: Pos2) {
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    h.step();
}

/// What the dragged label says this frame instead of the count, if a drop target set it.
fn hint(h: &Headless) -> Option<String> {
    h.view.ctx.data(|d| d.get_temp::<(u64, String)>(egui::Id::new("drag-photos-hint"))).map(|(_, text)| text)
}

/// Click a widget the way a person does (press and release on it).
fn click(h: &mut Headless, id: &str) {
    let at = widget(h, id).unwrap_or_else(|| panic!("no {id}")).center();
    press(h, at);
    release(h, at);
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast().map(|(t, _, _)| t.clone()).unwrap_or_default()
}

#[test]
fn photos_dropped_on_a_folder_move_there_on_disk() {
    let (root, trip, home) = folders("move");
    let mut h = app(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)]);
    let beach = id_of(&h, "beach.png");
    // the two folders sit in one that starts closed: only it is there to drop on
    let parent = folder_row(&h, &root).unwrap_or_else(|| {
        panic!("the folder holding both: {:?}", h.app.widgets.iter().filter(|(w, _)| w.starts_with("source:")).collect::<Vec<_>>())
    });
    assert!(folder_row(&h, &home).is_none(), "closed");
    // Given the beach photo dragged from the grid and held over the closed folder
    let from = widget(&h, &format!("thumb:{beach}")).unwrap().center();
    press(&mut h, from);
    glide(&mut h, from, parent.center());
    assert_eq!(h.app.ui.dragging_photos, Some(vec![beach]));
    for _ in 0..60 {
        h.step();
    }
    // Then it opens, so a folder inside it can be reached
    let target = folder_row(&h, &home).expect("held over, the folder opened");
    // When it is brought over home: the label says where it will go
    glide(&mut h, parent.center(), target.center());
    assert_eq!(hint(&h).as_deref(), Some("Move 1 photo to “home”"));
    // and released there: it asks first, nothing moved yet
    release(&mut h, target.center());
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::MovePhotos { .. })), "{:?}", h.app.ui.dialog);
    assert!(trip.join("beach.png").is_file(), "not before Move");
    h.step();
    click(&mut h, "button:dialogOk");
    // Then the file moved, the photo follows it, and the toast says Undo moves it back
    assert!(home.join("beach.png").is_file() && !trip.join("beach.png").exists(), "moved on disk");
    let photo = h.app.session.catalog.photo(lcc_catalog::PhotoId(beach)).unwrap();
    assert!(matches!(&photo.source, lcc_catalog::Source::File { path } if Path::new(path) == home.join("beach.png")));
    assert!(toast(&h).starts_with("Moved 1 photo to “home”. Edit → Undo moves them back."), "{}", toast(&h));
    assert!(h.app.ui.dragging_photos.is_none(), "the drag ended");
    // and one Undo moves it back
    h.app.run("edit.undo", json!({})).unwrap();
    assert!(trip.join("beach.png").is_file() && !home.join("beach.png").exists(), "back where it was");
    let _ = std::fs::remove_dir_all(&root);
}

/// A selection set with a repeated id holds each photo once, so the drag carries it once.
#[test]
fn a_drag_after_selecting_with_a_repeated_id_carries_each_photo_once() {
    let (root, trip, home) = folders("dedupe");
    let mut h = app(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)]);
    let (beach, garden) = (id_of(&h, "beach.png"), id_of(&h, "garden.png"));
    h.app.run("library.select", json!({"ids": [beach, garden, beach], "active": beach})).unwrap();
    let from = widget(&h, &format!("thumb:{beach}")).unwrap().center();
    press(&mut h, from);
    glide(&mut h, from, from + egui::vec2(60.0, 60.0));
    assert_eq!(h.app.ui.dragging_photos, Some(vec![beach, garden]));
    release(&mut h, from);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn local_photos_are_refused_with_the_reason() {
    let (root, trip, home) = folders("local");
    // the garden photo imported; the beach one only looked at under Local, where the grid shows it
    let mut h = app(&[(home.join("garden.png"), false)]);
    let r = h.request(
        "engine.execute",
        json!({"command": "library.browse", "params": {"path": trip.to_string_lossy()}}),
        std::time::Duration::from_secs(10),
    );
    assert_eq!(r["ok"], true, "{r}");
    // (the folder is read in the background)
    let shown = |h: &Headless| {
        h.app.session.catalog.photos().any(|p| p.file_name == "beach.png") && h.app.widgets.iter().any(|(w, _)| w.starts_with("thumb:"))
    };
    h.step_until(std::time::Duration::from_secs(20), |h| shown(h));
    let beach = id_of(&h, "beach.png");
    assert!(h.app.session.catalog.photo(lcc_catalog::PhotoId(beach)).unwrap().local);
    fold_local(&mut h);
    let target = folder_row(&h, &home).expect("the imported folder");
    let from = widget(&h, &format!("thumb:{beach}")).unwrap().center();
    press(&mut h, from);
    glide(&mut h, from, target.center());
    // (the sidebar can settle while the drag runs: end on the row where it is now)
    let target = folder_row(&h, &home).expect("the imported folder");
    h.app.synthetic.push(egui::Event::PointerMoved(target.center()));
    h.step();
    h.step();
    // Then the row says it can't take them, and a release says why
    assert_eq!(hint(&h).as_deref(), Some("Can't be moved here"));
    release(&mut h, target.center());
    assert!(trip.join("beach.png").is_file() && !home.join("beach.png").exists(), "stays");
    assert!(toast(&h).contains("Add to My Photos"), "{}", toast(&h));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn photos_already_in_the_folder_stay_put() {
    let (root, trip, home) = folders("here");
    let mut h = app(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)]);
    // open the folder holding both
    let toggle = widget(&h, &format!("libraryFolderToggle:{}", tree_path(&root))).unwrap();
    h.app.synthetic.push(egui::Event::PointerMoved(toggle.center()));
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: toggle.center(),
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    release(&mut h, toggle.center());
    let target = folder_row(&h, &home).expect("opened");
    let garden = id_of(&h, "garden.png");
    h.app.ui.dragging_photos = Some(vec![garden]);
    h.app.synthetic.push(egui::Event::PointerMoved(target.center()));
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: target.center(),
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    assert_eq!(hint(&h).as_deref(), Some("Already in “home”"));
    let undo = h.app.session.undo.len();
    release(&mut h, target.center());
    assert!(home.join("garden.png").is_file());
    assert_eq!(h.app.session.undo.len(), undo, "nothing done");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn photos_that_leave_the_view_leave_the_selection() {
    let (root, trip, home) = folders("selection");
    std::fs::copy(trip.join("beach.png"), trip.join("dunes.png")).unwrap();
    let mut h = app(&[(trip.join("beach.png"), false), (trip.join("dunes.png"), false), (home.join("garden.png"), false)]);
    // shown by folder: trip's two photos, both selected
    let r = h.request(
        "engine.execute",
        json!({"command": "library.source", "params": {"kind": "libraryFolder", "path": trip.to_string_lossy()}}),
        std::time::Duration::from_secs(10),
    );
    assert_eq!(r["ok"], true, "{r}");
    let (beach, dunes) = (id_of(&h, "beach.png"), id_of(&h, "dunes.png"));
    h.app.session.selection = lcc_engine::Selection { ids: vec![lcc_catalog::PhotoId(beach)], active: Some(lcc_catalog::PhotoId(beach)) };
    for _ in 0..4 {
        h.step();
    }
    let target = folder_row(&h, &home).expect("the other folder is listed beside it");
    let from = widget(&h, &format!("thumb:{beach}")).unwrap().center();
    press(&mut h, from);
    glide(&mut h, from, target.center());
    release(&mut h, target.center());
    h.step();
    click(&mut h, "button:dialogOk");
    assert!(home.join("beach.png").is_file(), "moved");
    // it left trip's view, and the selection with it: a Delete next can't reach it unseen
    assert!(!h.app.session.selection.contains(lcc_catalog::PhotoId(beach)), "{:?}", h.app.session.selection);
    assert!(h.app.session.visible_cloned().contains(&lcc_catalog::PhotoId(dunes)));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_folder_left_and_come_back_to_waits_again_before_opening() {
    let (root, trip, home) = folders("spring");
    let mut h = app(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)]);
    let beach = id_of(&h, "beach.png");
    let parent = folder_row(&h, &root).unwrap();
    let from = widget(&h, &format!("thumb:{beach}")).unwrap().center();
    press(&mut h, from);
    // over the closed folder for a moment, then away, longer than it waits
    glide(&mut h, from, parent.center());
    glide(&mut h, parent.center(), from);
    for _ in 0..60 {
        h.step();
    }
    // back over it: it doesn't open at once
    glide(&mut h, from, parent.center());
    assert!(folder_row(&h, &home).is_none(), "came back to: it waits again");
    for _ in 0..60 {
        h.step();
    }
    assert!(folder_row(&h, &home).is_some(), "held: it opens");
    release(&mut h, parent.center());
    let _ = std::fs::remove_dir_all(&root);
}

/// Drag the beach photo from the grid onto `target` and let go.
fn drop_beach(h: &mut Headless, beach: u64, target: Rect) {
    let from = widget(h, &format!("thumb:{beach}")).unwrap().center();
    press(h, from);
    glide(h, from, target.center());
    release(h, target.center());
    h.step();
}

#[test]
fn cancelling_the_question_moves_nothing() {
    let (root, trip, home) = folders("cancel");
    let mut h = app(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)]);
    let beach = id_of(&h, "beach.png");
    let target = folder_row(&h, &home).unwrap_or_else(|| folder_row(&h, &root).unwrap());
    let target = if folder_row(&h, &home).is_some() {
        target
    } else {
        click(&mut h, &format!("libraryFolderToggle:{}", tree_path(&root)));
        folder_row(&h, &home).unwrap()
    };
    let undo = h.app.session.undo.len();
    drop_beach(&mut h, beach, target);
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::MovePhotos { .. })));
    click(&mut h, "button:dialogCancel");
    assert!(h.app.ui.dialog.is_none());
    assert!(trip.join("beach.png").is_file() && !home.join("beach.png").exists(), "nothing moved");
    assert_eq!(h.app.session.undo.len(), undo);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dont_ask_again_moves_the_next_drop_at_once() {
    let (root, trip, home) = folders("dont-ask");
    std::fs::copy(trip.join("beach.png"), trip.join("dunes.png")).unwrap();
    let mut h = app(&[(trip.join("beach.png"), false), (trip.join("dunes.png"), false), (home.join("garden.png"), false)]);
    click(&mut h, &format!("libraryFolderToggle:{}", tree_path(&root)));
    let target = folder_row(&h, &home).unwrap();
    let beach = id_of(&h, "beach.png");
    h.app.session.selection = lcc_engine::Selection::single(lcc_catalog::PhotoId(beach));
    drop_beach(&mut h, beach, target);
    // the question, with Don't ask again ticked
    click(&mut h, "check:moveDontAsk");
    click(&mut h, "button:dialogOk");
    assert!(home.join("beach.png").is_file());
    assert!(!h.app.ui.settings.ask_before_move, "kept in Settings");
    // the next drop goes at once
    let dunes = id_of(&h, "dunes.png");
    h.app.session.selection = lcc_engine::Selection::single(lcc_catalog::PhotoId(dunes));
    h.step();
    let target = folder_row(&h, &home).unwrap();
    drop_beach(&mut h, dunes, target);
    assert!(h.app.ui.dialog.is_none(), "not asked");
    assert!(home.join("dunes.png").is_file(), "moved at once");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dont_ask_again_is_kept_only_when_the_move_began() {
    let (root, trip, home) = folders("dont-ask-refused");
    let mut h = app(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)]);
    click(&mut h, &format!("libraryFolderToggle:{}", tree_path(&root)));
    let target = folder_row(&h, &home).unwrap();
    let beach = id_of(&h, "beach.png");
    // a file of that name is in the folder already (not in the library): the move is refused
    std::fs::copy(trip.join("beach.png"), home.join("beach.png")).unwrap();
    drop_beach(&mut h, beach, target);
    click(&mut h, "check:moveDontAsk");
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::MovePhotos { dont_ask: true, .. })), "ticked");
    click(&mut h, "button:dialogOk");
    assert!(h.app.ui.dialog.is_none(), "the refusal closes the question");
    assert!(trip.join("beach.png").is_file(), "nothing moved");
    assert!(h.app.ui.settings.ask_before_move, "a refused move leaves the question on");
    let _ = std::fs::remove_dir_all(&root);
}

/// A confirmation that finds the photos in the folder already (they were moved there while the question was
/// open, or a folder synchronization relinked them) moves nothing, so it doesn't turn the question off either.
#[test]
fn dont_ask_again_is_kept_only_when_a_photo_moved() {
    let (root, trip, home) = folders("dont-ask-already");
    let mut h = app(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)]);
    click(&mut h, &format!("libraryFolderToggle:{}", tree_path(&root)));
    let target = folder_row(&h, &home).unwrap();
    let beach = id_of(&h, "beach.png");
    drop_beach(&mut h, beach, target);
    click(&mut h, "check:moveDontAsk");
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::MovePhotos { dont_ask: true, .. })), "ticked");
    // the photo gets there some other way while the question is open
    h.app.run("photo.moveToFolder", json!({"folder": home.to_string_lossy(), "ids": [beach]})).unwrap();
    assert!(home.join("beach.png").is_file());
    click(&mut h, "button:dialogOk");
    assert!(h.app.ui.dialog.is_none());
    assert!(h.app.ui.settings.ask_before_move, "nothing moved: the question stays on");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_question_left_open_while_another_library_opens_moves_nothing() {
    let (root, trip, home) = folders("other-library");
    let mut h = app(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)]);
    click(&mut h, &format!("libraryFolderToggle:{}", tree_path(&root)));
    let beach = id_of(&h, "beach.png");
    let target = folder_row(&h, &home).unwrap();
    drop_beach(&mut h, beach, target);
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::MovePhotos { .. })));
    // another library opens meanwhile (the menu bar and agents reach past a dialog)
    let other = root.join("other-library");
    h.app.session.open_library(other.join("lib"), false).unwrap();
    let r = h.request("ui.dialog.confirm", json!({}), std::time::Duration::from_secs(10));
    assert_eq!(r["ok"], false, "{r}");
    assert!(r["error"].as_str().unwrap_or_default().contains("Another catalog was opened"), "{r}");
    assert!(trip.join("beach.png").is_file() && !home.join("beach.png").exists(), "nothing moved");
    let _ = std::fs::remove_dir_all(&root);
}

/// Move Selected Photos Here stays in a folder's menu with nothing selected, grayed out (a click
/// moves nothing), so the menu keeps its shape; once a photo is selected it moves its file there.
#[test]
fn move_selected_photos_here_is_grayed_out_until_photos_are_selected() {
    let (root, trip, home) = folders("menu-move");
    let mut h = app(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)]);
    click(&mut h, &format!("libraryFolderToggle:{}", tree_path(&root)));
    let garden = id_of(&h, "garden.png");
    let menu_item = |h: &mut Headless| {
        let at = folder_row(h, &trip).unwrap().center();
        let r = h.request("ui.click", json!({"x": at.x, "y": at.y, "button": "right"}), std::time::Duration::from_secs(10));
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        let r = h.request("ui.clickWidget", json!({"id": "folderMovePhotosHere"}), std::time::Duration::from_secs(10));
        assert_eq!(r["ok"], true, "listed: {r}");
        h.step();
        h.step();
    };
    h.app.run("library.select", json!({"ids": []})).unwrap();
    h.app.ui.toasts.clear();
    menu_item(&mut h);
    // an enabled row would try and say "no photos to move"; a grayed-out one says nothing
    assert!(h.app.ui.toast().is_none(), "a grayed-out row does nothing: {}", toast(&h));
    assert!(home.join("garden.png").is_file() && !trip.join("garden.png").exists(), "grayed out: nothing moved");
    // close the menu, then select the photo and open it again
    let r = h.request("ui.key", json!({"key": "Escape"}), std::time::Duration::from_secs(10));
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    h.app.run("library.select", json!({"ids": [garden], "active": garden})).unwrap();
    h.step();
    menu_item(&mut h);
    assert!(trip.join("garden.png").is_file() && !home.join("garden.png").exists(), "the selected photo's file moved");
    let path = h.app.session.catalog.photo(lcc_catalog::PhotoId(garden)).map(|p| p.source.clone());
    assert!(
        matches!(&path, Some(lcc_catalog::Source::File { path }) if Path::new(path) == trip.join("garden.png")),
        "the library follows the file: {path:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn export_folder_with_a_preset_writes_exactly_the_folders_photos() {
    let (root, trip, home) = folders("menu-export");
    std::fs::create_dir_all(trip.join("dunes")).unwrap();
    std::fs::copy(trip.join("beach.png"), trip.join("dunes").join("sand.png")).unwrap();
    let out = root.join("out");
    let mut h = app(&[(trip.join("beach.png"), false), (trip.join("dunes").join("sand.png"), false), (home.join("garden.png"), false)]);
    h.app.services.write_shared = Some(std::sync::Arc::new(|p: &str, bytes: &[u8]| lcc_engine::export::write_file(p, bytes)));
    h.app.run("export.savePreset", json!({"name": "Tiny PNG", "params": {"format": "png", "longEdge": 4}})).unwrap();
    // a preset names no folder: the last export's is used
    h.app.session.last_export = Some(json!({"dir": out}));
    click(&mut h, &format!("libraryFolderToggle:{}", tree_path(&root)));
    let at = folder_row(&h, &trip).unwrap().center();
    let r = h.request("ui.click", json!({"x": at.x, "y": at.y, "button": "right"}), std::time::Duration::from_secs(10));
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    for id in ["folderExportPresetMenu", "folderExportPreset:Tiny PNG"] {
        let r = h.request("ui.clickWidget", json!({"id": id}), std::time::Duration::from_secs(10));
        assert_eq!(r["ok"], true, "{id}: {r}");
        h.step();
        h.step();
    }
    assert!(h.app.ui.dialog.is_none(), "a preset starts without asking: {}", toast(&h));
    let t0 = std::time::Instant::now();
    while (h.app.export.is_some() || h.app.last_export_result.is_none()) && t0.elapsed() < std::time::Duration::from_secs(60) {
        h.step();
    }
    let last = h.app.last_export_result.clone().expect("the export reports");
    assert_eq!(last["files"].as_array().map(Vec::len).or(last["files"].as_u64().map(|n| n as usize)), Some(2), "{last}");
    let mut names: Vec<String> = std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    names.sort();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.iter().any(|n| n.contains("beach")) && names.iter().any(|n| n.contains("sand")), "{names:?}");
    assert!(!names.iter().any(|n| n.contains("garden")), "{names:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Right-click the row `id` and click the menu item `item`.
fn menu_click(h: &mut Headless, row: &str, item: &str) {
    let t = std::time::Duration::from_secs(10);
    let at = widget(h, row).unwrap_or_else(|| panic!("{row} is drawn")).center();
    assert_eq!(h.request("ui.click", json!({"x": at.x, "y": at.y, "button": "right"}), t)["ok"], true);
    h.step();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id": item}), t);
    assert_eq!(r["ok"], true, "{item}: {r}");
}

/// The Import review once the scan has finished.
fn review(h: &mut Headless) -> crate::import::ImportDialog {
    assert!(
        h.step_until(std::time::Duration::from_secs(20), |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. }))),
        "the review opens"
    );
    // (the wait above ended on this dialog)
    let Some(crate::state::Dialog::Import { opts }) = h.app.ui.dialog.clone() else { return Default::default() };
    *opts
}

/// A folder's menu starts bringing photos in: Import to This Folder… asks for a folder, then opens
/// the Import review for its files copying into this folder (all into it, not into dated
/// subfolders), and confirming puts the files there while the originals stay.
#[test]
fn import_to_this_folder_opens_the_review_copying_into_the_folder() {
    let (root, trip, home) = folders("menu-import-here");
    let card = root.join("card");
    std::fs::create_dir_all(&card).unwrap();
    write_png(&card.join("sunset.png"), [200, 90, 30]);
    let from = card.to_string_lossy().to_string();
    let services = Services { png: None, pick_folder: Some(Box::new(move || Some(from.clone()))), ..Default::default() };
    let mut h = app_with(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)], services);
    click(&mut h, &format!("libraryFolderToggle:{}", tree_path(&root)));
    let row = format!("source:libfolder:{}", tree_path(&trip));
    menu_click(&mut h, &row, "folderImportHere");
    let d = review(&mut h);
    assert_eq!(d.sources, [card.to_string_lossy().to_string()]);
    assert_eq!(d.candidates.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["sunset.png"]);
    assert!(d.copy && !d.move_files, "copies by default: the originals stay where they are");
    assert_eq!(d.destination, tree_path(&trip), "the folder as the Folders tree names it");
    assert_eq!(d.organize, "flat", "the photos go into the folder itself");
    let r = h.request("ui.dialog.confirm", json!({}), std::time::Duration::from_secs(10));
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(std::time::Duration::from_secs(30), |_| trip.join("sunset.png").is_file()), "the copy is in the folder");
    assert!(card.join("sunset.png").is_file(), "the original stays");
    let _ = std::fs::remove_dir_all(&root);
}

/// A folder picker that is cancelled opens nothing; a host with no picker has no such row.
#[test]
fn import_to_this_folder_does_nothing_when_the_picker_is_cancelled_and_needs_a_picker() {
    let (root, trip, home) = folders("menu-import-cancel");
    let services = Services { png: None, pick_folder: Some(Box::new(|| None)), ..Default::default() };
    let mut h = app_with(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)], services);
    click(&mut h, &format!("libraryFolderToggle:{}", tree_path(&root)));
    let row = format!("source:libfolder:{}", tree_path(&trip));
    menu_click(&mut h, &row, "folderImportHere");
    for _ in 0..10 {
        h.step();
    }
    assert!(h.app.ui.dialog.is_none() && h.app.scan.is_none(), "cancelled: nothing opens: {:?}", h.app.ui.dialog);
    let mut bare = app(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)]);
    click(&mut bare, &format!("libraryFolderToggle:{}", tree_path(&root)));
    let at = widget(&bare, &row).unwrap().center();
    bare.request("ui.click", json!({"x": at.x, "y": at.y, "button": "right"}), std::time::Duration::from_secs(10));
    bare.step();
    bare.step();
    assert!(widget(&bare, "folderRename").is_some() && widget(&bare, "folderImportHere").is_none(), "no picker, no row");
    let _ = std::fs::remove_dir_all(&root);
}

/// A Local folder's Add to My Photos… opens the review for the folder, adding in place: the
/// photos the folder was browsed with are offered (checked) instead of "In catalog", and
/// confirming brings them into the library without moving a file.
#[test]
fn add_to_my_photos_reviews_a_local_folders_browsed_photos() {
    let (root, trip, _home) = folders("menu-add-local");
    write_png(&trip.join("dunes.png"), [210, 180, 90]);
    let mut h = app(&[(trip.join("beach.png"), true), (trip.join("dunes.png"), true)]);
    let place = trip.to_string_lossy().to_string();
    h.app.run("local.addRoot", json!({"path": place})).unwrap();
    assert!(h.step_until(std::time::Duration::from_secs(10), |h| widget(h, &format!("source:local:{place}")).is_some()));
    menu_click(&mut h, &format!("source:local:{place}"), "folderAddToMyPhotos");
    let d = review(&mut h);
    assert_eq!(d.sources, std::slice::from_ref(&place));
    assert!(!d.copy, "added where they are");
    assert_eq!(d.candidates.len(), 2);
    assert_eq!(d.selected_paths().len(), 2, "both browsed photos are offered, checked");
    let r = h.request("ui.dialog.confirm", json!({}), std::time::Duration::from_secs(10));
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(std::time::Duration::from_secs(30), |h| h.app.session.catalog.photos().all(|p| !p.local)), "the photos joined the library");
    let paths: std::collections::BTreeSet<String> = h
        .app
        .session
        .catalog
        .photos()
        .filter(|p| !p.deleted)
        .filter_map(|p| match &p.source {
            lcc_catalog::Source::File { path } => Some(path.clone()),
            _ => None,
        })
        .collect();
    let want: std::collections::BTreeSet<String> = ["beach.png", "dunes.png"].iter().map(|f| trip.join(f).to_string_lossy().to_string()).collect();
    assert_eq!(paths, want, "the same two photos, now in the library");
    assert!(trip.join("beach.png").is_file() && trip.join("dunes.png").is_file(), "no file moved");
    let _ = std::fs::remove_dir_all(&root);
}

/// While a scan or review is waiting, both rows are grayed out: another scan would be refused.
#[test]
fn the_import_rows_are_grayed_out_while_a_scan_runs() {
    let (root, trip, home) = folders("menu-import-busy");
    let services = Services { png: None, pick_folder: Some(Box::new(|| Some("/nowhere".into()))), ..Default::default() };
    let mut h = app_with(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)], services);
    click(&mut h, &format!("libraryFolderToggle:{}", tree_path(&root)));
    let _scan = crate::import::stall_scan(&mut h.app);
    menu_click(&mut h, &format!("source:libfolder:{}", tree_path(&trip)), "folderImportHere");
    for _ in 0..5 {
        h.step();
    }
    assert!(h.app.ui.dialog.is_none(), "a grayed-out row opens nothing: {:?}", h.app.ui.dialog);
    assert!(h.app.ui.toast().is_none(), "and says nothing: {}", toast(&h));
    // a Local row's Add to My Photos… is grayed out the same way
    let place = trip.to_string_lossy().to_string();
    h.app.run("local.addRoot", json!({"path": place})).unwrap();
    assert!(h.step_until(std::time::Duration::from_secs(10), |h| widget(h, &format!("source:local:{place}")).is_some()));
    menu_click(&mut h, &format!("source:local:{place}"), "folderAddToMyPhotos");
    for _ in 0..5 {
        h.step();
    }
    assert!(h.app.ui.dialog.is_none() && h.app.ui.toast().is_none(), "{:?} {}", h.app.ui.dialog, toast(&h));
    let _ = std::fs::remove_dir_all(&root);
}
