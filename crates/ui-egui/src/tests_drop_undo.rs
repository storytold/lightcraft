//! A drop that adds photos to an album or gives them a keyword says so in a toast with an Undo
//! button: one click takes the drop back. The button isn't offered when the drop changed nothing.

use egui::{Event, Modifiers, PointerButton};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

fn app() -> Headless {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.left_panel = true;
    // tall enough for the keyword rows of the right panel to be on screen
    let mut h = Headless::new(app, [1400.0, 1600.0], 1.0);
    for _ in 0..5 {
        h.step();
    }
    h
}

fn rect(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(key, _)| key == id).map(|(_, r)| *r)
}

/// Press and release on `pos` with `ids` being dragged.
fn drop_on(h: &mut Headless, ids: &[u64], pos: egui::Pos2) {
    h.app.ui.dragging_photos = Some(ids.to_vec());
    h.app.synthetic.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    h.app.synthetic.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    h.step();
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast.as_ref().map(|(t, _, _)| t.clone()).unwrap_or_default()
}

fn click(h: &mut Headless, id: &str) {
    let at = rect(h, id).unwrap_or_else(|| panic!("no {id}")).center();
    h.app.synthetic.push(Event::PointerMoved(at));
    h.app.synthetic.push(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    h.app.synthetic.push(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    h.step();
}

#[test]
fn photos_dropped_on_an_album_can_be_taken_back_from_the_toast() {
    let mut h = app();
    let album = h.app.run("album.create", json!({"name": "Undo Target", "addSelected": false})).unwrap()["id"].as_u64().unwrap();
    h.step();
    h.step();
    let row = rect(&h, &format!("source:album:{album}")).expect("the album row");
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|id| id.0).collect();
    drop_on(&mut h, &ids, row.center());
    let held = |h: &Headless| h.app.session.catalog.album(lightcraft_catalog::AlbumId(album)).unwrap().photos.len();
    assert_eq!(held(&h), 2);
    assert!(toast(&h).starts_with("Added 2 photos to “Undo Target”"), "{}", toast(&h));
    assert!(rect(&h, "button:toastUndo").is_some(), "the toast offers Undo");
    // When Undo is clicked, the album is as it was
    click(&mut h, "button:toastUndo");
    assert_eq!(held(&h), 0);
    assert_eq!(toast(&h), "Undo");
    // (the frame after the click paints the toast without it)
    h.step();
    assert!(rect(&h, "button:toastUndo").is_none());
}

#[test]
fn a_drop_that_added_nothing_has_no_undo() {
    let mut h = app();
    let album = h.app.run("album.create", json!({"name": "Full", "addSelected": false})).unwrap()["id"].as_u64().unwrap();
    h.step();
    h.step();
    let row = rect(&h, &format!("source:album:{album}")).expect("the album row");
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|id| id.0).collect();
    drop_on(&mut h, &ids, row.center());
    // (the toast of the first drop goes away before the second is told)
    h.app.ui.toast = None;
    h.step();
    drop_on(&mut h, &ids, row.center());
    assert!(toast(&h).starts_with("Added 0 photos"), "{}", toast(&h));
    assert!(rect(&h, "button:toastUndo").is_none(), "nothing was done, so there is nothing to undo");
}

#[test]
fn photos_dropped_on_a_keyword_can_be_taken_back_from_the_toast() {
    let mut h = app();
    h.app.run("panel.keywords", json!({})).unwrap();
    h.app.run("keyword.create", json!({"name": "Undo Keyword"})).unwrap();
    h.step();
    h.step();
    let row = rect(&h, "keywordRow:Undo Keyword").expect("the keyword row");
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|id| id.0).collect();
    let tagged = |h: &Headless| {
        ids.iter()
            .filter(|id| h.app.session.catalog.photo(lightcraft_catalog::PhotoId(**id)).unwrap().meta.keywords.iter().any(|k| k == "Undo Keyword"))
            .count()
    };
    drop_on(&mut h, &ids, row.center());
    assert_eq!(tagged(&h), 2);
    assert!(rect(&h, "button:toastUndo").is_some(), "{}", toast(&h));
    click(&mut h, "button:toastUndo");
    assert_eq!(tagged(&h), 0);
}

/// The brightest pixel (sum of r, g, b) inside `r`.
fn brightest(image: &egui::ColorImage, r: egui::Rect) -> u32 {
    let mut best = 0;
    for y in r.top().floor() as usize..r.bottom().ceil() as usize {
        for x in r.left().floor() as usize..r.right().ceil() as usize {
            let c = image.pixels[y * image.size[0] + x];
            best = best.max(u32::from(c.r()) + u32::from(c.g()) + u32::from(c.b()));
        }
    }
    best
}

#[test]
fn the_undo_button_is_drawn_above_the_toast_not_under_its_backdrop() {
    let mut h = app();
    let album = h.app.run("album.create", json!({"name": "Undo Visible", "addSelected": false})).unwrap()["id"].as_u64().unwrap();
    h.step();
    h.step();
    let row = rect(&h, &format!("source:album:{album}")).expect("the album row");
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|id| id.0).collect();
    drop_on(&mut h, &ids, row.center());
    let button = rect(&h, "button:toastUndo").expect("the toast offers Undo");
    // a toast is a dark plate (alpha 200 black) with light ink: a label hidden under that plate would be no brighter than it
    let image = h.paint();
    assert!(brightest(&image, button) > 450, "the button's label is as light as the toast's ink, so it is not under the plate");
}

/// A toast with the Undo button, after two photos were dropped on a new album.
fn after_a_drop(name: &str) -> Headless {
    let mut h = app();
    let album = h.app.run("album.create", json!({"name": name, "addSelected": false})).unwrap()["id"].as_u64().unwrap();
    h.step();
    h.step();
    let row = rect(&h, &format!("source:album:{album}")).expect("the album row");
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|id| id.0).collect();
    drop_on(&mut h, &ids, row.center());
    assert!(rect(&h, "button:toastUndo").is_some(), "the toast offers Undo: {}", toast(&h));
    h
}

#[test]
fn the_button_goes_when_the_drop_is_taken_back_from_the_menu() {
    let mut h = after_a_drop("Taken Back");
    // When the drop is undone by Edit > Undo (a menu runs the command and shows no toast of its own)
    crate::menubar::run_item(&mut h.app, "edit.undo", json!({})).unwrap();
    h.step();
    h.step();
    // Then the button is gone: a click would have undone the step before the drop
    assert!(rect(&h, "button:toastUndo").is_none());
}

#[test]
fn the_button_returns_when_the_drop_is_undone_and_redone() {
    let mut h = after_a_drop("Redone");
    crate::menubar::run_item(&mut h.app, "edit.undo", json!({})).unwrap();
    crate::menubar::run_item(&mut h.app, "edit.redo", json!({})).unwrap();
    h.step();
    h.step();
    // (the stacks are as the drop left them again, so it is the drop that Undo would take back)
    assert!(rect(&h, "button:toastUndo").is_some());
}

#[test]
fn the_button_is_not_offered_under_a_dialog() {
    let mut h = after_a_drop("Under A Dialog");
    h.app.ui.dialog = Some(crate::state::Dialog::About);
    h.step();
    h.step();
    assert!(rect(&h, "button:toastUndo").is_none(), "a dialog's backdrop is not to be clicked through");
}
