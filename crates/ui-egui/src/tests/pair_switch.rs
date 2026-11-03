//! The bottom-bar JPEG switch turns paired culling on and off.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

#[test]
fn jpeg_button_turns_paired_culling_on_and_off() {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::new(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    assert!(h.app.widgets.iter().any(|(id, _)| id == "button:pairedCulling-both"), "Both is on the bottom bar");
    assert!(h.app.widgets.iter().any(|(id, _)| id == "button:pairedCulling-jpeg"), "JPEG is on the bottom bar");
    assert!(!h.app.session.paired_culling);

    let r = h.request("ui.clickWidget", json!({"id": "button:pairedCulling-jpeg"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.session.paired_culling);
    assert!(h.app.session.sync_pair_metadata);

    let r = h.request("ui.clickWidget", json!({"id": "button:pairedCulling-jpeg"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.app.session.paired_culling, "clicking the side that is already on leaves it on");

    let r = h.request("ui.clickWidget", json!({"id": "button:pairedCulling-both"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(!h.app.session.paired_culling);
    assert!(!h.app.session.sync_pair_metadata);
}
