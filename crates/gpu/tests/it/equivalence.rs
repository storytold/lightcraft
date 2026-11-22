//! CPU ↔ GPU equivalence: the same settings rendered by the CPU pipeline (the reference) and by
//! `lcc_gpu::render`, compared in 8-bit sRGB.
//!
//! Bounds (documented in `docs/gpu-pipeline.md`): mean |Δ| < 0.5 LSB and max |Δ| ≤ 3 LSB per
//! channel for every case. Skips (passes with a note) when no GPU adapter exists, e.g. on CI.
//!
//! On a software adapter (`<PREFIX>_GPU_SOFTWARE=1`: lavapipe on Linux, WARP on Windows; the
//! nightly job in `docs/ci.md`) the per-pixel bounds are wider ([`SOFT_MEAN_LSB`], [`SOFT_MAX_LSB`]):
//! those rasterizers round transcendental functions and fused multiply-adds differently from
//! hardware, but a kernel that is wrong still moves far more than that.

use std::sync::Arc;

use lcc_develop::{BrushStroke, DevelopSettings, Mask, MaskComponent, MaskOp, MaskShape, Process, Spot, Treatment, VignetteStyle, WbMode, Wheel};
use lcc_geom::{Orientation, Point, Rect};
use lcc_pipeline::{Quality, RenderRequest, SourceInfo, StageCache, render};
use lcc_raster::{Rgb32f, Rgba8};

const HW_MEAN_LSB: f64 = 0.5;
const HW_MAX_LSB: u8 = 3;
/// The bounds on a software adapter.
const SOFT_MEAN_LSB: f64 = 1.0;
const SOFT_MAX_LSB: u8 = 6;

/// Mean |Δ| bound for the adapter in use.
fn mean_lsb() -> f64 {
    if lcc_gpu::software_adapter() { SOFT_MEAN_LSB } else { HW_MEAN_LSB }
}

/// Max |Δ| bound for the adapter in use.
fn max_lsb() -> u8 {
    if lcc_gpu::software_adapter() { SOFT_MAX_LSB } else { HW_MAX_LSB }
}

fn gpu() -> bool {
    let ok = lcc_gpu::available();
    if !ok {
        // no adapter (CI) skips; an adapter whose kernels don't build is a bug in them
        let why = lcc_gpu::unavailable_reason().unwrap_or_default();
        assert!(!why.contains("kernels failed to build"), "{why}");
        lcc_testkit::skip!(Gpu, "no GPU adapter");
    }
    // which card the comparisons ran on (`<PREFIX>_GPU_ADAPTER` picks one on a machine with two)
    static NAMED: std::sync::Once = std::sync::Once::new();
    NAMED.call_once(|| eprintln!("GPU equivalence tests on {}", lcc_gpu::adapter_name().unwrap_or_default()));
    // Planned from the device, the joint colour NR's bands follow the renders in flight: with these
    // tests rendering side by side, one render of a pair could run that step on the device and the
    // other on the CPU (equal only to within an 8-bit level), and the cached / fitted comparisons
    // here are exact. A pinned budget gives every render the same plan.
    lcc_gpu::set_nr_band_budget(Some(512 << 20));
    ok
}

#[test]
fn camera_tone_and_relative_wb() {
    if !gpu() {
        return;
    }
    let src = scene(2, 320, 240);
    let curve = lcc_pipeline::tone::CameraTone::new(std::array::from_fn(|i| {
        let x = 0.004 * 1.18f32.powi(i as i32);
        [x, 1.0 - (-2.0 * x).exp()]
    }))
    .unwrap();
    let info = SourceInfo { raw: true, relative_wb: true, camera_tone: Some(curve), ..Default::default() };
    let mut s = DevelopSettings::default();
    check("camera tone neutral", &src, &info, &s, &RenderRequest::fit(320, 240));
    s.light.exposure = 1.0;
    s.light.contrast = 40.0;
    s.wb.mode = WbMode::Custom;
    s.wb.temp = 8000.0;
    check("camera tone edited", &src, &info, &s, &RenderRequest::fit(320, 240));
    // each process's shoulder past the last knot, with highlights lifted into it (upstream #3783)
    for p in Process::ALL {
        let mut lifted = DevelopSettings { process: p.version(), ..DevelopSettings::default() };
        lifted.light.exposure = 2.0;
        check(&format!("camera tone {p:?} exposure +2"), &src, &info, &lifted, &RenderRequest::fit(320, 240));
    }
    // a camera chroma curve: richer shadows, highlights bleached toward white
    let curve = curve.with_chroma([1.4, 1.3, 1.1, 1.0, 0.7, 0.4, 0.25, 0.2]).unwrap();
    let info = SourceInfo { camera_tone: Some(curve), ..info };
    check("camera chroma curve", &src, &info, &DevelopSettings::default(), &RenderRequest::fit(320, 240));
    check("camera chroma curve edited", &src, &info, &s, &RenderRequest::fit(320, 240));
}

fn scene(i: usize, w: usize, h: usize) -> Arc<Rgb32f> {
    Arc::new(lcc_scenes::demo_library()[i].render(w, h))
}

/// (mean |Δ|, max |Δ|, share of channels with |Δ| > 1).
fn diff(a: &Rgba8, b: &Rgba8) -> (f64, u8, f64) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let (mut sum, mut max, mut over) = (0u64, 0u8, 0u64);
    for (p, q) in a.data.iter().zip(&b.data) {
        for c in 0..3 {
            let d = p[c].abs_diff(q[c]);
            sum += d as u64;
            max = max.max(d);
            over += (d > 1) as u64;
        }
    }
    let n = (a.data.len() * 3) as f64;
    (sum as f64 / n, max, over as f64 / n)
}

fn check(name: &str, src: &Arc<Rgb32f>, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest) -> (f64, u8) {
    let cpu = render(src, info, s, req).image;
    let gpu = lcc_gpu::render(src, info, s, req, None).unwrap_or_else(|| panic!("gpu render ({:?})", lcc_gpu::last_fallback()));
    let (mean, max, over) = diff(&cpu, &gpu.image);
    eprintln!("{name:<28} {}x{}  mean {mean:.4}  max {max}  >1: {:.4}%", cpu.width, cpu.height, over * 100.0);
    assert!(mean < mean_lsb() && max <= max_lsb(), "{name}: mean {mean:.4} LSB, max {max} LSB");
    (mean, max)
}

/// [`check`] for a healed spot: a few pixels may differ by up to 8 LSB, the mean must stay tiny.
fn check_spot(name: &str, src: &Arc<Rgb32f>, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest) {
    let cpu = render(src, info, s, req).image;
    let gpu = lcc_gpu::render(src, info, s, req, None).unwrap_or_else(|| panic!("gpu render ({:?})", lcc_gpu::last_fallback()));
    let (mean, max, over) = diff(&cpu, &gpu.image);
    eprintln!("{name:<28} {}x{}  mean {mean:.4}  max {max}  >1: {:.4}%", cpu.width, cpu.height, over * 100.0);
    assert!(mean < mean_lsb() / 10.0 && max <= 8, "{name}: mean {mean:.4} LSB, max {max} LSB");
}

/// [`check`] for gamut-warning renders: a warning is a hard decision at the gamut boundary, so a
/// pixel sitting exactly on it may be painted by one renderer and not the other (float rounding).
/// Such flips are allowed for at most 0.01 % of the pixels; everything else is held to [`check`]'s bounds.
fn check_warned(name: &str, src: &Arc<Rgb32f>, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest) {
    let cpu = render(src, info, s, req).image;
    let gpu = lcc_gpu::render(src, info, s, req, None).unwrap_or_else(|| panic!("gpu render ({:?})", lcc_gpu::last_fallback()));
    let (mean, _, over) = diff(&cpu, &gpu.image);
    let flips = cpu.data.iter().zip(&gpu.image.data).filter(|(p, q)| (0..3).any(|c| p[c].abs_diff(q[c]) > max_lsb())).count();
    let share = flips as f64 / cpu.data.len() as f64;
    eprintln!("{name:<28} {}x{}  mean {mean:.4}  >1: {:.4}%  boundary flips {flips}", cpu.width, cpu.height, over * 100.0);
    assert!(mean < mean_lsb() && share <= 1e-4, "{name}: mean {mean:.4} LSB, {flips} pixels differ by more than {} LSB", max_lsb());
}

type Edit = fn(&mut DevelopSettings);

fn typical(s: &mut DevelopSettings) {
    s.light.exposure = 0.3;
    s.light.highlights = -40.0;
    s.light.shadows = 30.0;
    s.effects.clarity = 15.0;
    s.effects.texture = 10.0;
    s.effects.dehaze = 10.0;
    s.detail.nr_luminance = 30.0;
    s.detail.nr_color = 25.0;
    s.detail.sharpen_amount = 40.0;
}

/// Issue #960: a preview's noise reduction works in the original's pixels (a smaller radius, a
/// smaller `eps`, faded below one pixel). The GPU takes the same parameters (`local::nr_params`).
#[test]
fn preview_noise_reduction_matches() {
    if !gpu() {
        return;
    }
    let src = scene(3, 640, 480);
    let mut s = DevelopSettings::default();
    s.detail.nr_luminance = 70.0;
    s.detail.nr_detail = 30.0;
    s.detail.nr_color = 60.0;
    s.detail.nr_color_smoothness = 50.0;
    // a preview level of a 6000-px original: loupe-, grid- and tiny-thumbnail-sized outputs
    let info = SourceInfo { raw: true, native_long: Some(6000), ..Default::default() };
    for (name, edge) in [("NR preview 640 of 6000", 640), ("NR preview 320 of 6000", 320), ("NR preview 128 of 6000", 128)] {
        check(name, &src, &info, &s, &RenderRequest::fit(edge, edge));
    }
}

/// The joint colour noise reduction of process V8 (upstream #1348) on the device: the same kernels'
/// result as the CPU's at the raw default and at 100, on a noisy raw scene, a preview level of a large
/// original (the whole frame's noise model from tiles) and a zoomed window; V7 keeps the blurred
/// chromaticity on both.
#[test]
fn joint_colour_noise_reduction_matches() {
    if !gpu() {
        return;
    }
    let src = scene(3, 640, 480);
    let info = SourceInfo { raw: true, ..Default::default() };
    for p in [Process::V7, Process::V8] {
        for (color, detail, smooth) in [(25.0, 50.0, 50.0), (100.0, 0.0, 100.0)] {
            let mut s = DevelopSettings { process: p.version(), ..Default::default() };
            s.detail.nr_luminance = 20.0;
            s.detail.nr_color = color;
            s.detail.nr_color_detail = detail;
            s.detail.nr_color_smoothness = smooth;
            check(&format!("colour NR {color} {p:?}"), &src, &info, &s, &RenderRequest::fit(640, 480));
        }
    }
    let mut s = DevelopSettings { process: Process::V8.version(), ..Default::default() };
    s.detail.nr_color = 60.0;
    // a 1600-px frame: many noise tiles; the window is grown by the filter's reach and cut back
    let big = scene(3, 1600, 1200);
    let preview = SourceInfo { raw: true, native_long: Some(6000), ..Default::default() };
    check("joint colour NR 1600 of 6000", &big, &preview, &s, &RenderRequest::fit(1600, 1200));
    let win = lcc_pipeline::PixelWindow { x: 700, y: 400, w: 320, h: 240 };
    check("joint colour NR window", &big, &info, &s, &RenderRequest { window: Some(win), ..RenderRequest::fit(1600, 1200) });
    // an HDR window: grown by the filter's reach and cut back, floats and texels alike
    s.hdr.enabled = true;
    s.light.exposure = 1.5;
    let req = RenderRequest { window: Some(win), depth: lcc_pipeline::OutputDepth::F32Hdr, ..RenderRequest::fit(1600, 1200) };
    check_hdr_req("joint colour NR HDR window", &big, &info, &s, &req);
    let (r, texels) = lcc_gpu::try_render_hdr_texels(&big, &info, &s, &req, None, 4.0).expect("the GPU renders HDR texels");
    assert_eq!((r.image.width, r.image.height), (win.w, win.h), "the HDR window's own size");
    assert_eq!(texels.len(), 4 * win.w * win.h, "four texel values per pixel of the window");
}

/// A render big enough for the per-pixel stage to run in several bands of rows (and the render
/// in many submissions), with grain (position-dependent) and masks.
#[test]
fn banded_full_size_render_matches() {
    if !gpu() {
        return;
    }
    let (w, h) = (3000, 2000);
    let src = scene(3, w, h);
    let mut s = DevelopSettings::default();
    typical(&mut s);
    // the value-noise grain (processes V1–V6)
    s.process = lcc_develop::Process::V6.version();
    s.grain.amount = 30.0;
    s.vignette.amount = -30.0;
    s.masks = vec![Mask {
        components: vec![MaskComponent {
            anchor: None,
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Linear { start: Point::new(0.5, 0.0), end: Point::new(0.5, 0.9) },
        }],
        adjust: lcc_develop::LocalAdjustments { exposure: -0.6, ..Default::default() },
        ..Default::default()
    }];
    check("banded 3000×2000", &src, &SourceInfo { raw: true, ..Default::default() }, &s, &RenderRequest::fit(w, h));
}

fn cases() -> Vec<(&'static str, Edit)> {
    vec![
        ("default", |_| {}),
        // issue #69: the SDR rendition of an HDR edit rolls its highlights off instead of clipping
        ("hdr edit sdr rendition", |s| {
            s.hdr.enabled = true;
            s.light.exposure = 1.5;
            s.hdr.sdr_whites = 20.0;
        }),
        ("typical", typical),
        ("exposure/contrast/whites", |s| {
            s.light.exposure = -0.7;
            s.light.contrast = 45.0;
            s.light.whites = 30.0;
            s.light.blacks = -25.0;
        }),
        ("rgb contrast/blacks", |s| {
            s.light.exposure = -0.3;
            s.light.contrast = 50.0;
            s.light.whites = 20.0;
            s.light.blacks = -35.0;
            s.light.contrast_mode = lcc_develop::ContrastMode::Rgb;
        }),
        ("rgb contrast/blacks lifted", |s| {
            s.light.contrast = -40.0;
            s.light.blacks = 45.0;
            s.light.contrast_mode = lcc_develop::ContrastMode::Rgb;
        }),
        ("vibrance/saturation", |s| {
            s.color.vibrance = 40.0;
            s.color.saturation = -20.0;
        }),
        ("colour mixer", |s| {
            s.mixer.blue.sat = -60.0;
            s.mixer.orange.hue = 30.0;
            s.mixer.green.lum = 40.0;
        }),
        ("point color", |s| {
            use lcc_develop::PointColor;
            s.point_colors = vec![
                PointColor {
                    lum: 0.62,
                    chroma: 0.09,
                    hue: 60.0,
                    hue_shift: 40.0,
                    sat_shift: -30.0,
                    lum_shift: 20.0,
                    range: 80.0,
                    ..Default::default()
                },
                PointColor { lum: 0.5, chroma: 0.12, hue: 270.0, variance: -60.0, sat_shift: 40.0, hue_range: 80.0, ..Default::default() },
                PointColor { lum: 0.4, chroma: 0.01, hue: 0.0, lum_shift: -30.0, ..Default::default() },
            ];
        }),
        ("b&w mix", |s| {
            s.treatment = Treatment::Bw;
            s.bw_mix.blue = 60.0;
            s.bw_mix.red = -30.0;
        }),
        ("colour grading", |s| {
            s.grading.shadows = Wheel { hue: 220.0, sat: 40.0, lum: -10.0 };
            s.grading.highlights = Wheel { hue: 40.0, sat: 30.0, lum: 10.0 };
            s.grading.global = Wheel { hue: 300.0, sat: 10.0, lum: 0.0 };
            s.grading.blending = 70.0;
            s.grading.balance = 20.0;
        }),
        ("colourist primaries", |s| {
            s.primaries.lift = Wheel { hue: 220.0, sat: 50.0, lum: -10.0 };
            s.primaries.gamma = Wheel { hue: 30.0, sat: 20.0, lum: 0.0 };
            s.primaries.gain = Wheel { hue: 0.0, sat: 25.0, lum: 15.0 };
            s.primaries.offset = Wheel { hue: 200.0, sat: 10.0, lum: 5.0 };
        }),
        ("colourist primaries (log)", |s| {
            s.primaries.log_wheels.shadow = Wheel { hue: 220.0, sat: 50.0, lum: -10.0 };
            s.primaries.log_wheels.highlight = Wheel { hue: 0.0, sat: 25.0, lum: 15.0 };
            s.primaries.low = 0.25;
            s.primaries.high = 0.8;
        }),
        ("colourist primaries (both sets)", |s| {
            s.primaries.lift = Wheel { hue: 220.0, sat: 50.0, lum: -10.0 };
            s.primaries.gamma = Wheel { hue: 30.0, sat: 20.0, lum: 0.0 };
            s.primaries.gain = Wheel { hue: 0.0, sat: 25.0, lum: 15.0 };
            s.primaries.offset = Wheel { hue: 200.0, sat: 10.0, lum: 5.0 };
            s.primaries.log_wheels.shadow = Wheel { hue: 220.0, sat: 50.0, lum: -10.0 };
            s.primaries.log_wheels.midtone = Wheel { hue: 60.0, sat: 15.0, lum: 5.0 };
            s.primaries.log_wheels.highlight = Wheel { hue: 0.0, sat: 25.0, lum: 15.0 };
            s.primaries.log_wheels.offset = Wheel { hue: 200.0, sat: 10.0, lum: 5.0 };
        }),
        ("hsl curves", |s| {
            s.hsl_curves.hue = vec![Point::new(0.55, -0.4), Point::new(0.8, 0.3)];
            s.hsl_curves.sat = vec![Point::new(0.3, -0.6)];
            s.hsl_curves.lum = vec![Point::new(0.65, 0.5)];
        }),
        ("tone curves", |s| {
            s.curve.highlights = -30.0;
            s.curve.shadows = 25.0;
            s.curve.master = vec![Point::new(0.0, 0.05), Point::new(0.5, 0.55), Point::new(1.0, 0.95)];
            s.curve.blue = vec![Point::new(0.0, 0.0), Point::new(0.5, 0.45), Point::new(1.0, 1.0)];
        }),
        ("levels (master points)", |s| {
            s.levels.master.black = 24.0;
            s.levels.master.white = 232.0;
        }),
        ("levels (midtone + output)", |s| {
            s.levels.master.midtone = 0.5;
            s.levels.master.out_black = 12.0;
            s.levels.master.out_white = 244.0;
        }),
        ("levels (channels + master + curve)", |s| {
            s.levels.master.black = 10.0;
            s.levels.master.midtone = -0.3;
            s.levels.red.white = 220.0;
            s.levels.red.midtone = 0.6;
            s.levels.green.black = 18.0;
            s.levels.blue.out_black = 30.0;
            s.levels.blue.out_white = 200.0;
            s.curve.master = vec![Point::new(0.0, 0.03), Point::new(0.5, 0.55), Point::new(1.0, 0.97)];
        }),
        ("levels (inverted output, strong midtone)", |s| {
            s.levels.master.midtone = 0.8;
            s.levels.master.out_black = 255.0;
            s.levels.master.out_white = 0.0;
        }),
        ("levels (hdr edit)", |s| {
            s.hdr.enabled = true;
            s.light.exposure = 1.5;
            s.levels.master.black = 20.0;
            s.levels.master.midtone = 0.4;
            s.levels.master.white = 230.0;
        }),
        ("levels (inverted input range)", |s| {
            s.levels.master.black = 215.0;
            s.levels.master.white = 40.0;
        }),
        ("levels (inverted input, midtone, channels)", |s| {
            s.levels.master.black = 230.0;
            s.levels.master.white = 25.0;
            s.levels.master.midtone = -0.5;
            s.levels.red.black = 10.0;
            s.levels.red.white = 200.0;
            s.levels.green.out_black = 240.0;
            s.levels.green.out_white = 15.0;
            s.curve.master = vec![Point::new(0.0, 0.03), Point::new(0.5, 0.55), Point::new(1.0, 0.97)];
        }),
        ("levels (inverted red, hdr edit)", |s| {
            // red runs backwards and holds its end value above SDR white; green and blue rise and continue
            s.hdr.enabled = true;
            s.light.exposure = 1.5;
            s.levels.master.midtone = 0.3;
            s.levels.red.black = 235.0;
            s.levels.red.white = 15.0;
        }),
        ("tone curves + refine saturation", |s| {
            s.curve.master = vec![Point::new(0.0, 0.0), Point::new(0.25, 0.15), Point::new(0.75, 0.88), Point::new(1.0, 1.0)];
            s.curve.lights = 30.0;
            s.curve.refine_saturation = 20.0;
        }),
        ("channel curves + brightness + levels in the V10 wide space (#1142)", |s| {
            s.curve.red = vec![Point::new(0.0, 0.0), Point::new(0.62, 0.38), Point::new(1.0, 1.0)];
            s.curve.green = vec![Point::new(0.0, 0.0), Point::new(0.514, 0.482), Point::new(1.0, 1.0)];
            s.curve.refine_saturation = 60.0;
            s.light.brightness = 30.0;
            s.levels.master.midtone = 0.3;
        }),
        ("channel curves on output values (process V9)", |s| {
            s.process = lcc_develop::Process::V9.version();
            s.curve.red = vec![Point::new(0.0, 0.0), Point::new(0.62, 0.38), Point::new(1.0, 1.0)];
            s.light.brightness = 30.0;
        }),
        ("vignette (highlight)", |s| {
            s.vignette.amount = -60.0;
            s.vignette.highlights = 50.0;
            s.vignette.roundness = -40.0;
        }),
        ("vignette (paint, +)", |s| {
            s.vignette.amount = 40.0;
            s.vignette.style = VignetteStyle::PaintOverlay;
        }),
        ("vignette (paint, -)", |s| {
            s.vignette.amount = -40.0;
            s.vignette.style = VignetteStyle::PaintOverlay;
        }),
        ("particle grain (V7)", |s| {
            s.process = lcc_develop::Process::V7.version();
            s.grain.amount = 50.0;
            s.grain.size = 40.0;
            s.grain.roughness = 60.0;
            s.grain.color = 30.0;
        }),
        ("grain", |s| {
            // processes V1–V6 keep their value-noise grain
            s.process = lcc_develop::Process::V6.version();
            s.grain.amount = 50.0;
            s.grain.size = 40.0;
            s.grain.roughness = 60.0;
        }),
        ("dehaze -", |s| s.effects.dehaze = -50.0),
        ("dehaze +", |s| s.effects.dehaze = 60.0),
        ("clarity/texture -", |s| {
            s.effects.clarity = -60.0;
            s.effects.texture = -40.0;
        }),
        ("sharpen radius 2.5 detail 100", |s| {
            s.detail.sharpen_amount = 100.0;
            s.detail.sharpen_radius = 2.5;
            s.detail.sharpen_detail = 100.0;
        }),
        ("sharpen radius 0.6 detail 0", |s| {
            s.detail.sharpen_amount = 80.0;
            s.detail.sharpen_radius = 0.6;
            s.detail.sharpen_detail = 0.0;
        }),
        ("sharpen + texture + masking", |s| {
            s.detail.sharpen_amount = 70.0;
            s.detail.sharpen_masking = 30.0;
            s.effects.texture = 40.0;
        }),
        ("local sharpness", |s| {
            s.masks = vec![Mask {
                components: vec![MaskComponent {
                    anchor: None,
                    name: None,
                    op: MaskOp::Add,
                    invert: false,
                    shape: MaskShape::Linear { start: Point::new(0.5, 0.0), end: Point::new(0.5, 0.6) },
                }],
                adjust: lcc_develop::LocalAdjustments { sharpness: 80.0, ..Default::default() },
                ..Default::default()
            }];
        }),
        ("sharpen + texture + masking on process V3", |s| {
            s.process = lcc_develop::Process::V3.version();
            s.detail.sharpen_amount = 70.0;
            s.detail.sharpen_masking = 30.0;
            s.effects.texture = 20.0;
        }),
        ("sharpen masking", |s| {
            s.detail.sharpen_amount = 90.0;
            s.detail.sharpen_masking = 60.0;
        }),
        ("white balance", |s| {
            s.wb.mode = WbMode::Custom;
            s.wb.temp = 8200.0;
            s.wb.tint = 15.0;
        }),
        ("noise reduction", |s| {
            s.detail.nr_luminance = 70.0;
            s.detail.nr_detail = 30.0;
            s.detail.nr_color = 60.0;
            s.detail.nr_color_smoothness = 50.0;
        }),
        ("crop + straighten + flip", |s| {
            s.crop.geometry.rect = Rect::new(0.1, 0.05, 0.85, 0.9);
            s.crop.geometry.angle = 7.5;
            s.crop.flip_h = true;
        }),
        ("orientation", |s| s.orientation = Orientation::Rotate90),
        ("lens + perspective", |s| {
            s.optics.distortion = 30.0;
            s.optics.vignetting = 40.0;
            s.optics.ca_red = 50.0;
            s.optics.ca_blue = -40.0;
            s.geometry.vertical = 20.0;
        }),
        ("masks (gradients)", |s| {
            s.masks = vec![
                Mask {
                    components: vec![MaskComponent {
                        anchor: None,
                        name: None,
                        op: MaskOp::Add,
                        invert: false,
                        shape: MaskShape::Linear { start: Point::new(0.5, 0.0), end: Point::new(0.5, 0.6) },
                    }],
                    adjust: lcc_develop::LocalAdjustments {
                        exposure: -0.8,
                        temp: -30.0,
                        saturation: 20.0,
                        grade_shadows_hue: 210.0,
                        grade_shadows_sat: 40.0,
                        grade_highlights_hue: 40.0,
                        grade_highlights_sat: 30.0,
                        grade_global_lum: 10.0,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                Mask {
                    components: vec![
                        MaskComponent {
                            anchor: None,
                            name: None,
                            op: MaskOp::Add,
                            invert: false,
                            shape: MaskShape::Radial { center: Point::new(0.4, 0.6), rx: 0.25, ry: 0.15, angle: 20.0, feather: 60.0, invert: false },
                        },
                        MaskComponent {
                            anchor: None,
                            name: None,
                            op: MaskOp::Intersect,
                            invert: true,
                            shape: MaskShape::Linear { start: Point::new(0.0, 0.0), end: Point::new(1.0, 1.0) },
                        },
                    ],
                    adjust: lcc_develop::LocalAdjustments {
                        shadows: 40.0,
                        clarity: 30.0,
                        contrast: 20.0,
                        color_hue: 30.0,
                        color_sat: 50.0,
                        hue: 10.0,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ];
        }),
        ("masks (brush + ranges)", |s| {
            let stroke = BrushStroke {
                points: vec![Point::new(0.1, 0.3), Point::new(0.5, 0.4), Point::new(0.8, 0.35)],
                size: 0.04,
                flow: 60.0,
                ..Default::default()
            };
            let erase = BrushStroke { points: vec![Point::new(0.5, 0.4)], size: 0.03, erase: true, ..Default::default() };
            s.masks = vec![
                Mask {
                    components: vec![MaskComponent {
                        anchor: None,
                        name: None,
                        op: MaskOp::Add,
                        invert: false,
                        shape: MaskShape::Brush { strokes: vec![stroke, erase] },
                    }],
                    adjust: lcc_develop::LocalAdjustments { exposure: 0.7, whites: 20.0, blacks: -20.0, dehaze: 30.0, ..Default::default() },
                    ..Default::default()
                },
                Mask {
                    components: vec![MaskComponent {
                        anchor: None,
                        name: None,
                        op: MaskOp::Add,
                        invert: false,
                        shape: MaskShape::LuminanceRange { lo: 0.5, hi: 0.8, lo_feather: 0.1, hi_feather: 0.1 },
                    }],
                    adjust: lcc_develop::LocalAdjustments { highlights: -50.0, texture: 30.0, sharpness: 40.0, ..Default::default() },
                    ..Default::default()
                },
                Mask {
                    components: vec![MaskComponent {
                        anchor: None,
                        name: None,
                        op: MaskOp::Add,
                        invert: false,
                        shape: MaskShape::ColorRange { samples: vec![[0.6, -0.05, -0.08]], refine: 50.0, areas: Vec::new() },
                    }],
                    adjust: lcc_develop::LocalAdjustments { saturation: 40.0, tint: 20.0, ..Default::default() },
                    invert: true,
                    ..Default::default()
                },
            ];
        }),
        ("masks (cpu shapes, ops, amount)", |s| {
            s.masks = vec![
                Mask {
                    components: vec![
                        MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape: MaskShape::Sky },
                        MaskComponent {
                            anchor: None,
                            name: None,
                            op: MaskOp::Subtract,
                            invert: false,
                            shape: MaskShape::Radial { center: Point::new(0.7, 0.2), rx: 0.1, ry: 0.1, angle: 0.0, feather: 30.0, invert: false },
                        },
                    ],
                    adjust: lcc_develop::LocalAdjustments { exposure: -0.5, dehaze: 40.0, amount: 70.0, ..Default::default() },
                    ..Default::default()
                },
                Mask {
                    components: vec![MaskComponent { anchor: None, name: None, op: MaskOp::Intersect, invert: false, shape: MaskShape::Subject }],
                    adjust: lcc_develop::LocalAdjustments { exposure: 0.5, ..Default::default() },
                    ..Default::default()
                },
                Mask {
                    components: vec![MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: true, shape: MaskShape::Background }],
                    adjust: lcc_develop::LocalAdjustments { saturation: -60.0, ..Default::default() },
                    invert: true,
                    ..Default::default()
                },
            ];
        }),
        ("masks (inverted, add and subtract)", |s| {
            let radial = |x: f64| MaskShape::Radial { center: Point::new(x, 0.5), rx: 0.15, ry: 0.15, angle: 0.0, feather: 30.0, invert: false };
            let comp = |op, x| MaskComponent { anchor: None, name: None, op, invert: false, shape: radial(x) };
            s.masks = vec![
                Mask {
                    components: vec![comp(MaskOp::Add, 0.3), comp(MaskOp::Add, 0.7), comp(MaskOp::Subtract, 0.3)],
                    adjust: lcc_develop::LocalAdjustments { exposure: 0.5, ..Default::default() },
                    invert: true,
                    ..Default::default()
                },
                Mask {
                    components: vec![comp(MaskOp::Add, 0.3), comp(MaskOp::Intersect, 0.5)],
                    adjust: lcc_develop::LocalAdjustments { exposure: -0.5, ..Default::default() },
                    invert: true,
                    ..Default::default()
                },
            ];
        }),
        ("masks (local noise, moiré, defringe)", |s| {
            let radial = MaskShape::Radial { center: Point::new(0.4, 0.6), rx: 0.3, ry: 0.25, angle: 0.0, feather: 50.0, invert: false };
            let linear = MaskShape::Linear { start: Point::new(0.5, 0.0), end: Point::new(0.5, 0.7) };
            let m = |shape, adjust| Mask {
                components: vec![MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape }],
                adjust,
                ..Default::default()
            };
            use lcc_develop::LocalAdjustments as L;
            s.masks = vec![
                m(radial, L { noise: 80.0, moire: 60.0, defringe: 100.0, ..Default::default() }),
                m(linear, L { noise: -60.0, moire: -40.0, ..Default::default() }),
            ];
        }),
        ("masks (auto mask brush)", |s| {
            let st = |pts: &[(f64, f64)], auto_mask, erase| BrushStroke {
                points: pts.iter().map(|p| Point::new(p.0, p.1)).collect(),
                size: 0.06,
                feather: 40.0,
                flow: 80.0,
                auto_mask,
                erase,
                ..Default::default()
            };
            let strokes = vec![
                st(&[(0.2, 0.55), (0.5, 0.6), (0.8, 0.5)], true, false),
                st(&[(0.3, 0.2), (0.6, 0.25)], false, false),
                st(&[(0.5, 0.58)], true, true),
            ];
            s.masks = vec![Mask {
                components: vec![MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape: MaskShape::Brush { strokes } }],
                adjust: lcc_develop::LocalAdjustments { exposure: 1.0, saturation: -50.0, ..Default::default() },
                ..Default::default()
            }];
        }),
        ("masks (local curves)", |s| {
            use lcc_develop::{LocalAdjustments as L, PointCurves};
            let pts = |v: &[(f64, f64)]| v.iter().map(|p| Point::new(p.0, p.1)).collect::<Vec<_>>();
            let m = |shape, adjust, curve, invert| Mask {
                components: vec![MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape }],
                adjust,
                curve,
                invert,
                ..Default::default()
            };
            let radial = MaskShape::Radial { center: Point::new(0.4, 0.55), rx: 0.3, ry: 0.22, angle: 15.0, feather: 70.0, invert: false };
            let linear = MaskShape::Linear { start: Point::new(0.5, 0.0), end: Point::new(0.5, 0.7) };
            let s_curve = pts(&[(0.0, 0.05), (0.25, 0.15), (0.5, 0.5), (0.75, 0.88), (1.0, 0.97)]);
            s.masks = vec![
                // RGB + blue curve on a feathered radial, with a slider as well
                m(
                    radial,
                    L { exposure: 0.3, ..Default::default() },
                    PointCurves { master: s_curve, blue: pts(&[(0.0, 0.12), (1.0, 0.9)]), ..Default::default() },
                    false,
                ),
                // a mask without one in between: the slots skip it
                m(linear.clone(), L { saturation: -30.0, ..Default::default() }, PointCurves::default(), false),
                // a red curve at 60 % Amount, inverted
                m(
                    linear,
                    L { amount: 60.0, ..Default::default() },
                    PointCurves {
                        red: pts(&[(0.0, 0.0), (0.4, 0.6), (1.0, 1.0)]),
                        green: pts(&[(0.0, 0.0), (0.6, 0.45), (1.0, 1.0)]),
                        ..Default::default()
                    },
                    true,
                ),
            ];
        }),
        ("masks (local point color)", |s| {
            use lcc_develop::PointColor;
            let radial = MaskShape::Radial { center: Point::new(0.45, 0.5), rx: 0.35, ry: 0.3, angle: 0.0, feather: 60.0, invert: false };
            let linear = MaskShape::Linear { start: Point::new(0.5, 0.1), end: Point::new(0.5, 0.8) };
            let m = |shape, point_colors| Mask {
                components: vec![MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape }],
                point_colors,
                ..Default::default()
            };
            s.masks = vec![
                m(
                    radial,
                    vec![
                        PointColor {
                            lum: 0.62,
                            chroma: 0.09,
                            hue: 60.0,
                            hue_shift: 40.0,
                            sat_shift: -30.0,
                            lum_shift: 20.0,
                            range: 80.0,
                            ..Default::default()
                        },
                        PointColor { lum: 0.4, chroma: 0.01, hue: 0.0, lum_shift: -30.0, ..Default::default() },
                    ],
                ),
                // a mask without samples in between: its record is empty
                m(linear.clone(), Vec::new()),
                m(
                    linear,
                    vec![PointColor { lum: 0.5, chroma: 0.12, hue: 270.0, variance: -60.0, sat_shift: 40.0, hue_range: 80.0, ..Default::default() }],
                ),
            ];
        }),
        ("masks on the photo under transform + distortion", |s| {
            use lcc_develop::{BrushStroke, LocalAdjustments};
            s.geometry.vertical = 35.0;
            s.geometry.rotate = 4.0;
            s.optics.distortion = 30.0;
            let comp = |shape| MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape };
            let adj = LocalAdjustments { exposure: 1.2, saturation: -40.0, ..Default::default() };
            s.masks = vec![
                Mask {
                    components: vec![comp(MaskShape::Radial {
                        center: Point::new(0.4, 0.45),
                        rx: 0.25,
                        ry: 0.2,
                        angle: 20.0,
                        feather: 50.0,
                        invert: false,
                    })],
                    adjust: adj,
                    ..Default::default()
                },
                Mask {
                    components: vec![comp(MaskShape::Linear { start: Point::new(0.5, 0.0), end: Point::new(0.5, 0.6) })],
                    adjust: adj,
                    ..Default::default()
                },
                Mask {
                    components: vec![comp(MaskShape::Brush {
                        strokes: vec![BrushStroke { points: vec![Point::new(0.2, 0.7), Point::new(0.7, 0.75)], size: 0.04, ..Default::default() }],
                    })],
                    adjust: adj,
                    ..Default::default()
                },
            ];
        }),
        ("masks (local color mixer)", |s| {
            use lcc_develop::{Hsl, Mixer, PointColor};
            let radial = MaskShape::Radial { center: Point::new(0.45, 0.5), rx: 0.35, ry: 0.3, angle: 0.0, feather: 60.0, invert: false };
            let linear = MaskShape::Linear { start: Point::new(0.5, 0.1), end: Point::new(0.5, 0.8) };
            let comp = |shape| vec![MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape }];
            s.masks = vec![
                // a mixer alone, strong in several bands
                Mask {
                    components: comp(radial),
                    mixer: Mixer {
                        red: Hsl { hue: 40.0, sat: -50.0, lum: 30.0 },
                        yellow: Hsl { hue: -60.0, sat: 80.0, lum: -40.0 },
                        blue: Hsl { hue: 30.0, sat: -100.0, lum: 50.0 },
                        ..Default::default()
                    },
                    ..Default::default()
                },
                // a mixer with a Point Color sample after it
                Mask {
                    components: comp(linear),
                    mixer: Mixer { green: Hsl { hue: 70.0, sat: 40.0, lum: -60.0 }, ..Default::default() },
                    point_colors: vec![PointColor { lum: 0.5, chroma: 0.1, hue: 120.0, sat_shift: -40.0, range: 80.0, ..Default::default() }],
                    ..Default::default()
                },
            ];
        }),
        ("masks (Lightroom brush)", |s| {
            use lcc_develop::BrushStroke;
            // overlapping soft strokes add up, a hard one, an eraser and a legacy stroke in one component
            let st = |pts: &[(f64, f64)], feather, erase, model| BrushStroke {
                points: pts.iter().map(|p| Point::new(p.0, p.1)).collect(),
                size: 0.03,
                feather,
                flow: 70.0,
                density: 90.0,
                erase,
                model,
                ..Default::default()
            };
            let lrc = lcc_develop::BRUSH_MODEL_LRC;
            let strokes = vec![
                st(&[(0.2, 0.3), (0.5, 0.35), (0.7, 0.3)], 100.0, false, lrc),
                st(&[(0.3, 0.45), (0.6, 0.4)], 80.0, false, lrc),
                st(&[(0.25, 0.6)], 0.0, false, lrc),
                st(&[(0.45, 0.38)], 40.0, true, lrc),
                st(&[(0.6, 0.7), (0.8, 0.65)], 50.0, false, lcc_develop::BRUSH_MODEL_LEGACY),
            ];
            s.masks = vec![Mask {
                components: vec![MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape: MaskShape::Brush { strokes } }],
                adjust: lcc_develop::LocalAdjustments { exposure: -1.0, ..Default::default() },
                ..Default::default()
            }];
        }),
        ("masks (Lightroom auto mask brush)", |s| {
            use lcc_develop::BrushStroke;
            let st = |pts: &[(f64, f64)], auto_mask, erase| BrushStroke {
                points: pts.iter().map(|p| Point::new(p.0, p.1)).collect(),
                size: 0.04,
                feather: 60.0,
                flow: 90.0,
                auto_mask,
                erase,
                model: lcc_develop::BRUSH_MODEL_LRC,
                ..Default::default()
            };
            let strokes = vec![
                st(&[(0.2, 0.55), (0.5, 0.6), (0.8, 0.5)], true, false),
                st(&[(0.3, 0.5), (0.6, 0.55)], true, false),
                st(&[(0.3, 0.2), (0.6, 0.25)], false, false),
                st(&[(0.5, 0.58)], true, true),
            ];
            s.masks = vec![Mask {
                components: vec![MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape: MaskShape::Brush { strokes } }],
                adjust: lcc_develop::LocalAdjustments { exposure: 1.0, saturation: -50.0, ..Default::default() },
                ..Default::default()
            }];
        }),
        ("masks (Auto Mask range, note 1)", |s| {
            // the same strokes tight, at the default and wide: each keeps its own range
            let st = |pts: &[(f64, f64)], auto_range| BrushStroke {
                points: pts.iter().map(|p| Point::new(p.0, p.1)).collect(),
                size: 0.04,
                feather: 60.0,
                flow: 90.0,
                auto_mask: true,
                auto_range,
                model: lcc_develop::BRUSH_MODEL_LRC,
                ..Default::default()
            };
            let strokes = vec![
                st(&[(0.2, 0.3), (0.8, 0.35)], -100.0),
                st(&[(0.2, 0.55), (0.5, 0.6), (0.8, 0.5)], 0.0),
                st(&[(0.2, 0.75), (0.8, 0.7)], 100.0),
                st(&[(0.5, 0.45)], 37.0),
            ];
            s.masks = vec![Mask {
                components: vec![MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape: MaskShape::Brush { strokes } }],
                adjust: lcc_develop::LocalAdjustments { exposure: 1.0, saturation: -50.0, ..Default::default() },
                ..Default::default()
            }];
        }),
        ("calibration", |s| {
            s.calibration.shadows_tint = 40.0;
            s.calibration.red_hue = 50.0;
            s.calibration.red_sat = -30.0;
            s.calibration.green_hue = -40.0;
            s.calibration.blue_sat = 60.0;
            s.calibration.blue_hue = 25.0;
        }),
        ("retouch teeth (cpu stage)", |s| {
            // teeth found over the middle of the frame: the GPU reads its scene-linear image back
            // for the retouch (lcc_pipeline::retouch_people), so both renderers run the same code
            let side = 64;
            let logits: Vec<f32> = (0..side * side)
                .map(|i| {
                    let (x, y) = ((i % side) as f64 / side as f64, (i / side) as f64 / side as f64);
                    if (x - 0.5).hypot(y - 0.5) < 0.25 { 10.0 } else { -10.0 }
                })
                .collect();
            s.retouch.teeth = lcc_develop::RetouchTeeth { person: 0, whiten: 80.0, brighten: 60.0 };
            s.retouch_analysis = Some(lcc_develop::RetouchAnalysis {
                orientation: Orientation::Normal,
                masks: vec![lcc_develop::RetouchMask {
                    part: lcc_develop::RetouchPart::Teeth,
                    person: 0,
                    seg: Some(lcc_develop::SegMask::from_logits(side, &logits)),
                }],
            });
        }),
        ("smooth skin (gpu)", |s| {
            // a face over the middle of the frame, its skin a soft ellipse: Smooth Skin runs on
            // the device (the `skin` module, from skin::plan)
            let side = 128;
            let logits: Vec<f32> = (0..side * side)
                .map(|i| {
                    let (x, y) = (((i % side) as f64 + 0.5) / side as f64, ((i / side) as f64 + 0.5) / side as f64);
                    let d = ((x - 0.5) / 0.18).powi(2) + ((y - 0.5) / 0.24).powi(2);
                    (8.0 * (1.0 - d)).clamp(-12.0, 12.0) as f32
                })
                .collect();
            s.skin = lcc_develop::SkinSmoothing { amount: 80.0, texture: -30.0, even_tone: 50.0, redness: 40.0, shine: 30.0, ..Default::default() };
            s.skin_analysis = Some(lcc_develop::SkinAnalysis {
                area: s.skin.area,
                person: s.skin.person,
                orientation: s.orientation,
                seg: lcc_develop::SegMask::from_logits(side, &logits),
                faces: vec![lcc_develop::SkinFace { x: 0.5, y: 0.45, eyes: 0.06 }],
            });
        }),
        ("spots + defringe (cpu stage)", |s| {
            s.spots = vec![Spot { points: vec![Point::new(0.3, 0.3)], size: 0.03, source_offset: Some(Point::new(0.1, 0.0)), ..Default::default() }];
            s.optics.defringe_purple_amount = 5.0;
        }),
    ]
}

/// `ContrastMode::Rgb` evaluates the per-channel Contrast/Blacks LUT on the GPU and matches the CPU.
#[test]
fn rgb_contrast_mode_renders_on_the_gpu() {
    if !gpu() {
        return;
    }
    let src = scene(0, 480, 320);
    let (raw, req) = (SourceInfo { raw: true, ..Default::default() }, RenderRequest::fit(480, 480));
    for name in ["rgb contrast/blacks", "rgb contrast/blacks lifted"] {
        let mut s = DevelopSettings::default();
        let edit = cases().into_iter().find(|(n, _)| *n == name).map(|c| c.1).unwrap();
        edit(&mut s);
        check(name, &src, &raw, &s, &req);
    }
}

/// A mask's local Point Color is drawn by the kernel, as the CPU draws it.
#[test]
fn local_point_colors_render_on_the_gpu() {
    if !gpu() {
        return;
    }
    let src = scene(0, 480, 320);
    let (raw, req) = (SourceInfo { raw: true, ..Default::default() }, RenderRequest::fit(480, 480));
    let mut s = DevelopSettings::default();
    let edit = cases().into_iter().find(|(name, _)| *name == "masks (local point color)").map(|c| c.1).unwrap();
    edit(&mut s);
    check("local point color", &src, &raw, &s, &req);
    let with = lcc_gpu::render(&src, &raw, &s, &req, None).expect("gpu render").image;
    for m in &mut s.masks {
        m.point_colors.clear();
    }
    let without = lcc_gpu::render(&src, &raw, &s, &req, None).expect("gpu render").image;
    let (mean, max, _) = diff(&with, &without);
    assert!(mean > 0.2 && max > 10, "the samples change the GPU render: mean {mean:.2}, max {max}");
}

/// A mask's local Color Mixer is drawn by the kernel, as the CPU draws it.
#[test]
fn local_color_mixers_render_on_the_gpu() {
    if !gpu() {
        return;
    }
    let src = scene(0, 480, 320);
    let (raw, req) = (SourceInfo { raw: true, ..Default::default() }, RenderRequest::fit(480, 480));
    let mut s = DevelopSettings::default();
    let edit = cases().into_iter().find(|(name, _)| *name == "masks (local color mixer)").map(|c| c.1).unwrap();
    edit(&mut s);
    check("local color mixer", &src, &raw, &s, &req);
    let with = lcc_gpu::render(&src, &raw, &s, &req, None).expect("gpu render").image;
    for m in &mut s.masks {
        m.mixer = Default::default();
    }
    let without = lcc_gpu::render(&src, &raw, &s, &req, None).expect("gpu render").image;
    let (mean, max, _) = diff(&with, &without);
    assert!(mean > 0.2 && max > 10, "the mixers change the GPU render: mean {mean:.2}, max {max}");
}

/// A mask's local Curve is drawn by the kernel (not left out, nor handed to the CPU), as the CPU draws it.
#[test]
fn local_curves_render_on_the_gpu() {
    if !gpu() {
        return;
    }
    let src = scene(0, 480, 320);
    let (raw, req) = (SourceInfo { raw: true, ..Default::default() }, RenderRequest::fit(480, 480));
    let mut s = DevelopSettings::default();
    let edit = cases().into_iter().find(|(name, _)| *name == "masks (local curves)").map(|c| c.1).unwrap();
    edit(&mut s);
    check("local curves", &src, &raw, &s, &req);
    let with = lcc_gpu::render(&src, &raw, &s, &req, None).expect("gpu render").image;
    for m in &mut s.masks {
        m.curve = Default::default();
    }
    let without = lcc_gpu::render(&src, &raw, &s, &req, None).expect("gpu render").image;
    let (mean, max, _) = diff(&with, &without);
    assert!(mean > 2.0 && max > 20, "the curves change the GPU render: mean {mean:.2}, max {max}");
}

#[test]
fn gpu_matches_cpu() {
    if !gpu() {
        return;
    }
    let src = scene(0, 960, 640);
    let raw = SourceInfo { raw: true, ..Default::default() };
    let req = RenderRequest::fit(720, 720);
    let mut worst = (0.0f64, 0u8);
    for (name, edit) in cases() {
        let mut s = DevelopSettings::default();
        edit(&mut s);
        let (m, x) = check(name, &src, &raw, &s, &req);
        worst = (worst.0.max(m), worst.1.max(x));
    }
    eprintln!("worst: mean {:.4} LSB, max {} LSB", worst.0, worst.1);
}

/// Issue #523: a raw's clipped highlights rebuilt as the raw decode does it (fully clipped ones to the neutral of
/// the colour model applied after them, here a look whose rows don't sum to one) stay neutral when darkened, on
/// the GPU as on the CPU, and the two renders agree.
#[test]
fn clipped_highlights_stay_neutral_on_both() {
    if !gpu() {
        return;
    }
    let wb = [2.4f32, 1.0, 1.6];
    let look = lcc_color::Mat3([[0.42, 0.548, 0.04], [0.246, 0.728, 0.033], [-0.247, 0.679, 0.447]]);
    let luma = |c: [f64; 3]| (0..3).map(|i| c[i] * f64::from(lcc_color::LUMA_2020[i])).sum::<f64>();
    let n = look.inverse().unwrap().apply([1.0; 3]);
    let white = n.map(|v| (v * luma(look.apply([1.0; 3])) / luma(look.apply(n))) as f32);
    // a grey wall with a fully clipped light, a light clipped in green only, and a dark bar with a purple fringe
    // in front of the second
    let mut cam = Rgb32f::from_fn(320, 240, |x, y| {
        let d = |cx: f32| ((x as f32 - cx).powi(2) + (y as f32 - 120.0).powi(2)).sqrt();
        if d(90.0) < 50.0 {
            [1.0; 3]
        } else if (226..232).contains(&x) && (70..170).contains(&y) {
            [0.02, 0.03, 0.02]
        } else if (x == 225 || x == 232) && (70..170).contains(&y) {
            [0.3, 0.25, 0.6]
        } else if d(228.0) < 50.0 {
            [0.6 / wb[0], 1.0, 0.7 / wb[2]]
        } else {
            [0.3 / wb[0], 0.3, 0.3 / wb[2]]
        }
    });
    lcc_raw::highlight::reconstruct_with(&mut cam, wb, 0.99, white);
    let m = look.to_f32();
    let src = Arc::new(cam.map(|p| {
        let c = [p[0] * wb[0], p[1] * wb[1], p[2] * wb[2]];
        std::array::from_fn(|i| (m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2]).max(0.0))
    }));
    let info = SourceInfo { raw: true, ..Default::default() };
    let req = RenderRequest::fit(320, 240);
    let mut h100 = DevelopSettings::default();
    h100.light.highlights = -100.0;
    let mut issue = DevelopSettings::default();
    let l = &mut issue.light;
    (l.exposure, l.contrast, l.highlights, l.shadows, l.whites, l.blacks) = (-0.88, 5.0, -75.0, 52.0, 6.0, -37.0);
    for (name, s) in [("clipped highlights -100", h100), ("clipped highlights #523", issue)] {
        check(name, &src, &info, &s, &req);
        let cpu = render(&src, &info, &s, &req).image;
        let gpu = lcc_gpu::render(&src, &info, &s, &req, None).expect("gpu render").image;
        for (who, img) in [("CPU", &cpu), ("GPU", &gpu)] {
            let p = img.get(90, 120);
            assert!(p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]) <= 1, "{name} {who}: clipped light {p:?}");
        }
    }
}

/// Black pixels have no hue. The GPU's `atan2(0, 0)` is undefined (NaN on some GPUs), which made
/// every exactly black pixel NaN once a colour tool ran, and a lightening vignette then showed it:
/// the CPU lifted the black corners, the GPU left them black.
#[test]
fn black_pixels_match_under_colour_tools_and_a_light_vignette() {
    if !gpu() {
        return;
    }
    let base = scene(0, 320, 240);
    // black bars across the frame, reaching into the corners the vignette lightens
    let src = Arc::new(Rgb32f::from_fn(320, 240, |x, y| if y % 40 < 8 || x % 64 < 6 { [0.0; 3] } else { base.get(x, y) }));
    let info = SourceInfo { raw: true, ..Default::default() };
    let req = RenderRequest::fit(320, 240);
    for (name, f) in [
        ("black, light vignette + saturation", (|s: &mut DevelopSettings| s.color.saturation = 40.0) as fn(&mut DevelopSettings)),
        ("black, light vignette + vibrance", |s| s.color.vibrance = 30.0),
        ("black, light vignette + mixer", |s| s.mixer.yellow.hue = 30.0),
    ] {
        let mut s = DevelopSettings::default();
        s.vignette.amount = 25.0;
        s.vignette.style = VignetteStyle::HighlightPriority;
        f(&mut s);
        check(name, &src, &info, &s, &req);
        let gpu = lcc_gpu::render(&src, &info, &s, &req, None).expect("gpu render").image;
        assert!(gpu.get(0, 0)[1] > 0, "{name}: the black corner is not lightened on the GPU");
    }
}

#[test]
fn rendered_sources_and_other_scenes() {
    if !gpu() {
        return;
    }
    // A display-referred source (JPEG-like: display tone map) and other scenes / sizes / draft.
    let jpeg = SourceInfo::default();
    for (i, (w, h)) in [(3usize, (800usize, 533usize)), (5, (640, 960)), (7, (1200, 800))] {
        let src = scene(i, w, h);
        let mut s = DevelopSettings::default();
        typical(&mut s);
        s.color.vibrance = 25.0;
        check(&format!("scene {i} display-referred"), &src, &jpeg, &s, &RenderRequest::fit(700, 700));
        let draft = RenderRequest { quality: Quality::Draft, ..RenderRequest::fit(500, 500) };
        check(&format!("scene {i} draft"), &src, &SourceInfo { raw: true, ..Default::default() }, &s, &draft);
        check(&format!("scene {i} full size"), &src, &SourceInfo { raw: true, ..Default::default() }, &s, &RenderRequest::fit(w, h));
    }
}

#[test]
fn geometry_variants() {
    if !gpu() {
        return;
    }
    let src = scene(2, 900, 600);
    let raw = SourceInfo { raw: true, ..Default::default() };
    use Orientation::*;
    for o in [Normal, Rotate90, Rotate180, Rotate270, FlipH, Transverse, FlipV, Transpose] {
        for (crop, req) in [(false, RenderRequest::fit(900, 900)), (true, RenderRequest::fit(500, 500))] {
            let mut s = DevelopSettings { orientation: o, ..Default::default() };
            if crop {
                s.crop.geometry.rect = Rect::new(0.2, 0.1, 0.9, 0.8);
                s.crop.geometry.angle = -4.0;
                s.crop.flip_v = true;
            }
            check(&format!("{o:?} crop={crop}"), &src, &raw, &s, &req);
        }
    }
}

#[test]
fn embedded_lens_perspective_edges_match() {
    if !gpu() {
        return;
    }
    use lcc_develop::{EmbeddedLens, EmbeddedVignette, EmbeddedWarp};
    let src = scene(2, 900, 600);
    let raw = SourceInfo { raw: true, ..Default::default() };
    // The unrotated case includes a position only 0.000035 px outside the edge.
    // f32 classification previously sampled the photo instead of blank canvas.
    // Embedded DNG lens corrections (per-plane warp + vignette) with manual CA.
    let lens = EmbeddedLens {
        warp: Some(EmbeddedWarp {
            planes: [[1.0, -0.03, 0.01, 0.0, 0.001, -0.002], [1.0, -0.028, 0.01, 0.0, 0.001, -0.002], [1.0, -0.026, 0.01, 0.0, 0.001, -0.002]],
            center: Point::new(0.52, 0.48),
            radius: 0.6,
        }),
        vignette: Some(EmbeddedVignette { k: [0.4, -0.1, 0.02, 0.0, 0.0], center: Point::new(0.5, 0.5), radius: 0.6 }),
        stages: Default::default(),
    };
    let info = SourceInfo { lens: Some(lens), ..raw };
    let mut s = DevelopSettings::default();
    s.optics.lens_profile = true;
    s.optics.ca_red = 30.0;
    s.geometry.horizontal = -15.0;
    check("embedded lens + perspective", &src, &info, &s, &RenderRequest::fit(700, 700));
    s.orientation = Orientation::Rotate270;
    check("embedded lens rotated", &src, &info, &s, &RenderRequest::fit(700, 700));
}

/// Issue #211: both renderers leave the same blank border out of the histogram.
#[test]
fn histograms_leave_out_the_same_blank_border() {
    if !gpu() {
        return;
    }
    let src = scene(2, 900, 600);
    let mut s = DevelopSettings::default();
    s.geometry.vertical = -40.0;
    s.geometry.rotate = 3.0;
    let req = RenderRequest::fit(700, 700);
    let cpu = render(&src, &SourceInfo::default(), &s, &req);
    let gpu = lcc_gpu::render(&src, &SourceInfo::default(), &s, &req, None).expect("gpu render");
    let cov = cpu.coverage.as_deref().expect("a blank border");
    assert_eq!(gpu.coverage.as_deref(), Some(cov), "the same pixels show the photo");
    assert_eq!(gpu.histogram.total, cpu.histogram.total);
    assert!((cpu.histogram.total as usize) < cpu.image.len());
}

#[test]
fn cached_renders_match_uncached() {
    if !gpu() {
        return;
    }
    // Slider drags reuse device-resident stages: the result must equal a fresh render.
    let src = scene(1, 900, 600);
    let info = SourceInfo { raw: true, ..Default::default() };
    let cache = StageCache::default();
    let req = RenderRequest::fit(640, 640);
    let mut s = DevelopSettings::default();
    typical(&mut s);
    for k in 0..4 {
        s.light.exposure = 0.1 * k as f64;
        s.effects.clarity = 10.0 + 5.0 * (k / 2) as f64;
        s.detail.nr_luminance = 20.0 + 10.0 * (k % 2) as f64;
        let warm = lcc_gpu::render(&src, &info, &s, &req, Some(&cache)).expect("gpu");
        let fresh = lcc_gpu::render(&src, &info, &s, &req, None).expect("gpu");
        assert_eq!(warm.image, fresh.image, "step {k}");
    }
}

/// Issue #632: the Highlights / Shadows base of each process (V1: the edge-aware filter of log
/// luminance; V2: of its fine median, a radius that grows with the render) matches the CPU's, and
/// a warm cache never hands one process the other's base.
#[test]
fn highlights_shadows_base_matches_on_every_process() {
    if !gpu() {
        return;
    }
    let raw = SourceInfo { raw: true, ..Default::default() };
    let src = scene(0, 1200, 800);
    let cache = StageCache::default();
    for &(w, h) in &[(360, 240), (1200, 800)] {
        let req = RenderRequest::fit(w, h);
        for p in Process::ALL {
            let mut s = DevelopSettings { process: p.version(), ..DevelopSettings::default() };
            s.light.highlights = -100.0;
            s.light.shadows = 60.0;
            check(&format!("{p:?} highlights/shadows {w}"), &src, &raw, &s, &req);
            typical(&mut s);
            check(&format!("{p:?} typical {w}"), &src, &raw, &s, &req);
            s.masks = vec![Mask {
                components: vec![MaskComponent {
                    anchor: None,
                    name: None,
                    op: MaskOp::Add,
                    invert: false,
                    shape: MaskShape::Linear { start: Point::new(0.5, 0.0), end: Point::new(0.5, 0.9) },
                }],
                adjust: lcc_develop::LocalAdjustments { highlights: -70.0, shadows: 40.0, ..Default::default() },
                ..Default::default()
            }];
            check(&format!("{p:?} local highlights {w}"), &src, &raw, &s, &req);
        }
        for p in [Process::V1, Process::V2, Process::V1, Process::V2] {
            let mut s = DevelopSettings { process: p.version(), ..DevelopSettings::default() };
            s.light.highlights = -100.0;
            let warm = lcc_gpu::render(&src, &raw, &s, &req, Some(&cache)).expect("gpu");
            let fresh = lcc_gpu::render(&src, &raw, &s, &req, None).expect("gpu");
            assert_eq!(warm.image, fresh.image, "{p:?} at {w} px, cached");
        }
    }
}

#[test]
fn overlays_match() {
    if !gpu() {
        return;
    }
    // Diagnostic overlays run on the finished 8-bit image, after either renderer.
    let src = scene(0, 900, 600);
    let raw = SourceInfo { raw: true, ..Default::default() };
    let s = DevelopSettings {
        point_colors: vec![lcc_develop::PointColor { lum: 0.6, chroma: 0.05, hue: 300.0, hue_shift: 50.0, range: 90.0, ..Default::default() }],
        ..Default::default()
    };
    let req = RenderRequest { overlay: lcc_pipeline::Overlay::PointColorRange(0), ..RenderRequest::fit(640, 640) };
    check("point color range overlay", &src, &raw, &s, &req);
    // Visualize Spots is a binary threshold of a high-pass: a 1-LSB difference near the threshold
    // flips a pixel, so compare the share of differing pixels instead of LSBs.
    let s = DevelopSettings::default();
    for t in [20u8, 50, 90] {
        let req = RenderRequest { overlay: lcc_pipeline::Overlay::Spots(t), ..RenderRequest::fit(640, 640) };
        let cpu = render(&src, &raw, &s, &req).image;
        let gpu = lcc_gpu::render(&src, &raw, &s, &req, None).expect("gpu").image;
        let differ = cpu.data.iter().zip(&gpu.data).filter(|(a, b)| a != b).count() as f64 / cpu.data.len() as f64;
        let white = cpu.data.iter().filter(|p| p[0] == 255).count() as f64 / cpu.data.len() as f64;
        eprintln!("visualize spots t={t:<3}          white {:.3}%  differing {:.4}%", white * 100.0, differ * 100.0);
        assert!(differ < 0.002, "spots t={t}: {differ}");
    }
    // Mask overlays: the alpha the GPU evaluated (or, for a hidden mask, the CPU's) drawn the same way.
    let mut s = DevelopSettings::default();
    let stroke = BrushStroke { points: vec![Point::new(0.2, 0.3), Point::new(0.6, 0.5)], size: 0.05, ..Default::default() };
    s.masks = vec![
        Mask {
            id: 1,
            components: vec![MaskComponent {
                anchor: None,
                name: None,
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::Radial { center: Point::new(0.4, 0.6), rx: 0.25, ry: 0.15, angle: 20.0, feather: 60.0, invert: false },
            }],
            ..Default::default()
        },
        Mask {
            id: 2,
            components: vec![MaskComponent {
                anchor: None,
                name: None,
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::Brush { strokes: vec![stroke] },
            }],
            ..Default::default()
        },
        Mask {
            id: 3,
            visible: false,
            components: vec![MaskComponent {
                anchor: None,
                name: None,
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::Linear { start: Point::new(0.5, 0.0), end: Point::new(0.5, 0.6) },
            }],
            ..Default::default()
        },
    ];
    s.masks[0].adjust.exposure = 0.5;
    use lcc_pipeline::{MaskView, Overlay};
    for (id, view) in
        [(1, MaskView::Color), (2, MaskView::ColorOnBw), (2, MaskView::WhiteOnBlack), (3, MaskView::ImageOnWhite), (1, MaskView::ImageOnBlack)]
    {
        let req = RenderRequest { overlay: Overlay::Mask { id, view, color: [230, 30, 40], opacity: 50 }, ..RenderRequest::fit(640, 640) };
        check(&format!("mask {id} overlay {}", view.name()), &src, &raw, &s, &req);
    }
}

#[test]
fn red_eye_matches() {
    if !gpu() {
        return;
    }
    // A face-like patch with a red pupil (left) and a glowing pet pupil (right).
    let src = Arc::new(Rgb32f::from_fn(800, 400, |x, y| {
        let d = |cx: f32, cy: f32| ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
        if d(250.0, 200.0) < 20.0 {
            [0.55, 0.05, 0.04]
        } else if d(560.0, 190.0) < 24.0 {
            [0.5, 0.7, 0.3]
        } else if d(250.0, 200.0) < 40.0 || d(560.0, 190.0) < 45.0 {
            [0.12, 0.1, 0.08]
        } else {
            [0.45, 0.3, 0.22]
        }
    }));
    let info = SourceInfo::default();
    let eyes = vec![
        lcc_develop::RedEye { center: Point::new(0.31, 0.5), rx: 0.06, ry: 0.05, darken: 70.0, ..Default::default() },
        lcc_develop::RedEye {
            center: Point::new(0.7, 0.48),
            rx: 0.06,
            ry: 0.06,
            pupil_size: 60.0,
            pet: true,
            catchlight: Some(Point::new(-0.35, -0.35)),
            ..Default::default()
        },
    ];
    let s = DevelopSettings { red_eye: eyes.clone(), ..Default::default() };
    check("red + pet eye", &src, &info, &s, &RenderRequest::fit(800, 800));
    let g = lcc_gpu::render(&src, &info, &s, &RenderRequest::fit(800, 800), None).expect("gpu").image;
    let (red, pet) = (g.data[200 * 800 + 250], g.data[190 * 800 + 560]);
    assert!(red[0] < 80 && red[0].abs_diff(red[1]) < 10, "red pupil fixed on the GPU: {red:?}");
    assert!(pet[1] < 80 && pet[0].abs_diff(pet[1]) < 5, "pet pupil darkened on the GPU: {pet:?}");
    check("red + pet eye (small)", &src, &info, &s, &RenderRequest::fit(300, 300));
    // with a spot the scene-linear stage runs on the CPU (eyes included)
    let s = DevelopSettings {
        red_eye: eyes,
        spots: vec![Spot { points: vec![Point::new(0.1, 0.2)], size: 0.02, source_offset: Some(Point::new(0.05, 0.0)), ..Default::default() }],
        ..Default::default()
    };
    check("red eye + spot (cpu stage)", &src, &info, &s, &RenderRequest::fit(600, 600));
}

fn saturated(s: &mut DevelopSettings) {
    typical(s);
    s.color.saturation = 70.0;
    s.color.vibrance = 40.0;
    s.curve.highlights = 30.0;
    s.curve.shadows = -20.0;
    // the value-noise grain (processes V1–V6)
    s.process = lcc_develop::Process::V6.version();
    s.grain.amount = 30.0;
}

#[test]
fn output_spaces_match() {
    if !gpu() {
        return;
    }
    // Export colour spaces: matrix to the target primaries, gamut mapping into the target gamut and
    // its encoding curve, on both renderers. Saturated edits push colours to the gamut edges.
    use lcc_pipeline::OutputSpace;
    let src = scene(0, 900, 600);
    let raw = SourceInfo { raw: true, ..Default::default() };
    for space in OutputSpace::ALL {
        for (name, edit) in [("typical", typical as Edit), ("saturated + curves + grain", saturated as Edit)] {
            let mut s = DevelopSettings::default();
            edit(&mut s);
            let req = RenderRequest { space, ..RenderRequest::fit(640, 640) };
            check(&format!("{space:?} {name}"), &src, &raw, &s, &req);
        }
    }
}

#[test]
fn tint_directions_match_cpu_for_raw_and_rendered_sources() {
    if !gpu() {
        return;
    }
    let src = Arc::new(Rgb32f::filled(64, 64, [0.18; 3]));
    for info in [
        SourceInfo::default(),
        SourceInfo { raw: true, relative_wb: true, ..Default::default() },
        SourceInfo { raw: true, as_shot_temp: 4200.0, as_shot_tint: 15.0, ..Default::default() },
    ] {
        for delta in [-50.0, 50.0] {
            let mut s = DevelopSettings::default();
            s.wb.mode = WbMode::Custom;
            s.wb.temp = info.as_shot_temp;
            s.wb.tint = info.as_shot_tint + delta;
            check("tint direction", &src, &info, &s, &RenderRequest::fit(64, 64));
        }
    }
}

/// Issue #323: windows of a zoomed frame. The GPU renders the window like the CPU does: the
/// vignette and airlight belong to the whole frame, spots grow the window they work on.
#[test]
fn windows_match() {
    if !gpu() {
        return;
    }
    use lcc_pipeline::PixelWindow;
    let src = scene(1, 960, 640);
    let info = SourceInfo { raw: true, ..Default::default() };
    let win = PixelWindow { x: 720, y: 480, w: 480, h: 360 };
    let req = RenderRequest { window: Some(win), ..RenderRequest::fit(1800, 1200) };
    let mut cases = cases();
    cases.push(("vignette at the corner", |s| {
        s.vignette.amount = -80.0;
        s.vignette.midpoint = 10.0;
    }));
    cases.push(("spot reading from outside", |s| {
        s.spots.push(Spot {
            points: vec![Point::new(0.45, 0.45)],
            size: 0.03,
            feather: 30.0,
            opacity: 100.0,
            source_offset: Some(Point::new(0.15, 0.0)),
            auto: None,
            ..Default::default()
        });
    }));
    for (name, edit) in cases {
        let mut s = DevelopSettings::default();
        edit(&mut s);
        let name = format!("window: {name}");
        if name.contains("spot") {
            // Heal divides by its blurred source, so float rounding (Metal) moves a few pixels by
            // more than max_lsb(); a window missing part of the spot's ring moves many: hold the mean
            // ten times tighter instead
            check_spot(&name, &src, &info, &s, &req);
        } else {
            check(&name, &src, &info, &s, &req);
        }
    }
    // the corner window (where the vignette is strong)
    let mut s = DevelopSettings::default();
    s.vignette.amount = -80.0;
    s.vignette.midpoint = 10.0;
    let corner = RenderRequest { window: Some(PixelWindow { x: 1320, y: 840, w: 480, h: 360 }), ..RenderRequest::fit(1800, 1200) };
    check("window: vignette corner", &src, &info, &s, &corner);
}

/// A zoomed window of a photo with a dress brushed over (thousands of dabs, TEST-NOTES 2026-10-16
/// item 28: 9–55 s a window): the brush kernel looks only at the dabs near each pixel, matches the
/// CPU, and a window of a few megapixels takes well under a second (`LC_BRUSH_BENCH=1` prints the
/// times at 45 MP, 100 %).
#[test]
fn a_zoomed_window_of_a_big_brush_matches_and_is_quick() {
    if !gpu() {
        return;
    }
    use lcc_pipeline::PixelWindow;
    let dress = |auto_mask: bool| {
        let mut strokes = Vec::new();
        for k in 0..12 {
            let y0 = 0.25 + 0.04 * k as f64;
            let points: Vec<Point> = (0..=200)
                .map(|i| {
                    let t = i as f64 / 200.0;
                    Point::new(0.3 + 0.4 * (t * 6.0 * std::f64::consts::PI).sin().abs(), y0 + 0.08 * t)
                })
                .collect();
            strokes.push(BrushStroke { points, size: 0.02, auto_mask, ..Default::default() });
        }
        DevelopSettings {
            masks: vec![Mask {
                components: vec![MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape: MaskShape::Brush { strokes } }],
                adjust: lcc_develop::LocalAdjustments { exposure: 1.0, saturation: -40.0, ..Default::default() },
                ..Default::default()
            }],
            ..Default::default()
        }
    };
    let bench = std::env::var_os("LC_BRUSH_BENCH").is_some();
    let (fw, fh) = if bench { (8192, 5464) } else { (4096, 2732) };
    let src = scene(1, if bench { 2048 } else { 960 }, if bench { 1366 } else { 640 });
    let info = SourceInfo { raw: true, ..Default::default() };
    let win = if bench { PixelWindow { x: 3000, y: 2000, w: 2048, h: 1792 } } else { PixelWindow { x: 1500, y: 1000, w: 1024, h: 896 } };
    let req = RenderRequest { window: Some(win), ..RenderRequest::fit(fw, fh) };
    for auto in [false, true] {
        let s = dress(auto);
        // (the first render builds the kernels and uploads the source)
        let _ = lcc_gpu::render(&src, &info, &s, &req, None).expect("gpu render");
        let t = std::time::Instant::now();
        let gpu = lcc_gpu::render(&src, &info, &s, &req, None).expect("gpu render");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        eprintln!("brushed dress window {}x{} (auto {auto}): {ms:.0} ms on the GPU", gpu.image.width, gpu.image.height);
        if !bench {
            check(&format!("big brush window (auto {auto})"), &src, &info, &s, &req);
        }
        // a speed budget for hardware; a software rasterizer takes seconds
        assert!(ms < 3000.0 || lcc_gpu::software_adapter(), "a zoomed brush window took {ms:.0} ms");
    }
}

/// Issue #323: a window of a 24 MP photo is cut from a source of 288 MB on the device. Keeping that
/// upload with the view's stages counted it against the stage budget on every tick of a drag and
/// got the stages cleared, and the upload and resampling redone: the window's stages are its own
/// pixels, not the photo's.
// (these two share the process's one big source: a test lock keeps them from replacing each other's)
static BIG_SOURCE_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn a_window_does_not_keep_the_whole_source_on_the_device() {
    let _lock = BIG_SOURCE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    if !gpu() {
        return;
    }
    if lcc_gpu::software_adapter() {
        // WARP refuses the 288 MB source buffer (its limits are below a hardware card's)
        lcc_testkit::skip!(Gpu, "a 24 MP source on the device needs a hardware adapter");
        return;
    }
    use lcc_pipeline::PixelWindow;
    let src = scene(1, 6000, 4000);
    let info = SourceInfo { raw: true, ..Default::default() };
    let stages = StageCache::default();
    let s = DevelopSettings::default();
    let req = RenderRequest { window: Some(PixelWindow { x: 2000, y: 1500, w: 640, h: 480 }), ..RenderRequest::fit(6000, 4000) };
    let _ = lcc_gpu::render(&src, &info, &s, &req, Some(&stages)).expect("gpu render");
    let held = lcc_gpu::stage_bytes(&stages);
    assert!(held < 100 << 20, "the view's GPU stages hold {} MiB for a 640×480 window", held >> 20);
    // …and the next tick of a drag reuses what is there
    let mut s2 = s.clone();
    s2.light.exposure = 0.5;
    let again = lcc_gpu::render(&src, &info, &s2, &req, Some(&stages)).expect("gpu render");
    assert_eq!((again.image.width, again.image.height), (640, 480));
    assert!(lcc_gpu::stage_bytes(&stages) < 100 << 20);
}

/// …but the 288 MB is uploaded once for every window of every view of the photo, not once per
/// window: panning at 1:1, and the Before and After windows, share one device copy of it, which
/// goes when the app idles (`trim_pool(0)`).
#[test]
fn windows_of_a_big_photo_share_one_uploaded_source() {
    let _lock = BIG_SOURCE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    if !gpu() {
        return;
    }
    use lcc_pipeline::PixelWindow;
    let src = scene(1, 6000, 4000);
    let info = SourceInfo { raw: true, ..Default::default() };
    let s = DevelopSettings::default();
    let (after, before) = (StageCache::default(), StageCache::default());
    let at = |x: usize| RenderRequest { window: Some(PixelWindow { x, y: 1500, w: 640, h: 480 }), ..RenderRequest::fit(6000, 4000) };
    let start = lcc_gpu::source_uploads();
    for (cache, x) in [(&after, 1000), (&after, 1256), (&after, 1512), (&before, 1512), (&before, 1768)] {
        lcc_gpu::render(&src, &info, &s, &at(x), Some(cache)).expect("gpu render");
    }
    assert_eq!(lcc_gpu::source_uploads() - start, 1, "one upload for five windows of two views");
    assert!(lcc_gpu::shared_source_bytes() >= 280_000_000, "{}", lcc_gpu::shared_source_bytes());
    assert!(lcc_gpu::stage_bytes(&after) < 100 << 20, "and it is not in the views' stages");
    lcc_gpu::trim_pool(0);
    assert_eq!(lcc_gpu::shared_source_bytes(), 0, "an idle app gives it back");
    lcc_gpu::render(&src, &info, &s, &at(1000), Some(&after)).expect("gpu render");
    assert_eq!(lcc_gpu::source_uploads() - start, 2);
}

/// The histogram binned on the GPU is exactly the CPU oracle's histogram of the image the GPU
/// returned: channels, chroma, vectorscope and waveform, sampled (> 1 MP) or not, with and without
/// a blank border.
#[test]
fn gpu_histogram_matches_the_cpu_oracle() {
    if !gpu() {
        return;
    }
    let mut bordered = DevelopSettings::default();
    bordered.geometry.vertical = -40.0;
    bordered.geometry.rotate = 3.0;
    let mut edited = DevelopSettings::default();
    edited.light.exposure = 0.7;
    edited.color.saturation = 40.0;
    for (name, s, w, h) in [("plain", DevelopSettings::default(), 320, 240), ("bordered", bordered, 900, 600), ("sampled", edited, 1600, 1200)] {
        let src = scene(2, w, h);
        let r = lcc_gpu::render(&src, &SourceInfo::default(), &s, &RenderRequest::fit(w, h), None).expect("gpu render");
        let oracle = lcc_raster::Histogram::of_srgb8_covered(&r.image, r.coverage.as_deref());
        assert!(r.histogram.total > 0 && r.histogram.has_waveform(), "{name}: empty histogram");
        assert_eq!(r.histogram, oracle, "{name}: GPU histogram differs from the CPU oracle");
    }
}

/// An HDR render's linear float samples encoded with the sRGB curve extended past 1 (×255), so the
/// part above SDR white compares in the same 8-bit steps as the rest.
fn hdr_codes(r: &lcc_pipeline::Rendered) -> Vec<f32> {
    let Some(lcc_pipeline::DeepImage { samples: lcc_pipeline::DeepSamples::F32(v), .. }) = &r.deep else { panic!("no float HDR samples") };
    v.iter().map(|&x| lcc_color::transfer::linear_to_srgb(x.max(0.0)) * 255.0).collect()
}

fn check_hdr(name: &str, src: &Arc<Rgb32f>, info: &SourceInfo, s: &DevelopSettings, w: usize, h: usize) {
    let req = RenderRequest { depth: lcc_pipeline::OutputDepth::F32Hdr, ..RenderRequest::fit(w, h) };
    check_hdr_req(name, src, info, s, &req);
}

fn check_hdr_req(name: &str, src: &Arc<Rgb32f>, info: &SourceInfo, s: &DevelopSettings, req: &RenderRequest) {
    let (w, h) = (req.max_w, req.max_h);
    let cpu = render(src, info, s, req);
    let gpu = lcc_gpu::try_render(src, info, s, req, None).expect("the GPU renders HDR requests");
    let (a, b) = (hdr_codes(&cpu), hdr_codes(&gpu));
    assert_eq!(a.len(), b.len(), "{name}: same size");
    let (mut sum, mut max) = (0.0f64, 0.0f32);
    for (p, q) in a.iter().zip(&b) {
        let d = (p - q).abs();
        sum += d as f64;
        max = max.max(d);
    }
    let mean = sum / a.len() as f64;
    let peak = s.hdr.peak();
    let above = a.iter().filter(|&&c| c > 255.5).count() as f64 / a.len() as f64;
    eprintln!("{name:<28} {w}x{h}  peak {peak:.2}  mean {mean:.4}  max {max:.2}  above SDR white {:.2}%", above * 100.0);
    assert!(peak <= 1.0 || above > 0.0, "{name}: the case reaches above SDR white");
    assert!(mean < mean_lsb() && max <= max_lsb() as f32, "{name}: mean {mean:.4}, max {max:.2} (8-bit steps)");
    let (m8, x8, _) = diff(&cpu.image, &gpu.image);
    assert!(m8 < mean_lsb() && x8 <= max_lsb(), "{name}: 8-bit reduction mean {m8:.4}, max {x8}");
}

// Given an HDR edit, the GPU's float HDR render matches the CPU's (the reference) above and below
// SDR white, through the tone map, highlight edits, vignette, curves and grain
#[test]
fn hdr_renders_match() {
    if !gpu() {
        return;
    }
    let src = scene(2, 320, 240);
    let raw = SourceInfo { raw: true, ..Default::default() };
    let mut s = DevelopSettings::default();
    s.hdr.enabled = true;
    s.light.exposure = 1.5;
    check_hdr("hdr tone map", &src, &raw, &s, 320, 240);
    s.hdr.max_ev = 2.0;
    s.light.contrast = 30.0;
    s.light.highlights = -40.0;
    s.light.whites = 25.0;
    s.light.blacks = -20.0;
    check_hdr("hdr light edits", &src, &raw, &s, 320, 240);
    s.vignette.amount = 40.0;
    s.effects.clarity = 30.0;
    s.curve.master = vec![Point::new(0.0, 0.0), Point::new(0.5, 0.6), Point::new(1.0, 1.0)];
    check_hdr("hdr vignette + curve", &src, &raw, &s, 320, 240);
    // the value-noise grain (processes V1–V6)
    s.process = lcc_develop::Process::V6.version();
    s.grain.amount = 30.0;
    s.color.saturation = 20.0;
    check_hdr("hdr grain + colour", &src, &raw, &s, 320, 240);
    // a display-referred source and a camera look take the other tone-map arms
    check_hdr("hdr display source", &src, &SourceInfo::default(), &s, 320, 240);
    let curve = lcc_pipeline::tone::CameraTone::default_raw().expect("default camera tone");
    check_hdr("hdr camera tone", &src, &SourceInfo { raw: true, camera_tone: Some(curve), ..Default::default() }, &s, 320, 240);
    // an HDR request of an SDR edit: the float render clipped at SDR white, as on the CPU
    check_hdr("hdr request, SDR edit", &src, &raw, &DevelopSettings { light: s.light, ..Default::default() }, 320, 240);
}

/// HDR render time, CPU vs GPU (`cargo test -p lcc-gpu --test equivalence hdr_render_time -- --ignored --nocapture`).
#[test]
#[ignore = "timing, run by hand"]
fn hdr_render_time() {
    if !gpu() {
        return;
    }
    let (w, h) = (3000, 2000);
    let src = scene(2, w, h);
    let info = SourceInfo { raw: true, ..Default::default() };
    let mut s = DevelopSettings::default();
    s.hdr.enabled = true;
    s.light.exposure = 1.0;
    s.light.highlights = -30.0;
    s.effects.clarity = 20.0;
    let req = RenderRequest { depth: lcc_pipeline::OutputDepth::F32Hdr, ..RenderRequest::fit(w, h) };
    let time = |f: &dyn Fn()| {
        f();
        let t = std::time::Instant::now();
        for _ in 0..3 {
            f();
        }
        t.elapsed().as_secs_f64() * 1e3 / 3.0
    };
    let cpu = time(&|| drop(render(&src, &info, &s, &req)));
    let gpu = time(&|| drop(lcc_gpu::try_render(&src, &info, &s, &req, None).expect("gpu")));
    eprintln!("HDR render {w}x{h}: CPU {cpu:.0} ms, GPU {gpu:.0} ms");
}

/// Raw decoding keeps negative channels (near-black sensor noise through a saturating camera matrix, a NIKON D780
/// profile's): pushed and edited, the GPU renders them as the CPU does.
#[test]
fn negative_channels_match_cpu() {
    if !gpu() {
        return;
    }
    let m = [[3.48f32, -2.41, -0.06], [-0.41, 1.57, -0.16], [0.32, -1.86, 2.58]];
    let noise = |x: usize, y: usize, c: usize| {
        let h = ((x * 7919 + y * 104_729 + c * 1_299_709) as u32).wrapping_mul(2_654_435_761);
        (h >> 8) as f32 / (1u32 << 24) as f32 * 0.008 - 0.004
    };
    let src = Arc::new(Rgb32f::from_fn(320, 240, |x, y| {
        let g = 0.0005 * 1.02f32.powi(x as i32);
        let cam: [f32; 3] = std::array::from_fn(|c| g + noise(x, y, c));
        std::array::from_fn(|i| m[i][0] * cam[0] + m[i][1] * cam[1] + m[i][2] * cam[2])
    }));
    assert!(src.data.iter().flatten().any(|v| *v < 0.0));
    let info = SourceInfo { raw: true, ..Default::default() };
    let req = RenderRequest::fit(320, 240);
    for (name, edit) in cases() {
        let mut s = DevelopSettings::default();
        edit(&mut s);
        s.light.exposure += 4.0;
        check(&format!("negatives +4 EV, {name}"), &src, &info, &s, &req);
    }
}

#[test]
fn soft_proofs_match() {
    if !gpu() {
        return;
    }
    // Soft proofing against an output space (matrix proofs): into the proof gamut, back to the
    // output space, with the destination and display gamut warnings painted alike on both renderers.
    // Saturated edits put plenty of colours outside the smaller proof spaces.
    use lcc_pipeline::{OutputSpace, Proof};
    let src = scene(0, 900, 600);
    let raw = SourceInfo { raw: true, ..Default::default() };
    for space in [OutputSpace::Srgb, OutputSpace::AdobeRgb, OutputSpace::DisplayP3] {
        for (dest_warning, display_warning) in [(false, false), (true, false), (false, true), (true, true)] {
            for out in [OutputSpace::Srgb, OutputSpace::Rec2020] {
                let mut s = DevelopSettings::default();
                saturated(&mut s);
                let proof = Some(Proof { space, dest_warning, display_warning, profile: 0 });
                let req = RenderRequest { space: out, proof, ..RenderRequest::fit(640, 640) };
                let name = format!("proof {space:?} → {out:?} warn {dest_warning}/{display_warning}");
                if dest_warning || display_warning {
                    check_warned(&name, &src, &raw, &s, &req);
                } else {
                    check(&name, &src, &raw, &s, &req);
                }
            }
        }
    }
    // the case is meaningful: proofing sRGB from Rec. 2020 output warns on some pixels, and the GPU paints the same ones
    let mut s = DevelopSettings::default();
    saturated(&mut s);
    let req = RenderRequest {
        space: OutputSpace::Rec2020,
        proof: Some(Proof { space: OutputSpace::Srgb, dest_warning: true, display_warning: false, profile: 0 }),
        ..RenderRequest::fit(640, 640)
    };
    let warned = |i: &Rgba8| i.data.iter().filter(|p| p[1] == 0 && p[2] == 0 && p[0] > 200).count();
    let (cpu, gpu) = (render(&src, &raw, &s, &req).image, lcc_gpu::render(&src, &raw, &s, &req, None).expect("gpu render").image);
    assert!(warned(&cpu) > 100, "{} warned pixels", warned(&cpu));
    let (a, b) = (warned(&cpu), warned(&gpu));
    assert!(a.abs_diff(b) as f64 <= 1e-4 * cpu.data.len() as f64, "warned pixels: CPU {a}, GPU {b}");
    // an output-device profile (a 3D table) stays on the CPU
    let n = 2;
    let lut = lcc_pipeline::ProofLut::new(n, vec![[0.5, 0.5, 0.5, 0.0]; n * n * n]).unwrap();
    let id = lcc_pipeline::register_proof_profile(0x5eed_9f00, lut);
    let req = RenderRequest {
        proof: Some(Proof { space: OutputSpace::Srgb, dest_warning: true, display_warning: false, profile: id }),
        ..RenderRequest::fit(64, 64)
    };
    assert!(lcc_gpu::render(&src, &raw, &DevelopSettings::default(), &req, None).is_none(), "declined: the CPU renders it");
}

/// Soft proofing an HDR edit: the proof gamut map limits colours to the HDR peak, not SDR white, on both renderers
/// (process V5 keeps the hue of over-range colours inside the proof space too).
#[test]
fn hdr_soft_proofs_match() {
    if !gpu() {
        return;
    }
    use lcc_pipeline::{OutputSpace, Proof};
    let src = scene(0, 320, 240);
    let raw = SourceInfo { raw: true, ..Default::default() };
    for p in Process::ALL {
        for (space, warn) in [(OutputSpace::Srgb, false), (OutputSpace::DisplayP3, false), (OutputSpace::Srgb, true)] {
            let mut s = DevelopSettings { process: p.version(), ..DevelopSettings::default() };
            saturated(&mut s);
            s.hdr.enabled = true;
            s.light.exposure = 2.0;
            let req = RenderRequest {
                depth: lcc_pipeline::OutputDepth::F32Hdr,
                space: OutputSpace::Rec2020,
                proof: Some(Proof { space, dest_warning: warn, display_warning: warn, profile: 0 }),
                ..RenderRequest::fit(320, 240)
            };
            check_hdr_req(&format!("hdr proof {space:?} warn {warn}, {p:?}"), &src, &raw, &s, &req);
        }
    }
}

/// Non-finite and huge source values (a corrupt raw, an overflowing edit) through process V5's hue clip render as on
/// the CPU, where the GPU can represent them: the guards for a NaN or infinite brightest or darkest channel match.
#[test]
fn non_finite_colours_match_cpu() {
    if !gpu() {
        return;
    }
    let vals = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e30, -1e30, 4.0, 0.2];
    let src = Arc::new(Rgb32f::from_fn(343, 7, |x, _| [vals[x % 7], vals[(x / 7) % 7], vals[(x / 49) % 7]]));
    let info = SourceInfo { raw: true, ..Default::default() };
    let mut s = DevelopSettings::default();
    s.light.exposure = 1.0;
    check("non-finite, SDR", &src, &info, &s, &RenderRequest::fit(343, 7));
    s.hdr.enabled = true;
    check_hdr("non-finite, HDR", &src, &info, &s, 343, 7);
}

/// Colours pushed past white (process V5 keeps their hue, older processes move them toward their luminance) match
/// the CPU's on every process.
#[test]
fn overrange_colours_match_on_every_process() {
    if !gpu() {
        return;
    }
    let raw = SourceInfo { raw: true, ..Default::default() };
    let src = scene(0, 900, 600);
    let req = RenderRequest::fit(640, 640);
    for p in Process::ALL {
        let mut s = DevelopSettings { process: p.version(), ..DevelopSettings::default() };
        s.light.exposure = 2.5;
        s.color.saturation = 40.0;
        check(&format!("{p:?} pushed past white"), &src, &raw, &s, &req);
    }
}

/// The loupe's display fit done on the device (`try_render_fitted`) gives the pixels the CPU's
/// `fit_rgba8` gives the same render (it rounds alike; only an FMA's last bit may differ), and the
/// histogram of the whole render, not of the fitted image. A fit that shrinks nothing, an overlay
/// and an HDR render come back unfitted, for the caller.
#[test]
fn the_display_fit_on_the_device_matches_the_cpus() {
    if !gpu() {
        return;
    }
    let src = scene(1, 1280, 853);
    let info = SourceInfo::default();
    let mut s = DevelopSettings::default();
    typical(&mut s);
    let req = RenderRequest::fit(1280, 1280);
    let whole = lcc_gpu::try_render(&src, &info, &s, &req, None).expect("gpu render");
    for fit in [(853, 853), (640, 480), (1001, 667), (97, 64)] {
        let on_device = lcc_gpu::try_render_fitted(&src, &info, &s, &req, None, Some(fit)).expect("gpu render");
        let on_cpu = lcc_raster::resample::fit_rgba8(&whole.image, fit.0, fit.1, lcc_raster::resample::Filter::Lanczos3);
        let (mean, max, _) = diff(&on_device.image, &on_cpu);
        eprintln!("display fit {fit:?}: {}x{}  mean {mean:.4}  max {max}", on_cpu.width, on_cpu.height);
        assert!(max <= 1 && mean < 0.01, "{fit:?}: mean {mean:.4} LSB, max {max} LSB");
        assert!(on_device.image.data.iter().all(|p| p[3] == 255), "opaque");
        assert_eq!(on_device.histogram.total, whole.histogram.total, "{fit:?}: the whole render's histogram");
        assert_eq!(on_device.histogram.luma, whole.histogram.luma, "{fit:?}");
    }
    // kept for a view cache: the fitted image as above, and the full render as it is
    let (fitted, full) = lcc_gpu::try_render_fitted_keeping_full(&src, &info, &s, &req, None, (640, 480)).expect("gpu render");
    let alone = lcc_gpu::try_render_fitted(&src, &info, &s, &req, None, Some((640, 480))).expect("gpu render");
    assert_eq!(fitted.image, alone.image, "the same fitted image");
    assert_eq!(full.as_ref(), Some(&whole.image), "the full render read back with it");
    let (_, none) = lcc_gpu::try_render_fitted_keeping_full(&src, &info, &s, &req, None, (4000, 4000)).expect("gpu render");
    assert!(none.is_none(), "nothing fitted: the render is the full image");
    let larger = lcc_gpu::try_render_fitted(&src, &info, &s, &req, None, Some((4000, 4000))).expect("gpu render");
    assert_eq!((larger.image.width, larger.image.height), (whole.image.width, whole.image.height), "never enlarged");
    let overlay = RenderRequest { overlay: lcc_pipeline::Overlay::Spots(50), ..req };
    if let Ok(r) = lcc_gpu::try_render_fitted(&src, &info, &s, &overlay, None, Some((640, 480))) {
        assert_eq!(r.image.width, whole.image.width, "an overlay is drawn on the CPU, after the fit there");
    }
}

/// A non-trivial 3D LUT (cross-process look: lifted blacks, warm highlights).
fn look_lut(n: usize, domain_max: f32) -> lcc_pipeline::lut::Lut3d {
    let mut data = Vec::with_capacity(n * n * n);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let c = [r, g, b].map(|v| v as f32 / (n - 1) as f32 * domain_max);
                data.push([
                    (0.06 + 0.9 * c[0].powf(0.9) + 0.05 * c[2]).min(1.0),
                    (0.04 + 0.92 * c[1] * (1.0 - 0.1 * c[2])).min(1.0),
                    (0.1 + 0.6 * c[2] + 0.2 * c[0] * c[1]).min(1.0),
                ]);
            }
        }
    }
    lcc_pipeline::lut::Lut3d { size: n, data, domain_min: [0.0; 3], domain_max: [domain_max; 3], title: None }
}

/// LUT profiles render on the GPU (the table is applied after grain, blended by the amount).
#[test]
fn lut_profiles_match() {
    if !gpu() {
        return;
    }
    let src = scene(1, 900, 600);
    let raw = SourceInfo { raw: true, ..Default::default() };
    lcc_pipeline::lut::register("lut:gpu-test-17", look_lut(17, 1.0));
    lcc_pipeline::lut::register("lut:gpu-test-domain", look_lut(9, 1.2));
    for (id, amount) in [("lut:gpu-test-17", 100.0), ("lut:gpu-test-17", 160.0), ("lut:gpu-test-domain", 60.0)] {
        for (name, edit) in [("default", (|_| {}) as Edit), ("saturated", saturated as Edit)] {
            let mut s = DevelopSettings::default();
            edit(&mut s);
            s.profile.id = id.into();
            s.profile.amount = amount;
            check(&format!("{id} {amount} {name}"), &src, &raw, &s, &RenderRequest::fit(640, 640));
        }
    }
    // the case is meaningful: the LUT changes the image
    let s = DevelopSettings::default();
    let looked = DevelopSettings { profile: lcc_develop::Profile { id: "lut:gpu-test-17".into(), amount: 100.0, ..s.profile.clone() }, ..s.clone() };
    let gpu = |s: &DevelopSettings| lcc_gpu::render(&src, &raw, s, &RenderRequest::fit(320, 320), None).expect("gpu render").image;
    assert!(diff(&gpu(&s), &gpu(&looked)).1 > 10, "the LUT applies");
}

/// Refine Edges (the cross guided filter, blended by √refine) and Feather (a Gaussian of the
/// combined alpha) on a mask render on the GPU, before the mask's invert and amount.
#[test]
fn refine_edges_and_feather_match() {
    if !gpu() {
        return;
    }
    let src = scene(0, 900, 600);
    let raw = SourceInfo { raw: true, ..Default::default() };
    for (refine, feather) in [(15.0, 0.0), (60.0, 0.0), (100.0, 0.0), (0.0, 40.0), (50.0, 80.0)] {
        let mut s = DevelopSettings::default();
        typical(&mut s);
        s.masks = vec![
            Mask {
                components: vec![MaskComponent {
                    anchor: None,
                    name: None,
                    op: MaskOp::Add,
                    invert: false,
                    shape: MaskShape::Radial { center: Point::new(0.45, 0.55), rx: 0.3, ry: 0.2, angle: 10.0, feather: 30.0, invert: false },
                }],
                adjust: lcc_develop::LocalAdjustments { exposure: 0.9, saturation: -30.0, ..Default::default() },
                refine,
                feather,
                ..Default::default()
            },
            Mask {
                components: vec![MaskComponent {
                    anchor: None,
                    name: None,
                    op: MaskOp::Add,
                    invert: false,
                    shape: MaskShape::Linear { start: Point::new(0.5, 0.0), end: Point::new(0.5, 0.5) },
                }],
                adjust: lcc_develop::LocalAdjustments { exposure: -0.7, amount: 70.0, ..Default::default() },
                refine: refine / 2.0,
                feather: feather / 2.0,
                invert: true,
                ..Default::default()
            },
        ];
        check(&format!("refine {refine} feather {feather}"), &src, &raw, &s, &RenderRequest::fit(640, 640));
        let gpu = |s: &DevelopSettings| lcc_gpu::render(&src, &raw, s, &RenderRequest::fit(640, 640), None).expect("gpu render").image;
        let plain = DevelopSettings { masks: s.masks.iter().map(|m| Mask { refine: 0.0, feather: 0.0, ..m.clone() }).collect(), ..s.clone() };
        // (a light refine barely moves a soft radial edge; a strong one or a feather visibly does. On
        // process V7: the V8 highlight roll-off compresses the brightened highlights the change shows in)
        let v7 = |x: &DevelopSettings| DevelopSettings { process: Process::V7.version(), ..x.clone() };
        assert!(
            refine < 50.0 && feather == 0.0 || diff(&gpu(&v7(&s)), &gpu(&v7(&plain))).1 > 3,
            "refine {refine} / feather {feather} change the mask"
        );
    }
}

/// Spot removal on the device: Clone, Heal and Remove (automatic source, scored on the device),
/// brushed strokes, overlapping spots (each reads what the earlier ones left), feather 0 and
/// spots cut by the image edge.
#[test]
fn spots_match() {
    if !gpu() {
        return;
    }
    use lcc_develop::SpotMode;
    let src = scene(3, 900, 600);
    let raw = SourceInfo { raw: true, ..Default::default() };
    let spot = |mode, pts: &[(f64, f64)], size, feather, off: Option<(f64, f64)>| Spot {
        mode,
        points: pts.iter().map(|p| Point::new(p.0, p.1)).collect(),
        size,
        feather,
        opacity: 100.0,
        source_offset: off.map(|o| Point::new(o.0, o.1)),
        ..Default::default()
    };
    let sets: Vec<(&str, Vec<Spot>)> = vec![
        ("clone", vec![spot(SpotMode::Clone, &[(0.3, 0.4)], 0.04, 40.0, Some((0.15, 0.05)))]),
        ("heal", vec![spot(SpotMode::Heal, &[(0.6, 0.5)], 0.05, 60.0, Some((-0.12, 0.1)))]),
        ("remove (auto source)", vec![spot(SpotMode::Remove, &[(0.5, 0.45)], 0.03, 50.0, None)]),
        ("heal (auto source)", vec![spot(SpotMode::Heal, &[(0.5, 0.45)], 0.03, 50.0, None)]),
        ("remove near the edge (auto)", vec![spot(SpotMode::Remove, &[(0.04, 0.5)], 0.03, 50.0, None)]),
        ("brushed heal stroke", vec![spot(SpotMode::Heal, &[(0.2, 0.7), (0.3, 0.72), (0.4, 0.69)], 0.02, 30.0, Some((0.0, -0.15)))]),
        ("feather 0, half opacity", vec![Spot { opacity: 50.0, ..spot(SpotMode::Clone, &[(0.7, 0.3)], 0.04, 0.0, Some((-0.2, 0.0))) }]),
        ("cut by the edge", vec![spot(SpotMode::Heal, &[(0.99, 0.02)], 0.05, 50.0, Some((-0.2, 0.2)))]),
        (
            "overlapping, auto after manual",
            vec![
                spot(SpotMode::Clone, &[(0.45, 0.5)], 0.04, 50.0, Some((0.1, 0.0))),
                spot(SpotMode::Remove, &[(0.47, 0.52)], 0.04, 50.0, None),
                spot(SpotMode::Heal, &[(0.5, 0.5)], 0.03, 70.0, Some((0.0, 0.12))),
            ],
        ),
    ];
    for (name, spots) in sets {
        let mut s = DevelopSettings::default();
        typical(&mut s);
        s.spots = spots;
        check(&format!("spots: {name}"), &src, &raw, &s, &RenderRequest::fit(640, 640));
    }
}

/// Defringe on the device: purple and green at several amounts and hue ranges, at preview and
/// full size (its filter radius follows the output size).
#[test]
fn defringe_matches() {
    if !gpu() {
        return;
    }
    let src = scene(1, 1800, 1200);
    let raw = SourceInfo { raw: true, ..Default::default() };
    for (purple, green, hue) in [(4.0, 0.0, (30.0, 70.0)), (0.0, 10.0, (20.0, 80.0)), (20.0, 15.0, (0.0, 100.0))] {
        let mut s = DevelopSettings::default();
        typical(&mut s);
        s.color.saturation = 40.0;
        s.optics.defringe_purple_amount = purple;
        s.optics.defringe_green_amount = green;
        s.optics.defringe_purple_hue_lo = hue.0;
        s.optics.defringe_purple_hue_hi = hue.1;
        s.optics.defringe_green_hue_lo = hue.0;
        s.optics.defringe_green_hue_hi = hue.1;
        for edge in [640, 1800] {
            check(&format!("defringe {purple}/{green} {hue:?} @{edge}"), &src, &raw, &s, &RenderRequest::fit(edge, edge));
        }
    }
}

/// Note 4 of 2026-10-31: a slider draft on the GPU has the large-scale tone of the full render (the
/// Highlights / Shadows base was narrower in drafts), for a slider drag and a mask stroke, in a
/// 100 % window of a 6000 px frame and in a whole frame drawn 2000 px wide; and it matches the CPU's
/// full render as closely.
#[test]
fn a_gpu_draft_has_the_large_scale_tone_of_the_full_render() {
    use lcc_pipeline::PixelWindow;
    if !gpu() {
        return;
    }
    let src = scene(0, 960, 640);
    let info = SourceInfo { raw: true, ..Default::default() };
    let mut slider = DevelopSettings::default();
    typical(&mut slider);
    slider.light.shadows = 60.0;
    slider.light.highlights = -60.0;
    let mut stroke = DevelopSettings::default();
    typical(&mut stroke);
    stroke.masks.push(Mask {
        components: vec![MaskComponent {
            anchor: None,
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Radial { center: Point::new(0.3, 0.3), rx: 0.1, ry: 0.1, angle: 0.0, feather: 50.0, invert: false },
        }],
        adjust: lcc_develop::LocalAdjustments { exposure: 0.5, shadows: 50.0, ..Default::default() },
        ..Default::default()
    });
    // worst 16 px block mean difference, in 8-bit levels
    let block_diff = |a: &Rgba8, b: &Rgba8| {
        assert_eq!((a.width, a.height), (b.width, b.height));
        let mut worst = 0f64;
        for by in 0..a.height / 16 {
            for bx in 0..a.width / 16 {
                let (mut da, mut db) = (0f64, 0f64);
                for y in by * 16..by * 16 + 16 {
                    for x in bx * 16..bx * 16 + 16 {
                        let (p, q) = (a.get(x, y), b.get(x, y));
                        da += (p[0] as f64 + p[1] as f64 + p[2] as f64) / 3.0;
                        db += (q[0] as f64 + q[1] as f64 + q[2] as f64) / 3.0;
                    }
                }
                worst = worst.max((da - db).abs() / 256.0);
            }
        }
        worst
    };
    let cases = [
        RenderRequest { window: Some(PixelWindow { x: 2400, y: 1500, w: 384, h: 256 }), ..RenderRequest::fit(6000, 4000) },
        RenderRequest::fit(2000, 2000),
    ];
    for (name, s) in [("slider", &slider), ("stroke", &stroke)] {
        for req in cases {
            let full = lcc_gpu::render(&src, &info, s, &RenderRequest { quality: Quality::Full, ..req }, None).expect("gpu full").image;
            let draft = lcc_gpu::render(&src, &info, s, &RenderRequest { quality: Quality::Draft, ..req }, None).expect("gpu draft").image;
            let cpu = render(&src, &info, s, &RenderRequest { quality: Quality::Full, ..req }).image;
            let (d, c) = (block_diff(&full, &draft), block_diff(&cpu, &draft));
            eprintln!("{name} {:?}: gpu draft vs gpu full {d:.3}/255, vs cpu full {c:.3}/255", req.window);
            assert!(
                d <= 0.5 && c <= 0.5,
                "{name} {:?}: the GPU draft's large-scale tone is {d:.2}/255 off the GPU full render, {c:.2}/255 off the CPU's",
                req.window
            );
        }
    }
}

/// A drag of a photo with masks reuses their device-resident alpha planes (AI masks are
/// evaluated on the CPU): the result must equal a fresh render while the exposure moves (a range
/// mask selects on it), the masks' own sliders change, and a mask moves, is feathered or hidden.
#[test]
fn cached_mask_planes_match_uncached() {
    use lcc_develop::LocalAdjustments;
    if !gpu() {
        return;
    }
    let src = scene(2, 900, 600);
    let info = SourceInfo { raw: true, ..Default::default() };
    let cache = StageCache::default();
    let req = RenderRequest::fit(640, 640);
    let comp = |shape| MaskComponent { anchor: None, name: None, op: MaskOp::Add, invert: false, shape };
    let mut s = DevelopSettings::default();
    typical(&mut s);
    let radial = MaskShape::Radial { center: Point::new(0.5, 0.4), rx: 0.3, ry: 0.2, angle: 0.0, feather: 50.0, invert: false };
    s.masks.push(Mask {
        id: 1,
        components: vec![comp(radial)],
        adjust: LocalAdjustments { exposure: 0.5, ..Default::default() },
        ..Default::default()
    });
    let range = MaskShape::LuminanceRange { lo: 0.3, hi: 0.6, lo_feather: 0.1, hi_feather: 0.1 };
    s.masks.push(Mask {
        id: 2,
        components: vec![comp(range)],
        adjust: LocalAdjustments { saturation: -50.0, ..Default::default() },
        ..Default::default()
    });
    s.masks.push(Mask {
        id: 3,
        components: vec![comp(MaskShape::Subject)],
        adjust: LocalAdjustments { contrast: 30.0, ..Default::default() },
        ..Default::default()
    });
    let step = |s: &DevelopSettings, what: &str| {
        let warm = lcc_gpu::render(&src, &info, s, &req, Some(&cache)).expect("gpu");
        let fresh = lcc_gpu::render(&src, &info, s, &req, None).expect("gpu");
        assert_eq!(warm.image, fresh.image, "{what}");
    };
    step(&s, "first");
    for k in 0..3 {
        s.light.exposure = -0.5 + 0.6 * k as f64;
        step(&s, "exposure");
    }
    s.masks[0].adjust.exposure = -0.3;
    s.masks[2].adjust.amount = 40.0;
    step(&s, "mask sliders and amount");
    if let Some(MaskShape::Radial { center, .. }) = s.masks.first_mut().and_then(|m| m.components.first_mut()).map(|c| &mut c.shape) {
        *center = Point::new(0.3, 0.6);
    }
    step(&s, "moved radial");
    s.masks[0].feather = 40.0;
    s.masks[1].curve.master = vec![Point::new(0.0, 0.1), Point::new(1.0, 0.9)];
    step(&s, "feather and a mask's curve");
    s.masks[1].visible = false;
    step(&s, "hidden mask");
}

/// The 8-bit dither of process V8 (adapted from upstream #3509) lands on the same pixels on both, also in
/// a zoomed window off the 8×8 grid: out of phase, about half the pixels of a gradient would differ.
#[test]
fn dithered_gradients_match_in_phase() {
    if !gpu() {
        return;
    }
    let (w, h) = (512, 64);
    let src = Arc::new(Rgb32f::from_fn(w, h, |x, _| {
        let v = 0.16 + 0.03 * x as f32 / (w - 1) as f32;
        [v, v, v]
    }));
    let s = DevelopSettings { process: Process::V8.version(), ..DevelopSettings::default() };
    let window = lcc_pipeline::PixelWindow { x: 77, y: 13, w: 101, h: 37 };
    for req in [RenderRequest::fit(w, h), RenderRequest { window: Some(window), ..RenderRequest::fit(w, h) }] {
        let cpu = render(&src, &SourceInfo::default(), &s, &req).image;
        let gpu =
            lcc_gpu::render(&src, &SourceInfo::default(), &s, &req, None).unwrap_or_else(|| panic!("gpu render ({:?})", lcc_gpu::last_fallback()));
        assert_eq!((cpu.width, cpu.height), (gpu.image.width, gpu.image.height));
        let differ = cpu.data.iter().zip(&gpu.image.data).filter(|(a, b)| a[1] != b[1]).count();
        let share = differ as f64 / cpu.data.len() as f64;
        eprintln!("dithered gradient ({:?}): {:.2} % of pixels differ", req.window, share * 100.0);
        assert!(share < 0.05, "{:.1} % of the pixels differ: the dither is out of phase", share * 100.0);
    }
}

/// A flat-field map (Capture One LCC "Uniform Light", `lcc_pipeline::vignetting_calibration`): the GPU's `warp_gain` samples
/// the same field as `Warp::gain`. On a raw source whose 5:4 image area sits inside a 3:2 map's window, turned by
/// the user; with manual vignetting on top; with a lens profile's vignetting (stepping aside) and distortion; at the
/// largest amount; and in a preview window with the joint colour NR on, whose noise model samples the whole frame
/// in tiles (`Frame::sample_windows`).
#[test]
fn vignetting_calibration_matches() {
    if !gpu() {
        return;
    }
    use lcc_develop::{EmbeddedLens, EmbeddedVignette, EmbeddedVignettingMap, EmbeddedWarp};
    use lcc_raw::SensorWindow;
    use lcc_raw::gaintable::SourcePlacement;
    // a 960 × 640 sensor; the photo's 800 × 640 image area at (80, 0) on it
    let sensor = [960usize, 640];
    let grid = [48usize, 32];
    let ev: Vec<f64> = (0..grid[0] * grid[1])
        .map(|i| {
            let (dx, dy) = (((i % grid[0]) as f64 + 0.5) / grid[0] as f64 - 0.53, (((i / grid[0]) as f64 + 0.5) / grid[1] as f64 - 0.48) / 1.5);
            let d2 = (dx * dx + dy * dy) / (0.25 + 0.25 / 2.25);
            1.3 * d2 * d2 + 0.25 * d2
        })
        .collect();
    let map = EmbeddedVignettingMap::from_ev(sensor, grid, Some([960, 640, 0, 0, 960, 640]), Default::default(), &ev).unwrap();
    let win = SensorWindow { sensor, origin: [80, 0], size: [800, 640] };
    let rect = [4.0 / 800.0, 2.0 / 640.0, 792.0 / 800.0, 636.0 / 640.0];
    let placement = SourcePlacement { active_size: Some(win.size), window: Some(win), ..SourcePlacement::new(rect, Orientation::Normal) };
    let vignetting_maps = Some(lcc_pipeline::vignetting_calibration::VignettingMapStore::of([map.clone()]));
    let info = SourceInfo { raw: true, placement: Some(placement), vignetting_maps, ..Default::default() };
    let id = lcc_develop::VignettingMapId::of(&map);
    let src = scene(3, 792, 636);
    let mut s = DevelopSettings::default();
    s.optics.vignetting_calibration = Some(id.clone());
    s.orientation = Orientation::Rotate90;
    assert!(lcc_pipeline::frame_for(&src, &info, &s, true).warp.is_some_and(|w| w.field.is_some()), "the map applies");
    check("flat field 5:4 rotated", &src, &info, &s, &RenderRequest::fit(700, 700));
    // the GPU really applies it: its corner is brighter than without the map
    let req = RenderRequest::fit(700, 700);
    let with = lcc_gpu::render(&src, &info, &s, &req, None).expect("gpu render").image;
    let without = lcc_gpu::render(&src, &info, &DevelopSettings { orientation: Orientation::Rotate90, ..Default::default() }, &req, None)
        .expect("gpu render")
        .image;
    let corner = |i: &Rgba8| (0..3).map(|c| u32::from(i.data[i.width * 5 + 5][c])).sum::<u32>();
    assert!(corner(&with) > corner(&without) + 20, "{} vs {}", corner(&with), corner(&without));
    s.optics.vignetting = -40.0;
    check("flat field + manual vignetting", &src, &info, &s, &RenderRequest::fit(700, 700));
    let lens = EmbeddedLens {
        warp: Some(EmbeddedWarp { planes: [[1.0, -0.03, 0.01, 0.0, 0.0, 0.0]; 3], center: Point::new(0.5, 0.5), radius: 0.6 }),
        vignette: Some(EmbeddedVignette { k: [0.5, -0.1, 0.0, 0.0, 0.0], center: Point::new(0.5, 0.5), radius: 0.6 }),
        stages: Default::default(),
    };
    let lensed = SourceInfo { lens: Some(lens), ..info.clone() };
    s.optics.lens_profile = true;
    check("flat field + lens profile", &src, &lensed, &s, &RenderRequest::fit(700, 700));
    s.optics.vignetting_calibration_amount = 120.0;
    check("flat field at 120", &src, &lensed, &s, &RenderRequest::fit(700, 700));
    // a preview level of a large original with the joint colour NR, and a zoomed window of it
    let mut s = DevelopSettings { process: Process::V8.version(), orientation: Orientation::Rotate90, ..Default::default() };
    s.optics.vignetting_calibration = Some(id);
    s.detail.nr_color = 60.0;
    let preview = SourceInfo { native_long: Some(8000), ..info.clone() };
    check("flat field + colour NR preview", &src, &preview, &s, &RenderRequest::fit(636, 792));
    let w = lcc_pipeline::PixelWindow { x: 0, y: 500, w: 300, h: 260 };
    check("flat field + colour NR window", &src, &info, &s, &RenderRequest { window: Some(w), ..RenderRequest::fit(636, 792) });
}

/// Levels at their defaults, spelled out, change nothing: the same settings, no stage, and the GPU
/// render equals the render of settings that never had Levels.
#[test]
fn neutral_levels_render_like_no_levels() {
    if !gpu() {
        return;
    }
    let src = scene(1, 320, 240);
    let info = SourceInfo::default();
    let req = RenderRequest::fit(320, 240);
    let plain = DevelopSettings::default();
    let mut spelled = DevelopSettings::default();
    spelled.levels.master = Default::default();
    spelled.levels.red.black = 0.0;
    assert_eq!(plain, spelled);
    assert!(lcc_pipeline::levels::LevelsK::new(&spelled.levels).is_none());
    let a = lcc_gpu::render(&src, &info, &plain, &req, None).unwrap().image;
    let b = lcc_gpu::render(&src, &info, &spelled, &req, None).unwrap().image;
    assert_eq!(a.data, b.data, "GPU");
    let (ca, cb) = (render(&src, &info, &plain, &req).image, render(&src, &info, &spelled, &req).image);
    assert_eq!(ca.data, cb.data, "CPU");
}

/// The ends of the midtone range (and some values between), ordinary and inverted (output levels reversed, and on
/// every other scene the input range too), on every demo scene: the GPU matches the CPU within [`max_lsb`] everywhere
/// and no pixel is off by more than that (the scales are 8-bit levels).
#[test]
fn midtone_extremes_match_cpu_on_every_scene() {
    if !gpu() {
        return;
    }
    let (info, req) = (SourceInfo::default(), RenderRequest::fit(720, 480));
    let (mut worst_max, mut worst_share) = (0u8, 0.0f64);
    for i in 0..lcc_scenes::demo_library().len().min(6) {
        let src = scene(i, 720, 480);
        for midtone in [1.0, -1.0, 0.5, -0.5, 0.1, -0.1] {
            for inverted in [false, true] {
                let mut s = DevelopSettings::default();
                s.levels.master.midtone = midtone;
                if inverted && i % 2 == 1 {
                    s.levels.master.black = 215.0;
                    s.levels.master.white = 40.0;
                }
                if inverted {
                    s.levels.master.out_black = 255.0;
                    s.levels.master.out_white = 0.0;
                }
                let cpu = render(&src, &info, &s, &req).image;
                let gpu = lcc_gpu::render(&src, &info, &s, &req, None)
                    .unwrap_or_else(|| panic!("scene {i} midtone {midtone}: gpu render ({:?})", lcc_gpu::last_fallback()));
                let (mean, max, _) = diff(&cpu, &gpu.image);
                let far = cpu.data.iter().zip(&gpu.image.data).filter(|(p, q)| (0..3).any(|c| p[c].abs_diff(q[c]) > max_lsb())).count();
                let share = far as f64 / cpu.data.len() as f64;
                worst_max = worst_max.max(max);
                worst_share = worst_share.max(share);
                assert!(
                    mean < mean_lsb() && share == 0.0,
                    "scene {i} midtone {midtone} inverted {inverted}: mean {mean:.4}, max {max}, share {share:.5}"
                );
            }
        }
    }
    eprintln!("levels midtone extremes: worst max {worst_max} LSB (8-bit), worst share {worst_share:.5}");
}

/// 16-bit output samples of a render.
fn u16_samples(r: &lcc_pipeline::Rendered) -> &[u16] {
    let Some(lcc_pipeline::DeepImage { samples: lcc_pipeline::DeepSamples::U16(v), .. }) = &r.deep else { panic!("no 16-bit samples") };
    v
}

/// Levels in the deep outputs, for every Levels case. The GPU renders two depths only (8-bit and the float
/// HDR path; 16-bit and linear-float requests are the CPU's by design, `lcc_gpu::handles`), so:
/// - the GPU's float samples match the CPU's, in 8-bit steps of the encoding they decode to
///   (`check_hdr_req`, which also holds the 8-bit reductions together);
/// - the CPU's 16-bit samples agree with its own 8-bit render to within the 8-bit rounding and dither
///   (one level), so Levels act the same at every depth.
#[test]
fn levels_match_in_16_bit_and_float_output() {
    if !gpu() {
        return;
    }
    let src = scene(2, 480, 320);
    let info = SourceInfo::default();
    for (name, edit) in cases().into_iter().filter(|(n, _)| n.starts_with("levels")) {
        let mut s = DevelopSettings::default();
        edit(&mut s);
        let req = RenderRequest::fit(480, 320);
        let eight = render(&src, &info, &s, &req);
        let req16 = RenderRequest { depth: lcc_pipeline::OutputDepth::U16, ..req };
        let sixteen = render(&src, &info, &s, &req16);
        let deep = u16_samples(&sixteen);
        let (mut sum, mut max) = (0.0f64, 0.0f64);
        for (px, d) in eight.image.data.iter().zip(deep.as_chunks::<3>().0) {
            for c in 0..3 {
                let diff = (f64::from(px[c]) - f64::from(d[c]) / 257.0).abs();
                sum += diff;
                max = max.max(diff);
            }
        }
        let mean = sum / (deep.len() as f64);
        eprintln!("{name:<42} CPU 16-bit vs 8-bit: mean {mean:.4}  max {max:.3} (8-bit levels)");
        assert!(mean < 0.6 && max <= 1.01, "{name}: 16-bit against 8-bit: mean {mean:.4}, max {max:.3}");
        // float, linear (the HDR path): GPU against CPU
        let reqf = RenderRequest { depth: lcc_pipeline::OutputDepth::F32Hdr, ..req };
        check_hdr_req(&format!("{name} (float)"), &src, &info, &s, &reqf);
    }
}

// ---------------------------------------------------------------------------------------------
// Brightness: the stage is closed form on both sides (`lcc_pipeline::brightness`, `brightness_*` in finish.wgsl), so the
// float renders agree far closer than the 8-bit bounds above: within 0.1 of an 8-bit level on a dense sweep of greys and
// colours and on the 63 colour patches of the calibration chart, SDR and HDR (the stage adds at most 0.1 of a level on top of
// the difference the two renderers already have before it, scaled by its steepest slope).

/// The 63 colour patches of the Brightness calibration chart (sRGB-encoded): 6 hues × 2 saturations × 3 levels, and 9
/// natural tones (skin, foliage, sky, stone) at 3 levels.
fn brightness_chart_colours() -> Vec<[f32; 3]> {
    let mut v = Vec::new();
    let hues = [[1.0f32, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 1.0], [1.0, 0.0, 1.0], [1.0, 1.0, 0.0]];
    for h in hues {
        for s in [1.0f32, 0.5] {
            for level in [0.30f32, 0.60, 0.90] {
                v.push(h.map(|c| level * ((1.0 - s) + s * c)));
            }
        }
    }
    let tones = [
        [1.00f32, 0.82, 0.72],
        [0.93, 0.68, 0.50],
        [0.80, 0.55, 0.40],
        [0.55, 0.70, 0.20],
        [0.25, 0.60, 0.25],
        [0.20, 0.50, 0.40],
        [0.45, 0.65, 1.00],
        [0.70, 0.82, 1.00],
        [0.85, 0.75, 0.55],
    ];
    for t in tones {
        for k in [0.35f32, 0.65, 1.0] {
            v.push(t.map(|c| c * k));
        }
    }
    v
}

/// A dense set of sRGB-encoded colours: a grey ramp (up to `top`, encoded), a hue ring at several saturations and
/// levels, a colour cube, and the chart's patches.
fn brightness_sweep(top: f32) -> Vec<[f32; 3]> {
    let mut v = Vec::new();
    for i in 0..2048 {
        let g = top * i as f32 / 2047.0;
        v.push([g; 3]);
    }
    for h in 0..180 {
        let a = h as f32 / 180.0 * std::f32::consts::TAU;
        let rgb = [a.cos(), (a - 2.0944).cos(), (a + 2.0944).cos()].map(|c| 0.5 + 0.5 * c.clamp(-1.0, 1.0));
        for s in [0.15f32, 0.5, 1.0] {
            for level in [0.05f32, 0.3, 0.6, 0.95] {
                v.push(rgb.map(|c| (level * ((1.0 - s) + s * c)).min(top)));
            }
        }
    }
    for r in 0..17 {
        for g in 0..17 {
            for b in 0..17 {
                v.push([r, g, b].map(|c| top * c as f32 / 16.0));
            }
        }
    }
    v.extend(brightness_chart_colours());
    v
}

/// `colours` (encoded in sRGB) as a scene-linear Rec.2020 image `w` wide, padded with mid grey.
fn brightness_image(colours: &[[f32; 3]], w: usize) -> (Arc<Rgb32f>, usize) {
    let h = colours.len().div_ceil(w);
    let m = lcc_color::SRGB.to_space(&lcc_color::REC2020);
    let mut img = Rgb32f::new(w, h);
    for (i, px) in img.data.iter_mut().enumerate() {
        let e = colours.get(i).copied().unwrap_or([0.5; 3]);
        *px = m.apply_f32(e.map(lcc_color::transfer::srgb_to_linear));
    }
    (Arc::new(img), h)
}

fn brightness_codes(src: &Arc<Rgb32f>, s: &DevelopSettings, w: usize, h: usize, gpu_side: bool) -> Vec<f32> {
    let req = RenderRequest { depth: lcc_pipeline::OutputDepth::F32Hdr, ..RenderRequest::fit(w, h) };
    let info = SourceInfo::default();
    let r =
        if gpu_side { lcc_gpu::try_render(src, &info, s, &req, None).expect("the GPU renders float requests") } else { render(src, &info, s, &req) };
    hdr_codes(&r)
}

fn brightness_agreement(name: &str, stops: Option<f64>) {
    let hdr = stops.is_some();
    // an HDR render of `stops` stops of headroom has the encoded headroom H; the grey ramp runs up to just below it, so it covers
    // the whole range from SDR white to H (the pass-through above H is in the CPU stage tests)
    let top = stops.map_or(1.0, |ev| {
        let h = lcc_pipeline::brightness::BrightnessK::for_peak(1.0, 2f32.powf(ev as f32)).expect("a stage").h;
        0.98 * h
    });
    let colours = brightness_sweep(top);
    let (src, h) = brightness_image(&colours, 128);
    let mut flat = DevelopSettings::default();
    if let Some(ev) = stops {
        flat.hdr.enabled = true;
        flat.hdr.max_ev = ev;
    }
    // the two renderers' own difference without Brightness, for reference
    let base = {
        let (a, b) = (brightness_codes(&src, &flat, 128, h, false), brightness_codes(&src, &flat, 128, h, true));
        a.iter().zip(&b).map(|(p, q)| (p - q).abs()).fold(0.0f32, f32::max)
    };
    eprintln!("{name:<24} without Brightness: CPU and GPU differ by at most {base:.4} of an 8-bit level");
    for cf in [-100.0, -50.0, -20.0, -1.0, 1.0, 20.0, 50.0, 100.0] {
        let mut s = flat.clone();
        s.light.brightness = cf;
        let (a, b) = (brightness_codes(&src, &s, 128, h, false), brightness_codes(&src, &s, 128, h, true));
        assert_eq!(a.len(), b.len());
        let (mut max, mut sum) = (0.0f32, 0.0f64);
        for (p, q) in a.iter().zip(&b) {
            let d = (p - q).abs();
            max = max.max(d);
            sum += d as f64;
        }
        eprintln!("{name:<24} cf {cf:>6}: max {max:.4}  mean {:.5} (8-bit levels, {} samples)", sum / a.len() as f64, a.len());
        // the two renderers already differ by `base` before the stage (the sRGB encode near black), and the stage amplifies
        // that by at most its steepest slope; what it adds itself must stay within 0.1 of a level
        let slope = (lcc_pipeline::brightness::PHI_PER_UNIT * f64::abs(cf) / lcc_pipeline::brightness::CF_PER_C1).exp() as f32;
        assert!(
            max <= slope * base + 0.1,
            "{name}, cf {cf}: CPU and GPU differ by {max:.4} of an 8-bit level (before the stage {base:.4}, slope {slope:.2})"
        );
        // the stage did something
        let plain = brightness_codes(&src, &flat, 128, h, false);
        let moved = a.iter().zip(&plain).map(|(p, q)| (p - q).abs()).fold(0.0f32, f32::max);
        assert!(moved > 0.1, "{name}, cf {cf}: Brightness changed nothing ({moved})");
        if hdr {
            assert!(a.iter().any(|&c| c > 255.5), "{name}, cf {cf}: the sweep reaches above SDR white");
        }
    }
}

#[test]
fn brightness_matches_the_cpu_on_a_dense_sweep_and_the_chart_colours() {
    if !gpu() {
        return;
    }
    brightness_agreement("brightness SDR", None);
    // the HDR headroom from 1 stop to the largest (5 stops, peak 32, H = 4.42 encoded)
    for ev in [1.0, 2.0, 3.0, 5.0] {
        brightness_agreement(&format!("brightness HDR {ev} stops"), Some(ev));
    }
}
