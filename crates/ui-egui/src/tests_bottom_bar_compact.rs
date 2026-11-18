//! The Compact bottom bar's own controls (ported from OneSlips): the Auto Sync switch at its right
//! end. Classic keeps its bar as it was.

use serde_json::json;

use crate::headless::Headless;
use crate::tests_classic::{T, run};
use crate::{LccApp, Services};

fn library(view: &str, compact: bool, size: [f32; 2]) -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, size, 1.0);
    h.settle(std::time::Duration::from_secs(120));
    run(&mut h, "view.compactLayout", json!({"on": compact}));
    let r = h.request("ui.set", json!({"view": view}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    h
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    h.step();
}

/// Compact: the bottom bar carries an Auto Sync switch in the grid and in Detail, with its name whenever the name
/// fits next to the bar's other items (it needed a 1600-point bar before, the window less its side panels, so it
/// never showed); a click turns Auto Sync on and off, the same switch as Edit ▸ Auto Sync. Classic keeps its bottom
/// bar: Auto Sync stays under the Edit panel only.
#[test]
fn the_compact_bottom_bar_auto_sync_switch_shows_its_name_and_turns_it_on_and_off() {
    let bare = 22.0 + 14.0;
    let mut h = library("photoGrid", true, [1600.0, 1000.0]);
    for view in ["photoGrid", "detail"] {
        let r = h.request("ui.set", json!({"view": view}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert!(!h.app.session.auto_sync);
        let r =
            h.app.widgets.iter().find(|(w, _)| w == "toggle:autoSync").map(|(_, r)| *r).unwrap_or_else(|| panic!("{view}: the switch is on the bar"));
        assert!(r.width() > bare + 30.0, "{view}: the switch has its name at 1600 wide ({r:?})");
        assert!(h.painted_text().iter().any(|s| s == "Auto Sync"), "{view}: its name is painted");
        click(&mut h, "toggle:autoSync");
        assert!(h.app.session.auto_sync, "{view}: on");
        click(&mut h, "toggle:autoSync");
        assert!(!h.app.session.auto_sync, "{view}: off again");
    }
    run(&mut h, "view.compactLayout", json!({"on": false}));
    let r = h.request("ui.set", json!({"view": "photoGrid"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(!has(&h, "toggle:autoSync"), "Classic's bottom bar has no switch");
}
