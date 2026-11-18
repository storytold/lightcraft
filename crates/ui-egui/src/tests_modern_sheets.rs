//! Headless tests of the Modern layout's Settings screen and Export sheet (`crate::modern_sheets`):
//! Settings shows the desktop's own rows (one settings state, two presentations) and fits a
//! folding phone's two screens, the Layout row switches layouts both ways and is saved with the UI
//! state, and the Export sheet exports in the background, then hands the files to the host.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// A folding phone's cover screen and its inner screen, in points.
const COVER: [f32; 2] = [412.0, 960.0];
const INNER: [f32; 2] = [832.0, 750.0];

fn ask(h: &mut Headless, method: &str, params: Value) -> Value {
    let r = h.request(method, params.clone(), T);
    h.settle(SETTLE);
    r
}

fn exec(h: &mut Headless, command: &str, params: Value) -> Value {
    ask(h, "engine.execute", json!({"command": command, "params": params}))
}

fn click(h: &mut Headless, id: &str) -> Value {
    ask(h, "ui.clickWidget", json!({"id": id}))
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn touch(size: [f32; 2]) -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, size, 1.0);
    assert_eq!(exec(&mut h, "view.modernLayout", json!({"on": true}))["ok"], true);
    h
}

/// The width of the Settings rail's General item: icons only (narrow) or with names (the list).
fn rail_w(h: &Headless) -> f32 {
    h.app.widgets.iter().rev().find(|(w, _)| w == "settings:section:general").map_or(0.0, |(_, r)| r.width())
}

#[test]
fn settings_on_a_phone_is_a_section_with_an_icon_rail_and_back_shows_the_list() {
    let mut h = touch(COVER);
    assert_eq!(exec(&mut h, "app.settings", json!({}))["ok"], true);
    // a section open beside the icons of the others
    assert!(rail_w(&h) > 0.0 && rail_w(&h) < 80.0, "icons only: {}", rail_w(&h));
    assert!(has(&h, "settings:section:folders") && has(&h, "settings:back"));
    assert!(has(&h, "button:settingsLayout-0") && has(&h, "button:settingsLayout-2"), "the Layout row");
    // the same setting Classic changes
    crate::settings_screen::reveal(&mut h, "button:settingsStartup-1");
    assert_eq!(click(&mut h, "button:settingsStartup-1")["ok"], true);
    assert_eq!(h.app.ui.settings.startup_view, crate::state::StartupView::Grid, "one settings state for both layouts");
    // Back: the list of sections (names); a pick opens one; Back again from the list closes
    click(&mut h, "settings:back");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Settings { .. })), "Back from a section keeps Settings open");
    assert!(rail_w(&h) > 200.0, "the list: {}", rail_w(&h));
    click(&mut h, "settings:section:folders");
    assert!(rail_w(&h) < 80.0, "a section again");
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::Settings { tab }) if tab == "folders"));
    click(&mut h, "settings:back");
    click(&mut h, "settings:back");
    assert!(h.app.ui.dialog.is_none());
}

#[test]
fn settings_fits_both_screens_of_a_folding_phone() {
    for size in [COVER, INNER] {
        let mut h = touch(size);
        for (tab, _) in crate::panels::settings::TABS {
            exec(&mut h, "app.settings", json!({"tab": tab}));
            assert!(matches!(&h.app.ui.dialog, Some(Dialog::Settings { tab: t }) if t == tab), "{size:?} {tab}");
            // every row's control starts on screen (wide ones scroll or wrap inside the page)
            let off: Vec<String> = h
                .app
                .widgets
                .iter()
                .filter(|(w, r)| (w.starts_with("button:") || w.starts_with("check:")) && (r.left() < -0.5 || r.left() > size[0] - 8.0))
                .map(|(w, r)| format!("{w} {r:?}"))
                .collect();
            assert!(off.is_empty(), "{size:?} {tab}: off screen: {off:?}");
            // a way out: ‹ (on a narrow screen twice: the list, then closed)
            for _ in 0..2 {
                if h.app.ui.dialog.is_some() {
                    assert!(has(&h, "settings:back"), "{size:?} {tab}: a way out");
                    click(&mut h, "settings:back");
                }
            }
            assert!(h.app.ui.dialog.is_none(), "{size:?} {tab}: closed");
        }
    }
}

#[test]
fn the_layout_row_switches_both_ways_and_is_saved() {
    let mut h = touch(COVER);
    exec(&mut h, "app.settings", json!({"tab": "general"}));
    assert_eq!(click(&mut h, "button:settingsLayout-0")["ok"], true);
    assert!(!h.app.ui.modern.enabled, "Classic chosen");
    // saved with the UI state (ui.json), so it holds after a restart
    let saved = serde_json::to_string(&h.app.ui).unwrap();
    let back: crate::state::UiState = serde_json::from_str(&saved).unwrap();
    assert!(!back.modern.enabled);
    // Classic shows the same Settings, with the same row: the way back
    h.settle(SETTLE);
    assert!(has(&h, "settings:section:general"), "Settings in Classic");
    assert_eq!(click(&mut h, "button:settingsLayout-2")["ok"], true);
    assert!(h.app.ui.modern.enabled, "Modern chosen");
    let back: crate::state::UiState = serde_json::from_str(&serde_json::to_string(&h.app.ui).unwrap()).unwrap();
    assert!(back.modern.enabled);
}

#[test]
fn a_ui_json_saved_with_the_touch_layout_opens_in_modern() {
    // ui.json written before the touch layout became Modern keeps it on
    let mut v = serde_json::to_value(crate::state::UiState::default()).unwrap();
    let obj = v.as_object_mut().unwrap();
    obj.remove("modern");
    obj.insert("touch".into(), json!({"enabled": true, "histogram": "off"}));
    let back: crate::state::UiState = serde_json::from_value(v).unwrap();
    assert!(back.modern.enabled);
    // the touch layout's histogram was off by default: Modern's shows
    assert_eq!(back.modern.histogram, crate::modern::HistogramPlace::Small);
}

type Written = Arc<Mutex<Vec<(String, usize)>>>;
type Handed = Arc<Mutex<Vec<(Vec<String>, bool)>>>;

/// The demo library with a writer that keeps what it is given and a stand-in gallery.
fn with_gallery(size: [f32; 2]) -> (Headless, Written, Handed) {
    let mut h = touch(size);
    let written: Written = Arc::default();
    let w = written.clone();
    h.app.services.write_shared = Some(Arc::new(move |path: &str, bytes: &[u8]| {
        w.lock().unwrap().push((path.to_string(), bytes.len()));
        Ok(())
    }));
    let handed: Handed = Arc::default();
    let g = handed.clone();
    h.app.services.save_to_gallery = Some(Box::new(move |paths: &[String], share: bool| {
        g.lock().unwrap().push((paths.to_vec(), share));
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(Ok(paths.iter().map(|p| format!("content://media/{p}")).collect()));
        Ok(rx)
    }));
    (h, written, handed)
}

#[test]
fn the_export_sheet_exports_in_the_background_then_saves_to_pictures() {
    let (mut h, written, handed) = with_gallery(COVER);
    let id = h.app.session.visible_cloned()[0].0;
    exec(&mut h, "library.select", json!({"ids": [id]}));
    exec(&mut h, "module.develop", json!({}));
    exec(&mut h, "view.detail", json!({}));
    exec(&mut h, "modern.chrome", json!({"on": true}));
    // Shortcuts ▸ Export, with the host's gallery: the sheet
    exec(&mut h, "modern.menu", json!({"on": true}));
    assert_eq!(click(&mut h, "modern:shortcut:export")["ok"], true);
    assert!(h.app.modern_sheets.export.is_some(), "Shortcuts ▸ Export opens the sheet");
    assert!(has(&h, "modern:export:save") && has(&h, "modern:export:share"));
    click(&mut h, "modern:export:preset:share");
    let r = exec(&mut h, "modern.export", json!({"dir": "/touch-test-out", "then": "share"}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(Duration::from_secs(120), |h| h.app.export.is_none()), "the export ends");
    h.settle(SETTLE);
    let files = written.lock().unwrap().clone();
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].0.starts_with("/touch-test-out") && files[0].0.ends_with(".jpg") && files[0].1 > 0, "{files:?}");
    let handed = handed.lock().unwrap().clone();
    assert_eq!(handed, vec![(vec![files[0].0.clone()], true)], "the file went to Pictures and Share");
    assert!(h.step_until(Duration::from_secs(10), |h| h.app.session.activity.list().is_empty()), "no row left behind");
    assert!(h.app.modern_sheets.export.is_none(), "the sheet closed");
}

#[test]
fn library_exports_its_selection_to_a_folder_without_a_gallery() {
    let mut h = touch(INNER);
    let written: Written = Arc::default();
    let w = written.clone();
    h.app.services.write_shared = Some(Arc::new(move |path: &str, bytes: &[u8]| {
        w.lock().unwrap().push((path.to_string(), bytes.len()));
        Ok(())
    }));
    exec(&mut h, "view.photoGrid", json!({}));
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
    exec(&mut h, "library.select", json!({"ids": ids}));
    assert_eq!(exec(&mut h, "modern.exportSheet", json!({"on": true}))["ok"], true);
    assert_eq!(h.app.modern_sheets.export.as_ref().map(|s| s.ids.len()), Some(2));
    assert!(has(&h, "modern:export:go") && !has(&h, "modern:export:share"), "no share sheet without the host's");
    let r = exec(&mut h, "modern.export", json!({"dir": "/touch-test-lib", "longEdge": 1080, "quality": 80}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(Duration::from_secs(120), |h| h.app.export.is_none()));
    assert_eq!(written.lock().unwrap().len(), 2);
}

#[test]
fn back_leaves_settings_and_the_export_sheet_and_bad_export_requests_are_errors() {
    let mut h = touch(COVER);
    let back = |h: &mut Headless| {
        let r = exec(h, "modern.back", json!({}));
        assert_eq!(r["ok"], true, "{r}");
        r["result"]["closed"].clone()
    };
    // Settings on a phone: a section, then the list of sections, then closed
    exec(&mut h, "app.settings", json!({}));
    assert!(rail_w(&h) < 80.0, "a section with the icon rail");
    assert_eq!(back(&mut h), "settingsList");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Settings { .. })), "Back from a section keeps Settings open");
    assert!(rail_w(&h) > 200.0, "on the list of sections");
    back(&mut h);
    assert!(h.app.ui.dialog.is_none(), "Back on the list closes Settings");

    // the Export sheet closes, and Back goes no further (still the Library)
    exec(&mut h, "view.photoGrid", json!({}));
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(1).map(|p| p.0).collect();
    exec(&mut h, "library.select", json!({"ids": ids}));
    assert_eq!(exec(&mut h, "modern.exportSheet", json!({"on": true}))["ok"], true);
    assert!(h.app.modern_sheets.export.is_some());
    assert_eq!(back(&mut h), "export");
    assert!(h.app.modern_sheets.export.is_none(), "Back closes the Export sheet");
    assert!(!has(&h, "modern:export:go"));
    assert_eq!(h.app.ui.module, crate::module::ModuleId::Library);

    // bad export requests are errors, not panics
    exec(&mut h, "library.select", json!({"ids": []}));
    for params in [
        json!({"format": "bmp"}),
        json!({"format": 3}),
        json!({"longEdge": -1}),
        json!({"longEdge": 1e12}),
        json!({"quality": 0}),
        json!({"quality": 101}),
        json!({"then": "print"}),
        json!({"then": "share", "ids": [1]}),
        json!({"ids": "all"}),
        json!({"ids": [-4]}),
        json!({"dir": 5}),
        json!({"dir": "  ", "then": "folder", "ids": [1]}),
    ] {
        let r = exec(&mut h, "modern.export", params.clone());
        assert_eq!(r["ok"], false, "modern.export {params}: {r}");
    }
    let r = exec(&mut h, "modern.exportSheet", json!({}));
    assert_eq!(r["ok"], false, "nothing selected: {r}");
}
