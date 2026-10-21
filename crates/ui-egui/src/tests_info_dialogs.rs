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
fn all_metadata_displays_and_copies_png_generator_fields() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    h.settle(SETTLE);
    h.app.ui.dialog = Some(crate::state::Dialog::AllMetadata {
        title: "Qwen_image.png".into(),
        search: String::new(),
        rows: json!({"catalog":[{"name":"City","value":"Paris"}],"exif":[],"xmp":[],"generation":[{"name":"positive","value":"Church in the Forest"}],"pngText":[{"name":"workflow","value":"{\"nodes\":[]}"},{"name":"parameters","value":"Steps: 30, Model: Qwen"}]}),
    });
    h.settle(SETTLE);
    let text = h.painted_text();
    for value in [
        "Catalog · saved values",
        "Paris",
        "ComfyUI generation",
        "PNG text / generator metadata",
        "Church in the Forest",
        "parameters",
        "Steps: 30, Model: Qwen",
    ] {
        assert!(text.iter().any(|s| s == value), "{value}: {text:?}");
    }
    assert!(!text.iter().any(|s| s == "No metadata found"));
    let r = h.request("ui.clickWidget", json!({"id":"button:copyMetadata"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.view.clipboard.contains("Qwen"), "copied: {:?}; clipboard: {:?}", h.app.copied, h.view.clipboard);
}
