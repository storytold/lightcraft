//! Thumbnail throughput, CPU vs GPU: `cargo test --release -p lightcraft-gpu --test thumbs_bench -- --ignored --nocapture`.
//! Six threads (the app's render workers) each render thumbnails of 512 px sources at 256 px with
//! settings like a typical preset (noise reduction, sharpening, clarity, grading, a gradient mask).

use std::sync::Arc;
use std::time::Instant;

use lightcraft_develop::{DevelopSettings, Mask, MaskComponent, MaskOp, MaskShape};
use lightcraft_geom::Point;
use lightcraft_pipeline::{RenderRequest, SourceInfo};

fn preset_like() -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.light.exposure = 0.4;
    s.light.contrast = 20.0;
    s.light.highlights = -60.0;
    s.light.shadows = 50.0;
    s.effects.clarity = 15.0;
    s.detail.nr_luminance = 16.0;
    s.detail.sharpen_amount = 60.0;
    s.color.vibrance = 15.0;
    s.grading.shadows.hue = 220.0;
    s.grading.shadows.sat = 30.0;
    s.masks = vec![Mask {
        components: vec![MaskComponent {
            name: None,
            op: MaskOp::Add,
            invert: false,
            shape: MaskShape::Linear { start: Point::new(0.5, 0.0), end: Point::new(0.5, 0.5) },
        }],
        adjust: lightcraft_develop::LocalAdjustments { exposure: -0.6, ..Default::default() },
        ..Default::default()
    }];
    s
}

#[test]
#[ignore]
fn thumbnail_throughput() {
    if !lightcraft_gpu::available() {
        eprintln!("skipped: no GPU ({:?})", lightcraft_gpu::unavailable_reason());
        return;
    }
    let lib = lightcraft_scenes::demo_library();
    let sources: Vec<Arc<lightcraft_raster::Rgb32f>> = (0..12).map(|i| Arc::new(lib[i % lib.len()].render(512, 341))).collect();
    let info = SourceInfo { raw: true, ..Default::default() };
    let s = preset_like();
    let req = RenderRequest::fit(256, 256);
    let per_thread = 40;
    for gpu in [false, true, false, true] {
        let t = Instant::now();
        std::thread::scope(|sc| {
            for k in 0..6 {
                let (sources, info, s) = (&sources, &info, &s);
                sc.spawn(move || {
                    for i in 0..per_thread {
                        let src = &sources[(k * per_thread + i) % sources.len()];
                        let r = if gpu {
                            lightcraft_gpu::render(src, info, s, &req, None)
                        } else {
                            Some(lightcraft_pipeline::render(src, info, s, &req))
                        };
                        assert!(r.is_some());
                    }
                });
            }
        });
        let n = 6 * per_thread;
        let ms = t.elapsed().as_secs_f64() * 1e3;
        eprintln!("{}: {n} thumbnails in {ms:.0} ms ({:.2} ms each)", if gpu { "gpu" } else { "cpu" }, ms / n as f64);
    }
}
