//! #782: dialogs that only show something (System Info, All Metadata, What's New) end with one
//! Close, like About, instead of a Cancel / OK pair that both just close them.

use crate::headless::Headless;
use crate::{LccApp, Services};
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
    // a new window sizes itself over its first frames, and on a loaded machine renders still
    // arriving keep the app busy: wait (by the clock, not a frame count) until nothing is pending
    // and the Close button has stayed put for a few frames, so a click lands where it is drawn
    let (mut last, mut still) = (None, 0);
    let t0 = std::time::Instant::now();
    while still < 5 && t0.elapsed() < SETTLE {
        h.step();
        let now = rect(h, "button:dialogOk");
        still = if now.is_some() && now == last && !h.busy() { still + 1 } else { 0 };
        last = now;
    }
}

fn rect(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

#[test]
fn information_dialogs_end_with_one_close() {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
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
        let closed = h.step_until(SETTLE, |h| h.app.ui.dialog.is_none());
        assert!(closed, "{id}: Close closes the dialog (clicked {}, button now at {:?})", r["result"], rect(&h, "button:dialogOk"));
        // Esc still closes it too
        open(&mut h, id);
        let r = h.request("ui.key", json!({"key": "Escape"}), T);
        assert_eq!(r["ok"], true, "{id}: {r}");
        assert!(h.step_until(SETTLE, |h| h.app.ui.dialog.is_none()), "{id}: Esc closes the dialog");
    }
}
