use lightcraft_develop::{DevelopSettings, Process};
use lightcraft_pipeline::{PixelWindow, Quality, RenderRequest, SourceInfo};
use lightcraft_raster::{Rgb32f, Rgba8};
use std::sync::Arc;

pub fn check(mut render: impl FnMut(&Arc<Rgb32f>, &SourceInfo, &DevelopSettings, &RenderRequest) -> Rgba8, whole: bool, max_allowed: u8) {
    let src = Arc::new(Rgb32f::from_fn(601, 403, |x, y| {
        let l = if (x / 3 + y / 5) % 7 == 0 { 0.003 } else { 0.01 + ((x * 17 + y * 23) % 103) as f32 / 103.0 };
        [l, l * 0.82, l * 1.15]
    }));
    let mut failures = Vec::new();
    for process in [Process::V1, Process::V2] {
        let mut s = DevelopSettings { process: process.version(), ..Default::default() };
        s.effects.dehaze = 100.0;
        let info = SourceInfo::default();
        // (full quality: drafts of a window take main's window and Dehaze proxy, as a drag needs)
        let req = RenderRequest { quality: Quality::Full, ..RenderRequest::fit(1801, 1207) };
        let full = render(&src, &info, &s, &req);
        assert_eq!((full.width, full.height), (1801, 1207));
        let at = if whole { PixelWindow { x: 0, y: 0, w: full.width, h: full.height } } else { PixelWindow { x: 601, y: 0, w: 121, h: 83 } };
        let window = render(&src, &info, &s, &RenderRequest { window: Some(at), ..req });
        assert_eq!((window.width, window.height, window.data.len()), (at.w, at.h, at.w * at.h));
        let crop = full.crop(at.x, at.y, at.w, at.h);
        let mut max = 0;
        let mut sum = 0u64;
        for (a, b) in crop.data.iter().zip(&window.data) {
            for c in 0..3 {
                let d = a[c].abs_diff(b[c]);
                max = max.max(d);
                sum += u64::from(d);
            }
        }
        let mean = sum as f64 / (at.w * at.h * 3) as f64;
        eprintln!("{process:?} whole={whole} max={max} mean={mean:.6}");
        if max > max_allowed || mean >= 0.01 {
            failures.push(format!("{process:?} whole={whole} max={max} mean={mean:.6}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

pub fn check_edits(mut render: impl FnMut(&Arc<Rgb32f>, &SourceInfo, &DevelopSettings, &RenderRequest, bool) -> Rgba8) {
    use lightcraft_develop::{Spot, SpotMode, WbMode};
    use lightcraft_geom::Point;
    let mut src = Arc::new(Rgb32f::from_fn(301, 203, |x, y| {
        let l = 0.02 + ((x / 7 * 17 + y / 11 * 23) % 103) as f32 / 103.0;
        [l, l * 0.82, l * 1.15]
    }));
    let mut info = SourceInfo::default();
    let mut s = DevelopSettings::default();
    s.effects.dehaze = 100.0;
    let full_req = RenderRequest::fit(901, 607);
    for step in 0..9 {
        match step {
            1 => s.light.exposure = 0.4,
            2 => {
                s.wb.mode = WbMode::Custom;
                s.wb.temp = 8500.0;
            }
            3 => s.detail.nr_luminance = 70.0,
            4 => s.detail.nr_color = 60.0,
            5 => s.spots.push(Spot {
                mode: SpotMode::Clone,
                points: vec![Point::new(0.4, 0.4)],
                size: 0.03,
                source_offset: Some(Point::new(0.2, 0.1)),
                ..Default::default()
            }),
            6 => s.optics.defringe_purple_amount = 50.0,
            7 => {
                s.crop.geometry.angle = 3.0;
                info.raw = true;
            }
            8 => src = Arc::new(src.map(|p| [p[0] * 0.8, p[1], p[2] * 0.9])),
            _ => {}
        }
        let full = render(&src, &info, &s, &full_req, false);
        let at = PixelWindow { x: 201 + step * 3, y: 111, w: 121, h: 83 };
        let req = RenderRequest { window: Some(at), ..full_req };
        let cached = render(&src, &info, &s, &req, true);
        assert_eq!(cached, render(&src, &info, &s, &req, false), "stale window at step {step}");
        let crop = full.crop(at.x, at.y, at.w, at.h);
        assert_eq!((cached.width, cached.height, cached.data.len()), (at.w, at.h, at.w * at.h));
        let max = cached.data.iter().flatten().zip(crop.data.iter().flatten()).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        eprintln!("upstream step={step} max={max}");
        assert!(max <= 2, "upstream step={step} max={max}");
    }
}

/// A work entry prepared while Dehaze was disabled must not substitute its
/// window-local upstream pixels or an inactive mask's dark-channel statistic.
pub fn check_enabling(mut render: impl FnMut(&Arc<Rgb32f>, &SourceInfo, &DevelopSettings, &RenderRequest, bool) -> Rgba8) {
    use lightcraft_develop::{Mask, MaskComponent, MaskOp, MaskShape};
    use lightcraft_geom::Point;
    let src = Arc::new(Rgb32f::from_fn(301, 203, |x, y| {
        let l = 0.02 + ((x / 7 * 17 + y / 11 * 23) % 103) as f32 / 103.0;
        [l, l * 0.82, l * 1.15]
    }));
    let info = SourceInfo::default();
    let req = RenderRequest { window: Some(PixelWindow { x: 211, y: 111, w: 121, h: 83 }), ..RenderRequest::fit(901, 607) };
    let mut s = DevelopSettings::default();
    s.detail.nr_luminance = 70.0;
    s.effects.clarity = 100.0;
    let mut hidden = Mask {
        visible: false,
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Linear { start: Point::new(0.0, 0.0), end: Point::new(1.0, 1.0) },
        }],
        ..Default::default()
    };
    hidden.adjust.dehaze = 100.0;
    s.masks.push(hidden);
    render(&src, &info, &s, &req, true);
    for amount in [15.0, 0.0] {
        s.effects.dehaze = amount;
        let cached = render(&src, &info, &s, &req, true);
        let fresh = render(&src, &info, &s, &req, false);
        assert!(cached == fresh, "stale planes after setting Dehaze to {amount}");
    }
}
