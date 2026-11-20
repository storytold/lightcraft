//! Region rendering (issue #323): a window of the virtual full-size output is rendered on its own
//! and must match the same pixels of the full render, so zoomed views can be sharp at any zoom
//! without materializing the whole frame.

use lightcraft_develop::{DevelopSettings, controls};
use lightcraft_geom::{Orientation, Rect};
use lightcraft_raster::{Rgb32f, Rgba8};

use crate::{OutputDepth, PixelWindow, RenderRequest, SourceInfo, render};

const W: usize = 960;
const H: usize = 640;
/// Context rendered around the pixels a caller keeps (spatial filters read their neighbours).
const HALO: usize = 64;

fn scene() -> Rgb32f {
    lightcraft_scenes::demo_library()[0].render(480, 320)
}

fn full(s: &DevelopSettings) -> Rgba8 {
    render(&scene(), &SourceInfo::default(), s, &RenderRequest::fit(W, H)).image
}

fn window(s: &DevelopSettings, win: PixelWindow) -> Rgba8 {
    let req = RenderRequest { window: Some(win), ..RenderRequest::fit(W, H) };
    render(&scene(), &SourceInfo::default(), s, &req).image
}

/// Largest per-channel difference and mean absolute difference between `win` (rendered for
/// `at`, kept `HALO` px in from its edges) and the same pixels of `full`.
fn compare(full: &Rgba8, win: &Rgba8, at: PixelWindow) -> (u8, f64) {
    compare_inset(full, win, at, HALO)
}

/// [`compare`] keeping `inset` px in from the window's edges.
fn compare_inset(full: &Rgba8, win: &Rgba8, at: PixelWindow, inset: usize) -> (u8, f64) {
    let (mut max, mut sum, mut n) = (0u8, 0f64, 0f64);
    for y in inset..at.h - inset {
        for x in inset..at.w - inset {
            let (a, b) = (full.get(at.x + x, at.y + y), win.get(x, y));
            for k in 0..3 {
                let d = a[k].abs_diff(b[k]);
                max = max.max(d);
                sum += d as f64;
                n += 1.0;
            }
        }
    }
    (max, sum / n)
}

fn check(name: &str, s: &DevelopSettings, win: PixelWindow, max_ok: u8, mean_ok: f64) {
    let (f, w) = (full(s), window(s, win));
    assert_eq!((w.width, w.height), (win.w, win.h), "{name}: the window's own size");
    let (max, mean) = compare(&f, &w, win);
    assert!(max <= max_ok && mean <= mean_ok, "{name}: max {max} (≤ {max_ok}), mean {mean:.3} (≤ {mean_ok})");
}

const MID: PixelWindow = PixelWindow { x: 300, y: 180, w: 400, h: 300 };

// Given the window is the whole output, then it is the full render, byte for byte
#[test]
fn the_whole_frame_as_a_window_is_the_full_render() {
    let s = DevelopSettings::default();
    let f = full(&s);
    let w = window(&s, PixelWindow { x: 0, y: 0, w: f.width, h: f.height });
    assert_eq!(f.data, w.data);
}

// Given default settings, a window in the middle matches the full render
#[test]
fn a_window_matches_the_full_render() {
    check("default", &DevelopSettings::default(), MID, 1, 0.1);
}

// Given a window at each corner, it matches too (edges of the frame are not special)
#[test]
fn windows_at_the_corners_match() {
    let s = DevelopSettings::default();
    for (x, y) in [(0, 0), (W - 400, 0), (0, H - 300), (W - 400, H - 300)] {
        check("corner", &s, PixelWindow { x, y, w: 400, h: 300 }, 1, 0.1);
    }
}

// Given crop, straighten and flip, the window still lands on the same pixels
#[test]
fn a_cropped_straightened_flipped_view_matches() {
    let mut s = DevelopSettings::default();
    s.crop.geometry.rect = Rect { x0: 0.1, y0: 0.15, x1: 0.85, y1: 0.9 };
    s.crop.geometry.angle = 4.0;
    s.crop.flip_h = true;
    check("crop+angle+flipH", &s, MID, 3, 0.3);
    s.crop.flip_h = false;
    s.crop.flip_v = true;
    check("crop+angle+flipV", &s, MID, 3, 0.3);
}

// Given a rotated photo, the same
#[test]
fn rotated_and_mirrored_orientations_match() {
    for o in [Orientation::Rotate90, Orientation::Rotate270, Orientation::FlipH, Orientation::Transpose] {
        let s = DevelopSettings { orientation: o, ..Default::default() };
        let f = full(&s);
        let win = PixelWindow { x: f.width / 4, y: f.height / 4, w: f.width / 2, h: f.height / 2 };
        check(&format!("{o:?}"), &s, win, 3, 0.3);
    }
}

// Given a post-crop vignette, the window shows the vignette of the whole frame, not its own
#[test]
fn the_vignette_belongs_to_the_frame_not_the_window() {
    let mut s = DevelopSettings::default();
    controls::set(&mut s, "vignette.amount", -90.0);
    controls::set(&mut s, "vignette.midpoint", 10.0);
    // the vignette is really there: the corner is much darker than without it
    let (with, without) = (full(&s), full(&DevelopSettings::default()));
    let corner = |i: &Rgba8| i.get(W - 20, H - 20)[1] as i32;
    assert!(corner(&without) - corner(&with) > 30, "{} vs {}", corner(&without), corner(&with));
    // …and a window over that corner shows it, at the same place
    check("vignette", &s, PixelWindow { x: W - 400, y: H - 300, w: 400, h: 300 }, 2, 0.2);
    check("vignette middle", &s, MID, 2, 0.2);
}

// Given spatial adjustments, the window agrees away from its halo
#[test]
fn spatial_adjustments_agree_inside_the_halo() {
    for (id, v) in
        [("effects.clarity", 70.0), ("effects.dehaze", 40.0), ("detail.sharpenAmount", 80.0), ("light.shadows", 60.0), ("effects.texture", 60.0)]
    {
        let mut s = DevelopSettings::default();
        controls::set(&mut s, id, v);
        check(id, &s, MID, 6, 0.8);
    }
}

// Given a huge virtual frame (48000 px: an 800 % zoom), only the window is rendered
#[test]
fn a_window_of_a_huge_frame_is_cheap() {
    let req = RenderRequest { window: Some(PixelWindow { x: 20_000, y: 12_000, w: 256, h: 192 }), ..RenderRequest::fit(48_000, 32_000) };
    let r = render(&scene(), &SourceInfo::default(), &DevelopSettings::default(), &req);
    assert_eq!((r.image.width, r.image.height), (256, 192));
}

// Hostile windows (from UI state) are clamped into the frame, never a panic or a huge buffer
#[test]
fn windows_outside_or_degenerate_are_clamped() {
    let s = DevelopSettings::default();
    for win in [
        PixelWindow { x: W + 500, y: 5, w: 100, h: 100 },
        PixelWindow { x: 0, y: 0, w: 0, h: 0 },
        PixelWindow { x: usize::MAX, y: usize::MAX, w: usize::MAX, h: usize::MAX },
        PixelWindow { x: 900, y: 600, w: 5000, h: 5000 },
    ] {
        let r = window(&s, win);
        assert!(r.width >= 1 && r.height >= 1 && r.width <= W && r.height <= H, "{win:?}: {}×{}", r.width, r.height);
    }
}

/// A model's segmentation on a `side²` grid: an ellipse in the middle of the photo.
fn middle_seg(side: usize) -> lightcraft_develop::SegMask {
    let logits: Vec<f32> = (0..side * side)
        .map(|i| {
            let (x, y) = (((i % side) as f32 + 0.5) / side as f32, ((i / side) as f32 + 0.5) / side as f32);
            let r = (((x - 0.5) / 0.3).powi(2) + ((y - 0.5) / 0.25).powi(2)).sqrt();
            (12.0 * (1.0 - r) * 4.0).clamp(-12.0, 12.0)
        })
        .collect();
    lightcraft_develop::SegMask::from_logits(side, &logits)
}

/// A textured, noisy source: what noise reduction and edge refinement act on.
fn noisy_scene() -> Rgb32f {
    let base = scene();
    Rgb32f::from_fn(base.width, base.height, |x, y| {
        let mut v = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841);
        v ^= v >> 13;
        v = v.wrapping_mul(0x5bd1_e995);
        v ^= v >> 15;
        let n = 1.0 + 0.25 * ((v & 0xffff) as f32 / 32768.0 - 1.0);
        base.data[y * base.width + x].map(|c| c * n)
    })
}

fn check_noisy(name: &str, s: &DevelopSettings, win: PixelWindow, max_ok: u8, mean_ok: f64) {
    let src = noisy_scene();
    let f = render(&src, &SourceInfo::default(), s, &RenderRequest::fit(W, H)).image;
    let w = render(&src, &SourceInfo::default(), s, &RenderRequest { window: Some(win), ..RenderRequest::fit(W, H) }).image;
    let (max, mean) = compare(&f, &w, win);
    assert!(max <= max_ok && mean <= mean_ok, "{name}: max {max} (≤ {max_ok}), mean {mean:.3} (≤ {mean_ok})");
}

// Given noise reduction, its strength follows the whole frame's size, not the window's
#[test]
fn noise_reduction_does_not_depend_on_the_window() {
    for (id, v) in [("detail.nrLuminance", 80.0), ("detail.nrColor", 80.0)] {
        let mut s = DevelopSettings::default();
        controls::set(&mut s, id, v);
        check_noisy(id, &s, MID, 3, 0.15);
    }
}

// Given a mask with Refine Edges, the refinement width follows the whole frame's size
#[test]
fn mask_edge_refinement_does_not_depend_on_the_window() {
    use lightcraft_develop::{LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
    use lightcraft_geom::Point;
    let shape = MaskShape::Linear { start: Point::new(0.45, 0.5), end: Point::new(0.55, 0.5) };
    let mut s = DevelopSettings::default();
    s.masks.push(Mask {
        id: 1,
        components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }],
        adjust: LocalAdjustments { exposure: 1.5, ..Default::default() },
        refine: 100.0,
        ..Default::default()
    });
    check_noisy("refine", &s, MID, 3, 0.2);
}

// Given a mask overlay (the Masking panel's view of the selected mask), a window of it matches the
// same pixels of the whole render with that overlay, for every view and for drawn, AI and
// refined shapes: the zoomed loupe can show the overlay as a window (issue #547)
#[test]
fn a_mask_overlay_window_matches_the_whole_render() {
    use crate::{MaskView, Overlay};
    use lightcraft_develop::{LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
    use lightcraft_geom::Point;
    let shapes = [
        ("radial", MaskShape::Radial { center: Point::new(0.5, 0.5), rx: 0.2, ry: 0.15, angle: 20.0, feather: 50.0, invert: false }, 0.0),
        ("linear refined", MaskShape::Linear { start: Point::new(0.45, 0.5), end: Point::new(0.55, 0.5) }, 100.0),
        ("sky", MaskShape::Sky, 0.0),
        // a model's grid, enlarged along the photo's edges (issue #3272)
        ("prompt", MaskShape::Prompt { text: "middle".into(), seg: Some(middle_seg(32)), detail: vec![], edge: 0.0 }, 0.0),
    ];
    for (name, shape, refine) in shapes {
        let mut s = DevelopSettings::default();
        s.masks.push(Mask {
            id: 3,
            components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }],
            adjust: LocalAdjustments { exposure: 1.0, ..Default::default() },
            refine,
            ..Default::default()
        });
        for view in [MaskView::Color, MaskView::WhiteOnBlack, MaskView::ImageOnBlack] {
            let overlay = Overlay::Mask { id: 3, view, color: [255, 0, 0], opacity: 70 };
            let f = render(&noisy_scene(), &SourceInfo::default(), &s, &RenderRequest { overlay, ..RenderRequest::fit(W, H) }).image;
            let w =
                render(&noisy_scene(), &SourceInfo::default(), &s, &RenderRequest { overlay, window: Some(MID), ..RenderRequest::fit(W, H) }).image;
            let (max, mean) = compare(&f, &w, MID);
            // (refine edges differs by a rounding step, as in `mask_edge_refinement_does_not_depend_on_the_window`)
            assert!(max <= 3 && mean <= 0.25, "{name} {view:?}: max {max} (≤ 3), mean {mean:.3} (≤ 0.25)");
        }
    }
}

// Given the Show All Masks view (issue #1712), a window of it matches the same pixels of the
// whole render with that overlay: the union is composed from the same mask alphas, so the zoomed
// loupe can show it as a window like a single mask's overlay (issue #547)
#[test]
fn an_all_masks_overlay_window_matches_the_whole_render() {
    use crate::{MaskView, Overlay};
    use lightcraft_develop::{LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
    use lightcraft_geom::Point;
    let shapes = [
        (3, MaskShape::Radial { center: Point::new(0.45, 0.5), rx: 0.2, ry: 0.15, angle: 20.0, feather: 50.0, invert: false }, 0.0),
        (4, MaskShape::Linear { start: Point::new(0.4, 0.55), end: Point::new(0.6, 0.5) }, 100.0),
    ];
    let mut s = DevelopSettings::default();
    for (id, shape, refine) in shapes {
        s.masks.push(Mask {
            id,
            components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }],
            adjust: LocalAdjustments { exposure: 1.0, ..Default::default() },
            refine,
            ..Default::default()
        });
    }
    for view in [MaskView::Color, MaskView::WhiteOnBlack, MaskView::ImageOnBlack] {
        let overlay = Overlay::AllMasks { view, color: [255, 0, 0], opacity: 70 };
        let f = render(&noisy_scene(), &SourceInfo::default(), &s, &RenderRequest { overlay, ..RenderRequest::fit(W, H) }).image;
        let w = render(&noisy_scene(), &SourceInfo::default(), &s, &RenderRequest { overlay, window: Some(MID), ..RenderRequest::fit(W, H) }).image;
        let (max, mean) = compare(&f, &w, MID);
        // (refine edges differs by a rounding step, as in `mask_edge_refinement_does_not_depend_on_the_window`)
        assert!(max <= 3 && mean <= 0.25, "{view:?}: max {max} (≤ 3), mean {mean:.3} (≤ 0.25)");
    }
}

// ---- Deep zoom: the wide stages' kernels are far wider than any margin -------------------------

/// Hard-edged blocks of very different brightness at several scales: the wide stages' edge-aware
/// filters and the dark channel have something to be wrong about.
fn blocks() -> Rgb32f {
    Rgb32f::from_fn(600, 400, |x, y| {
        let cell = |size: usize, k: usize| ((x / size * 7 + y / size * 13 + k) % 5) as f32 / 4.0;
        let l = 0.02 + 0.5 * cell(97, 1) + 0.25 * cell(31, 2) + 0.1 * cell(11, 3);
        [l * 1.1, l, l * 0.8]
    })
}

const BIG_W: usize = 7680;
const BIG_H: usize = 5120;

/// A window of a 7680 × 5120 frame against the same pixels of the whole render. The wide stages
/// (dehaze, clarity, highlights/shadows base) have XX/// can't contain them, so they come from the whole frame at reduced size.
fn check_big(name: &str, s: &DevelopSettings, max_ok: u8, mean_ok: f64) {
    let src = blocks();
    let f = render(&src, &SourceInfo::default(), s, &RenderRequest::fit(BIG_W, BIG_H)).image;
    let win = PixelWindow { x: 3000, y: 1800, w: 512, h: 384 };
    let w = render(&src, &SourceInfo::default(), s, &RenderRequest { window: Some(win), ..RenderRequest::fit(BIG_W, BIG_H) }).image;
    let (max, mean) = compare(&f, &w, win);
    assert!(max <= max_ok && mean <= mean_ok, "{name}: max {max} (≤ {max_ok}), mean {mean:.3} (≤ {mean_ok})");
}

#[test]
fn dehaze_at_depth_matches_the_whole_render() {
    let mut s = DevelopSettings::default();
    controls::set(&mut s, "effects.dehaze", 60.0);
    check_big("dehaze", &s, 6, 0.6);
}

#[test]
fn clarity_at_depth_matches_the_whole_render() {
    let mut s = DevelopSettings::default();
    controls::set(&mut s, "effects.clarity", 80.0);
    check_big("clarity", &s, 6, 0.6);
}

#[test]
fn highlights_and_shadows_at_depth_match_the_whole_render() {
    let mut s = DevelopSettings::default();
    controls::set(&mut s, "light.shadows", 80.0);
    controls::set(&mut s, "light.highlights", -80.0);
    check_big("shadows+highlights", &s, 6, 0.6);
}

// ---- Spot removal reads outside the spot -------------------------------------------------------

/// Like [`check`], over every pixel of the window (a spot at its edge must be right too).
fn check_whole(name: &str, s: &DevelopSettings, win: PixelWindow, max_ok: u8, mean_ok: f64) {
    let (f, w) = (full(s), window(s, win));
    let (max, mean) = compare_inset(&f, &w, win, 0);
    assert_eq!((w.width, w.height), (win.w, win.h), "{name}: the window's own size");
    assert!(max <= max_ok && mean <= mean_ok, "{name}: max {max} (≤ {max_ok}), mean {mean:.3} (≤ {mean_ok})");
}

fn with_spot(mode: lightcraft_develop::SpotMode, at: (f64, f64), offset: Option<(f64, f64)>) -> DevelopSettings {
    use lightcraft_develop::Spot;
    use lightcraft_geom::Point;
    let mut s = DevelopSettings::default();
    s.spots.push(Spot {
        mode,
        points: vec![Point::new(at.0, at.1)],
        size: 0.04,
        feather: 30.0,
        opacity: 100.0,
        source_offset: offset.map(|(x, y)| Point::new(x, y)),
    });
    s
}

// Given a spot whose source patch lies outside the window, the window still shows it healed
#[test]
fn a_spot_with_its_source_outside_the_window_matches_the_whole_render() {
    use lightcraft_develop::SpotMode;
    // the target is at x ≈ 0.36 of the frame; its source is 0.12 of the width to the right
    for mode in [SpotMode::Clone, SpotMode::Heal] {
        let s = with_spot(mode, (0.40, 0.5), Some((0.12, 0.05)));
        // a window that holds the target (x ≈ 384) but not the source (x ≈ 499..)
        check_whole(&format!("{mode:?}"), &s, PixelWindow { x: 250, y: 200, w: 200, h: 200 }, 1, 0.1);
    }
}

// Given a spot just outside the window whose feathered edge reaches in, the window shows it
#[test]
fn a_spot_overlapping_the_window_edge_matches_the_whole_render() {
    let s = with_spot(lightcraft_develop::SpotMode::Heal, (0.30, 0.5), Some((-0.1, 0.0)));
    check_whole("edge", &s, PixelWindow { x: 300, y: 200, w: 240, h: 240 }, 1, 0.1);
}

// Given a spot with an automatic source, two windows over the same spot choose the same source
#[test]
fn an_automatic_spot_source_does_not_depend_on_the_window() {
    let s = with_spot(lightcraft_develop::SpotMode::Heal, (0.5, 0.5), None);
    // windows so tight that the candidate sources can't all be inside them
    let (a, b) = (PixelWindow { x: 430, y: 280, w: 110, h: 90 }, PixelWindow { x: 450, y: 290, w: 110, h: 90 });
    let (ia, ib) = (window(&s, a), window(&s, b));
    let (mut max, mut n) = (0u8, 0);
    for y in 0..80 {
        for x in 0..90 {
            let (p, q) = (ia.get(x + 20, y + 10), ib.get(x, y));
            for k in 0..3 {
                max = max.max(p[k].abs_diff(q[k]));
                n += 1;
            }
        }
    }
    assert!(n > 0 && max <= 2, "overlapping windows disagree by up to {max}");
}

// Given Auto Mask strokes that run in and out of the window, the window shows the whole render's mask
#[test]
fn auto_mask_strokes_match_the_whole_render() {
    use lightcraft_develop::{BrushStroke, LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
    use lightcraft_geom::Point;
    let st = |pts: &[(f64, f64)], auto_mask, erase| BrushStroke {
        points: pts.iter().map(|p| Point::new(p.0, p.1)).collect(),
        size: 0.06,
        feather: 40.0,
        flow: 80.0,
        auto_mask,
        erase,
        ..Default::default()
    };
    let strokes = vec![st(&[(0.2, 0.55), (0.5, 0.6), (0.8, 0.5)], true, false), st(&[(0.5, 0.58)], true, true)];
    let s = DevelopSettings {
        masks: vec![Mask {
            components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape: MaskShape::Brush { strokes } }],
            adjust: LocalAdjustments { exposure: 1.0, saturation: -50.0, ..Default::default() },
            ..Default::default()
        }],
        ..Default::default()
    };
    check_noisy("auto mask", &s, PixelWindow { x: 400, y: 300, w: 300, h: 250 }, 2, 0.1);
}

// Given a lens blur, a window (zoomed view) blurs right up to its edges as the whole render does:
// it is rendered with the blur's reach around it
#[test]
fn lens_blur_matches_the_whole_render_up_to_the_window_edges() {
    lens_blur_window_matches(OutputDepth::U8);
    lens_blur_window_matches(OutputDepth::U16);
}

/// A high-bit-depth window grows for the lens blur too, and is cut back to the request.
fn lens_blur_window_matches(depth: OutputDepth) {
    use lightcraft_develop::{Mask, MaskComponent, MaskOp, MaskShape};
    use lightcraft_geom::Point;
    let radial = MaskShape::Radial { center: Point::new(0.5, 0.5), rx: 0.15, ry: 0.2, angle: 0.0, feather: 30.0, invert: false };
    let mut s = DevelopSettings {
        masks: vec![Mask {
            id: 4,
            components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape: radial }],
            ..Default::default()
        }],
        ..Default::default()
    };
    s.lens_blur.enabled = true;
    s.lens_blur.amount = 80.0;
    s.lens_blur.boost = 40.0;
    s.lens_blur.focus_mask = Some(4);
    let deep = |win: Option<PixelWindow>| {
        let r = render(&scene(), &SourceInfo::default(), &s, &RenderRequest { window: win, depth, ..RenderRequest::fit(W, H) });
        if let Some(d) = &r.deep {
            assert_eq!((d.width, d.height), (r.image.width, r.image.height));
        }
        r.image
    };
    let (f, w) = (deep(None), deep(Some(MID)));
    assert_eq!((w.width, w.height), (MID.w, MID.h));
    let (max, mean) = compare_inset(&f, &w, MID, 0);
    assert!(max <= 6 && mean <= 0.4, "max {max}, mean {mean:.3}");
    // and the blur shows: the whole render differs from the one without it
    let mut off = s.clone();
    off.lens_blur.enabled = false;
    let (_, mean) = compare_inset(&full(&off), &f, PixelWindow { x: 0, y: 0, w: f.width, h: f.height }, 0);
    assert!(mean > 1.0, "{mean:.3}");
}

/// With the automatic subject focus (no focus mask, no fixed shape, no person matte) a window
/// focuses where the whole render does: the estimate comes from the whole photo, not from the
/// pixels the window holds.
#[test]
fn lens_blur_on_the_estimated_subject_matches_the_whole_render() {
    let mut s = DevelopSettings::default();
    s.lens_blur.enabled = true;
    s.lens_blur.amount = 80.0;
    // a sharp cut: the window then grows only by the blur's radius, less than the estimate reads
    s.lens_blur.transition = 0.0;
    for depth in [OutputDepth::U8, OutputDepth::U16] {
        let r = |win: Option<PixelWindow>| {
            render(&scene(), &SourceInfo::default(), &s, &RenderRequest { window: win, depth, ..RenderRequest::fit(W, H) }).image
        };
        // a window at the middle and one at an edge, where the estimate's neighbourhood is cut off
        for win in [MID, PixelWindow { x: 0, y: 0, w: 300, h: 220 }] {
            let (f, w) = (r(None), r(Some(win)));
            assert_eq!((w.width, w.height), (win.w, win.h));
            let (max, mean) = compare_inset(&f, &w, win, 0);
            assert!(max <= 6 && mean <= 0.4, "{depth:?} {win:?}: max {max}, mean {mean:.3}");
        }
    }
}

// Given the joint colour NR, a window is filtered with the whole frame's noise model: a quiet part of
// the frame does not make a window of its noisy part (or the other way round) smooth differently
#[test]
fn joint_colour_nr_measures_the_noise_of_the_whole_frame() {
    let mut s = DevelopSettings::default();
    s.detail.nr_color = 25.0;
    s.detail.nr_color_smoothness = 0.0;
    // a frame that is one noise tile, and one of many tiles (`nrc_joint::tiles`)
    for (w, h, win) in [(768, 128, PixelWindow { x: 640, y: 32, w: 64, h: 64 }), (1664, 1536, PixelWindow { x: 1400, y: 700, w: 128, h: 128 })] {
        let src = crate::tests::colour_noise_scene(w, h, w * 2 / 3);
        let req = RenderRequest::fit(w, h);
        let f = render(&src, &SourceInfo::default(), &s, &req).image;
        let part = render(&src, &SourceInfo::default(), &s, &RenderRequest { window: Some(win), ..req }).image;
        let (max, mean) = compare_inset(&f, &part, win, win.w / 4);
        assert!(max <= 2 && mean <= 0.1, "{w}×{h}: max {max}, mean {mean:.3}");
    }
}

// Given a 16-bit or float render of a window with the joint colour NR, then its edges are the whole
// render's too: the window is grown by the filter's reach and cut back
#[test]
fn a_deep_window_with_colour_noise_reduction_matches_the_whole_render() {
    use crate::OutputDepth;
    let (w, h, win) = (768, 128, PixelWindow { x: 600, y: 32, w: 96, h: 64 });
    let src = crate::tests::colour_noise_scene(w, h, w * 2 / 3);
    let mut s = DevelopSettings::default();
    s.detail.nr_color = 25.0;
    s.detail.nr_color_smoothness = 0.0;
    for depth in [OutputDepth::U16, OutputDepth::F32Linear] {
        let req = RenderRequest { depth, ..RenderRequest::fit(w, h) };
        let deep = |req: &RenderRequest| render(&src, &SourceInfo::default(), &s, req).deep.map(|d| d.to_rgba8());
        let (Some(f), Some(part)) = (deep(&req), deep(&RenderRequest { window: Some(win), ..req })) else {
            panic!("{depth:?}: no deep image");
        };
        assert_eq!((part.width, part.height), (win.w, win.h), "{depth:?}: the window's own size");
        let (max, mean) = compare_inset(&f, &part, win, 0);
        assert!(max <= 1 && mean <= 0.01, "{depth:?}: max {max}, mean {mean:.3}");
    }
}

/// The margin the loupe's windows carry around what they show at this size (`region::margin_for`:
/// 4.5 % of the long edge).
const OVERLAY_INSET: usize = W * 45 / 1000;

/// [`check`] for a diagnostic overlay, with the photo as a raw (Visualize HDR needs one), keeping the
/// loupe's margin in from the window's edges.
fn check_overlay(name: &str, s: &DevelopSettings, overlay: crate::Overlay, win: PixelWindow, max_ok: u8, mean_ok: f64) {
    let info = SourceInfo { raw: true, ..Default::default() };
    let full_req = RenderRequest { overlay, ..RenderRequest::fit(W, H) };
    let f = render(&scene(), &info, s, &full_req).image;
    let w = render(&scene(), &info, s, &RenderRequest { window: Some(win), ..full_req }).image;
    assert_eq!((w.width, w.height), (win.w, win.h), "{name}: the window's own size");
    assert_ne!(f.data, render(&scene(), &info, s, &RenderRequest::fit(W, H)).image.data, "{name}: the overlay draws something");
    let (max, mean) = compare_inset(&f, &w, win, OVERLAY_INSET);
    assert!(max <= max_ok && mean <= mean_ok, "{name}: max {max} (≤ {max_ok}), mean {mean:.3} (≤ {mean_ok})");
}

// Given a diagnostic overlay (Visualize Spots, a Point Color range, Visualize HDR), a window shows
// the same pixels as the whole-frame render, so the loupe can draw them as a zoom window
#[test]
fn diagnostic_overlays_in_a_window_match_the_full_render() {
    use crate::Overlay;
    let mut spots = DevelopSettings::default();
    spots.detail.sharpen_amount = 60.0;
    // a binary threshold: a pixel at the threshold may flip on a one-level difference underneath
    check_overlay("spots", &spots, Overlay::Spots(70), MID, 255, 0.05);
    for (x, y) in [(0, 0), (W - 400, H - 300)] {
        check_overlay("spots at a corner", &spots, Overlay::Spots(70), PixelWindow { x, y, w: 400, h: 300 }, 255, 0.05);
    }
    let mut pc = DevelopSettings::default();
    pc.point_colors.push(lightcraft_develop::PointColor { lum: 0.6, chroma: 0.08, hue: 60.0, hue_shift: 40.0, ..Default::default() });
    check_overlay("point colour range", &pc, Overlay::PointColorRange(0), MID, 1, 0.1);
    let mut hdr = DevelopSettings::default();
    hdr.hdr.enabled = true;
    hdr.hdr.max_ev = 3.0;
    hdr.light.exposure = 1.5;
    check_overlay("hdr range", &hdr, Overlay::HdrRange, MID, 1, 0.1);
}

/// A flat grey photo: every difference between two renders of it is the vignette's.
fn flat() -> Rgb32f {
    Rgb32f { width: 480, height: 320, data: vec![[0.18; 3]; 480 * 320] }
}

/// A strong post-crop vignette on an off-centre, straightened crop.
fn crop_tool_vignette() -> DevelopSettings {
    let mut s = DevelopSettings::default();
    controls::set(&mut s, "vignette.amount", -80.0);
    controls::set(&mut s, "vignette.midpoint", 20.0);
    s.crop.geometry.rect = Rect { x0: 0.12, y0: 0.15, x1: 0.6, y1: 0.75 };
    s.crop.geometry.angle = 5.0;
    s
}

/// The crop tool's render: the whole photo, uncropped.
fn uncropped(s: &DevelopSettings, window: Option<PixelWindow>) -> Rgba8 {
    let req = RenderRequest { apply_crop: false, window, ..RenderRequest::fit(W, H) };
    render(&flat(), &SourceInfo::default(), s, &req).image
}

/// Largest difference between the cropped render and `shown`, the crop tool's uncropped render
/// (or the window of it at `at`), over points of the crop that land on the photo in both.
fn crop_tool_vs_cropped(s: &DevelopSettings, shown: &Rgba8, at: PixelWindow) -> u8 {
    let src = flat();
    let cropped = render(&src, &SourceInfo::default(), s, &RenderRequest::fit(W, H)).image;
    let (cw, ch) = (cropped.width, cropped.height);
    let (full_w, full_h) = crate::geometry::Frame::new(src.width, src.height, s, false).fit(W, H);
    let to_norm = crate::geometry::Frame::new(src.width, src.height, s, true).out_to_norm(cw, ch);
    let to_uncropped = crate::geometry::Frame::new(src.width, src.height, s, false).norm_to_out(full_w, full_h);
    let mut max = 0u8;
    let mut n = 0;
    for j in 1..16 {
        for i in 1..16 {
            let (x, y) = (cw * i / 16, ch * j / 16);
            let norm = to_norm.apply(lightcraft_geom::Point::new(x as f64 + 0.5, y as f64 + 0.5));
            if !(0.02..0.98).contains(&norm.x) || !(0.02..0.98).contains(&norm.y) {
                continue;
            }
            let q = to_uncropped.apply(norm);
            let (ux, uy) = (q.x.floor() as isize - at.x as isize, q.y.floor() as isize - at.y as isize);
            if ux < 0 || uy < 0 || ux as usize >= shown.width || uy as usize >= shown.height {
                continue;
            }
            let (a, b) = (cropped.get(x, y), shown.get(ux as usize, uy as usize));
            max = max.max((0..3).map(|k| a[k].abs_diff(b[k])).max().unwrap_or(0));
            n += 1;
        }
    }
    assert!(n >= 30, "only {n} points compared");
    max
}

// Issue #3484: given a post-crop vignette, the crop tool's uncropped view shows it around the crop
// (where the edit's export has it), not around the whole photo
#[test]
fn the_crop_tools_uncropped_view_anchors_the_vignette_to_the_crop() {
    let mut s = crop_tool_vignette();
    let whole = |img: &Rgba8| PixelWindow { x: 0, y: 0, w: img.width, h: img.height };
    for (name, flip_h, flip_v) in [("crop", false, false), ("crop + flip H", true, false), ("crop + flip V", false, true)] {
        s.crop.flip_h = flip_h;
        s.crop.flip_v = flip_v;
        let shown = uncropped(&s, None);
        // the vignette is there: the photo's far corner, outside the crop, is darker than the crop's middle
        let bright = shown.get(shown.width / 2, shown.height / 2)[1].max(shown.get(shown.width * 36 / 100, shown.height * 45 / 100)[1]);
        let (fx, fy) = (if flip_h { 4 } else { shown.width - 5 }, if flip_v { 4 } else { shown.height - 5 });
        assert!(bright as i32 - shown.get(fx, fy)[1] as i32 > 40, "{name}: no vignette outside the crop");
        let max = crop_tool_vs_cropped(&s, &shown, whole(&shown));
        assert!(max <= 2, "{name}: the crop tool's vignette differs from the export's by {max} levels");
    }
}

// Given Constrain Crop under a keystone correction, the crop tool's vignette frames the crop the
// export uses: the one Constrain Crop shrank, not the stored one
#[test]
fn the_crop_tools_vignette_frames_the_constrained_crop() {
    let mut s = crop_tool_vignette();
    s.crop.geometry = lightcraft_geom::CropGeometry::default();
    s.geometry.vertical = -60.0;
    s.geometry.rotate = 4.0;
    s.geometry.constrain_crop = true;
    let shown = uncropped(&s, None);
    let max = crop_tool_vs_cropped(&s, &shown, PixelWindow { x: 0, y: 0, w: shown.width, h: shown.height });
    assert!(max <= 2, "the crop tool's vignette differs from the export's by {max} levels");
}

// Given the crop tool, dragging the crop leaves its frame as it was: the rendered stages, keyed by
// the frame, are reused and only the per-pixel stage (where the vignette is) runs again
#[test]
fn the_crop_tools_frame_does_not_change_with_the_crop() {
    let s = crop_tool_vignette();
    let mut moved = s.clone();
    moved.crop.geometry.rect = Rect { x0: 0.3, y0: 0.2, x1: 0.9, y1: 0.95 };
    moved.crop.geometry.angle = -8.0;
    let (a, b) = (crate::geometry::Frame::new(480, 320, &s, false), crate::geometry::Frame::new(480, 320, &moved, false));
    assert_eq!(a, b);
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
    // …while the vignette's frame follows the crop
    assert_ne!(a.vignette_frame(&s, W, H), b.vignette_frame(&moved, W, H));
}

// Given the crop tool zoomed in (a window of its uncropped view), the window shows the same
// crop-anchored vignette
#[test]
fn a_window_of_the_crop_tools_view_anchors_the_vignette_to_the_crop() {
    let s = crop_tool_vignette();
    let whole = uncropped(&s, None);
    // over the crop's top left, and over its bottom right corner and the photo past it
    for at in [PixelWindow { x: 40, y: 40, w: 400, h: 300 }, PixelWindow { x: 360, y: 300, w: 400, h: 300 }] {
        let win = uncropped(&s, Some(at));
        assert_eq!((win.width, win.height), (at.w, at.h));
        let (max, _) = compare_inset(&whole, &win, at, 2);
        assert!(max <= 1, "window at {at:?}: differs from the whole view by {max}");
        let max = crop_tool_vs_cropped(&s, &win, at);
        assert!(max <= 2, "window at {at:?}: the vignette differs from the export's by {max} levels");
    }
}

// Given a crop with nothing in it, reversed, not finite or far past the photo, the crop tool's view
// renders without a panic and its vignette falls back to the whole photo
#[test]
fn the_crop_tools_vignette_survives_degenerate_crops() {
    let mut s = crop_tool_vignette();
    s.crop.geometry.angle = 0.0;
    let mut plain = s.clone();
    plain.crop.geometry.rect = Rect::UNIT;
    let expected = uncropped(&plain, None);
    for rect in [
        Rect { x0: 0.3, y0: 0.3, x1: 0.3, y1: 0.3 },
        Rect { x0: 0.6, y0: 0.6, x1: 0.2, y1: 0.2 },
        Rect { x0: f64::NAN, y0: 0.1, x1: 0.5, y1: f64::INFINITY },
        Rect { x0: -50.0, y0: -50.0, x1: 80.0, y1: 80.0 },
    ] {
        s.crop.geometry.rect = rect;
        let shown = uncropped(&s, None);
        assert_eq!(shown.data, expected.data, "{rect:?}");
    }
}

// Given lens corrections, which shrink the export's crop inside the corrected photo with or without
// Constrain Crop (#2361), the crop tool's vignette frames that shrunk crop
#[test]
fn the_crop_tools_vignette_frames_the_lens_corrected_crop() {
    let mut s = crop_tool_vignette();
    s.optics.distortion = -80.0;
    s.crop.geometry.rect = Rect { x0: 0.0, y0: 0.0, x1: 0.7, y1: 0.8 };
    let export = crate::geometry::Frame::new(480, 320, &s, true);
    assert!(export.crop.rect.width() < 0.65, "the export's crop is shrunk: {:?}", export.crop.rect);
    let shown = uncropped(&s, None);
    let max = crop_tool_vs_cropped(&s, &shown, PixelWindow { x: 0, y: 0, w: shown.width, h: shown.height });
    assert!(max <= 2, "the crop tool's vignette differs from the export's by {max} levels");
}

// Given lens corrections, perspective, the automatic lateral CA estimate (a second shrink after it)
// and crops so far out that nothing fits (the shrink's floor, reached twice), the crop tool frames
// the vignette by exactly the crop the export has
#[test]
fn the_crop_tools_vignette_crop_is_exactly_the_exports() {
    let src = crate::optics::tests::ca_grid(480, 320, 0.004, -0.003);
    assert_ne!(crate::optics::estimate_lateral_ca(&src), [0.0; 2], "the photo has lateral CA to estimate");
    let info = SourceInfo::default();
    let corner = Rect { x0: 0.0, y0: 0.0, x1: 0.05, y1: 0.05 };
    let cases: [(&str, f64, f64, f64, bool, bool, Rect); 8] = [
        ("distortion", -80.0, 0.0, 0.0, false, false, Rect::UNIT),
        ("distortion + keystone", -80.0, 0.0, -40.0, false, false, Rect::UNIT),
        ("constrained keystone", 0.0, 0.0, -40.0, true, false, Rect::UNIT),
        ("estimated CA", 0.0, 0.0, 0.0, false, true, Rect::UNIT),
        ("manual + estimated CA", 0.0, 60.0, 0.0, false, true, Rect::UNIT),
        ("distortion + estimated CA, constrained", 80.0, 0.0, -30.0, true, true, Rect::UNIT),
        ("nothing fits", -100.0, 0.0, 0.0, false, false, corner),
        ("nothing fits, twice", -100.0, 0.0, 0.0, false, true, corner),
    ];
    for (name, distortion, ca, vertical, constrain, remove_ca, rect) in cases {
        let mut s = DevelopSettings::default();
        s.optics.distortion = distortion;
        s.optics.ca_red = ca;
        s.optics.remove_ca = remove_ca;
        s.geometry.vertical = vertical;
        s.geometry.constrain_crop = constrain;
        s.crop.geometry.rect = rect;
        let export = crate::frame_for(&src, &info, &s, true);
        let tool = crate::frame_for(&src, &info, &s, false);
        assert!(export.crop.rect.width() < rect.width() * 0.999, "{name}: the export's crop is shrunk");
        assert_eq!(tool.export_crop(&s).crop, export.crop, "{name}");
        let (_, size) = tool.vignette_frame(&s, W, H);
        // (the repeated floors leave sides below a pixel: the export counts each as one)
        let (nw, nh) = export.native_size();
        assert_eq!(size, (nw.max(1.0), nh.max(1.0)), "{name}");
    }
    // the floor is reached twice when the estimate shrinks the crop again
    let mut s = DevelopSettings::default();
    (s.optics.distortion, s.optics.remove_ca, s.crop.geometry.rect) = (-100.0, true, corner);
    assert!(crate::frame_for(&src, &info, &s, true).crop.rect.width() < corner.width() * 0.001);
}

/// Where `export` pixel `q` (centre coordinates) sits in the vignette's frame, as the export's
/// vignette sees it (`(x + 0.5) / w`) and as the crop tool's view (`tool`, a `fw × fh` render,
/// optionally its window `win`) sees it: both as `|2n − 1|` per axis, which is all the vignette
/// reads, so a flip doesn't matter.
fn vignette_positions(
    s: &DevelopSettings,
    export: &crate::geometry::Frame,
    tool: &crate::geometry::Frame,
    (fw, fh): (usize, usize),
    win: Option<PixelWindow>,
    q: lightcraft_geom::Point,
) -> ([f64; 2], [f64; 2]) {
    let (ew, eh) = export.fit(W, H);
    let oriented = export.out_to_oriented(ew, eh).apply(q);
    let Some(to_render) = tool.out_to_oriented(fw, fh).inverse() else { panic!("the crop tool's view has no inverse") };
    let r = to_render.apply(oriented);
    let (frame, r, (w, h)) = match win {
        Some(win) => (tool.window(fw, fh, win), lightcraft_geom::Point::new(r.x - win.x as f64, r.y - win.y as f64), (win.w, win.h)),
        None => (tool.clone(), r, (fw, fh)),
    };
    let (a, (vw, vh)) = frame.vignette_frame(s, w, h);
    let v = a.apply(r);
    let axes = |x: f64, y: f64| [(2.0 * x - 1.0).abs(), (2.0 * y - 1.0).abs()];
    (axes(v.x / vw, v.y / vh), axes(q.x / ew as f64, q.y / eh as f64))
}

// Given a crop smaller than a source pixel on a side, which the export frames with each side
// counted as one pixel (`Frame::aspect`, `fit`), the crop tool's vignette has the export's shape:
// the same place in the frame and the same aspect, also turned, flipped or in a window
#[test]
fn the_crop_tools_vignette_has_the_exports_shape_for_crops_below_a_pixel() {
    let src = flat();
    let info = SourceInfo::default();
    let (sw, sh) = (src.width as f64, src.height as f64);
    let px = |x0: f64, y0: f64, w: f64, h: f64| Rect { x0, y0, x1: x0 + w / sw, y1: y0 + h / sh };
    let cases: [(&str, Rect, f64, bool, Option<PixelWindow>); 7] = [
        ("ordinary", Rect { x0: 0.12, y0: 0.15, x1: 0.6, y1: 0.75 }, 5.0, false, None),
        (
            "ordinary, flipped, window",
            Rect { x0: 0.12, y0: 0.15, x1: 0.6, y1: 0.75 },
            5.0,
            true,
            Some(PixelWindow { x: 300, y: 200, w: 320, h: 240 }),
        ),
        ("0.25 × 0.75 px", px(0.4, 0.4, 0.25, 0.75), 0.0, false, None),
        ("0.5 × 40 px", px(0.4, 0.3, 0.5, 40.0), 0.0, false, None),
        ("40 × 0.5 px", px(0.3, 0.4, 40.0, 0.5), 0.0, false, None),
        ("0.25 × 0.75 px, turned, flipped", px(0.4, 0.4, 0.25, 0.75), 10.0, true, None),
        ("0.5 × 40 px, window", px(0.4, 0.3, 0.5, 40.0), 0.0, false, Some(PixelWindow { x: 350, y: 180, w: 200, h: 200 })),
    ];
    for (name, rect, angle, flip, win) in cases {
        let mut s = crop_tool_vignette();
        s.crop.geometry.rect = rect;
        s.crop.geometry.angle = angle;
        s.crop.flip_h = flip;
        let export = crate::frame_for(&src, &info, &s, true);
        let tool = crate::frame_for(&src, &info, &s, false);
        let full = tool.fit(W, H);
        let (nw, nh) = export.native_size();
        let (_, size) = tool.vignette_frame(&s, full.0, full.1);
        assert_eq!(size, (nw.max(1.0), nh.max(1.0)), "{name}: the vignette's frame counts each side as at least a pixel");
        assert_eq!(size.0 / size.1, export.aspect(), "{name}: the export's aspect");
        let (ew, eh) = export.fit(W, H);
        for (fx, fy) in [(0.5, 0.5), (0.1, 0.2), (0.9, 0.35), (0.3, 0.95)] {
            let q = lightcraft_geom::Point::new(fx * ew as f64, fy * eh as f64);
            let (tool_at, export_at) = vignette_positions(&s, &export, &tool, full, win, q);
            for k in 0..2 {
                assert!((tool_at[k] - export_at[k]).abs() < 1e-6, "{name} at {fx}, {fy}: crop tool {tool_at:?}, export {export_at:?}");
            }
        }
    }
}

// Given crops far below a pixel, whose vignette frame the f32 kernels can't evaluate (a size
// that rounds to 0 in f32; coefficients that fit f32 but overflow at the far edge, turned or not;
// positions that fit but overflow when raised to the vignette's power, also only at Roundness
// −100's power of 8; positions f32 can't place within the frame, as where a tiny crop's
// translation cancels its pixels' products), the crop tool shows the whole-photo vignette
// instead of none or of one f32 has misplaced
#[test]
fn the_crop_tools_vignette_falls_back_to_the_whole_photo_where_f32_cant_hold_its_frame() {
    let cases: [(&str, Rect, f64, f64); 9] = [
        ("1e-50 square", Rect { x0: 0.0, y0: 0.0, x1: 1e-50, y1: 1e-50 }, 0.0, 0.0),
        ("1e-50 wide", Rect { x0: 0.0, y0: 0.2, x1: 1e-50, y1: 0.8 }, 0.0, 0.0),
        ("1e-39: the far edge overflows", Rect { x0: 0.0, y0: 0.0, x1: 1e-39, y1: 1e-39 }, 0.0, 0.0),
        ("1e-39 turned: cancellation", Rect { x0: 0.0, y0: 0.0, x1: 1e-39, y1: 1e-39 }, 45.0, 0.0),
        ("1e-22: the square overflows", Rect { x0: 0.0, y0: 0.0, x1: 1e-22, y1: 1e-22 }, 0.0, 0.0),
        ("1e-22 at Roundness -100", Rect { x0: 0.0, y0: 0.0, x1: 1e-22, y1: 1e-22 }, 0.0, -100.0),
        ("1e-6 at Roundness -100: the 8th power overflows", Rect { x0: 0.0, y0: 0.0, x1: 1e-6, y1: 1e-6 }, 0.0, -100.0),
        ("1e-6: the far edge is 2^20 frames away", Rect { x0: 0.0, y0: 0.0, x1: 1e-6, y1: 1e-6 }, 0.0, 0.0),
        ("1e-8 at 0.405: f32 can't place the crop's centre", Rect { x0: 0.405, y0: 0.405, x1: 0.405 + 1e-8, y1: 0.405 + 1e-8 }, 0.0, 0.0),
    ];
    for (name, rect, angle, roundness) in cases {
        let mut s = crop_tool_vignette();
        s.vignette.roundness = roundness;
        // the fallback the crop tool already had: a crop with nothing in it
        let mut empty = s.clone();
        empty.crop.geometry.rect = Rect { x0: 0.3, y0: 0.3, x1: 0.3, y1: 0.3 };
        empty.crop.geometry.angle = 0.0;
        let expected = uncropped(&empty, None);
        // the whole-photo vignette is there to fall back to: the corners are darker than the middle
        let (mid, corner) = (expected.get(expected.width / 2, expected.height / 2)[1], expected.get(2, 2)[1]);
        assert!(mid as i32 - corner as i32 > 40, "{name}: no vignette in the expected render");
        s.crop.geometry.rect = rect;
        s.crop.geometry.angle = angle;
        let shown = uncropped(&s, None);
        assert!(shown.data == expected.data, "{name}: the crop tool doesn't show the whole-photo vignette");
    }
    // …while a crop of half a pixel holds in f32: its vignette stays on the crop, so the photo's
    // middle, far from it, is as dark as its corners
    let mut s = crop_tool_vignette();
    s.crop.geometry = lightcraft_geom::CropGeometry { rect: Rect { x0: 0.0, y0: 0.0, x1: 1e-3, y1: 1e-3 }, angle: 0.0 };
    let shown = uncropped(&s, None);
    let (mid, far) = (shown.get(shown.width / 2, shown.height / 2)[1], shown.get(shown.width - 3, shown.height - 3)[1]);
    assert!(mid.abs_diff(far) <= 2, "the 1e-3 crop's vignette is not on the crop: middle {mid}, far corner {far}");
}

// Given a crop whose frame f32 can hold over a window around it but not over the whole view (here
// its 8th power overflows at the far edge), the window shows the whole view's vignette, not its own
#[test]
fn a_window_falls_back_like_the_whole_view_it_is_part_of() {
    let mut s = crop_tool_vignette();
    s.vignette.roundness = -100.0;
    s.crop.geometry = lightcraft_geom::CropGeometry { rect: Rect { x0: 0.4, y0: 0.4, x1: 0.4 + 1e-6, y1: 0.4 + 1e-6 }, angle: 0.0 };
    let whole = uncropped(&s, None);
    let (x, y) = (whole.width * 2 / 5, whole.height * 2 / 5);
    let at = PixelWindow { x: x - 2, y: y - 2, w: 4, h: 4 };
    let (max, _) = compare_inset(&whole, &uncropped(&s, Some(at)), at, 0);
    assert!(max <= 1, "the window differs from the whole view by {max}");
}

// Given vignette frames whose f32 evaluation overflows only at some stage (an affine product, the
// power of 8 but not of 2, a size of 0 or NaN), the guard rejects exactly those, and keeps the
// whole output's frame
#[test]
fn the_vignette_frame_guard_bounds_every_stage_of_the_kernels() {
    use crate::finish::vig_frame_fits_f32 as fits;
    let id = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    assert!(fits(id, [960.0, 640.0], (960, 640), 8.0), "the whole output");
    // the products overflow, though the coefficients and the frame's size fit, and the division
    // by that large frame would bring the positions back into range (f32 would place them)
    assert!(!fits([1e38, 0.0, 0.0, 1.0, 0.0, 0.0], [1e38, 1e38], (480, 320), 2.0));
    // …also where they cancel at the far corner, but not at the others
    assert!(!fits([1e38, 0.0, -1e38, 1.0, 0.0, 0.0], [1e38, 1e38], (480, 480), 2.0));
    assert!(fits([1e34, 0.0, -1e34, 1.0, 0.0, 0.0], [1e38, 1e38], (480, 480), 2.0));
    // the positions fit, and their squares, but not their 8th powers (16× below f32::MAX: the
    // kernels' rounding could still overflow them)
    assert!(fits(id, [1.0, 1.0], (65536, 65536), 2.0));
    assert!(!fits(id, [1.0, 1.0], (65536, 65536), 8.0));
    assert!(fits(id, [1.0, 1.0], (9000, 9000), 8.0));
    // f32 places the pixels to within 1/16 of the frame while they are at most 2^16 frames away
    assert!(fits(id, [1.0, 1.0], (65536, 1), 2.0));
    assert!(!fits(id, [1.0, 1.0], (65538, 1), 2.0));
    assert!(!fits(id, [1.0, 1.0], (1, 65538), 2.0));
    // an extreme aspect overflows the scaling (one pixel: f32 would place it)
    assert!(!fits(id, [1e38, 1e-3], (1, 1), 2.0));
    assert!(fits(id, [1e30, 1e-3], (1, 1), 2.0));
    for size in [[0.0, 640.0], [-960.0, 640.0], [960.0, f32::NAN], [f32::INFINITY, 640.0]] {
        assert!(!fits(id, size, (960, 640), 2.0), "{size:?}");
    }
    assert!(!fits([f32::INFINITY, 0.0, 0.0, 1.0, 0.0, 0.0], [960.0, 640.0], (960, 640), 2.0));
    assert!(!fits(id, [960.0, 640.0], (960, 640), f32::NAN));
    // a frame of 1 px around x = y = 40.5, 1e6 render pixels across: the products and the
    // translation (-4.05e7) cancel, where f32 is 4 px, 4 frames, apart
    assert!(!fits([1e6, 0.0, 0.0, 1e6, -4.05e7, -4.05e7], [1.0, 1.0], (100, 100), 2.0));
    // …while 2 px crops at the far corner of the largest photo place to within 1/16 of a frame
    assert!(fits([1.0, 0.0, 0.0, 1.0, -65533.0, -65533.0], [2.0, 2.0], (65535, 65535), 2.0));
    assert!(!fits([1.0, 0.0, 0.0, 1.0, -65533.0, -65533.0], [1.0, 1.0], (65535, 65535), 2.0));
}

// Given a crop of at least a pixel on each side, the crop tool's vignette frame is exactly the one
// it had before crops below a pixel were framed like the export (#3621 review): the same affine
// and size, bit for bit, whole or in a window, flipped, turned, under lens corrections
#[test]
fn ordinary_crops_keep_their_vignette_frame() {
    use lightcraft_geom::{Affine, CropGeometry, Vec2};
    // the formula as it was at 5204ee01: the crop's own size, unclamped
    let before = |f: &crate::geometry::Frame, s: &DevelopSettings, w: usize, h: usize| {
        let (vx, vy, fw, fh) = f.view.map_or((0.0, 0.0, w as f64, h as f64), |v| (v.x, v.y, v.full_w, v.full_h));
        let offset = Affine::translate(Vec2::new(vx, vy));
        let cropped = f.export_crop(s);
        let (cw, ch) = cropped.native_size();
        let to_oriented = crate::geometry::Frame { crop: CropGeometry::default(), ..cropped.clone() }.out_to_oriented(fw as usize, fh as usize);
        let Some(to_crop) = cropped.crop.output_to_source(f.ow, f.oh, cw, ch).inverse() else { panic!("no inverse") };
        (to_crop * to_oriented * offset, (cw, ch))
    };
    let src = flat();
    let info = SourceInfo::default();
    let cases: [(&str, Rect, f64, bool, bool, f64, Option<PixelWindow>); 6] = [
        ("off-centre, turned", Rect { x0: 0.12, y0: 0.15, x1: 0.6, y1: 0.75 }, 5.0, false, false, 0.0, None),
        ("flip H", Rect { x0: 0.3, y0: 0.05, x1: 0.95, y1: 0.6 }, -3.0, true, false, 0.0, None),
        (
            "flip V, window",
            Rect { x0: 0.3, y0: 0.05, x1: 0.95, y1: 0.6 },
            -3.0,
            false,
            true,
            0.0,
            Some(PixelWindow { x: 120, y: 90, w: 300, h: 200 }),
        ),
        ("lens corrected", Rect { x0: 0.0, y0: 0.0, x1: 0.7, y1: 0.8 }, 0.0, false, false, -80.0, None),
        ("a crop of 1.5 × 1.5 px", Rect { x0: 0.5, y0: 0.5, x1: 0.5 + 1.5 / 480.0, y1: 0.5 + 1.5 / 320.0 }, 0.0, false, false, 0.0, None),
        ("uncut", Rect::UNIT, 0.0, false, false, 0.0, None),
    ];
    for (name, rect, angle, flip_h, flip_v, distortion, win) in cases {
        let mut s = crop_tool_vignette();
        s.crop.geometry = CropGeometry { rect, angle };
        (s.crop.flip_h, s.crop.flip_v, s.optics.distortion) = (flip_h, flip_v, distortion);
        let tool = crate::frame_for(&src, &info, &s, false);
        let (fw, fh) = tool.fit(W, H);
        let (frame, w, h) = match win {
            Some(win) => (tool.window(fw, fh, win), win.w, win.h),
            None => (tool, fw, fh),
        };
        let (cw, ch) = frame.export_crop(&s).native_size();
        assert!(cw >= 1.0 && ch >= 1.0, "{name}: {cw} × {ch}");
        let (aff, (vw, vh)) = before(&frame, &s, w, h);
        assert_eq!(frame.vignette_frame(&s, w, h), (aff, (vw, vh)), "{name}");
        // …and the kernels get it: f32 holds it
        let fp = crate::finish::FinishParams::new(crate::finish::FinishParamsInput {
            settings: &s,
            frame: &frame,
            info: &info,
            image_size: (w, h),
            px_per_long: fw.max(fh) as f64,
            air_pre: 1.0,
            space: crate::OutputSpace::Srgb,
        });
        assert_eq!((fp.vig_aff, fp.vig_size), (aff.0.map(|v| v as f32), [vw as f32, vh as f32]), "{name}");
    }
}

#[test]
fn dithering_keeps_the_output_origin_when_the_vignette_uses_a_crop() {
    let src = Rgb32f::filled(480, 320, [0.123456; 3]);
    let info = SourceInfo::default();
    let mut s = DevelopSettings::default();
    s.crop.geometry.rect = Rect { x0: 0.12, y0: 0.15, x1: 0.6, y1: 0.75 };
    s.crop.geometry.angle = 5.0;
    let req = RenderRequest { apply_crop: false, ..RenderRequest::fit(480, 480) };
    let at = PixelWindow { x: 7, y: 12, w: 120, h: 80 };
    let full = render(&src, &info, &s, &req).image;
    let part = render(&src, &info, &s, &RenderRequest { window: Some(at), ..req }).image;
    assert_eq!((part.width, part.height), (at.w, at.h));
    assert!(full.data.iter().any(|p| p[0] != full.data[0][0]), "the flat image must exercise Bayer quantization");
    for y in 0..at.h {
        for x in 0..at.w {
            assert_eq!(part.get(x, y), full.get(at.x + x, at.y + y), "Bayer phase at {x},{y}");
        }
    }
    // Enabling an off-centre crop vignette must never replace the dithering origin.
    for amount in [0.0, -80.0] {
        s.vignette.amount = amount;
        let mut frame = crate::frame_for(&src, &info, &s, false);
        frame.view = Some(crate::geometry::ViewWindow { x: 7.0, y: 12.0, full_w: 480.0, full_h: 320.0 });
        let fp = crate::finish::FinishParams::new(crate::finish::FinishParamsInput {
            settings: &s,
            frame: &frame,
            info: &info,
            image_size: (120, 80),
            px_per_long: 480.0,
            air_pre: 1.0,
            space: crate::OutputSpace::Srgb,
        });
        assert_eq!(fp.view, [7.0, 12.0, 480.0, 320.0]);
        if amount != 0.0 {
            assert_ne!(&fp.vig_aff[4..6], &fp.view[..2], "the crop vignette has a different origin");
        }
    }
    // …and renders keep it: with the crop's vignette on, a window inside the crop, where the
    // vignette leaves the pixels as they are, has the whole render's pixels exactly
    s.vignette.amount = -80.0;
    s.vignette.midpoint = 100.0;
    let full = render(&src, &info, &s, &req).image;
    assert!(full.get(2, 2)[1] + 20 < full.get(173, 144)[1], "the vignette darkens outside the crop");
    let at = PixelWindow { x: 157, y: 129, w: 32, h: 32 };
    let part = render(&src, &info, &s, &RenderRequest { window: Some(at), ..req }).image;
    for y in 0..at.h {
        for x in 0..at.w {
            assert_eq!(part.get(x, y), full.get(at.x + x, at.y + y), "Bayer phase under the vignette at {x},{y}");
        }
    }
}
