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
    assert!(h.step_until(Duration::from_secs(120), |h| optics(h).remove_ca), "Auto turns Remove Chromatic Aberration on");
    let before = optics(&h).defringe_protect;
    click(&mut h, "check:optics.defringeProtect");
    assert_eq!(optics(&h).defringe_protect, !before, "the checkbox toggles Protect Colours");
}

/// Review of #4445: Auto measures on a worker, with a row in the activity stack, so the window keeps drawing frames
/// meanwhile; the result lands on the photo it was asked for, as one undo step.
#[test]
fn auto_defringe_measures_in_the_background() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 1400.0], 1.0);
    let r = h.request(
        "ui.set",
        json!({"view": "detail", "right": "edit", "openSections": ["optics"], "openFlyouts": ["defringe"], "histogram": false}),
        T,
    );
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let id = h.app.session.active().expect("an active photo");
    let undo_before = h.app.session.undo.len();
    click(&mut h, "button:defringeAuto");
    // the press returned with the measuring still to come: its row is up and the photo not yet changed
    let rows = h.app.session.activity.list();
    assert!(rows.iter().any(|r| r.kind == "defringe"), "a row in the activity stack while it measures: {rows:?}");
    assert!(!optics(&h).remove_ca, "nothing set before the measuring ends");
    // frames go on meanwhile; then the result arrives, on that photo, in one undo step
    assert!(h.step_until(Duration::from_secs(120), |h| h.app.session.develop_of(id).is_some_and(|d| d.optics.remove_ca)));
    assert!(!h.app.session.activity.list().iter().any(|r| r.kind == "defringe"), "the row goes when it ends");
    assert_eq!(h.app.session.undo.len(), undo_before + 1, "one undo step");
    h.app.run("edit.undo", json!({})).unwrap();
    assert!(!optics(&h).remove_ca, "one undo takes it all back");
}
