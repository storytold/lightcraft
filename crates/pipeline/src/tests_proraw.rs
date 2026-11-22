//! Apple ProRAW in Lightroom's tone chain (process V13, [`crate::proraw`]): earlier processes render
//! such photos exactly as before, other sources render on V13 exactly as on V10, and a zoomed window
//! reads the chain's Highlights / Shadows neighbourhood from the whole frame.

use std::sync::Arc;

use lightcraft_develop::{DevelopSettings, Process, ProcessVersion, WbMode, controls};
use lightcraft_raster::Rgb32f;

use crate::proraw::{PRORAW_TONE_KNOTS, ProRawTone};
use crate::tone::CameraTone;
use crate::{DeepSamples, OutputDepth, PixelWindow, RenderRequest, SourceInfo, render};

/// A ProRAW-like curve: the same curve at 32 knots on luminance (the camera-curve path every
/// process before V13 takes) and at 128 knots for Lightroom's chain.
fn proraw_info() -> SourceInfo {
    let f = |x: f32| (x.powf(0.6) * 0.98).min(0.9995);
    let at = |i: usize, n: usize| 2f32.powf(-12.0 + 12.0 * i as f32 / (n - 1) as f32);
    let camera = CameraTone::new(std::array::from_fn(|i| [at(i, 32), f(at(i, 32))])).unwrap();
    let knots = (0..PRORAW_TONE_KNOTS).map(|i| [at(i, PRORAW_TONE_KNOTS), f(at(i, PRORAW_TONE_KNOTS))]).collect();
    let lr = ProRawTone::new(knots, -0.3, Some(-4.0)).unwrap();
    SourceInfo { raw: true, as_shot_temp: 5200.0, as_shot_tint: 4.0, camera_tone: Some(camera), proraw: Some(Arc::new(lr)), ..Default::default() }
}

fn scene() -> Rgb32f {
    lightcraft_scenes::demo_library()[0].render(240, 160)
}

fn edited(process: Process) -> DevelopSettings {
    let mut s = DevelopSettings::for_raw(5200.0, 4.0);
    for (id, v) in [
        ("light.exposure", 0.4),
        ("light.contrast", 30.0),
        ("light.highlights", -60.0),
        ("light.shadows", 40.0),
        ("light.whites", 15.0),
        ("light.blacks", -20.0),
        ("effects.clarity", 20.0),
    ] {
        controls::set(&mut s, id, v);
    }
    DevelopSettings { process: process.version(), ..s }
}

fn deep(info: &SourceInfo, s: &DevelopSettings) -> Vec<u16> {
    let req = RenderRequest { depth: OutputDepth::U16, ..RenderRequest::fit(240, 240) };
    let deep = render(&scene(), info, s, &req).deep.unwrap();
    let DeepSamples::U16(v) = deep.samples else { panic!("16-bit samples") };
    v
}

/// Processes before V13 render Apple ProRAW exactly as they did before Lightroom's chain existed
/// (the file's 32-knot curve on luminance, the sliders after it), and V13 renders it differently.
#[test]
fn proraw_renders_as_before_until_v13() {
    let info = proraw_info();
    let before = SourceInfo { proraw: None, ..info.clone() };
    for p in Process::ALL.into_iter().filter(|p| *p < Process::V13) {
        for s in [DevelopSettings { process: p.version(), ..DevelopSettings::for_raw(5200.0, 4.0) }, edited(p)] {
            assert!(deep(&info, &s) == deep(&before, &s), "{p:?} renders ProRAW as before");
        }
    }
    for s in [DevelopSettings { process: Process::V13.version(), ..DevelopSettings::for_raw(5200.0, 4.0) }, edited(Process::V13)] {
        assert!(deep(&info, &s) != deep(&before, &s), "V13 renders ProRAW in Lightroom's chain");
    }
}

/// With no ProRAW curve, the candidate keeps all V10 behavior, including its checked WB.
#[test]
fn v13_renders_everything_else_as_v10() {
    let camera = SourceInfo { proraw: None, ..proraw_info() };
    let raw = SourceInfo { camera_tone: None, ..camera.clone() };
    for info in [SourceInfo::default(), raw, camera] {
        assert!(deep(&info, &edited(Process::V10)) == deep(&info, &edited(Process::V13)), "{info:?}");
    }
}

#[test]
fn provisional_proraw_process_keeps_the_v10_white_balance_and_v9_fallback() {
    let info = SourceInfo { as_shot_temp: 5500.0, as_shot_tint: 0.0, proraw: None, ..proraw_info() };
    let mut warm = edited(Process::V10);
    (warm.wb.mode, warm.wb.temp, warm.wb.tint) = (WbMode::Custom, 2000.0, 30.0);
    let v10 = deep(&info, &warm);
    warm.process = Process::V13.version();
    assert_eq!(deep(&info, &warm), v10);
    warm.process = ProcessVersion(8);
    let v8 = deep(&info, &warm);
    assert_ne!(v8, v10, "the warm-green fixture must distinguish the old and checked white balance");
    warm.process = ProcessVersion(9);
    assert_eq!(deep(&info, &warm), v8);
}

/// An HDR render of Apple ProRAW keeps Lightroom's chain through the midtones, so they match the
/// SDR render, and lifts the highlights past SDR white as it does a camera curve.
#[test]
fn proraw_hdr_midtones_match_the_sdr_render() {
    let info = proraw_info();
    let src =
        lightcraft_scenes::demo_library().iter().find(|s| s.kind == lightcraft_scenes::Kind::OceanSunset).map_or_else(scene, |s| s.render(240, 160));
    let render_at = |s: &DevelopSettings, depth| {
        let req = RenderRequest { depth, ..RenderRequest::fit(240, 240) };
        match render(&src, &info, s, &req).deep.map(|d| d.samples) {
            Some(DeepSamples::F32(v)) => v,
            other => panic!("expected float samples, got {other:?}"),
        }
    };
    let mut on = DevelopSettings { process: Process::V13.version(), ..DevelopSettings::default() };
    (on.hdr.enabled, on.hdr.max_ev) = (true, 3.0);
    let hdr = render_at(&on, OutputDepth::F32Hdr);
    let sdr = render_at(&DevelopSettings { process: Process::V13.version(), ..DevelopSettings::default() }, OutputDepth::F32Linear);
    let mut n = 0;
    for (h, s) in hdr.as_chunks::<3>().0.iter().zip(sdr.as_chunks::<3>().0) {
        if s.iter().all(|v| *v < 0.4) {
            for (a, b) in h.iter().zip(s) {
                assert!((a - b).abs() < 2e-3, "{h:?} vs {s:?}");
            }
            n += 1;
        }
    }
    assert!(n > 100, "the scene should have midtones and shadows ({n})");
    assert!(hdr.iter().any(|v| *v > 1.0), "highlights go past SDR white");
}

/// Lightroom's Highlights / Shadows on Apple ProRAW read a Gaussian neighbourhood of 6.4 % of the
/// long edge: a zoomed window holds a small part of it, so it comes from the whole frame at
/// reduced size. Here the window sits in a dark area next to a bright one it doesn't show.
#[test]
fn proraw_highlights_and_shadows_at_depth_match_the_whole_render() {
    window_matches_the_whole_render(&proraw_info(), DevelopSettings::default());
}

/// With the camera's local tone mapping on, the neighbourhood a window reads has its gain too:
/// here a uniform 8× gain, which lifts the whole frame's neighbourhood by three stops.
#[test]
fn proraw_window_neighbourhood_has_the_camera_local_tone() {
    use lightcraft_raw::gaintable::{GainTableMap, SourcePlacement};
    let map = GainTableMap {
        points_v: 1,
        points_h: 1,
        points_n: 2,
        spacing_v: 1.0,
        spacing_h: 1.0,
        origin_v: 0.0,
        origin_h: 0.0,
        weights: [0.25, 0.5, 0.25, 0.0, 0.0],
        gamma: 1.0,
        gains: vec![8.0, 8.0],
    };
    let placement = SourcePlacement { rect: [0.0, 0.0, 1.0, 1.0], orientation: lightcraft_geom::Orientation::Normal };
    let info = SourceInfo { local_tone: Some(Arc::new(crate::local_tone::LocalTone { map, placement })), ..proraw_info() };
    let mut s = DevelopSettings::default();
    s.profile.camera_local_tone = true;
    window_matches_the_whole_render(&info, s);
}

fn window_matches_the_whole_render(info: &SourceInfo, mut s: DevelopSettings) {
    let info = info.clone();
    s.process = Process::V13.version();
    assert!(crate::proraw::active(&s, &info).is_some());
    let src = Rgb32f::from_fn(600, 400, |x, y| {
        let l = if x < 300 { 0.03 } else { 0.8 };
        let t = 1.0 + 0.3 * (((x * 7 + y * 13) % 11) as f32 / 10.0 - 0.5);
        [l * t * 1.05, l * t, l * t * 0.9]
    });
    controls::set(&mut s, "light.shadows", 80.0);
    controls::set(&mut s, "light.highlights", -80.0);
    let req = RenderRequest::fit(7680, 5120);
    let f = render(&src, &info, &s, &req).image;
    // (the bright half starts at x 3840)
    let win = PixelWindow { x: 3200, y: 2400, w: 512, h: 384 };
    let w = render(&src, &info, &s, &RenderRequest { window: Some(win), ..req }).image;
    let (mut max, mut sum, mut n) = (0u8, 0f64, 0f64);
    for y in 64..win.h - 64 {
        for x in 64..win.w - 64 {
            let (a, b) = (f.get(win.x + x, win.y + y), w.get(x, y));
            for k in 0..3 {
                let d = a[k].abs_diff(b[k]);
                max = max.max(d);
                sum += d as f64;
                n += 1.0;
            }
        }
    }
    let mean = sum / n;
    assert!(max <= 3 && mean <= 0.6, "max {max}, mean {mean:.3}");
}
