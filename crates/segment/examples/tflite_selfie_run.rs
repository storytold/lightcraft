//! Run the MediaPipe multiclass selfie segmenter on photos and save red-tinted person overlays.
//!
//! `cargo run -p lightcraft-segment --release --example tflite_selfie_run -- <selfie_multiclass_256x256.tflite> <out dir> <photo.jpg>...`
//!
//! The photo is stretched to 256 × 256 and scaled to −1..1; the six logits per pixel (background,
//! hair, body skin, face skin, clothes, other) go through a softmax, and the person mask is
//! 1 − P(background), resized back to the photo bilinearly.

use std::path::Path;

use image::imageops::FilterType;
use lightcraft_segment::tflite::Model;

const SIDE: usize = 256;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(model), Some(out)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: tflite_selfie_run <model.tflite> <out dir> <photo.jpg>...");
        return;
    };
    let model = Model::load(Path::new(model)).unwrap();
    std::fs::create_dir_all(out).unwrap();
    for photo in &args[3..] {
        let img = image::open(photo).unwrap().to_rgb8();
        let small = image::imageops::resize(&img, SIDE as u32, SIDE as u32, FilterType::Triangle);
        let x: Vec<f32> = small.as_raw().iter().map(|&v| v as f32 / 127.5 - 1.0).collect();
        let t = std::time::Instant::now();
        let logits = model.run(&[&x]).unwrap().remove(0);
        let dt = t.elapsed();
        let person: Vec<u8> = logits
            .as_chunks::<6>()
            .0
            .iter()
            .map(|l| {
                let m = l.iter().fold(f32::NEG_INFINITY, |m, v| m.max(*v));
                let e: Vec<f32> = l.iter().map(|v| (v - m).exp()).collect();
                let bg = e[0] / e.iter().sum::<f32>();
                ((1.0 - bg) * 255.0).round() as u8
            })
            .collect();
        let mask = image::GrayImage::from_raw(SIDE as u32, SIDE as u32, person).unwrap();
        let mask = image::imageops::resize(&mask, img.width(), img.height(), FilterType::Triangle);
        let mut over = img.clone();
        for (p, m) in over.pixels_mut().zip(mask.pixels()) {
            let a = 0.55 * m.0[0] as f32 / 255.0;
            for (c, tint) in p.0.iter_mut().zip([255.0, 40.0, 40.0]) {
                *c = (*c as f32 * (1.0 - a) + tint * a).round().clamp(0.0, 255.0) as u8;
            }
        }
        let name = Path::new(photo).file_stem().unwrap().to_string_lossy().to_string();
        let dst = Path::new(out).join(format!("{name}_selfie.jpg"));
        over.save(&dst).unwrap();
        println!("{} → {} ({:.1} ms)", photo, dst.display(), dt.as_secs_f64() * 1e3);
    }
}
