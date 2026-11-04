//! SAM 3's two necks on the GPU against the candle necks on the CPU (the reference), on one photo: every level, and
//! what the difference does to the masks downstream (click masks and a Describe mask). Both models get the same
//! GPU backbone output, so only the necks differ.
//!
//! Needs the checkpoint: `LIGHTCRAFT_SAM3_DIR=<model dir> cargo test --release -p lightcraft-gpu --features sam
//! --test it sam3_necks:: -- --nocapture`. `LIGHTCRAFT_SAM3_PROMPT` names the Describe prompt (default "sky"). Skipped
//! without a model or a GPU.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use candle_core::{Device, Tensor};
use lightcraft_gpu::sam3::{self, Precision, Sam3Gpu};
use lightcraft_segment::{BackboneAccel, Click, INPUT, NeckKind, Sam3};

fn values(t: &Tensor) -> Vec<f32> {
    t.to_device(&Device::Cpu).unwrap().flatten_all().unwrap().to_vec1::<f32>().unwrap()
}

/// Max |a − b|, that relative to max |b|, and mean |a − b|.
fn errors(name: &str, a: &[f32], b: &[f32]) -> (f32, f32, f64) {
    assert_eq!(a.len(), b.len(), "{name}: length");
    let scale = b.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-6);
    let max = a.iter().zip(b).fold(0f32, |m, (x, y)| m.max((x - y).abs()));
    let mean = a.iter().zip(b).map(|(x, y)| f64::from((x - y).abs())).sum::<f64>() / a.len().max(1) as f64;
    eprintln!("{name}: max abs {max:.3e}, max rel {:.3e} (scale {scale:.2}), mean abs {mean:.3e}", max / scale);
    (max, max / scale, mean)
}

/// Intersection over union of two masks (logits > 0).
fn iou(a: &[f32], b: &[f32]) -> f32 {
    // masks of different sizes are no match (`zip` would compare only the shorter's pixels)
    assert_eq!(a.len(), b.len(), "mask sizes");
    let (mut both, mut either) = (0usize, 0usize);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (*x > 0.0, *y > 0.0);
        both += usize::from(x && y);
        either += usize::from(x || y);
    }
    if either == 0 { 1.0 } else { both as f32 / either as f32 }
}

/// The largest |Δ| between two masks' logits, and between the masks themselves (their sigmoid, 0..1).
fn mask_diff(a: &[f32], b: &[f32]) -> (f32, f32) {
    let sigmoid = |v: f32| 1.0 / (1.0 + (-v).exp());
    a.iter().zip(b).fold((0f32, 0f32), |(l, m), (x, y)| (l.max((x - y).abs()), m.max((sigmoid(*x) - sigmoid(*y)).abs())))
}

/// One GPU backbone for both models: with its necks (counting the levels it hands out), or without (candle's run).
struct Shared {
    gpu: Arc<Sam3Gpu>,
    necks: bool,
    runs: Arc<AtomicUsize>,
}

impl BackboneAccel for Shared {
    fn forward(&self, pixels: &[f32]) -> Result<Vec<f32>, String> {
        self.gpu.forward(pixels)
    }
    fn device(&self) -> String {
        self.gpu.device()
    }
    fn runs_necks(&self) -> bool {
        true
    }
    fn neck(&self, which: NeckKind, backbone: &[f32], n: usize) -> Result<Vec<Vec<f32>>, String> {
        if !self.necks {
            return Err("necks off".into());
        }
        let levels = self.gpu.neck(which, backbone, n)?;
        self.runs.fetch_add(1, Ordering::Relaxed);
        Ok(levels)
    }
}

#[test]
fn the_gpu_necks_match_the_cpu_necks() {
    let Some(dir) = std::env::var_os("LIGHTCRAFT_SAM3_DIR").map(PathBuf::from).filter(|d| lightcraft_segment::is_model_dir(d)) else {
        lightcraft_testkit::skip!(Model, "set LIGHTCRAFT_SAM3_DIR to the SAM 3 model directory");
        return;
    };
    let adapter = match sam3::adapter() {
        Ok(a) => a,
        // a kernel that doesn't compile is a failure, not a missing GPU
        Err(e) if e.contains("did not compile") => panic!("{e}"),
        Err(e) => {
            lightcraft_testkit::skip!(Gpu, "no GPU for the SAM 3 kernels ({e})");
            return;
        }
    };
    let img = lightcraft_scenes::demo_library()[0].render_fit(1600).to_srgb8();
    let rgb: Vec<u8> = img.data.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let px = lightcraft_segment::mask::preprocess(&rgb, img.width, img.height, INPUT).unwrap();
    let pixels = Tensor::from_vec(px, (1, 3, INPUT, INPUT), &Device::Cpu).unwrap();

    let reader = lightcraft_segment::WeightReader::open(&dir).unwrap();
    let gpu = Arc::new(sam3::load(&reader, Precision::F32).unwrap());
    let runs = Arc::new(AtomicUsize::new(0));
    let accel = |necks: bool| {
        let shared = Shared { gpu: gpu.clone(), necks, runs: runs.clone() };
        move |_: &lightcraft_segment::WeightReader| -> Result<Box<dyn BackboneAccel>, String> { Ok(Box::new(shared)) }
    };
    let mut cpu = Sam3::load_accelerated(&dir, Device::Cpu, accel(false)).unwrap();
    let mut on_gpu = Sam3::load_accelerated(&dir, Device::Cpu, accel(true)).unwrap();
    let mut want = cpu.encode_pixels(&pixels).unwrap();
    let mut got = on_gpu.encode_pixels(&pixels).unwrap();
    assert_eq!(values(want.backbone()), values(got.backbone()), "the same backbone output for both");

    for which in [NeckKind::Tracker, NeckKind::Detector] {
        let t = Instant::now();
        let reference = cpu.neck_levels(which, &want).unwrap();
        eprintln!("{which:?} neck, CPU: {:?}", t.elapsed());
        let before = runs.load(Ordering::Relaxed);
        let t = Instant::now();
        let levels = on_gpu.neck_levels(which, &got).unwrap();
        eprintln!("{which:?} neck, GPU (first, builds it): {:?} on {adapter}", t.elapsed());
        let t = Instant::now();
        on_gpu.neck_levels(which, &got).unwrap();
        eprintln!("{which:?} neck, GPU (again): {:?}", t.elapsed());
        assert_eq!(runs.load(Ordering::Relaxed), before + 2, "{which:?}: the GPU neck ran");
        for (i, (g, c)) in levels.iter().zip(&reference).enumerate() {
            assert_eq!(g.dims(), c.dims(), "{which:?} level {i}");
            let (_, rel, mean) = errors(&format!("{which:?} level {i}"), &values(g), &values(c));
            assert!(rel < 1e-4 && mean < 1e-5, "{which:?} level {i} is off");
        }
    }

    // a click mask (the tracker head) and a Describe mask (the detector head) from both
    for click in [Click { x: 0.5, y: 0.6, positive: true }, Click { x: 0.3, y: 0.3, positive: true }] {
        let (m_cpu, m_gpu) = (cpu.segment_clicks(&mut want, &[click]).unwrap(), on_gpu.segment_clicks(&mut got, &[click]).unwrap());
        let i = iou(&m_gpu.logits, &m_cpu.logits);
        let (dl, dm) = mask_diff(&m_gpu.logits, &m_cpu.logits);
        eprintln!(
            "click ({}, {}): mask IoU {i:.5}, max |Δ| logits {dl:.2e}, mask {dm:.2e}, score {:.4} vs {:.4}",
            click.x, click.y, m_gpu.score, m_cpu.score
        );
        assert!(i > 0.995, "click mask IoU {i}");
    }
    let prompt = std::env::var("LIGHTCRAFT_SAM3_PROMPT").unwrap_or_else(|_| "sky".into());
    let (d_cpu, d_gpu) = (cpu.segment_text(&mut want, &prompt, 0.5).unwrap(), on_gpu.segment_text(&mut got, &prompt, 0.5).unwrap());
    match (d_cpu, d_gpu) {
        (Some(c), Some(g)) => {
            let i = iou(&g.logits, &c.logits);
            let (dl, dm) = mask_diff(&g.logits, &c.logits);
            let cover = c.logits.iter().filter(|v| **v > 0.0).count() as f32 / c.logits.len() as f32;
            eprintln!(
                "describe {prompt:?}: mask IoU {i:.5} (covers {:.1} %), max |Δ| logits {dl:.2e}, mask {dm:.2e}, score {:.4} vs {:.4}",
                cover * 100.0,
                g.score,
                c.score
            );
            assert!(i > 0.99, "describe mask IoU {i}");
        }
        (None, None) => eprintln!("describe {prompt:?}: nothing found by either"),
        (c, g) => panic!("describe {prompt:?}: CPU found {}, GPU {}", c.is_some(), g.is_some()),
    }
    assert_eq!(runs.load(Ordering::Relaxed), 4 + 2, "the clicks and the prompt used the GPU necks");
    eprintln!("neck device memory: {:.0} MB", gpu.neck_bytes() as f64 / 1e6);
    drop((cpu, on_gpu, gpu));
    // no GPU work in flight when the process ends (issue #620)
    lightcraft_gpu::begin_shutdown();
    lightcraft_gpu::wait_idle(std::time::Duration::from_secs(5));
}
