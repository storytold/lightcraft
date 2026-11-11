//! Headless tests of the mode dropdowns in the Crop and Remove panels: Overlay, Upright and the
//! Remove mode are a label with a dropdown beside it (as White Balance is), not rows of buttons.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::i18n::Locale;
use crate::state::{CropOverlay, RightPanel};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// The demo library in Detail with `panel` (a `panel.*` command) open.
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

/// Click the widget registered as `id`.
fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
}

/// Open the dropdown `id` and choose its item `key`.
fn choose(h: &mut Headless, id: &str, key: &str) {
    click(h, &format!("dropdown:{id}"));
    click(h, &format!("{id}:{key}"));
    h.step();
}

/// Whether a widget whose id starts with `prefix` was drawn in the last frame.
fn has_widget(h: &Headless, prefix: &str) -> bool {
    h.app.widgets.iter().any(|(id, _)| id.starts_with(prefix))
}

/// Where the widget `id` was drawn in the last frame.
fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

/// Whether `text` was painted inside the widget `id` in the last frame.
fn shows(h: &Headless, id: &str, text: &str) -> bool {
    let r = rect(h, id);
    h.view.painted_shapes().iter().any(|s| matches!(s, egui::Shape::Text(t) if t.galley.job.text == text && r.contains(t.pos)))
}

/// Overlay and Upright are dropdowns that set the overlay, the Upright mode and (for Guided) the
/// guide tool, as their buttons did.
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

/// The overlay names are translated in the open menu and in the closed dropdown.
#[test]
fn crop_overlay_dropdown_is_translated() {
    let mut h = detail("panel.crop");
    let r = h.request("ui.set", json!({"language": Locale::De.code()}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let golden = Locale::De.tr("Golden Ratio");
    assert_ne!(golden, "Golden Ratio", "German has its own name for the overlay");
    click(&mut h, "dropdown:cropOverlay");
    h.step();
    assert!(shows(&h, "cropOverlay:golden", golden), "menu item {golden}");
    click(&mut h, "cropOverlay:golden");
    h.step();
    assert_eq!(h.app.ui.crop_overlay, CropOverlay::Golden);
    assert!(!has_widget(&h, "cropOverlay:golden"), "the menu closed");
    assert!(shows(&h, "dropdown:cropOverlay", golden), "closed dropdown says {golden}");
    crate::i18n::set_language(Locale::En);
}

/// Like White Balance, a mode dropdown is as wide as the chosen item, so the chevron follows the
/// text instead of leaving a gap after a short name.
#[test]
fn a_mode_dropdown_fits_the_chosen_item() {
    let mut h = detail("panel.crop");
    for (id, short, long) in [("cropOverlay", "thirds", "golden"), ("upright", "off", "vertical")] {
        choose(&mut h, id, short);
        let narrow = rect(&h, &format!("dropdown:{id}")).width();
        choose(&mut h, id, long);
        let wide = rect(&h, &format!("dropdown:{id}")).width();
        assert!(narrow < wide, "{id}: {short} ({narrow}) is narrower than {long} ({wide})");
        choose(&mut h, id, short);
        assert_eq!(rect(&h, &format!("dropdown:{id}")).width(), narrow, "{id}: back to {short}");
    }
    let mut h = detail("panel.remove");
    choose(&mut h, "removeMode", "none");
    let narrow = rect(&h, "dropdown:removeMode").width();
    choose(&mut h, "removeMode", "fill");
    assert!(narrow < rect(&h, "dropdown:removeMode").width(), "None is narrower than Content-Aware");
}

/// The number of spots on the active photo.
fn spots(h: &Headless) -> usize {
    let id = h.app.session.active().expect("active photo");
    h.app.session.develop_of(id).unwrap_or_default().spots.len()
}

/// A click on the photo at (`x`, `y`), normalized image coordinates.
fn click_photo(h: &mut Headless, x: f64, y: f64) {
    let r = h.request("ui.pointer", json!({"events": [{"kind": "down", "x": x, "y": y}, {"kind": "up", "x": x, "y": y}]}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
}

/// The Remove mode is a dropdown. None says no brush is chosen, and then a click on the photo adds
/// no spot; the panel opens with Remove, which does.
#[test]
fn remove_mode_is_a_dropdown_with_none() {
    let mut h = detail("panel.remove");
    assert_eq!(h.app.ui.right, RightPanel::Remove);
    assert!(has_widget(&h, "dropdown:removeMode"));
    assert!(!has_widget(&h, "button:removeMode-"), "no row of mode buttons");
    assert_eq!(h.app.ui.tool, "remove", "the panel opens ready to paint");
    assert!(shows(&h, "dropdown:removeMode", "Remove"));
    for (key, tool) in [("heal", "heal"), ("fill", "fill"), ("clone", "clone"), ("none", ""), ("remove", "remove")] {
        choose(&mut h, "removeMode", key);
        assert_eq!(h.app.ui.tool, tool, "{key}");
    }
    click_photo(&mut h, 0.3, 0.3);
    assert_eq!(spots(&h), 1, "Remove paints a spot");
    choose(&mut h, "removeMode", "none");
    assert!(h.app.ui.tool.is_empty());
    assert!(shows(&h, "dropdown:removeMode", "None"), "the dropdown says no brush is chosen");
    click_photo(&mut h, 0.7, 0.7);
    assert_eq!(spots(&h), 1, "None paints nothing");
}

/// Visualize Spots (A) from another panel opens Remove ready to paint, as the Remove button does;
/// in Remove it only toggles the view and keeps the Mode chosen there, None included.
#[test]
fn visualize_spots_opens_remove_with_a_brush() {
    let mut h = detail("panel.crop");
    let r = h.request("ui.menu.invoke", json!({"id": "view.visualizeSpots"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_eq!(h.app.ui.right, RightPanel::Remove);
    assert!(h.app.ui.visualize_spots);
    assert_eq!(h.app.ui.tool, "remove", "the panel opens ready to paint");
    choose(&mut h, "removeMode", "none");
    let r = h.request("ui.menu.invoke", json!({"id": "view.visualizeSpots"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(!h.app.ui.visualize_spots);
    assert!(h.app.ui.tool.is_empty(), "None stays chosen");
}
