use lightcraft_develop::{DevelopSettings, Mask, MaskComponent, MaskOp, MaskShape, Process};
use lightcraft_geom::Point;
use lightcraft_pipeline::{PixelWindow, RenderRequest, SourceInfo};
use lightcraft_raster::{Rgb32f, Rgba8};
use std::sync::Arc;

/// The round 3 repro plus native repeating detail. Shared by CPU and Metal tests.
pub fn check_windows(mut render: impl FnMut(&Arc<Rgb32f>, &SourceInfo, &DevelopSettings, &RenderRequest) -> Rgba8, stripes: bool) {
    let src = Arc::new(if stripes {
        Rgb32f::from_fn(2397, 1598, |x, _| {
            let l = if x % 17 < 3 { 0.003 } else { 0.6 };
            [l, l * 0.82, l * 1.15]
        })
    } else {
        Rgb32f::from_fn(600, 400, |x, y| {
            let l = 0.003 + (((x * 17 + y * 23) % 103) as f32 / 102.0).powi(2);
            [l * 1.1, l, l * 0.8]
        })
    });
    let info = SourceInfo::default();
    let req = RenderRequest::fit(2397, 1598);
    let mask = || Mask {
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Linear { start: Point::new(0.1, 0.2), end: Point::new(0.9, 0.8) },
        }],
        ..Default::default()
    };
    let mut failures = Vec::new();
    for process in [Process::V1, Process::V2] {
        for case in ["hidden", "empty", "local", "tone", "clarity", "nr_refine"] {
            let mut s = DevelopSettings { process: process.version(), ..Default::default() };
            if matches!(case, "hidden" | "empty" | "local") {
                let mut m = mask();
                m.visible = case == "empty";
                if case == "empty" {
                    m.components.clear();
                }
                m.adjust.highlights = -80.0;
                m.adjust.shadows = 80.0;
                s.masks.push(m);
            }
            if case == "local" {
                let mut m = mask();
                m.id = 2;
                m.adjust.clarity = 100.0;
                s.masks.push(m);
            } else if case != "tone" {
                s.effects.clarity = 100.0;
            }
            if matches!(case, "tone" | "nr_refine") {
                s.light.highlights = -80.0;
                s.light.shadows = 80.0;
            }
            if case == "nr_refine" {
                s.detail.nr_luminance = 80.0;
                s.detail.nr_color = 70.0;
                let mut m = mask();
                m.refine = 25.0;
                m.adjust.exposure = 1.0;
                s.masks.push(m);
            }
            let full = render(&src, &info, &s, &req);
            assert_eq!((full.width, full.height), (2397, 1598));
            for (x, y) in [(0, 532), (799, 532), (0, 0), (2276, 0), (0, 1515), (2276, 1515), (797, 531)] {
                let at = PixelWindow { x, y, w: 121, h: 83 };
                let window = render(&src, &info, &s, &RenderRequest { window: Some(at), ..req });
                let crop = full.crop(x, y, at.w, at.h);
                assert_eq!((window.width, window.height, window.data.len()), (at.w, at.h, at.w * at.h));
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
                let label = format!("stripes={stripes} {process:?} {case} ({x},{y}): max={max} mean={mean:.6}");
                eprintln!("{label}");
                if max > 1 || mean >= 0.01 {
                    failures.push(label);
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
