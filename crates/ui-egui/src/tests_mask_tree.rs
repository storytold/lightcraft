//! The Masks list as a tree: each mask's components hang under it as
//! branches, with how each one combines (+ − ∩) in a column of its own.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn masking() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 1000.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "moduleBar": false}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": "panel.masking"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    h.settle(SETTLE);
    r["result"].clone()
}

fn masks(h: &Headless) -> Vec<lcc_develop::Mask> {
    let id = h.app.session.active().expect("a photo");
    h.app.session.develop_of(id).expect("settings").masks.clone()
}

fn count(h: &Headless, id: &str) -> usize {
    h.app.widgets.iter().filter(|(w, _)| w == id).count()
}

/// No bundled font has ∩, so the sign was painted as an empty box (a "tofu" glyph). It is drawn as a
/// vector now: lit over the top and down both legs, open at the bottom, where a box would be closed.
#[test]
fn the_intersect_sign_is_drawn_open_at_the_bottom_not_as_a_missing_glyph_box() {
    let mut h = masking();
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.5, 0.5]}));
    exec(&mut h, "mask.addComponent", json!({"op": "intersect", "kind": "linear", "start": [0.5, 0.0], "end": [0.5, 0.4]}));
    h.step();
    let op = h.app.widgets.iter().find(|(w, _)| w == "componentOp:1").map(|(_, r)| *r).expect("the intersect sign");
    let image = h.paint();
    let w = image.size[0];
    let lum = |x: f32, y: f32| {
        let p = image.pixels[y.round() as usize * w + x.round() as usize];
        (p.r() as u32 + p.g() as u32 + p.b() as u32) / 3
    };
    let bg = lum(op.left() + 0.5, op.bottom() - 0.5);
    let lit = |x: f32, y0: f32, y1: f32| (y0.round() as i32..=y1.round() as i32).any(|y| lum(x, y as f32) > bg + 40);
    let c = op.center();
    assert!(lit(c.x, op.top(), c.y), "the arc over the top is drawn");
    assert!(lit(c.x - 3.0, c.y, op.bottom() - 1.0) && lit(c.x + 3.0, c.y, op.bottom() - 1.0), "both legs are drawn");
    assert!(!lit(c.x, c.y + 0.5, op.bottom() - 0.5), "open at the bottom: nothing between the legs below the arc");
}

#[test]
fn component_rows_show_their_operation_invert_alone_and_the_mask_renames() {
    let mut h = masking();
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.5, 0.5]}));
    exec(&mut h, "mask.addComponent", json!({"op": "subtract", "kind": "linear", "start": [0.5, 0.0], "end": [0.5, 0.4]}));
    h.step();
    assert_eq!(masks(&h)[0].components.len(), 2);
    let at = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no {id}"));
    assert_eq!(count(&h, "componentOp:0"), 0, "the first component has no sign");
    let op = at(&h, "componentOp:1");
    let row = at(&h, "component:1");
    let thumb = at(&h, "componentIcon:1");
    assert!(row.contains_rect(op), "the sign is on its row");
    assert!(op.right() <= thumb.left(), "the sign before the thumbnail, in a column of its own");
    // the sign and the thumbnail line up on every row
    assert!((at(&h, "componentIcon:0").left() - thumb.left()).abs() < 0.5);

    // each component row inverts that component alone
    let before = masks(&h)[0].invert;
    let edited = h.app.ui.active_mask_component;
    let r = h.request("ui.clickWidget", json!({"id": "componentInvert:1"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let m = &masks(&h)[0];
    assert!(m.components[1].invert && !m.components[0].invert, "only that component");
    assert_eq!(m.invert, before, "the mask itself is left alone");
    assert_eq!(h.app.ui.active_mask_component, edited, "inverting doesn't switch the component edited");
    let r = h.request("ui.clickWidget", json!({"id": "componentInvert:1"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!masks(&h)[0].components[1].invert, "a second click turns it back");

    // clearing a mask's name gives it back its automatic name
    let automatic = masks(&h)[0].name.clone();
    exec(&mut h, "mask.rename", json!({"name": "Vignette"}));
    assert_eq!(masks(&h)[0].name, "Vignette");
    let mid = masks(&h)[0].id;
    let r = h.request("ui.clickWidget", json!({"id": format!("mask:{mid}"), "count": 2}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(Duration::from_secs(5));
    assert_eq!(h.app.ui.renaming_mask.as_ref().map(|r| r.0), Some(mid), "double-click starts renaming");
    h.request("ui.key", json!({"key": "A", "cmd": true}), T);
    h.request("ui.key", json!({"key": "Backspace"}), T);
    h.request("ui.key", json!({"key": "Enter"}), T);
    h.settle(Duration::from_secs(5));
    assert!(h.app.ui.renaming_mask.is_none());
    assert_eq!(masks(&h)[0].name, automatic, "an empty name: the automatic one again");
}

#[test]
fn component_rows_are_named_after_their_source() {
    use crate::panels::masking::component_label;
    use lcc_develop::recipe::Who;
    use lcc_develop::{MaskComponent, MaskShape};
    let comp = |shape: MaskShape| MaskComponent { name: None, anchor: None, op: lcc_develop::MaskOp::Add, invert: false, shape };
    let groom = MaskShape::People { person: 0, parts: vec!["Clothes".into(), "Hair".into()], who: Some(Who::Role("Groom".into())), seg: None };
    assert_eq!(component_label(&comp(groom)), "Groom - Clothes, Hair", "a person's role and parts");
    assert_eq!(component_label(&comp(MaskShape::Concept { name: "Wedding dress".into(), seg: None })), "Wedding dress", "a taught object's name");
    assert_eq!(component_label(&comp(MaskShape::Landscape { class: "water".into() })), "Water", "a landscape part's class");
    assert_eq!(component_label(&comp(MaskShape::Sky)), "Sky");
    // a name the photographer gave wins; an empty one doesn't count
    let named = MaskComponent { name: Some("Veil".into()), ..comp(MaskShape::Sky) };
    assert_eq!(component_label(&named), "Veil");
    let blank = MaskComponent { name: Some("  ".into()), ..comp(MaskShape::Sky) };
    assert_eq!(component_label(&blank), "Sky");
}

#[test]
fn only_the_selected_masks_branch_opens_by_itself() {
    let mut h = masking();
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.3, 0.5]}));
    exec(&mut h, "mask.add", json!({"kind": "linear", "start": [0.5, 0.0], "end": [0.5, 0.4]}));
    let [first, second] = [masks(&h)[0].id, masks(&h)[1].id];
    assert_eq!(h.app.session.active_mask, Some(second));
    h.step();
    let at = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no {id}"));
    assert_eq!(count(&h, "component:0"), 1, "one branch open: the selected mask's");
    assert!(at(&h, "component:0").top() >= at(&h, &format!("mask:{second}")).bottom(), "under the selected mask");
    // the chevron opens another mask's branch as well
    let r = h.request("ui.clickWidget", json!({"id": format!("button:maskCollapse:{first}")}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(count(&h, "component:0"), 2, "both branches open");
    let r = h.request("ui.clickWidget", json!({"id": format!("button:maskCollapse:{first}")}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(count(&h, "component:0"), 1, "and folds it again");
    // selecting the other mask moves the open branch to it
    exec(&mut h, "mask.select", json!({"id": first}));
    h.step();
    assert_eq!(count(&h, "component:0"), 1);
    assert!(at(&h, "component:0").bottom() <= at(&h, &format!("mask:{second}")).top(), "under the first mask now");
}

#[test]
fn the_eye_is_always_on_the_mask_row_and_switches_its_effect() {
    let mut h = masking();
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.5, 0.5]}));
    exec(&mut h, "mask.adjust", json!({"values": {"exposure": 1.0}}));
    let mid = masks(&h)[0].id;
    // the pointer away from the list
    let r = h.request("ui.move", json!({"x": 5.0, "y": 5.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert_eq!(count(&h, &format!("maskVisible:{mid}")), 1, "the eye shows without the pointer on the row");
    let r = h.request("ui.clickWidget", json!({"id": format!("maskVisible:{mid}")}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!masks(&h)[0].visible, "the eye turns the mask's effect off");
    assert_eq!(masks(&h)[0].adjust.exposure, 1.0, "and keeps its settings");
}

/// Two radial components each show a Feather slider under their row; each slider has an egui id of
/// its own (they shared one, and a debug build painted "First use of widget ID" / "Second use of
/// widget ID" over the component rows below them).
#[test]
fn two_radial_components_have_feather_sliders_of_their_own() {
    let mut h = masking();
    exec(&mut h, "mask.add", json!({"kind": "radial", "center": [0.5, 0.5]}));
    exec(&mut h, "mask.addComponent", json!({"op": "subtract", "kind": "radial", "center": [0.5, 0.5], "rx": 0.1, "ry": 0.1}));
    h.step();
    h.step();
    assert_eq!(masks(&h)[0].components.len(), 2);
    let text = h.painted_text();
    assert!(text.iter().filter(|s| s.as_str() == "Feather").count() >= 2, "both radial components show Feather: {text:?}");
    let clash: Vec<_> = text.iter().filter(|s| s.contains("use of widget ID")).collect();
    assert!(clash.is_empty(), "no egui id clash: {clash:?}");
}
