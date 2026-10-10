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
