//! Sharpening: an unsharp mask at the pixel scale, with Radius (source pixels), Detail (halo
//! damping) and Masking (edge mask), the same at every preview and export size.

use lcc_develop::DevelopSettings;
use lcc_raster::{Rgb32f, Rgba8};

use crate::{Downscale, Quality, RenderRequest, SourceInfo, render};

fn info() -> SourceInfo {
    SourceInfo { raw: true, ..Default::default() }
}

fn render_at(src: &Rgb32f, s: &DevelopSettings, w: usize) -> Rgba8 {
    render_with(src, s, w, Downscale::Smooth)
}

fn render_with(src: &Rgb32f, s: &DevelopSettings, w: usize, downscale: Downscale) -> Rgba8 {
    render(src, &info(), s, &RenderRequest { downscale, ..RenderRequest::fit(w, w) }).image
}

/// RMS difference of the green channel.
fn rms(a: &Rgba8, b: &Rgba8) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let sum: f64 = a.data.iter().zip(&b.data).map(|(p, q)| (p[1] as f64 - q[1] as f64).powi(2)).sum();
    (sum / a.data.len() as f64).sqrt()
}

/// A grating with the given period (px at 512 wide), in scene-linear light.
fn grating(w: usize, period: f32) -> Rgb32f {
    let h = w / 8;
    Rgb32f::from_fn(w, h, |x, _| {
        let k = w as f32 / 512.0;
        let v = 0.18 * (1.0 + 0.4 * (std::f32::consts::TAU * x as f32 / (period * k)).sin());
        [v; 3]
    })
}

/// A scene with a hard edge, fine texture and a gradient, defined in normalized coordinates so it
/// can be sampled at any size.
fn scene(w: usize) -> Rgb32f {
    let h = w * 3 / 4;
    Rgb32f::from_fn(w, h, |x, y| {
        let (u, v) = (x as f32 / w as f32, y as f32 / w as f32);
        let edge = if u > 0.5 { 0.3 } else { 0.08 };
        let tex = 0.03 * (u * 150.0).sin() * (v * 130.0).cos();
        let g = 0.1 * v;
        let l = (edge + tex + g).max(0.001);
        [l, l * 0.9, l * 0.8]
    })
}

fn sharp(amount: f64, radius: f64, detail: f64, masking: f64) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.detail.sharpen_amount = amount;
    s.detail.sharpen_radius = radius;
    s.detail.sharpen_detail = detail;
    s.detail.sharpen_masking = masking;
    s
}

#[test]
fn amount_zero_is_the_identity_whatever_radius_and_detail() {
    let src = scene(256);
    let plain = render_at(&src, &DevelopSettings::default(), 256);
    for (r, d) in [(0.5, 0.0), (1.0, 25.0), (3.0, 100.0)] {
        assert_eq!(render_at(&src, &sharp(0.0, r, d, 0.0), 256).data, plain.data, "radius {r} detail {d}");
    }
}

#[test]
fn larger_radius_boosts_coarser_detail() {
    // gain on a fine (4 px) and a coarse (24 px) grating, for a small and a large radius
    let gain = |period: f32, radius: f64| {
        let src = grating(512, period);
        rms(&render_at(&src, &sharp(80.0, radius, 100.0, 0.0), 512), &render_at(&src, &sharp(0.0, radius, 100.0, 0.0), 512))
    };
    let (fine_small, fine_large) = (gain(4.0, 0.6), gain(4.0, 3.0));
    let (coarse_small, coarse_large) = (gain(24.0, 0.6), gain(24.0, 3.0));
    eprintln!("fine {fine_small} {fine_large}, coarse {coarse_small} {coarse_large}");
    assert!(coarse_large > 2.0 * coarse_small, "coarse: small {coarse_small}, large {coarse_large}");
    assert!(fine_small > coarse_small, "a small radius favours fine detail: {fine_small} vs {coarse_small}");
    // the large radius moves its emphasis to coarser detail
    assert!(coarse_large / fine_large > 2.0 * coarse_small / fine_small, "{coarse_large}/{fine_large} vs {coarse_small}/{fine_small}");
}

#[test]
fn detail_changes_the_output_and_damps_halos() {
    let src = scene(256);
    let lo = render_at(&src, &sharp(80.0, 1.5, 0.0, 0.0), 256);
    let hi = render_at(&src, &sharp(80.0, 1.5, 100.0, 0.0), 256);
    let plain = render_at(&src, &sharp(0.0, 1.5, 0.0, 0.0), 256);
    assert_ne!(lo.data, hi.data);
    // the overshoot next to the hard edge (u = 0.5) shrinks with Detail 0
    let row = 96 * 256;
    let at = |img: &Rgba8, x: usize| img.data[row + x][1] as i32;
    let over = |img: &Rgba8| (118..128).map(|x| (at(img, x) - at(&plain, x)).abs()).max().unwrap_or(0);
    assert!(over(&lo) < over(&hi), "halo {} (detail 0) vs {} (detail 100)", over(&lo), over(&hi));
}

#[test]
fn masking_leaves_flat_areas_alone() {
    // gentle noise on a flat field: unmasked sharpening changes it, a high Masking does not
    let src = Rgb32f::from_fn(128, 96, |x, y| {
        let n = ((x * 7 + y * 13) % 5) as f32 * 0.0015;
        [0.18 + n; 3]
    });
    let plain = render_at(&src, &sharp(0.0, 1.0, 25.0, 0.0), 128);
    let open = render_at(&src, &sharp(100.0, 1.0, 25.0, 0.0), 128);
    let masked = render_at(&src, &sharp(100.0, 1.0, 25.0, 100.0), 128);
    assert!(rms(&open, &plain) > rms(&masked, &plain), "{} vs {}", rms(&open, &plain), rms(&masked, &plain));
}

#[test]
fn sharpening_is_the_same_at_every_size() {
    // the same photo previewed at half size and rendered in full: what sharpening adds (sharpened
    // minus plain) agrees between the preview and the shrunk full render far better than with zero
    let src = scene(512);
    let shrink = |img: &Rgba8| -> Vec<f64> {
        (0..256 * 192)
            .map(|i| {
                let (x, y) = (i % 256 * 2, i / 256 * 2);
                let p = |dx: usize, dy: usize| img.data[(y + dy) * 512 + x + dx][1] as f64;
                (p(0, 0) + p(1, 0) + p(0, 1) + p(1, 1)) / 4.0
            })
            .collect()
    };
    let added = |a: &[f64], b: &[f64]| -> Vec<f64> { a.iter().zip(b).map(|(p, q)| p - q).collect() };
    let norm = |v: &[f64]| (v.iter().map(|x| x * x).sum::<f64>() / v.len() as f64).sqrt();
    let (s, off) = (sharp(60.0, 2.0, 25.0, 0.0), sharp(0.0, 2.0, 25.0, 0.0));
    let full = added(&shrink(&render_at(&src, &s, 512)), &shrink(&render_at(&src, &off, 512)));
    // previews shrink with Mitchell or (full quality) Lanczos-3 as exports do: both agree
    for down in [Downscale::Smooth, Downscale::Sharp] {
        let half_on = render_with(&src, &s, 256, down);
        let half_off = render_with(&src, &off, 256, down);
        let half: Vec<f64> = half_on.data.iter().zip(&half_off.data).map(|(p, q)| p[1] as f64 - q[1] as f64).collect();
        let miss = norm(&added(&full, &half));
        eprintln!("size invariance {down:?}: effect {} (half) {} (full, shrunk), disagreement {miss}", norm(&half), norm(&full));
        assert!(norm(&half) > 0.5, "{down:?}: the sharpening shows at half size");
        assert!(miss < 0.6 * norm(&half), "{down:?}: preview and export disagree: {miss} vs an effect of {}", norm(&half));
    }
}

#[test]
fn a_full_preview_shrinks_like_an_export_once_it_shrinks_a_lot() {
    // a full-quality preview at a quarter of the photo renders as the export's filter does; a draft
    // and a near-1:1 preview keep the soft one
    let src = scene(512);
    let s = sharp(60.0, 1.0, 25.0, 0.0);
    let at = |w: usize, quality: Quality, down: Downscale| {
        render(&src, &info(), &s, &RenderRequest { downscale: down, quality, ..RenderRequest::fit(w, w) }).image.data
    };
    assert_eq!(at(128, Quality::Full, Downscale::Preview), at(128, Quality::Full, Downscale::Sharp));
    assert_eq!(at(128, Quality::Draft, Downscale::Preview), at(128, Quality::Draft, Downscale::Smooth));
    assert_eq!(at(384, Quality::Full, Downscale::Preview), at(384, Quality::Full, Downscale::Smooth));
}

#[test]
fn a_sub_pixel_radius_fades_away() {
    // a 1 px radius on a render 8x smaller than the source is far under a pixel: no sharpening
    let src = scene(512);
    let small = render_at(&src, &sharp(100.0, 1.0, 25.0, 0.0), 64);
    assert_eq!(small.data, render_at(&src, &sharp(0.0, 1.0, 25.0, 0.0), 64).data);
}

#[test]
fn texture_does_not_depend_on_sharpening_settings() {
    let src = scene(256);
    let mut a = DevelopSettings::default();
    a.effects.texture = 50.0;
    let mut b = a.clone();
    b.detail.sharpen_radius = 3.0;
    b.detail.sharpen_detail = 100.0;
    assert_eq!(render_at(&src, &a, 256).data, render_at(&src, &b, 256).data);
}

#[test]
fn local_sharpness_uses_the_sharpen_band() {
    use lcc_develop::{LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
    use lcc_geom::Point;
    let src = scene(256);
    let mut s = DevelopSettings {
        masks: vec![Mask {
            components: vec![MaskComponent {
                anchor: None,
                name: None,
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::Linear { start: Point::new(0.5, 0.0), end: Point::new(0.5, 0.01) },
            }],
            adjust: LocalAdjustments { sharpness: 100.0, ..Default::default() },
            ..Default::default()
        }],
        ..Default::default()
    };
    let plain = render_at(&src, &DevelopSettings::default(), 256);
    let with = render_at(&src, &s, 256);
    assert!(rms(&with, &plain) > 0.05, "{}", rms(&with, &plain));
    s.detail.sharpen_radius = 3.0;
    assert_ne!(render_at(&src, &s, 256).data, with.data, "Radius reaches the local sharpness band");
}

#[test]
fn photos_on_older_processes_keep_their_sharpening() {
    use lcc_develop::{Process, ProcessVersion};
    let src = scene(256);
    for p in [Process::V1, Process::V2, Process::V3] {
        let on = |mut s: DevelopSettings| {
            s.process = p.version();
            render_at(&src, &s, 256)
        };
        // Radius and Detail are new in V4; older photos look as they did
        let base = on(sharp(60.0, 1.0, 25.0, 0.0));
        assert_eq!(base, on(sharp(60.0, 3.0, 100.0, 0.0)), "{p:?}");
        assert!(rms(&base, &on(sharp(0.0, 1.0, 25.0, 0.0))) > 0.1, "{p:?} still sharpens");
    }
    // and the latest renders them
    let mut v4 = sharp(60.0, 1.0, 25.0, 0.0);
    v4.process = ProcessVersion::LATEST;
    assert!(rms(&render_at(&src, &v4, 256), &render_at(&src, &sharp(60.0, 3.0, 100.0, 0.0), 256)) > 0.1);
}

#[test]
fn different_radii_differ_at_upscale() {
    // Regression for PR762: at 2x export scale, Radius 2 and 3 must produce different
    // output. Before the fix, the ceiling clamp in sharpen_sigma made them identical.
    let src = scene(512);
    let s2 = sharp(60.0, 2.0, 25.0, 0.0);
    let s3 = sharp(60.0, 3.0, 25.0, 0.0);
    let r2 = render_at(&src, &s2, 1024);
    let r3 = render_at(&src, &s3, 1024);
    let diff = rms(&r2, &r3);
    assert!(diff > 0.1, "Radius 2 and 3 should differ at 2x scale (got {})", diff);
}

/// The Sharpening views (⌥/Alt on a Sharpening slider) replace the photo with black-and-white
/// images of the band sharpening boosts, whatever the edit: no sharpening, or an old process.
fn view(src: &Rgb32f, s: &DevelopSettings, v: crate::SharpenView) -> Rgba8 {
    render(src, &info(), s, &RenderRequest { overlay: crate::Overlay::Sharpen(v), ..RenderRequest::fit(256, 256) }).image
}

#[test]
fn sharpening_views_are_grey_and_differ_by_view() {
    use crate::{Overlay, SharpenView};
    let src = scene(256);
    // (masked: the edit has Masking up, so its mask view is a picture rather than all white)
    for (s, masked) in [(DevelopSettings::default(), false), (sharp(80.0, 1.0, 25.0, 40.0), true)] {
        let greys: Vec<Vec<u8>> = SharpenView::ALL
            .iter()
            .map(|v| {
                view(&src, &s, *v)
                    .data
                    .iter()
                    .map(|p| {
                        assert_eq!((p[0], p[1]), (p[1], p[2]), "grey");
                        p[0]
                    })
                    .collect()
            })
            .collect();
        // each view is a picture, not one flat tone (how far it spreads depends on the photo's
        // detail, so only a modest range is asked of this smooth scene)
        for (v, g) in SharpenView::ALL.iter().zip(&greys).filter(|(_, _)| masked) {
            let (lo, hi) = g.iter().fold((255u8, 0u8), |(lo, hi), &x| (lo.min(x), hi.max(x)));
            assert!(hi - lo > 24, "{v:?}: {lo}..{hi}");
        }
        // no two views show the same picture
        for (i, a) in greys.iter().enumerate() {
            for b in &greys[i + 1..] {
                assert!(a.iter().zip(b).any(|(&x, &y)| x.abs_diff(y) > 8), "{:?} {:?}", SharpenView::ALL[i], SharpenView::ALL[i + 1]);
            }
        }
    }
    for v in SharpenView::ALL {
        let (kind, value) = Overlay::Sharpen(v).to_parts();
        assert_eq!(Overlay::from_parts(kind, value), Overlay::Sharpen(v), "{v:?}");
    }
    assert_ne!(Overlay::Sharpen(SharpenView::Masking).key(), Overlay::Sharpen(SharpenView::Radius).key());
}

/// Masking's view is the mask the render sharpens by: all white with Masking at 0, and with Masking
/// up, white on the edge and dark over the flat texture.
#[test]
fn masking_view_is_the_edge_mask() {
    use crate::SharpenView;
    let src = scene(256);
    let off = view(&src, &sharp(80.0, 1.0, 25.0, 0.0), SharpenView::Masking);
    assert!(off.data.iter().all(|p| p[0] == 255), "Masking 0 sharpens everywhere");
    let on = view(&src, &sharp(80.0, 1.0, 25.0, 100.0), SharpenView::Masking);
    let at = |img: &Rgba8, u: f32, v: f32| img.data[(v * img.height as f32) as usize * img.width + (u * img.width as f32) as usize][0];
    assert!(at(&on, 0.5, 0.5) > 200, "the hard edge is sharpened: {}", at(&on, 0.5, 0.5));
    assert!(at(&on, 0.9, 0.5) < 60, "the flat gradient is left alone: {}", at(&on, 0.9, 0.5));
}

/// An edit too old to sharpen with its own band still gets the views.
#[test]
fn sharpening_views_need_no_v4_sharpening_edit() {
    use crate::SharpenView;
    let src = scene(256);
    let mut old = sharp(0.0, 1.0, 25.0, 80.0);
    old.process = lcc_develop::Process::V1.version();
    let img = view(&src, &old, SharpenView::Masking);
    assert!(img.data.iter().any(|p| p[0] < 60), "the mask is shown, not the photo");
    // the settings the caller handed in are not changed
    assert_eq!(old.detail.sharpen_amount, 0.0);
}
