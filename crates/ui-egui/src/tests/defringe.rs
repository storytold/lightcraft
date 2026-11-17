//! Optics ▸ Defringe: the Auto button runs Auto Defringe (Remove Chromatic Aberration on, the photo's purple
//! fringes measured), and Protect Colours is a checkbox under it.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
}

fn optics(h: &Headless) -> lightcraft_develop::Optics {
    let id = h.app.session.active().expect("an active photo");
    h.app.session.develop_of(id).expect("its settings").optics
}

#[test]
fn defringe_auto_and_protect_colours_from_the_panel() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 1400.0], 1.0);
    let r = h.request(
        "ui.set",
        json!({"view": "detail", "right": "edit", "openSections": ["optics"], "openFlyouts": ["defringe"], "histogram": false}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(!optics(&h).remove_ca);
    click(&mut h, "button:defringeAuto");
    assert!(optics(&h).remove_ca, "Auto turns Remove Chromatic Aberration on");
    let before = optics(&h).defringe_protect;
    click(&mut h, "check:optics.defringeProtect");
    assert_eq!(optics(&h).defringe_protect, !before, "the checkbox toggles Protect Colours");
}
