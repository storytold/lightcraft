//! The text detector's DETR encoder on the GPU against the candle encoder (the reference), on one photo: the encoder
//! memory, and what the difference does to the detector's outputs (per-query logits, presence, boxes and the merged
//! Describe mask) for a few prompts. Both models get the same backbone output (one GPU backbone, shared), so only the
//! encoder differs.
//!
//! Needs the checkpoint: `LIGHTCRAFT_SAM3_DIR=<model dir> cargo test --release -p lightcraft-gpu --features sam
//! --test it sam3_detr:: -- --nocapture`. The photo is a procedural one, or the input of a reference file made by
//! `tools/sam3_reference.py` (`LIGHTCRAFT_SAM3_REF=<ref.safetensors>`). `LIGHTCRAFT_SAM3_PROMPTS` lists the prompts,
//! comma-separated (default "person,sky,the red car"). Skipped without a model or a GPU.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use candle_core::{Device, Tensor};
use lightcraft_gpu::sam3::{self, Precision, Sam3Gpu};
use lightcraft_segment::{BackboneAccel, DetectorOutput, INPUT, Sam3, WeightReader};

fn values(t: &Tensor) -> Vec<f32> {
    t.to_device(&Device::Cpu).unwrap().flatten_all().unwrap().to_vec1::<f32>().unwrap()
}

/// Max |a − b|, that relative to max |b|, and mean |a − b|.
fn errors(name: &str, a: &[f32], b: &[f32]) -> (f32, f32, f64) {
    assert_eq!(a.len(), b.len(), "{name}: length");
    let scale = b.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-6);
    let max = a.iter().zip(b).fold(0f32, |m, (x, y)| m.max((x - y).abs()));
    let mean = a.iter().zip(b).map(|(x, y)| f64::from((x - y).abs())).sum::<f64>() / a.len().max(1) as f64;
    eprintln!("  {name}: max abs {max:.3e}, max rel {:.3e} (scale {scale:.2}), mean abs {mean:.3e}", max / scale);
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

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// The Describe mask of `out` as `Sam3::segment_text` merges it at 0.5 (the per-pixel maximum over the instances
/// scoring above it), and how many instances that is.
fn merged(out: &DetectorOutput) -> (Vec<f32>, usize) {
    let masks = values(&out.masks);
    let side = lightcraft_segment::MASK_SIDE * lightcraft_segment::MASK_SIDE;
    let mut merged = vec![f32::MIN; side];
    let keep: Vec<usize> = (0..out.logits.len()).filter(|&i| sigmoid(out.logits[i]) * sigmoid(out.presence) > 0.5).collect();
    for i in &keep {
        for (m, v) in merged.iter_mut().zip(&masks[i * side..(i + 1) * side]) {
            *m = m.max(*v);
        }
    }
    (merged, keep.len())
}

/// The photo as preprocessed pixels `[1, 3, 1008, 1008]`.
fn photo() -> Tensor {
    if let Some(path) = std::env::var_os("LIGHTCRAFT_SAM3_REF").map(PathBuf::from) {
        let mut r = candle_core::safetensors::load(&path, &Device::Cpu).unwrap();
        return r.remove("pixel_values").unwrap();
    }
    let img = lightcraft_scenes::demo_library()[0].render_fit(1600).to_srgb8();
    let rgb: Vec<u8> = img.data.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let px = lightcraft_segment::mask::preprocess(&rgb, img.width, img.height, INPUT).unwrap();
    Tensor::from_vec(px, (1, 3, INPUT, INPUT), &Device::Cpu).unwrap()
}

/// One GPU backbone for both models; the DETR encoder only for one of them (`asked` counts the calls).
struct Shared {
    gpu: Arc<Sam3Gpu>,
    detr: bool,
    asked: Arc<AtomicUsize>,
}

impl BackboneAccel for Shared {
    fn forward(&self, pixels: &[f32]) -> Result<Vec<f32>, String> {
        self.gpu.forward(pixels)
    }

    fn device(&self) -> String {
        self.gpu.device()
    }

    fn runs_detr_encoder(&self) -> bool {
        true
    }
    fn detr_encoder(&self, image: &[f32], pos: &[f32], text: &[f32], valid: &[bool]) -> Result<Vec<f32>, String> {
        self.asked.fetch_add(1, Ordering::Relaxed);
        if self.detr { self.gpu.detr_encoder(image, pos, text, valid) } else { Err("left to candle for the comparison".into()) }
    }
}

#[test]
fn the_gpu_detr_encoder_matches_the_candle_encoder() {
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
    let gpu = Arc::new(sam3::load(&WeightReader::open(&dir).unwrap(), Precision::F32).unwrap());
    let (asked_cpu, asked_gpu) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let with = |detr: bool, asked: &Arc<AtomicUsize>| {
        let (gpu, asked) = (gpu.clone(), asked.clone());
        move |_: &WeightReader| -> Result<Box<dyn BackboneAccel>, String> { Ok(Box::new(Shared { gpu, detr, asked })) }
    };
    let mut cpu = Sam3::load_accelerated(&dir, Device::Cpu, with(false, &asked_cpu)).unwrap();
    let mut acc = Sam3::load_accelerated(&dir, Device::Cpu, with(true, &asked_gpu)).unwrap();
    let pixels = photo();
    let mut enc_cpu = cpu.encode_pixels(&pixels).unwrap();
    let mut enc_acc = acc.encode_pixels(&pixels).unwrap();
    assert_eq!(values(enc_cpu.backbone()), values(enc_acc.backbone()), "the same backbone output for both");

    let prompts = std::env::var("LIGHTCRAFT_SAM3_PROMPTS").unwrap_or_else(|_| "person,sky,the red car".into());
    let prompts: Vec<&str> = prompts.split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
    for (n, prompt) in prompts.iter().copied().enumerate() {
        let t = Instant::now();
        let want = cpu.detect(&mut enc_cpu, prompt).unwrap();
        let cpu_time = t.elapsed();
        let t = Instant::now();
        let got = acc.detect(&mut enc_acc, prompt).unwrap();
        let gpu_time = t.elapsed();
        let first = if n == 0 { " (first prompt: with the detector neck and loading)" } else { "" };
        eprintln!("{prompt:?}: detector with the candle encoder {cpu_time:?}, with the GPU encoder {gpu_time:?}{first}");
        assert_eq!(acc.detr_fallback(), None, "the GPU encoder ran");
        assert!(cpu.detr_fallback().is_some_and(|w| w.contains("left to candle")));

        let (_, rel, mean) = errors("memory", &values(&got.memory), &values(&want.memory));
        assert!(rel < 1e-3 && mean < 1e-4, "the GPU encoder memory is off");
        let (logits, _, _) = errors("query logits", &got.logits, &want.logits);
        let presence = (got.presence - want.presence).abs();
        let boxes = got.boxes.iter().flatten().zip(want.boxes.iter().flatten()).fold(0f32, |m, (a, b)| m.max((a - b).abs()));
        let ((mask_g, kept_g), (mask_c, kept_c)) = (merged(&got), merged(&want));
        let i = iou(&mask_g, &mask_c);
        eprintln!(
            "  presence {:.4} vs {:.4} (logit diff {presence:.2e}), boxes max diff {boxes:.2e}, instances {kept_g} vs {kept_c}, mask IoU {i:.5}",
            sigmoid(got.presence),
            sigmoid(want.presence)
        );
        // every query's mask, not only the merged ones: the largest |Δ| of the logits and of the masks (sigmoid, 0..1)
        let (dl, dm) = values(&got.masks)
            .iter()
            .zip(&values(&want.masks))
            .fold((0f32, 0f32), |(l, m), (a, b)| (l.max((a - b).abs()), m.max((sigmoid(*a) - sigmoid(*b)).abs())));
        eprintln!("  masks of all {} queries: max |Δ| logits {dl:.2e}, mask {dm:.2e}", got.logits.len());
        assert!(logits < 0.05 && presence < 0.05 && boxes < 1e-2, "the detector outputs moved");
        assert_eq!(kept_g, kept_c, "the same instances");
        assert!(i > 0.99, "describe mask IoU {i}");
    }
    // an accelerator that failed on the encoder once isn't asked again
    assert_eq!(asked_cpu.load(Ordering::Relaxed), 1);
    assert_eq!(asked_gpu.load(Ordering::Relaxed), prompts.len());

    // the encoder alone (with the uploads and the read-back)
    let (image, pos, text) = (vec![0.1f32; 5184 * 256], vec![0.2f32; 5184 * 256], vec![0.3f32; 32 * 256]);
    let valid: Vec<bool> = (0..32).map(|i| i < 4).collect();
    let mut best = f64::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        gpu.detr_encoder(&image, &pos, &text, &valid).unwrap();
        best = best.min(t.elapsed().as_secs_f64());
    }
    let (w, b) = gpu.detr_bytes().unwrap();
    eprintln!("GPU DETR encoder alone: {:.1} ms on {adapter}; {:.0} MB weights, {:.0} MB work buffers", best * 1e3, w as f64 / 1e6, b as f64 / 1e6);
}
