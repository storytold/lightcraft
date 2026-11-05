//! #782: dialogs that only show something (System Info, All Metadata, What's New) end with one
//! Close, like About, instead of a Cancel / OK pair that both just close them.

use crate::headless::Headless;
use crate::{LightcraftApp, Services};
use serde_json::json;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(60);

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// Open the dialog through its menu item; let the new window size itself before reading it.
fn open(h: &mut Headless, id: &str) {
    let r = h.request("ui.menu.invoke", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    assert!(h.app.ui.dialog.is_some(), "{id}: no dialog");
    h.step();
    h.step();
}

#[test]
fn information_dialogs_end_with_one_close() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.settle(SETTLE);
    for id in ["app.systemInfo", "dialog.allMetadata", "app.whatsNew"] {
        open(&mut h, id);
        assert!(!has(&h, "button:dialogCancel"), "{id}: a Cancel button, though there is nothing to confirm");
        assert!(has(&h, "button:dialogOk"), "{id}: no Close button");
        let close = h.app.widgets.iter().filter(|(w, _)| w == "button:dialogOk").count();
        assert_eq!(close, 1, "{id}: one footer button");
        let text = h.painted_text();
        assert!(text.iter().any(|t| t == "Close"), "{id}: the button reads Close: {text:?}");
        assert!(!text.iter().any(|t| t == "Cancel" || t == "OK"), "{id}: no Cancel / OK: {text:?}");
        let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
        assert_eq!(r["ok"], true, "{id}: {r}");
        h.step();
        assert!(h.app.ui.dialog.is_none(), "{id}: Close closes the dialog");
        // Esc still closes it too
        open(&mut h, id);
        let r = h.request("ui.key", json!({"key": "Escape"}), T);
        assert_eq!(r["ok"], true, "{id}: {r}");
        h.step();
        assert!(h.app.ui.dialog.is_none(), "{id}: Esc closes the dialog");
    }
}

#[test]
fn location_view_action_is_available_without_a_photo_selection() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    for (method, params) in
        [("ui.set", json!({"right":"info","view":"photoGrid"})), ("engine.execute", json!({"command":"library.select","params":{"ids":[]}}))]
    {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
    }
    h.settle(SETTLE);
    assert!(has(&h, "button:fillLocationView"));
    let r = h.request("ui.clickWidget", json!({"id":"button:fillLocationView"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.session.activity.list().is_empty(), "Photos without GPS should not start a lookup");
}

/// Fill Location works on every selected photo with GPS (`metadata.reverseGeocode` takes the whole
/// selection), so the button is on when the active photo has no GPS but another selected one has.
#[test]
fn fill_location_is_on_when_any_selected_photo_has_gps() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    // tall enough that Fill Location, low in the Info panel, is on screen
    let mut h = Headless::new(app, [1300.0, 2000.0], 1.0);
    let ids: Vec<_> = h.app.session.visible_cloned().into_iter().take(2).collect();
    let (no_gps, with_gps) = (ids[0], ids[1]);
    for (id, gps) in [(no_gps, None), (with_gps, Some((48.86, 2.34)))] {
        let mut meta = h.app.session.catalog.photo(id).unwrap().meta.clone();
        meta.gps = gps;
        h.app.session.catalog.apply(lightcraft_catalog::Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
    }
    let r = h.request("ui.set", json!({"right": "info", "view": "photoGrid"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [no_gps.0, with_gps.0], "active": no_gps.0}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(lightcraft_engine::cmd::geocode::has_usable_gps(&h.app.session, &h.app.session.selected()));
    assert!(has(&h, "button:fillLocation"));
    h.app.ui.toast = None;
    let r = h.request("ui.clickWidget", json!({"id": "button:fillLocation"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    // the command ran: it filled the photo with GPS, or (with no place-name data here) says what's
    // missing; either way the button was on, though the active photo has no GPS
    let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(
        toast.starts_with("Locations saved to catalog") || toast.contains("place names") || toast.contains("settings folder"),
        "Fill Location did nothing for a selection with GPS: {toast:?}"
    );
}
