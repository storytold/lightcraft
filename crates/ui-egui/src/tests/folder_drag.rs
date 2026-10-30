//! A Folders row dragged onto another Folders row moves that folder there on disk: a click still
//! chooses the row, a drag lights up the rows that can take it and says why the others can't, a
//! drop asks first (unless the person said not to), then moves it (one undo step). Esc gives the
//! drag up.

use std::path::{Path, PathBuf};

use egui::Pos2;
use serde_json::json;

use crate::headless::Headless;
use crate::tests::folder_drop::{app, click, folder_key, folder_row, glide, hint, id_of, press, release, toast};

/// `root` with the folders `trip` (a photo, and `trip/sub` with one), `home` and `archive` (one
/// photo each), each opened in the sidebar.
fn tree(tag: &str) -> (Headless, PathBuf) {
    let (root, trip, home) = crate::tests::folder_drop::folders(&format!("drag-{tag}"));
    let archive = root.join("archive");
    let sub = trip.join("sub");
    for dir in [&archive, &sub] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::copy(home.join("garden.png"), archive.join("vault.png")).unwrap();
    std::fs::copy(home.join("garden.png"), sub.join("deep.png")).unwrap();
    let mut h =
        app(&[(trip.join("beach.png"), false), (sub.join("deep.png"), false), (home.join("garden.png"), false), (archive.join("vault.png"), false)]);
    click(&mut h, &format!("libraryFolderToggle:{}", folder_key(&root)));
    // (opened as a reveal does: the drag tests are about what comes after, not the triangle)
    let key = lightcraft_catalog::query::folder_key(&folder_key(&trip));
    h.view.ctx.data_mut(|d| d.insert_temp(egui::Id::unique(("libfolder-open", key)), true));
    for _ in 0..3 {
        h.step();
    }
    assert!(folder_row(&h, &trip.join("sub")).is_some(), "the folder is opened to show the one inside it");
    (h, root)
}

fn centre(h: &Headless, dir: &Path) -> Pos2 {
    folder_row(h, dir)
        .unwrap_or_else(|| {
            panic!(
                "no row for {}: {:?}",
                dir.display(),
                h.app.widgets.iter().filter(|(w, _)| w.starts_with("source:libfolder")).map(|(w, _)| w).collect::<Vec<_>>()
            )
        })
        .center()
}

/// Pick up the row of `from` and bring the pointer, still pressed, over the row of `to`.
fn drag_over(h: &mut Headless, from: &Path, to: &Path) {
    // picked up beside the middle, so a drag onto the row it started on moves the pointer too
    let (a, b) = (centre(h, from) + egui::vec2(24.0, 0.0), centre(h, to));
    press(h, a);
    glide(h, a, b);
}

#[test]
fn a_click_chooses_the_row_and_a_drag_picks_it_up() {
    let (mut h, root) = tree("start");
    let (trip, home) = (root.join("trip"), root.join("home"));
    // a click on a row is still a click
    let at = centre(&h, &home);
    press(&mut h, at);
    assert_eq!(h.app.ui.dragging_folder, None, "pressed, not moved");
    release(&mut h, at);
    assert_eq!(h.app.ui.dragging_folder, None);
    assert_eq!(h.app.session.library_folder.as_deref().map(|f| folder_key(Path::new(f))), Some(folder_key(&home)), "chosen");
    // a few points of movement are still a click; further, the folder is picked up
    let at = centre(&h, &trip);
    press(&mut h, at);
    h.app.synthetic.push(egui::Event::PointerMoved(at + egui::vec2(2.0, 0.0)));
    h.step();
    assert_eq!(h.app.ui.dragging_folder, None, "below the drag threshold");
    glide(&mut h, at, at + egui::vec2(0.0, 60.0));
    assert_eq!(h.app.ui.dragging_folder.as_deref().map(|f| folder_key(Path::new(f))), Some(folder_key(&trip)));
    release(&mut h, at + egui::vec2(0.0, 60.0));
    assert_eq!(h.app.ui.dragging_folder, None, "the drag ended with the button");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_folder_dropped_on_another_asks_then_moves_and_undo_brings_it_back() {
    let (mut h, root) = tree("move");
    let (trip, archive) = (root.join("trip"), root.join("archive"));
    drag_over(&mut h, &trip, &archive);
    assert_eq!(hint(&h).as_deref(), Some("Move “trip” into “archive”"));
    let at = centre(&h, &archive);
    release(&mut h, at);
    // it asks first, naming both folders and the photos that go with it
    let Some(crate::state::Dialog::MoveFolder { name, into_name, count, .. }) = h.app.ui.dialog.clone() else { panic!("{:?}", h.app.ui.dialog) };
    assert_eq!(
        (name.as_str(), into_name.as_str(), count),
        (
            &format!("{}/trip", root.file_name().unwrap().to_string_lossy())[..],
            &format!("{}/archive", root.file_name().unwrap().to_string_lossy())[..],
            2
        )
    );
    assert!(trip.join("beach.png").is_file(), "not before Move");
    h.step();
    click(&mut h, "button:dialogOk");
    assert!(archive.join("trip").join("beach.png").is_file() && !trip.exists(), "moved on disk");
    let beach = id_of(&h, "beach.png");
    let photo = h.app.session.catalog.photo(lightcraft_catalog::PhotoId(beach)).unwrap();
    assert!(matches!(&photo.source, lightcraft_catalog::Source::File { path } if Path::new(path) == archive.join("trip").join("beach.png")));
    assert_eq!(toast(&h), "Moved; 2 photo(s) relinked");
    // one Undo moves it back
    h.app.run("edit.undo", json!({})).unwrap();
    assert!(trip.join("beach.png").is_file() && !archive.join("trip").exists(), "back where it was");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cancelling_the_question_moves_nothing() {
    let (mut h, root) = tree("cancel");
    let (trip, archive) = (root.join("trip"), root.join("archive"));
    drag_over(&mut h, &trip, &archive);
    let at = centre(&h, &archive);
    release(&mut h, at);
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::MoveFolder { .. })));
    let undo = h.app.session.undo.len();
    h.step();
    click(&mut h, "button:dialogCancel");
    assert!(h.app.ui.dialog.is_none());
    assert!(trip.is_dir() && !archive.join("trip").exists());
    assert_eq!(h.app.session.undo.len(), undo);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_row_that_cannot_take_the_folder_says_why_and_a_drop_changes_nothing() {
    let (mut h, root) = tree("refuse");
    let (trip, sub) = (root.join("trip"), root.join("trip").join("sub"));
    let undo = h.app.session.undo.len();
    // onto itself, onto a folder inside it, and onto the folder it is in already
    assert!(folder_row(&h, &sub).is_some(), "sub is listed to start with");
    for (target, reason) in [
        (&trip, "A folder can't move into itself or a folder inside it".to_string()),
        (&sub, "A folder can't move into itself or a folder inside it".to_string()),
        (&root, format!("Already in “{}”", root.file_name().unwrap().to_string_lossy())),
    ] {
        drag_over(&mut h, &trip, target);
        assert_eq!(hint(&h), Some(reason.clone()), "over {}", target.display());
        assert_eq!(h.last_cursor, egui::CursorIcon::NotAllowed, "the pointer says so too");
        let at = centre(&h, target);
        release(&mut h, at);
        // (letting go over the row it was picked up on is a slow click: it says nothing)
        assert_eq!(toast(&h), if target == &trip { String::new() } else { reason }, "over {}", target.display());
        assert!(h.app.ui.dialog.is_none(), "no question for what can't be done");
        assert!(trip.join("beach.png").is_file() && sub.join("deep.png").is_file(), "nothing moved");
        assert_eq!(h.app.session.undo.len(), undo);
        assert_eq!(h.app.ui.dragging_folder, None);
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_librarys_own_folder_is_refused_as_what_is_dragged() {
    let (mut h, root) = tree("library");
    let lib = root.join("home").join("mylib");
    std::fs::create_dir_all(&lib).unwrap();
    h.app.session.open_library(&lib, false).unwrap();
    // home holds the library: dragging it anywhere is refused
    let node = |dir: &Path| lightcraft_catalog::FolderNode {
        name: dir.file_name().unwrap().to_string_lossy().to_string(),
        path: folder_key(dir),
        count: 1,
        own: 1,
        volume: false,
        selectable: true,
        label: None,
        children: Vec::new(),
    };
    let refusal = crate::panels::left::folder_drop_refusal(&h.app, &folder_key(&root.join("home")), &node(&root.join("archive")));
    assert_eq!(refusal, Err("The library's own folder can't be moved".to_string()));
    // a folder elsewhere may still go into any other row
    assert_eq!(crate::panels::left::folder_drop_refusal(&h.app, &folder_key(&root.join("archive")), &node(&root.join("trip"))), Ok(()));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn escape_gives_the_drag_up() {
    let (mut h, root) = tree("escape");
    let (trip, archive) = (root.join("trip"), root.join("archive"));
    drag_over(&mut h, &trip, &archive);
    assert!(h.app.ui.dragging_folder.is_some());
    h.events.push(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    h.step();
    assert_eq!(h.app.ui.dragging_folder, None, "given up");
    let at = centre(&h, &archive);
    release(&mut h, at);
    assert!(h.app.ui.dialog.is_none(), "a release after Esc asks nothing");
    assert!(trip.is_dir() && !archive.join("trip").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn escape_in_the_frame_of_the_release_cancels_the_drop() {
    let (mut h, root) = tree("escape-release");
    let (trip, archive) = (root.join("trip"), root.join("archive"));
    // with the question off, a drop that got through would move the folder at once
    h.app.ui.settings.ask_before_move = false;
    drag_over(&mut h, &trip, &archive);
    let at = centre(&h, &archive);
    // (both go to egui in one frame: the synthetic queue would deliver the release a frame apart from the key)
    h.events.push(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    h.events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.step();
    h.step();
    assert_eq!(h.app.ui.dragging_folder, None, "given up");
    assert!(h.app.ui.dialog.is_none(), "a drop that Esc cancelled asks nothing");
    assert!(trip.join("beach.png").is_file() && !archive.join("trip").exists(), "nothing moved");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn with_the_question_turned_off_the_drop_moves_at_once() {
    let (mut h, root) = tree("noask");
    let (trip, archive) = (root.join("trip"), root.join("archive"));
    h.app.ui.settings.ask_before_move = false;
    drag_over(&mut h, &trip, &archive);
    let at = centre(&h, &archive);
    release(&mut h, at);
    assert!(h.app.ui.dialog.is_none(), "not asked");
    assert!(archive.join("trip").join("beach.png").is_file() && !trip.exists(), "moved at once");
    assert_eq!(toast(&h), "Moved; 2 photo(s) relinked");
    h.app.run("edit.undo", json!({})).unwrap();
    assert!(trip.join("beach.png").is_file(), "one Undo");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_move_the_disk_refuses_says_why_in_the_message() {
    let (mut h, root) = tree("taken");
    let (trip, archive) = (root.join("trip"), root.join("archive"));
    // a folder of that name is already there: the move's own refusal reaches the person
    std::fs::create_dir_all(archive.join("trip")).unwrap();
    h.app.ui.settings.ask_before_move = false;
    drag_over(&mut h, &trip, &archive);
    let at = centre(&h, &archive);
    release(&mut h, at);
    assert!(toast(&h).contains("already exists"), "{}", toast(&h));
    assert!(trip.join("beach.png").is_file(), "nothing moved");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn only_the_primary_button_picks_a_row_up() {
    let (mut h, root) = tree("secondary");
    let at = centre(&h, &root.join("trip"));
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Secondary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    glide(&mut h, at, at + egui::vec2(0.0, 80.0));
    assert_eq!(h.app.ui.dragging_folder, None, "the context menu's button doesn't move folders");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_question_left_open_while_another_library_opens_moves_nothing() {
    let (mut h, root) = tree("other-library");
    let (trip, archive) = (root.join("trip"), root.join("archive"));
    drag_over(&mut h, &trip, &archive);
    let at = centre(&h, &archive);
    release(&mut h, at);
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::MoveFolder { .. })));
    // the menu bar or an agent opens another library behind the question
    let lib = root.join("home").join("mylib");
    std::fs::create_dir_all(&lib).unwrap();
    h.app.session.open_library(&lib, false).unwrap();
    h.step();
    click(&mut h, "button:dialogOk");
    assert!(trip.join("beach.png").is_file() && !archive.join("trip").exists(), "nothing moved");
    assert!(toast(&h).starts_with("Another library was opened"), "{}", toast(&h));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_destination_that_went_away_meanwhile_is_not_made_again() {
    let (mut h, root) = tree("gone");
    let (trip, archive) = (root.join("trip"), root.join("archive"));
    drag_over(&mut h, &trip, &archive);
    let at = centre(&h, &archive);
    release(&mut h, at);
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::MoveFolder { .. })));
    // renamed in the file manager while the question is up
    std::fs::rename(&archive, root.join("archive-old")).unwrap();
    h.step();
    click(&mut h, "button:dialogOk");
    assert!(toast(&h).contains("no longer there"), "{}", toast(&h));
    assert!(trip.join("beach.png").is_file(), "nothing moved");
    assert!(!archive.exists(), "and the destination was not made again");
    // the same with no question
    std::fs::rename(root.join("archive-old"), &archive).unwrap();
    h.step();
    h.app.ui.settings.ask_before_move = false;
    h.app.ui.toast = None;
    drag_over(&mut h, &trip, &archive);
    std::fs::rename(&archive, root.join("archive-old")).unwrap();
    let at = centre(&h, &archive);
    release(&mut h, at);
    assert!(toast(&h).contains("no longer there"), "{}", toast(&h));
    assert!(trip.join("beach.png").is_file() && !archive.exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dont_ask_again_is_kept_only_by_a_move_that_happened() {
    let (mut h, root) = tree("dont-ask-fails");
    let (trip, archive) = (root.join("trip"), root.join("archive"));
    drag_over(&mut h, &trip, &archive);
    let at = centre(&h, &archive);
    release(&mut h, at);
    let Some(crate::state::Dialog::MoveFolder { dont_ask, .. }) = &mut h.app.ui.dialog else { panic!("{:?}", h.app.ui.dialog) };
    *dont_ask = true;
    // the destination goes away while the question is up, so the move fails
    std::fs::rename(&archive, root.join("archive-old")).unwrap();
    h.step();
    click(&mut h, "button:dialogOk");
    assert!(toast(&h).contains("no longer there"), "{}", toast(&h));
    assert!(h.app.ui.settings.ask_before_move, "a move that did not happen does not turn the question off");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_release_of_another_button_in_the_middle_of_a_drag_is_not_a_drop() {
    let (mut h, root) = tree("secondary-release");
    let (trip, archive) = (root.join("trip"), root.join("archive"));
    h.app.ui.settings.ask_before_move = false;
    drag_over(&mut h, &trip, &archive);
    let at = centre(&h, &archive);
    for pressed in [true, false] {
        h.app.synthetic.push(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    }
    assert!(h.app.ui.dragging_folder.is_some(), "still dragging");
    assert!(trip.join("beach.png").is_file(), "nothing moved");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_closed_folder_does_not_open_under_its_own_drag() {
    let (mut h, root) = tree("own-spring");
    let trip = root.join("trip");
    let key = lightcraft_catalog::query::folder_key(&folder_key(&trip));
    h.view.ctx.data_mut(|d| d.insert_temp(egui::Id::unique(("libfolder-open", key)), false));
    h.step();
    h.step();
    assert!(folder_row(&h, &trip.join("sub")).is_none(), "closed");
    let a = centre(&h, &trip) + egui::vec2(24.0, 0.0);
    press(&mut h, a);
    glide(&mut h, a, a + egui::vec2(0.0, 2.0));
    let b = centre(&h, &trip);
    glide(&mut h, a, b);
    for _ in 0..60 {
        h.step();
    }
    assert!(folder_row(&h, &trip.join("sub")).is_none(), "still closed after waiting over its own row");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_drag_does_not_outlive_the_library_it_started_in() {
    let (mut h, root) = tree("drag-library");
    let (trip, archive) = (root.join("trip"), root.join("archive"));
    h.app.ui.settings.ask_before_move = false;
    drag_over(&mut h, &trip, &archive);
    assert!(h.app.ui.dragging_folder.is_some());
    let at = centre(&h, &archive);
    // another library opens (the menu bar, an agent) while the button is still down
    let lib = root.join("home").join("mylib");
    std::fs::create_dir_all(&lib).unwrap();
    h.app.session.open_library(&lib, false).unwrap();
    h.step();
    assert_eq!(h.app.ui.dragging_folder, None, "given up");
    release(&mut h, at);
    assert!(trip.join("beach.png").is_file() && !archive.join("trip").exists(), "nothing moved");
    let _ = std::fs::remove_dir_all(&root);
}
