use std::sync::Arc;

use lightcraft_develop::DevelopSettings;
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
