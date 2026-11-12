//! Headless tests of Convert Negative: Photo ▸ Convert Negative switches the conversion on and
//! opens its panel, Simple / Advanced switch the panel, and the Film Base selector samples the
//! photo where it is clicked.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{RightPanel, ViewMode};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn negative(h: &Headless) -> lightcraft_develop::Negative {
    let id = h.app.session.active().unwrap();
    h.app.session.develop_of(id).unwrap().negative.clone()
}

#[test]
fn photo_menu_opens_the_panel_and_the_selector_picks_the_film_base() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let tree = h.request("ui.menu.tree", json!({}), T);
    assert!(tree.to_string().contains("Convert Negative"), "listed in the menus");
    let r = h.request("ui.menu.invoke", json!({"command": "photo.convertNegative"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!((h.app.ui.right, h.app.ui.view), (RightPanel::Negative, ViewMode::Detail));
    let n = negative(&h);
    assert!(n.enabled && !n.advanced && n.measured.is_some(), "{n:?}");
    assert!(!h.app.widgets.iter().any(|(w, _)| w == "icon:negBase"), "Simple has no selector");

    let r = h.request("ui.clickWidget", json!({"id": "button:negativeMode-advanced"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(negative(&h).advanced);
    assert!(negative(&h).base.is_none());

    let r = h.request("ui.clickWidget", json!({"id": "icon:negBase"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.tool, "negBase");
    let img = h.app.image_rect.expect("the photo is on screen");
    let at = img.left_top() + egui::vec2(4.0, img.height() / 2.0);
    h.request("ui.click", json!({"x": at.x, "y": at.y}), T);
    h.settle(SETTLE);
    assert!(negative(&h).base.is_some(), "the click picked the film base");
    assert_eq!(h.app.ui.tool, "", "one pick, then the selector is put away");

    // the panel's Clear button takes the picked base back
    let r = h.request("ui.clickWidget", json!({"id": "button:negative-clearBase"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(negative(&h).base.is_none());

    // the checkbox switches the conversion off, keeping its settings
    let r = h.request("ui.clickWidget", json!({"id": "check:negativeOn"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let n = negative(&h);
    assert!(!n.enabled && n.advanced && n.measured.is_some(), "{n:?}");
}

fn demo() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    Headless::new(app, [1400.0, 900.0], 1.0)
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.settle(SETTLE);
}

/// The Gray Point selector, as the Film Base one; leaving Advanced or switching the conversion off
/// puts an armed selector away, and switching modes while off leaves it off.
#[test]
fn the_gray_point_selector_and_the_modes() {
    let mut h = demo();
    h.request("ui.menu.invoke", json!({"command": "photo.convertNegative"}), T);
    h.settle(SETTLE);
    click(&mut h, "button:negativeMode-advanced");
    click(&mut h, "icon:negGray");
    assert_eq!(h.app.ui.tool, "negGray");
    let img = h.app.image_rect.expect("the photo is on screen");
    let at = img.center();
    h.request("ui.click", json!({"x": at.x, "y": at.y}), T);
    h.settle(SETTLE);
    let n = negative(&h);
    let gray = n.gray.expect("the click picked a gray point");
    assert!(gray.iter().all(|v| (0.0..=1.0).contains(v)), "{gray:?}");
    assert!(n.base.is_none(), "the film base is left alone");
    assert_eq!(h.app.ui.tool, "");

    // an armed selector goes when its row does
    click(&mut h, "icon:negBase");
    assert_eq!(h.app.ui.tool, "negBase");
    click(&mut h, "button:negativeMode-simple");
    assert_eq!(h.app.ui.tool, "", "Simple has no selector to leave armed");
    assert!(!negative(&h).advanced);

    // switching modes while off leaves the conversion off
    click(&mut h, "check:negativeOn");
    assert!(!negative(&h).enabled);
    click(&mut h, "button:negativeMode-advanced");
    let n = negative(&h);
    assert!(n.advanced && !n.enabled, "{n:?}");
}

/// Done and Return close the panel back to Edit, also while the conversion is off; the Photo menu
/// entry keeps a conversion that is on, and measures one that has no measurement.
#[test]
fn done_return_and_the_menu_entry_again() {
    let mut h = demo();
    h.request("ui.menu.invoke", json!({"command": "photo.convertNegative"}), T);
    h.settle(SETTLE);
    h.request("engine.execute", json!({"command": "develop.negative", "params": {"preset": "Gold 200"}}), T);
    let before = negative(&h);
    let r = h.request("ui.menu.invoke", json!({"command": "photo.convertNegative"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(negative(&h), before, "invoking it again changes nothing");

    h.request("ui.menu.invoke", json!({"command": "tool.done"}), T);
    h.settle(SETTLE);
    assert_eq!(h.app.ui.right, RightPanel::Edit, "Return leaves the panel");

    // a conversion that is on but was never measured (pasted, or saved by an older build)
    let id = h.app.session.active().unwrap();
    let mut d = (*h.app.session.develop_of(id).unwrap()).clone();
    d.negative.measured = None;
    h.app.session.set_develop(id, d, "test").unwrap();
    h.request("ui.menu.invoke", json!({"command": "photo.convertNegative"}), T);
    h.settle(SETTLE);
    assert!(negative(&h).measured.is_some(), "measured on the way in");
    assert_eq!(h.app.ui.right, RightPanel::Negative);

    // off, Done is still there
    click(&mut h, "check:negativeOn");
    click(&mut h, "button:negative-done");
    assert_eq!(h.app.ui.right, RightPanel::Edit);
}

#[test]
fn without_a_photo_the_menu_entry_is_an_error() {
    let app = LightcraftApp::new(lightcraft_engine::Session::new(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.menu.invoke", json!({"command": "photo.convertNegative"}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert_ne!(h.app.ui.right, RightPanel::Negative);
}
