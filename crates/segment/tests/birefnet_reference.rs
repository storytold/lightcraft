//! The BiRefNet port against the reference implementation (the official PyTorch code, fp32).
//!
//! Needs a checkpoint directory and a reference file made by the scratch script `ref.py`
//! (input pixels, backbone outputs, squeeze and decoder-block outputs, an ASPP probe, logits):
//! `LIGHTCRAFT_BIREFNET_DIR=<model dir> LIGHTCRAFT_BIREFNET_REF=<ref.safetensors> cargo test -p lightcraft-segment --release --test birefnet_reference -- --nocapture`.
//! Skipped (passes) when either is missing.

use std::collections::HashMap;
use std::path::PathBuf;

use candle_core::{Device, Tensor};
use lightcraft_segment::{BIREFNET_SIDE, BiRefNet};

fn setup() -> Option<(PathBuf, HashMap<String, Tensor>)> {
    let dir = PathBuf::from(std::env::var_os("LIGHTCRAFT_BIREFNET_DIR")?);
    let reference = PathBuf::from(std::env::var_os("LIGHTCRAFT_BIREFNET_REF")?);
    let r = candle_core::safetensors::load(&reference, &Device::Cpu).ok()?;
    Some((dir, r))
}

fn vec(t: &Tensor) -> Vec<f32> {
    t.flatten_all().unwrap().to_vec1::<f32>().unwrap()
}

/// Max |a − b| relative to the reference's max |b|.
fn rel_err(name: &str, a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "{name}: length");
    let scale = b.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-6);
    let err = a.iter().zip(b).fold(0f32, |m, (x, y)| m.max((x - y).abs()));
    let mean = a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.len() as f32;
    eprintln!("{name:>10}: max abs err {err:.2e}, rel {:.2e}, mean abs err {mean:.2e} (scale {scale:.2e})", err / scale);
    err / scale
}

/// Fraction of pixels where the two logit maps disagree on the mask (> 0).
fn mask_disagreement(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).filter(|(x, y)| (**x > 0.0) != (**y > 0.0)).count() as f32 / a.len() as f32
}

#[test]
fn matches_reference() {
    let Some((dir, r)) = setup() else {
        lightcraft_testkit::skip!(Model, "set LIGHTCRAFT_BIREFNET_DIR and LIGHTCRAFT_BIREFNET_REF");
        return;
    };
    let t = std::time::Instant::now();
    let model = BiRefNet::load_on(&dir, Device::Cpu).unwrap();
    eprintln!("{}: loaded in {:?}", model.backbone_name(), t.elapsed());

    // one ASPP (deformable convolutions) on the reference's own input
    let x = &r["aspp_in"];
    let (_, c, h, w) = x.dims4().unwrap();
    let y = model.probe_aspp(2, &vec(x), c, h, w).unwrap();
    assert!(rel_err("aspp", &y, &vec(&r["aspp_out"])) < 1e-3);

    let pixels = vec(&r["pixel_values"]);
    assert_eq!(pixels.len(), 3 * BIREFNET_SIDE * BIREFNET_SIDE);
    let t = std::time::Instant::now();
    let (logits, trace) = model.trace_pixels(&pixels).unwrap();
    eprintln!("forward (with trace): {:?}", t.elapsed());
    for (name, got, shape) in &trace {
        let want = &r[name.as_str()];
        assert_eq!(want.dims()[1..], shape[..], "{name}: shape");
        // the decoder amplifies the backbone's float rounding; features stay close
        assert!(rel_err(name, got, &vec(want)) < 1e-3, "{name}");
    }
    let want = vec(&r["logits"]);
    let e = rel_err("logits", &logits, &want);
    let dis = mask_disagreement(&logits, &want);
    eprintln!("mask disagreement: {dis:.2e}");
    assert!(e < 1e-3);
    assert!(dis < 1e-3);

    let t = std::time::Instant::now();
    let again = model.segment_pixels(&pixels).unwrap();
    eprintln!("forward: {:?}", t.elapsed());
    assert_eq!(again, logits);
}
