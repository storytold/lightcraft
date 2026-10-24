//! Tiled GPU renders (`lightcraft_gpu::render_tiled`, and whole renders over the device's buffer
//! limit) against the CPU's whole render: the same bounds as `equivalence.rs` (mean |Δ| < 0.5 LSB,
//! max |Δ| ≤ 3 LSB). Skips (passes with a note) when no GPU adapter exists.

use std::sync::Arc;

use lightcraft_develop::{BrushStroke, DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape, RedEye};
use lightcraft_geom::{Orientation, Point, Rect};
use lightcraft_gpu::Fault;
use lightcraft_pipeline::tiled::TileOptions;
use lightcraft_pipeline::{RenderRequest, SourceInfo, render};
use lightcraft_raster::{Rgb32f, Rgba8};

const MEAN_LSB: f64 = 0.5;
const MAX_LSB: u8 = 3;

fn gpu() -> bool {
    let ok = lightcraft_gpu::available();
    if !ok {
        eprintln!("skipped: no GPU adapter");
    }
    ok
}

fn diff(a: &Rgba8, b: &Rgba8) -> (f64, u8) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let (mut sum, mut max) = (0u64, 0u8);
    for (p, q) in a.data.iter().zip(&b.data) {
        for c in 0..3 {
            let d = p[c].abs_diff(q[c]);
            sum += d as u64;
            max = max.max(d);
        }
    }
    (sum as f64 / (a.data.len() * 3) as f64, max)
}

fn source(w: usize, h: usize) -> Arc<Rgb32f> {
    let img = lightcraft_scenes::demo_library()[0].render(w, h);
    Arc::new(Rgb32f::from_fn(w, h, |x, y| {
        let mut c = img.get(x, y);
        // a red disc for the red-eye case
        let (dx, dy) = (x as f32 - 0.7 * w as f32, y as f32 - 0.55 * h as f32);
        if dx * dx + dy * dy < 36.0 {
            c = [0.5, 0.05, 0.05];
        }
        c
    }))
}

fn mask(id: u32, shape: MaskShape, adjust: LocalAdjustments) -> Mask {
    Mask { id, components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }], adjust, ..Default::default() }
}

fn cases() -> Vec<(&'static str, DevelopSettings)> {
    let mut v = vec![("default", DevelopSettings::default())];
    let mut s = DevelopSettings::default();
    s.light.exposure = 0.4;
    s.light.highlights = -60.0;
    s.light.shadows = 50.0;
    s.effects.clarity = 40.0;
    s.effects.texture = 30.0;
    s.detail.sharpen_amount = 60.0;
    v.push(("tone", s.clone()));
    s.effects.dehaze = 45.0;
    s.vignette.amount = -40.0;
    s.grain.amount = 30.0;
    v.push(("dehaze vignette grain", s));
    let mut s = DevelopSettings::default();
    s.detail.nr_luminance = 60.0;
    s.detail.nr_color = 50.0;
    s.optics.defringe_purple_amount = 6.0;
    s.red_eye.push(RedEye { center: Point::new(0.7, 0.55), rx: 0.02, ry: 0.02, ..Default::default() });
    v.push(("noise defringe red eye", s));
    let mut s = DevelopSettings::default();
    s.crop.geometry.rect = Rect::new(0.1, 0.05, 0.85, 0.9);
    s.crop.geometry.angle = 7.0;
    s.crop.flip_h = true;
    s.orientation = Orientation::Rotate90;
    s.optics.distortion = 30.0;
    s.light.shadows = 40.0;
    v.push(("geometry", s));
    let mut s = DevelopSettings::default();
    let adj = |exposure, clarity, noise| LocalAdjustments { exposure, clarity, noise, ..Default::default() };
    s.masks.push(mask(1, MaskShape::Linear { start: Point::new(0.2, 0.0), end: Point::new(0.5, 0.6) }, adj(0.8, 0.0, 0.0)));
    s.masks.push(mask(
        2,
        MaskShape::Radial { center: Point::new(0.6, 0.5), rx: 0.2, ry: 0.15, angle: 20.0, feather: 50.0, invert: false },
        adj(0.0, 50.0, 0.0),
    ));
    let stroke = BrushStroke { points: vec![Point::new(0.1, 0.8), Point::new(0.4, 0.7), Point::new(0.7, 0.9)], size: 0.04, ..Default::default() };
    s.masks.push(mask(3, MaskShape::Brush { strokes: vec![stroke] }, adj(-0.6, 0.0, 60.0)));
    s.masks.push(mask(4, MaskShape::Sky, adj(-0.5, 0.0, 0.0)));
    v.push(("masks", s));
    v
}

#[test]
fn tiled_gpu_renders_match_the_cpu() {
    if !gpu() {
        return;
    }
    let (w, h) = (900, 600);
    let src = source(w, h);
    let info = SourceInfo { raw: true, ..Default::default() };
    for (name, s) in cases() {
        let req = RenderRequest::fit(w, w);
        let cpu = render(&src, &info, &s, &req).image;
        let opts = TileOptions { core: Some(160), ..Default::default() };
        let gpu = lightcraft_gpu::render_tiled(&src, &info, &s, &req, &opts)
            .unwrap_or_else(|| panic!("{name}: gpu tiled render ({:?})", lightcraft_gpu::last_fallback()));
        let (mean, max) = diff(&cpu, &gpu.image);
        eprintln!("{name:<24} {}x{}  mean {mean:.4}  max {max}", cpu.width, cpu.height);
        assert!(mean < MEAN_LSB && max <= MAX_LSB, "{name}: mean {mean:.4} LSB, max {max} LSB");
    }
}

/// A whole render over the device's storage-buffer limit renders in tiles on the GPU instead of
/// falling back to the CPU.
#[test]
fn renders_over_the_buffer_limit_are_tiled() {
    if !gpu() {
        return;
    }
    let (w, h) = (3000, 2000);
    let src = source(w, h);
    let info = SourceInfo { raw: true, ..Default::default() };
    let s = &cases()[2].1;
    let req = RenderRequest::fit(w, h);
    let cpu = render(&src, &info, s, &req).image;
    // 16 MiB: a 6 MP RGB buffer is 69 MiB
    lightcraft_gpu::inject_fault(Fault::Limit(16 << 20));
    let gpu = lightcraft_gpu::render(&src, &info, s, &req, None).unwrap_or_else(|| panic!("over the limit: {:?}", lightcraft_gpu::last_fallback()));
    let (mean, max) = diff(&cpu, &gpu.image);
    eprintln!("over the limit {w}x{h}  mean {mean:.4}  max {max}");
    assert!(mean < MEAN_LSB && max <= MAX_LSB, "mean {mean:.4} LSB, max {max} LSB");
}

#[test]
fn settings_that_cant_be_tiled_are_declined() {
    if !gpu() {
        return;
    }
    let src = source(400, 300);
    let mut s = DevelopSettings::default();
    s.masks.push(mask(1, MaskShape::Subject, LocalAdjustments { exposure: 1.0, ..Default::default() }));
    assert!(lightcraft_gpu::render_tiled(&src, &SourceInfo::default(), &s, &RenderRequest::fit(400, 300), &TileOptions::default()).is_none());
    assert!(lightcraft_gpu::available());
}
