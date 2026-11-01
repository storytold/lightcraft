//! Issue #323: the Detail view must not be softened by a hidden render-size cap. By default the
//! loupe renders at the size it is drawn at (up to the photo's own pixels and a safety ceiling);
//! "Preview size" in Settings only lowers that when the user asks for it.

use crate::state::{AppSettings, LOUPE_EDGE_CEILING, PREVIEW_LIMITS, STANDARD_PREVIEW_EDGE};

/// A texture side no GPU we run on refuses (see the tests of small ones below).
const BIG: usize = 16384;

fn with_limit(preview_limit: u32) -> AppSettings {
    AppSettings { preview_limit, ..Default::default() }
}

// Given a fresh install
#[test]
fn default_is_automatic() {
    assert_eq!(AppSettings::default().preview_limit, 0);
    assert_eq!(PREVIEW_LIMITS[0], 0, "Automatic is the first choice");
}

// Given Automatic, when a 24 MP photo is shown at 1:1, then the render is the photo's own width
#[test]
fn automatic_renders_a_one_to_one_view_at_native_size() {
    assert_eq!(with_limit(0).loupe_edge(6000.0, 6000, BIG), 6000);
}

// Given Automatic, a Retina fit view of 3024 px is not held to the old 2560 px cap
#[test]
fn automatic_does_not_cap_a_fit_view_at_the_old_default() {
    assert_eq!(with_limit(0).loupe_edge(3024.0, 6000, BIG), 3072, "rounded up to the next 64 px step");
}

// Dragging a window edge must not re-render (and re-key the caches) on every pixel
#[test]
fn render_sizes_move_in_steps() {
    let s = with_limit(0);
    assert_eq!(s.loupe_edge(2945.0, 6000, BIG), s.loupe_edge(3008.0, 6000, BIG));
    assert_ne!(s.loupe_edge(3008.0, 6000, BIG), s.loupe_edge(3009.0, 6000, BIG));
}

// Neighbour prefetch and hover / before stand-ins stay at the preview level: a full-size decode
// would evict the open photo's single full-resolution source (review of #323)
#[test]
fn prefetch_and_stand_ins_never_ask_for_the_full_source_level() {
    use lightcraft_engine::SourceLevel;
    let s = with_limit(0);
    for wanted in [1000.0, 2560.0, 3024.0, 6000.0, 48000.0] {
        assert!(s.prefetch_edge(wanted, 6000, BIG) <= SourceLevel::Preview.max_edge(), "{wanted}");
        assert!(s.stand_in_edge(s.loupe_edge(wanted, 6000, BIG), BIG) <= SourceLevel::Preview.max_edge());
    }
    assert_eq!(s.prefetch_edge(1000.0, 6000, BIG), 1024, "a small view is not raised");
}

// Given Automatic, a small photo zoomed far in is not rendered above its own pixels
#[test]
fn never_renders_above_the_photos_own_pixels() {
    assert_eq!(with_limit(0).loupe_edge(8000.0, 1000, BIG), 1000);
}

// Given Automatic, a huge zoom on a huge photo stays under the memory/GPU ceiling
#[test]
fn automatic_is_bounded_by_the_ceiling() {
    assert_eq!(with_limit(0).loupe_edge(48000.0, 12000, BIG), LOUPE_EDGE_CEILING as usize);
}

// Given the user chose 1600 px, the render never exceeds it
#[test]
fn an_explicit_limit_caps_the_render() {
    assert_eq!(with_limit(1600).loupe_edge(6000.0, 6000, BIG), 1600);
    assert_eq!(with_limit(1600).loupe_edge(900.0, 6000, BIG), 960, "and does not raise a small view past its step");
}

// Given a browser whose GPU allows 2048 px textures (egui_glow panics on a bigger one), no render
// the loupe asks for is larger than that: the loupe, its neighbours' prefetch and the stand-ins
#[test]
fn nothing_is_rendered_larger_than_the_gpu_texture_side() {
    let s = with_limit(0);
    for tex in [512, 2048, 4096, 8192] {
        for wanted in [100.0, 2000.0, 6000.0, 48000.0, f32::INFINITY] {
            let e = s.loupe_edge(wanted, 12000, tex);
            assert!(e <= tex, "loupe {e} > {tex}");
            assert!(s.prefetch_edge(wanted, 12000, tex) <= tex);
            assert!(s.stand_in_edge(e, tex) <= tex);
        }
    }
    assert_eq!(s.loupe_edge(6000.0, 6000, 2048), 2048);
}

// A host that reports nothing sensible gets the smallest safe sizes, never a panic or zero
#[test]
fn a_nonsense_texture_side_is_clamped() {
    let s = with_limit(0);
    for tex in [0, 1, 7] {
        let e = s.loupe_edge(1000.0, 6000, tex);
        assert!((8..=512).contains(&e), "{tex}: {e}");
    }
}

// Whatever the inputs (hostile or degenerate), the result is a usable size
#[test]
fn degenerate_inputs_give_a_usable_size() {
    let s = with_limit(0);
    for wanted in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -5.0, 0.0] {
        let e = s.loupe_edge(wanted, 6000, BIG);
        assert!((8..=LOUPE_EDGE_CEILING as usize).contains(&e), "{wanted}: {e}");
    }
    assert!((8..=LOUPE_EDGE_CEILING as usize).contains(&s.loupe_edge(1000.0, 0, BIG)) && s.prefetch_edge(f32::NAN, 0, BIG) >= 8);
    assert!(with_limit(7).loupe_edge(1000.0, 6000, BIG) >= 8, "a corrupt limit is clamped");
}

// Build Standard-Sized Previews needs a number even when the limit is Automatic
#[test]
fn standard_previews_use_the_limit_or_the_standard_size() {
    assert_eq!(with_limit(0).standard_preview_edge(), STANDARD_PREVIEW_EDGE);
    assert_eq!(with_limit(3840).standard_preview_edge(), 3840);
}

// Given a ui.json from before this setting (key previewEdge), when it loads, then the user is on
// Automatic: the old default of 2560 was the bug, and the key no longer means anything
#[test]
fn an_old_saved_preview_edge_does_not_pin_the_cap() {
    let s: AppSettings = serde_json::from_str(r#"{"previewEdge": 2560}"#).unwrap();
    assert_eq!(s.preview_limit, 0);
}

// A saved limit is kept; an unknown one (hand-edited file) falls back to Automatic
#[test]
fn saved_limits_round_trip_and_unknown_ones_are_reset() {
    let mut ui = crate::UiState::default();
    ui.settings.preview_limit = 3840;
    let back = serde_json::from_str::<crate::UiState>(&serde_json::to_string(&ui).unwrap()).unwrap().sanitized();
    assert_eq!(back.settings.preview_limit, 3840);
    ui.settings.preview_limit = 123;
    assert_eq!(ui.sanitized().settings.preview_limit, 0);
}

// ---- Behaviour in the running UI -------------------------------------------------------------

mod in_the_loupe {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use serde_json::json;

    use crate::headless::Headless;
    use crate::render::{RenderOffload, Renderer, Slot};
    use crate::tests_render_fixtures::{HEIGHT, LONG_EDGE, RenderFixture, WIDTH};
    use crate::{LightcraftApp, Services};
    use lightcraft_engine::SourceLevel;
    use lightcraft_engine::media::{RenderJob, RenderResult, SourceRef};

    const T: Duration = Duration::from_secs(20);
    const SETTLE: Duration = Duration::from_secs(120);

    /// The original 24 MP procedural demo library in a 1400×900 window, the first photo open
    /// in Detail. Kept for `profile_slider_drag`, whose meaning depends on the demo's render cost
    /// and neighbour prefetch.
    fn demo_detail_in(size: [f32; 2]) -> (Headless, usize) {
        let services = Services { png: None, ..Default::default() };
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
        let mut h = Headless::new(app, size, 1.0);
        let r = h.request("ui.set", json!({"view": "detail", "right": "none", "filmstrip": false}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let native = {
            let id = h.app.session.active().expect("a photo is open");
            let p = h.app.session.catalog.photo(id).expect("it is in the catalog");
            p.width.max(p.height) as usize
        };
        (h, native)
    }

    /// Two real 6000×4000 file-backed photos in a 1400×900 window, the first open in Detail.
    /// Destructure the returned tuple immediately, binding the fixture before `Headless`, so the
    /// app drops first. Keeping the tuple intact would drop its fixture field first.
    fn detail() -> (RenderFixture, Headless, usize) {
        detail_in([1400.0, 900.0])
    }

    fn detail_in(size: [f32; 2]) -> (RenderFixture, Headless, usize) {
        detail_with(RenderFixture::new("preview", 2), size)
    }

    fn detail_with(fixture: RenderFixture, size: [f32; 2]) -> (RenderFixture, Headless, usize) {
        let services = Services { png: None, ..Default::default() };
        let app = LightcraftApp::new(fixture.session(), services);
        let mut h = Headless::new(app, size, 1.0);
        let r = h.request("ui.set", json!({"view": "detail", "right": "none", "filmstrip": false}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let (native, dims) = {
            let id = h.app.session.active().expect("a photo is open");
            let p = h.app.session.catalog.photo(id).expect("it is in the catalog");
            (p.width.max(p.height) as usize, (p.width, p.height))
        };
        assert_eq!(dims, (WIDTH, HEIGHT), "the render fixture is a real 24 MP landscape 3:2 photo");
        assert_eq!(native, LONG_EDGE, "the render fixture is a real 24 MP photo");
        (fixture, h, native)
    }

    fn rendered_long_edge(h: &Headless) -> usize {
        let t = h.app.renderer.textures.get(&Slot::Main).expect("the loupe rendered");
        t.size[0].max(t.size[1])
    }

    // Given a photo larger than the canvas, when I zoom to 1:1, then the whole frame stays about
    // canvas-sized (a drag redoes that, not 24 MP) and the pixels on screen come from a window
    // cut from the photo's own pixels
    #[test]
    fn one_to_one_is_a_canvas_sized_frame_plus_a_native_window() {
        let (_fx, mut h, native) = detail();
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        let main = rendered_long_edge(&h);
        assert!(main <= 2560 && main < native, "native {native}: the whole-frame render is {main}");
        let region = h.app.region_view.expect("a window render");
        assert_eq!(region.full.0.max(region.full.1), native, "the window is cut from the photo's own pixels");
        let (w, hh) = region_tile(&h).expect("its texture");
        assert!(w >= 1300 && hh >= 700, "it covers the canvas: {w}×{hh}");
    }

    // Given a mask selected with its overlay shown at 1:1 (issue #547), the zoomed view keeps its
    // zoom and pan and stays a canvas-sized frame plus a native window carrying the overlay, as
    // without it, instead of a whole-frame render at the zoomed size; hiding or showing the
    // overlay swaps the window for one with the other look
    #[test]
    fn a_mask_overlay_at_one_to_one_is_a_window_and_keeps_zoom_and_pan() {
        let (_fx, mut h, native) = detail();
        for (command, params) in [("panel.masking", json!({})), ("mask.add", json!({"kind": "radial", "center": [0.4, 0.5], "rx": 0.2, "ry": 0.2}))] {
            let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
            assert_eq!(r["ok"], true, "{command}: {r}");
        }
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.app.ui.pan = (0.3, 0.4);
        h.app.ui.mask_overlay = false;
        h.settle(SETTLE);
        let (zoom, pan) = (h.app.ui.zoom, h.app.ui.pan);
        assert_eq!(zoom, crate::state::Zoom::Percent(100.0));
        let plain = h.app.region_view.expect("a window render without the overlay");

        h.app.ui.mask_overlay = true;
        h.settle(SETTLE);
        let id = h.app.session.active().expect("a photo");
        let d = h.app.session.develop_of(id).expect("settings");
        assert!(matches!(crate::panels::detail::view_overlay(&h.app, &d), lightcraft_pipeline::Overlay::Mask { .. }), "the mask overlay is shown");
        assert_eq!((h.app.ui.zoom, h.app.ui.pan), (zoom, pan), "showing the overlay keeps zoom and pan");
        let main = rendered_long_edge(&h);
        assert!(main <= 2560 && main < native, "native {native}: the whole-frame render is {main}");
        let shown = h.app.region_view.expect("a window render with the overlay");
        assert_eq!(shown.full.0.max(shown.full.1), native, "cut from the photo's own pixels");
        assert_eq!(shown.window, plain.window, "the same part of the photo");
        assert_ne!(shown.settings, plain.settings, "the overlay is part of the window's look");

        h.app.ui.mask_overlay = false;
        h.settle(SETTLE);
        assert_eq!((h.app.ui.zoom, h.app.ui.pan), (zoom, pan), "hiding it keeps them too");
        assert_eq!(h.app.region_view.map(|r| r.settings), Some(plain.settings), "back to the plain window");
    }

    // Given the Show All Masks view (issue #1712) at 1:1, the zoomed loupe is a canvas-sized frame
    // plus a native window carrying the overlay, as a selected mask's is: the union is evaluated
    // from the same mask alphas, so it must not fall back to a whole-frame render at the zoomed
    // size (slower, and its detail stops at the frame render's resolution)
    #[test]
    fn an_all_masks_overlay_at_one_to_one_is_a_window() {
        let (_fx, mut h, native) = detail();
        for (command, params) in [
            ("panel.masking", json!({})),
            ("mask.add", json!({"kind": "radial", "center": [0.4, 0.5], "rx": 0.2, "ry": 0.2})),
            ("mask.add", json!({"kind": "linear", "start": [0.2, 0.3], "end": [0.8, 0.7]})),
        ] {
            let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
            assert_eq!(r["ok"], true, "{command}: {r}");
        }
        let r = h.request("engine.execute", json!({"command": "view.maskOverlayAll", "params": {}}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.app.ui.mask_overlay_all, "the all-masks overlay is on");
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        let id = h.app.session.active().expect("a photo");
        let d = h.app.session.develop_of(id).expect("settings");
        assert!(
            matches!(crate::panels::detail::view_overlay(&h.app, &d), lightcraft_pipeline::Overlay::AllMasks { .. }),
            "the all-masks overlay is shown"
        );
        let main = rendered_long_edge(&h);
        assert!(main <= 2560 && main < native, "native {native}: the whole-frame render is {main}");
        let region = h.app.region_view.expect("a window render with the all-masks overlay");
        assert_eq!(region.full.0.max(region.full.1), native, "cut from the photo's own pixels");
        let (w, hh) = region_tile(&h).expect("its texture");
        assert!(w >= 1300 && hh >= 700, "it covers the canvas: {w}×{hh}");
    }

    // Given soft proofing on at 1:1 (LR-VIEW-ZOOM), the view is a canvas-sized frame plus a
    // native window carrying the proof, as without it, instead of a whole-frame render at the
    // photo's full size; switching the proof off swaps the window back to the plain look
    #[test]
    fn soft_proofing_at_one_to_one_is_a_window() {
        let (_fx, mut h, native) = detail();
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        let plain = h.app.region_view.expect("a window render without the proof");
        let r = h.request("engine.execute", json!({"command": "view.softProof", "params": {"on": true, "destWarning": true}}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!(h.app.ui.soft_proof);
        let main = rendered_long_edge(&h);
        assert!(main <= 2560 && main < native, "native {native}: the whole-frame render is {main}");
        let proofed = h.app.region_view.expect("a window render with the proof");
        assert_eq!(proofed.full.0.max(proofed.full.1), native, "cut from the photo's own pixels");
        assert_ne!(proofed.settings, plain.settings, "the proof is part of the window's look");
        h.request("engine.execute", json!({"command": "view.softProof", "params": {"on": false}}), T);
        h.settle(SETTLE);
        assert_eq!(h.app.region_view.map(|r| r.settings), Some(plain.settings), "back to the plain window");
    }

    // Given a diagnostic overlay (Visualize Spots), when I zoom to 1:1, then the visible part is a
    // window render carrying the overlay, over a canvas-sized whole frame, as without it
    #[test]
    fn visualize_spots_at_one_to_one_is_a_window() {
        let (_fx, mut h, native) = detail();
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        let plain = h.app.region_view.expect("a window render without the overlay");
        let r = h.request("ui.menu.invoke", json!({"id": "view.visualizeSpots"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!(h.app.ui.visualize_spots);
        let main = rendered_long_edge(&h);
        assert!(main <= 2560 && main < native, "native {native}: the whole-frame render is {main}");
        let spots = h.app.region_view.expect("a window render with the overlay");
        assert_eq!(spots.full.0.max(spots.full.1), native, "cut from the photo's own pixels");
        assert_ne!(spots.settings, plain.settings, "the overlay is part of the window's look");
    }

    // Given the user capped the preview at 1600 px, then 1:1 is rendered no larger than that
    #[test]
    fn an_explicit_limit_still_caps_one_to_one() {
        let (_fx, mut h, native) = detail();
        assert!(native > 1600, "the demo photo must be larger than the cap for this to mean anything (is {native})");
        h.app.ui.settings.preview_limit = 1600;
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        assert!(rendered_long_edge(&h) <= 1600, "rendered {}", rendered_long_edge(&h));
    }

    fn region_tile(h: &Headless) -> Option<(usize, usize)> {
        h.app.renderer.textures.get(&Slot::Region).map(|t| (t.size[0], t.size[1]))
    }

    // Given a fit view, the whole-frame render is all there is: no second render, no original decoded
    #[test]
    fn a_fit_view_makes_no_window_render() {
        let (_fx, h, _) = detail();
        assert_eq!(region_tile(&h), None);
        assert_eq!(h.app.region_view, None);
    }

    // Given the user capped the preview at 1600 px, when I zoom to 1:1, then the visible part is
    // rendered on its own at the zoom scale and drawn over the whole-frame render
    #[test]
    fn a_capped_one_to_one_view_gets_a_sharp_window() {
        let (_fx, mut h, native) = detail();
        h.app.ui.settings.preview_limit = 1600;
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        let (w, hh) = region_tile(&h).expect("a window render");
        // it covers what is on screen (the 1400×900 window less its chrome), and is not the frame
        assert!(w >= 1300 && hh >= 700, "{w}×{hh}");
        assert!(w < native && hh < native, "{w}×{hh} of a {native} px frame");
        let region = h.app.region_view.expect("the window is described for the inspector");
        assert_eq!(region.full.0.max(region.full.1), native, "1:1 means the frame is the photo's own size");
    }

    // Given 800 % zoom (a frame of 48000 px), the window is rendered and bounded
    #[test]
    fn eight_to_one_renders_only_what_is_visible() {
        let (_fx, mut h, native) = detail();
        h.app.ui.zoom = crate::state::Zoom::Percent(800.0);
        h.settle(SETTLE);
        let (w, hh) = region_tile(&h).expect("a window render");
        assert!(w.max(hh) <= crate::region::MAX_SPAN && w < native * 8 / 2, "{w}×{hh}");
        // …and panning far away asks for another window
        let before = h.app.region_view.unwrap().window;
        h.app.ui.pan = (0.9, 0.9);
        h.settle(SETTLE);
        assert_ne!(h.app.region_view.unwrap().window, before);
    }

    // The window is drawn where it belongs: its pixels agree with the whole-frame render at the
    // same place of the frame (a shift, a flip or a wrong scale would not)
    #[test]
    fn the_window_lines_up_with_the_whole_frame_render() {
        let (_fx, mut h, _) = detail();
        h.app.renderer.keep_pixels = true;
        h.app.ui.settings.preview_limit = 1600;
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        let view = h.app.region_view.expect("a window render");
        let tile = h.app.renderer.textures.get(&Slot::Region).and_then(|t| t.pixels.clone()).expect("tile pixels");
        let main = h.app.renderer.textures.get(&Slot::Main).and_then(|t| t.pixels.clone()).expect("main pixels");
        let mean_difference = |offset: usize| {
            let (mut sum, mut n) = (0.0f32, 0.0f32);
            for j in (8..tile.size[1] - 8).step_by(37) {
                for i in (8..tile.size[0] - 8).step_by(37) {
                    // the same point of the frame in the whole-frame render
                    let u = (view.window.x + i + offset) as f32 / view.full.0 as f32;
                    let v = (view.window.y + j) as f32 / view.full.1 as f32;
                    let (mx, my) =
                        (((u * main.size[0] as f32) as usize).min(main.size[0] - 1), ((v * main.size[1] as f32) as usize).min(main.size[1] - 1));
                    let (a, b) = (tile.pixels[j * tile.size[0] + i], main.pixels[my * main.size[0] + mx]);
                    sum += (0..3).map(|k| (a.to_array()[k] as f32 - b.to_array()[k] as f32).abs()).sum::<f32>() / 3.0;
                    n += 1.0;
                }
            }
            sum / n
        };
        let difference = mean_difference(0);
        assert!(difference < 6.0, "mean difference {:.2} / 255 between the window and the frame render", difference);
        assert!(mean_difference(16) > 6.0, "the fixture must expose a 16-pixel horizontal misalignment");
    }

    // Given a Fit view, a slider drag (whose Main draft is at 0.6 scale) does not start window renders
    #[test]
    fn a_slider_drag_at_fit_makes_no_window_render() {
        let (_fx, mut h, _) = detail();
        h.app.session.begin_interaction("Exposure").unwrap();
        for _ in 0..5 {
            h.step();
        }
        assert_eq!(h.app.region_view, None);
        assert_eq!(region_tile(&h), None);
    }

    // Given a zoomed view with a window render, when I zoom back to Fit, then the window's texture is freed
    #[test]
    fn the_window_texture_is_freed_when_it_is_not_needed() {
        let (_fx, mut h, _) = detail();
        h.app.ui.settings.preview_limit = 1600;
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        assert!(region_tile(&h).is_some());
        h.request("engine.execute", json!({"command": "view.zoomFit"}), T);
        h.settle(SETTLE);
        assert_eq!(region_tile(&h), None);
        assert_eq!(h.app.region_view, None);
    }

    // Given a zoomed view with a window render, when I go to the grid, then its windows are freed
    // too (while one is held the app never counts as idle and keeps the GPU copy of the original)
    #[test]
    fn leaving_a_zoomed_detail_view_frees_its_windows() {
        let (_fx, mut h, _) = detail();
        h.app.ui.before_after = crate::state::BeforeAfter::SideBySide;
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        assert!(tile(&h, Slot::Region).is_some() && tile(&h, Slot::RegionBefore).is_some());
        h.request("ui.set", json!({"view": "photoGrid"}), T);
        h.step();
        assert_eq!((tile(&h, Slot::Region), tile(&h, Slot::RegionBefore)), (None, None));
        assert_eq!((h.app.region_view, h.app.region_before_view), (None, None));
    }

    // Given a zoomed view, when the photo is edited, then the old window (made with the old look)
    // is no longer current and is replaced by one for the new look
    #[test]
    fn an_edit_replaces_the_window() {
        let (_fx, mut h, _) = detail();
        h.app.ui.settings.preview_limit = 1600;
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        let first = h.app.region_view.expect("a window");
        h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 1.0}}), T);
        h.settle(SETTLE);
        let second = h.app.region_view.expect("a window");
        assert_ne!(first.settings, second.settings);
        assert_ne!(first.key, second.key);
        assert_eq!(h.app.renderer.textures.get(&Slot::Region).map(|t| t.key), Some(second.key), "the new window arrived");
    }

    // Given a photo with spot removal, a zoomed view still gets its sharp window
    #[test]
    fn a_photo_with_spots_gets_a_window_too() {
        let (_fx, mut h, _) = detail();
        let id = h.app.session.active().unwrap();
        let mut s = (*h.app.session.develop_of(id).unwrap()).clone();
        s.spots.push(lightcraft_develop::Spot {
            points: vec![lightcraft_geom::Point::new(0.5, 0.5)],
            size: 0.01,
            source_offset: Some(lightcraft_geom::Point::new(0.05, 0.0)),
            ..Default::default()
        });
        h.app.session.set_develop(id, s, "Spot").unwrap();
        h.app.ui.settings.preview_limit = 1600;
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        assert!(region_tile(&h).is_some(), "a window render");
    }

    // Given a browser GPU that takes only 2048 px textures (egui_glow panics on a bigger one), when
    // I zoom to 1:1 and 8:1, then no texture the loupe holds is larger than that
    #[test]
    fn no_loupe_texture_exceeds_the_gpu_limit() {
        let (_fx, mut h, _) = detail();
        h.max_texture_side = 2048;
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        for zoom in [crate::state::Zoom::Fit, crate::state::Zoom::Percent(100.0), crate::state::Zoom::Percent(800.0)] {
            h.app.ui.zoom = zoom;
            h.settle(SETTLE);
            for (slot, t) in &h.app.renderer.textures {
                assert!(t.size[0] <= 2048 && t.size[1] <= 2048, "{zoom:?} {slot:?}: {}×{}", t.size[0], t.size[1]);
            }
            let main = h.app.renderer.textures.get(&Slot::Main).expect("loupe");
            assert!(main.size[0].max(main.size[1]) >= 700, "still a real render, not a thumbnail: {:?}", main.size);
        }
    }

    // Issue #652: given a canvas wider than 2048 px on a desktop GPU (which reports its texture
    // limit to egui once, in the first frame), when I zoom to 1:1, then the window render covers
    // the whole canvas: no strip of the magnified whole-frame render beside it
    #[test]
    fn a_window_covers_a_canvas_wider_than_2048_px() {
        let (_fx, mut h, native) = detail_in([2600.0, 1200.0]);
        assert!(native > 2600, "the photo is wider than the canvas at 1:1: {native}");
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        let canvas = h.app.canvas_rect.expect("the loupe was drawn");
        let image = h.app.image_rect.expect("the photo was drawn");
        let region = h.app.region_view.expect("a window");
        // the canvas in pixels of the window's frame (scale 1, at 100 %: a point is a pixel)
        let to_px = region.full.0 as f32 / image.width();
        let (left, right) = (to_px * (canvas.left() - image.left()), to_px * (canvas.right() - image.left()));
        assert!(right - left > 2048.0, "the canvas shows more than 2048 px: {left}..{right}");
        let (x0, x1) = (region.window.x as f32, (region.window.x + region.window.w) as f32);
        assert!(x0 <= left + 0.5 && x1 >= right - 0.5, "the window {x0}..{x1} leaves part of the canvas {left}..{right} to the preview");
        let (w, _) = region_tile(&h).expect("the window rendered");
        assert_eq!(w, region.window.w, "and that is the texture drawn");
    }

    // Given 400 % on a big photo, when a slider is dragged, then the work per frame follows the
    // canvas, not the zoom: a draft of the whole frame at canvas scale and a draft of the window
    #[test]
    fn a_slider_drag_when_zoomed_in_renders_canvas_sized_drafts() {
        let (_fx, mut h, native) = detail();
        h.app.ui.zoom = crate::state::Zoom::Percent(400.0);
        h.settle(SETTLE);
        h.app.session.begin_interaction("Exposure").unwrap();
        h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 0.5}}), T);
        h.settle(SETTLE);
        let main = rendered_long_edge(&h);
        // canvas-sized: the whole canvas on the GPU, a 0.6 draft of it on the CPU (#330)
        let canvas = h.app.canvas_rect.expect("the loupe was drawn");
        let canvas_long = canvas.width().max(canvas.height()) as usize + crate::state::LOUPE_EDGE_STEP;
        assert!(main <= canvas_long && main * 4 < native, "the whole-frame draft is {main} px for a {native} px photo");
        let region = h.app.region_view.expect("the window is drafted too");
        assert_eq!(region.full.0.max(region.full.1), native, "at 100 %, magnified by the GPU");
        let (w, hh) = region_tile(&h).expect("a window draft");
        assert!(w * hh <= 3_000_000, "{w}×{hh}: the window holds what is on screen, not the frame");
    }

    /// Drag Exposure in the loupe and return the long edge of the whole-frame draft shown, with
    /// the largest a small (CPU-sized) draft of the photo as drawn can be.
    fn drag_exposure(h: &mut Headless) -> (usize, usize) {
        h.app.session.begin_interaction("Exposure").unwrap();
        h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 0.5}}), T);
        h.settle(SETTLE);
        let image = h.app.image_rect.expect("the photo was drawn");
        let small = (image.width().max(image.height()) * crate::region::CPU_DRAFT_SCALE) as usize + crate::state::LOUPE_EDGE_STEP;
        (rendered_long_edge(h), small)
    }

    // Given a drag whose full-size drafts were too slow before (the CPU, a LUT profile, a slow
    // GPU), when the same slider is dragged again, then it drafts small (#330)
    #[test]
    fn a_slow_drag_drafts_small() {
        // the GPU device up front (the loupe never waits for it): with one, it is the earlier
        // drag's time that makes this drag small
        let _ = lightcraft_engine::gpu::available();
        let (_fx, mut h, _) = detail();
        let id = h.app.session.active().expect("a photo is open");
        h.app.drag_scale.remember_slow(id, "Exposure");
        let (main, small) = drag_exposure(&mut h);
        assert!(main <= small, "the draft is {main} px, a small one at most {small}");
    }

    // Given soft proofing against an output-device profile (rendered on the CPU even with a GPU), a
    // drag drafts small (#330); proofs into an output space run on the GPU like any render
    #[test]
    fn a_soft_proofed_drag_drafts_small() {
        // the GPU device up front (the loupe never waits for it): with one, this is the case where
        // the GPU is there and the render still goes to the CPU
        let _ = lightcraft_engine::gpu::available();
        let (_fx, mut h, _) = detail();
        let lut = lightcraft_pipeline::ProofLut::new(2, vec![[0.5, 0.5, 0.5, 0.0]; 8]).expect("a valid table");
        h.app.ui.proof.profile = lightcraft_pipeline::register_proof_profile(0x5eed_9f01, lut);
        h.app.ui.soft_proof = true;
        h.settle(SETTLE);
        let (main, small) = drag_exposure(&mut h);
        assert!(main <= small, "the draft is {main} px, a small one at most {small}");
    }

    fn tile(h: &Headless, slot: Slot) -> Option<(usize, usize)> {
        h.app.renderer.textures.get(&slot).map(|t| (t.size[0], t.size[1]))
    }

    // Given Before/After side by side at 1:1, both sides are sharp windows of the same frame: the
    // Before is not a 2560 px stand-in beside a full resolution After
    #[test]
    fn side_by_side_at_one_to_one_compares_like_with_like() {
        let (_fx, mut h, native) = detail();
        h.app.ui.before_after = crate::state::BeforeAfter::SideBySide;
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        let (after, before) = (h.app.region_view.expect("After window"), h.app.region_before_view.expect("Before window"));
        assert_eq!(after.full, before.full, "the same frame");
        assert_eq!(after.full.0.max(after.full.1), native);
        let (a, b) = (tile(&h, Slot::Region).expect("After tile"), tile(&h, Slot::RegionBefore).expect("Before tile"));
        assert_eq!(a, b, "the same window of each");
        let main = h.app.renderer.textures.get(&Slot::Main).unwrap();
        let before_main = h.app.renderer.textures.get(&Slot::Before).unwrap();
        assert_eq!(main.size, before_main.size, "and the same whole-frame size under them");
    }

    // Given Before/After side by side, when I zoom to 1:1, then one window job decodes the original
    // and the other side waits for it (the Before, asked for first, used to start a second decode
    // beside the After's); both windows still arrive
    #[test]
    fn side_by_side_at_one_to_one_decodes_the_original_once() {
        let (_fx, mut h, _) = detail();
        let id = h.app.session.active().expect("a photo is open");
        h.app.ui.before_after = crate::state::BeforeAfter::SideBySide;
        h.app.ui.zoom = crate::state::Zoom::Percent(100.0);
        let pending = |h: &Headless| [Slot::Region, Slot::RegionBefore].into_iter().filter(|s| h.app.renderer.is_pending(*s)).count();
        for _ in 0..100 {
            h.step();
            if pending(&h) > 0 {
                break;
            }
        }
        assert!(!h.app.session.media.has_source(id, lightcraft_engine::SourceLevel::Full), "the original is still decoding");
        assert_eq!(pending(&h), 1, "one window job decodes it");
        h.settle(SETTLE);
        assert!(tile(&h, Slot::Region).is_some() && tile(&h, Slot::RegionBefore).is_some(), "both windows arrived");
    }

    // Given the Before view alone (\), only the Before gets a window
    #[test]
    fn the_original_view_windows_only_the_before() {
        let (_fx, mut h, _) = detail();
        h.app.ui.before_after = crate::state::BeforeAfter::Original;
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        assert!(h.app.region_before_view.is_some());
        assert_eq!(h.app.region_view, None);
        assert!(tile(&h, Slot::RegionBefore).is_some() && tile(&h, Slot::Region).is_none());
    }

    // Given a wipe (Split), both sides are windows, drawn over the same rect
    #[test]
    fn a_wipe_windows_both_sides() {
        let (_fx, mut h, _) = detail();
        h.app.ui.before_after = crate::state::BeforeAfter::Split;
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        assert_eq!(h.app.region_view.map(|v| v.window), h.app.region_before_view.map(|v| v.window));
        assert!(tile(&h, Slot::Region).is_some() && tile(&h, Slot::RegionBefore).is_some());
    }

    // Given the Before view turned off, its window is freed
    #[test]
    fn leaving_before_after_frees_the_before_window() {
        let (_fx, mut h, _) = detail();
        h.app.ui.before_after = crate::state::BeforeAfter::SideBySide;
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        assert!(tile(&h, Slot::RegionBefore).is_some());
        h.app.ui.before_after = crate::state::BeforeAfter::Off;
        h.settle(SETTLE);
        assert_eq!(tile(&h, Slot::RegionBefore), None);
        assert_eq!(h.app.region_before_view, None);
        assert!(tile(&h, Slot::Region).is_some());
    }

    // Given a trackpad pinch from fit to well past 100 %, the render sizes are held while it runs:
    // no stream of renders of every size in between, and the sharp window arrives once it stops
    #[test]
    fn a_pinch_holds_the_render_sizes_until_it_stops() {
        let (_fx, mut h, native) = detail();
        let main_at_fit = rendered_long_edge(&h);
        let at = h.app.canvas_rect.unwrap().center();
        h.request("ui.move", json!({"x": at.x, "y": at.y}), T);
        for _ in 0..40 {
            h.request("ui.zoom", json!({"factor": 1.08}), T);
            assert_eq!(h.app.region_view, None, "no window while the pinch runs");
        }
        assert!(matches!(h.app.ui.zoom, crate::state::Zoom::Percent(p) if p > 100.0), "{:?}", h.app.ui.zoom);
        assert_eq!(rendered_long_edge(&h), main_at_fit, "the whole-frame render keeps its size");
        assert!(!h.app.renderer.is_pending(Slot::Region), "no window was asked for during the pinch");
        for _ in 0..60 {
            h.step();
        }
        h.settle(SETTLE);
        let region = h.app.region_view.expect("the window arrives when the pinch is over");
        assert_eq!(region.full.0.max(region.full.1), native);
    }

    #[derive(Default)]
    struct PinchWork {
        jobs: Vec<(Slot, lightcraft_engine::media::RenderJob)>,
        finished: Vec<(Slot, lightcraft_engine::media::RenderResult, f64)>,
    }

    struct PinchOffload(std::rc::Rc<std::cell::RefCell<PinchWork>>);

    impl crate::render::RenderOffload for PinchOffload {
        fn try_start(&mut self, slot: Slot, job: lightcraft_engine::media::RenderJob) -> Option<lightcraft_engine::media::RenderJob> {
            self.0.borrow_mut().jobs.push((slot, job));
            None
        }

        fn finished(&mut self) -> Vec<(Slot, lightcraft_engine::media::RenderResult, f64)> {
            std::mem::take(&mut self.0.borrow_mut().finished)
        }
    }

    // Complete the first pinch-step render only after later steps have superseded it. Running
    // real jobs through the offload makes the worker order explicit without timing sleeps.
    #[test]
    fn pinch_display_fit_keeps_the_request_key_until_the_hold_expires() {
        let (mut h, native) = demo_detail_in([1400.0, 900.0]);
        assert!(h.settle(SETTLE), "the starting render must be complete");
        let main = h.app.renderer.textures.get(&Slot::Main).unwrap();
        let (original_key, original_size) = (main.key, main.size);
        let work = std::rc::Rc::new(std::cell::RefCell::new(PinchWork::default()));
        h.app.renderer.set_offload(Box::new(PinchOffload(work.clone())));
        let at = h.app.canvas_rect.unwrap().center();
        assert_eq!(h.request("ui.move", json!({"x": at.x, "y": at.y}), T)["ok"], true);
        let mut keys = std::collections::HashSet::new();
        for _ in 0..40 {
            assert_eq!(h.request("ui.zoom", json!({"factor": 1.08}), T)["ok"], true);
            keys.insert(h.app.renderer.wanted(Slot::Main));
            assert!(h.app.region_view.is_none());
        }
        let first = work.borrow().jobs.iter().position(|(slot, _)| *slot == Slot::Main);
        if let Some(i) = first {
            let (slot, job) = work.borrow_mut().jobs.remove(i);
            let result = job.run();
            assert!(result.rendered.is_ok());
            work.borrow_mut().finished.push((slot, result, 0.0));
            h.app.renderer.poll(&h.view.ctx, &mut h.app.session);
        }
        h.step();
        let main = h.app.renderer.textures.get(&Slot::Main).unwrap();
        assert_eq!(main.size, original_size, "the whole-frame render keeps its size even when workers finish during the pinch");
        assert_eq!(keys, std::collections::HashSet::from([Some(original_key)]), "a held gesture must not schedule new fits");
        for _ in 0..60 {
            h.step();
        }
        // Finish the final requests at the chosen frame, without advancing time while they run.
        let jobs = std::mem::take(&mut work.borrow_mut().jobs);
        for (slot, job) in jobs {
            let result = job.run();
            assert!(result.rendered.is_ok());
            work.borrow_mut().finished.push((slot, result, 0.0));
        }
        h.step();
        let region = h.app.region_view.expect("the final window arrives after the hold expires");
        assert_eq!(region.full.0.max(region.full.1), native);
        assert_eq!(h.app.renderer.textures.get(&Slot::Region).unwrap().key, region.key);
        assert!(!h.app.renderer.is_pending(Slot::Main) && !h.app.renderer.is_pending(Slot::Region));
        assert_ne!(h.app.renderer.wanted(Slot::Main), Some(original_key));
    }

    fn complete_pinch_jobs(h: &mut Headless, work: &std::rc::Rc<std::cell::RefCell<PinchWork>>) {
        let jobs = std::mem::take(&mut work.borrow_mut().jobs);
        for (slot, job) in jobs {
            let result = job.run();
            assert!(result.rendered.is_ok());
            work.borrow_mut().finished.push((slot, result, 0.0));
        }
        h.step();
    }

    fn drawn_fit(h: &Headless) -> (usize, usize) {
        let rect = h.app.image_rect.unwrap();
        let ppp = h.view.ctx.pixels_per_point();
        ((rect.width() * ppp).round() as usize, (rect.height() * ppp).round() as usize)
    }

    #[test]
    fn pinch_display_fit_refreshes_main_and_before_after_a_short_pinch() {
        let (mut h, _) = demo_detail_in([1400.0, 900.0]);
        h.app.ui.before_after = crate::state::BeforeAfter::SideBySide;
        assert!(h.settle(SETTLE));
        let slots = [Slot::Main, Slot::Before];
        let keys = slots.map(|slot| h.app.renderer.wanted(slot).unwrap());
        let initial_fit = drawn_fit(&h);
        let work = std::rc::Rc::new(std::cell::RefCell::new(PinchWork::default()));
        h.app.renderer.set_offload(Box::new(PinchOffload(work.clone())));
        let at = h.app.image_rect.unwrap().center();
        assert_eq!(h.request("ui.move", json!({"x": at.x, "y": at.y}), T)["ok"], true);
        for _ in 0..2 {
            assert_eq!(h.request("ui.zoom", json!({"factor": 1.08}), T)["ok"], true);
            assert_eq!(slots.map(|slot| h.app.renderer.wanted(slot).unwrap()), keys);
        }
        let final_fit = drawn_fit(&h);
        assert!(final_fit.0 > initial_fit.0 && final_fit.1 > initial_fit.1, "the image still follows the gesture");
        assert!(h.app.size_hold.holding(h.view.ctx.input(|i| i.time)));
        assert!(work.borrow().jobs.iter().all(|(slot, _)| !slots.contains(slot)), "no new Main or Before job during the hold");
        for _ in 0..20 {
            h.step();
        }
        for slot in slots {
            let work = work.borrow();
            let jobs: Vec<_> = work.jobs.iter().filter(|(s, _)| *s == slot).collect();
            assert_eq!(jobs.len(), 1, "one final render for {slot:?}");
            assert_eq!(jobs[0].1.display_fit, Some(final_fit));
        }
        complete_pinch_jobs(&mut h, &work);
        for slot in slots {
            let tex = h.app.renderer.textures.get(&slot).unwrap();
            assert!(tex.size[0].abs_diff(final_fit.0) <= 1 && tex.size[1].abs_diff(final_fit.1) <= 1, "{slot:?}: {:?} vs {final_fit:?}", tex.size);
            assert!(!h.app.renderer.is_pending(slot));
        }
        assert!(h.app.region_view.is_none(), "a short pinch still fits in the whole-frame render");
    }

    #[test]
    fn pinch_display_fit_does_not_follow_the_old_photo() {
        let (mut h, _) = demo_detail_in([1400.0, 900.0]);
        assert!(h.settle(SETTLE));
        let old = h.app.session.active().unwrap();
        let next = h.app.session.visible_cloned().into_iter().find(|id| *id != old).unwrap();
        let initial_fit = drawn_fit(&h);
        let work = std::rc::Rc::new(std::cell::RefCell::new(PinchWork::default()));
        h.app.renderer.set_offload(Box::new(PinchOffload(work.clone())));
        let at = h.app.image_rect.unwrap().center();
        assert_eq!(h.request("ui.move", json!({"x": at.x, "y": at.y}), T)["ok"], true);
        assert_eq!(h.request("ui.zoom", json!({"factor": 1.08}), T)["ok"], true);
        assert!(h.app.size_hold.holding(h.view.ctx.input(|i| i.time)));
        assert_eq!(h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [next.0]}}), T)["ok"], true);
        assert_eq!(h.app.session.active(), Some(next));
        assert!(!h.app.size_hold.holding(h.view.ctx.input(|i| i.time)), "the new photo resets the hold");
        let fit = drawn_fit(&h);
        assert_ne!(fit, initial_fit, "the fixture must distinguish the two fits");
        let job = work.borrow().jobs.iter().rev().find(|(slot, _)| *slot == Slot::Main).unwrap().1.clone();
        assert_eq!(job.photo, next);
        assert_eq!(job.display_fit, Some(fit));
        complete_pinch_jobs(&mut h, &work);
        assert_eq!(h.app.renderer.textures.get(&Slot::Main).unwrap().photo, next);
    }

    #[test]
    fn pinch_display_fit_is_shared_with_a_hover_during_the_hold() {
        let (mut h, _) = demo_detail_in([1400.0, 900.0]);
        assert_eq!(h.request("engine.execute", json!({"command": "panel.profiles"}), T)["ok"], true);
        assert!(h.settle(SETTLE));
        let initial_fit = drawn_fit(&h);
        let work = std::rc::Rc::new(std::cell::RefCell::new(PinchWork::default()));
        h.app.renderer.set_offload(Box::new(PinchOffload(work.clone())));
        let at = h.app.image_rect.unwrap().center();
        assert_eq!(h.request("ui.move", json!({"x": at.x, "y": at.y}), T)["ok"], true);
        assert_eq!(h.request("ui.zoom", json!({"factor": 1.08}), T)["ok"], true);
        assert_eq!(h.request("ui.hoverWidget", json!({"id": "profileCell:lc.vivid"}), T)["ok"], true);
        assert!(h.app.size_hold.holding(h.view.ctx.input(|i| i.time)));
        let job = work.borrow().jobs.iter().find(|(slot, _)| *slot == Slot::Hover).unwrap().1.clone();
        assert_eq!(job.display_fit, Some(initial_fit), "hover uses the held Main fit");
        complete_pinch_jobs(&mut h, &work);
        let held_key = h.app.renderer.wanted(Slot::Hover).unwrap();
        assert_eq!(h.app.loupe_shown.map(|shown| shown.1), Some("hover"));
        for _ in 0..20 {
            h.step();
        }
        let job = work.borrow().jobs.iter().rev().find(|(slot, _)| *slot == Slot::Hover).unwrap().1.clone();
        assert_eq!(job.display_fit, Some(drawn_fit(&h)));
        assert_ne!(job.key, held_key);
        complete_pinch_jobs(&mut h, &work);
        assert!(!h.app.renderer.is_pending(Slot::Hover));
    }

    #[test]
    fn pinch_display_fit_keeps_edits_live_and_refits_changed_crop_geometry() {
        let (mut h, _) = demo_detail_in([1400.0, 900.0]);
        assert!(h.settle(SETTLE));
        let initial_fit = drawn_fit(&h);
        let initial_key = h.app.renderer.wanted(Slot::Main).unwrap();
        let work = std::rc::Rc::new(std::cell::RefCell::new(PinchWork::default()));
        h.app.renderer.set_offload(Box::new(PinchOffload(work.clone())));
        let at = h.app.image_rect.unwrap().center();
        assert_eq!(h.request("ui.move", json!({"x": at.x, "y": at.y}), T)["ok"], true);
        assert_eq!(h.request("ui.zoom", json!({"factor": 1.08}), T)["ok"], true);
        for (control, value) in [("light.exposure", 0.75), ("crop.angle", -30.0)] {
            assert_eq!(h.request("engine.execute", json!({"command": "develop.set", "params": {"control": control, "value": value}}), T)["ok"], true);
        }
        assert!(h.app.size_hold.holding(h.view.ctx.input(|i| i.time)));
        let job = work.borrow().jobs.iter().rev().find(|(slot, _)| *slot == Slot::Main).unwrap().1.clone();
        assert_eq!(job.settings.light.exposure, 0.75);
        assert_eq!(job.settings.crop.geometry.angle, -30.0);
        assert!(job.request.apply_crop);
        assert_ne!(job.key, initial_key, "holding sizes does not suppress edits");
        assert_eq!(job.display_fit, Some(initial_fit));
        complete_pinch_jobs(&mut h, &work);
        let final_fit = drawn_fit(&h);
        assert_ne!(final_fit, initial_fit);
        for _ in 0..20 {
            h.step();
        }
        let job = work.borrow().jobs.iter().rev().find(|(slot, _)| *slot == Slot::Main).unwrap().1.clone();
        assert_eq!(job.display_fit, Some(final_fit));
        assert_eq!(job.settings.light.exposure, 0.75);
        assert_eq!(job.settings.crop.geometry.angle, -30.0);
        let key = job.key;
        complete_pinch_jobs(&mut h, &work);
        assert_eq!(h.app.renderer.textures.get(&Slot::Main).unwrap().key, key);
        assert!(!h.app.renderer.is_pending(Slot::Main));
    }

    // Given a picture on screen that is not the frame (an unsupported raw's embedded JPEG cropped
    // differently, #280), no window is rendered: its coordinates are the frame's
    #[test]
    fn no_window_over_a_picture_that_is_not_the_frame() {
        let (_fx, mut h, _) = detail();
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        assert!(h.app.region_view.is_some());
        let t = h.app.renderer.textures.get_mut(&Slot::Main).unwrap();
        t.size = [t.size[0], t.size[1] / 2];
        h.step();
        assert_eq!(h.app.region_view, None);
        assert_eq!(region_tile(&h), None);
    }

    /// `cargo test -p lightcraft-ui-egui --release profile_slider_drag -- --ignored --nocapture`:
    /// milliseconds from a slider tick to the loupe showing it, at 100 % and 400 % of the demo
    /// library's 24 MP photos (what the maintainers asked for in the review of #351).
    #[test]
    #[ignore = "a measurement, not a check"]
    fn profile_slider_drag() {
        for (size, percent, proof) in [
            ([1400.0, 900.0], 0.0, false),
            ([1400.0, 900.0], 100.0, false),
            ([1400.0, 900.0], 400.0, false),
            ([2800.0, 1700.0], 100.0, false),
            ([2800.0, 1700.0], 400.0, false),
            ([1400.0, 900.0], 100.0, true),
        ] {
            let (mut h, native) = demo_detail_in(size);
            h.app.ui.soft_proof = proof;
            h.app.ui.zoom = if percent == 0.0 { crate::state::Zoom::Fit } else { crate::state::Zoom::Percent(percent) };
            h.settle(SETTLE);
            h.app.session.begin_interaction("Exposure").unwrap();
            let mut ms = Vec::new();
            for tick in 0..20 {
                let t0 = std::time::Instant::now();
                let r = h.request(
                    "engine.execute",
                    json!({"command": "develop.set", "params": {"control": "light.exposure", "value": tick as f64 * 0.05}}),
                    T,
                );
                assert_eq!(r["ok"], true);
                let mut frames = 0;
                while (h.app.renderer.is_pending(Slot::Main) || h.app.renderer.is_pending(Slot::Region)) && frames < 2000 {
                    h.step();
                    std::thread::sleep(std::time::Duration::from_micros(200));
                    frames += 1;
                }
                ms.push(t0.elapsed().as_secs_f64() * 1e3);
            }
            ms.sort_by(|a, b| a.total_cmp(b));
            let (w, hh) = region_tile(&h).unwrap_or((0, 0));
            eprintln!(
                "PROFILE {}x{} canvas, {percent:>4}% (0 = fit){} of {native} px: tick→shown median {:.1} ms, p90 {:.1} ms, max {:.1} ms; whole-frame render {} px, window tile {w}×{hh}",
                size[0],
                size[1],
                if proof { ", soft proof" } else { "" },
                ms[ms.len() / 2],
                ms[ms.len() * 9 / 10],
                ms[ms.len() - 1],
                rendered_long_edge(&h)
            );
            eprintln!("        {}", h.app.renderer.memory());
            let job_ms = |slot| h.app.renderer.textures.get(&slot).map_or(0.0, |t| t.ms);
            eprintln!("        last job times: whole frame {:.1} ms, window {:.1} ms", job_ms(Slot::Main), job_ms(Slot::Region));
        }
    }

    /// Work held by a test offload. [`RegionGate`] keeps `Slot::Region` jobs here; [`Held`]
    /// keeps every job. `finished` is what the renderer's next `poll` collects.
    #[derive(Default)]
    struct OffloadWork {
        jobs: Vec<RenderJob>,
        finished: Vec<(Slot, RenderResult, f64)>,
    }

    fn take_job(work: &Rc<RefCell<OffloadWork>>) -> RenderJob {
        let mut w = work.borrow_mut();
        assert!(!w.jobs.is_empty(), "a render job was handed to the offload");
        w.jobs.remove(0)
    }

    /// The next window job the UI asked for, stepping a few frames if the request is still a frame
    /// away (deterministic frames, no wall-clock wait).
    fn next_region_job(h: &mut Headless, work: &Rc<RefCell<OffloadWork>>) -> RenderJob {
        for _ in 0..3 {
            if !work.borrow().jobs.is_empty() {
                break;
            }
            h.step();
        }
        take_job(work)
    }

    fn deliver(work: &Rc<RefCell<OffloadWork>>, slot: Slot, result: RenderResult) {
        work.borrow_mut().finished.push((slot, result, 0.0));
    }

    /// Holds the window jobs and runs every other job synchronously, returning it through
    /// `finished`: the UI keeps its real `request_window` / `defer_window` / draft path, but the
    /// window's original has not arrived until the test says so.
    struct RegionGate(Rc<RefCell<OffloadWork>>);

    impl RenderOffload for RegionGate {
        fn try_start(&mut self, slot: Slot, job: RenderJob) -> Option<RenderJob> {
            if slot == Slot::Region {
                self.0.borrow_mut().jobs.push(job);
            } else {
                let result = job.run();
                self.0.borrow_mut().finished.push((slot, result, 0.0));
            }
            None
        }

        fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)> {
            std::mem::take(&mut self.0.borrow_mut().finished)
        }
    }

    /// Holds every job for the test to run and deliver explicitly (the standalone collector test).
    struct Held(Rc<RefCell<OffloadWork>>);

    impl RenderOffload for Held {
        fn try_start(&mut self, _slot: Slot, job: RenderJob) -> Option<RenderJob> {
            self.0.borrow_mut().jobs.push(job);
            None
        }

        fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)> {
            std::mem::take(&mut self.0.borrow_mut().finished)
        }
    }

    fn drag_first_window(h: &mut Headless, work: &Rc<RefCell<OffloadWork>>, id: lightcraft_catalog::PhotoId) -> RenderJob {
        let first = next_region_job(h, work);
        let view = h.app.region_view.expect("Detail asked for a window");
        assert_eq!(view.photo, id);
        assert_eq!(view.full, (WIDTH as usize, HEIGHT as usize), "at 400 % the window's frame is the photo's own pixels");
        assert!(view.window.w >= 1300 && view.window.h >= 700, "the window covers the canvas: {:?}", view.window);
        assert!(view.window.x + view.window.w <= view.full.0 && view.window.y + view.window.h <= view.full.1, "inside the frame: {:?}", view.window);
        assert_eq!(first.request.window, Some(view.window), "the job is the window on screen");
        assert_eq!((first.request.max_w, first.request.max_h), view.full, "the job renders the frame the view uses");
        assert_eq!(first.request.quality, lightcraft_pipeline::Quality::Draft, "a drag drafts the window");
        assert_eq!(first.level, SourceLevel::Full);
        assert!(matches!(&first.source, SourceRef::File { .. }), "the first window decodes the real file");
        assert_ne!(first.request_id, 0, "the renderer gave the request its own identity");
        first
    }

    /// An edit during the first decode waits; the first result is accepted with its original.
    fn drag_accepts_first_original(
        h: &mut Headless,
        work: &Rc<RefCell<OffloadWork>>,
        id: lightcraft_catalog::PhotoId,
        first: RenderJob,
    ) -> (u64, u64) {
        // An edit changes the desired window key while the first original is still held.
        // Detail must defer a second decode until this first result has populated the cache.
        let (first_key, first_request) = (first.key, first.request_id);
        let reply = h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 0.5}}), T);
        assert_eq!(reply["ok"], true);
        h.step();
        assert!(work.borrow().jobs.is_empty(), "a changed window waits for the first original");
        assert_eq!(h.app.renderer.wanted(Slot::Region), Some(first_key));
        assert_eq!(h.app.session.memory_report().full_source.bytes, 0);

        deliver(work, Slot::Region, first.run());
        h.step();
        assert!(h.app.region_view.is_some());
        assert_eq!(h.app.renderer.textures.get(&Slot::Region).map(|t| t.key), Some(first_key), "the first window arrived");
        assert!(h.app.session.memory_report().full_source.bytes >= WIDTH as usize * HEIGHT as usize * 3 * 4);
        assert!(h.app.session.media.has_source(id, SourceLevel::Full), "the original was accepted before any supersession");
        (first_key, first_request)
    }

    fn drag_later_windows_reuse_original(h: &mut Headless, work: &Rc<RefCell<OffloadWork>>, first: (u64, u64), discarded: u64) {
        let (first_key, first_request) = first;
        let second = next_region_job(h, work);
        let (second_key, second_request) = (second.key, second.request_id);
        assert_ne!(second_key, first_key);
        assert_ne!(second_request, first_request);
        assert_eq!(h.app.renderer.wanted(Slot::Region), Some(second_key), "the newer window is pending");
        assert!(matches!(&second.source, SourceRef::Loaded(_)), "the newer window reuses the accepted original");

        let reply = h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 1.0}}), T);
        assert_eq!(reply["ok"], true);
        let third = next_region_job(h, work);
        assert_ne!(third.key, second_key);
        assert_eq!(h.app.renderer.wanted(Slot::Region), Some(third.key));

        deliver(work, Slot::Region, second.run());
        h.step();
        assert_eq!(h.app.renderer.textures.get(&Slot::Region).map(|t| t.key), Some(second_key), "the superseded draft still reached the screen");
        assert_eq!(h.app.renderer.wanted(Slot::Region), Some(third.key), "the newest request is still pending");
        assert_eq!(h.app.renderer.discarded, discarded, "the superseded draft was accepted, not discarded");
        assert!(h.app.session.memory_report().full_source.bytes >= WIDTH as usize * HEIGHT as usize * 3 * 4);
        assert!(matches!(&third.source, SourceRef::Loaded(_)), "the pending window still reuses the original");
    }

    // Given a 400 % view of a real 24 MP photo, when the first window is decoded and accepted and
    // later drag ticks supersede each other, then the original stays in the cache and every later
    // window is cut from it. The first accepted result is the actual Detail wiring's job: the
    // window `request_window` asked for (respecting `defer_window`), drafted while the drag runs.
    #[test]
    fn a_drag_keeps_the_decoded_original_after_the_window_is_accepted() {
        let (_fx, mut h, native) = detail();
        let id = h.app.session.active().expect("a photo");
        assert_eq!(native, WIDTH as usize);
        assert_eq!(h.app.session.memory_report().full_source.bytes, 0, "fit needed no original");
        h.app.session.media.set_on_screen(Some(id));
        let work = Rc::new(RefCell::new(OffloadWork::default()));
        h.app.renderer.set_offload(Box::new(RegionGate(work.clone())));
        let discarded = h.app.renderer.discarded;
        h.app.ui.zoom = crate::state::Zoom::Percent(400.0);
        h.app.session.begin_interaction("Exposure").expect("a drag");
        let first = drag_first_window(&mut h, &work, id);
        let keys = drag_accepts_first_original(&mut h, &work, id, first);
        drag_later_windows_reuse_original(&mut h, &work, keys, discarded);
    }

    // The collector's accept branch on its own: a superseded window result still carries the
    // original it decoded, and `Renderer::poll` must insert it (`session.accept(&r)` at
    // render.rs:911) although a newer request for the same slot is pending. The Detail wiring
    // above can only reach this once the first original is held; this standalone test proves the
    // cache reuse without depending on UI timing.
    #[test]
    fn a_superseded_window_result_keeps_the_original_it_decoded() {
        let fixture = RenderFixture::new("preview-limit-superseded", 1);
        let mut session = fixture.session();
        let id = session.active().expect("a photo");
        session.media.set_on_screen(Some(id));
        let (full_w, full_h) = (WIDTH as usize, HEIGHT as usize);
        let window = lightcraft_pipeline::PixelWindow { x: 1000, y: 800, w: 1300, h: 700 };
        let next_window = lightcraft_pipeline::PixelWindow { x: 1200, y: 800, w: 1300, h: 700 };
        let mut renderer = Renderer::with_worker_threads(1);
        let work = Rc::new(RefCell::new(OffloadWork::default()));
        renderer.set_offload(Box::new(Held(work.clone())));
        let ctx = egui::Context::default();

        let first = session.region_job(id, full_w, full_h, window, true).expect("the first window");
        renderer.request(Slot::Region, first, 100);
        let first = take_job(&work);
        let old_key = first.key;
        let old_request = first.request_id;

        let second = session.region_job(id, full_w, full_h, next_window, true).expect("the newer window");
        renderer.request(Slot::Region, second, 100);
        let second = take_job(&work);
        let new_key = second.key;
        assert_ne!(new_key, old_key, "the newer window has its own identity");
        assert_eq!(renderer.wanted(Slot::Region), Some(new_key));

        let old = first.run();
        assert!(old.rendered.is_ok(), "{:?}", old.rendered.as_ref().err());
        assert_eq!(old.request_id, old_request);
        assert_eq!(old.level, SourceLevel::Full);
        let loaded = old.loaded.as_ref().expect("the first window decoded the original");
        assert_eq!((loaded.image.width, loaded.image.height), (WIDTH as usize, HEIGHT as usize));
        deliver(&work, Slot::Region, old);
        renderer.poll(&ctx, &mut session);

        assert_eq!(renderer.textures.get(&Slot::Region).map(|t| t.key), Some(old_key));
        assert_eq!(renderer.wanted(Slot::Region), Some(new_key));
        assert_eq!(renderer.discarded, 0, "the superseded result was accepted, not discarded");
        assert!(session.memory_report().full_source.bytes >= full_w * full_h * 3 * 4);
        assert!(session.media.has_source(id, SourceLevel::Full));
        let reused = session.region_job(id, full_w, full_h, window, true).expect("another window");
        assert!(matches!(&reused.source, SourceRef::Loaded(_)), "the accepted original is reused");
    }

    // Given an original bigger than the cache budget (a 64 MP raw's 797 MB exceeds the default
    // share), zoomed to 1:1 it stays decoded, so a pan does not decode the raw again; back in the
    // grid it is let go
    #[test]
    fn a_zoomed_original_over_the_budget_stays_until_the_view_changes() {
        let (_fx, mut h, _) = detail();
        h.app.session.media.set_budget(16 << 20);
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        assert!(h.app.session.memory_report().full_source.bytes > 16 << 20, "the original stayed over budget");
        h.request("ui.set", json!({"view": "photoGrid"}), T);
        h.step();
        assert_eq!(h.app.session.memory_report().full_source.bytes, 0, "the grid needs no original");
    }

    /// The meshes the last frame drew with `slot`'s texture: (paint order, clip rect, bounds).
    fn drawn(h: &Headless, slot: Slot) -> Vec<(usize, egui::Rect, egui::Rect)> {
        let Some(tex) = h.app.renderer.textures.get(&slot) else { return vec![] };
        h.view
            .shapes
            .iter()
            .enumerate()
            .filter_map(|(i, c)| match &c.shape {
                egui::epaint::Shape::Mesh(m) if m.texture_id == tex.tex.id() => Some((i, c.clip_rect, m.calc_bounds())),
                _ => None,
            })
            .collect()
    }

    // Given a wipe at 1:1, the Before window is painted after (over) the low-resolution Before
    // stand-in on its side of the line, never under it, and inside its side of the line
    #[test]
    fn a_wipe_paints_the_before_window_over_its_stand_in() {
        for mode in [crate::state::BeforeAfter::Split, crate::state::BeforeAfter::SplitTopBottom] {
            let (_fx, mut h, _) = detail();
            h.app.ui.before_after = mode;
            h.request("engine.execute", json!({"command": "view.zoom100"}), T);
            h.settle(SETTLE);
            h.step();
            let (stand_in, window) = (drawn(&h, Slot::Before), drawn(&h, Slot::RegionBefore));
            assert!(!stand_in.is_empty() && !window.is_empty(), "{mode:?}: the stand-in {stand_in:?} and the window {window:?} are both painted");
            assert!(window[0].0 > stand_in[0].0, "{mode:?}: the Before window is under its stand-in");
            let mid = h.app.image_rect.unwrap().center();
            let clip = window[0].1;
            match mode {
                crate::state::BeforeAfter::Split => assert!(clip.right() <= mid.x + 0.5, "{clip:?} crosses the line at x = {}", mid.x),
                _ => assert!(clip.bottom() <= mid.y + 0.5, "{clip:?} crosses the line at y = {}", mid.y),
            }
            let after = drawn(&h, Slot::Region);
            assert!(!after.is_empty());
            match mode {
                crate::state::BeforeAfter::Split => assert!(after[0].1.left() >= mid.x - 0.5),
                _ => assert!(after[0].1.top() >= mid.y - 0.5),
            }
        }
    }

    // Given `\` held in a wipe at 1:1, the Before is shown everywhere, so its window is painted
    // everywhere (not only left of the line)
    #[test]
    fn holding_backslash_in_a_wipe_paints_the_before_window_across_the_canvas() {
        let (_fx, mut h, _) = detail();
        h.app.ui.before_after = crate::state::BeforeAfter::Split;
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        h.events.push(egui::Event::Key {
            key: egui::Key::Backslash,
            physical_key: Some(egui::Key::Backslash),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.settle(SETTLE);
        // (the key's own shortcut switches to the Before view; the wipe is chosen again with the
        // key still down, which is what holding `\` in a wipe is)
        h.app.ui.before_after = crate::state::BeforeAfter::Split;
        h.settle(SETTLE);
        h.step();
        let canvas = h.app.canvas_rect.unwrap();
        let window = drawn(&h, Slot::RegionBefore);
        assert!(!window.is_empty());
        assert!(window[0].1.right() >= canvas.right() - 0.5, "the Before window stops at the wipe line: {:?}", window[0].1);
    }

    // Given Before and After side by side at 1:1, each window is painted inside its own pane: its
    // margin (a window is wider than what its pane shows) does not spill into the neighbour
    #[test]
    fn side_by_side_windows_stay_in_their_panes() {
        let (_fx, mut h, _) = detail_in([1800.0, 900.0]);
        h.app.ui.before_after = crate::state::BeforeAfter::SideBySide;
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        h.step();
        let canvas = h.app.canvas_rect.unwrap();
        let (before, after) = (drawn(&h, Slot::RegionBefore), drawn(&h, Slot::Region));
        assert!(!before.is_empty() && !after.is_empty());
        assert!(before.iter().all(|(_, clip, _)| clip.right() <= canvas.center().x), "the Before window spills into the After pane: {before:?}");
        assert!(after.iter().all(|(_, clip, _)| clip.left() >= canvas.center().x), "the After window spills into the Before pane: {after:?}");
    }

    // Given a Before and an After with the same look (an unedited photo), both windows are kept
    // apart: the job key is the same, and one tile must not replace the other
    #[test]
    fn identical_before_and_after_keep_a_tile_each() {
        let (_fx, mut h, _) = detail();
        h.app.ui.before_after = crate::state::BeforeAfter::SideBySide;
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        assert_eq!(h.app.region_tiles.len(), 2, "{:?}", h.app.region_tiles.keys().collect::<Vec<_>>());
    }

    // Given a window the photo's spots make too big for one render (refused), the loupe falls back
    // to the whole frame at the drawn size, and tries windows again when the look changes
    #[test]
    fn a_refused_window_falls_back_to_a_full_size_render() {
        let (_fx, mut h, native) = detail();
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        assert!(h.app.region_view.is_some());
        let id = h.app.session.active().unwrap();
        let look = h.app.session.develop_of(id).unwrap().hash64();
        h.app.window_refused = Some((id, look, native));
        h.settle(SETTLE);
        assert_eq!(h.app.region_view, None);
        assert_eq!(rendered_long_edge(&h), native);
        h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 0.5}}), T);
        h.settle(SETTLE);
        assert!(h.app.region_view.is_some(), "a new look tries a window again");
    }

    // Given a pinch out from 400 %, the window there is is kept (magnified, then shrunk) until the
    // pinch stops: a new, ever wider window every frame is not asked for
    #[test]
    fn pinching_out_asks_for_no_new_windows() {
        let (_fx, mut h, _) = detail();
        h.app.ui.zoom = crate::state::Zoom::Percent(400.0);
        h.settle(SETTLE);
        let key = h.app.region_view.expect("a window").key;
        let at = h.app.canvas_rect.unwrap().center();
        h.request("ui.move", json!({"x": at.x, "y": at.y}), T);
        for _ in 0..12 {
            h.request("ui.zoom", json!({"factor": 0.9}), T);
            assert_eq!(h.app.region_view.map(|v| v.key), Some(key), "the window of before the pinch");
        }
    }

    // Given a stage budget below what the open photo's views hold (a 2800 px canvas at 1:1), a
    // slider drag still reuses them: they are not cleared every frame
    #[test]
    fn a_drag_over_the_stage_budget_does_not_clear_the_open_views() {
        let (_fx, mut h, _) = detail();
        h.app.ui.zoom = crate::state::Zoom::Percent(100.0);
        h.settle(SETTLE);
        let held = |h: &Headless| {
            let m = h.app.renderer.memory();
            (m["stageCaches"]["gpuBytes"].as_u64().unwrap() + m["stageCaches"]["cpuBytes"].as_u64().unwrap()) as usize
        };
        let before = held(&h);
        assert!(before > 3, "the views hold something ({before} bytes)");
        // a third of it is the budget: over it, but inside the runaway limit (four times)
        h.app.renderer.stage_budget_override = Some(before / 3);
        h.app.session.begin_interaction("Exposure").unwrap();
        for tick in 0..8 {
            h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": tick as f64 * 0.1}}), T);
            h.settle(SETTLE);
        }
        assert_eq!(h.app.renderer.memory()["stageCaches"]["trimmed"], 0);
        assert!(held(&h) > before / 3, "and they are still there");
    }

    /// Load a Display P3 monitor profile (`app.displayProfile`), as Settings ▸ Display does.
    fn use_display_profile(h: &mut Headless) {
        let dir = std::env::temp_dir().join(format!("lightcraft-zoom-display-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("p3.icc");
        std::fs::write(&path, lightcraft_codecs::icc::write_named(lightcraft_codecs::NamedSpace::DisplayP3)).expect("profile written");
        let r = h.request("engine.execute", json!({"command": "app.displayProfile", "params": {"path": path.to_string_lossy()}}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.app.renderer.display_id().is_some());
    }

    // Given a monitor profile, when I zoom to 1:1, then the sharp window is painted over the
    // whole-frame render, as without one (a display profile changes the texture's render key, and
    // the window used to be looked up by the key it had before: never found, so only the
    // whole-frame render, magnified and blurred, was shown)
    #[test]
    fn a_display_profile_keeps_the_sharp_window_at_one_to_one() {
        let (_fx, mut h, native) = detail();
        use_display_profile(&mut h);
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        assert!(rendered_long_edge(&h) < native, "the whole frame is smaller than the photo");
        let view = h.app.region_view.expect("a window render");
        assert_eq!(view.full.0.max(view.full.1), native);
        let (whole, window) = (drawn(&h, Slot::Main), drawn(&h, Slot::Region));
        assert!(!whole.is_empty(), "the whole-frame render is painted");
        assert!(!window.is_empty(), "the window is painted");
        assert!(window[0].0 > whole[0].0, "over the whole-frame render");
    }

    // …and in Compare, each pane's window too
    #[test]
    fn a_display_profile_keeps_the_compare_windows_at_one_to_one() {
        let (_fx, mut h, _) = detail();
        use_display_profile(&mut h);
        let r = h.request("engine.execute", json!({"command": "view.compare"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.app.ui.zoom = crate::state::Zoom::Percent(100.0);
        h.settle(SETTLE);
        for i in 0..2u8 {
            assert!(h.app.compare_windows[i as usize].is_some(), "pane {i} asked for a window");
            let (whole, window) = (drawn(&h, Slot::Compare(i)), drawn(&h, Slot::CompareRegion(i)));
            assert!(!whole.is_empty() && !window.is_empty(), "pane {i}: both are painted");
            assert!(window[0].0 > whole[0].0, "pane {i}: the window is painted over the whole-frame render");
        }
    }

    // …and with no monitor profile chosen but a screen that reports one (macOS: previews follow
    // the screen's profile, #787), the sharp window is painted too (#1365): every Mac renders for
    // a display
    #[test]
    fn with_the_screen_profile_the_window_is_drawn() {
        let (_fx, mut h, native) = detail();
        h.app.services.screen_profile = Some(Box::new(|| Some(lightcraft_codecs::icc::write_named(lightcraft_codecs::NamedSpace::DisplayP3))));
        h.step();
        assert!(h.app.ui.settings.display_profile.is_empty(), "no profile chosen");
        assert!(h.app.renderer.display_id().is_some(), "the screen's profile is in use");
        h.request("engine.execute", json!({"command": "view.zoom100"}), T);
        h.settle(SETTLE);
        assert!(rendered_long_edge(&h) < native, "the whole frame is smaller than the photo");
        let (whole, window) = (drawn(&h, Slot::Main), drawn(&h, Slot::Region));
        assert!(!whole.is_empty(), "the whole-frame render is painted");
        assert!(!window.is_empty(), "the window is painted");
        assert!(window[0].0 > whole[0].0, "over the whole-frame render");
    }

    /// The engine caches' share of the default memory budget (1.5 GiB) on a machine with 6 GiB of
    /// RAM or more: room for both panes' 24 MP originals (288 MB each as linear float).
    const COMPARE_CACHE: usize = 768 << 20;

    /// Compare view of the render fixture at `zoom`, settled.
    ///
    /// The session's cache budget is set here rather than taken from the process-wide memory
    /// budget, which other tests change while they run (Settings ▸ Performance ▸ 1 GB leaves a
    /// 512 MiB share, too small for two originals, so one pane's was evicted).
    fn compare_at(zoom: crate::state::Zoom) -> (RenderFixture, Headless) {
        let (fixture, mut h, _) = detail();
        h.app.session.media.set_budget(COMPARE_CACHE);
        let r = h.request("engine.execute", json!({"command": "view.compare"}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.ui.view, crate::state::ViewMode::Compare);
        h.app.ui.zoom = zoom;
        h.settle(SETTLE);
        (fixture, h)
    }

    fn native_of(h: &Headless, id: lightcraft_catalog::PhotoId) -> usize {
        let p = h.app.session.catalog.photo(id).expect("in the catalog");
        p.width.max(p.height) as usize
    }

    // Given two photos in Compare, when I zoom to 1:1 to check focus, then each pane shows a window
    // cut from its own photo's pixels, not the 2560 px whole-frame render magnified
    #[test]
    fn compare_at_one_to_one_gets_a_sharp_window_per_pane() {
        let (_fx, h) = compare_at(crate::state::Zoom::Percent(100.0));
        let (sel, cand) = h.app.ui.compare.map(|(a, b)| (lightcraft_catalog::PhotoId(a), lightcraft_catalog::PhotoId(b))).expect("a pair");
        for (i, id) in [(0u8, sel), (1u8, cand)] {
            let whole = h.app.renderer.textures.get(&Slot::Compare(i)).expect("the whole-frame render");
            assert!(whole.size[0].max(whole.size[1]) <= 2560, "pane {i}: the whole frame stays small");
            let w = h.app.compare_windows[i as usize].unwrap_or_else(|| panic!("pane {i} asked for a window"));
            assert_eq!(w.photo, id);
            assert_eq!(w.full.0.max(w.full.1), native_of(&h, id), "pane {i}: 1:1 is the photo's own pixels");
            let tex = h.app.renderer.textures.get(&Slot::CompareRegion(i)).unwrap_or_else(|| panic!("pane {i}: its texture"));
            assert_eq!(tex.photo, id);
            assert!(tex.size[0] >= 600 && tex.size[1] >= 600, "pane {i}: it covers the pane: {:?}", tex.size);
            assert!(h.app.session.media.has_source(id, lightcraft_engine::SourceLevel::Full), "pane {i}: both originals stay decoded");
        }
    }

    // Given Compare at fit, nothing more is rendered than before windows
    #[test]
    fn compare_at_fit_makes_no_window() {
        let (_fx, h) = compare_at(crate::state::Zoom::Fit);
        assert_eq!(h.app.compare_windows, [None, None]);
        assert!(!h.app.renderer.textures.contains_key(&Slot::CompareRegion(0)) && !h.app.renderer.textures.contains_key(&Slot::CompareRegion(1)));
    }

    // Given Compare at 1:1, when I zoom back to fit or go back to Detail, then the windows' textures
    // are freed, and back in Detail only one original is kept decoded
    #[test]
    fn leaving_a_zoomed_compare_frees_its_windows() {
        let (_fx, mut h) = compare_at(crate::state::Zoom::Percent(100.0));
        assert!(h.app.renderer.textures.contains_key(&Slot::CompareRegion(1)));
        h.app.ui.zoom = crate::state::Zoom::Fit;
        h.settle(SETTLE);
        assert!(!h.app.renderer.textures.contains_key(&Slot::CompareRegion(1)), "fit: no window");
        h.app.ui.zoom = crate::state::Zoom::Percent(100.0);
        h.settle(SETTLE);
        assert!(h.app.renderer.textures.contains_key(&Slot::CompareRegion(0)));
        let r = h.request("ui.set", json!({"view": "detail"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert_eq!(h.app.compare_windows, [None, None]);
        assert!(!h.app.renderer.textures.contains_key(&Slot::CompareRegion(0)) && !h.app.renderer.textures.contains_key(&Slot::CompareRegion(1)));
        assert!(h.app.session.media.usage().2.count <= 1, "one original in Detail");
    }

    // A pane's window is drawn where it belongs: its pixels agree with the pane's whole-frame render
    // at the same place of the frame, and it is painted over that render, inside the pane
    #[test]
    fn a_compare_window_lines_up_with_its_pane() {
        let (_fx, mut h, _) = detail();
        h.app.renderer.keep_pixels = true;
        h.request("engine.execute", json!({"command": "view.compare"}), T);
        h.app.ui.zoom = crate::state::Zoom::Percent(100.0);
        h.settle(SETTLE);
        h.step();
        for i in 0..2u8 {
            let view = h.app.compare_windows[i as usize].expect("a window");
            let tile = h.app.renderer.textures.get(&Slot::CompareRegion(i)).and_then(|t| t.pixels.clone()).expect("tile pixels");
            let main = h.app.renderer.textures.get(&Slot::Compare(i)).and_then(|t| t.pixels.clone()).expect("pane pixels");
            let mean_difference = |offset: usize| {
                let (mut sum, mut n) = (0.0f32, 0.0f32);
                for j in (8..tile.size[1] - 8).step_by(37) {
                    for x in (8..tile.size[0] - 8).step_by(37) {
                        let u = (view.window.x + x + offset) as f32 / view.full.0 as f32;
                        let v = (view.window.y + j) as f32 / view.full.1 as f32;
                        let (mx, my) =
                            (((u * main.size[0] as f32) as usize).min(main.size[0] - 1), ((v * main.size[1] as f32) as usize).min(main.size[1] - 1));
                        let (a, b) = (tile.pixels[j * tile.size[0] + x], main.pixels[my * main.size[0] + mx]);
                        sum += (0..3).map(|k| (a.to_array()[k] as f32 - b.to_array()[k] as f32).abs()).sum::<f32>() / 3.0;
                        n += 1.0;
                    }
                }
                sum / n
            };
            let difference = mean_difference(0);
            assert!(difference < 6.0, "pane {i}: mean difference {:.2} / 255 between the window and the pane's render", difference);
            assert!(mean_difference(16) > 6.0, "the fixture must expose a 16-pixel horizontal misalignment");
            let (whole, window) = (drawn(&h, Slot::Compare(i)), drawn(&h, Slot::CompareRegion(i)));
            assert!(!whole.is_empty() && !window.is_empty(), "pane {i}: both are painted");
            assert!(window[0].0 > whole[0].0, "pane {i}: the window is painted over the whole-frame render");
            assert!(window[0].1.width() < h.size.x * 0.6, "pane {i}: clipped to its pane, {:?}", window[0].1);
        }
        // the two panes' clips don't overlap
        let (a, b) = (drawn(&h, Slot::CompareRegion(0))[0].1, drawn(&h, Slot::CompareRegion(1))[0].1);
        assert!(a.right() <= b.left() + 0.5, "{a:?} / {b:?}");
    }

    // Given Reference view at 1:1, the reference and the active photo both get windows; a slider
    // drag on the active photo asks for no new window until it ends
    #[test]
    fn reference_view_at_one_to_one_gets_windows_and_a_drag_keeps_them() {
        let (_fx, mut h, _) = detail();
        let r = h.request("engine.execute", json!({"command": "photo.setReference"}), T);
        assert_eq!(r["ok"], true, "{r}");
        let r = h.request("ui.key", json!({"key": "r", "shift": true}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.ui.view, crate::state::ViewMode::Reference);
        h.app.ui.zoom = crate::state::Zoom::Percent(100.0);
        h.settle(SETTLE);
        let before = h.app.compare_windows[1].expect("the active photo's window");
        assert!(h.app.compare_windows[0].is_some(), "the reference's window");
        h.app.session.begin_interaction("Exposure").unwrap();
        h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 0.8}}), T);
        for _ in 0..5 {
            h.step();
        }
        assert_eq!(h.app.compare_windows[1], Some(before), "no window per drag tick");
        assert!(!h.app.renderer.is_pending(Slot::CompareRegion(1)));
        h.app.session.end_interaction().unwrap();
        h.settle(SETTLE);
        let after = h.app.compare_windows[1].expect("a window for the new look");
        assert_ne!(after.settings, before.settings);
    }
}
