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
