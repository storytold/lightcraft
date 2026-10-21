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
