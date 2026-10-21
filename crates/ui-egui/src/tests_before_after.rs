//! Headless tests of input and zoom animation in the two-view Before / After layouts.

use std::time::Duration;

use serde_json::json;

use crate::headless::{Headless, RenderGate};
use crate::render::Slot;
use crate::state::{BeforeAfter, ViewMode, Zoom};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// Click at a screen position with real egui pointer events (as `ui.pointer` sends them).
fn click_at(h: &mut Headless, pos: egui::Pos2) {
    h.app.synthetic.push(egui::Event::PointerMoved(pos));
    h.app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    h.settle(SETTLE);
}

/// The white-balance selector picks the same point of the photo whichever view of a side-by-side
/// or top/bottom Before / After layout is clicked, at Fit and zoomed in. Clicks on the Before
/// view used to be mapped through the After view's rectangle, so they sampled the photo's edge.
#[test]
fn wb_selector_picks_the_same_point_in_the_before_view() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let id = h.app.session.visible_cloned()[2];
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0], "active": id.0}}), T);
    h.app.ui.view = ViewMode::Detail;
    let wb = |h: &Headless| h.app.session.catalog.photo(id).map(|p| (p.develop.wb.temp, p.develop.wb.tint));
    // lay the view out and arm the selector; returns the After view's image rect and the canvas
    let arm = |h: &mut Headless, mode: BeforeAfter, zoom: Zoom| {
        h.request("engine.execute", json!({"command": "develop.reset"}), T);
        h.app.ui.before_after = mode;
        h.app.ui.zoom = zoom;
        h.app.ui.pan = Default::default();
        h.settle(SETTLE);
        let r = h.request("engine.execute", json!({"command": "tool.wbPicker"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        (h.app.image_rect.expect("After view on screen"), h.app.canvas_rect.expect("canvas"))
    };
    for mode in [BeforeAfter::SideBySide, BeforeAfter::TopBottom] {
        // the Before view is the After view moved by one pane: measured at Fit, where both are
        // centred in their panes and so mirror each other about the canvas centre
        let (img, canvas) = arm(&mut h, mode, Zoom::Fit);
        let shift = match mode {
            BeforeAfter::SideBySide => egui::vec2(img.left() + img.right() - 2.0 * canvas.center().x, 0.0),
            _ => egui::vec2(0.0, img.top() + img.bottom() - 2.0 * canvas.center().y),
        };
        assert!(shift.length() > 100.0, "{mode:?}: two views, {shift:?} apart");
        h.request("engine.execute", json!({"command": "tool.wbPicker"}), T);
        for zoom in [Zoom::Fit, Zoom::Percent(100.0)] {
            let mut picked = Vec::new();
            for before_view in [false, true] {
                let (img, canvas) = arm(&mut h, mode, zoom);
                // the middle of the After pane (on the photo at Fit and zoomed in), or the same
                // point of the photo in the Before view
                let after = canvas.center() + shift / 2.0;
                assert!(img.contains(after), "{mode:?} {zoom:?}: {after:?} on the photo {img:?}");
                click_at(&mut h, if before_view { after - shift } else { after });
                assert_eq!(h.app.ui.tool, "", "{mode:?} {zoom:?}: the selector is done after one click");
                picked.push(wb(&h));
            }
            assert_ne!(picked[0], Some((6500.0, 0.0)), "{mode:?} {zoom:?}: the click picked a white balance");
            assert_eq!(picked[1], picked[0], "{mode:?} {zoom:?}: the Before view picks the After view's point");
        }
    }
}

/// A click at Fit zooms in on the point clicked whichever view of a side-by-side or top/bottom
/// Before / After layout it lands in: both views then show the same part of the photo. A click
/// on the Before view used to be measured against the After view's rectangle, so it zoomed in on
/// the photo's edge (#1742).
#[test]
fn a_click_in_the_before_view_zooms_in_on_the_point_clicked() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "none", "filmstrip": false}), T);
    assert_eq!(r["ok"], true, "{r}");
    let fit = |h: &mut Headless, mode: BeforeAfter| {
        h.app.ui.before_after = mode;
        h.app.ui.zoom = Zoom::Fit;
        h.app.ui.pan = Default::default();
        h.settle(SETTLE);
        (h.app.image_rect.expect("After view on screen"), h.app.canvas_rect.expect("canvas"))
    };
    for mode in [BeforeAfter::SideBySide, BeforeAfter::TopBottom] {
        let (img, canvas) = fit(&mut h, mode);
        let shift = match mode {
            BeforeAfter::SideBySide => egui::vec2(img.left() + img.right() - 2.0 * canvas.center().x, 0.0),
            _ => egui::vec2(0.0, img.top() + img.bottom() - 2.0 * canvas.center().y),
        };
        assert!(shift.length() > 100.0, "{mode:?}: two views, {shift:?} apart");
        // off the middle of the photo, so that a zoom on another point shows
        let after = img.center() + egui::vec2(img.width() * 0.3, img.height() * -0.25);
        let mut pans = Vec::new();
        for before_view in [false, true] {
            fit(&mut h, mode);
            click_at(&mut h, if before_view { after - shift } else { after });
            assert!(matches!(h.app.ui.zoom, Zoom::Percent(_)), "{mode:?}: the click zoomed in ({:?})", h.app.ui.zoom);
            pans.push(h.app.ui.pan);
        }
        let (a, b) = (pans[0], pans[1]);
        assert!((a.0 - 0.8).abs() < 0.02 && (a.1 - 0.25).abs() < 0.02, "{mode:?}: the After view zooms in on the point clicked ({a:?})");
        assert!((b.0 - a.0).abs() < 1e-3 && (b.1 - a.1).abs() < 1e-3, "{mode:?}: the Before view zooms in on the same point ({b:?}, not {a:?})");
    }
}

/// A texture mesh in its pane, excluding the Navigator's copy of the Main texture.
fn pane_mesh(h: &Headless, slot: Slot, pane: egui::Rect) -> egui::Rect {
    let tex = &h.app.renderer.textures[&slot];
    let meshes: Vec<_> = h
        .view
        .shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::epaint::Shape::Mesh(m) if m.texture_id == tex.tex.id() && c.clip_rect == pane => Some(m.calc_bounds()),
            _ => None,
        })
        .collect();
    assert_eq!(meshes.len(), 1, "{slot:?}: one image clipped to {pane:?}, got {meshes:?}");
    meshes[0]
}

fn assert_rect(actual: egui::Rect, expected: egui::Rect) {
    assert!(
        (actual.min - expected.min).abs().max_elem() < 1.0 && (actual.max - expected.max).abs().max_elem() < 1.0,
        "image bounds {actual:?}, expected {expected:?}"
    );
}

/// Start with both textures ready, then hold replacement renders so worker timing cannot hide
/// a frame of the transition or replace a cached tile under the assertions.
fn animation_view(mode: BeforeAfter, zoom: Zoom) -> (Headless, [egui::Rect; 2], std::rc::Rc<std::cell::Cell<bool>>) {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "none", "filmstrip": false}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.before_after = mode;
    h.app.ui.zoom = zoom;
    h.app.ui.pan = (0.5, 0.5);
    h.app.ui.click_zoom = 300;
    assert!(h.settle(SETTLE), "baseline renders settled");
    let before_id = h.app.renderer.textures[&Slot::Before].tex.id();
    let before = h
        .view
        .shapes
        .iter()
        .find_map(|c| match &c.shape {
            egui::epaint::Shape::Mesh(m) if m.texture_id == before_id => Some(c.clip_rect),
            _ => None,
        })
        .expect("Before image on screen");
    let canvas = h.app.canvas_rect.expect("canvas");
    let shift = 2.0 * (canvas.center() - before.center());
    let after = before.translate(shift);
    assert!(canvas.contains_rect(before) && canvas.contains_rect(after), "both clips stay in the canvas");
    assert!(!before.intersects(after), "pane clips do not overlap");
    let open = std::rc::Rc::new(std::cell::Cell::new(false));
    h.app.renderer.set_offload(Box::new(RenderGate::new(open.clone())));
    (h, [before, after], open)
}

fn queue_click(h: &mut Headless, pos: egui::Pos2) {
    h.app.synthetic.extend([
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() },
    ]);
    while !h.app.synthetic.is_empty() {
        h.step();
    }
    assert!(h.app.ui.zoom_anim, "the click started an animation");
}

fn check_click_animation(mode: BeforeAfter) {
    let (mut h, [before, after], open) = animation_view(mode, Zoom::Fit);
    let shift = before.center() - after.center();
    let fit = h.app.image_rect.expect("Fit image");
    let id = h.app.session.active().expect("photo");
    let photo = h.app.session.catalog.photo(id).expect("photo");
    let native = [photo.width as usize, photo.height as usize];
    let aspect = native[0] as f32 / native[1] as f32;
    // Exercise clicks in both panes, including their return to Fit, without settling the motion.
    for before_click in [false, true] {
        for zoom_in in [true, false] {
            let pos = if zoom_in { fit.center() + egui::vec2(fit.width() * 0.3, fit.height() * -0.25) } else { after.center() };
            queue_click(&mut h, if before_click { pos + shift } else { pos });
            assert_eq!(h.app.ui.zoom, if zoom_in { Zoom::Percent(300.0) } else { Zoom::Fit });
            if zoom_in {
                let pan = h.app.ui.pan;
                assert!((pan.0 - 0.8).abs() < 0.02 && (pan.1 - 0.25).abs() < 0.02, "{mode:?}: clicked point {pan:?}");
            }
            let target = crate::panels::detail::fit_rect(after, aspect, h.app.ui.zoom, native, 1.0, h.app.ui.pan);
            let mut intermediate = 0;
            let mut keys = std::collections::HashSet::new();
            for _ in 0..40 {
                h.step();
                keys.insert((h.app.renderer.wanted(Slot::Before), h.app.renderer.wanted(Slot::Main)));
                let img = h.app.image_rect.expect("animated image");
                let between = img.width() > fit.width() + 1.0 && img.width() < native[0] as f32 * 3.0 - 1.0;
                if between {
                    intermediate += 1;
                }
                if between || !h.app.ui.zoom_anim {
                    let a = pane_mesh(&h, Slot::Main, after);
                    let b = pane_mesh(&h, Slot::Before, before);
                    assert_rect(b, a.translate(shift));
                }
            }
            assert!(intermediate > 0, "{mode:?}: inspected intermediate animation frames");
            assert!(!h.app.ui.zoom_anim, "animation finished");
            assert_rect(h.app.image_rect.expect("settled image"), target);
            assert_rect(pane_mesh(&h, Slot::Main, after), crate::panels::detail::fit_texture_rect(target, h.app.renderer.textures[&Slot::Main].size));
            assert_eq!(keys.len(), 1, "both panes request stable final-size renders throughout the animation: {keys:?}");
        }
    }
    open.set(true);
    assert!(h.settle(SETTLE));
}

#[test]
fn both_before_after_panes_use_the_animated_rect_side_by_side() {
    check_click_animation(BeforeAfter::SideBySide);
}

#[test]
fn both_before_after_panes_use_the_animated_rect_top_bottom() {
    check_click_animation(BeforeAfter::TopBottom);
}

fn check_region_animation(mode: BeforeAfter) {
    let (mut h, [before, after], open) = animation_view(mode, Zoom::Percent(200.0));
    let shift = before.center() - after.center();
    let start = h.app.image_rect.expect("200 percent image");
    let tiles = [(Slot::RegionBefore, before, true), (Slot::Region, after, false)].map(|(slot, pane, is_before)| {
        let tex = &h.app.renderer.textures[&slot];
        let tile = h.app.region_tiles[&(is_before, tex.key)];
        pane_mesh(&h, slot, pane);
        (slot, pane, tile, tex.key)
    });
    h.app.ui.zoom = Zoom::Percent(400.0);
    h.app.ui.zoom_anim = true;
    let mut intermediate = 0;
    let mut keys = std::collections::HashSet::new();
    for _ in 0..40 {
        h.step();
        keys.insert((h.app.renderer.wanted(Slot::RegionBefore), h.app.renderer.wanted(Slot::Region)));
        let img = h.app.image_rect.expect("animated image");
        if img.width() > start.width() + 1.0 && img.width() < start.width() * 2.0 - 1.0 {
            intermediate += 1;
        }
        if img.width() <= start.width() + 1.0 {
            continue;
        }
        for (slot, pane, tile, key) in tiles {
            assert_eq!(h.app.renderer.textures[&slot].key, key, "the cached tile stays on screen while renders wait");
            let full = if tile.before { img.translate(shift) } else { img };
            let (fw, fh) = (tile.full.0 as f32, tile.full.1 as f32);
            let w = tile.window;
            let expected = egui::Rect::from_min_size(
                full.min + egui::vec2(w.x as f32 / fw * full.width(), w.y as f32 / fh * full.height()),
                egui::vec2(w.w as f32 / fw * full.width(), w.h as f32 / fh * full.height()),
            );
            assert_rect(pane_mesh(&h, slot, pane), expected);
        }
    }
    assert!(intermediate > 0, "{mode:?}: inspected intermediate region frames");
    assert!(!h.app.ui.zoom_anim);
    assert!((h.app.image_rect.expect("settled image").width() - start.width() * 2.0).abs() < 1.0);
    assert_eq!(keys.len(), 1, "region requests use the final target throughout the animation: {keys:?}");
    open.set(true);
    assert!(h.settle(SETTLE));
}

#[test]
fn before_after_region_tiles_use_the_animated_rect_side_by_side() {
    check_region_animation(BeforeAfter::SideBySide);
}

#[test]
fn before_after_region_tiles_use_the_animated_rect_top_bottom() {
    check_region_animation(BeforeAfter::TopBottom);
}

/// Mean (r, g, b) of the 41 × 41 screen pixels around `at`.
fn mean_around(shot: &egui::ColorImage, at: egui::Pos2) -> [f32; 3] {
    let mut sum = [0.0f32; 3];
    let mut n = 0.0;
    for y in (at.y as usize - 20)..=(at.y as usize + 20) {
        for x in (at.x as usize - 20)..=(at.x as usize + 20) {
            let c = shot[(x, y)];
            sum[0] += f32::from(c.r());
            sum[1] += f32::from(c.g());
            sum[2] += f32::from(c.b());
            n += 1.0;
        }
    }
    sum.map(|s| s / n)
}

/// Zoomed in to compare a detail, every Before / After view still shows the edited photo on its
/// After side. A wipe drew its line through the middle of the photo, which zoomed in is mostly off
/// screen: the whole view was the Before, the original photo (#1351).
#[test]
fn zoomed_in_the_after_side_shows_the_edits() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail", "right": "none", "filmstrip": false}), T);
    assert_eq!(r["ok"], true, "{r}");
    let r = h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 1.5}}), T);
    assert_eq!(r["ok"], true, "{r}");
    let shot = |h: &mut Headless, mode: BeforeAfter, zoom: Zoom| {
        h.app.ui.before_after = mode;
        h.app.ui.zoom = zoom;
        // (far enough from the middle of the photo that, zoomed in, it is off screen)
        h.app.ui.pan = (0.25, 0.75);
        h.settle(SETTLE);
        h.paint()
    };
    for zoom in [Zoom::Fit, Zoom::Percent(100.0), Zoom::Percent(400.0)] {
        let (edited, original) = (shot(&mut h, BeforeAfter::Off, zoom), shot(&mut h, BeforeAfter::Original, zoom));
        let c = h.app.canvas_rect.expect("canvas").center();
        let luma = |m: [f32; 3]| m.iter().sum::<f32>();
        for mode in [BeforeAfter::SideBySide, BeforeAfter::TopBottom, BeforeAfter::Split, BeforeAfter::SplitTopBottom] {
            let s = shot(&mut h, mode, zoom);
            let canvas = h.app.canvas_rect.expect("canvas");
            // the middle of the After pane, or of the After side of the line
            let (after, before) = match mode {
                BeforeAfter::SideBySide | BeforeAfter::Split => {
                    (c + egui::vec2(canvas.width() / 4.0, 0.0), c - egui::vec2(canvas.width() / 4.0, 0.0))
                }
                _ => (c + egui::vec2(0.0, canvas.height() / 4.0), c - egui::vec2(0.0, canvas.height() / 4.0)),
            };
            let (a, b) = (mean_around(&s, after), mean_around(&s, before));
            if matches!(mode, BeforeAfter::Split | BeforeAfter::SplitTopBottom) {
                // a wipe shows the loupe's own pixels on each side of the line
                let (e, o) = (mean_around(&edited, after), mean_around(&original, before));
                assert!((luma(a) - luma(e)).abs() < 6.0, "{mode:?} {zoom:?}: the After side {a:?} is the edited photo {e:?}");
                assert!((luma(b) - luma(o)).abs() < 6.0, "{mode:?} {zoom:?}: the Before side {b:?} is the original {o:?}");
            } else {
                // two views of the same point of the photo: the After one is the brighter
                assert!(luma(a) > luma(b) + 30.0, "{mode:?} {zoom:?}: the After {a:?} is the edited photo, brighter than the Before {b:?}");
            }
        }
    }
}

/// Show Original (`\`) is not one of the Before / After layouts, so the bottom bar's
/// Before/After (Y) button stays dark for it; clicking the button then lays out Before and After
/// side by side, as it does from the plain view. It used to light up for `\` too, so the same
/// lit button stood for two different views (#1497).
#[test]
fn show_original_does_not_light_the_before_after_button() {
    assert!(!BeforeAfter::Off.is_layout());
    assert!(!BeforeAfter::Original.is_layout());
    for mode in [BeforeAfter::SideBySide, BeforeAfter::Split, BeforeAfter::TopBottom, BeforeAfter::SplitTopBottom] {
        assert!(mode.is_layout(), "{mode:?}");
    }
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let id = h.app.session.visible_cloned()[2];
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0], "active": id.0}}), T);
    h.app.ui.view = ViewMode::Detail;
    h.settle(SETTLE);
    assert_eq!(h.request("engine.execute", json!({"command": "view.showOriginal"}), T)["ok"], true);
    assert_eq!(h.app.ui.before_after, BeforeAfter::Original);
    assert!(!h.app.ui.before_after.is_layout(), "the button is dark while the original is shown");
    h.settle(SETTLE);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "icon:beforeAfter"}), T)["ok"], true);
    assert_eq!(h.app.ui.before_after, BeforeAfter::SideBySide, "a click on the dark button opens the layout");
    assert_eq!(h.request("ui.clickWidget", json!({"id": "icon:beforeAfter"}), T)["ok"], true);
    assert_eq!(h.app.ui.before_after, BeforeAfter::Off, "a second click closes it");
}
