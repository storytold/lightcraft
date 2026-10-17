//! Tiled rendering matches the whole-image render (within one 8-bit step).

use lightcraft_develop::{BrushStroke, DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape, RedEye};
use lightcraft_geom::{Orientation, Point, Rect};
use lightcraft_raster::Rgb32f;

use crate::tiled::{TileOptions, render_tiled};
use crate::{DeepSamples, OutputDepth, RenderRequest, SourceInfo, render};

const W: usize = 600;
const H: usize = 400;

/// A deterministic hash in −1..1.
fn noise(x: usize, y: usize) -> f32 {
    let mut v = (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841);
    v ^= v >> 13;
    v = v.wrapping_mul(0x5bd1_e995);
    v ^= v >> 15;
    (v & 0xffff) as f32 / 32768.0 - 1.0
}

/// A demo photo with some noise, a hazy blue top and a red disc (red eye).
fn source() -> Rgb32f {
    let img = lightcraft_scenes::demo_library()[0].render(W, H);
    Rgb32f::from_fn(W, H, |x, y| {
        let mut c = img.get(x, y).map(|v| v * (1.0 + 0.08 * noise(x, y)));
        if y < H / 5 {
            c = [c[0] * 0.6 + 0.2, c[1] * 0.6 + 0.25, c[2] * 0.6 + 0.4];
        }
        let (dx, dy) = (x as f32 - 420.0, y as f32 - 220.0);
        if dx * dx + dy * dy < 36.0 {
            c = [0.5, 0.05, 0.05];
        }
        c
    })
}

fn mask(id: u32, shape: MaskShape, adjust: LocalAdjustments) -> Mask {
    Mask { id, components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }], adjust, ..Default::default() }
}

/// Settings exercising every tiled stage.
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
    s.optics.defringe_green_amount = 3.0;
    s.red_eye.push(RedEye { center: Point::new(420.0 / W as f64, 220.0 / H as f64), rx: 0.02, ry: 0.02, ..Default::default() });
    v.push(("noise defringe red eye", s));
    let mut s = DevelopSettings::default();
    s.crop.geometry.rect = Rect::new(0.1, 0.05, 0.85, 0.9);
    s.crop.geometry.angle = 7.0;
    s.crop.flip_h = true;
    s.light.shadows = 40.0;
    v.push(("crop straighten flip", s));
    let mut s = DevelopSettings::default();
    s.optics.distortion = 40.0;
    s.geometry.vertical = -25.0;
    s.geometry.rotate = 2.0;
    s.effects.dehaze = -30.0;
    v.push(("warp", s));
    let mut s = DevelopSettings::default();
    let adj = |exposure, clarity, noise, moire| LocalAdjustments { exposure, clarity, noise, moire, ..Default::default() };
    s.masks.push(mask(1, MaskShape::Linear { start: Point::new(0.2, 0.0), end: Point::new(0.5, 0.6) }, adj(0.8, 0.0, 0.0, 0.0)));
    s.masks.push(mask(
        2,
        MaskShape::Radial { center: Point::new(0.6, 0.5), rx: 0.2, ry: 0.15, angle: 20.0, feather: 50.0, invert: false },
        adj(0.0, 50.0, 0.0, 0.0),
    ));
    let stroke = BrushStroke { points: vec![Point::new(0.1, 0.8), Point::new(0.4, 0.7), Point::new(0.7, 0.9)], size: 0.04, ..Default::default() };
    s.masks.push(mask(3, MaskShape::Brush { strokes: vec![stroke] }, adj(-0.6, 0.0, 60.0, 40.0)));
    s.masks.push(mask(4, MaskShape::Sky, adj(-0.5, 0.0, 0.0, 0.0)));
    s.masks.push(mask(5, MaskShape::LuminanceRange { lo: 0.6, hi: 0.9, lo_feather: 0.1, hi_feather: 0.1 }, adj(0.0, 0.0, 0.0, 0.0)));
    v.push(("masks", s));
    v
}

fn max_diff(a: &[[u8; 4]], b: &[[u8; 4]]) -> (u8, usize) {
    let mut worst = (0, 0);
    for (i, (p, q)) in a.iter().zip(b).enumerate() {
        let d = (0..3).map(|k| p[k].abs_diff(q[k])).max().unwrap_or(0);
        if d > worst.0 {
            worst = (d, i);
        }
    }
    worst
}

fn check(name: &str, src: &Rgb32f, s: &DevelopSettings, req: &RenderRequest, core: usize) {
    let whole = render(src, &SourceInfo::default(), s, req);
    let tiled = render_tiled(src, &SourceInfo::default(), s, req, &TileOptions { core: Some(core), ..Default::default() })
        .unwrap_or_else(|| panic!("{name}: not tiled"));
    assert_eq!((tiled.image.width, tiled.image.height), (whole.image.width, whole.image.height), "{name}");
    let (d, i) = max_diff(&whole.image.data, &tiled.image.data);
    let w = whole.image.width;
    assert!(d <= 1, "{name} (core {core}): differs by {d} at {},{}", i % w, i / w);
}

#[test]
fn tiled_matches_whole_render() {
    let src = source();
    let req = RenderRequest::fit(W, H);
    for (name, s) in cases() {
        check(name, &src, &s, &req, 96);
    }
}

#[test]
fn tiled_matches_whole_render_at_odd_tile_sizes() {
    let src = source();
    let req = RenderRequest::fit(W, H);
    let s = &cases()[2].1;
    for core in [37, 250, 1000] {
        check("dehaze vignette grain", &src, s, &req, core);
    }
}

#[test]
fn tiled_matches_whole_render_in_every_orientation() {
    let src = source();
    let req = RenderRequest::fit(W, W);
    use Orientation::*;
    for o in [FlipH, Rotate180, FlipV, Transpose, Rotate90, Transverse, Rotate270] {
        let mut s = cases()[4].1.clone();
        s.orientation = o;
        check(&format!("{o:?}"), &src, &s, &req, 80);
    }
}

#[test]
fn tiled_deep_output_matches() {
    let src = source();
    let s = &cases()[1].1;
    for depth in [OutputDepth::U16, OutputDepth::F32Linear] {
        let req = RenderRequest { depth, ..RenderRequest::fit(W, H) };
        let whole = render(&src, &SourceInfo::default(), s, &req).deep.expect("deep");
        let tiled = render_tiled(&src, &SourceInfo::default(), s, &req, &TileOptions { core: Some(100), ..Default::default() })
            .and_then(|r| r.deep)
            .expect("tiled deep");
        match (&whole.samples, &tiled.samples) {
            (DeepSamples::U16(a), DeepSamples::U16(b)) => {
                let d = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
                assert!(d <= 256, "u16 differs by {d}");
            }
            (DeepSamples::F32(a), DeepSamples::F32(b)) => {
                let d = a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max);
                assert!(d <= 2e-3, "f32 differs by {d}");
            }
            _ => panic!("sample formats differ"),
        }
    }
}

#[test]
fn far_reaching_settings_are_not_tiled() {
    let src = source();
    let req = RenderRequest::fit(W, H);
    let opts = TileOptions::default();
    let mut s = DevelopSettings::default();
    s.masks.push(mask(1, MaskShape::Subject, LocalAdjustments { exposure: 1.0, ..Default::default() }));
    assert!(render_tiled(&src, &SourceInfo::default(), &s, &req, &opts).is_none());
    let mut s = cases()[6].1.clone();
    s.masks[0].refine = 30.0;
    assert!(render_tiled(&src, &SourceInfo::default(), &s, &req, &opts).is_none());
    // a downscaled render needs the prefilter
    assert!(render_tiled(&src, &SourceInfo::default(), &DevelopSettings::default(), &RenderRequest::fit(200, 200), &opts).is_none());
}
