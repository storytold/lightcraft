//! Compares SAM 3 masks made from the candle backbone with masks made from a Core ML backbone (an experiment):
//! `sam3_agree <model dir> <backbone.mlmodelc | build:<scratch dir>> <image.rgb> <w> <h> [cpu|gpu|ane]`: `build:` writes
//! and compiles the backbone from the checkpoint in Rust (the compiled model is kept in the scratch dir).

use candle_core::{D, Tensor};
use lightcraft_coreml::{ComputeUnits, Model};
use lightcraft_segment::{Click, INPUT, Sam3, mask};

fn iou(a: &[f32], b: &[f32]) -> f32 {
    let (mut i, mut u) = (0usize, 0usize);
    for (x, y) in a.iter().zip(b) {
        let (p, q) = (*x > 0.0, *y > 0.0);
        i += usize::from(p && q);
        u += usize::from(p || q);
    }
    if u == 0 { 1.0 } else { i as f32 / u as f32 }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 5 {
        return Err("usage: sam3_agree <model dir> <backbone.mlmodelc> <image.rgb> <w> <h> [cpu|gpu|ane]".into());
    }
    let (w, h): (usize, usize) = (a[3].parse()?, a[4].parse()?);
    let units = match a.get(5).map(String::as_str) {
        Some("cpu") => ComputeUnits::CpuOnly,
        Some("gpu") => ComputeUnits::CpuAndGpu,
        _ => ComputeUnits::CpuAndNeuralEngine,
    };
    let mut sam = Sam3::load(std::path::Path::new(&a[0]))?;
    let (path, output) = match a[1].strip_prefix("build:") {
        Some(scratch) => {
            let scratch = std::path::Path::new(scratch);
            let t = std::time::Instant::now();
            let (package, output) = sam.write_coreml_backbone(&scratch.join("backbone.mlpackage"))?;
            eprintln!("wrote the package in {:?} (output {output})", t.elapsed());
            let t = std::time::Instant::now();
            let compiled = lightcraft_coreml::compile(&package, scratch).map_err(|e| format!("{e:?}"))?;
            eprintln!("compiled in {:?}", t.elapsed());
            (compiled, output)
        }
        // a compiled model, with the name of its output after a `#` when it is not the one coremltools gave
        None => match a[1].split_once('#') {
            Some((path, output)) => (std::path::PathBuf::from(path), output.to_string()),
            None => (std::path::PathBuf::from(&a[1]), "add_127".to_string()),
        },
    };
    let t = std::time::Instant::now();
    let core = Model::load(&path, units, "x", &output).map_err(|e| format!("{e:?}"))?;
    eprintln!("loaded in {:?}", t.elapsed());
    let rgb = std::fs::read(&a[2])?;
    let pixels = Tensor::from_vec(mask::preprocess(&rgb, w, h, INPUT)?, (1, 3, INPUT, INPUT), sam.device())?;

    let t = std::time::Instant::now();
    let mut ea = sam.encode_pixels(&pixels)?;
    lightcraft_segment::sync(&sam)?;
    eprintln!("candle backbone {:?}", t.elapsed());

    let emb: Vec<f32> = sam.embed_pixels(&pixels)?.flatten_all()?.to_vec1()?;
    let t = std::time::Instant::now();
    let out = core.predict(&[1, 5184, 1024], &emb).map_err(|e| format!("{e:?}"))?;
    eprintln!("core ml backbone {:?}", t.elapsed());
    let feats = Tensor::from_vec(out.data, (1, 72, 72, 1024), sam.device())?.permute((0, 3, 1, 2))?.contiguous()?;
    let mut eb = sam.encoded_from(feats);

    let (fa, fb) = (ea.backbone().flatten_all()?, eb.backbone().flatten_all()?);
    let dot = (&fa * &fb)?.sum_all()?.to_scalar::<f32>()?;
    let (na, nb) = (fa.sqr()?.sum_all()?.to_scalar::<f32>()?.sqrt(), fb.sqr()?.sum_all()?.to_scalar::<f32>()?.sqrt());
    let rel = (&fa - &fb)?.sqr()?.sum_all()?.to_scalar::<f32>()?.sqrt() / na;
    let _ = D::Minus1;
    println!("backbone features: cosine {:.5}, relative L2 error {:.4}", dot / (na * nb), rel);

    println!("clicks (positive, normalized x y): IoU of candle vs core ml mask, and the two scores");
    let mut ious = Vec::new();
    for y in [0.25f32, 0.5, 0.75] {
        for x in [0.15f32, 0.4, 0.65, 0.85] {
            let c = [Click { x, y, positive: true }];
            let pa = sam.segment_clicks(&mut ea, &c)?;
            let pb = sam.segment_clicks(&mut eb, &c)?;
            let i = iou(&pa.logits, &pb.logits);
            ious.push(i);
            println!("  ({x:.2},{y:.2}) IoU {i:.4}  score {:.3} vs {:.3}", pa.score, pb.score);
        }
    }
    println!("  clicks: mean IoU {:.4}, min {:.4}", ious.iter().sum::<f32>() / ious.len() as f32, ious.iter().cloned().fold(1.0, f32::min));

    println!("text prompts (threshold 0.5)");
    for text in ["ibex", "goat", "animal", "rock", "grass", "tree", "sky", "person", "building"] {
        let pa = sam.segment_text(&mut ea, text, 0.5)?;
        let pb = sam.segment_text(&mut eb, text, 0.5)?;
        match (pa, pb) {
            (Some(p), Some(q)) => println!("  {text:9} IoU {:.4}  score {:.3} vs {:.3}", iou(&p.logits, &q.logits), p.score, q.score),
            (None, None) => println!("  {text:9} nothing in both"),
            (Some(p), None) => println!("  {text:9} candle found it (score {:.3}), core ml found nothing", p.score),
            (None, Some(q)) => println!("  {text:9} core ml found it (score {:.3}), candle found nothing", q.score),
        }
    }
    Ok(())
}
