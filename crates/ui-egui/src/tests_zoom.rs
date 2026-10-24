//! Pixel-accurate zoom, pane clipping and anchored horizontal scrub navigation.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The demo library in a 1400×900 window at 1 px per point, the first photo open in Detail.
fn detail() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "none", "filmstrip": false}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn space(pressed: bool) -> egui::Event {
    egui::Event::Key { key: egui::Key::Space, physical_key: Some(egui::Key::Space), pressed, repeat: false, modifiers: egui::Modifiers::NONE }
}

fn edit(h: &mut Headless, f: impl FnOnce(&mut lightcraft_develop::DevelopSettings)) {
    let id = h.app.session.active().expect("a photo is open");
    let mut s = (*h.app.session.develop_of(id).expect("settings")).clone();
    f(&mut s);
    h.app.session.set_develop(id, s, "Test").expect("edit");
}

fn at_100(h: &mut Headless) -> (f32, f32) {
    h.request("engine.execute", json!({"command": "view.zoom100"}), T);
    h.settle(SETTLE);
    let r = h.app.image_rect.expect("the loupe drew the photo");
    (r.width(), r.height())
}

fn size_of(h: &Headless) -> (f32, f32) {
    let id = h.app.session.active().expect("a photo is open");
    let p = h.app.session.catalog.photo(id).expect("photo");
    (p.width as f32, p.height as f32)
}

// Given an uncropped photo, 100 % is its pixel size
#[test]
fn an_uncropped_photo_at_100_is_its_pixel_size() {
    let mut h = detail();
    let (nw, nh) = size_of(&h);
    let (w, hh) = at_100(&mut h);
    assert!((w - nw).abs() < 1.5 && (hh - nh).abs() < 1.5, "{w}×{hh} for {nw}×{nh}");
}

// Given a crop to half the width and a third of the height, 100 % is the cropped pixels
#[test]
fn a_cropped_photo_at_100_is_the_crops_pixel_size() {
    let mut h = detail();
    let (nw, nh) = size_of(&h);
    edit(&mut h, |s| s.crop.geometry.rect = lightcraft_geom::Rect { x0: 0.25, y0: 0.2, x1: 0.75, y1: 0.2 + 1.0 / 3.0 });
    let (w, hh) = at_100(&mut h);
    assert!((w - nw / 2.0).abs() < 1.5 && (hh - nh / 3.0).abs() < 1.5, "{w}×{hh} for a crop of {}×{}", nw / 2.0, nh / 3.0);
}

// Given a photo rotated a quarter turn, 100 % is its pixel size with the axes swapped
#[test]
fn a_rotated_photo_at_100_is_its_pixel_size_swapped() {
    let mut h = detail();
    let (nw, nh) = size_of(&h);
    edit(&mut h, |s| s.orientation = lightcraft_geom::Orientation::Rotate90);
    let (w, hh) = at_100(&mut h);
    assert!((w - nh).abs() < 1.5 && (hh - nw).abs() < 1.5, "{w}×{hh} for {nh}×{nw}");
}

/// Zoomed in, side by side and stacked Before / After views each stay in their pane: the margins
/// around the panes and the gap between them show the background, as at Fit. The whole-frame
/// renders used to be drawn unclipped, so the After's (magnified, blurred) covered the Before
/// pane's margins.
#[test]
fn zoomed_before_after_views_stay_in_their_panes() {
    use crate::state::{BeforeAfter, Zoom};
    let mut h = detail();
    for mode in [BeforeAfter::SideBySide, BeforeAfter::TopBottom] {
        let mut shots = Vec::new();
        for zoom in [Zoom::Fit, Zoom::Percent(100.0)] {
            h.app.ui.before_after = mode;
            h.app.ui.zoom = zoom;
            h.app.ui.pan = (0.3, 0.4);
            h.settle(SETTLE);
            let img = h.app.image_rect.expect("After view on screen");
            let canvas = h.app.canvas_rect.expect("canvas");
            shots.push((h.paint(), img, canvas));
        }
        let (zoomed, img, canvas) = &shots[1];
        assert!(!canvas.contains_rect(*img), "{mode:?}: 100 % reaches past the pane ({img:?})");
        // outside both panes: the canvas' edges and the gap between the panes
        let c = canvas.center();
        let points = [
            egui::pos2(canvas.left() + 4.0, c.y),
            egui::pos2(c.x, canvas.top() + 4.0),
            egui::pos2(c.x, canvas.bottom() - 4.0),
            match mode {
                BeforeAfter::SideBySide => c,
                _ => egui::pos2(canvas.left() + 200.0, c.y),
            },
        ];
        for q in points {
            let px = |shot: &egui::ColorImage| shot[(q.x as usize, q.y as usize)];
            assert_eq!(px(zoomed), px(&shots[0].0), "{mode:?}: background at {q:?}, zoomed in as at Fit");
        }
    }
}

/// The "Before" label (\ or the Before view) at 100 %: the photo's corner is off screen, and the
/// label used to leave with it (issue #2267). It now sits at the canvas' corner, in view.
#[test]
fn zoomed_in_the_before_label_stays_in_view() {
    use crate::state::{BeforeAfter, Zoom};
    let mut h = detail();
    h.app.ui.before_after = BeforeAfter::Original;
    let label = |h: &mut Headless| -> egui::Rect {
        let w = h.request("ui.widgets", json!({"filter": "canvas:beforeLabel"}), T);
        let r = w["result"][0]["rect"].as_array().unwrap_or_else(|| panic!("the Before label is registered: {w}"));
        let v: Vec<f32> = r.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect();
        egui::Rect::from_min_size(egui::pos2(v[0], v[1]), egui::vec2(v[2], v[3]))
    };
    h.app.ui.zoom = Zoom::Fit;
    h.settle(SETTLE);
    let (img, canvas) = (h.app.image_rect.expect("photo on screen"), h.app.canvas_rect.expect("canvas"));
    let fitted = label(&mut h);
    assert_eq!(fitted.min, img.min + egui::vec2(8.0, 8.0), "at Fit the label sits on the photo's corner");
    h.app.ui.zoom = Zoom::Percent(100.0);
    h.app.ui.pan = (0.3, 0.4);
    h.settle(SETTLE);
    let img = h.app.image_rect.expect("photo on screen");
    assert!(img.min.x < canvas.min.x && img.min.y < canvas.min.y, "100 % reaches past the canvas ({img:?} in {canvas:?})");
    let zoomed = label(&mut h);
    assert!(canvas.contains_rect(zoomed), "the label stays in view: {zoomed:?} in {canvas:?}");
    assert_eq!(zoomed.min, canvas.min + egui::vec2(8.0, 8.0));
}

/// With the info overlay on, zoomed in, the Before label sits below the overlay's lines instead
/// of under them (both live in the canvas' top-left corner).
#[test]
fn zoomed_in_the_before_label_keeps_clear_of_the_info_overlay() {
    use crate::state::{BeforeAfter, InfoOverlay, Zoom};
    let mut h = detail();
    h.app.ui.before_after = BeforeAfter::Original;
    h.app.ui.info_overlay = InfoOverlay::Exposure;
    h.app.ui.zoom = Zoom::Percent(100.0);
    h.app.ui.pan = (0.3, 0.4);
    h.settle(SETTLE);
    let rect = |h: &mut Headless, id: &str| -> egui::Rect {
        let w = h.request("ui.widgets", json!({"filter": id}), T);
        let r = w["result"][0]["rect"].as_array().unwrap_or_else(|| panic!("{id} is registered: {w}"));
        let v: Vec<f32> = r.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect();
        egui::Rect::from_min_size(egui::pos2(v[0], v[1]), egui::vec2(v[2], v[3]))
    };
    let (badge, overlay) = (rect(&mut h, "canvas:beforeLabel"), rect(&mut h, "canvas:infoOverlay"));
    let canvas = h.app.canvas_rect.expect("canvas");
    assert!(badge.top() >= overlay.bottom(), "the label {badge:?} is below the overlay {overlay:?}");
    assert!(canvas.contains_rect(badge), "and still in view: {badge:?} in {canvas:?}");
}

/// A crop of no size (or a hair wide), from a sidecar, gave the loupe a NaN aspect:
/// the photo was laid out at a NaN size, which egui's debug checks stop on (a panic in a debug
/// build) and nothing could be drawn or clicked. It now lays out at a finite size, fitted and at
/// 100 %.
#[test]
fn a_crop_of_no_size_lays_out_at_a_finite_size() {
    let mut h = detail();
    for rect in [[0.5, 0.5, 0.5, 0.5], [0.0, 0.0, 1e-9, 1.0], [0.0, 0.3, 1.0, 0.3]] {
        // as a sidecar can set it: `crop.set` refuses a crop this small
        edit(&mut h, |d| d.crop.geometry.rect = lightcraft_geom::Rect::new(rect[0], rect[1], rect[2], rect[3]));
        for zoom in ["view.zoomFit", "view.zoom100"] {
            let r = h.request("engine.execute", json!({"command": zoom}), T);
            assert_eq!(r["ok"], true, "{zoom}: {r}");
            h.settle(SETTLE);
            let img = h.app.image_rect.expect("the loupe laid the photo out");
            assert!(img.min.is_finite() && img.max.is_finite(), "{rect:?} at {zoom}: {img:?}");
            let r = h.request(
                "ui.pointer",
                json!({"events": [{"kind": "down", "x": 0.5, "y": 0.5}, {"kind": "drag", "x": 0.6, "y": 0.4}, {"kind": "up", "x": 0.6, "y": 0.4}]}),
                T,
            );
            assert_eq!(r["ok"], true, "{r}");
            h.step();
        }
    }
}

/// A scrub keeps the pressed image point fixed, reverses without drift and doesn't click on release.
#[test]
fn scrub_zoom_tracks_horizontal_travel_and_preserves_the_anchor() {
    use crate::panels::detail::Gesture;
    use crate::state::Zoom;
    let mut h = detail();
    assert_eq!(h.request("engine.execute", json!({"command": "tool.zoom"}), T)["ok"], true);
    h.request("engine.execute", json!({"command": "view.zoom100"}), T);
    h.step();
    let image = h.app.image_rect.unwrap();
    let at = image.center() + egui::vec2(35.0, 20.0);
    let point = (at - image.min) / image.size();
    let modifiers = egui::Modifiers::default();
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.step();
    h.app.synthetic.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers });
    h.step();
    let to = at + egui::vec2(200.0, 40.0);
    h.app.synthetic.push(egui::Event::PointerMoved(to));
    h.step();
    assert!(matches!(h.app.gesture, Some(Gesture::ScrubZoom(_))));
    let Zoom::Percent(percent) = h.app.ui.zoom else { panic!("scrub must zoom in") };
    assert!((percent - 200.0).abs() < 0.01, "{percent}");
    let image = h.app.image_rect.unwrap();
    assert!((image.min + image.size() * point).distance(at) < 0.05, "the press position is the zoom anchor");
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.step();
    assert!(matches!(h.app.ui.zoom, Zoom::Percent(p) if (p - 100.0).abs() < 0.01));
    let left = at - egui::vec2(200.0, 0.0);
    h.app.synthetic.push(egui::Event::PointerMoved(left));
    h.step();
    assert!(matches!(h.app.ui.zoom, Zoom::Percent(p) if (p - 50.0).abs() < 0.01));
    h.app.synthetic.push(egui::Event::PointerButton { pos: left, button: egui::PointerButton::Primary, pressed: false, modifiers });
    h.step();
    assert!(h.app.gesture.is_none());
    assert!(matches!(h.app.ui.zoom, Zoom::Percent(p) if (p - 50.0).abs() < 0.01), "release must not toggle Fit");
}

/// Shift temporarily scrubs, ordinary dragging still pans, and editing tools retain their input.
#[test]
fn shift_scrub_does_not_replace_pan_or_editing_tools() {
    use crate::state::Zoom;
    let mut h = detail();
    h.request("engine.execute", json!({"command": "view.zoom100"}), T);
    h.step();
    let at = h.app.image_rect.unwrap().center();
    let drag = |h: &mut Headless, shift| {
        let r = h.request("ui.drag", json!({"x": at.x, "y": at.y, "toX": at.x + 80.0, "toY": at.y, "shift": shift, "steps": 8}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
    };
    let pan = h.app.ui.pan;
    drag(&mut h, false);
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0));
    assert!(h.app.ui.pan.0 < pan.0, "an ordinary drag pans the zoomed photo");
    drag(&mut h, true);
    assert!(matches!(h.app.ui.zoom, Zoom::Percent(p) if p > 100.0));
    assert!(h.app.gesture.is_none());
    h.request("engine.execute", json!({"command": "tool.wbPicker"}), T);
    let zoom = h.app.ui.zoom;
    drag(&mut h, true);
    assert_eq!(h.app.ui.zoom, zoom, "a colour picker must not become scrub zoom");
}

#[test]
fn middle_button_pans_detail_without_scrubbing_or_toggling_zoom() {
    use crate::state::Zoom;
    let mut h = detail();
    for tool in ["", "zoom"] {
        for shift in [false, true] {
            for zoom in [Zoom::Percent(100.0), Zoom::Fit] {
                h.app.ui.tool = tool.into();
                h.app.ui.zoom = zoom;
                h.app.ui.pan = (0.5, 0.5);
                h.settle(SETTLE);
                let image = h.app.image_rect.unwrap();
                let at = h.app.canvas_rect.unwrap().center();
                let delta = egui::vec2(80.0, 40.0);
                let to = at + delta;
                let modifiers = egui::Modifiers { shift, ..Default::default() };
                h.app.synthetic.push(egui::Event::PointerMoved(at));
                h.step();
                h.app.synthetic.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Middle, pressed: true, modifiers });
                h.step();
                h.app.synthetic.push(egui::Event::PointerMoved(to));
                h.step();
                h.step();
                assert_eq!(h.app.ui.zoom, zoom, "middle drag must not scrub: tool={tool}, shift={shift}");
                assert!(h.app.gesture.is_none(), "middle drag must not own a scrub gesture");
                if matches!(zoom, Zoom::Percent(_)) {
                    let moved = h.app.image_rect.unwrap().min - image.min;
                    assert!(
                        (moved - delta).length() < 0.1,
                        "middle drag must move the image by {delta:?}, got {moved:?}: tool={tool}, shift={shift}"
                    );
                    assert!((h.app.ui.pan.0 - (0.5 - delta.x / image.width())).abs() < 0.0001);
                    assert!((h.app.ui.pan.1 - (0.5 - delta.y / image.height())).abs() < 0.0001);
                } else {
                    assert_eq!(h.app.ui.pan, (0.5, 0.5), "a fitted photo must not pan");
                }
                let pan = h.app.ui.pan;
                h.app.synthetic.push(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Middle, pressed: false, modifiers });
                h.step();
                assert_eq!(h.app.ui.zoom, zoom, "middle release must not click-toggle");
                assert_eq!(h.app.ui.pan, pan, "middle release must not pan again");
            }
        }
    }
}

#[test]
fn selected_zoom_space_taps_toggle_once_on_release_to_the_chosen_ratio() {
    use crate::state::{ViewMode, Zoom};
    let mut h = detail();
    let ids: Vec<_> = h.app.session.visible_cloned().into_iter().take(2).map(|id| id.0).collect();
    h.app.run("library.select", json!({"ids": ids, "active": ids[1]})).unwrap();
    h.app.run("view.clickZoom", json!({"ratio": 2})).unwrap();
    for view in [ViewMode::Detail, ViewMode::Compare, ViewMode::Reference] {
        h.app.ui.view = view;
        h.app.ui.compare = Some((ids[0], ids[1]));
        h.app.ui.reference = Some(ids[0]);
        h.app.run("tool.zoom", json!({})).unwrap();
        h.app.run("view.zoomFit", json!({})).unwrap();
        h.step();
        h.app.synthetic.push(space(true));
        h.step();
        assert_eq!(h.app.ui.zoom, Zoom::Fit, "{view:?}: press waits for tap versus pan");
        let mut repeat = space(true);
        if let egui::Event::Key { repeat, .. } = &mut repeat {
            *repeat = true;
        }
        h.app.synthetic.push(repeat);
        h.step();
        assert_eq!(h.app.ui.zoom, Zoom::Fit, "holding Space must not toggle");
        h.app.synthetic.push(space(false));
        h.step();
        assert_eq!(h.app.ui.zoom, Zoom::Percent(200.0), "{view:?}: release uses the chosen ratio");
        h.app.synthetic.push(space(false));
        h.step();
        assert_eq!(h.app.ui.zoom, Zoom::Percent(200.0), "release without a new press must not toggle again");
        assert_eq!(h.request("ui.key", json!({"key": "Space"}), T)["ok"], true);
        h.step();
        assert_eq!(h.app.ui.zoom, Zoom::Fit, "a press and release in one frame is still one tap");
    }
}

#[test]
fn batched_space_releases_do_not_consume_a_later_press() {
    use crate::state::Zoom;
    let mut h = detail();
    h.app.run("tool.zoom", json!({})).unwrap();
    for cancelled in [false, true] {
        h.app.run("view.zoom100", json!({})).unwrap();
        h.step();
        h.events.push(space(true));
        h.step();
        if cancelled {
            let at = h.app.image_rect.unwrap().center();
            let r = h.request("ui.drag", json!({"x": at.x, "y": at.y, "toX": at.x + 80.0, "toY": at.y, "steps": 8}), T);
            assert_eq!(r["ok"], true, "{r}");
            assert!(h.app.ui.space_zoom_tap.is_none(), "the first Space hold was used for pan");
        }
        // Raw host events must stay in one frame; app.synthetic splits at the first release.
        h.events.extend([space(false), space(true)]);
        h.step();
        let after_first_release = if cancelled { Zoom::Percent(100.0) } else { Zoom::Fit };
        assert_eq!(h.app.ui.zoom, after_first_release, "only the earlier eligible tap may toggle");
        assert!(h.app.ui.space_zoom_tap.is_some(), "the later press stays armed for its own release");
        h.events.push(space(false));
        h.step();
        let after_second_release = if cancelled { Zoom::Fit } else { Zoom::Percent(100.0) };
        assert_eq!(h.app.ui.zoom, after_second_release, "the later tap toggles exactly on its own release");
    }
}

#[test]
fn a_complete_pointer_drag_in_one_frame_cancels_space_without_swallowing_the_next_tap() {
    use crate::state::Zoom;
    let mut h = detail();
    h.app.run("tool.zoom", json!({})).unwrap();
    h.app.run("view.zoom100", json!({})).unwrap();
    h.step();
    let at = h.app.image_rect.unwrap().center();
    let to = at + egui::vec2(80.0, 0.0);
    let pointer = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    h.events.push(space(true));
    h.step();
    h.events.extend([egui::Event::PointerMoved(at), pointer(at, true), egui::Event::PointerMoved(to), pointer(to, false)]);
    h.step();
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0));
    assert!(h.app.ui.space_zoom_tap.is_none(), "a batched drag owns the Space hold even without an egui drag response");
    h.events.push(space(false));
    h.step();
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0), "releasing Space after the fast drag must not toggle");

    // A later keyboard-only tap in the same host batch remains eligible after the pointer is up.
    h.events.extend([space(true), pointer(at, true), egui::Event::PointerMoved(to), pointer(to, false), space(false), space(true)]);
    h.step();
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0), "the cancelled hold's release must not toggle");
    assert!(h.app.ui.space_zoom_tap.is_some(), "a new press after the pointer release is independent");
    h.events.push(space(false));
    h.step();
    assert_eq!(h.app.ui.zoom, Zoom::Fit, "the new tap toggles on its release");

    // Likewise a drag begun in an earlier frame must not reject a new tap after its release.
    h.app.run("view.zoom100", json!({})).unwrap();
    h.step();
    h.events.push(space(true));
    h.step();
    h.events.extend([egui::Event::PointerMoved(at), pointer(at, true)]);
    h.step();
    h.events.push(egui::Event::PointerMoved(to));
    h.step();
    h.events.extend([pointer(to, false), space(false), space(true)]);
    h.step();
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0));
    assert!(h.app.ui.space_zoom_tap.is_some(), "old drag-release state must not suppress a later fresh press");
    h.events.push(space(false));
    h.step();
    assert_eq!(h.app.ui.zoom, Zoom::Fit);
}

#[test]
fn deferred_space_taps_are_discarded_on_focus_tool_or_view_changes() {
    use crate::state::Zoom;
    let mut h = detail();
    for change in ["focus", "tool", "view", "dialog", "panel", "fullscreen"] {
        h.app.run("tool.zoom", json!({})).unwrap();
        h.app.run("view.zoomFit", json!({})).unwrap();
        h.step();
        h.app.synthetic.push(space(true));
        h.step();
        assert!(h.app.ui.space_zoom_tap.is_some(), "{change}: begin with an eligible tap");
        match change {
            "focus" => {
                h.app.synthetic.push(egui::Event::WindowFocused(false));
                h.step();
                h.app.synthetic.push(egui::Event::WindowFocused(true));
                h.step();
            }
            "tool" => {
                h.app.run("tool.none", json!({})).unwrap();
                h.app.run("tool.zoom", json!({})).unwrap();
            }
            "view" => {
                h.app.run("view.photoGrid", json!({})).unwrap();
                h.app.run("view.detail", json!({})).unwrap();
            }
            "dialog" => {
                h.app.run("app.shortcuts", json!({})).unwrap();
                h.app.ui.dialog = None;
            }
            "panel" => {
                h.app.run("panel.crop", json!({})).unwrap();
                h.app.run("tool.zoom", json!({})).unwrap();
            }
            other => {
                assert_eq!(other, "fullscreen", "known input-ownership transition");
                h.app.run("view.fullScreenPreview", json!({})).unwrap();
                h.app.run("view.fullScreenPreview", json!({})).unwrap();
                h.app.run("tool.zoom", json!({})).unwrap();
            }
        }
        h.app.synthetic.push(space(false));
        h.step();
        assert_eq!(h.app.ui.zoom, Zoom::Fit, "{change}: returning to the old context cannot replay the tap");
        h.app.synthetic.extend([space(true), space(false)]);
        h.step();
        assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0), "{change}: the next fresh tap still works");
    }
}

#[test]
fn selected_zoom_space_yields_to_typing_and_custom_or_native_bindings() {
    use crate::state::Zoom;
    let mut h = detail();
    h.app.run("tool.zoom", json!({})).unwrap();
    h.app.run("view.zoomFit", json!({})).unwrap();
    assert_eq!(h.request("ui.clickWidget", json!({"id": "field:search"}), T)["ok"], true);
    h.app.synthetic.extend([space(true), egui::Event::Text(" ".into()), space(false)]);
    h.step();
    assert_eq!(h.app.ui.zoom, Zoom::Fit, "typing a space does not zoom");
    assert_eq!(h.app.ui.search, " ");
    h.view.ctx.memory_mut(|m| m.surrender_focus(crate::text_field::id("field:search")));
    h.step();
    h.app.synthetic.push(space(true));
    h.step();
    assert!(h.app.ui.space_zoom_tap.is_some(), "start with an eligible tap before typing takes focus");
    h.app.run("view.focusSearch", json!({})).unwrap();
    h.step();
    h.step();
    h.view.ctx.memory_mut(|m| m.surrender_focus(crate::text_field::id("field:search")));
    h.step();
    h.app.synthetic.push(space(false));
    h.step();
    assert_eq!(h.app.ui.zoom, Zoom::Fit, "typing taking ownership cancels an already-pending tap");
    h.app.ui.search.clear();
    h.app.run("app.setShortcut", json!({"id": "view.filmstrip", "shortcut": "Space"})).unwrap();
    let filmstrip = h.app.ui.filmstrip;
    h.app.synthetic.push(space(true));
    h.step();
    assert_eq!(h.app.ui.filmstrip, !filmstrip, "custom Space still runs on press");
    h.app.synthetic.push(space(false));
    h.step();
    assert_eq!(h.app.ui.zoom, Zoom::Fit);
    h.app.run("app.resetShortcuts", json!({})).unwrap();
    h.app.native_shortcuts.insert("Space".into());
    h.app.synthetic.extend([space(true), space(false)]);
    h.step();
    assert_eq!(h.app.ui.zoom, Zoom::Fit, "the native menu owns its binding");
    h.app.native_shortcuts.remove("Space");
    h.app.run("app.setShortcut", json!({"id": "view.zoomToggle", "shortcut": "Space"})).unwrap();
    h.app.synthetic.push(space(true));
    h.step();
    assert_eq!(h.app.ui.zoom, Zoom::Fit, "an explicitly assigned zoom binding also waits for release");
    h.app.synthetic.push(space(false));
    h.step();
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0), "the primary Space binding must not double-toggle with the alias");
}

#[test]
fn selected_zoom_space_taps_do_not_escape_popups_or_owned_widgets() {
    use crate::state::Zoom;
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.run("tool.zoom", json!({})).unwrap();
    let ctx = egui::Context::default();
    let popup = egui::Id::new("zoom-space-menu");
    for owner in ["popup", "button"] {
        for (frame, events) in [vec![space(true)], vec![space(false)]].into_iter().enumerate() {
            let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
                if owner == "popup" {
                    egui::Popup::open_id(&ctx, popup);
                } else {
                    ui.button("Own Space").request_focus();
                }
                crate::shortcuts::handle(&mut app, &ctx);
            });
            output.textures_delta.clear();
            assert_eq!(app.ui.zoom, Zoom::Fit, "{owner}, frame {frame}");
        }
        egui::Popup::close_all(&ctx);
    }
    // An owner appearing during an already-pending tap discards it, even if it leaves before release.
    for owner in ["popup", "button"] {
        let ctx = egui::Context::default();
        let mut button = None;
        for (frame, events) in [vec![space(true)], vec![], vec![space(false)]].into_iter().enumerate() {
            let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
                if frame == 1 {
                    if owner == "popup" {
                        egui::Popup::open_id(&ctx, popup);
                    } else {
                        let response = ui.button("Own Space");
                        button = Some(response.id);
                        response.request_focus();
                    }
                } else if frame == 2 {
                    egui::Popup::close_all(&ctx);
                    if let Some(id) = button {
                        ui.memory_mut(|m| m.surrender_focus(id));
                    }
                }
                crate::shortcuts::handle(&mut app, &ctx);
            });
            output.textures_delta.clear();
            if frame == 0 {
                assert!(app.ui.space_zoom_tap.is_some(), "{owner}: begin with an eligible tap");
            }
        }
        assert_eq!(app.ui.zoom, Zoom::Fit, "{owner}: giving up ownership cannot replay a previous tap");
    }
}

#[test]
fn holding_space_pans_with_the_zoom_tool_selected() {
    use crate::state::{ViewMode, Zoom};
    let mut h = detail();
    let ids: Vec<_> = h.app.session.visible_cloned().into_iter().take(2).map(|id| id.0).collect();
    h.app.run("library.select", json!({"ids": ids, "active": ids[1]})).unwrap();
    for view in [ViewMode::Detail, ViewMode::Compare, ViewMode::Reference] {
        h.app.ui.view = view;
        h.app.ui.compare = Some((ids[0], ids[1]));
        h.app.ui.reference = Some(ids[0]);
        h.app.run("tool.zoom", json!({})).unwrap();
        for zoom in [Zoom::Fit, Zoom::Percent(100.0)] {
            h.app.ui.zoom = zoom;
            h.app.ui.pan = (0.5, 0.5);
            h.step();
            let at = if view == ViewMode::Detail {
                h.app.image_rect.unwrap().center()
            } else {
                h.app.widgets.iter().find(|(id, _)| id == &format!("cull:{}", ids[1])).unwrap().1.center()
            };
            let pan = h.app.ui.pan;
            h.app.synthetic.push(space(true));
            h.step();
            let r = h.request("ui.drag", json!({"x": at.x, "y": at.y, "toX": at.x + 80.0, "toY": at.y, "steps": 8}), T);
            assert_eq!(r["ok"], true, "{r}");
            // Key repeat after the pointer is up must not re-arm the cancelled tap.
            let mut repeat = space(true);
            if let egui::Event::Key { repeat, .. } = &mut repeat {
                *repeat = true;
            }
            h.app.synthetic.push(repeat);
            h.step();
            h.app.synthetic.push(space(false));
            h.step();
            assert_eq!(h.app.ui.zoom, zoom, "{view:?}: a Space drag must not scrub or toggle on release");
            if zoom != Zoom::Fit {
                assert!(h.app.ui.pan.0 < pan.0, "{view:?}: the held drag pans");
            }
            assert_eq!(h.app.ui.tool, "zoom", "Space is a temporary override");
            assert!(h.app.gesture.is_none());
        }
    }
}

#[test]
fn space_keeps_zoom_toggle_and_slideshow_pause_outside_pan_override() {
    use crate::state::Zoom;
    let mut h = detail();
    h.app.run("view.zoomFit", json!({})).expect("Fit");
    h.request("ui.key", json!({"key": "Space"}), T);
    h.step();
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0), "Space retains the ordinary zoom toggle");
    h.app.run("view.slideshow", json!({"interval": 120.0})).expect("slideshow");
    h.app.run("tool.zoom", json!({})).expect("magnifier during slideshow");
    assert_eq!(h.app.ui.tool, "zoom");
    let zoom = h.app.ui.zoom;
    assert!(h.app.ui.slideshow.as_ref().is_some_and(|run| !run.play.is_paused()));
    h.app.synthetic.push(space(true));
    h.step();
    assert!(h.app.ui.slideshow.as_ref().is_some_and(|run| run.play.is_paused()), "Space pauses on press even with the magnifier selected");
    h.app.synthetic.push(space(false));
    h.step();
    assert!(h.app.ui.slideshow.as_ref().is_some_and(|run| run.play.is_paused()), "release must not resume or toggle zoom");
    assert_eq!(h.app.ui.zoom, zoom);
    h.request("ui.key", json!({"key": "Space"}), T);
    h.step();
    assert!(h.app.ui.slideshow.as_ref().is_some_and(|run| !run.play.is_paused()));
    assert_eq!(h.app.ui.zoom, zoom);
}

#[test]
fn zoom_button_toggles_the_tool_and_command_preserves_comparison_views() {
    use crate::state::{RightPanel, ViewMode};
    let mut h = detail();
    for tool in ["zoom", ""] {
        let r = h.request("ui.clickWidget", json!({"id": "icon:zoomTool"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert_eq!(h.app.ui.tool, tool);
    }
    for right in [RightPanel::Crop, RightPanel::Masking, RightPanel::Remove, RightPanel::RedEye] {
        h.app.ui.right = right;
        h.app.run("tool.zoom", json!({})).expect("select zoom");
        assert_eq!(h.app.ui.right, RightPanel::Edit);
        assert_eq!(h.app.ui.tool, "zoom");
    }
    for view in [ViewMode::Compare, ViewMode::Reference] {
        h.app.ui.view = view;
        h.app.run("tool.zoom", json!({})).expect("select zoom");
        assert_eq!(h.app.ui.view, view);
    }
}

// Given the magnifier selected in Detail, a click zooms about the clicked point, as in Compare
#[test]
fn detail_magnifier_click_keeps_the_point_under_the_pointer() {
    use crate::state::Zoom;
    let mut h = detail();
    h.app.run("tool.zoom", json!({})).unwrap();
    h.app.run("view.clickZoom", json!({"ratio": 1})).unwrap();
    h.app.run("view.zoomFit", json!({})).unwrap();
    h.settle(SETTLE);
    let fitted = h.app.image_rect.unwrap();
    let point = egui::vec2(0.3, 0.4);
    let at = fitted.min + fitted.size() * point;
    assert_eq!(h.request("ui.click", json!({"x": at.x, "y": at.y}), T)["ok"], true);
    h.settle(SETTLE);
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0));
    let zoomed = h.app.image_rect.unwrap();
    let drift = (zoomed.min + zoomed.size() * point).distance(at);
    assert!(drift < 0.75, "the clicked point drifted {drift} points");
}

// Given the magnifier selected and Masking open (which keeps the tool), Space still toggles zoom
#[test]
fn space_toggles_zoom_under_an_editing_panel_that_keeps_the_magnifier() {
    use crate::state::{RightPanel, Zoom};
    let mut h = detail();
    h.app.run("tool.zoom", json!({})).unwrap();
    h.app.run("view.zoomFit", json!({})).unwrap();
    h.app.run("panel.masking", json!({})).unwrap();
    assert_eq!((h.app.ui.right, h.app.ui.tool.as_str()), (RightPanel::Masking, "zoom"));
    h.step();
    h.request("ui.key", json!({"key": "Space"}), T);
    h.step();
    assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0), "Space is not swallowed where the magnifier can't scrub");
}

#[test]
fn space_pressed_during_a_scrub_cancels_zoom_and_pans_without_restarting() {
    use crate::panels::detail::Gesture;
    use crate::state::Zoom;
    let mut h = detail();
    h.app.run("tool.zoom", json!({})).unwrap();
    h.app.run("view.zoom100", json!({})).unwrap();
    h.step();
    let at = h.app.image_rect.unwrap().center();
    let modifiers = egui::Modifiers::NONE;
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.step();
    h.app.synthetic.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers });
    h.step();
    let to = at + egui::vec2(100.0, 0.0);
    h.app.synthetic.push(egui::Event::PointerMoved(to));
    h.step();
    assert!(matches!(h.app.gesture, Some(Gesture::ScrubZoom(_))));
    let zoom = h.app.ui.zoom;
    assert!(matches!(zoom, Zoom::Percent(p) if p > 100.0));
    let pan = h.app.ui.pan;
    let key = space;
    h.app.synthetic.push(key(true));
    h.step();
    assert_eq!(h.app.ui.zoom, zoom, "Space press must not toggle the active scrub");
    h.app.synthetic.push(egui::Event::PointerMoved(to + egui::vec2(60.0, 20.0)));
    h.step();
    assert_eq!(h.app.ui.zoom, zoom, "Space stops the already-active scrub");
    assert!(h.app.gesture.is_none());
    assert!(h.app.ui.pan.0 < pan.0, "the held drag now pans");
    h.app.synthetic.push(key(false));
    h.step();
    assert_eq!(h.app.ui.zoom, zoom, "Space release during the held pointer must not toggle");
    h.app.synthetic.push(egui::Event::PointerMoved(to + egui::vec2(80.0, 20.0)));
    h.step();
    assert_eq!(h.app.ui.zoom, zoom, "releasing Space must not replay the original scrub anchor");
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: to + egui::vec2(80.0, 20.0),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers,
    });
    h.step();
    assert_eq!(h.app.ui.zoom, zoom, "release must not become a zoom click");
    assert_eq!(h.app.ui.tool, "zoom");
}

#[test]
fn magnifier_clicks_preserve_off_center_anchors_and_toggle_the_chosen_ratio_in_both_panes() {
    use crate::state::{ViewMode, Zoom};
    let mut h = detail();
    let ids: Vec<_> = h.app.session.visible_cloned().into_iter().take(2).map(|id| id.0).collect();
    h.app.run("library.select", json!({"ids": ids, "active": ids[1]})).unwrap();
    h.app.ui.settings.zoom_to_focus = true;
    for id in &ids {
        let id = lightcraft_catalog::PhotoId(*id);
        let mut meta = h.app.session.catalog.photo(id).unwrap().meta.clone();
        meta.focus = Some(lightcraft_meta::FocusPoint {
            rect: lightcraft_geom::Rect { x0: 0.70, y0: 0.20, x1: 0.74, y1: 0.26 },
            mode: None,
            area: None,
            tracking: None,
        });
        h.app.session.catalog.apply(lightcraft_catalog::Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
    }
    for view in [ViewMode::Compare, ViewMode::Reference] {
        h.app.ui.view = view;
        h.app.ui.compare = Some((ids[0], ids[1]));
        h.app.ui.reference = Some(ids[0]);
        h.app.run("tool.zoom", json!({})).unwrap();
        for (pane, id) in ids.iter().enumerate() {
            h.app.run("view.clickZoom", json!({"ratio": pane + 1})).unwrap();
            h.app.ui.zoom = Zoom::Fit;
            h.app.ui.pan = (0.5, 0.5);
            h.step();
            let before = h.app.session.active();
            let widget = format!("cull:{id}");
            let rect = |h: &Headless| h.app.widgets.iter().find(|(key, _)| key == &widget).unwrap().1;
            h.request("ui.widgets", json!({}), T);
            h.step();
            let fitted = rect(&h);
            let point = egui::vec2(0.4, 0.6);
            let at = fitted.min + fitted.size() * point;
            let r = h.request("ui.click", json!({"x": at.x, "y": at.y}), T);
            assert_eq!(r["ok"], true, "{r}");
            h.settle(SETTLE);
            assert_eq!(h.app.ui.zoom, Zoom::Percent((pane + 1) as f32 * 100.0), "{view:?}: single click uses the chosen ratio");
            let zoomed = rect(&h);
            let drift = (zoomed.min + zoomed.size() * point).distance(at);
            assert!(drift < 0.75, "{view:?} pane {pane}: the clicked point drifted {drift} points instead of staying at {at:?}");
            assert_eq!(h.app.ui.view, view);
            assert_eq!(h.app.ui.reference, Some(ids[0]));
            if view == ViewMode::Reference {
                assert_eq!(h.app.session.active(), before, "reference navigation must not select a different edit target");
            } else {
                assert_eq!(h.app.session.active().map(|p| p.0), Some(*id));
            }
            let r = h.request("ui.click", json!({"x": at.x, "y": at.y}), T);
            assert_eq!(r["ok"], true, "{r}");
            h.step();
            assert_eq!(h.app.ui.zoom, Zoom::Fit, "{view:?}: second click returns to Fit even when egui reports a double click");
        }
    }
}
