//! Video grading on the GPU matches the CPU grade (within 8-bit rounding).

use std::sync::Arc;

use lcc_develop::DevelopSettings;
use lcc_pipeline::{Quality, grade};

fn settings() -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.light.exposure = 0.3;
    s.light.contrast = 15.0;
    s.light.highlights = -25.0;
    s.light.shadows = 20.0;
    s.color.vibrance = 10.0;
    s.wb.temp = 7000.0;
    s
}

#[test]
fn gpu_grade_matches_cpu() {
    let img = Arc::new(lcc_scenes::demo_library()[0].render(480, 270));
    let s = settings();
    let (gpu, on_gpu) = lcc_gpu::grade_frame(&img, &s, Quality::Full);
    let cpu = grade::grade_frame(&img, &s, Quality::Full);
    assert_eq!((gpu.width, gpu.height), (cpu.width, cpu.height));
    if !on_gpu {
        assert_eq!(gpu, cpu);
        return;
    }
    let mut sum = 0u64;
    for (a, b) in gpu.data.iter().zip(cpu.data.iter()) {
        for k in 0..3 {
            sum += a[k].abs_diff(b[k]) as u64;
        }
    }
    let mean = sum as f64 / (3 * gpu.data.len()) as f64;
    assert!(mean < 1.5, "mean abs diff {mean}");
}
