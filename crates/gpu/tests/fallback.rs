//! Issue #78: a GPU render that cannot be trusted — over the device's buffer limits, a device
//! error, work that silently did not run — must come back as `None` (the caller renders on the
//! CPU) with the reason recorded, never as an image of zeros. One test, sequential: the simulated
//! fatal faults stop GPU use on this thread until `reset_failures` (issue #675: on this thread
//! only, so a test of the fallback can run beside tests that render on the GPU).
//! Skips (passes with a note) when no GPU adapter exists.

use std::sync::Arc;

use lightcraft_develop::{DevelopSettings, Mask, MaskComponent, MaskOp, MaskShape};
use lightcraft_geom::Point;
use lightcraft_gpu::{Fallback, Fault};
use lightcraft_pipeline::{RenderRequest, SourceInfo, render};
use lightcraft_raster::Rgba8;

fn mean_abs_diff(a: &Rgba8, b: &Rgba8) -> f64 {
    let sum: u64 = a.data.iter().zip(&b.data).map(|(p, q)| (0..3).map(|c| p[c].abs_diff(q[c]) as u64).sum::<u64>()).sum();
    sum as f64 / (a.data.len() * 3) as f64
}

fn masked(s: &mut DevelopSettings, k: usize) {
    for i in 0..k {
        let c = Point::new(0.2 + 0.2 * i as f64, 0.5);
        s.masks.push(Mask {
            id: i as u32 + 1,
            components: vec![MaskComponent {
                name: None,
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::Radial { center: c, rx: 0.2, ry: 0.15, angle: 0.0, feather: 50.0, invert: false },
            }],
            ..Default::default()
        });
        s.masks[i].adjust.exposure = 0.5;
    }
}

#[test]
fn failed_gpu_renders_fall_back_with_a_reason() {
    if !lightcraft_gpu::available() {
        eprintln!("skipped: no GPU adapter ({:?})", lightcraft_gpu::unavailable_reason());
        return;
    }
    assert_eq!(lightcraft_gpu::unavailable_reason(), None);
    let (w, h) = (1200, 800);
    let src = Arc::new(lightcraft_scenes::demo_library()[0].render(w, h));
    let info = SourceInfo { raw: true, ..Default::default() };
    let mut s = DevelopSettings::default();
    s.light.exposure = 0.3;
    s.effects.clarity = 20.0;
    masked(&mut s, 4);
    let req = RenderRequest::fit(w, h);
    let cpu = render(&src, &info, &s, &req).image;
    let ok = |what: &str| {
        let g = lightcraft_gpu::render(&src, &info, &s, &req, None)
            .unwrap_or_else(|| panic!("{what}: gpu render ({:?})", lightcraft_gpu::last_fallback()));
        let d = mean_abs_diff(&cpu, &g.image);
        assert!(d < 0.5, "{what}: GPU differs from the CPU by {d:.3} LSB");
    };
    ok("healthy");
    // the render that fell back says why itself; the process-wide diagnostic agrees
    let failed = |what: &str| -> String {
        match lightcraft_gpu::try_render(&src, &info, &s, &req, None) {
            Ok(_) => panic!("{what}: the GPU render must fall back"),
            Err(Fallback::NotTried) => panic!("{what}: the render must be tried on the GPU ({:?})", lightcraft_gpu::unavailable_reason()),
            Err(Fallback::Failed(why)) => {
                assert_eq!(lightcraft_gpu::last_fallback().as_deref(), Some(why.as_str()), "{what}");
                why
            }
        }
    };

    // the output itself is over the limit: refused up front, the GPU stays in use
    lightcraft_gpu::inject_fault(Fault::Limit(1 << 20));
    let why = failed("output over the limit");
    assert!(why.contains("limit"), "{why}");
    assert!(lightcraft_gpu::available(), "a limit is no device failure");

    // the output fits but the mask planes (4 masks: 4 planes in one buffer) don't: caught when the
    // buffer is created, without a device error, and the GPU stays in use
    lightcraft_gpu::inject_fault(Fault::Limit((w * h * 3 * 4 + 1024) as u64));
    let why = failed("mask planes over the limit");
    assert!(why.contains("exceeds"), "{why}");
    assert!(lightcraft_gpu::available(), "{:?}", lightcraft_gpu::unavailable_reason());
    ok("after the limit faults");

    // a device (validation) error: captured by the render's error scope; the GPU is not used again
    // on this thread — other threads keep it (a simulated failure, not a real one)
    lightcraft_gpu::inject_fault(Fault::Validation);
    let why = failed("validation error");
    assert!(why.contains("device error"), "{why}");
    assert!(!lightcraft_gpu::available());
    assert!(lightcraft_gpu::unavailable_reason().is_some_and(|r| r.contains("device error")), "{:?}", lightcraft_gpu::unavailable_reason());
    assert_eq!(lightcraft_gpu::render(&src, &info, &s, &req, None).map(|_| ()), None, "no GPU render on this thread after a fatal fault");
    let elsewhere = std::thread::spawn(|| (lightcraft_gpu::available(), lightcraft_gpu::unavailable_reason())).join();
    assert_eq!(elsewhere.ok(), Some((true, None)), "a simulated failure switches the GPU off for its own thread only");
    lightcraft_gpu::reset_failures();
    assert_eq!(lightcraft_gpu::unavailable_reason(), None);
    ok("after a device error");

    // the per-pixel work never runs (a driver dropping a submission without an error): before
    // the fix this came back as a fully black image
    lightcraft_gpu::inject_fault(Fault::DropWork);
    let why = failed("dropped work");
    assert!(why.contains("incomplete"), "an unwritten image must not be returned: {why}");
    assert!(!lightcraft_gpu::available());
    lightcraft_gpu::reset_failures();
    ok("after dropped work");

    // a black photo is still rendered (on the CPU, to be sure)
    let black = Arc::new(lightcraft_raster::Rgb32f::from_fn(256, 256, |_, _| [0.0; 3]));
    let s0 = DevelopSettings::default();
    match lightcraft_gpu::try_render(&black, &info, &s0, &RenderRequest::fit(256, 256), None) {
        Err(Fallback::Failed(why)) => assert!(why.contains("black"), "{why}"),
        other => panic!("a black result is redone on the CPU: {:?}", other.map(|_| ())),
    }
    assert!(lightcraft_gpu::last_fallback().unwrap_or_default().contains("black"));
    assert!(lightcraft_gpu::available());

    // a request the GPU never renders is not a fallback
    let deep = RenderRequest { depth: lightcraft_pipeline::OutputDepth::U16, ..RenderRequest::fit(256, 256) };
    assert_eq!(lightcraft_gpu::try_render(&src, &info, &s, &deep, None).err(), Some(Fallback::NotTried));
}
