//! The SigLIP 2 port against the reference implementation (Hugging Face `transformers`, fp32).
//!
//! Needs the `google/siglip2-base-patch16-256` files:
//! `LIGHTCRAFT_VISION_DIR=<model dir> cargo test -p lightcraft-vision --release --test reference -- --nocapture`.
//! The committed fixture (`tests/fixtures/siglip2_reference.json`, from `tools/siglip_reference.py`)
//! holds token ids and image–text scores; set `LIGHTCRAFT_VISION_REF=<full json>` as well to
//! compare every embedding. Skipped (passes, with a note) without the model folder.

mod inputs;

use std::path::PathBuf;

use lightcraft_vision::manifest::builtin_siglip2_b16_256;
use lightcraft_vision::tokenizer::SentencePiece;
use lightcraft_vision::{Towers, dot};
use serde_json::Value;

fn model_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("LIGHTCRAFT_VISION_DIR")?);
    dir.join("model.safetensors").is_file().then_some(dir)
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/siglip2_reference.json")).unwrap()
}

fn ids(v: &Value) -> Vec<u32> {
    v.as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as u32).collect()
}

fn floats(v: &Value) -> Vec<f32> {
    v.as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect()
}

#[test]
fn tokenizer_matches_reference_ids() {
    let Some(dir) = model_dir() else {
        eprintln!("skipped: set LIGHTCRAFT_VISION_DIR to the siglip2-base-patch16-256 folder");
        return;
    };
    let t = std::time::Instant::now();
    let tok = SentencePiece::load(&dir).unwrap();
    eprintln!("tokenizer load: {:?}", t.elapsed());
    let f = fixture();
    let context = f["context"].as_u64().unwrap() as usize;
    let cases = f["tokenCases"].as_array().unwrap();
    assert_eq!(cases.len(), inputs::token_cases().len());
    for (case, text) in cases.iter().zip(inputs::token_cases()) {
        assert_eq!(case["text"].as_str().unwrap(), text);
        assert_eq!(tok.encode_padded(&text, context), ids(&case["ids"]), "tokens of {text:?}");
    }
    for (want, text) in f["textIds"].as_array().unwrap().iter().zip(inputs::TEXTS) {
        assert_eq!(tok.encode_padded(text, context), ids(want), "tokens of {text:?}");
    }
    eprintln!("{} tokenizer cases match exactly", cases.len() + inputs::TEXTS.len());
}

#[test]
fn embeddings_and_scores_match_reference() {
    let Some(dir) = model_dir() else {
        eprintln!("skipped: set LIGHTCRAFT_VISION_DIR to the siglip2-base-patch16-256 folder");
        return;
    };
    let f = fixture();
    let t = std::time::Instant::now();
    let model = lightcraft_vision::load(&dir, &builtin_siglip2_b16_256(), Towers::BOTH).unwrap();
    eprintln!("load: {:?}", t.elapsed());
    let images: Vec<_> = inputs::images().into_iter().map(|(_, i)| i).collect();
    let t = std::time::Instant::now();
    let img = model.embed_images(&images).unwrap();
    eprintln!("{} images: {:?}", images.len(), t.elapsed());
    let t = std::time::Instant::now();
    let txt = model.embed_texts(inputs::TEXTS).unwrap();
    eprintln!("{} texts: {:?}", inputs::TEXTS.len(), t.elapsed());
    assert_eq!(img[0].len(), 768);

    // scores
    let probs = f["probs"].as_array().unwrap();
    let mut worst = 0f32;
    for (i, row) in probs.iter().enumerate() {
        for (j, want) in floats(row).iter().enumerate() {
            let got = model.score(&img[i], &txt[j]);
            worst = worst.max((got - want).abs());
        }
    }
    eprintln!("sigmoid scores: max |Δ| {worst:.2e}");
    assert!(worst < 0.01, "score delta {worst}");

    // embeddings, when the full reference is at hand
    let Some(full) = std::env::var_os("LIGHTCRAFT_VISION_REF") else {
        eprintln!("embeddings not compared: set LIGHTCRAFT_VISION_REF to the full reference json");
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
