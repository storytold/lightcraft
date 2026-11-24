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

fn rect_of(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("{id} shown"))
}

/// Issue #1305: Color Grading is its own section right under Color, Color's tools are distinct
/// rows, and `mask.solo` keeps one mask section open at a time.
#[test]
fn mask_solo_keeps_one_section_open_and_grading_sits_under_color() {
    let mut h = masking();
    for s in ["flyout:maskTint", "flyout:maskMixer", "flyout:maskPointColor"] {
        assert!(has(&mut h, s), "{s} under Color");
    }
    let (color, grading, effects) = (rect_of(&h, "section:mask:color"), rect_of(&h, "section:mask:grading"), rect_of(&h, "section:mask:effects"));
    let point = rect_of(&h, "flyout:maskPointColor");
    assert!(
        color.top() < point.top() && point.top() < grading.top() && grading.top() < effects.top(),
        "Color, its tools, then Color Grading, then Effects"
    );

    // Color Grading folds on its own, Color's tools stay
    req(&mut h, "ui.clickWidget", json!({"id": "section:mask:grading"}));
    h.settle(SETTLE);
    assert!(!has(&mut h, "slider:grade_shadows_sat") && has(&mut h, "flyout:maskMixer"));

    // Solo Mode: turning it on keeps the first open section; opening another folds the rest
    exec(&mut h, "mask.solo", json!({"on": true}));
    h.settle(SETTLE);
    assert!(has(&mut h, "slider:exposure"), "Light (the first open) stays");
    assert!(!has(&mut h, "slider:clarity") && !has(&mut h, "flyout:maskMixer"), "the others fold");
    req(&mut h, "ui.clickWidget", json!({"id": "section:mask:effects"}));
    h.settle(SETTLE);
    assert!(has(&mut h, "slider:clarity") && !has(&mut h, "slider:exposure"), "Effects open, Light folded");
    // {section} solos one even with the mode off; the mode toggles with no params
    exec(&mut h, "mask.solo", json!({}));
    assert!(!h.app.ui.mask_solo);
    exec(&mut h, "mask.solo", json!({"section": "grading"}));
    h.settle(SETTLE);
    assert!(has(&mut h, "slider:grade_shadows_sat") && !has(&mut h, "slider:clarity"));
    req(&mut h, "ui.clickWidget", json!({"id": "section:mask:detail"}));
    h.settle(SETTLE);
    assert!(has(&mut h, "slider:grade_shadows_sat"), "mode off: opening Detail keeps Grading open");

    // hostile arguments are errors, never a panic or a change
    for bad in [json!({"section": "nope"}), json!({"section": 3}), json!({"on": "yes"}), json!({"section": ""})] {
        let r = h.request("engine.execute", json!({"command": "mask.solo", "params": bad}), T);
        assert_eq!(r["ok"], false, "{bad} refused: {r}");
    }
    assert!(!h.app.ui.mask_solo);
}

/// Issue #1301: the Targeted Adjustment buttons and the mask's colour rows draw distinct icons.
#[test]
fn colour_tools_have_distinct_icons() {
    use crate::icons::Icon;
    let draw = |icon: Icon| {
        let mut view = crate::headless::HeadlessView::new();
        let raw = crate::headless::HeadlessView::raw_input(egui::vec2(24.0, 24.0), 1.0, 0.0, vec![]);
        view.run(raw, |ui| {
            ui.painter().rect_filled(ui.max_rect(), 0.0, egui::Color32::BLACK);
            crate::icons::paint(ui.painter(), egui::Rect::from_min_size(egui::pos2(2.0, 2.0), egui::vec2(20.0, 20.0)), icon, egui::Color32::WHITE);
        });
        view.paint(&Default::default()).pixels.iter().map(|p| p.r() > 100).collect::<Vec<_>>()
    };
    let set = [Icon::Target, Icon::Bucket, Icon::Palette, Icon::Picker, Icon::GradeAll];
    for (i, a) in set.iter().enumerate() {
        for b in &set[i + 1..] {
            let (pa, pb) = (draw(*a), draw(*b));
            let differ = pa.iter().zip(&pb).filter(|(x, y)| x != y).count();
            assert!(differ > 20, "{a:?} and {b:?} look alike ({differ} pixels differ)");
        }
    }
}
