//! The content models on the GPU against the candle CPU path and the Hugging Face references, with the real weights:
//!
//! `LIGHTCRAFT_VISION_DIR=<siglip2-b16-256> LIGHTCRAFT_CLIP_DIR=<clip-vit-b32-openai> LIGHTCRAFT_SO400M_DIR=<siglip2-so400m-16-384>
//!  cargo test --release -p lightcraft-gpu --features vision --test vision_towers -- --nocapture`
//!
//! (`LIGHTCRAFT_VISION_REF` / `LIGHTCRAFT_CLIP_REF`: the full reference embeddings, as in `crates/vision/tests`.) Each
//! model is skipped (passes, with a note) without its folder or without a usable GPU. `--ignored` adds the timings at
//! batch 1, 8 and 32 on both.

#[path = "../../vision/tests/inputs.rs"]
mod inputs;

use std::path::PathBuf;
use std::time::Instant;

use lightcraft_vision::manifest::{Manifest, builtin_clip_vit_b32_openai, builtin_siglip2_b16_256, builtin_siglip2_so400m_16_384};
use lightcraft_vision::{ContentModel, Rgb8, Towers, dot};
use serde_json::Value;

fn dir(var: &str) -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os(var)?);
    dir.join("model.safetensors").is_file().then_some(dir)
}

fn floats(v: &Value) -> Vec<f32> {
    v.as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect()
}

/// The CPU model and the GPU model, or `None` (with a note) when either is not at hand.
fn both(var: &str, m: &Manifest) -> Option<(Box<dyn ContentModel>, Box<dyn ContentModel>)> {
    let Some(dir) = dir(var) else {
        eprintln!("skipped: set {var} to the {} folder", m.id);
        return None;
    };
    if let Err(e) = lightcraft_gpu::transformer::adapter() {
        eprintln!("skipped: no GPU here ({e})");
        return None;
    }
    let t = Instant::now();
    let cpu = lightcraft_vision::load(&dir, m, Towers::BOTH).unwrap();
    eprintln!("{}: CPU load {:?}", m.id, t.elapsed());
    let t = Instant::now();
    let accel = lightcraft_gpu::transformer::accelerator();
    let gpu = lightcraft_vision::load_with(&dir, m, Towers::BOTH, Some(&*accel)).unwrap();
    eprintln!("{}: GPU load {:?} ({})", m.id, t.elapsed(), gpu.runs_on());
    assert!(gpu.runs_on().starts_with("GPU: "), "{}", gpu.runs_on());
    Some((cpu, gpu))
}

/// More images than the reference set: the five references plus every demo scene, so a pass has odd sizes.
fn images() -> Vec<Rgb8> {
    let mut v: Vec<Rgb8> = inputs::images().into_iter().map(|(_, i)| i).collect();
    for s in lightcraft_scenes::demo_library().iter().take(8) {
        let rgba = s.render(320, 240).to_srgb8();
        v.push(Rgb8 { width: rgba.width, height: rgba.height, data: rgba.data.iter().map(|p| [p[0], p[1], p[2]]).collect() });
    }
    v
}

const MORE_TEXTS: &[&str] = &["kids on a beach at sunset", "trees seen from above", "a dog on a jetty", "ein Hund"];

/// GPU against CPU: every embedding's cosine ≥ 0.999 and every score within 0.005. Returns the GPU's embeddings.
fn agree(m: &dyn ContentModel, cpu: &dyn ContentModel) -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
    let images = images();
    let texts: Vec<&str> = inputs::TEXTS.iter().chain(MORE_TEXTS).copied().collect();
    let t = Instant::now();
    let gi = m.embed_images(&images).unwrap();
    let gt = m.embed_texts(&texts).unwrap();
    eprintln!("GPU: {} images + {} texts in {:?}", images.len(), texts.len(), t.elapsed());
    let t = Instant::now();
    let ci = cpu.embed_images(&images).unwrap();
    let ct = cpu.embed_texts(&texts).unwrap();
    eprintln!("CPU: {:?}", t.elapsed());
    let min = |a: &[Vec<f32>], b: &[Vec<f32>]| a.iter().zip(b).map(|(x, y)| dot(x, y)).fold(1f32, f32::min);
    let (img_cos, txt_cos) = (min(&gi, &ci), min(&gt, &ct));
    let mut worst = 0f32;
    for (g, c) in gi.iter().zip(&ci) {
        for (gt, ct) in gt.iter().zip(&ct) {
            worst = worst.max((m.score(g, gt) - cpu.score(c, ct)).abs());
        }
    }
    eprintln!("GPU vs CPU: image cosine ≥ {img_cos:.6}, text cosine ≥ {txt_cos:.6}, scores within {worst:.2e}");
    assert!(img_cos >= 0.999 && txt_cos >= 0.999, "embeddings differ: {img_cos} / {txt_cos}");
    assert!(worst <= 0.005, "scores differ by {worst}");
    // a text alone, and one image alone, give what they give in a batch
    let one = m.embed_texts(&texts[..1]).unwrap();
    assert!(dot(&one[0], &gt[0]) > 0.99999);
    let one = m.embed_images(&images[..1]).unwrap();
    assert!(dot(&one[0], &gi[0]) > 0.99999);
    assert!(m.gpu_fallback().is_none(), "{:?}", m.gpu_fallback());
    (gi, gt)
}

fn full_reference(var: &str, img: &[Vec<f32>], txt: &[Vec<f32>]) {
    let Some(full) = std::env::var_os(var) else {
        eprintln!("embeddings not compared with the reference: set {var}");
        return;
    };
    let full: Value = serde_json::from_str(&std::fs::read_to_string(full).unwrap()).unwrap();
    for (key, ours) in [("imageEmbeds", img), ("textEmbeds", txt)] {
        let min = full[key].as_array().unwrap().iter().zip(ours).map(|(w, g)| dot(&floats(w), g)).fold(1f32, f32::min);
        eprintln!("{key}: min cosine to the reference {min:.6}");
        assert!(min >= 0.999, "{key}: cosine {min}");
    }
}

#[test]
fn siglip_base_on_the_gpu_matches_the_cpu_and_the_reference() {
    let m = builtin_siglip2_b16_256();
    let Some((cpu, gpu)) = both("LIGHTCRAFT_VISION_DIR", &m) else { return };
    let (img, txt) = agree(&*gpu, &*cpu);
    let f: Value = serde_json::from_str(include_str!("../../vision/tests/fixtures/siglip2_reference.json")).unwrap();
    let mut worst = 0f32;
    for (i, row) in f["probs"].as_array().unwrap().iter().enumerate() {
        for (j, want) in floats(row).iter().enumerate() {
            worst = worst.max((gpu.score(&img[i], &txt[j]) - want).abs());
        }
    }
    eprintln!("sigmoid scores vs the reference: max |Δ| {worst:.2e}");
    assert!(worst < 0.005, "score delta {worst}");
    full_reference("LIGHTCRAFT_VISION_REF", &img[..5], &txt[..inputs::TEXTS.len()]);
}

#[test]
fn clip_on_the_gpu_matches_the_cpu_and_the_reference() {
    let m = builtin_clip_vit_b32_openai();
    let Some((cpu, gpu)) = both("LIGHTCRAFT_CLIP_DIR", &m) else { return };
    let (img, txt) = agree(&*gpu, &*cpu);
    let f: Value = serde_json::from_str(include_str!("../../vision/tests/fixtures/clip_b32_reference.json")).unwrap();
    let mut worst = 0f32;
    for (i, row) in f["cosines"].as_array().unwrap().iter().enumerate() {
        for (j, want) in floats(row).iter().enumerate() {
            worst = worst.max((dot(&img[i], &txt[j]) - want).abs());
        }
    }
    eprintln!("cosines vs the reference: max |Δ| {worst:.2e}");
    assert!(worst < 2e-3, "cosine delta {worst}");
    full_reference("LIGHTCRAFT_CLIP_REF", &img[..5], &txt[..inputs::TEXTS.len()]);
}

#[test]
fn so400m_on_the_gpu_matches_the_cpu() {
    let m = builtin_siglip2_so400m_16_384();
    let Some((cpu, gpu)) = both("LIGHTCRAFT_SO400M_DIR", &m) else { return };
    agree(&*gpu, &*cpu);
}

/// ms per image at batch 1, 8 and 32 and ms per text, on the GPU and the CPU, for every model at hand.
#[test]
#[ignore = "timings: run by hand (see the doc comment)"]
fn timings() {
    for (var, m) in [
        ("LIGHTCRAFT_VISION_DIR", builtin_siglip2_b16_256()),
        ("LIGHTCRAFT_CLIP_DIR", builtin_clip_vit_b32_openai()),
        ("LIGHTCRAFT_SO400M_DIR", builtin_siglip2_so400m_16_384()),
    ] {
        let Some((cpu, gpu)) = both(var, &m) else { continue };
        // a photo-sized input, so the resize is part of the time (as when indexing)
        let img = Rgb8::from_fn(1024, 683, |x, y| [(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8]);
        let skip_cpu_32 = std::env::var_os("LC_SKIP_CPU").is_some();
        for (name, model) in [("GPU", &gpu), ("CPU", &cpu)] {
            model.embed_images(std::slice::from_ref(&img)).unwrap();
            for batch in [1usize, 8, 32] {
                if name == "CPU" && skip_cpu_32 && batch > 8 {
                    continue;
                }
                let imgs = vec![img.clone(); batch];
                let runs = if batch == 1 { 5 } else { 2 };
                let t = Instant::now();
                for _ in 0..runs {
                    model.embed_images(&imgs).unwrap();
                }
                println!("{} {name}: images, batch {batch:>2}: {:.1} ms/image", m.id, t.elapsed().as_secs_f64() * 1000.0 / (runs * batch) as f64);
            }
            model.embed_texts(&["warm up"]).unwrap();
            let texts = ["a beach", "kids on a beach at sunset", "a red car", "a scorpion", "une plage", "海滩", "a dog", "mountains in fog"];
            let t = Instant::now();
            for s in texts {
                model.embed_texts(&[s]).unwrap();
            }
            println!("{} {name}: texts one at a time: {:.1} ms/text", m.id, t.elapsed().as_secs_f64() * 1000.0 / texts.len() as f64);
            let t = Instant::now();
            model.embed_texts(&texts).unwrap();
            println!("{} {name}: texts, batch of 8: {:.1} ms/text", m.id, t.elapsed().as_secs_f64() * 1000.0 / texts.len() as f64);
        }
    }
}
