//! Headless tests of the Retouch section (Smooth Skin, Find Blemishes, Teeth), ported from OneSlips.

use std::time::Duration;

use lcc_develop::{SkinArea, SkinSmoothing};
use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn detail(name: &str) -> (lcc_testkit::TempDir, Headless) {
    let dir = lcc_testkit::TempDir::new(&format!("lc-ui-retouch-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut session = lcc_engine::Session::with_demo();
    // a models folder without the models: they are offered, never fetched
    session.segmenter.models_root = Some(dir.to_path_buf());
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 2400.0], 1.0);
    let r = h.request("engine.execute", json!({"command": "view.detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    (dir, h)
}

/// The Smooth Skin section switches it on subtly, offers the models it needs, and sets the skin and the person.
#[test]
fn smooth_skin_and_teeth_sections_switch_on_offer_the_models_and_set_their_values() {
    let (dir, mut h) = detail("skin");
    h.app.ui.right = crate::state::RightPanel::Edit;
    h.settle(SETTLE);
    open_retouch(&mut h, "skin");
    let id = h.app.session.active().unwrap();
    let r = h.request("ui.clickWidget", json!({"id": "button:skinSmooth"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.session.develop_of(id).unwrap().skin, SkinSmoothing::SUBTLE);
    if lcc_engine::segment::Segmenter::AVAILABLE {
        assert!(
            matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { tab }) if tab == "ai"),
            "the missing model is offered: {:?}",
            h.app.ui.dialog
        );
        h.app.ui.dialog = None;
        h.settle(SETTLE);
    }
    for (widget, area) in [("button:skinArea-body", SkinArea::Body), ("button:skinArea-both", SkinArea::Both)] {
        let r = h.request("ui.clickWidget", json!({"id": widget}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.app.ui.dialog = None;
        h.settle(SETTLE);
        assert_eq!(h.app.session.develop_of(id).unwrap().skin.area, area);
    }
    let r = h.request("ui.clickWidget", json!({"id": "button:skinPerson-2"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.dialog = None;
    h.settle(SETTLE);
    assert_eq!(h.app.session.develop_of(id).unwrap().skin.person, 2);
    let r = h.request("ui.clickWidget", json!({"id": "button:skinOff"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!h.app.session.develop_of(id).unwrap().skin.active());
    // a part's sub-section (Teeth): its sliders, the person, its eye and Turn Off
    open_retouch(&mut h, "teeth");
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:retouchPerson-teeth-2"), "the person picker is there");
    let r = h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "retouch.teethWhiten", "value": 35}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.dialog = None;
    h.settle(SETTLE);
    let r = h.request("ui.clickWidget", json!({"id": "button:retouchPerson-teeth-2"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.dialog = None;
    h.settle(SETTLE);
    let t = h.app.session.develop_of(id).unwrap().retouch.teeth;
    assert_eq!((t.whiten, t.person), (35.0, 2));
    let r = h.request("ui.clickWidget", json!({"id": "retouchEye:teeth"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!h.app.session.develop_of(id).unwrap().retouch_section_on("teeth"));
    let r = h.request("ui.clickWidget", json!({"id": "button:retouchOff-teeth"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.dialog = None;
    h.settle(SETTLE);
    let t = h.app.session.develop_of(id).unwrap().retouch.teeth;
    assert!(!t.active() && t.person == 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Open the Edit panel's Retouch section and its sub-section `key`.
fn open_retouch(h: &mut Headless, key: &str) {
    let has = |h: &Headless, w: &str| h.app.widgets.iter().any(|(id, _)| id == w);
    if !has(h, &format!("retouch:{key}")) {
        let r = h.request("ui.clickWidget", json!({"id": "section:retouch"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    }
    if !h.app.ui.flyout_open(&format!("retouch.{key}")) {
        let r = h.request("ui.clickWidget", json!({"id": format!("retouch:{key}")}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
    }
}

/// The Retouch section groups Skin and Blemishes; a sub-section's eye switches it off and on (a
/// before/after), and the section's own eye all of Retouch.
#[test]
fn retouch_section_groups_skin_and_blemishes_and_their_eyes_switch_them() {
    let (dir, mut h) = detail("panel");
    h.app.ui.right = crate::state::RightPanel::Edit;
    h.settle(SETTLE);
    let id = h.app.session.active().unwrap();
    open_retouch(&mut h, "blemishes");
    assert!(h.app.widgets.iter().any(|(w, _)| w == "button:findBlemishes"), "Find Blemishes is in Retouch");
    // Find Blemishes offers the models it needs; Include body skin is remembered
    let r = h.request("ui.clickWidget", json!({"id": "check:blemishBody"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.blemish_body);
    let spots = h.app.session.develop_of(h.app.session.active().unwrap()).unwrap().spots.len();
    let r = h.request("ui.clickWidget", json!({"id": "button:findBlemishes"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    if lcc_engine::segment::Segmenter::AVAILABLE {
        assert!(matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Settings { tab }) if tab == "ai"), "{:?}", h.app.ui.dialog);
    }
    assert_eq!(h.app.session.develop_of(h.app.session.active().unwrap()).unwrap().spots.len(), spots, "nothing added without the models");
    h.app.ui.dialog = None;
    h.settle(SETTLE);
    open_retouch(&mut h, "skin");
    let r = h.request("engine.execute", json!({"command": "skin.set", "params": {"shine": 40}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.dialog = None;
    h.settle(SETTLE);
    assert_eq!(h.app.session.develop_of(id).unwrap().skin.shine, 40.0);
    let r = h.request("ui.clickWidget", json!({"id": "retouchEye:skin"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let d = h.app.session.develop_of(id).unwrap();
    assert!(!d.retouch_section_on("skin") && d.skin.shine == 40.0, "the eye hides it, the settings stay");
    let r = h.request("ui.clickWidget", json!({"id": "retouchEye:skin"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.session.develop_of(id).unwrap().retouch_section_on("skin"));
    // the whole section's eye
    let r = h.request("engine.execute", json!({"command": "develop.sectionEnabled", "params": {"section": "retouch", "enabled": false}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!h.app.session.develop_of(id).unwrap().retouch_section_on("skin"));
    let _ = std::fs::remove_dir_all(&dir);
}
