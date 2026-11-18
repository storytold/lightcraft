//! Headless tests of the Modern Library's extras (`crate::modern_library`): the Info card edits every
//! selected photo, leaves mixed values alone unless edited, and the search pill narrows the grid.

use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn ask(h: &mut Headless, method: &str, params: Value) -> Value {
    let r = h.request(method, params, T);
    h.settle(SETTLE);
    r
}

fn exec(h: &mut Headless, command: &str, params: Value) -> Value {
    ask(h, "engine.execute", json!({"command": command, "params": params}))
}

fn library() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1280.0, 800.0], 1.0);
    assert_eq!(exec(&mut h, "view.modernLayout", json!({"on": true}))["ok"], true);
    exec(&mut h, "view.photoGrid", json!({}));
    h
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn title(h: &Headless, id: u64) -> String {
    h.app.session.catalog.photo(lcc_catalog::PhotoId(id)).map(|p| p.meta.title.clone()).unwrap_or_default()
}

/// Types into a field of the card and ends the edit with Return.
fn type_into(h: &mut Headless, field: &str, text: &str) {
    assert_eq!(ask(h, "ui.clickWidget", json!({"id": field}))["ok"], true, "{field}");
    ask(h, "ui.key", json!({"key": "A", "cmd": true}));
    ask(h, "ui.key", json!({"key": "Backspace"}));
    if !text.is_empty() {
        ask(h, "ui.text", json!({"text": text}));
    }
    ask(h, "ui.key", json!({"key": "Enter"}));
}

#[test]
fn the_info_card_writes_to_every_selected_photo_and_leaves_mixed_values_alone() {
    let mut h = library();
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
    // two photos share a caption, the third differs: mixed
    exec(&mut h, "photo.setMeta", json!({"ids": [ids[0], ids[1]], "caption": "same"}));
    exec(&mut h, "photo.setMeta", json!({"ids": [ids[2]], "caption": "other"}));
    exec(&mut h, "library.select", json!({"ids": ids}));
    // Info lives in the second bar, opened from the top bar's button
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:actionsButton"}));
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:infoButton"}));
    assert!(h.app.ui.modern.info && has(&h, "modern:info"), "the card shows");
    type_into(&mut h, "modernInfo:title", "Alps");
    for id in &ids {
        assert_eq!(title(&h, *id), "Alps", "photo {id}");
    }
    // the mixed caption was never edited: each photo keeps its own
    let caption = |h: &Headless, id: u64| h.app.session.catalog.photo(lcc_catalog::PhotoId(id)).map(|p| p.meta.caption.clone()).unwrap_or_default();
    assert_eq!((caption(&h, ids[0]), caption(&h, ids[2])), ("same".to_string(), "other".to_string()));
    // keywords as a comma list
    type_into(&mut h, "modernInfo:keywords", "snow,  peak ,");
    let kw = h.app.session.catalog.photo(lcc_catalog::PhotoId(ids[1])).map(|p| p.meta.keywords.clone()).unwrap_or_default();
    assert!(kw.iter().any(|k| k.ends_with("snow")) && kw.iter().any(|k| k.ends_with("peak")), "{kw:?}");
    // Window ▸ Panels ▸ Info (I) hides and shows this card in Modern
    exec(&mut h, "panel.info", json!({}));
    assert!(!h.app.ui.modern.info, "panel.info hides the card");
    exec(&mut h, "panel.info", json!({}));
    assert!(h.app.ui.modern.info, "and shows it again");
    assert_eq!(crate::menubar::checked(&h.app, "panel.info"), Some(true));
}

#[test]
fn the_search_pill_narrows_the_grid_and_takes_any_text() {
    let mut h = library();
    let all = h.app.session.visible_cloned().len();
    let first = h.app.session.visible_cloned()[0].0;
    exec(&mut h, "photo.setMeta", json!({"ids": [first], "title": "zebracrossing"}));
    assert!(has(&h, "modern:search"));
    type_into(&mut h, "modernSearch", "zebracrossing");
    let n = h.app.session.visible_cloned().len();
    assert!(n >= 1 && n < all, "narrowed: {n} of {all} (filter text {:?}, title {:?})", h.app.session.filter.text, title(&h, first));
    // hostile text is only a search
    type_into(&mut h, "modernSearch", "\u{0}%%' OR 1=1 --\u{202e}🦀");
    assert!(h.app.session.visible_cloned().len() < all);
    type_into(&mut h, "modernSearch", "");
    assert_eq!(h.app.session.visible_cloned().len(), all, "cleared");
    // the chips
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:search:stars"}));
    assert_eq!(h.app.session.filter.rating, 3);
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:search:stars"}));
    assert_eq!(h.app.session.filter.rating, 0);
    ask(&mut h, "ui.clickWidget", json!({"id": "modern:search:picks"}));
    assert_eq!(h.app.session.filter.flag, Some(lcc_catalog::Flag::Pick));
}

/// On a phone the Info card is a sheet along the bottom (full width), and its ✕ closes it.
#[test]
fn on_a_phone_the_info_card_is_a_bottom_sheet() {
    let app = crate::LccApp::new(lcc_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
    let mut h = crate::headless::Headless::new(app, [412.0, 900.0], 1.0);
    h.app.run("view.modernLayout", serde_json::json!({"on": true})).unwrap();
    h.app.run("view.photoGrid", serde_json::json!({})).unwrap();
    let id = h.app.session.visible_cloned()[0].0;
    h.app.run("library.select", serde_json::json!({"ids": [id]})).unwrap();
    h.app.run("modern.info", serde_json::json!({"on": true})).unwrap();
    h.request("ui.widgets", serde_json::json!({}), std::time::Duration::from_secs(20));
    h.settle(std::time::Duration::from_secs(60));
    let card = h.app.widgets.iter().rev().find(|(w, _)| w == "modern:info").map(|(_, r)| *r).expect("the card");
    assert!(card.width() >= 400.0 && card.bottom() >= 880.0, "a bottom sheet: {card:?}");
    assert!(!h.app.widgets.iter().any(|(w, _)| w == "modernSearch"), "the search pill steps aside");
    let r = h.request("ui.clickWidget", serde_json::json!({"id": "modern:info:close"}), std::time::Duration::from_secs(20));
    assert_eq!(r["ok"], true, "{r}");
    h.settle(std::time::Duration::from_secs(60));
    assert!(!h.app.ui.modern.info, "✕ closed it");
}
