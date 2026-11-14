//! What a drag of photos (or a keyword) promises: Esc gives it up and does nothing else, every row
//! it passes over says what a drop would do, a drop that would do nothing does nothing (no undo
//! step, no toast), and a row that can't take it shows so (the not-allowed cursor and a reason).

use std::time::Duration;

use egui::{Pos2, Rect};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The demo library in the grid with the sidebar's Albums and Keywords and the Keyword List open,
/// in a window tall enough for every row to be on screen.
fn app() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), services), [1600.0, 2600.0], 1.0);
    let hidden = |id: &str| json!({"id": id, "hidden": true});
    let r = h.request(
        "ui.set",
        json!({
            "view": "photoGrid", "leftPanel": true, "right": "keywords",
            "sidebar": ["albums", "keywords", hidden("local"), hidden("byDate"), hidden("folders")],
        }),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn run(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    h.settle(SETTLE);
    r["result"].clone()
}

fn widget(h: &Headless, id: &str) -> Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn pointer(h: &mut Headless, at: Pos2, pressed: Option<bool>) {
    h.events.push(egui::Event::PointerMoved(at));
    if let Some(pressed) = pressed {
        h.events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
    }
    h.step();
}

/// Press on `from`, then move to `to` over a few frames, holding.
fn hold(h: &mut Headless, from: Pos2, to: Pos2) {
    pointer(h, from, Some(true));
    for i in 1..=8 {
        pointer(h, from + (to - from) * (i as f32 / 8.0), None);
    }
    // a few more, so what a row says about the drop has settled
    h.step();
    h.step();
}

fn release(h: &mut Headless, at: Pos2) {
    pointer(h, at, Some(false));
    // (not settled: a toast lapses with the frames)
    h.step();
    h.step();
}

fn esc(h: &mut Headless) {
    h.events.push(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    h.step();
    h.events.push(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: false, repeat: false, modifiers: egui::Modifiers::NONE });
    h.step();
}

/// What the dragged label says this frame instead of the count, if a drop target set it.
fn hint(h: &Headless) -> Option<String> {
    // (only the frame just run: `step` leaves the counter at the next one)
    let frame = h.view.ctx.cumulative_frame_nr();
    h.view
        .ctx
        .data(|d| d.get_temp::<(u64, String)>(egui::Id::unique("drag-photos-hint")))
        .filter(|(f, _)| frame.saturating_sub(*f) <= 1)
        .map(|(_, text)| text)
}

fn toast(h: &Headless) -> String {
    h.app.ui.toast.clone().map(|t| t.0).unwrap_or_default()
}

fn select(h: &mut Headless, ids: &[u64]) {
    run(h, "library.select", json!({"ids": ids}));
}

fn members(h: &Headless, album: u64) -> Vec<u64> {
    h.app.session.catalog.album(lightcraft_catalog::AlbumId(album)).map(|a| a.photos.iter().map(|p| p.0).collect()).unwrap_or_default()
}

fn keywords_of(h: &Headless, id: u64) -> Vec<String> {
    h.app.session.catalog.photo(lightcraft_catalog::PhotoId(id)).map(|p| p.meta.keywords.clone()).unwrap_or_default()
}

/// Two photos of the grid that are not in `album`, and one that is.
fn photos_around(h: &mut Headless, album: u64) -> (Vec<u64>, u64) {
    let held = members(h, album);
    let shown: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    let outside: Vec<u64> = shown.iter().copied().filter(|id| !held.contains(id)).take(2).collect();
    let inside = *held.iter().find(|id| shown.contains(id)).expect("the album holds a photo the grid shows");
    (outside, inside)
}

const GARDEN: u64 = 6;

/// Drag the selection `ids` from its first photo to `to`, still holding.
fn drag_photos(h: &mut Headless, ids: &[u64], to: &str) -> Pos2 {
    select(h, ids);
    let from = widget(h, &format!("thumb:{}", ids[0])).center();
    let target = widget(h, to).center();
    hold(h, from, target);
    assert_eq!(h.app.ui.dragging_photos.as_deref(), Some(ids), "the photos are being dragged");
    target
}

#[test]
fn esc_gives_up_a_photo_drag_and_does_nothing_else() {
    let mut h = app();
    let (outside, _) = photos_around(&mut h, GARDEN);
    // Esc's own command (Back: lights out → on) is something the drag must not also trigger
    h.app.ui.lights_out = crate::state::LightsOut::Dim;
    let before = members(&h, GARDEN);
    let undo = h.app.session.undo.len();
    let target = drag_photos(&mut h, &outside, &format!("source:album:{GARDEN}"));
    esc(&mut h);
    assert_eq!(h.app.ui.dragging_photos, None, "given up");
    assert_eq!(h.app.ui.lights_out, crate::state::LightsOut::Dim, "Esc only gave up the drag");
    assert_eq!(h.app.session.selection.ids.iter().map(|p| p.0).collect::<Vec<_>>(), outside, "the selection stays");
    // let go over the album afterwards: nothing is added
    release(&mut h, target);
    assert_eq!(members(&h, GARDEN), before);
    assert_eq!(h.app.session.undo.len(), undo);
    assert_eq!(toast(&h), "");
}

/// A headless screenshot draws the UI in an offscreen context that has no pointer: it must not
/// give up the drag the window is holding.
#[test]
fn a_headless_screenshot_during_a_drag_leaves_the_drag() {
    let mut h = app();
    let (outside, _) = photos_around(&mut h, GARDEN);
    drag_photos(&mut h, &outside, &format!("source:album:{GARDEN}"));
    let main = h.view.ctx.clone();
    h.app.headless_screenshot(&main, false);
    assert_eq!(h.app.ui.dragging_photos.as_deref(), Some(&outside[..]), "the window's drag goes on");
}

/// Esc and the button's release in one frame: the drag is given up, and Esc's own command (Back)
/// doesn't also run.
#[test]
fn esc_on_the_frame_the_button_is_released_only_gives_up_the_drag() {
    let mut h = app();
    let (outside, _) = photos_around(&mut h, GARDEN);
    h.app.ui.lights_out = crate::state::LightsOut::Dim;
    let target = drag_photos(&mut h, &outside, &format!("source:album:{GARDEN}"));
    let before = members(&h, GARDEN);
    h.events.push(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    h.events.push(egui::Event::PointerButton { pos: target, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.step();
    h.step();
    assert_eq!(h.app.ui.dragging_photos, None, "the drag is over");
    assert_eq!(h.app.ui.lights_out, crate::state::LightsOut::Dim, "Esc only gave up the drag");
    assert_eq!(members(&h, GARDEN), before, "Esc beat the release: nothing was added");
}

#[test]
fn esc_only_gives_up_an_album_or_keyword_drag() {
    let mut h = app();
    h.app.ui.lights_out = crate::state::LightsOut::Dim;
    // an album dragged in the sidebar
    let from = widget(&h, &format!("source:album:{GARDEN}")).center();
    hold(&mut h, from, from + egui::vec2(0.0, 40.0));
    assert_eq!(h.app.ui.dragging_album, Some(GARDEN), "dragging an album");
    esc(&mut h);
    assert_eq!(h.app.ui.dragging_album, None);
    assert_eq!(h.app.ui.lights_out, crate::state::LightsOut::Dim, "an album drag's Esc is its own");
    release(&mut h, from);
    // a keyword dragged in the Keyword List
    let from = widget(&h, "keywordRow:mountains").center();
    hold(&mut h, from, from + egui::vec2(0.0, 40.0));
    assert_eq!(h.app.ui.dragging_keyword.as_deref(), Some("mountains"));
    esc(&mut h);
    assert_eq!(h.app.ui.dragging_keyword, None);
    assert_eq!(h.app.ui.lights_out, crate::state::LightsOut::Dim, "a keyword drag's Esc is its own");
    release(&mut h, from);
    // and with nothing dragged, Esc does what it did
    esc(&mut h);
    assert_eq!(h.app.ui.lights_out, crate::state::LightsOut::On);
}

fn key(h: &mut Headless, key: egui::Key) {
    for pressed in [true, false] {
        h.events.push(egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE });
        h.step();
    }
}

#[test]
fn a_drag_started_before_full_screen_ends_with_the_button_and_esc_leaves() {
    let mut h = app();
    let (outside, _) = photos_around(&mut h, GARDEN);
    let target = drag_photos(&mut h, &outside, &format!("source:album:{GARDEN}"));
    // F while the button is still held: the layout that runs the drag's feedback is gone
    key(&mut h, egui::Key::F);
    assert!(h.app.ui.fullscreen, "F opened the full-screen preview");
    release(&mut h, target);
    assert_eq!(h.app.ui.dragging_photos, None, "the drag ended with the button");
    key(&mut h, egui::Key::Escape);
    assert!(!h.app.ui.fullscreen, "Esc leaves full screen");
}

/// In full screen no drop target reads the release, so the release frame ends the drag itself rather
/// than leaving it standing for one more frame (where the shortcuts no longer hold Esc back for it).
#[test]
fn esc_the_frame_after_a_release_in_full_screen_leaves_full_screen() {
    let mut h = app();
    let (outside, _) = photos_around(&mut h, GARDEN);
    let target = drag_photos(&mut h, &outside, &format!("source:album:{GARDEN}"));
    key(&mut h, egui::Key::F);
    assert!(h.app.ui.fullscreen, "F opened the full-screen preview");
    pointer(&mut h, target, Some(false));
    assert_eq!(h.app.ui.dragging_photos, None, "the release frame ended the drag");
    key(&mut h, egui::Key::Escape);
    assert!(!h.app.ui.fullscreen, "and Esc then leaves full screen");
}

/// A right-button release over a row that refuses photos (a smart album, an album folder, a keyword
/// row) is not the drop the primary button's release is: no refusal is said. (Whether the drag itself
/// goes on after that release is the grid's `drag_feedback`, not this row's.)
#[test]
fn a_right_button_release_over_a_refusing_row_says_nothing() {
    let mut h = app();
    let smart = run(&mut h, "album.createSmart", json!({"name": "Fours", "rules": {"rating": 4}}))["id"].as_u64().unwrap();
    h.settle(SETTLE);
    let (outside, _) = photos_around(&mut h, GARDEN);
    for row in [format!("source:album:{smart}"), "source:folder:1".to_string(), "source:keyword:mountains".to_string()] {
        let target = drag_photos(&mut h, &outside, &row);
        for pressed in [true, false] {
            h.events.push(egui::Event::PointerButton {
                pos: target,
                button: egui::PointerButton::Secondary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            h.step();
        }
        assert_eq!(toast(&h), "", "{row}: nothing is said: {}", toast(&h));
        release(&mut h, target);
        h.app.ui.toast = None;
    }
}

/// A release that a hidden pass took leaves the drag set with no button down: Esc on the next frame
/// still only gives it up, before Back can run.
#[test]
fn esc_with_a_drag_left_over_and_no_button_down_only_gives_up_the_drag() {
    let mut h = app();
    let (outside, _) = photos_around(&mut h, GARDEN);
    h.app.ui.lights_out = crate::state::LightsOut::Dim;
    h.app.ui.dragging_photos = Some(outside);
    esc(&mut h);
    assert_eq!(h.app.ui.dragging_photos, None, "given up");
    assert_eq!(h.app.ui.lights_out, crate::state::LightsOut::Dim, "Esc only gave up the drag");
}

#[test]
fn esc_during_a_drag_leaves_a_dialog_open() {
    let mut h = app();
    let (outside, _) = photos_around(&mut h, GARDEN);
    drag_photos(&mut h, &outside, &format!("source:album:{GARDEN}"));
    run(&mut h, "dialog.newAlbum", json!({}));
    h.step();
    assert!(h.app.ui.dialog.is_some(), "a dialog opened over the drag");
    esc(&mut h);
    assert_eq!(h.app.ui.dragging_photos, None, "Esc gave up the drag");
    assert!(h.app.ui.dialog.is_some(), "and only the drag");
}

#[test]
fn an_album_row_says_how_many_photos_a_drop_adds() {
    let mut h = app();
    let (outside, inside) = photos_around(&mut h, GARDEN);
    let before = members(&h, GARDEN);
    // two photos that aren't in the album
    let target = drag_photos(&mut h, &outside, &format!("source:album:{GARDEN}"));
    let said = hint(&h).expect("the row says what a drop does");
    assert!(said.contains("2 photos") && said.contains("Garden"), "{said}");
    assert!(h.painted_text().contains(&said), "the dragged label shows it");
    release(&mut h, target);
    assert_eq!(members(&h, GARDEN).len(), before.len() + 2);
    assert_eq!(toast(&h), "Added 2 photos to “Garden”");
    // one that is there already and one that isn't: only the one counts, and the toast says so
    let (fresh, _) = photos_around(&mut h, GARDEN);
    let fresh = *fresh.first().expect("the grid shows a photo Garden still lacks");
    let pair = [inside, fresh];
    let target = drag_photos(&mut h, &pair, &format!("source:album:{GARDEN}"));
    assert!(hint(&h).is_some_and(|t| t.contains("1 photo ") && !t.contains("2 ")), "{:?}", hint(&h));
    release(&mut h, target);
    assert_eq!(toast(&h), "Added 1 photo to “Garden” · 1 already there");
}

#[test]
fn a_drop_on_an_album_that_adds_nothing_does_nothing() {
    let mut h = app();
    let (_, inside) = photos_around(&mut h, GARDEN);
    let before = members(&h, GARDEN);
    let undo = h.app.session.undo.len();
    let target = drag_photos(&mut h, &[inside], &format!("source:album:{GARDEN}"));
    let said = hint(&h).expect("the row says so");
    assert!(said.contains("Garden") && !said.starts_with("Add "), "{said}");
    assert_ne!(h.last_cursor, egui::CursorIcon::NotAllowed, "it isn't refused: it is just already there");
    release(&mut h, target);
    assert_eq!(members(&h, GARDEN), before);
    assert_eq!(h.app.session.undo.len(), undo, "no undo step for nothing");
    assert_eq!(toast(&h), "", "and nothing to say");
    assert_eq!(h.app.ui.dragging_photos, None);
}

/// A drag of a photo the album holds and a Local one: nothing is added, and the row says why the
/// Local one stays out instead of only "Already in".
#[test]
fn an_album_row_explains_the_local_photos_of_a_drag_it_adds_nothing_of() {
    use lightcraft_catalog::{Op, Photo, Source};
    let mut h = app();
    let (_, inside) = photos_around(&mut h, GARDEN);
    let id = h.app.session.catalog.alloc_photo_id();
    let mut local = Photo::new(id, Source::File { path: "/nowhere/local.png".into() }, "local.png", "PNG", 8, 8, "2026-01-01T10:00:00");
    local.local = true;
    h.app.session.catalog.apply(Op::AddPhoto { photo: Box::new(local) }).unwrap();
    let before = members(&h, GARDEN);
    let undo = h.app.session.undo.len();
    let target = drag_photos(&mut h, &[inside], &format!("source:album:{GARDEN}"));
    // the Local photo joins the drag (the grid shows none; a folder view's selection would)
    h.app.ui.dragging_photos = Some(vec![inside, id.0]);
    h.step();
    let said = hint(&h).expect("the row says what a drop does");
    assert!(said.contains("Add to My Photos first"), "{said}");
    assert_eq!(h.last_cursor, egui::CursorIcon::NotAllowed);
    release(&mut h, target);
    assert!(toast(&h).contains("1 Local photo skipped"), "{}", toast(&h));
    assert_eq!(members(&h, GARDEN), before);
    assert_eq!(h.app.session.undo.len(), undo);
    assert_eq!(h.app.ui.dragging_photos, None);
}

#[test]
fn rows_that_cannot_hold_photos_say_so_and_refuse() {
    let mut h = app();
    let smart = run(&mut h, "album.createSmart", json!({"name": "Fours", "rules": {"rating": 4}}))["id"].as_u64().unwrap();
    h.settle(SETTLE);
    let (outside, _) = photos_around(&mut h, GARDEN);
    let travel_folder = 1u64;
    let undo = h.app.session.undo.len();
    let albums_before: Vec<_> = h.app.session.catalog.albums().map(|a| (a.id.0, a.photos.len())).collect();
    for (row, pointing_at) in [
        (format!("source:album:{smart}"), "Edit Smart Album…"),
        (format!("source:folder:{travel_folder}"), "inside"),
        ("source:keyword:mountains".to_string(), "Keyword List"),
    ] {
        let target = drag_photos(&mut h, &outside, &row);
        assert_eq!(h.last_cursor, egui::CursorIcon::NotAllowed, "{row}: refused");
        assert!(hint(&h).is_some(), "{row}: the dragged label says so");
        release(&mut h, target);
        assert!(toast(&h).contains(pointing_at), "{row}: the toast says why: {}", toast(&h));
        assert_eq!(h.app.ui.dragging_photos, None, "{row}");
        assert_eq!(h.app.session.undo.len(), undo, "{row}: nothing changed");
        h.app.ui.toast = None;
    }
    let albums_after: Vec<_> = h.app.session.catalog.albums().map(|a| (a.id.0, a.photos.len())).collect();
    assert_eq!(albums_before, albums_after);
}

#[test]
fn a_keyword_row_says_how_many_photos_would_get_it() {
    let mut h = app();
    let shown: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    let (with, without): (Vec<u64>, Vec<u64>) = shown.iter().copied().partition(|id| keywords_of(&h, *id).iter().any(|k| k == "mountains"));
    assert!(!with.is_empty() && without.len() >= 2, "the demo has photos with and without the keyword");
    let pair = [with[0], without[0], without[1]];
    let target = drag_photos(&mut h, &pair, "keywordRow:mountains");
    let said = hint(&h).expect("the row says what a drop does");
    assert!(said.contains("mountains") && said.contains("2 photos"), "{said}");
    release(&mut h, target);
    assert!(pair.iter().all(|id| keywords_of(&h, *id).iter().any(|k| k == "mountains")));
}

#[test]
fn a_drop_on_a_keyword_every_photo_has_does_nothing() {
    let mut h = app();
    let shown: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
    let with: Vec<u64> = shown.iter().copied().filter(|id| keywords_of(&h, *id).iter().any(|k| k == "mountains")).take(2).collect();
    assert!(!with.is_empty());
    let undo = h.app.session.undo.len();
    let target = drag_photos(&mut h, &with, "keywordRow:mountains");
    assert!(hint(&h).is_some_and(|t| !t.starts_with("Add ")), "{:?}", hint(&h));
    release(&mut h, target);
    assert_eq!(h.app.session.undo.len(), undo, "no undo step for nothing");
    assert_eq!(toast(&h), "");
}

#[test]
fn a_keyword_cannot_be_dropped_inside_itself() {
    let mut h = app();
    run(&mut h, "keyword.create", json!({"name": "Weddings", "parent": "Events"}));
    let r = h.request("ui.clickWidget", json!({"id": "keywordRowToggle:Events"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let tree = |h: &Headless| (h.app.session.catalog.has_keyword("Events|Weddings"), h.app.session.catalog.has_keyword("Weddings"));
    assert_eq!(tree(&h), (true, false));
    let undo = h.app.session.undo.len();
    for over in ["keywordRow:Events", "keywordRow:Events|Weddings"] {
        let (from, to) = (widget(&h, "keywordRow:Events").center(), widget(&h, over).center());
        hold(&mut h, from, to + egui::vec2(60.0, 3.0));
        assert_eq!(h.app.ui.dragging_keyword.as_deref(), Some("Events"), "{over}");
        assert_eq!(h.last_cursor, egui::CursorIcon::NotAllowed, "{over}: refused");
        assert!(h.painted_text().iter().any(|t| t == "Can't go inside itself"), "{over}: the label says why: {:?}", h.painted_text());
        release(&mut h, to);
        assert_eq!(tree(&h), (true, false), "{over}: nothing moved");
        assert_eq!(h.app.session.undo.len(), undo);
    }
}
