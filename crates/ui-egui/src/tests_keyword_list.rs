//! The Keyword List in the right panel's Keywords (Lightroom Classic's Keyword List panel),
//! driven through the control channel.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{DacApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The demo library in the Library grid with the Keywords panel open, two photos selected.
fn keywords_panel() -> (Headless, Vec<u64>) {
    keywords_panel_at([1400.0, 1000.0])
}

/// [`keywords_panel`] in a window of `size` (tall enough, every keyword row is on screen).
fn keywords_panel_at(size: [f32; 2]) -> (Headless, Vec<u64>) {
    let app = DacApp::new(dac_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, size, 1.0);
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
    for (method, params) in [
        ("ui.set", json!({"view": "photoGrid", "right": "keywords"})),
        ("engine.execute", json!({"command": "library.select", "params": {"ids": ids}})),
    ] {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.settle(SETTLE);
    (h, ids)
}

fn run(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    h.settle(SETTLE);
    r
}

fn ask(h: &mut Headless, method: &str, params: serde_json::Value) {
    let r = h.request(method, params.clone(), T);
    assert_eq!(r["ok"], true, "{method} {params}: {r}");
    h.settle(SETTLE);
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn keywords_of(h: &Headless, id: u64) -> Vec<String> {
    h.app.session.catalog.photo(dac_catalog::PhotoId(id)).unwrap().meta.keywords.clone()
}

/// The list shows every keyword with its photo count, a keyword without photos too; the triangle
/// opens a level.
#[test]
fn the_keyword_list_shows_every_keyword() {
    let (mut h, _) = keywords_panel();
    run(&mut h, "keyword.create", json!({"name": "Weddings", "parent": "Events"}));
    assert!(has(&h, "keywordRow:Events"), "a keyword with no photos is listed");
    assert!(!has(&h, "keywordRow:Events|Weddings"), "inside a closed level");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordRowToggle:Events"}));
    assert!(has(&h, "keywordRow:Events|Weddings"));
    assert!(has(&h, "keywordCount:mountains"), "photo counts are shown");
}

/// The tick box gives the keyword to every selected photo, then takes it away; with only some of
/// them having it, a click gives it to all.
#[test]
fn the_tick_box_gives_a_keyword_to_the_selection() {
    let (mut h, ids) = keywords_panel();
    run(&mut h, "keyword.create", json!({"name": "Weddings"}));
    // the demo has many keywords: find it with the filter box
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordFilter"}));
    ask(&mut h, "ui.text", json!({"text": "wedd"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordCheck:Weddings"}));
    assert!(ids.iter().all(|id| keywords_of(&h, *id).contains(&"Weddings".to_string())), "given to both");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordCheck:Weddings"}));
    assert!(ids.iter().all(|id| !keywords_of(&h, *id).contains(&"Weddings".to_string())), "taken from both");
    run(&mut h, "photo.setMeta", json!({"ids": [ids[0]], "addKeywords": ["Weddings"]}));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordCheck:Weddings"}));
    assert!(ids.iter().all(|id| keywords_of(&h, *id).contains(&"Weddings".to_string())), "some had it: now all do");
}

/// Typing in the filter box keeps the keywords whose name holds the text, with their parents.
#[test]
fn the_filter_box_narrows_the_list() {
    let (mut h, _) = keywords_panel();
    run(&mut h, "keyword.create", json!({"name": "Weddings", "parent": "Events"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordFilter"}));
    ask(&mut h, "ui.text", json!({"text": "wed"}));
    let rows: Vec<String> = h.app.widgets.iter().filter_map(|(w, _)| w.strip_prefix("keywordRow:").map(str::to_string)).collect();
    assert_eq!(rows, ["Events", "Events|Weddings"]);
}

/// The arrow on a row shows the photos with that keyword.
#[test]
fn the_arrow_shows_the_photos_with_the_keyword() {
    let (mut h, _) = keywords_panel();
    ask(&mut h, "ui.hoverWidget", json!({"id": "keywordRow:mountains"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordShow:mountains"}));
    assert_eq!(h.app.session.filter.keyword.as_deref(), Some("mountains"));
}

fn find(h: &mut Headless, text: &str) {
    ask(h, "ui.clickWidget", json!({"id": "field:keywordFilter"}));
    ask(h, "ui.key", json!({"key": "A", "cmd": true}));
    ask(h, "ui.text", json!({"text": text}));
}

fn info(h: &mut Headless, keyword: &str) -> serde_json::Value {
    h.app.session.execute("keyword.info", &json!({"keyword": keyword})).unwrap_or(serde_json::Value::Null)
}

/// The + button creates a keyword (Create Keyword Tag): inside the keyword picked in the list, with
/// synonyms, and given to the selected photos when asked; Return creates it.
#[test]
fn plus_creates_a_keyword_inside_the_picked_one() {
    let (mut h, ids) = keywords_panel();
    find(&mut h, "mountains");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordRow:mountains"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordList:create"}));
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::KeywordTag { editing: None, .. })), "{:?}", h.app.ui.dialog);
    ask(&mut h, "ui.text", json!({"text": "Alps"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordSynonyms"}));
    ask(&mut h, "ui.text", json!({"text": "peaks, summits"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "check:keywordAddToSelected"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordTagName"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.ui.dialog, None);
    let i = info(&mut h, "mountains|Alps");
    assert_eq!((i["listed"].as_bool(), i["synonyms"].clone()), (Some(true), json!(["peaks", "summits"])), "{i}");
    assert!(ids.iter().all(|id| keywords_of(&h, *id).contains(&"mountains|Alps".to_string())), "given to the selection");
}

/// Double-clicking a keyword edits it (Edit Keyword Tag): the dialog shows what it is, and a new
/// name and an option change apply together, in one undo step.
#[test]
fn double_clicking_a_keyword_edits_it() {
    let (mut h, _) = keywords_panel();
    run(&mut h, "keyword.create", json!({"name": "Weddings", "synonyms": ["marriage"]}));
    find(&mut h, "wedd");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordRow:Weddings", "count": 2}));
    let Some(crate::state::Dialog::KeywordTag { editing, name, synonyms, .. }) = h.app.ui.dialog.clone() else { panic!("{:?}", h.app.ui.dialog) };
    assert_eq!((editing.as_deref(), name.as_str(), synonyms.as_str()), (Some("Weddings"), "Weddings", "marriage"));
    let undo = h.app.session.undo.len();
    ask(&mut h, "ui.text", json!({"text": "Marriages"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "check:keywordIncludeOnExport"}));
    ask(&mut h, "ui.dialog.confirm", json!({}));
    let i = info(&mut h, "Marriages");
    assert_eq!((i["includeOnExport"].as_bool(), i["synonyms"].clone()), (Some(false), json!(["marriage"])), "{i}");
    assert_eq!(h.app.session.undo.len(), undo + 1, "one undo step");
}

/// The − button deletes the picked keyword after asking how many photos lose it; Cancel keeps it, and undo
/// brings a deleted one back.
#[test]
fn minus_deletes_a_keyword_after_asking() {
    let (mut h, _) = keywords_panel();
    find(&mut h, "mountains");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordRow:mountains"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordList:delete"}));
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::DeleteKeyword { .. })), "{:?}", h.app.ui.dialog);
    ask(&mut h, "ui.dialog.cancel", json!({}));
    assert!(h.app.session.catalog.has_keyword("mountains"), "Cancel kept it");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordList:delete"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "button:dialogOk"}));
    assert!(!h.app.session.catalog.has_keyword("mountains"));
    run(&mut h, "edit.undo", json!({}));
    assert!(h.app.session.catalog.has_keyword("mountains"));
}

/// Creating a keyword that exists says so, and the dialog stays open with what was typed.
#[test]
fn creating_a_keyword_that_exists_keeps_the_dialog() {
    let (mut h, _) = keywords_panel();
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordList:create"}));
    ask(&mut h, "ui.text", json!({"text": "mountains"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::KeywordTag { .. })), "still open");
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("already")), "{:?}", h.app.ui.toast);
}

/// A keyword's context menu: create a keyword inside it, edit it, make it where new keywords go
/// (marked in the list), and purge the keywords no photo has.
#[test]
fn a_keywords_menu_offers_its_actions() {
    let (mut h, _) = keywords_panel();
    run(&mut h, "keyword.create", json!({"name": "Weddings", "parent": "Events"}));
    find(&mut h, "events");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordRow:Events", "button": "right"}));
    for item in ["create", "edit", "defaultParent", "purge", "delete"] {
        assert!(has(&h, &format!("keywordMenu:{item}")), "{item}");
    }
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordMenu:defaultParent"}));
    assert_eq!(h.app.session.catalog.default_keyword_parent().as_deref(), Some("Events"));
    assert!(has(&h, "keywordDefault:Events"), "the list marks where new keywords go");
    // Create inside it
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordRow:Events", "button": "right"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordMenu:create"}));
    let Some(crate::state::Dialog::KeywordTag { parent, inside, .. }) = h.app.ui.dialog.clone() else { panic!("{:?}", h.app.ui.dialog) };
    assert_eq!((parent.as_deref(), inside), (Some("Events"), true));
    ask(&mut h, "ui.dialog.cancel", json!({}));
    // Purge: Events|Weddings and Events have no photos
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordRow:Events", "button": "right"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordMenu:purge"}));
    assert!(!h.app.session.catalog.has_keyword("Events"));
    assert_eq!(h.app.session.catalog.default_keyword_parent(), None, "gone with it");
}

fn center(h: &Headless, id: &str) -> egui::Pos2 {
    h.app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| r.center()).unwrap_or_else(|| panic!("no {id} on screen"))
}

/// Drag the widget `from` and drop it on the widget `onto`.
fn drag(h: &mut Headless, from: &str, onto: &str) {
    let to = center(h, onto);
    ask(h, "ui.dragWidget", json!({"id": from, "toX": to.x, "toY": to.y, "steps": 12}));
}

/// Dragging a keyword onto another nests it there, with its photos; dropping it on the Keyword
/// List's title takes it back to the top level.
#[test]
fn dragging_a_keyword_nests_it_and_back() {
    let (mut h, ids) = keywords_panel_at([1400.0, 2400.0]);
    run(&mut h, "photo.setMeta", json!({"ids": [ids[0]], "keywords": ["Lisbon"]}));
    run(&mut h, "keyword.create", json!({"name": "Portugal"}));
    drag(&mut h, "keywordRow:Lisbon", "keywordRow:Portugal");
    assert_eq!(keywords_of(&h, ids[0]), ["Portugal|Lisbon"]);
    assert!(h.app.ui.dragging_keyword.is_none(), "the drag ended");
    assert!(has(&h, "keywordRow:Portugal|Lisbon"), "Portugal opened to show it");
    drag(&mut h, "keywordRow:Portugal|Lisbon", "keywordList:topLevel");
    assert_eq!(keywords_of(&h, ids[0]), ["Lisbon"]);
}

/// A keyword can't be dropped inside itself or one of its own children: nothing changes.
#[test]
fn a_keyword_cant_be_dropped_inside_itself() {
    let (mut h, ids) = keywords_panel_at([1400.0, 2400.0]);
    run(&mut h, "photo.setMeta", json!({"ids": [ids[0]], "keywords": ["Portugal|Lisbon"]}));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordRowToggle:Portugal"}));
    let undo = h.app.session.undo.len();
    drag(&mut h, "keywordRow:Portugal", "keywordRow:Portugal|Lisbon");
    assert_eq!((keywords_of(&h, ids[0]), h.app.session.undo.len()), (vec!["Portugal|Lisbon".to_string()], undo));
    assert_eq!(h.app.ui.dialog, None);
    assert_eq!(h.app.ui.toast, None, "not even tried: no error to show");
}

/// Dropping a keyword where one of that name is already (Italy has a Rome) asks before merging
/// the two; Merge does it.
#[test]
fn dropping_onto_a_taken_name_asks_before_merging() {
    let (mut h, ids) = keywords_panel_at([1400.0, 2400.0]);
    run(&mut h, "photo.setMeta", json!({"ids": [ids[0]], "keywords": ["Rome"]}));
    run(&mut h, "photo.setMeta", json!({"ids": [ids[1]], "keywords": ["Italy|Rome"]}));
    drag(&mut h, "keywordRow:Rome", "keywordRow:Italy");
    assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::MoveKeyword { .. })), "{:?}", h.app.ui.dialog);
    assert_eq!(keywords_of(&h, ids[0]), ["Rome"], "nothing yet");
    ask(&mut h, "ui.clickWidget", json!({"id": "button:dialogOk"}));
    assert_eq!((keywords_of(&h, ids[0]), keywords_of(&h, ids[1])), (vec!["Italy|Rome".to_string()], vec!["Italy|Rome".to_string()]));
}

/// Photos dragged from the grid onto a keyword get it.
#[test]
fn dropping_photos_on_a_keyword_tags_them() {
    let (mut h, ids) = keywords_panel_at([1400.0, 2400.0]);
    run(&mut h, "keyword.create", json!({"name": "Portugal"}));
    drag(&mut h, &format!("thumb:{}", ids[0]), "keywordRow:Portugal");
    assert!(ids.iter().all(|id| keywords_of(&h, *id).contains(&"Portugal".to_string())), "the selection got it");
    assert!(h.app.ui.dragging_photos.is_none());
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no {id}"))
}

/// A drop lands only on what is shown: with the list scrolled so that its title is hidden under
/// the top bar, a keyword released over the top bar stays where it is.
#[test]
fn drops_land_only_on_what_is_shown() {
    let (mut h, ids) = keywords_panel_at([1400.0, 640.0]);
    run(&mut h, "photo.setMeta", json!({"ids": [ids[0]], "keywords": ["aaa|zz"]}));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordRowToggle:aaa"}));
    let title = rect(&h, "keywordList:topLevel");
    ask(&mut h, "ui.hoverWidget", json!({"id": "keywordRow:aaa"}));
    ask(&mut h, "ui.scroll", json!({"dx": 0, "dy": -(title.top() - 20.0)}));
    let title = rect(&h, "keywordList:topLevel");
    let hidden = egui::pos2(title.center().x, title.top() + 8.0);
    assert!(hidden.y < 44.0 && title.contains(hidden), "the title's top is under the top bar: {title:?}");
    let row = rect(&h, "keywordRow:aaa|zz");
    assert!(row.top() > 44.0, "the row is shown: {row:?}");
    ask(&mut h, "ui.dragWidget", json!({"id": "keywordRow:aaa|zz", "toX": hidden.x, "toY": hidden.y, "steps": 12}));
    assert_eq!(keywords_of(&h, ids[0]), ["aaa|zz"], "not moved to the top level");
}

/// With no photo selected the Keyword List is still there, to create and organize keywords; its
/// tick boxes wait for a selection.
#[test]
fn the_keyword_list_needs_no_selection() {
    let (mut h, _) = keywords_panel();
    run(&mut h, "library.select", json!({"ids": []}));
    assert!(has(&h, "keywordList:topLevel") && has(&h, "keywordList:create"), "the list is shown");
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordList:create"}));
    ask(&mut h, "ui.text", json!({"text": "Weddings"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(h.app.session.catalog.has_keyword("Weddings"), "and creates keywords");
}

/// Return in the Synonyms field creates the keyword too, as from the name.
#[test]
fn return_in_synonyms_creates_the_keyword() {
    let (mut h, _) = keywords_panel();
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordList:create"}));
    ask(&mut h, "ui.text", json!({"text": "Weddings"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordSynonyms"}));
    ask(&mut h, "ui.text", json!({"text": "marriage"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.ui.dialog, None);
    assert_eq!(info(&mut h, "Weddings")["synonyms"], json!(["marriage"]));
}

/// The picked keyword and the open levels follow a rename or a move made in the list: renamed,
/// Events stays open and picked; dropped into Portugal, Lisbon stays in sight and picked.
#[test]
fn the_list_follows_its_keywords() {
    let (mut h, ids) = keywords_panel_at([1400.0, 2400.0]);
    run(&mut h, "keyword.create", json!({"name": "Weddings", "parent": "Events"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordRowToggle:Events"}));
    // a moment later: the triangle's click and the row's two aren't a triple click
    for _ in 0..40 {
        h.step();
    }
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordRow:Events", "count": 2}));
    ask(&mut h, "ui.text", json!({"text": "Occasions"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert!(has(&h, "keywordRow:Occasions|Weddings"), "still open");
    assert_eq!(h.app.ui.keyword_list_selected.as_deref(), Some("Occasions"), "still picked");
    run(&mut h, "photo.setMeta", json!({"ids": [ids[0]], "keywords": ["Lisbon"]}));
    run(&mut h, "keyword.create", json!({"name": "Portugal", "parent": null}));
    ask(&mut h, "ui.clickWidget", json!({"id": "keywordRow:Lisbon"}));
    drag(&mut h, "keywordRow:Lisbon", "keywordRow:Portugal");
    assert!(has(&h, "keywordRow:Portugal|Lisbon"), "its new parent opened");
    assert_eq!(h.app.ui.keyword_list_selected.as_deref(), Some("Portugal|Lisbon"));
}

/// Press at `from`, move in steps to `to` (each a frame), without releasing.
fn press_and_move(h: &mut Headless, from: egui::Pos2, to: egui::Pos2) {
    let press = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    h.events.extend([egui::Event::PointerMoved(from), press(from, true)]);
    h.step();
    for i in 1..=8 {
        h.events.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
        h.step();
    }
}

fn release(h: &mut Headless, at: egui::Pos2) {
    h.events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.settle(SETTLE);
}

/// Esc gives up a keyword drag: released on another keyword afterwards, it stays where it is.
#[test]
fn escape_gives_up_a_keyword_drag() {
    let (mut h, ids) = keywords_panel_at([1400.0, 2400.0]);
    run(&mut h, "photo.setMeta", json!({"ids": [ids[0]], "keywords": ["Lisbon"]}));
    run(&mut h, "keyword.create", json!({"name": "Portugal", "parent": null}));
    let (from, to) = (center(&h, "keywordRow:Lisbon"), center(&h, "keywordRow:Portugal"));
    press_and_move(&mut h, from, to);
    assert_eq!(h.app.ui.dragging_keyword.as_deref(), Some("Lisbon"), "dragging");
    h.events.push(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    h.step();
    assert_eq!(h.app.ui.dragging_keyword, None, "given up");
    release(&mut h, to);
    assert_eq!(keywords_of(&h, ids[0]), ["Lisbon"]);
}

/// A drag ends with the button's release even when the Keyword List went away meanwhile (another
/// panel opened): it doesn't linger to the next time the list is shown.
#[test]
fn a_keyword_drag_ends_without_the_list() {
    let (mut h, _) = keywords_panel_at([1400.0, 2400.0]);
    let from = center(&h, "keywordRow:mountains");
    press_and_move(&mut h, from, from + egui::vec2(-300.0, 0.0));
    assert_eq!(h.app.ui.dragging_keyword.as_deref(), Some("mountains"));
    h.app.ui.right = crate::state::RightPanel::Info;
    h.step();
    release(&mut h, from + egui::vec2(-300.0, 0.0));
    assert_eq!(h.app.ui.dragging_keyword, None);
}

/// Pressing on a tick box and moving doesn't pick the keyword up.
#[test]
fn a_tick_box_doesnt_start_a_drag() {
    let (mut h, _) = keywords_panel_at([1400.0, 2400.0]);
    let from = center(&h, "keywordCheck:mountains");
    press_and_move(&mut h, from, from + egui::vec2(0.0, 60.0));
    assert_eq!(h.app.ui.dragging_keyword, None);
    release(&mut h, from + egui::vec2(0.0, 60.0));
}

/// Keywords typed in the Keywords panel's box go inside the default parent too when they are
/// new (Lightroom Classic's Put New Keywords Inside This Keyword); keywords the library has, and
/// paths typed whole, stay as typed.
#[test]
fn typed_keywords_go_inside_the_default_parent() {
    let (mut h, ids) = keywords_panel();
    run(&mut h, "keyword.create", json!({"name": "Events"}));
    run(&mut h, "keyword.setDefaultParent", json!({"keyword": "Events"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "field:keyword"}));
    ask(&mut h, "ui.text", json!({"text": "Birthdays, mountains, Places|Lisbon"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    let k = keywords_of(&h, ids[0]);
    for want in ["Events|Birthdays", "mountains", "Places|Lisbon"] {
        assert!(k.contains(&want.to_string()), "{want}: {k:?}");
    }
}

/// Dropping photos on a keyword says how many got it: one that had it already isn't counted.
#[test]
fn dropping_photos_counts_those_that_get_the_keyword() {
    let (mut h, ids) = keywords_panel_at([1400.0, 2400.0]);
    run(&mut h, "keyword.create", json!({"name": "Portugal"}));
    run(&mut h, "photo.setMeta", json!({"ids": [ids[1]], "addKeywords": ["Portugal"]}));
    drag(&mut h, &format!("thumb:{}", ids[0]), "keywordRow:Portugal");
    let toast = h.app.ui.toast.clone().map(|t| t.0).unwrap_or_default();
    assert!(toast.contains("1 photo") && !toast.contains("2 photos"), "{toast}");
}
