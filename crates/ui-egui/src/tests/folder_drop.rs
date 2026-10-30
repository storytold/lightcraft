//! Photos dragged from the grid onto a Folders row move there on disk: the row lights up, the
//! dragged label says where they will go, a closed folder held under the pointer opens, and a
//! release asks first (unless the person said not to), then moves the files in the background
//! (one undo step; `move_task`). Photos that can't go there, or are there already, say so and
//! stay where they are.

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

/// How the Folders section names `dir` in widget ids: with `/` separators, on Windows too.
fn folder_key(dir: &Path) -> String {
    dir.to_string_lossy().replace('\\', "/")
}

fn folder_row(h: &Headless, dir: &Path) -> Option<Rect> {
    widget(h, &format!("source:libfolder:{}", folder_key(dir)))
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
    h.view.ctx.data(|d| d.get_temp::<(u64, String)>(egui::Id::unique("drag-photos-hint"))).map(|(_, text)| text)
}

/// Click a widget the way a person does (press and release on it).
fn click(h: &mut Headless, id: &str) {
    let at = widget(h, id).unwrap_or_else(|| panic!("no {id}")).center();
    press(h, at);
    release(h, at);
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast.as_ref().map(|(t, _, _)| t.clone()).unwrap_or_default()
}

/// Let the background move end, as the frames that follow it do.
fn settle(h: &mut Headless) {
    assert!(crate::move_task::wait(&mut h.app, &h.view.ctx, std::time::Duration::from_secs(20)), "the move ended");
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
    settle(&mut h);
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
    let toggle = widget(&h, &format!("libraryFolderToggle:{}", folder_key(&root))).unwrap();
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
    settle(&mut h);
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
        click(&mut h, &format!("libraryFolderToggle:{}", folder_key(&root)));
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
    click(&mut h, &format!("libraryFolderToggle:{}", folder_key(&root)));
    let target = folder_row(&h, &home).unwrap();
    let beach = id_of(&h, "beach.png");
    h.app.session.selection = lightcraft_engine::Selection::single(lightcraft_catalog::PhotoId(beach));
    drop_beach(&mut h, beach, target);
    // the question, with Don't ask again ticked
    click(&mut h, "check:moveDontAsk");
    click(&mut h, "button:dialogOk");
    settle(&mut h);
    assert!(home.join("beach.png").is_file());
    assert!(!h.app.ui.settings.ask_before_move, "kept in Settings");
    // the next drop goes at once
    let dunes = id_of(&h, "dunes.png");
    h.app.session.selection = lightcraft_engine::Selection::single(lightcraft_catalog::PhotoId(dunes));
    h.step();
    let target = folder_row(&h, &home).unwrap();
    drop_beach(&mut h, dunes, target);
    settle(&mut h);
    assert!(h.app.ui.dialog.is_none(), "not asked");
    assert!(home.join("dunes.png").is_file(), "moved at once");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_question_left_open_while_another_library_opens_moves_nothing() {
    let (root, trip, home) = folders("other-library");
    let mut h = app(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)]);
    click(&mut h, &format!("libraryFolderToggle:{}", folder_key(&root)));
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

#[test]
fn a_background_move_shows_its_progress_and_holds_back_other_moves_until_it_ends() {
    let (root, trip, home) = folders("background");
    let mut h = app(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)]);
    let (beach, garden) = (id_of(&h, "beach.png"), id_of(&h, "garden.png"));
    // When an agent asks for the move in the background, the call comes back at once
    let r = h.app.run("photo.moveToFolder", json!({"ids": [beach], "folder": home.to_string_lossy(), "background": true})).unwrap();
    assert_eq!(r, json!({"background": true, "files": 1}));
    // Then the activity stack shows it, with how far it has got and no ✕ (it can't be stopped)
    let row = h.app.session.activity.list().into_iter().find(|t| t.kind == "move").expect("a row for the move");
    assert_eq!((row.label.as_str(), row.total, row.cancellable), ("Moving photos", 1, false));
    let inspect = crate::control::inspect(&h.app, &h.view.ctx);
    assert_eq!(inspect["move"]["running"]["files"], 1, "{}", inspect["move"]);
    // and, until it ends, another move waits for it, and Quit asks first
    let e = h.app.run("photo.moveToFolder", json!({"ids": [garden], "folder": trip.to_string_lossy()})).unwrap_err();
    assert!(e.contains("being moved"), "{e}");
    assert_eq!(toast(&h), e, "said where the person sees it: the status line isn't drawn");
    // so is a change that would take the moving photo out of the library, whichever menu or button asked
    h.app.ui.toast = None;
    h.app.run("library.select", json!({"ids": [beach]})).unwrap();
    let e = h.app.run("photo.deletePermanently", json!({"ids": [beach]})).unwrap_err();
    assert_eq!(e, "1 photo is being moved to another folder: wait for the move to finish");
    assert_eq!(toast(&h), e);
    assert!(h.app.session.catalog.photo(lightcraft_catalog::PhotoId(beach)).is_some());
    assert!(!crate::panels::notices::may_close(&mut h.app), "Quit asks while the move runs");
    assert!(format!("{:?}", h.app.quit_prompt).contains("Moving photos"), "{:?}", h.app.quit_prompt);
    h.app.quit_prompt = None;
    settle(&mut h);
    // Once it ends: the file moved, the photo follows, the row is gone and the end is reported
    assert!(home.join("beach.png").is_file() && !trip.join("beach.png").exists());
    let photo = h.app.session.catalog.photo(lightcraft_catalog::PhotoId(beach)).unwrap();
    assert!(matches!(&photo.source, lightcraft_catalog::Source::File { path } if Path::new(path) == home.join("beach.png")));
    assert!(h.app.session.activity.list().is_empty());
    assert!(toast(&h).starts_with("Moved 1 photo to “home”."), "{}", toast(&h));
    let inspect = crate::control::inspect(&h.app, &h.view.ctx);
    assert_eq!(inspect["move"]["running"], serde_json::Value::Null);
    assert_eq!(inspect["move"]["last"], json!({"moved": 1, "folder": home.to_string_lossy()}));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_background_move_refused_for_another_running_move_says_so_in_a_toast() {
    let (root, trip, home) = folders("background-refused");
    let mut h = app(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)]);
    let (beach, garden) = (id_of(&h, "beach.png"), id_of(&h, "garden.png"));
    h.app.run("photo.moveToFolder", json!({"ids": [beach], "folder": home.to_string_lossy(), "background": true})).unwrap();
    // a menu, key or button that asks for the move in the background drops the returned error: the toast is what the person sees
    let e = h.app.run("photo.moveToFolder", json!({"ids": [garden], "folder": trip.to_string_lossy(), "background": true})).unwrap_err();
    assert!(e.contains("being moved"), "{e}");
    assert_eq!(toast(&h), e, "said where the person sees it");
    // and it stays up for 6 s of the app clock
    h.app.ui.toast = None;
    let _ = h.app.run("photo.moveToFolder", json!({"ids": [garden], "folder": trip.to_string_lossy(), "background": true}));
    let expires = h.app.ui.toast.as_ref().map(|(_, until, _)| *until).expect("a toast");
    assert!((expires - (h.app.tick_now() + 6.0)).abs() < 1.0, "{expires}");
    settle(&mut h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_move_started_from_the_grid_or_a_folder_row_that_is_refused_says_so_for_as_long_as_an_error() {
    // the Folders rows and the grid's Move to Folder… start a move through `move_task::start` directly
    let (root, trip, home) = folders("start-refused");
    let mut h = app(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)]);
    let (beach, garden) = (id_of(&h, "beach.png"), id_of(&h, "garden.png"));
    h.app.run("photo.moveToFolder", json!({"ids": [beach], "folder": home.to_string_lossy(), "background": true})).unwrap();
    h.app.ui.toast = None;
    let ctx = h.view.ctx.clone();
    let e = crate::move_task::start(&mut h.app, &ctx, &json!({"ids": [garden], "folder": trip.to_string_lossy()})).unwrap_err();
    assert!(e.contains("being moved"), "{e}");
    let (text, until, _) = h.app.ui.toast.clone().expect("the refusal is told");
    assert_eq!(text, e);
    assert!(until - h.app.tick_now() > 5.0, "an error stays 6 s, not the 1.4 s of a plain toast: {}", until - h.app.tick_now());
    settle(&mut h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_move_refused_when_the_question_is_answered_stays_up_as_long_as_an_error() {
    let (root, trip, home) = folders("question-refused");
    std::fs::copy(trip.join("beach.png"), trip.join("dunes.png")).unwrap();
    let mut h = app(&[(trip.join("beach.png"), false), (trip.join("dunes.png"), false), (home.join("garden.png"), false)]);
    click(&mut h, &format!("libraryFolderToggle:{}", folder_key(&root)));
    let target = folder_row(&h, &home).unwrap();
    let (beach, dunes) = (id_of(&h, "beach.png"), id_of(&h, "dunes.png"));
    h.app.session.selection = lightcraft_engine::Selection::single(lightcraft_catalog::PhotoId(beach));
    drop_beach(&mut h, beach, target);
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::MovePhotos { .. })), "{:?}", h.app.ui.dialog);
    // another move starts while the question is up: the answer is refused
    // (started on the engine and never run: it holds the library as a move on a worker would)
    let _held = lightcraft_engine::move_job::start(&mut h.app.session, &json!({"ids": [dunes], "folder": home.to_string_lossy()})).unwrap();
    h.app.ui.toast = None;
    click(&mut h, "button:dialogOk");
    let (text, until, _) = h.app.ui.toast.clone().expect("the refusal is told");
    assert!(text.contains("being moved"), "{text}");
    assert!(until - h.app.tick_now() > 5.0, "an error stays 6 s, not the 1.4 s of a plain toast: {}", until - h.app.tick_now());
    settle(&mut h);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn quit_anyway_quits_while_a_move_without_a_stop_button_runs() {
    let (root, _trip, _home) = folders("quit");
    let mut h = app(&[]);
    let _stalled = crate::move_task::stalled(&mut h.app);
    assert!(!crate::panels::notices::may_close(&mut h.app), "Quit asks while the move runs");
    for _ in 0..3 {
        h.step();
    }
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:quitAnyway"), "the prompt is shown");
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:quitAnyway"}), std::time::Duration::from_secs(10))["ok"], true);
    assert!(h.quit_requested(), "Quit Anyway quits instead of asking again");
    // Cancel keeps working, and the next quit asks again
    h.app.ui.quit = false;
    h.app.quit_cuts_move = false;
    assert!(!crate::panels::notices::may_close(&mut h.app));
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_move_that_finished_as_the_app_exits_is_followed_by_the_library() {
    let (root, trip, home) = folders("exit");
    let mut h = app(&[(trip.join("beach.png"), false), (home.join("garden.png"), false)]);
    let beach = id_of(&h, "beach.png");
    h.app.run("photo.moveToFolder", json!({"ids": [beach], "folder": home.to_string_lossy(), "background": true})).unwrap();
    // no frame runs `poll`: wait until the worker has finished (so its result is sent), then the
    // exit path gets exactly one look
    h.app.move_run.as_mut().unwrap().worker.take().unwrap().join().unwrap();
    assert!(home.join("beach.png").is_file());
    h.app.finish_ready_move();
    let photo = h.app.session.catalog.photo(lightcraft_catalog::PhotoId(beach)).unwrap();
    assert!(matches!(&photo.source, lightcraft_catalog::Source::File { path } if Path::new(path) == home.join("beach.png")), "{:?}", photo.source);
    assert!(h.app.move_run.is_none());
    let _ = std::fs::remove_dir_all(&root);
}
