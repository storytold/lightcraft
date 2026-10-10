//! Region rendering (issue #323): a window of the virtual full-size output is rendered on its own
//! and must match the same pixels of the full render, so zoomed views can be sharp at any zoom
//! without materializing the whole frame.

use lightcraft_develop::{DevelopSettings, controls};
use lightcraft_geom::{Orientation, Rect};
use lightcraft_raster::{Rgb32f, Rgba8};

use crate::{PixelWindow, RenderRequest, SourceInfo, render};

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
