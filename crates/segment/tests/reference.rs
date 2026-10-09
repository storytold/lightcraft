//! The port against the reference implementation (Hugging Face `transformers`, fp32).
//!
//! Needs the checkpoint and a reference file made by `tools/sam3_reference.py`:
//! `{ENV_PREFIX}_SAM3_DIR=<model dir> {ENV_PREFIX}_SAM3_REF=<ref.safetensors> cargo test -p dac-segment --release -- --nocapture`.
//! Skipped (passes) when either is missing.

use std::collections::HashMap;
use std::path::PathBuf;

use candle_core::{Device, IndexOp, Tensor};
use dac_segment::{Click, Sam3, tokenizer::Tokenizer};

fn setup() -> Option<(PathBuf, HashMap<String, Tensor>)> {
    let dir = PathBuf::from(dac_brand::env_os("SAM3_DIR")?);
    let reference = PathBuf::from(dac_brand::env_os("SAM3_REF")?);
    let r = candle_core::safetensors::load(&reference, &Device::Cpu).ok()?;
    Some((dir, r))
}

fn vec(t: &Tensor) -> Vec<f32> {
    t.to_device(&Device::Cpu).unwrap().flatten_all().unwrap().to_vec1::<f32>().unwrap()
}

/// Max |a − b| relative to the reference's max |b|.
fn rel_err(name: &str, a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "{name}: length");
    let scale = b.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-6);
    let err = a.iter().zip(b).fold(0f32, |m, (x, y)| m.max((x - y).abs()));
    eprintln!("{name:>18}: max abs err {err:.2e}, rel {:.2e} (scale {scale:.2e})", err / scale);
    err / scale
}

/// Fraction of pixels where the two logit maps disagree on the mask (> 0).
fn mask_disagreement(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).filter(|(x, y)| (**x > 0.0) != (**y > 0.0)).count() as f32 / a.len() as f32
}

#[test]
fn matches_reference() {
    let Some((dir, r)) = setup() else {
        eprintln!("skipped: set {} and {}", dac_brand::env_var("SAM3_DIR"), dac_brand::env_var("SAM3_REF"));
        return;
    };
    let mut model = Sam3::load(&dir).unwrap();
    eprintln!("device: {:?}", model.device());
    let t = std::time::Instant::now();
    let mut enc = model.encode_pixels(&r["pixel_values"]).unwrap();
    let backbone = enc.backbone().permute((0, 2, 3, 1)).unwrap();
    assert!(rel_err("backbone", &vec(&backbone), &vec(&r["backbone"])) < 1e-3);
    eprintln!("backbone: {:?}", t.elapsed());

    // points: two clicks (single-mask output), one click (best of three)
    let clicks = [Click { x: 0.5, y: 0.55, positive: true }, Click { x: 0.2, y: 0.2, positive: false }];
    let t = std::time::Instant::now();
    let p = model.segment_clicks(&mut enc, &clicks).unwrap();
    eprintln!("clicks (incl. tracker load + features): {:?}", t.elapsed());
    let want = vec(&r["trk_masks"]);
    assert!(rel_err("2-click logits", &p.logits, &want) < 1e-2);
    assert!(mask_disagreement(&p.logits, &want) < 1e-3);
    let t = std::time::Instant::now();
    let p = model.segment_clicks(&mut enc, &clicks[..1]).unwrap();
    eprintln!("one more click: {:?}", t.elapsed());
    let iou = vec(&r["trk_multi_iou"]);
    let best = (0..3).max_by(|a, b| iou[*a].partial_cmp(&iou[*b]).unwrap()).unwrap();
    let want = vec(&r["trk_multi_masks"].i((0, 0, best)).unwrap());
    assert!(rel_err("1-click logits", &p.logits, &want) < 1e-2);
    assert!(mask_disagreement(&p.logits, &want) < 1e-3);

    // tokenizer
    let tok = Tokenizer::load(&dir).unwrap();
    let cases = ["person", "Red  Car", "a dog's toy", "sky, clouds!", "café 2 people", "tree-line"];
    for (i, c) in cases.iter().enumerate() {
        let (ids, _) = tok.encode(c);
        let want: Vec<i64> = r[&format!("tok{i}")].flatten_all().unwrap().to_vec1().unwrap();
        assert_eq!(ids.iter().map(|v| *v as i64).collect::<Vec<_>>(), want, "tokens of {c:?}");
    }

    // text prompts
    for (i, text) in ["moon", "sky", "bird"].iter().enumerate() {
        let t = std::time::Instant::now();
        let out = model.detect(&mut enc, text).unwrap();
        eprintln!("detect {text:?}: {:?}", t.elapsed());
        assert!(rel_err("text features", &vec(&out.text), &vec(&r[&format!("det{i}_text")])) < 1e-3);
        assert!(rel_err("query logits", &out.logits, &vec(&r[&format!("det{i}_logits")])) < 1e-2);
        let presence = vec(&r[&format!("det{i}_presence")])[0];
        assert!((out.presence - presence).abs() < 1e-2, "presence {} vs {presence}", out.presence);
        let boxes: Vec<f32> = out.boxes.iter().flatten().copied().collect();
        assert!(rel_err("boxes", &boxes, &vec(&r[&format!("det{i}_boxes")])) < 1e-2);
        // the best instance's mask
        let logits = vec(&r[&format!("det{i}_logits")]);
        let q = (0..logits.len()).max_by(|a, b| logits[*a].partial_cmp(&logits[*b]).unwrap()).unwrap();
        let ours = vec(&out.masks.i(q).unwrap());
        let want = vec(&r[&format!("det{i}_masks")].i((0, q)).unwrap());
        assert!(rel_err("best mask", &ours, &want) < 2e-2);
        assert!(mask_disagreement(&ours, &want) < 2e-3);
    }
}
