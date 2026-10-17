//! The BiRefNet port against the reference implementation (fp32).
//!
//! Needs the checkpoint and a reference file made by `tools/birefnet_reference.py`:
//! `{ENV_PREFIX}_BIREFNET_DIR=<model dir> {ENV_PREFIX}_BIREFNET_REF=<ref.safetensors> cargo test -p lcc-segment --release --test birefnet_reference -- --nocapture`.
//! Skipped (passes) when either is missing.

use std::path::PathBuf;

use candle_core::{Device, Tensor};
use lcc_segment::birefnet::BiRefNet;

fn vec(t: &Tensor) -> Vec<f32> {
    t.to_device(&Device::Cpu).unwrap().flatten_all().unwrap().to_vec1::<f32>().unwrap()
}

/// Max |a − b| relative to the reference's max |b|.
fn rel_err(name: &str, a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "{name}: length");
    let scale = b.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-6);
    let err = a.iter().zip(b).fold(0f32, |m, (x, y)| m.max((x - y).abs()));
    eprintln!("{name:>8}: max abs err {err:.2e}, rel {:.2e} (scale {scale:.2e})", err / scale);
    err / scale
}

#[test]
fn birefnet_matches_reference() {
    let (Some(dir), Some(reference)) = (lcc_brand::env_os("BIREFNET_DIR"), lcc_brand::env_os("BIREFNET_REF")) else {
        eprintln!("skipped: set {} and {}", lcc_brand::env_var("BIREFNET_DIR"), lcc_brand::env_var("BIREFNET_REF"));
        return;
    };
    let r = candle_core::safetensors::load(PathBuf::from(reference), &Device::Cpu).unwrap();
    let t = std::time::Instant::now();
    let model = BiRefNet::load(&PathBuf::from(dir)).unwrap();
    eprintln!("loaded in {:?} on {:?}: {:?}", t.elapsed(), model.device(), model.swin());
    let px = &r["pixel_values"];
    let t = std::time::Instant::now();
    let bb = model.backbone(px).unwrap();
    eprintln!("backbone: {:?}", t.elapsed());
    for (i, f) in bb.iter().enumerate() {
        assert!(rel_err(&format!("bb{i}"), &vec(f), &vec(&r[&format!("bb{i}")])) < 1e-3);
    }
    let t = std::time::Instant::now();
    let logits = vec(&model.forward(px).unwrap());
    eprintln!("forward: {:?}", t.elapsed());
    let want = vec(&r["logits"]);
    assert!(rel_err("logits", &logits, &want) < 1e-2);
    let disagree = logits.iter().zip(&want).filter(|(a, b)| (**a > 0.0) != (**b > 0.0)).count() as f32 / want.len() as f32;
    eprintln!("mask disagreement: {disagree:.2e}");
    assert!(disagree < 1e-3);
}
