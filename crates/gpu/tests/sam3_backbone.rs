//! The SAM 3 backbone on the GPU against the candle backbone on the CPU (the reference), on one photo, and what the
//! difference does to the masks downstream (a click mask and a Describe mask).
//!
//! Needs the checkpoint: `LIGHTCRAFT_SAM3_DIR=<model dir> cargo test --release -p lightcraft-gpu --features sam
//! --test sam3_backbone -- --nocapture`. The photo is a procedural one, or the input of a reference file made by
//! `tools/sam3_reference.py` (`LIGHTCRAFT_SAM3_REF=<ref.safetensors>`; then the backbone is also checked against
//! Hugging Face's). `LIGHTCRAFT_SAM3_PROMPT` names the Describe prompt (default "sky"); `LIGHTCRAFT_SAM3_GPU_F16=1`
//! stores the GPU weights as f16 (bounds as for f32 do not apply then: the errors are printed). Skipped without a
//! model or a GPU.

use std::path::PathBuf;

use candle_core::{Device, Tensor};
use lightcraft_gpu::sam3::{self, Precision};
use lightcraft_segment::{BackboneAccel, Click, INPUT, Sam3, WeightReader};

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
    let (mut both, mut either) = (0usize, 0usize);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (*x > 0.0, *y > 0.0);
        both += usize::from(x && y);
        either += usize::from(x || y);
    }
    if either == 0 { 1.0 } else { both as f32 / either as f32 }
}

/// The photo as preprocessed pixels `[1, 3, 1008, 1008]`, and the reference backbone (72 × 72 tokens of 1024) if there
/// is a reference file.
fn photo() -> (Tensor, Option<Tensor>) {
    if let Some(path) = std::env::var_os("LIGHTCRAFT_SAM3_REF").map(PathBuf::from) {
        let mut r = candle_core::safetensors::load(&path, &Device::Cpu).unwrap();
        return (r.remove("pixel_values").unwrap(), r.remove("backbone"));
    }
    let img = lightcraft_scenes::demo_library()[0].render_fit(1600).to_srgb8();
    let rgb: Vec<u8> = img.data.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let px = lightcraft_segment::mask::preprocess(&rgb, img.width, img.height, INPUT).unwrap();
    (Tensor::from_vec(px, (1, 3, INPUT, INPUT), &Device::Cpu).unwrap(), None)
}

#[test]
fn the_gpu_backbone_matches_the_cpu_backbone() {
    let Some(dir) = std::env::var_os("LIGHTCRAFT_SAM3_DIR").map(PathBuf::from).filter(|d| lightcraft_segment::is_model_dir(d)) else {
        lightcraft_testkit::skip!(Model, "set LIGHTCRAFT_SAM3_DIR to the SAM 3 model directory");
        return;
    };
    let adapter = match sam3::adapter() {
        Ok(a) => a,
        Err(e) => {
            lightcraft_testkit::skip!(Gpu, "no GPU for the SAM 3 kernels ({e})");
            return;
        }
    };
    let (pixels, reference) = photo();
    let mut cpu = Sam3::load_on(&dir, Device::Cpu).unwrap();
    let f16 = std::env::var("LIGHTCRAFT_SAM3_GPU_F16").is_ok_and(|v| v == "1");
    let mut gpu = if f16 {
        let make = |w: &WeightReader| -> Result<Box<dyn BackboneAccel>, String> { Ok(Box::new(sam3::load(w, Precision::F16)?)) };
        Sam3::load_accelerated(&dir, Device::Cpu, make).unwrap()
    } else {
        Sam3::load_accelerated(&dir, Device::Cpu, sam3::backbone).unwrap()
    };
    assert!(gpu.backbone_device().starts_with(&adapter[..adapter.len() - 1]), "the accelerator is in use");

    let t = std::time::Instant::now();
    let mut want = cpu.encode_pixels(&pixels).unwrap();
    eprintln!("CPU backbone: {:?}", t.elapsed());
    let t = std::time::Instant::now();
    let mut got = gpu.encode_pixels(&pixels).unwrap();
    eprintln!("GPU backbone (first run): {:?} on {adapter}", t.elapsed());
    let (a, b) = (values(got.backbone()), values(want.backbone()));
    let (_, rel, mean) = errors("GPU vs CPU backbone", &a, &b);
    assert!(f16 || (rel < 2e-3 && mean < 1e-4), "the GPU backbone is off");
    if let Some(r) = reference {
        let hf = values(&r.reshape((1, 72, 72, 1024)).unwrap().permute((0, 3, 1, 2)).unwrap());
        let (_, cpu_rel, _) = errors("CPU vs Hugging Face", &b, &hf);
        let (_, gpu_rel, _) = errors("GPU vs Hugging Face", &a, &hf);
        assert!(f16 || (gpu_rel < 2e-3 && cpu_rel < 2e-3));
    }

    // a click mask (the tracker head) and a Describe mask (the detector head) from both
    for click in [Click { x: 0.5, y: 0.6, positive: true }, Click { x: 0.3, y: 0.3, positive: true }] {
        let (m_cpu, m_gpu) = (cpu.segment_clicks(&mut want, &[click]).unwrap(), gpu.segment_clicks(&mut got, &[click]).unwrap());
        let i = iou(&m_gpu.logits, &m_cpu.logits);
        eprintln!("click ({}, {}): mask IoU {i:.5}, score {:.4} vs {:.4}", click.x, click.y, m_gpu.score, m_cpu.score);
        assert!(f16 || i > 0.99, "click mask IoU {i}");
    }
    let prompt = std::env::var("LIGHTCRAFT_SAM3_PROMPT").unwrap_or_else(|_| "sky".into());
    let (d_cpu, d_gpu) = (cpu.segment_text(&mut want, &prompt, 0.5).unwrap(), gpu.segment_text(&mut got, &prompt, 0.5).unwrap());
    match (d_cpu, d_gpu) {
        (Some(c), Some(g)) => {
            let i = iou(&g.logits, &c.logits);
            let cover = c.logits.iter().filter(|v| **v > 0.0).count() as f32 / c.logits.len() as f32;
            eprintln!("describe {prompt:?}: mask IoU {i:.5} (covers {:.1} %), score {:.4} vs {:.4}", cover * 100.0, g.score, c.score);
            assert!(f16 || i > 0.98, "describe mask IoU {i}");
        }
        (None, None) => eprintln!("describe {prompt:?}: nothing found by either"),
        (c, g) => panic!("describe {prompt:?}: CPU found {}, GPU {}", c.is_some(), g.is_some()),
    }
}
