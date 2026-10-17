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
    render(&scene(), &SourceInfo::default(), s, &RenderRequest::fit(W, H)).unwrap().image
}

fn window(s: &DevelopSettings, win: PixelWindow) -> Rgba8 {
    let req = RenderRequest { window: Some(win), ..RenderRequest::fit(W, H) };
    render(&scene(), &SourceInfo::default(), s, &req).unwrap().image
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

#[test]
fn supplied_mattes_skip_heuristic_window_context() {
    use crate::masks::{MatteKind, Mattes};
    use lightcraft_develop::{LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
    use lightcraft_raster::Image;
    use std::sync::Arc;

    let src = scene();
    let mut mattes = Mattes::default();
    mattes.push(MatteKind::Person, Image::from_fn(60, 40, |x, _| if x < 30 { 255 } else { 0 }));
    mattes.push(MatteKind::Sky, Image::from_fn(60, 40, |_, y| if y < 20 { 255 } else { 0 }));
    mattes.push(MatteKind::Skin, Image::from_fn(60, 40, |x, _| if x < 20 { 255 } else { 0 }));
    let info = SourceInfo { mattes: Some(Arc::new(mattes)), ..Default::default() };
    let req = RenderRequest { window: Some(MID), ..RenderRequest::fit(W, H) };
    for shape in [
        MaskShape::Subject,
        MaskShape::Background,
        MaskShape::Sky,
        MaskShape::People { person: 0, parts: vec![] },
        MaskShape::People { person: 0, parts: vec!["skin".into()] },
    ] {
        let mut s = DevelopSettings::default();
        s.masks.push(Mask {
            components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }],
            adjust: LocalAdjustments { exposure: 1.0, ..Default::default() },
            ..Default::default()
        });
        let p = crate::plan(&src, &info, &s, &req);
        assert_eq!((p.w, p.h), (MID.w, MID.h), "a supplied matte must not read heuristic context");
        let f = render(&src, &info, &s, &RenderRequest { window: None, ..req }).unwrap().image;
        let w = render(&src, &info, &s, &req).unwrap().image;
        assert_eq!(f.crop(MID.x, MID.y, MID.w, MID.h), w);
        s.masks[0].refine = 100.0;
        let refined = crate::plan(&src, &info, &s, &req);
        assert!(refined.w > MID.w, "Refine Edges still needs context with a supplied matte");
        s.masks[0].refine = 0.0;
        let heuristic = crate::plan(&src, &SourceInfo::default(), &s, &req);
        assert!(heuristic.w > MID.w, "without a matching matte the heuristic still needs context");
    }
}

#[test]
fn hidden_masks_do_not_expand_window_context() {
    use lightcraft_develop::{LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
    use lightcraft_geom::Point;
    use std::sync::Arc;
    let src = scene();
    let info = SourceInfo::default();
    let req = RenderRequest { window: Some(MID), ..RenderRequest::fit(W, H) };
    let mut s = DevelopSettings::default();
    s.masks.push(Mask {
        visible: false,
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Linear { start: Point::new(0.1, 0.1), end: Point::new(0.9, 0.9) },
        }],
        adjust: LocalAdjustments {
            highlights: -80.0,
            shadows: 80.0,
            clarity: 60.0,
            texture: 60.0,
            dehaze: 80.0,
            sharpness: 60.0,
            noise: 70.0,
            moire: 50.0,
            defringe: 80.0,
            ..Default::default()
        },
        ..Default::default()
    });
    let p = crate::plan(&src, &info, &s, &req);
    assert_eq!((p.w, p.h), (MID.w, MID.h), "hidden local adjustments must not request spatial context");
    assert!(!p.needs_airlight, "hidden Dehaze must not compute full-frame airlight");
    let mut planes = crate::local::Planes::default();
    let img = Arc::new(p.frame.sample(&src, p.w, p.h));
    crate::local::prepare(
        img,
        &s,
        &p.frame,
        p.px_per_long,
        req.quality,
        &mut planes,
        None,
        (&mut crate::masks::AlphaCache::default(), crate::masks::AlphaKeys { geo: 0, image: 0 }),
    );
    assert!(
        planes.base.is_some() && planes.clarity.is_some() && planes.texture.is_some() && planes.dark.is_some() && planes.chroma.is_some(),
        "rendering preserves main's plane selection; only the context calculation excludes unused planes"
    );
    s.masks[0].visible = true;
    let p = crate::plan(&src, &info, &s, &req);
    assert!(p.w > MID.w && p.needs_airlight, "showing the mask restores its context and airlight");
}

#[test]
fn defringe_window_includes_edge_neighbourhood() {
    let src = Rgb32f::from_fn(1200, 800, |x, _| if x < 500 { [0.01; 3] } else { [0.4, 0.08, 0.5] });
    let mut s = DevelopSettings::default();
    s.optics.defringe_purple_amount = 20.0;
    s.optics.defringe_purple_hue_lo = 0.0;
    s.optics.defringe_purple_hue_hi = 100.0;
    let info = SourceInfo::default();
    let req = RenderRequest::fit(1200, 800);
    let at = PixelWindow { x: 500, y: 300, w: 160, h: 120 };
    let f = render(&src, &info, &s, &req).unwrap().image;
    let w = render(&src, &info, &s, &RenderRequest { window: Some(at), ..req }).unwrap().image;
    let (max, mean) = compare_inset(&f, &w, at, 0);
    assert!(max <= 3 && mean <= 0.5, "Defringe at the window edge: max {max}, mean {mean}");
}

#[test]
fn defringe_context_surrounds_spot_source_reads() {
    use lightcraft_develop::{Spot, SpotMode};
    use lightcraft_geom::Point;
    let src = scene();
    let info = SourceInfo::default();
    let mut s = DevelopSettings::default();
    s.spots.push(Spot {
        mode: SpotMode::Clone,
        points: vec![Point::new(0.4, 0.4)],
        size: 0.01,
        source_offset: Some(Point::new(0.4, 0.0)),
        ..Default::default()
    });
    let win = PixelWindow { x: 450, y: 300, w: 100, h: 100 };
    let req = RenderRequest { window: Some(win), ..RenderRequest::fit(1200, 800) };
    let plain = crate::plan(&src, &info, &s, &req);
    let plain_size = (plain.w, plain.h);
    assert!(plain.w > win.w, "the clone source is outside the requested window");
    s.optics.defringe_purple_amount = 20.0;
    let fringed = crate::plan(&src, &info, &s, &req);
    // At 1200 px, amount 20 has radius round((1 + 20 * 0.3) * 1200 / 4000) = 2.
    assert_eq!(
        (fringed.w, fringed.h),
        (plain_size.0 + 4, plain_size.1 + 4),
        "Defringe context must surround the spot source as well as the requested window"
    );
}

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
    let r = render(&scene(), &SourceInfo::default(), &DevelopSettings::default(), &req).unwrap();
    assert_eq!((r.image.width, r.image.height), (256, 192));
}

#[test]
fn oversized_filter_support_keeps_mains_bounded_window() {
    let src = scene();
    let mut s = DevelopSettings::default();
    s.light.highlights = -40.;
    s.light.shadows = 30.;
    s.effects.clarity = 15.;
    s.effects.dehaze = 10.;
    s.detail.nr_luminance = 30.;
    s.detail.nr_color = 25.;
    let req = RenderRequest { window: Some(PixelWindow { x: 20_000, y: 12_000, w: 400, h: 300 }), ..RenderRequest::fit(48_000, 32_000) };
    let p = crate::plan(&src, &SourceInfo::default(), &s, &req);
    assert_eq!((p.w, p.h), (400, 300));
    assert!(p.needs_airlight);
    let out = render(&src, &SourceInfo::default(), &s, &req).unwrap();
    assert_eq!((out.image.width, out.image.height), (400, 300));
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
    let f = render(&src, &SourceInfo::default(), s, &RenderRequest::fit(W, H)).unwrap().image;
    let w = render(&src, &SourceInfo::default(), s, &RenderRequest { window: Some(win), ..RenderRequest::fit(W, H) }).unwrap().image;
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

#[test]
fn exact_dehaze_window_uses_the_original_size_for_noise_reduction() {
    let src = noisy_scene();
    let info = SourceInfo { native_long: Some(3840), ..Default::default() };
    let mut s = DevelopSettings::default();
    s.detail.nr_luminance = 80.0;
    s.detail.nr_color = 80.0;
    s.effects.dehaze = 30.0;
    let req = RenderRequest::fit(W, H);
    let full = render(&src, &info, &s, &req).unwrap().image;
    let window_req = RenderRequest { window: Some(MID), ..req };
    let plan = crate::plan(&src, &info, &s, &window_req);
    assert_eq!(plan.detail_scale, 0.25);
    let window = render(&src, &info, &s, &window_req).unwrap().image;
    let (max, mean) = compare_inset(&full, &window, MID, 0);
    assert!(max <= 1 && mean <= 0.1, "original-size NR: max {max}, mean {mean}");
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
            let f = render(&noisy_scene(), &SourceInfo::default(), &s, &RenderRequest { overlay, ..RenderRequest::fit(W, H) }).unwrap().image;
            let w = render(&noisy_scene(), &SourceInfo::default(), &s, &RenderRequest { overlay, window: Some(MID), ..RenderRequest::fit(W, H) })
                .unwrap()
                .image;
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
        let f = render(&noisy_scene(), &SourceInfo::default(), &s, &RenderRequest { overlay, ..RenderRequest::fit(W, H) }).unwrap().image;
        let w = render(&noisy_scene(), &SourceInfo::default(), &s, &RenderRequest { overlay, window: Some(MID), ..RenderRequest::fit(W, H) })
            .unwrap()
            .image;
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

/// A window of a 7680 × 5120 frame against the same pixels of the whole render.
/// The renderer must supply the context read by its wide spatial filters.
fn check_big(name: &str, s: &DevelopSettings, max_ok: u8, mean_ok: f64) {
    let src = blocks();
    let f = render(&src, &SourceInfo::default(), s, &RenderRequest::fit(BIG_W, BIG_H)).unwrap().image;
    let win = PixelWindow { x: 3000, y: 1800, w: 512, h: 384 };
    let w = render(&src, &SourceInfo::default(), s, &RenderRequest { window: Some(win), ..RenderRequest::fit(BIG_W, BIG_H) }).unwrap().image;
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

// Issue #1146: the renderer must supply context even when the caller's margin is
// smaller than a spatial operator's support. These are the sweep's exact pixels.
#[test]
fn dehaze_with_the_sweeps_small_margins_matches_full() {
    let src = blocks();
    let mut s = DevelopSettings::default();
    s.effects.dehaze = 100.0;
    let req = RenderRequest::fit(2400, 1600);
    let f = render(&src, &SourceInfo::default(), &s, &req).unwrap().image;
    let mut failures = Vec::new();
    for margin in [38, 77, 130] {
        let at = PixelWindow { x: 1000 - margin, y: 650 - margin, w: 400 + 2 * margin, h: 300 + 2 * margin };
        let w = render(&src, &SourceInfo::default(), &s, &RenderRequest { window: Some(at), ..req }).unwrap().image;
        let (max, mean) = compare_inset(&f, &w, at, margin);
        eprintln!("dehaze margin {margin}: max {max}, mean {mean:.6}");
        if max > 3 || mean > 0.5 {
            failures.push((margin, max, mean));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

fn check_tight_spatial_windows(id: &str, value: f64) {
    use lightcraft_develop::Process;
    let src = blocks();
    let mut failures = Vec::new();
    for process in [Process::V1, Process::V2] {
        let mut s = DevelopSettings { process: process.version(), ..Default::default() };
        controls::set(&mut s, id, value);
        // Odd dimensions and unsnapped positions also exercise the guided filter's grid.
        let req = RenderRequest::fit(2397, 1598);
        let f = render(&src, &SourceInfo::default(), &s, &req).unwrap().image;
        for (x, y) in [(999, 649), (388, 124), (0, 0), (1997, 0), (0, 1298), (1997, 1298)] {
            let at = PixelWindow { x, y, w: 400, h: 300 };
            let w = render(&src, &SourceInfo::default(), &s, &RenderRequest { window: Some(at), ..req }).unwrap().image;
            assert_eq!((w.width, w.height), (at.w, at.h));
            let (max, mean) = compare_inset(&f, &w, at, 0);
            eprintln!("{id} {process:?} ({x},{y}): max {max}, mean {mean:.6}");
            if max > 3 || mean > 0.5 {
                failures.push((process, x, y, max, mean));
            }
        }
    }
    assert!(failures.is_empty(), "{id}: {failures:?}");
}

#[test]
fn tight_spatial_dehaze_windows() {
    check_tight_spatial_windows("effects.dehaze", 100.0);
}
#[test]
fn tight_spatial_clarity_windows() {
    check_tight_spatial_windows("effects.clarity", 100.0);
}
#[test]
fn tight_spatial_texture_windows() {
    check_tight_spatial_windows("effects.texture", 100.0);
}
#[test]
fn tight_spatial_highlights_windows() {
    check_tight_spatial_windows("light.highlights", -100.0);
}
#[test]
fn tight_spatial_shadows_windows() {
    check_tight_spatial_windows("light.shadows", 100.0);
}

#[test]
fn spatial_context_is_not_capped_by_the_display_texture() {
    let src = blocks();
    let mut s = DevelopSettings::default();
    s.effects.dehaze = 100.0;
    let win = PixelWindow { x: 20_000, y: 12_000, w: 400, h: 300 };
    let req = RenderRequest { window: Some(win), ..RenderRequest::fit(48_000, 32_000) };
    let p = crate::plan(&src, &SourceInfo::default(), &s, &req);
    let radius: usize = lightcraft_raster::blur::box_radii(960.0).into_iter().sum();
    assert!(radius > 768);
    assert_eq!((p.w, p.h), (win.w + radius * 2, win.h + radius * 2));
    assert_eq!(p.keep, Some(PixelWindow { x: radius, y: radius, ..win }));
}

// The float and 16-bit paths must also return only the requested pixels after
// preparing the context, including a window clipped by the physical frame.
#[test]
fn deep_spatial_windows_keep_the_requested_extent() {
    use crate::OutputDepth;
    let src = blocks();
    let mut s = DevelopSettings::default();
    s.effects.texture = 100.0;
    let at = PixelWindow { x: 388, y: 0, w: 120, h: 90 };
    for depth in [OutputDepth::U16, OutputDepth::F32Linear, OutputDepth::F32Hdr] {
        let req = RenderRequest { depth, ..RenderRequest::fit(2400, 1600) };
        let f = render(&src, &SourceInfo::default(), &s, &req).unwrap();
        let w = render(&src, &SourceInfo::default(), &s, &RenderRequest { window: Some(at), ..req }).unwrap();
        let deep = w.deep.as_ref().unwrap();
        assert_eq!((deep.width, deep.height), (at.w, at.h));
        assert_eq!((w.image.width, w.image.height), (at.w, at.h));
        let (max, mean) = compare_inset(&f.image, &w.image, at, 0);
        assert!(max <= 1 && mean <= 0.1, "{depth:?}: {max}, {mean}");
    }
}

#[test]
fn combined_spatial_windows_and_mask_adjustments_match_full() {
    use lightcraft_develop::{LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape, Process};
    use lightcraft_geom::Point;
    let src = blocks();
    let mut s = DevelopSettings::default();
    s.effects.dehaze = 80.0;
    s.effects.clarity = 80.0;
    s.effects.texture = 60.0;
    s.light.highlights = -80.0;
    s.light.shadows = 80.0;
    s.detail.nr_luminance = 40.0;
    s.detail.nr_color = 30.0;
    s.masks.push(Mask {
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Linear { start: Point::new(0.1, 0.1), end: Point::new(0.9, 0.9) },
        }],
        adjust: LocalAdjustments { clarity: 50.0, texture: 40.0, highlights: -60.0, shadows: 50.0, ..Default::default() },
        ..Default::default()
    });
    for process in [Process::V1, Process::V2] {
        s.process = process.version();
        let req = RenderRequest::fit(2400, 1600);
        let f = render(&src, &SourceInfo::default(), &s, &req).unwrap().image;
        for (x, y) in [(1000, 650), (0, 0), (2000, 1300)] {
            let at = PixelWindow { x, y, w: 400, h: 300 };
            let w = render(&src, &SourceInfo::default(), &s, &RenderRequest { window: Some(at), ..req }).unwrap().image;
            let (max, mean) = compare_inset(&f, &w, at, 0);
            assert!(max <= 3 && mean <= 0.5, "{process:?} ({x},{y}): {max}, {mean}");
        }
    }
}

#[test]
fn tight_refined_mask_windows_match_full() {
    use lightcraft_develop::{LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
    use lightcraft_geom::Point;
    let src = blocks();
    let mut s = DevelopSettings::default();
    let req = RenderRequest::fit(960, 640);
    for shape in [MaskShape::Linear { start: Point::new(0.4, 0.1), end: Point::new(0.6, 0.9) }, MaskShape::Subject, MaskShape::Sky] {
        s.masks = vec![Mask {
            components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }],
            adjust: LocalAdjustments { exposure: 2.0, ..Default::default() },
            refine: 100.0,
            ..Default::default()
        }];
        let f = render(&src, &SourceInfo::default(), &s, &req).unwrap().image;
        for (x, y) in [(301, 181), (0, 0), (560, 340)] {
            let at = PixelWindow { x, y, w: 400, h: 300 };
            let w = render(&src, &SourceInfo::default(), &s, &RenderRequest { window: Some(at), ..req }).unwrap().image;
            let (max, mean) = compare_inset(&f, &w, at, 0);
            assert!(max <= 3 && mean <= 0.5, "{:?} ({x},{y}): {max}, {mean}", s.masks[0].components[0].shape);
        }
    }
}

#[test]
fn refined_mask_alpha_survives_exposure_edit() {
    use crate::local;
    use lightcraft_develop::{Mask, MaskComponent, MaskOp, MaskShape};
    use lightcraft_geom::Point;
    use std::sync::Arc;
    let img = Arc::new(scene());
    let info = SourceInfo::default();
    let mut s = DevelopSettings::default();
    s.masks.push(Mask {
        refine: 100.0,
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Linear { start: Point::new(0.2, 0.1), end: Point::new(0.8, 0.9) },
        }],
        ..Default::default()
    });
    let req = RenderRequest::fit(img.width, img.height);
    let settings = s.clone();
    let p = crate::plan(&img, &info, &settings, &req);
    let mut planes = local::Planes::default();
    let mut alphas = crate::masks::AlphaCache::default();
    let a = local::prepare(
        img.clone(),
        &s,
        &p.frame,
        p.px_per_long,
        req.quality,
        &mut planes,
        None,
        (&mut alphas, crate::masks::AlphaKeys { geo: 0, image: 0 }),
    );
    s.light.exposure = 1.0;
    let b =
        local::prepare(img, &s, &p.frame, p.px_per_long, req.quality, &mut planes, None, (&mut alphas, crate::masks::AlphaKeys { geo: 0, image: 0 }));
    assert_eq!(a.masks[0].alpha.data, b.masks[0].alpha.data);
    assert_eq!(a.masks[0].alpha.data.as_ptr(), b.masks[0].alpha.data.as_ptr(), "unchanged refined alpha must be reused");
}

#[test]
fn cache_bytes_include_chroma() {
    use std::sync::Arc;
    let src = Arc::new(Rgb32f::new(4, 3));
    let cache = crate::StageCache::default();
    cache.put(crate::CacheEntry {
        full_airlight: None,
        src: src.clone(),
        geo: 0,
        sampled: Arc::new((*src).clone()),
        lin: None,
        masks: Default::default(),
        planes: crate::local::Planes { chroma: Some((0, Arc::new(Rgb32f::new(4, 3)))), ..Default::default() },
    });
    assert_eq!(cache.bytes(), 2 * 4 * 3 * std::mem::size_of::<[f32; 3]>());
}

#[test]
fn native_frame_cache_shares_source_and_counts_only_owned_pixels() {
    use std::sync::Arc;
    let src = Arc::new(scene());
    let info = SourceInfo::default();
    let s = DevelopSettings::default();
    let cache = crate::StageCache::default();
    let req = RenderRequest { window: Some(PixelWindow { x: 0, y: 0, w: src.width, h: src.height }), ..RenderRequest::fit(src.width, src.height) };
    let cached = crate::render_cached(&src, &info, &s, &req, &cache).unwrap();
    assert_eq!(cached.image, render(&src, &info, &s, &req).unwrap().image);
    assert!(Arc::ptr_eq(&cache.lock()[0].sampled, &src));
    // The source belongs to the caller. Only the linear RGB and log-luminance plane are new.
    assert_eq!(cache.bytes(), src.data.len() * (12 + 4));
}

#[test]
fn temporary_windows_leave_exact_airlight_and_bounded_stages_alone() {
    use crate::{Quality, StageCache, render_cached};
    use std::sync::Arc;
    let src = Arc::new(Rgb32f::from_fn(401, 267, |x, y| {
        let l = 0.01 + ((x * 17 + y * 23) % 103) as f32 / 103.0;
        [l, l * 0.82, l * 1.15]
    }));
    let info = SourceInfo::default();
    let mut s = DevelopSettings::default();
    s.effects.dehaze = 40.0;
    s.effects.clarity = 30.0;
    let req = RenderRequest { window: Some(PixelWindow { x: 101, y: 77, w: 121, h: 83 }), ..RenderRequest::fit(1201, 803) };
    let mut cache = StageCache::default();
    let expected = render_cached(&src, &info, &s, &req, &cache).unwrap().image;
    let prepared = cache.airlight_frame.value().unwrap();
    let full = crate::plan(&src, &info, &s, &RenderRequest { window: None, ..req });
    let scalar = cache.airlight.get(&src, full.lin_key);
    let bytes = cache.bytes();
    let len = cache.len();
    for window in [req.window, Some(PixelWindow { x: 0, y: 0, w: 1201, h: 803 })] {
        let bounded = RenderRequest { bounded_window: true, window, ..req };
        let warm = render_cached(&src, &info, &s, &bounded, &cache).unwrap().image;
        assert_eq!(warm, render(&src, &info, &s, &bounded).unwrap().image);
        assert_eq!((cache.bytes(), cache.len()), (bytes, len));
        assert!(Arc::ptr_eq(&prepared, &cache.airlight_frame.value().unwrap()));
        assert_eq!(cache.airlight.get(&src, full.lin_key), scalar);
        let fresh = StageCache::default();
        render_cached(&src, &info, &s, &bounded, &fresh).unwrap();
        assert_eq!(fresh.len(), 0);
        assert!(fresh.airlight.get(&src, full.lin_key).is_none());
        assert!(fresh.airlight_frame.value().is_none());
    }
    // Even an over-budget draft must retain the exact preparation it will
    // need after the drag. It must not seed a proxy into the exact scalar cache.
    cache.airlight = crate::airlight::AirlightCache::with_budget(1);
    cache.airlight.remember(&src, full.lin_key, 0.25);
    render_cached(&src, &info, &s, &RenderRequest { quality: Quality::Draft, ..req }, &cache).unwrap();
    assert!(Arc::ptr_eq(&prepared, &cache.airlight_frame.value().unwrap()));
    assert_eq!(cache.airlight.get(&src, full.lin_key), Some(0.25));
    cache.airlight = crate::airlight::AirlightCache::default();
    assert_eq!(render_cached(&src, &info, &s, &req, &cache).unwrap().image, expected);
    s.detail.nr_luminance = 50.0;
    render_cached(&src, &info, &s, &RenderRequest { quality: Quality::Draft, ..req }, &cache).unwrap();
    assert_eq!(render_cached(&src, &info, &s, &req, &cache).unwrap().image, render(&src, &info, &s, &req).unwrap().image);
}

#[test]
fn bounded_flag_is_ignored_for_full_frames_and_deep_output() {
    use crate::{OutputDepth, StageCache, render_cached};
    use std::sync::Arc;
    let src = Arc::new(scene());
    let info = SourceInfo::default();
    let mut s = DevelopSettings::default();
    s.effects.dehaze = 30.0;
    s.effects.clarity = 20.0;
    for req in [
        RenderRequest::fit(480, 320),
        RenderRequest { depth: OutputDepth::U16, window: Some(PixelWindow { x: 100, y: 80, w: 121, h: 83 }), ..RenderRequest::fit(960, 640) },
    ] {
        let flagged = RenderRequest { bounded_window: true, ..req };
        assert!(!flagged.is_bounded_window());
        let cache = StageCache::default();
        let actual = render_cached(&src, &info, &s, &flagged, &cache).unwrap();
        let expected = render(&src, &info, &s, &req).unwrap();
        assert_eq!(actual.image, expected.image);
        if let (Some(a), Some(b)) = (actual.deep, expected.deep) {
            assert_eq!(a.to_rgba8(), b.to_rgba8());
        }
        assert!(!cache.is_empty());
    }
}
