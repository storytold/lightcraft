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

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn the_clipping_overlay_follows_j_and_alt_drags_on_the_tone_sliders() {
    let mut h = detail_edit();
    // clipping overlay toggled via j shortcut
    {
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
    // alt drag on tone sliders shows threshold mask
    {
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
    // alt drag on a mask exposure slider shows no clipping mask
    {
        let id = h.app.session.active().expect("active photo");
        let d = (*h.app.session.develop_of(id).unwrap_or_default()).clone();

        // A mask's Exposure slider: same "Exposure" label, but no global tone control is dragged.
        h.app.session.begin_interaction("Exposure").unwrap();
        assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, true), Overlay::None);
        h.app.ui.dragging_control = Some("mask.exposure".to_string());
        assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, true), Overlay::None);
        end_drag(&mut h);
    }
    // alt drag takes precedence over j clipping overlay
    {
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
}

/// The Sharpening sliders each show their own black-and-white view while ⌥/Alt is held on a drag,
/// and nothing without Alt or once the drag ends.
#[test]
fn alt_drags_on_the_sharpening_sliders_show_their_views() {
    use lcc_pipeline::SharpenView;
    let mut h = detail_edit();
    let id = h.app.session.active().expect("active photo");
    let d = (*h.app.session.develop_of(id).unwrap_or_default()).clone();
    for (control, view) in [
        ("detail.sharpenAmount", SharpenView::Amount),
        ("detail.sharpenRadius", SharpenView::Radius),
        ("detail.sharpenDetail", SharpenView::Detail),
        ("detail.sharpenMasking", SharpenView::Masking),
    ] {
        begin_drag(&mut h, control);
        assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, false), Overlay::None, "{control}");
        assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, true), Overlay::Sharpen(view), "{control}");
        end_drag(&mut h);
        assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, true), Overlay::None, "{control} after the drag");
    }
}

/// The Masking panel, one demo photo in Develop.
fn masking_edit() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "masking"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h
}

/// A mask with one radial component, as a pick in the Masking panel's list.
fn radial_mask_settings() -> lcc_develop::DevelopSettings {
    use lcc_develop::{Mask, MaskComponent, MaskOp, MaskShape};
    use lcc_geom::Point;
    let mut d = lcc_develop::DevelopSettings::default();
    d.masks.push(Mask {
        id: 1,
        visible: true,
        components: vec![MaskComponent {
            anchor: None,
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Radial { center: Point::new(0.5, 0.5), rx: 0.2, ry: 0.2, angle: 0.0, feather: 25.0, invert: false },
        }],
        ..Default::default()
    });
    d
}

/// ⌥/Alt held while dragging a mask's own tone sliders (the Masking panel's local adjustments,
/// whose control ids are the bare `LocalAdjustments` field names) shows the same clipping
/// previews as the Edit panel's sliders: Exposure / Highlights / Whites ask for highlight
/// clipping, Shadows / Blacks for shadow clipping. While the preview shows, the mask's region
/// overlay (tint), the outlines and pins (drawn only when not [`adjusting`]) and the J clipping
/// overlay all step aside.
#[test]
fn a_masks_tone_sliders_ask_for_their_clipping_preview() {
    let mut h = masking_edit();
    let d = radial_mask_settings();

    // the mask's region overlay shows on its own (the Show Overlay rule left on), and the
    // slider drag is one the outline painter steps aside for
    h.app.session.active_mask = Some(1);
    h.app.ui.mask_overlay = true;
    for control in ["exposure", "highlights", "whites"] {
        begin_drag(&mut h, control);
        assert!(crate::panels::detail::adjusting(&h.app), "{control}: the drag adjusts");
        assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, false), Overlay::None, "{control} without Alt");
        assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, true), Overlay::Clipping(Some(false)), "{control} with Alt");
        end_drag(&mut h);
    }
    for control in ["shadows", "blacks"] {
        begin_drag(&mut h, control);
        assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, true), Overlay::Clipping(Some(true)), "{control} with Alt");
        end_drag(&mut h);
    }

    // with nothing dragged the mask's region overlay is back, and it yields to the preview again
    // even when the J overlay is on
    let overlay_now = crate::panels::detail::view_overlay(&h.app, &d, false);
    assert!(matches!(overlay_now, Overlay::Mask { .. }), "the mask's region overlay is back, got {overlay_now:?}");
    h.app.ui.show_clipping = true;
    let overlay_j = crate::panels::detail::view_overlay(&h.app, &d, false);
    assert!(matches!(overlay_j, Overlay::Mask { .. }), "with nothing dragged the mask's region overlay keeps precedence over J, got {overlay_j:?}");
    begin_drag(&mut h, "blacks");
    assert_eq!(crate::panels::detail::view_overlay(&h.app, &d, true), Overlay::Clipping(Some(true)), "the Alt preview over the J overlay");
    end_drag(&mut h);
}
