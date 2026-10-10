//! Headless tests of Library's Classic panels (P1.4): the panel stacks, Folders, Collections,
//! Quick Develop, Keywording and Keyword List.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::module::PanelId;
use crate::panels::classic;
use crate::{DacApp, Services};

pub(crate) const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

pub(crate) fn demo() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = DacApp::new(dac_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.settle(SETTLE);
    run(&mut h, "module.library", json!({}));
    h.request("ui.set", json!({"view": "photoGrid", "leftPanel": true, "right": "none"}), T);
    h.step();
    h.step();
    h
}

pub(crate) fn run(h: &mut Headless, id: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": id, "params": params}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    h.step();
    r["result"].clone()
}

pub(crate) fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

pub(crate) fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    h.step();
}

/// Quick Develop: a step button adds to every selected photo's own value in one undo step; the
/// menus set a choice on each.
#[test]
fn quick_develop_steps_every_selected_photo() {
    let mut h = demo();
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
    run(&mut h, "library.select", json!({"ids": ids}));
    assert!(has(&h, "button:qd-light.exposure-3"), "the exposure row is on screen");
    let exposure = |h: &Headless, id: u64| h.app.session.develop_of(dac_catalog::PhotoId(id)).unwrap().light.exposure;
    let before: Vec<f64> = ids.iter().map(|i| exposure(&h, *i)).collect();
    let undo = h.app.session.undo.len();
    click(&mut h, "button:qd-light.exposure-3");
    for (i, id) in ids.iter().enumerate() {
        assert!((exposure(&h, *id) - before[i] - 1.0).abs() < 1e-9, "+1 stop on photo {id}");
    }
    assert_eq!(h.app.session.undo.len(), undo + 1, "one undo step");
    run(&mut h, "develop.quickSet", json!({"treatment": "bw"}));
    for id in &ids {
        assert_eq!(h.app.session.develop_of(dac_catalog::PhotoId(*id)).unwrap().treatment, dac_develop::Treatment::Bw);
    }
    assert!(has(&h, "button:qd-crop") && has(&h, "button:qd-wb") && has(&h, "button:qd-autoTone"));
}

/// Folders: each disk the library's photos are on is a volume row with its free space; a folder
/// created through the command shows up on disk and undoes.
#[test]
fn folders_panel_lists_volumes_with_free_space() {
    let mut h = demo();
    let dir = std::env::temp_dir().join(format!("classic-ui-vol-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("a.jpg").to_string_lossy().to_string();
    let id = h.app.session.catalog.alloc_photo_id();
    let p = dac_catalog::Photo::new(id, dac_catalog::Source::File { path }, "a.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
    h.app.session.catalog.apply(dac_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    // the space is read off the UI thread: wait for it
    h.step_until(SETTLE, |h| h.app.widgets.iter().any(|(w, _)| w.starts_with("volume:")));
    assert!(h.app.widgets.iter().any(|(w, _)| w.starts_with("volume:")), "a volume row");
    run(&mut h, "folder.create", json!({"parent": dir.to_string_lossy(), "name": "New"}));
    assert!(dir.join("New").is_dir());
    run(&mut h, "edit.undo", json!({}));
    assert!(!dir.join("New").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Collections: the target collection shows under the tree; its × goes back to the Quick
/// Collection.
#[test]
fn collections_panel_shows_and_clears_the_target() {
    let mut h = demo();
    // the Collections panel near the top
    h.app.ui.toggle_sidebar_section("panel:catalog");
    h.app.ui.toggle_sidebar_section("panel:folders");
    let id = run(&mut h, "album.create", json!({"name": "Keepers"}))["id"].as_u64().unwrap();
    assert!(has(&h, "collectionsTarget") && !has(&h, "collectionsTargetClear"));
    run(&mut h, "album.setTarget", json!({"id": id}));
    assert!(has(&h, "collectionsTargetClear"));
    click(&mut h, "collectionsTargetClear");
    assert_eq!(h.app.session.target_album, None);
}

/// Keywording: the tags field, suggestions (a click adds to every selected photo), the keyword
/// set grid and the shortcut field are on screen.
#[test]
fn keywording_panel_adds_suggestions_to_the_selection() {
    let mut h = demo();
    h.app.ui.hidden_panels = vec![PanelId::QuickDevelop];
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
    run(&mut h, "library.select", json!({"ids": ids}));
    assert!(has(&h, "field:keywordTags") && has(&h, "field:keywordAdd") && has(&h, "field:keywordShortcut"));
    let k = h.app.widgets.iter().find_map(|(w, _)| w.strip_prefix("kwdSuggest:").map(str::to_string)).expect("a suggestion");
    click(&mut h, &format!("kwdSuggest:{k}"));
    for id in &ids {
        assert!(
            h.app.session.catalog.photo(dac_catalog::PhotoId(*id)).unwrap().meta.keywords.iter().any(|x| x.eq_ignore_ascii_case(&k)),
            "{k} on {id}"
        );
    }
}

/// Keyword List: created keywords are listed (count 0); the mark toggles a keyword on the
/// selection; People shows person keywords only; a click on a row filters by it.
#[test]
fn keyword_list_marks_filters_and_lists_created_keywords() {
    let mut h = demo();
    h.app.ui.hidden_panels = vec![PanelId::QuickDevelop, PanelId::Keywording];
    run(&mut h, "keyword.create", json!({"keyword": "Anna", "person": true}));
    let id = h.app.session.visible_cloned()[0].0;
    run(&mut h, "library.select", json!({"ids": [id]}));
    assert!(has(&h, "keywordList:Anna"), "a created keyword is listed");
    click(&mut h, "keywordMark:Anna");
    let has_kw = |h: &Headless| h.app.session.catalog.photo(dac_catalog::PhotoId(id)).unwrap().meta.keywords.iter().any(|k| k == "Anna");
    assert!(has_kw(&h), "the mark adds it");
    click(&mut h, "keywordMark:Anna");
    assert!(!has_kw(&h), "and takes it away");
    click(&mut h, "keywordListKind:people");
    let rows = |h: &Headless| h.app.widgets.iter().filter(|(w, _)| w.starts_with("keywordList:")).count();
    assert_eq!(rows(&h), 1, "people only");
    click(&mut h, "keywordListKind:all");
    assert!(rows(&h) > 1);
    click(&mut h, "keywordList:Anna");
    assert_eq!(h.app.session.filter.keyword.as_deref(), Some("Anna"));
}

/// Library's columns are Classic panel stacks: every panel has a foldable header, Solo Mode
/// keeps one open per side, hidden panels go away, and the order is the user's.
#[test]
fn library_columns_are_classic_panels_with_solo_mode() {
    let mut h = demo();
    for p in ["catalog", "folders", "collections", "quickDevelop", "keywording", "keywordList", "metadata"] {
        assert!(has(&h, &format!("classicPanel:{p}")), "{p}");
    }
    assert!(has(&h, "sidebarSection:navigator"));
    // a click folds a panel and unfolds it again
    click(&mut h, "classicPanel:catalog");
    assert!(!classic::is_open(&h.app, PanelId::Catalog) && !has(&h, "source:all"));
    click(&mut h, "classicPanel:catalog");
    assert!(classic::is_open(&h.app, PanelId::Catalog) && has(&h, "source:all"));
    // solo mode: opening one panel folds the others on its side only
    run(&mut h, "panel.solo", json!({"on": true}));
    classic::set_open(&mut h.app, PanelId::Folders, false);
    classic::set_open(&mut h.app, PanelId::Folders, true);
    assert!(classic::is_open(&h.app, PanelId::Folders));
    assert!(!classic::is_open(&h.app, PanelId::Catalog) && !classic::is_open(&h.app, PanelId::Collections));
    assert!(classic::is_open(&h.app, PanelId::Keywording), "the other side is left alone");
    // hidden panels leave the column; the order is the user's
    run(&mut h, "panel.show", json!({"panel": "quickDevelop", "visible": false}));
    run(&mut h, "panel.order", json!({"order": ["metadata", "keywording"]}));
    assert!(!has(&h, "classicPanel:quickDevelop"));
    let y = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| r.top()).unwrap();
    assert!(y(&h, "classicPanel:metadata") < y(&h, "classicPanel:keywording"));
}

fn widget_center(h: &mut Headless, id: &str) -> (f64, f64) {
    let w = h.request("ui.widgets", json!({}), T);
    let r = w["result"].as_array().and_then(|a| a.iter().find(|x| x["id"] == id)).map(|x| x["rect"].clone()).unwrap();
    (r[0].as_f64().unwrap() + r[2].as_f64().unwrap() / 2.0, r[1].as_f64().unwrap() + r[3].as_f64().unwrap() / 2.0)
}

#[test]
fn reorder_moves_a_panel_to_a_slot() {
    use PanelId::*;
    let l = [QuickDevelop, Keywording, KeywordList, Metadata];
    assert_eq!(classic::reorder(&l, Metadata, 0), Some(vec![Metadata, QuickDevelop, Keywording, KeywordList]));
    assert_eq!(classic::reorder(&l, QuickDevelop, 4), Some(vec![Keywording, KeywordList, Metadata, QuickDevelop]));
    assert_eq!(classic::reorder(&l, Keywording, 1), None, "its own slot");
    assert_eq!(classic::reorder(&l, Keywording, 2), None, "just below itself");
    assert_eq!(classic::reorder(&l, Info, 0), None, "not on this side");
    assert_eq!(classic::reorder(&l, QuickDevelop, 99), Some(vec![Keywording, KeywordList, Metadata, QuickDevelop]));
}

/// Dragging a Classic panel header up its side moves the panel there.
#[test]
fn dragging_a_header_reorders_the_side() {
    let mut h = demo();
    run(&mut h, "panel.right", json!({"show": true}));
    for p in [PanelId::QuickDevelop, PanelId::Keywording, PanelId::KeywordList, PanelId::Metadata] {
        classic::set_open(&mut h.app, p, false);
    }
    h.step();
    h.step();
    let from = widget_center(&mut h, "classicPanel:metadata");
    let to = widget_center(&mut h, "classicPanel:quickDevelop");
    let r = h.request("ui.drag", json!({"x": from.0, "y": from.1, "toX": to.0, "toY": to.1 - 6.0, "steps": 8}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    let order = classic::ordered(&h.app, crate::module::LIBRARY_RIGHT);
    assert_eq!(order.first(), Some(&PanelId::Metadata), "{order:?}");
}

/// Auto Hide: a hidden side comes back on a click at the window edge (not on hover), and the
/// two modes exclude each other.
#[test]
fn auto_hide_shows_a_side_on_a_click_at_the_edge() {
    let mut h = demo();
    run(&mut h, "panel.left", json!({"show": false}));
    run(&mut h, "panel.autoShow", json!({"edge": "left", "on": true}));
    run(&mut h, "panel.autoHide", json!({"edge": "left", "on": true}));
    assert!(h.app.ui.auto_hide.left && !h.app.ui.auto_show.left);
    h.request("ui.move", json!({"x": 1.0, "y": 450.0}), T);
    h.step();
    h.step();
    assert!(!h.app.ui.peek.left, "hovering does not show it");
    h.request("ui.click", json!({"x": 1.0, "y": 450.0}), T);
    h.step();
    h.step();
    assert!(h.app.ui.peek.left, "a click at the edge shows it");
    h.request("ui.move", json!({"x": 900.0, "y": 450.0}), T);
    h.step();
    h.step();
    assert!(!h.app.ui.peek.left, "leaving hides it again");
    let r = h.request("engine.execute", json!({"command": "panel.autoHide", "params": {"edge": "middle"}}), T);
    assert_eq!(r["ok"], false, "{r}");
}
