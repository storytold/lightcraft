//! Headless tests of issue #642: the mask sliders in folding sections (Light, Color, Color
//! Grading, Effects, Detail), Color Grading inside a mask, and the bin on a mask's row.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(5);

fn masking() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 2000.0], 1.0);
    req(&mut h, "ui.set", json!({"view": "detail", "moduleBar": false}));
    exec(&mut h, "panel.masking", json!({}));
    exec(&mut h, "mask.add", json!({"kind": "radial"}));
    h.settle(SETTLE);
    h
}

fn req(h: &mut Headless, method: &str, params: serde_json::Value) {
    let r = h.request(method, params, T);
    assert_eq!(r["ok"], true, "{method}: {r}");
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
}

fn has(h: &mut Headless, id: &str) -> bool {
    h.request("ui.widgets", json!({"filter": id}), T).to_string().contains(&format!("\"{id}\""))
}

fn masks(h: &Headless) -> Vec<lcc_develop::Mask> {
    let id = h.app.session.active().expect("active photo");
    h.app.session.develop_of(id).unwrap_or_default().masks.clone()
}

#[test]
fn sections_fold_grading_sets_its_zone_and_the_row_bin_deletes() {
    let mut h = masking();
    for s in ["section:mask:light", "section:mask:color", "section:mask:grading", "section:mask:effects", "section:mask:detail"] {
        assert!(has(&mut h, s), "{s} shown");
    }
    assert!(has(&mut h, "slider:exposure") && has(&mut h, "slider:clarity"));
    req(&mut h, "ui.clickWidget", json!({"id": "section:mask:light"}));
    h.settle(SETTLE);
    assert!(!has(&mut h, "slider:exposure"), "Light folded: no Exposure");
    assert!(!has(&mut h, "flyout:maskCurve"), "Light folded: no Curve either");
    assert!(has(&mut h, "slider:clarity"), "the other sections stay open");
    req(&mut h, "ui.clickWidget", json!({"id": "section:mask:light"}));
    h.settle(SETTLE);
    assert!(has(&mut h, "slider:exposure"), "Light open again");

    // Color Grading in a mask sets the picked zone
    assert!(has(&mut h, "slider:grade_shadows_sat"), "Shadows is the first zone");
    req(&mut h, "ui.clickWidget", json!({"id": "button:maskGradeZone:highlights"}));
    h.settle(SETTLE);
    assert!(has(&mut h, "slider:grade_highlights_sat") && !has(&mut h, "slider:grade_shadows_sat"));
    // a drag to the right along the saturation track
    req(&mut h, "ui.dragWidget", json!({"id": "slider:grade_highlights_sat", "fx": 0.1, "dx": 600.0, "steps": 8}));
    h.settle(SETTLE);
    let a = masks(&h)[0].adjust;
    assert!(a.grade_highlights_sat > 50.0, "highlights saturation raised: {}", a.grade_highlights_sat);
    assert_eq!(a.grade_shadows_sat, 0.0, "other zones untouched");
    // and `mask.adjust` clamps grading values
    exec(&mut h, "mask.adjust", json!({"values": {"grade_midtones_hue": 720, "grade_midtones_sat": 150, "grade_global_lum": -400}}));
    let a = masks(&h)[0].adjust;
    assert_eq!((a.grade_midtones_hue, a.grade_midtones_sat, a.grade_global_lum), (360.0, 100.0, -100.0));

    // the bin on a hovered mask row deletes that mask
    exec(&mut h, "mask.add", json!({"kind": "linear"}));
    h.settle(SETTLE);
    let list = masks(&h);
    assert_eq!(list.len(), 2);
    let first = list[0].id;
    req(&mut h, "ui.move", json!({"x": 5.0, "y": 5.0}));
    h.settle(SETTLE);
    assert!(!has(&mut h, &format!("maskDeleteRow:{first}")), "the bin shows only on hover");
    req(&mut h, "ui.hoverWidget", json!({"id": format!("mask:{first}")}));
    h.settle(SETTLE);
    req(&mut h, "ui.clickWidget", json!({"id": format!("maskDeleteRow:{first}")}));
    h.settle(SETTLE);
    let left = masks(&h);
    assert_eq!(left.len(), 1, "one mask deleted");
    assert_ne!(left[0].id, first, "the hovered one");
}

/// Issue #1145: the Color swatch under Saturation opens a tint (hue in degrees, strength) that
/// the mask takes; with no strength there is no tint.
#[test]
fn the_color_swatch_tints_the_mask() {
    let mut h = masking();
    assert!(has(&mut h, "maskTint:swatch"), "the swatch row is under the Color sliders");
    assert!(!has(&mut h, "slider:color_sat"), "the tint sliders wait for the row");
    req(&mut h, "ui.clickWidget", json!({"id": "flyout:maskTint"}));
    h.settle(SETTLE);
    assert!(has(&mut h, "slider:color_hue") && has(&mut h, "slider:color_sat"));
    req(&mut h, "ui.dragWidget", json!({"id": "slider:color_sat", "fx": 0.1, "dx": 600.0, "steps": 8}));
    h.settle(SETTLE);
    let a = masks(&h)[0].adjust;
    assert!(a.color_sat > 50.0, "tint strength raised: {}", a.color_sat);
    assert!(crate::panels::masking::tint_swatch(a.color_hue, a.color_sat).is_some());
    assert!(crate::panels::masking::tint_swatch(200.0, 0.0).is_none(), "no strength, no tint");
    assert!(crate::panels::masking::tint_swatch(f64::NAN, 50.0).is_none());
}
