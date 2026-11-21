//! Settings ▸ Map (step 1.3b of the map work): the ArcGIS API key the Esri styles need is typed
//! there, applied on Return (never while typing), then never shown again; the Map Style list
//! offers the Esri styles only once a key is set; the key never reaches `ui.json`. The map is
//! never online in tests.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const KEY: &str = "AAPK0123456789abcdef_TEST-key.x";

fn widget(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

fn frames(h: &mut Headless, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

fn ok(h: &mut Headless, method: &str, params: serde_json::Value) {
    let r = h.request(method, params, T);
    assert_eq!(r["ok"], true, "{method}: {r}");
}

/// Type `text` into the key field and press Return.
fn type_key(h: &mut Headless, text: &str) {
    ok(h, "ui.clickWidget", json!({"id": "field:esriKey"}));
    frames(h, 2);
    ok(h, "ui.text", json!({"text": text}));
    frames(h, 2);
}

#[test]
fn the_esri_key_is_applied_on_return_and_never_shown_or_saved_with_the_window() {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::new(), Services::default()), [1300.0, 900.0], 1.0);
    ok(&mut h, "engine.execute", json!({"command": "app.settings", "params": {"tab": "map"}}));
    frames(&mut h, 4);
    assert!(widget(&h, "settings:esriKeyStatus").is_some(), "the Map tab shows the key status");
    assert!(widget(&h, "link:arcgisSignUp").is_some(), "and where to get a key");
    // typing alone changes nothing
    type_key(&mut h, KEY);
    assert!(!h.app.session.map.esri_key_set(), "not applied while typing");
    ok(&mut h, "ui.key", json!({"key": "Enter"}));
    frames(&mut h, 3);
    assert!(h.app.session.map.esri_key_set(), "Return applies it");
    // the field is empty again, and the key is in no saved window state
    let draft: Option<String> = h.view.ctx.data(|d| d.get_temp(egui::Id::unique("settings:esriKeyDraft")));
    assert!(draft.unwrap_or_default().is_empty(), "the field forgets the key once applied");
    assert!(!serde_json::to_string(&h.app.ui).unwrap().contains(KEY), "ui.json never holds the key");
    // the Map Style list now offers the Esri styles
    let offered: Vec<String> = h.app.session.map.offered().iter().map(|s| s.id().to_string()).collect();
    assert_eq!(offered, vec!["osm", "opentopomap", "esri-streets", "esri-topo", "offline"]);
    // a key of the wrong shape is refused, stays in the field, and the old one stays set
    type_key(&mut h, "not a key");
    ok(&mut h, "ui.key", json!({"key": "Enter"}));
    frames(&mut h, 3);
    assert!(h.app.session.map.esri_key_set());
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("ArcGIS API key") && !t.0.contains("not a key")), "{:?}", h.app.ui.toast);
    // Remove Key clears it
    let remove = widget(&h, "button:esriKeyClear").expect("Remove Key").center();
    ok(&mut h, "ui.click", json!({"x": remove.x, "y": remove.y}));
    frames(&mut h, 3);
    assert!(!h.app.session.map.esri_key_set());
}
