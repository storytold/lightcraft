//! The SAM 2.1 port against the reference implementation (Hugging Face `transformers`
//! `Sam2Model`, fp32 CPU), stage by stage.
//!
//! Needs the `facebook/sam2.1-hiera-tiny` checkpoint (Hugging Face format) and a reference file
//! made by the scratch script `make_ref.py` (inputs, backbone stages, neck levels, decoder
//! features and the decoder outputs for a one-click and a two-click prompt):
//! `LIGHTCRAFT_SAM2_DIR=<model dir> LIGHTCRAFT_SAM2_REF=<ref.safetensors> cargo test -p lightcraft-segment --release --test sam2_reference -- --nocapture`.
//! Skipped (passes) when either is missing.

use std::collections::HashMap;
use std::path::PathBuf;

use candle_core::{Device, IndexOp, Tensor};
use lightcraft_segment::{Click, SAM2_INPUT, SAM2_MASK_SIDE, Sam2, mask};

fn setup() -> Option<(PathBuf, HashMap<String, Tensor>)> {
    let dir = PathBuf::from(std::env::var_os("LIGHTCRAFT_SAM2_DIR")?);
    let reference = PathBuf::from(std::env::var_os("LIGHTCRAFT_SAM2_REF")?);
    let r = candle_core::safetensors::load(&reference, &Device::Cpu).ok()?;
    Some((dir, r))
}

fn vec(t: &Tensor) -> Vec<f32> {
    t.to_device(&Device::Cpu).unwrap().to_dtype(candle_core::DType::F32).unwrap().flatten_all().unwrap().to_vec1::<f32>().unwrap()
}

/// Max |a − b| relative to the reference's max |b|.
fn rel_err(name: &str, a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "{name}: length");
    let scale = b.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-6);
    let err = a.iter().zip(b).fold(0f32, |m, (x, y)| m.max((x - y).abs()));
    eprintln!("{name:>20}: max abs err {err:.2e}, rel {:.2e} (scale {scale:.2e})", err / scale);
    err / scale
}

/// Fraction of pixels where the two logit maps disagree on the mask (> 0).
fn mask_disagreement(name: &str, a: &[f32], b: &[f32]) -> f32 {
    let d = a.iter().zip(b).filter(|(x, y)| (**x > 0.0) != (**y > 0.0)).count() as f32 / a.len() as f32;
    eprintln!("{name:>20}: mask disagreement {d:.2e}");
    d
}

fn clicks(t: &Tensor) -> Vec<Click> {
    let v: Vec<Vec<f32>> = t.to_vec2().unwrap();
    v.iter().map(|c| Click { x: c[0], y: c[1], positive: c[2] > 0.5 }).collect()
}

#[test]
fn matches_reference() {
    let Some((dir, r)) = setup() else {
        lightcraft_testkit::skip!(Model, "set LIGHTCRAFT_SAM2_DIR and LIGHTCRAFT_SAM2_REF");
        return;
    };
    let t = std::time::Instant::now();
    let model = Sam2::load_on(&dir, Device::Cpu).unwrap();
    eprintln!("load: {:?}", t.elapsed());

    // preprocessing: our resize against the processor's (8-bit rounding may differ by a level)
    let rgb: Vec<u8> = r["rgb"].flatten_all().unwrap().to_vec1().unwrap();
    let size: Vec<i64> = r["size"].to_vec1().unwrap();
    let (w, h) = (size[0] as usize, size[1] as usize);
    let ours = mask::preprocess_normalized(&rgb, w, h, SAM2_INPUT, mask::IMAGENET_MEAN, mask::IMAGENET_STD).unwrap();
    let want = vec(&r["pixel_values"]);
    let level = 1.0 / 255.0 / 0.224;
    let off = ours.iter().zip(&want).filter(|(a, b)| (*a - *b).abs() > 0.5 * level).count();
    let max = ours.iter().zip(&want).fold(0f32, |m, (a, b)| m.max((a - b).abs()));
    eprintln!("       preprocessing: {off} of {} values off by more than half a level, max diff {:.2} levels", ours.len(), max / level);
    assert!(max < 1.6 * level && (off as f32) < 0.01 * ours.len() as f32);

    // encoder, stage by stage, from the reference's own pixels
    let px = &r["pixel_values"];
    let t = std::time::Instant::now();
    let (stages, levels) = model.encoder_stages(px).unwrap();
    eprintln!("encoder stages: {:?}", t.elapsed());
    for (i, s) in stages.iter().enumerate() {
        assert!(rel_err(&format!("stage{i}"), &vec(s), &vec(&r[&format!("stage{i}")])) < 1e-3);
    }
    for (i, l) in levels.iter().enumerate() {
        let l = l.permute((0, 3, 1, 2)).unwrap();
        assert!(rel_err(&format!("fpn{i}"), &vec(&l), &vec(&r[&format!("fpn{i}")])) < 1e-3);
    }
    let t = std::time::Instant::now();
    let enc = model.encode_pixels(px).unwrap();
    eprintln!("encode: {:?}", t.elapsed());
    let (image, s0, s1) = enc.features().unwrap();
    assert!(rel_err("image (+no memory)", &vec(&image), &vec(&r["image"])) < 1e-3);
    assert!(rel_err("s0", &vec(&s0), &vec(&r["s0"])) < 1e-3);
    assert!(rel_err("s1", &vec(&s1), &vec(&r["s1"])) < 1e-3);

    for name in ["one", "two"] {
        let c = clicks(&r[&format!("{name}_clicks")]);
        let sparse = model.sparse_embeddings(&c).unwrap();
        assert!(rel_err(&format!("{name} sparse"), &vec(&sparse), &vec(&r[&format!("{name}_sparse")])) < 1e-4);
        let t = std::time::Instant::now();
        let d = model.decode(&enc, &c).unwrap();
        eprintln!("{name}: decode {:?}", t.elapsed());
        let want_masks = &r[&format!("{name}_multi_masks")];
        for k in 0..3 {
            let a = vec(&d.masks.i(k + 1).unwrap());
            let b = vec(&want_masks.i(k).unwrap());
            assert_eq!(a.len(), SAM2_MASK_SIDE * SAM2_MASK_SIDE);
            assert!(rel_err(&format!("{name} mask {}", k + 1), &a, &b) < 1e-3);
            assert!(mask_disagreement(&format!("{name} mask {}", k + 1), &a, &b) < 1e-3);
        }
        let iou = vec(&r[&format!("{name}_multi_iou")]);
        assert!(rel_err(&format!("{name} iou"), &d.iou[1..], &iou) < 1e-3);
        let obj = vec(&r[&format!("{name}_obj")]);
        assert!(rel_err(&format!("{name} object score"), &[d.object_score], &obj) < 1e-3);

        // the reference's `multimask_output=False` (single mask with the stability fallback)
        let p = model.segment_clicks_with(&enc, &c, false).unwrap();
        let want = vec(&r[&format!("{name}_single_mask")]);
        assert!(rel_err(&format!("{name} single"), &p.logits, &want) < 1e-3);
        assert!(mask_disagreement(&format!("{name} single"), &p.logits, &want) < 1e-3);

        // the public choice: one click → best of three; more → single
        let p = model.segment_clicks(&enc, &c).unwrap();
        let want = if c.len() == 1 {
            let best = (0..3).max_by(|a, b| iou[*a].partial_cmp(&iou[*b]).unwrap()).unwrap();
            vec(&want_masks.i(best).unwrap())
        } else {
            want
        };
        assert!(mask_disagreement(&format!("{name} segment_clicks"), &p.logits, &want) < 1e-3);
        assert!(p.score > 0.0 && p.score <= 1.0);
    }
}
