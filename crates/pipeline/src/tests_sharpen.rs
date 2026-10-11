use std::sync::Arc;

use lightcraft_develop::{DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape, ProcessVersion};
use lightcraft_geom::Point;
use lightcraft_raster::Rgb32f;

use crate::{DeepSamples, OutputDepth, RenderRequest, SourceInfo, StageCache, render, render_cached};

fn edge(width: usize) -> Rgb32f {
    Rgb32f::from_fn(width, 32, |x, _| [if x < width / 2 { 0.18 } else { 0.36 }; 3])
}

fn settings(radius: f64) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.detail.sharpen_amount = 80.0;
    s.detail.sharpen_radius = radius;
    s
}

fn deep(src: &Rgb32f, s: &DevelopSettings) -> Vec<u16> {
    let req = RenderRequest { depth: OutputDepth::U16, ..RenderRequest::fit(src.width, src.height) };
    let DeepSamples::U16(pixels) = render(src, &SourceInfo::default(), s, &req).deep.unwrap().samples else { panic!("expected RGB16") };
    pixels
}

fn local_sharpening_fixture() -> (Arc<Rgb32f>, DevelopSettings) {
    let src = Arc::new(Rgb32f::from_fn(240, 120, |x, _| [0.18 * (1.0 + 0.12 * (x as f32 * 1.1).sin()); 3]));
    let mut s = DevelopSettings::default();
    s.masks.push(Mask {
        id: 1,
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Linear { start: Point::new(0.45, 0.5), end: Point::new(0.46, 0.5) },
        }],
        adjust: LocalAdjustments { sharpness: 100.0, ..Default::default() },
        ..Default::default()
    });
    (src, s)
}

fn max_difference(a: &[u16], b: &[u16]) -> u16 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
}

#[test]
fn local_sharpening_responds_to_global_masking() {
    let (src, mut s) = local_sharpening_fixture();
    let req = RenderRequest { depth: OutputDepth::U16, ..RenderRequest::fit(src.width, src.height) };
    let info = SourceInfo::default();
    let cache = StageCache::default();
    for process in [ProcessVersion::V1, ProcessVersion::V2] {
        s.process = process;
        s.detail.sharpen_masking = 0.0;
        let unmasked = deep(&src, &s);
        let plain = deep(&src, &DevelopSettings { process, ..Default::default() });
        let unmasked_response = max_difference(&unmasked, &plain);
        assert!(unmasked_response > 100, "fixture must exercise local sharpening in {process:?}");
        // Masking also suppresses local sharpening when global Amount is zero.
        for masking in [100.0, 50.0, 0.0] {
            s.detail.sharpen_masking = masking;
            let cold = deep(&src, &s);
            let DeepSamples::U16(warm) = render_cached(&src, &info, &s, &req, &cache).deep.unwrap().samples else { panic!("expected RGB16") };
            if masking > 0.0 {
                assert!(max_difference(&plain, &cold) < unmasked_response, "{process:?}: Masking {masking} must suppress local sharpening");
            } else {
                assert_eq!(max_difference(&unmasked, &cold), 0, "{process:?}: returning Masking to zero must restore local sharpening");
            }
            assert_eq!(max_difference(&cold, &warm), 0, "{process:?}: cached local sharpening at Masking {masking}");
        }
    }
}

#[test]
fn local_sharpening_uses_effective_masking_through_detail_bypass() {
    let (src, mut s) = local_sharpening_fixture();
    let req = RenderRequest { depth: OutputDepth::U16, ..RenderRequest::fit(src.width, src.height) };
    let info = SourceInfo::default();
    let cache = StageCache::default();
    for process in [ProcessVersion::V1, ProcessVersion::V2] {
        s.process = process;
        s.set_section_enabled("detail", true);
        s.detail.sharpen_masking = 0.0;
        let unmasked = deep(&src, &s);
        s.detail.sharpen_masking = 100.0;
        let masked = deep(&src, &s);
        assert!(max_difference(&unmasked, &masked) > 100, "fixture must exercise Masking of local sharpening in {process:?}");
        let saved_mask = s.masks.clone();
        let saved_detail = s.detail;
        for enabled in [true, false, true] {
            s.set_section_enabled("detail", enabled);
            let cold = deep(&src, &s);
            let DeepSamples::U16(warm) = render_cached(&src, &info, &s, &req, &cache).deep.unwrap().samples else { panic!("expected RGB16") };
            let expected = if enabled { &masked } else { &unmasked };
            assert_eq!(max_difference(expected, &cold), 0, "{process:?}: Detail enabled={enabled}: incorrect effective Masking");
            assert_eq!(max_difference(&cold, &warm), 0, "{process:?}: stale local sharpening cache when Detail enabled={enabled}");
            assert_eq!(s.masks, saved_mask, "bypass must preserve saved mask adjustments");
            assert_eq!(s.detail, saved_detail, "bypass must preserve saved Detail adjustments");
        }
    }
}

#[test]
fn global_masking_still_suppresses_global_sharpening() {
    let (src, _) = local_sharpening_fixture();
    let mut s = settings(1.0);
    for process in [ProcessVersion::V1, ProcessVersion::V2] {
        s.process = process;
        s.detail.sharpen_masking = 0.0;
        let plain = deep(&src, &DevelopSettings { process, ..Default::default() });
        let unmasked = deep(&src, &s);
        assert!(max_difference(&plain, &unmasked) > 100, "fixture must exercise global sharpening in {process:?}");
        s.detail.sharpen_masking = 100.0;
        let masked = deep(&src, &s);
        assert!(
            max_difference(&plain, &masked) < max_difference(&plain, &unmasked),
            "{process:?}: global Masking must retain its global-sharpening effect"
        );
    }
}

#[test]
fn radius_changes_edge_width_and_half_pixel_radius_is_active() {
    let src = edge(128);
    let mut off = settings(1.0);
    off.detail.sharpen_amount = 0.0;
    let (base, small, large) = (deep(&src, &off), deep(&src, &settings(0.5)), deep(&src, &settings(3.0)));
    let at = |a: &[u16], x: usize| a[(16 * 128 + x) * 3] as i32;
    assert!(at(&small, 64) - at(&base, 64) > 100, "half-pixel radius must sharpen the edge");
    assert!((at(&small, 67) - at(&base, 67)).abs() <= 1, "small radius stays near the edge");
    assert!(at(&large, 67) - at(&base, 67) > 100, "larger radius widens the halo");
    assert_ne!(small, large, "Radius must change the native output");
}

#[test]
fn native_radius_does_not_grow_with_canvas_size() {
    for radius in [0.5, 1.0, 3.0] {
        let a = deep(&edge(128), &settings(radius));
        let b = deep(&edge(1024), &settings(radius));
        for offset in -12isize..12 {
            let x = (64 + offset) as usize;
            let xx = (512 + offset) as usize;
            assert_eq!(a[(16 * 128 + x) * 3], b[(16 * 1024 + xx) * 3], "radius {radius}, offset {offset}");
        }
    }
}

#[test]
fn radius_leaves_texture_unchanged_when_global_sharpening_is_off() {
    let src = edge(128);
    let mut s = settings(0.5);
    s.detail.sharpen_amount = 0.0;
    s.effects.texture = 70.0;
    let a = deep(&src, &s);
    s.detail.sharpen_radius = 3.0;
    assert_eq!(a, deep(&src, &s));
}

#[test]
fn radius_cache_tracks_slider_and_native_source_metadata() {
    let src = Arc::new(edge(128));
    let cache = StageCache::default();
    let req = RenderRequest::fit(128, 32);
    let mut results = Vec::new();
    for (radius, native_long) in [(0.5, 128), (3.0, 128), (3.0, 256), (1.0, 128)] {
        let info = SourceInfo { native_long, ..Default::default() };
        let s = settings(radius);
        let cached = render_cached(&src, &info, &s, &req, &cache).image;
        assert_eq!(cached.data, render(&src, &info, &s, &req).image.data);
        results.push(cached.data);
    }
    assert_ne!(results[0], results[1]);
    assert_ne!(results[1], results[2], "retaining prior decoder reduction changes the radius footprint");
}

#[test]
fn crop_magnification_and_source_proxies_preserve_radius_units() {
    let src = edge(128);
    let mut s = settings(1.0);
    let info = SourceInfo { native_long: 512, ..Default::default() };
    let req = RenderRequest::fit(128, 128);
    let p = crate::plan(&src, &info, &s, &req);
    let sigma = crate::local::plane_sigmas(&p.settings, p.px_per_long, p.native_long, req.quality).sharpen.unwrap();
    assert!((sigma - 0.25).abs() < 1e-6);
    s.crop.geometry.rect = lightcraft_geom::Rect::new(0.25, 0.0, 0.75, 1.0);
    let p = crate::plan(&src, &info, &s, &req);
    let sigma = crate::local::plane_sigmas(&p.settings, p.px_per_long, p.native_long, req.quality).sharpen.unwrap();
    assert!((sigma - 0.5).abs() < 1e-6, "crop magnification doubles pixel radius: {sigma}");
}
