//! Photos dragged from the grid onto a Folders row move there on disk: the row lights up, the
//! dragged label says where they will go, a closed folder held under the pointer opens, and a
//! release asks first (unless the person said not to), then moves the files (one undo step).
//! Photos that can't go there, or are there already, say so and stay where they are.

use std::path::{Path, PathBuf};

use egui::{Pos2, Rect};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

/// Folders `trip` and `home` in a fresh temporary folder, with a photo file in each.
fn folders(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("lc-folder-drop-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (trip, home) = (root.join("trip"), root.join("home"));
    for (dir, file) in [(&trip, "beach.png"), (&home, "garden.png")] {
        std::fs::create_dir_all(dir).unwrap();
        let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 120, 160, 255]; 64] };
        let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(dir.join(file), png).unwrap();
    }
    (root, trip, home)
}

/// The photos at `paths` in the catalog (`local`: browsed, not in the library), in the grid with
/// the sidebar out, tall enough for the Folders section.
fn app(paths: &[(PathBuf, bool)]) -> Headless {
    use lightcraft_catalog::{Op, Photo, Source};
    let mut session = lightcraft_engine::Session::new().with_fs();
    for (path, local) in paths {
        let id = session.catalog.alloc_photo_id();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let mut p = Photo::new(id, Source::File { path: path.to_string_lossy().into() }, &name, "PNG", 8, 8, "2026-01-01T10:00:00");
        p.local = *local;
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1300.0, 1600.0], 1.0);
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

fn folder_row(h: &Headless, dir: &Path) -> Option<Rect> {
    widget(h, &format!("source:libfolder:{}", dir.to_string_lossy()))
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
    h.app.ui.toast().map(|(t, ..)| t.clone()).unwrap_or_default()
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
    let photo = h.app.session.catalog.photo(lightcraft_catalog::PhotoId(beach)).unwrap();
    assert!(matches!(&photo.source, lightcraft_catalog::Source::File { path } if Path::new(path) == home.join("beach.png")));
    assert!(toast(&h).starts_with("Moved 1 photo to “home”. Edit → Undo moves them back."), "{}", toast(&h));
    assert!(h.app.ui.dragging_photos.is_none(), "the drag ended");
    // and one Undo moves it back
    h.app.run("edit.undo", json!({})).unwrap();
    assert!(trip.join("beach.png").is_file() && !home.join("beach.png").exists(), "back where it was");
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
    assert!(h.app.session.catalog.photo(lightcraft_catalog::PhotoId(beach)).unwrap().local);
    let target = folder_row(&h, &home).expect("the imported folder");
    let from = widget(&h, &format!("thumb:{beach}")).unwrap().center();
    press(&mut h, from);
    glide(&mut h, from, target.center());
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
    let toggle = widget(&h, &format!("libraryFolderToggle:{}", root.to_string_lossy())).unwrap();
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
    h.app.session.selection =
        lightcraft_engine::Selection { ids: vec![lightcraft_catalog::PhotoId(beach)], active: Some(lightcraft_catalog::PhotoId(beach)) };
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
    assert!(!h.app.session.selection.contains(lightcraft_catalog::PhotoId(beach)), "{:?}", h.app.session.selection);
    assert!(h.app.session.visible_cloned().contains(&lightcraft_catalog::PhotoId(dunes)));
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
        click(&mut h, &format!("libraryFolderToggle:{}", root.to_string_lossy()));
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
    click(&mut h, &format!("libraryFolderToggle:{}", root.to_string_lossy()));
    let target = folder_row(&h, &home).unwrap();
    let beach = id_of(&h, "beach.png");
    h.app.session.selection = lightcraft_engine::Selection::single(lightcraft_catalog::PhotoId(beach));
    drop_beach(&mut h, beach, target);
    // the question, with Don't ask again ticked
    click(&mut h, "check:moveDontAsk");
    click(&mut h, "button:dialogOk");
    assert!(home.join("beach.png").is_file());
    assert!(!h.app.ui.settings.ask_before_move, "kept in Settings");
    // the next drop goes at once
    let dunes = id_of(&h, "dunes.png");
    h.app.session.selection = lightcraft_engine::Selection::single(lightcraft_catalog::PhotoId(dunes));
    h.step();
    let target = folder_row(&h, &home).unwrap();
    drop_beach(&mut h, dunes, target);
    assert!(h.app.ui.dialog.is_none(), "not asked");
    assert!(home.join("dunes.png").is_file(), "moved at once");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_question_left_open_while_another_library_opens_moves_nothing() {
    let (root, trip, home) = folders("other-library");
    let mut h = app(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)]);
    click(&mut h, &format!("libraryFolderToggle:{}", root.to_string_lossy()));
    let beach = id_of(&h, "beach.png");
    let target = folder_row(&h, &home).unwrap();
    drop_beach(&mut h, beach, target);
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::MovePhotos { .. })));
    // another library opens meanwhile (the menu bar and agents reach past a dialog)
    let other = root.join("other-library");
    h.app.session.open_library(other.join("lib"), false).unwrap();
    let r = h.request("ui.dialog.confirm", json!({}), std::time::Duration::from_secs(10));
    assert_eq!(r["ok"], false, "{r}");
    assert!(r["error"].as_str().unwrap_or_default().contains("Another library was opened"), "{r}");
    assert!(trip.join("beach.png").is_file() && !home.join("beach.png").exists(), "nothing moved");
    let _ = std::fs::remove_dir_all(&root);
}
