//! The scene (EoMT-S) and depth (Depth Anything V2 Small) ports against the reference
//! implementation (Hugging Face `transformers`, fp32 on the CPU).
//!
//! Needs the checkpoints and a reference file made by `make_refs.py` (kept outside the repo):
//! `LIGHTCRAFT_SCENE_DIR=<eomt dir> LIGHTCRAFT_DEPTH_DIR=<depth dir> LIGHTCRAFT_SCENE_REF=<ref.safetensors>
//! cargo test -p lightcraft-segment --release --test scene_reference -- --nocapture`.
//! Each model is skipped (passes) when its directory or the reference is missing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use candle_core::{Device, Tensor};
use lightcraft_segment::depth::DepthModel;
use lightcraft_segment::scene::{self, SceneModel};

fn reference() -> Option<HashMap<String, Tensor>> {
    let path = PathBuf::from(std::env::var_os("LIGHTCRAFT_SCENE_REF")?);
    candle_core::safetensors::load(&path, &Device::Cpu).ok()
}

fn vec(t: &Tensor) -> Vec<f32> {
    t.to_device(&Device::Cpu).unwrap().to_dtype(candle_core::DType::F32).unwrap().flatten_all().unwrap().to_vec1::<f32>().unwrap()
}

/// Max |a − b| relative to the reference's max |b|.
fn rel_err(name: &str, a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "{name}: length");
    let scale = b.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-6);
    let err = a.iter().zip(b).fold(0f32, |m, (x, y)| m.max((x - y).abs()));
    let mean = a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.len().max(1) as f32;
    eprintln!("{name:>22}: max abs err {err:.2e}, rel {:.2e}, mean abs {mean:.2e} (scale {scale:.2e})", err / scale);
    err / scale
}

fn rgb(r: &HashMap<String, Tensor>) -> (Vec<u8>, usize, usize) {
    let t = &r["rgb"];
    let (h, w, _) = t.dims3().unwrap();
    (t.flatten_all().unwrap().to_vec1::<u8>().unwrap(), w, h)
}

fn stage<'a>(stages: &'a [(&str, Tensor)], name: &str) -> &'a Tensor {
    &stages.iter().find(|(n, _)| *n == name).unwrap().1
}

#[test]
fn scene_matches_reference() {
    let (Some(dir), Some(r)) = (std::env::var_os("LIGHTCRAFT_SCENE_DIR"), reference()) else {
        lightcraft_testkit::skip!(Model, "set LIGHTCRAFT_SCENE_DIR and LIGHTCRAFT_SCENE_REF");
        return;
    };
    let t = Instant::now();
    let model = SceneModel::load_on(&PathBuf::from(dir), Device::Cpu).unwrap();
    eprintln!("scene load: {:?}", t.elapsed());
    let (img, w, h) = rgb(&r);

    // preprocessing: Pillow bilinear, exact up to Pillow's own rounding
    let (pixels, iw, ih) = SceneModel::preprocess(&img, w, h).unwrap();
    assert!(rel_err("pixels", &pixels, &vec(&r["eomt_pixels"])) < 1e-3);

    // the network on the reference input, stage by stage
    let raw = model.raw(&r["eomt_pixels"]).unwrap();
    for (ours, theirs) in [("emb", "eomt_emb"), ("layer8", "eomt_layer8"), ("final", "eomt_final"), ("upscaled", "eomt_upscaled")] {
        assert!(rel_err(ours, &vec(stage(&raw.stages, ours)), &vec(&r[theirs])) < 1e-3, "{ours}");
    }
    let ml = vec(&raw.mask_logits);
    let want = vec(&r["eomt_mask_logits"]);
    assert!(rel_err("mask logits", &ml, &want) < 1e-3);
    let dis = ml.iter().zip(&want).filter(|(a, b)| (**a > 0.0) != (**b > 0.0)).count() as f32 / ml.len() as f32;
    eprintln!("{:>22}: {dis:.2e}", "mask disagreement");
    assert!(dis < 1e-3);
    assert!(rel_err("class logits", &vec(&raw.class_logits), &vec(&r["eomt_class_logits"])) < 1e-3);

    // semantic maps over the image part
    let (cw, ch) = scene::mask_crop(iw, ih);
    let s = scene::postprocess(&raw, cw, ch).unwrap();
    let sem = &r["eomt_sem"];
    assert_eq!(sem.dims(), &[scene::CLASSES, ch, cw]);
    assert!(rel_err("semantic probs", &s.probs, &vec(sem)) < 1e-3);

    // end to end from the photo, timed
    for _ in 0..2 {
        let t = Instant::now();
        let s = model.segment(&img, w, h).unwrap();
        eprintln!("scene segment (preprocess + model + post): {:?}", t.elapsed());
        assert!(rel_err("semantic probs (e2e)", &s.probs, &vec(sem)) < 2e-2);
        let found: Vec<String> = s.instances.iter().map(|i| format!("{} {:.2}", scene::COCO_PANOPTIC_LABELS[i.class], i.score)).collect();
        eprintln!("instances: {found:?}");
    }
}

#[test]
fn depth_matches_reference() {
    let (Some(dir), Some(r)) = (std::env::var_os("LIGHTCRAFT_DEPTH_DIR"), reference()) else {
        lightcraft_testkit::skip!(Model, "set LIGHTCRAFT_DEPTH_DIR and LIGHTCRAFT_SCENE_REF");
        return;
    };
    let t = Instant::now();
    let model = DepthModel::load_on(&PathBuf::from(dir), Device::Cpu).unwrap();
    eprintln!("depth load: {:?}", t.elapsed());
    let (img, w, h) = rgb(&r);

    // preprocessing: torchvision's antialiased bicubic vs our Pillow-style one (±1 level)
    let (pixels, iw, ih) = DepthModel::preprocess(&img, w, h).unwrap();
    let want = &r["da_pixels"];
    assert_eq!(want.dims(), &[1, 3, ih, iw]);
    assert!(rel_err("pixels", &pixels, &vec(want)) < 2e-2);

    let (d, stages) = model.raw(want).unwrap();
    for (ours, theirs) in [("emb", "da_emb"), ("feat0", "da_feat0"), ("feat3", "da_feat3"), ("fused0", "da_fused0"), ("fused3", "da_fused3")] {
        assert!(rel_err(ours, &vec(stage(&stages, ours)), &vec(&r[theirs])) < 1e-3, "{ours}");
    }
    assert!(rel_err("depth", &vec(&d), &vec(&r["da_depth"])) < 1e-3);

    for _ in 0..2 {
        let t = Instant::now();
        let map = model.estimate(&img, w, h).unwrap();
        eprintln!("depth estimate (preprocess + model + normalize): {:?}", t.elapsed());
        assert_eq!((map.width, map.height), (iw, ih));
        let want = lightcraft_segment::depth::normalized(vec(&r["da_depth"]), iw, ih);
        assert!(rel_err("normalized depth (e2e)", &map.data, &want.data) < 2e-2);
    }
}
