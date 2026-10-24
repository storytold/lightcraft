//! Opt-in #2783 checks on private Nikon NEFs. Reads originals only; writes no photos or sidecars.
//! `LC_HISTOGRAM_RAW_DIR=/path/to/RAW cargo test --release -p lightcraft-engine --test it
//! gpu_histogram_raw:: -- --ignored --nocapture`

use std::sync::Arc;

use lightcraft_develop::DevelopSettings;
use lightcraft_pipeline::{PixelWindow, RenderRequest, StageCache};
use lightcraft_raster::Histogram;

#[test]
#[ignore = "needs LC_HISTOGRAM_RAW_DIR pointing at private NEFs and a GPU"]
fn private_nefs_match_cpu_and_keep_exact_histograms() {
    let Some(dir) = std::env::var_os("LC_HISTOGRAM_RAW_DIR") else {
        lightcraft_testkit::skip!(Input, "set LC_HISTOGRAM_RAW_DIR to a directory of Nikon NEFs");
        return;
    };
    if !lightcraft_engine::gpu::available() {
        lightcraft_testkit::skip!(Gpu, "no GPU adapter");
        return;
    }
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("read RAW directory")
        .map(|e| e.expect("directory entry").path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("nef")))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no NEFs in the selected directory");
    for path in files {
        let (src, info) = lightcraft_engine::files::load_vec(std::fs::read(&path).expect("read NEF"), usize::MAX).expect("decode sensor data");
        assert!(info.raw, "must decode sensor data: {}", path.display());
        let src = Arc::new(src);
        let mut settings = DevelopSettings::default();
        settings.light.exposure = 0.3;
        settings.light.highlights = -40.0;
        settings.light.shadows = 30.0;
        settings.effects.clarity = 15.0;
        settings.detail.nr_color = 25.0;
        let full = RenderRequest::fit(src.width, src.height);
        let window = RenderRequest { window: Some(PixelWindow { x: src.width / 3 + 1, y: src.height / 3 + 3, w: 1101, h: 991 }), ..full };
        for (name, request) in [("preview", RenderRequest::fit(1920, 1280)), ("zoom window", window), ("full", full)] {
            let stages = StageCache::default();
            let rendered =
                lightcraft_engine::gpu::try_render(&src, &info, &settings, &request, Some(&stages)).expect("GPU must render without fallback");
            assert_eq!(rendered.histogram, Histogram::of_srgb8(&rendered.image), "{} {name}: histogram", path.display());
            let cpu = lightcraft_pipeline::render(&src, &info, &settings, &request);
            assert_eq!((rendered.image.width, rendered.image.height), (cpu.image.width, cpu.image.height));
            let mut max = 0u8;
            let mut sum = 0u64;
            for (a, b) in rendered.image.data.iter().zip(&cpu.image.data) {
                for c in 0..3 {
                    let d = a[c].abs_diff(b[c]);
                    max = max.max(d);
                    sum += u64::from(d);
                }
            }
            let mean = sum as f64 / (cpu.image.len() * 3) as f64;
            println!(
                "{} {name} {}x{}: max {max} LSB, mean {mean:.6}, exact histogram",
                path.file_name().unwrap_or_default().to_string_lossy(),
                cpu.image.width,
                cpu.image.height
            );
            assert!(max <= 3 && mean < 0.5, "CPU/GPU equivalence: max {max}, mean {mean}");
            let warm = lightcraft_engine::gpu::try_render(&src, &info, &settings, &request, Some(&stages)).expect("cached GPU render");
            assert_eq!(warm.histogram, rendered.histogram);
            assert_eq!(warm.image, rendered.image);
        }
    }
}
