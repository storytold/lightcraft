//! NR physical-radius regressions through the actual render and cache paths.
use std::sync::Arc;

use lightcraft_develop::DevelopSettings;
use lightcraft_geom::Rect;
use lightcraft_raster::Rgb32f;

use crate::{DeepSamples, OutputDepth, RenderRequest, SourceInfo, StageCache, render, render_cached};

fn source(w: usize, h: usize) -> Rgb32f {
    Rgb32f::from_fn(w, h, |x, y| {
        let a = (x as f32 * 0.39).sin() * 0.05;
        let b = (y as f32 * 0.51).cos() * 0.025;
        [0.18 + a + b, 0.18 - a * 0.3 + b, 0.18 - a + b * 0.4]
    })
}

fn settings(luminance: f64, color: f64) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.detail.nr_luminance = luminance;
    s.detail.nr_color = color;
    s.detail.nr_color_detail = 0.0;
    s.detail.nr_color_smoothness = 50.0;
    s
}

fn request(w: usize, h: usize) -> RenderRequest {
    RenderRequest { depth: OutputDepth::U16, ..RenderRequest::fit(w, h) }
}

fn pixels(result: crate::Rendered) -> Vec<u16> {
    let DeepSamples::U16(pixels) = result.deep.unwrap().samples else { panic!("expected RGB16") };
    pixels
}

#[test]
fn native_crop_keeps_noise_reduction_footprint() {
    let src = source(512, 192);
    let info = SourceInfo { native_long: 512, ..Default::default() };
    for (lum, col) in [(0.0, 75.0), (75.0, 0.0), (75.0, 75.0)] {
        let mut s = settings(lum, col);
        let full = pixels(render(&src, &info, &s, &request(512, 192)));
        s.crop.geometry.rect = Rect::new(0.25, 0.0, 0.75, 1.0);
        let cropped = pixels(render(&src, &info, &s, &request(256, 192)));
        // Same original pixels at 1:1; stay farther from crop edges than every filter's support.
        // Two native LSB allow floating running-sum roundoff from starting at another x.
        let mut max_difference = 0;
        for y in 48..144 {
            for x in 64..192 {
                for channel in 0..3 {
                    max_difference = max_difference.max(full[(y * 512 + x + 128) * 3 + channel].abs_diff(cropped[(y * 256 + x) * 3 + channel]));
                }
            }
        }
        assert!(max_difference <= 2, "crop changed NR at 1:1: lum={lum}, color={col}, max native LSB={max_difference}");
    }
}

#[test]
fn native_source_metadata_changes_nr_and_invalidates_cached_linear_stage() {
    let src = Arc::new(source(256, 128));
    let req = request(256, 128);
    for (lum, col) in [(0.0, 75.0), (75.0, 0.0)] {
        let s = settings(lum, col);
        let cache = StageCache::default();
        let mut results = Vec::new();
        for native_long in [256, 512, 256] {
            let info = SourceInfo { native_long, ..Default::default() };
            let warm = pixels(render_cached(&src, &info, &s, &req, &cache));
            let cold = pixels(render(&src, &info, &s, &req));
            assert_eq!(warm, cold, "stale NR cache at native_long={native_long}, lum={lum}, color={col}");
            results.push(warm);
        }
        assert!(results[0] != results[1], "prior decoder reduction must affect the NR footprint");
        assert_eq!(results[0], results[2]);
    }
}

#[test]
fn unknown_native_size_keeps_full_size_nr_and_nr_off_ignores_metadata() {
    let src = source(256, 128);
    let req = request(256, 128);
    let unknown = SourceInfo::default();
    let native = SourceInfo { native_long: 256, ..Default::default() };
    for (lum, col) in [(0.0, 75.0), (75.0, 0.0), (75.0, 75.0)] {
        let s = settings(lum, col);
        assert_eq!(pixels(render(&src, &unknown, &s, &req)), pixels(render(&src, &native, &s, &req)));
    }
    let off = DevelopSettings::default();
    assert_eq!(pixels(render(&src, &native, &off, &req)), pixels(render(&src, &SourceInfo { native_long: 512, ..native }, &off, &req)));
}
