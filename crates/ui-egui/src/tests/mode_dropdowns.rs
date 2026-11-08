//! Headless tests of the mode dropdowns in the Crop and Remove panels: Overlay, Upright and the
//! Remove mode are a label with a dropdown beside it (as White Balance is), not rows of buttons.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{CropOverlay, RightPanel};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn detail(panel: &str) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    // tall enough that the Geometry section shows without scrolling the panel
    let mut h = Headless::new(app, [1400.0, 1400.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": panel}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h
}

/// Open the dropdown `id` and choose its item `key`.
fn choose(h: &mut Headless, id: &str, key: &str) {
    let r = h.request("ui.clickWidget", json!({"id": format!("dropdown:{id}")}), T);
    assert_eq!(r["ok"], true, "dropdown:{id}: {r}");
    let r = h.request("ui.clickWidget", json!({"id": format!("{id}:{key}")}), T);
    assert_eq!(r["ok"], true, "{id}:{key}: {r}");
    h.step();
}

fn has_widget(h: &Headless, prefix: &str) -> bool {
    h.app.widgets.iter().any(|(id, _)| id.starts_with(prefix))
}

#[test]
fn crop_overlay_and_upright_are_dropdowns() {
    let mut h = detail("panel.crop");
    assert_eq!(h.app.ui.right, RightPanel::Crop);
    for id in ["cropOverlay", "upright"] {
        assert!(has_widget(&h, &format!("dropdown:{id}")), "dropdown:{id}");
        assert!(!has_widget(&h, &format!("button:{id}-")), "no row of {id} buttons");
    }
    choose(&mut h, "cropOverlay", "golden");
    assert_eq!(h.app.ui.crop_overlay, CropOverlay::Golden);
    choose(&mut h, "upright", "guided");
    let id = h.app.session.active().expect("active photo");
    assert_eq!(h.app.session.develop_of(id).unwrap_or_default().geometry.upright, lightcraft_develop::Upright::Guided);
    assert_eq!(h.app.ui.tool, "guidedUpright", "Guided starts drawing guides, as the button did");
    choose(&mut h, "upright", "off");
    assert_eq!(h.app.session.develop_of(id).unwrap_or_default().geometry.upright, lightcraft_develop::Upright::Off);
    assert!(h.app.ui.tool.is_empty());
}

#[test]
fn remove_mode_is_a_dropdown() {
    let mut h = detail("panel.remove");
    assert_eq!(h.app.ui.right, RightPanel::Remove);
    assert!(has_widget(&h, "dropdown:removeMode"));
    assert!(!has_widget(&h, "button:removeMode-"), "no row of mode buttons");
    for (key, tool) in [("heal", "heal"), ("fill", "fill"), ("clone", "clone"), ("remove", "remove")] {
        choose(&mut h, "removeMode", key);
        assert_eq!(h.app.ui.tool, tool);
    }
}
