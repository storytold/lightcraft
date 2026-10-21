//! The CLIP ViT-B/32 port against the reference implementation (Hugging Face `transformers`, fp32, Pillow resize).
//!
//! Needs the `openai/clip-vit-base-patch32` files (`model.safetensors`, `config.json`, `vocab.json`, `merges.txt`):
//! `LIGHTCRAFT_CLIP_DIR=<model dir> cargo test -p lightcraft-vision --release --test reference_clip -- --nocapture`.
//! The committed fixture (`tests/fixtures/clip_b32_reference.json`, from `tools/clip_reference.py`) holds token ids and
//! image–text cosines; set `LIGHTCRAFT_CLIP_REF=<full json>` as well to compare every embedding. Skipped (passes, with
//! a note) without the model folder.

mod inputs;

use std::path::PathBuf;

use lightcraft_vision::manifest::builtin_clip_vit_b32_openai;
use lightcraft_vision::tokenizer::ClipBpe;
use lightcraft_vision::{Towers, dot};
use serde_json::Value;

fn model_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("LIGHTCRAFT_CLIP_DIR")?);
    dir.join("model.safetensors").is_file().then_some(dir)
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/clip_b32_reference.json")).unwrap()
}

fn ids(v: &Value) -> Vec<u32> {
    v.as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as u32).collect()
}

fn floats(v: &Value) -> Vec<f32> {
    v.as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect()
}

#[test]
fn clip_tokenizer_matches_reference_ids() {
    let Some(dir) = model_dir() else {
        eprintln!("skipped: set LIGHTCRAFT_CLIP_DIR to the clip-vit-base-patch32 folder");
        return;
    };
    let t = std::time::Instant::now();
    let tok = ClipBpe::load(&dir).unwrap();
    eprintln!("tokenizer load: {:?}", t.elapsed());
    let f = fixture();
    let context = f["context"].as_u64().unwrap() as usize;
    let cases = f["tokenCases"].as_array().unwrap();
    assert_eq!(cases.len(), inputs::clip_token_cases().len());
    for (case, text) in cases.iter().zip(inputs::clip_token_cases()) {
        assert_eq!(case["text"].as_str().unwrap(), text);
        assert_eq!(tok.encode(&text, context).0, ids(&case["ids"]), "tokens of {text:?}");
    }
    for (want, text) in f["textIds"].as_array().unwrap().iter().zip(inputs::TEXTS) {
        assert_eq!(tok.encode(text, context).0, ids(want), "tokens of {text:?}");
    }
    eprintln!("{} tokenizer cases match exactly", cases.len() + inputs::TEXTS.len());
}

#[test]
fn clip_embeddings_and_cosines_match_reference() {
    let Some(dir) = model_dir() else {
        eprintln!("skipped: set LIGHTCRAFT_CLIP_DIR to the clip-vit-base-patch32 folder");
        return;
    };
    let f = fixture();
    let t = std::time::Instant::now();
    let model = lightcraft_vision::load(&dir, &builtin_clip_vit_b32_openai(), Towers::BOTH).unwrap();
    eprintln!("load: {:?}", t.elapsed());
    let images: Vec<_> = inputs::images().into_iter().map(|(_, i)| i).collect();
    let t = std::time::Instant::now();
    let img = model.embed_images(&images).unwrap();
    eprintln!("{} images: {:?}", images.len(), t.elapsed());
    let t = std::time::Instant::now();
    let txt = model.embed_texts(inputs::TEXTS).unwrap();
    eprintln!("{} texts: {:?}", inputs::TEXTS.len(), t.elapsed());
    assert_eq!(img[0].len(), 512);
    let scale = model.scoring().unwrap().scale;
    assert!((scale - f["logitScaleExp"].as_f64().unwrap() as f32).abs() < 1e-3, "scale {scale}");

    // cosines
    let mut worst = 0f32;
    for (i, row) in f["cosines"].as_array().unwrap().iter().enumerate() {
        for (j, want) in floats(row).iter().enumerate() {
            worst = worst.max((dot(&img[i], &txt[j]) - want).abs());
        }
    }
    eprintln!("cosines: max |Δ| {worst:.2e}");
    assert!(worst < 2e-3, "cosine delta {worst}");

    // embeddings, when the full reference is at hand
    let Some(full) = std::env::var_os("LIGHTCRAFT_CLIP_REF") else {
        eprintln!("embeddings not compared: set LIGHTCRAFT_CLIP_REF to the full reference json");
        return;
    };
    let full: Value = serde_json::from_str(&std::fs::read_to_string(full).unwrap()).unwrap();
    for (key, ours) in [("imageEmbeds", &img), ("textEmbeds", &txt)] {
        let mut min = 1f32;
        for (want, got) in full[key].as_array().unwrap().iter().zip(ours.iter()) {
            min = min.min(dot(&floats(want), got));
        }
        eprintln!("{key}: min cosine to reference {min:.6}");
        assert!(min >= 0.999, "{key}: cosine {min}");
    }
}
