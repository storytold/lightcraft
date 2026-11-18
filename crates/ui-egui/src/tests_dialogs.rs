//! Modal application dialogs block background input while retaining their own controls.

use crate::headless::Headless;
use crate::{LccApp, Services};
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

fn demo(size: [f32; 2], ui: serde_json::Value) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, size, 1.0);
    let r = h.request("ui.set", ui, T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(Duration::from_secs(120));
    h
}

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn a_click_outside_closes_the_dialog_when_settings_allow_it_but_not_with_a_dropdown_open() {
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    // a click outside closes the dialog when settings allow it
    // Settings → Close a dialog by clicking outside it: a click on the backdrop closes the dialog
    // like Esc, without reaching the panels below; a click inside it, or a drag that ends outside, keeps it.
    {
        let active = h.app.session.active().unwrap();
        let rating = h.app.session.catalog.photo(active).unwrap().rating;
        h.app.run("dialog.copySettings", json!({})).unwrap();
        h.step();
        // off by default: the background click is blocked and the dialog stays
        let r = h.request("ui.clickWidget", json!({"id": "star:bottom:5"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_some(), "a stray click keeps the dialog unless Settings say otherwise");
        h.app.ui.settings.close_dialogs_outside = true;
        let r = h.request("ui.clickWidget", json!({"id": "copyGroup:none"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_some(), "a click inside the dialog keeps it");
        let window = h.app.widgets.iter().find(|(id, _)| id == "dialog:window").map(|(_, r)| *r).unwrap();
        let r = h.request("ui.drag", json!({"x": window.center().x, "y": window.center().y, "toX": 10.0, "toY": 10.0}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_some(), "a drag that ends outside the dialog keeps it");
        let r = h.request("ui.clickWidget", json!({"id": "star:bottom:5"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_none(), "a click on the backdrop closes the dialog");
        assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, rating, "the click doesn't reach the panel below");
        // the click that opens a dialog lands outside its window; a quick one on a slow frame arrives
        // pressed and released in that one frame
        let gear = h.app.widgets.iter().find(|(id, _)| id == "icon:copyGear").map(|(_, r)| r.center()).unwrap();
        let press =
            |pressed| egui::Event::PointerButton { pos: gear, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        h.events.extend([egui::Event::PointerMoved(gear), press(true), press(false)]);
        h.step();
        h.step();
        assert!(h.app.ui.dialog.is_some(), "the click that opens a dialog doesn't close it");
        let r = h.request("ui.clickWidget", json!({"id": "star:bottom:5"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_none(), "the next click on the backdrop does");
    }
    // a click outside an open dropdown keeps the dialog
    // With a dropdown open in the dialog, a click on the backdrop only closes the dropdown, even when
    // it arrives pressed and released in one frame (the dropdown closes while the dialog is drawn).
    {
        h.app.ui.settings.close_dialogs_outside = true;
        h.app.run("dialog.export", json!({})).unwrap();
        h.step();
        h.step();
        // (a wide window lists the presets beside the settings: the destination dropdown)
        let r = h.request("ui.clickWidget", json!({"id": "combo:exportDestinationMode"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(egui::Popup::is_any_open(&h.view.ctx), "the dropdown is open");
        let at = egui::pos2(10.0, 990.0);
        let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        h.events.extend([egui::Event::PointerMoved(at), press(true), press(false)]);
        h.step();
        h.step();
        assert!(!egui::Popup::is_any_open(&h.view.ctx), "the click closes the dropdown");
        assert!(h.app.ui.dialog.is_some(), "but not the dialog");
    }
}

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn modal_dialogs_close_menus_and_block_background_shortcuts_and_clicks() {
    let mut h = demo([1400.0, 1000.0], json!({"view": "detail", "right": "edit"}));
    // modal dialogs close background menus
    {
        let r = h.request("ui.clickWidget", json!({"id": "menu:Photo"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(egui::Popup::is_any_open(&h.view.ctx));
        h.app.run("dialog.copySettings", json!({})).unwrap();
        h.step();
        assert!(!egui::Popup::is_any_open(&h.view.ctx), "background menus cannot remain above the modal window");
        h.app.ui.dialog = None;
        h.step();
    }
    // modal dialogs block background shortcuts and keep escape
    {
        h.app.session.execute("photo.rate", &json!({"ids": [h.app.session.active().unwrap().0], "rating": 0})).unwrap();
        let active = h.app.session.active().unwrap();
        let rating = h.app.session.catalog.photo(active).unwrap().rating;
        h.app.run("dialog.copySettings", json!({})).unwrap();
        h.step();
        for key in ["5", "ArrowRight", "J"] {
            let r = h.request("ui.key", json!({"key": key}), T);
            assert_eq!(r["ok"], true, "{r}");
            h.step();
        }
        assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, rating);
        assert_eq!(h.app.session.active(), Some(active));
        assert!(!h.app.ui.show_clipping);
        let r = h.request("ui.key", json!({"key": "Escape"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_none());
        let r = h.request("ui.key", json!({"key": "5"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, 5, "closing restores shortcuts");
    }
    // modal dialogs block background clicks and preserve choices
    // Application dialogs block the toolbar, including the gear that would reopen Copy Settings
    // and discard its choices. Their own controls work, and closing restores background input.
    {
        h.app.session.execute("photo.rate", &json!({"ids": [h.app.session.active().unwrap().0], "rating": 0})).unwrap();
        let active = h.app.session.active().unwrap();
        let rating = h.app.session.catalog.photo(active).unwrap().rating;
        h.app.run("dialog.copySettings", json!({})).unwrap();
        // Also exercise the first frame of a newly opened dialog.
        let r = h.request("ui.clickWidget", json!({"id": "star:bottom:5"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, rating, "the dialog blocks background ratings");
        let r = h.request("ui.clickWidget", json!({"id": "copyGroup:none"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        let choices = h.app.ui.dialog.clone();
        assert!(matches!(&choices, Some(crate::state::Dialog::CopySettings { groups }) if groups.is_empty()));
        let r = h.request("ui.clickWidget", json!({"id": "icon:copyGear"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert_eq!(h.app.ui.dialog, choices, "a background click cannot reopen the dialog and reset its choices");
        let r = h.request("ui.clickWidget", json!({"id": "button:dialogCancel"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_none());
        let r = h.request("ui.clickWidget", json!({"id": "star:bottom:5"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, 5, "closing restores toolbar input");
        // A different dialog uses the same barrier.
        h.app.run("app.settings", json!({})).unwrap();
        h.step();
        let r = h.request("ui.clickWidget", json!({"id": "star:bottom:1"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert_eq!(h.app.session.catalog.photo(active).unwrap().rating, 5);
        // (Settings fills the window; its ‹ closes it)
        let r = h.request("ui.clickWidget", json!({"id": "settings:back"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_none());
    }
}

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn delete_rejected_asks_how_and_rename_asks_for_a_presets_text() {
    let mut h = demo([1400.0, 900.0], json!({"view": "photoGrid"}));
    // delete rejected photos asks how
    // Photo ▸ Delete Rejected Photos… always asks, with the number of rejects and how: removing them
    // (the default, undoable) or trashing their files too. Ported from OneSlips' LR-DELETE-REJECTED.
    {
        let ids: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
        h.app.session.execute("photo.flag", &json!({"ids": ids, "flag": "none"})).unwrap();
        h.app.session.execute("photo.flag", &json!({"ids": [ids[0], ids[1]], "flag": "reject"})).unwrap();
        let total = h.app.session.visible_cloned().len();
        crate::menubar::run_item(&mut h.app, "library.deleteRejected", json!({})).unwrap();
        assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::DeleteRejected { count: 2, recycle: false })), "{:?}", h.app.ui.dialog);
        h.step();
        let r = h.request("ui.dialog.confirm", json!({}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.ui.dialog.is_none(), "{:?}", h.app.ui.dialog);
        assert_eq!(h.app.session.visible_cloned().len(), total - 2);
        crate::menubar::run_item(&mut h.app, "library.deleteRejected", json!({})).unwrap();
        assert!(h.app.ui.dialog.is_none(), "nothing left to delete: no dialog");
    }
    // rename with a text preset asks for the text
    // Rename Photos: a naming preset sets the template, a `{text:…}` tag gets its own field, and the
    // rename uses what was typed. Ported from OneSlips' LR-LIB-RENAME.
    {
        let first = h.app.session.visible_cloned()[0];
        h.app.session.execute("library.select", &json!({"ids": [first.0]})).unwrap();
        h.app.ui.dialog = Some(crate::state::Dialog::Rename { template: "{name}".into(), start: 1, texts: Default::default() });
        h.settle(Duration::from_secs(60));
        // the built-in "Custom Text - Number" preset is the fourth
        let r = h.request("ui.clickWidget", json!({"id": "button:renamePreset-3"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(Duration::from_secs(60));
        assert!(h.app.widgets.iter().any(|(w, _)| w == "field:renameText-0"), "a field for the text tag");
        if let Some(crate::state::Dialog::Rename { texts, template, .. }) = h.app.ui.dialog.as_mut() {
            assert!(template.contains("{text:"), "{template}");
            texts.insert("Name".into(), "RA".into());
        }
        let r = h.request("ui.dialog.confirm", json!({}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(
            h.app.session.catalog.photo(first).is_some_and(|p| p.file_name.starts_with("RA-")),
            "{:?}",
            h.app.session.catalog.photo(first).map(|p| p.file_name.clone())
        );
    }
}

/// Show Alternatives: the bar over the frames around a photo appears, and Back leaves them.
#[test]
fn alternatives_bar_shows_and_goes_back() {
    let dir = lcc_testkit::TempDir::new("lcc-ui-alternatives");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut paths = Vec::new();
    for i in 0..5u8 {
        let p = dir.join(format!("IMG_{i:02}.jpg"));
        let img = lcc_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 13) as u8, i * 40, 255]);
        std::fs::write(&p, lcc_engine::export::encode_image(&img, &lcc_engine::export::ExportOptions::default()).unwrap()).unwrap();
        paths.push(p.to_string_lossy().into_owned());
    }
    let mut session = lcc_engine::Session::new().with_fs();
    session.execute("library.import", &json!({"paths": [paths[2]]})).unwrap();
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.app.ui.view = crate::state::ViewMode::Detail;
    let id = h.app.session.visible_cloned()[0];
    h.app.session.execute("photo.showAlternatives", &json!({"id": id.0})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    assert!(h.app.widgets.iter().any(|(w, _)| w == "alternatives:bar"));
    h.settle(Duration::from_secs(60));
    let r = h.request("ui.clickWidget", json!({"id": "button:alternativesBack"}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..4 {
        h.step();
    }
    assert!(h.app.session.alternatives.is_none(), "Back leaves the alternatives");
    assert_eq!(h.app.session.visible_cloned(), vec![id]);
    let _ = std::fs::remove_dir_all(&dir);
}
