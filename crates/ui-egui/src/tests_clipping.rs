//! Tests for issue #94: per-pixel clipping overlay (toggle via J shortcut / histogram indicators
//! and interactive Alt / Option + drag on tone sliders).

use lcc_pipeline::Overlay;
use serde_json::json;
use std::time::Duration;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);

fn detail_edit() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h
}

/// What the slider does when a drag starts: an undo interaction plus the dragged control's id.
fn begin_drag(h: &mut Headless, control: &str) {
    h.app.session.begin_interaction("Drag").unwrap();
    h.app.ui.dragging_control = Some(control.to_string());
}

fn end_drag(h: &mut Headless) {
    h.app.session.end_interaction().unwrap();
    h.app.ui.dragging_control = None;
}

#[test]
fn clipping_overlay_toggled_via_j_shortcut() {
    let mut h = detail_edit();
    let id = h.app.session.active().expect("active photo");
    let d = (*h.app.session.develop_of(id).unwrap_or_default()).clone();

    assert!(!h.app.ui.show_clipping);
    assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, false), Overlay::None);

    // Press 'J' to toggle clipping on
    let r = h.request("ui.key", json!({"key": "j"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.app.ui.show_clipping);
    assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, false), Overlay::Clipping(None));

    // Press 'J' again to toggle clipping off
    let r = h.request("ui.key", json!({"key": "j"}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(!h.app.ui.show_clipping);
    assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, false), Overlay::None);
}

#[test]
fn alt_drag_on_tone_sliders_shows_threshold_mask() {
    let mut h = detail_edit();
    let id = h.app.session.active().expect("active photo");
    let d = (*h.app.session.develop_of(id).unwrap_or_default()).clone();

    // Highlights / Exposure / Whites:
    for control in ["light.exposure", "light.highlights", "light.whites"] {
        begin_drag(&mut h, control);
        // Without alt held: no clipping overlay
        assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, false), Overlay::None);
        // With alt held: highlights clipping threshold (Some(false))
        assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, true), Overlay::Clipping(Some(false)));
        end_drag(&mut h);
        // Once interaction ends: no clipping overlay even if alt is held
        assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, true), Overlay::None);
    }

    // Shadows / Blacks:
    for control in ["light.shadows", "light.blacks"] {
        begin_drag(&mut h, control);
        // Without alt held: no clipping overlay
        assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, false), Overlay::None);
        // With alt held: shadows clipping threshold (Some(true))
        assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, true), Overlay::Clipping(Some(true)));
        end_drag(&mut h);
        // Once interaction ends: no clipping overlay even if alt is held
        assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, true), Overlay::None);
    }
}

#[test]
fn alt_drag_takes_precedence_over_j_clipping_overlay() {
    let mut h = detail_edit();
    let id = h.app.session.active().expect("active photo");
    let d = (*h.app.session.develop_of(id).unwrap_or_default()).clone();

    // Enable J overlay
    h.app.ui.show_clipping = true;
    assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, false), Overlay::Clipping(None));

    // Alt-drag Exposure overrides to highlight threshold
    begin_drag(&mut h, "light.exposure");
    assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, true), Overlay::Clipping(Some(false)));
    // Without Alt, it still shows J overlay
    assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, false), Overlay::Clipping(None));

    // Alt-drag Shadows overrides to shadow threshold
    end_drag(&mut h);
    begin_drag(&mut h, "light.shadows");
    assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, true), Overlay::Clipping(Some(true)));

    // End interaction reverts to J overlay
    end_drag(&mut h);
    assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, false), Overlay::Clipping(None));
}

#[test]
fn alt_drag_on_a_mask_exposure_slider_shows_no_clipping_mask() {
    let mut h = detail_edit();
    let id = h.app.session.active().expect("active photo");
    let d = (*h.app.session.develop_of(id).unwrap_or_default()).clone();

    // A mask's Exposure slider: same "Exposure" label, but no global tone control is dragged.
    h.app.session.begin_interaction("Exposure").unwrap();
    assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, true), Overlay::None);
    h.app.ui.dragging_control = Some("mask.exposure".to_string());
    assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, true), Overlay::None);
    end_drag(&mut h);
}
