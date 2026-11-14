//! Times the SigLIP 2 towers on this machine:
//! `cargo run --release -p lightcraft-vision --example vision_bench -- <model dir> [image|text|both]`.
//! Reports ms per image at batch 1, 8 and 32, ms per text, and peak memory (VmHWM, Linux).

use std::time::Instant;

use lightcraft_vision::manifest::builtin_siglip2_b16_256;
use lightcraft_vision::{Rgb8, Towers};

fn peak_rss_mb() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kb: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024.0)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().ok_or("usage: vision_bench <model dir> [image|text|both]")?);
    let towers = match args.next().as_deref() {
        Some("image") => Towers { image: true, text: false },
        Some("text") => Towers { image: false, text: true },
        _ => Towers::BOTH,
    };
    let t = Instant::now();
    let model = lightcraft_vision::load(&dir, &builtin_siglip2_b16_256(), towers)?;
    println!("load ({towers:?}): {:?}, peak RSS {:.0} MB", t.elapsed(), peak_rss_mb().unwrap_or(0.0));
    if towers.image {
        // a photo-sized input, so the resize is part of the time
        let img = Rgb8::from_fn(1024, 683, |x, y| [(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8]);
        model.embed_images(std::slice::from_ref(&img))?; // warm-up
        for batch in [1usize, 8, 32] {
            let imgs = vec![img.clone(); batch];
            let runs = if batch == 1 { 5 } else { 2 };
            let t = Instant::now();
            for _ in 0..runs {
                model.embed_images(&imgs)?;
            }
            let ms = t.elapsed().as_secs_f64() * 1000.0 / (runs * batch) as f64;
            println!("images, batch {batch:>2}: {ms:.1} ms/image, peak RSS {:.0} MB", peak_rss_mb().unwrap_or(0.0));
        }
    }
    if towers.text {
        model.embed_texts(&["warm up"])?;
        let texts = ["a beach", "kids on a beach at sunset", "a red car", "a scorpion", "une plage", "海滩", "a dog", "mountains in fog"];
        let t = Instant::now();
        for s in texts {
            model.embed_texts(&[s])?;
        }
        println!("texts, one at a time: {:.1} ms/text", t.elapsed().as_secs_f64() * 1000.0 / texts.len() as f64);
        let t = Instant::now();
        model.embed_texts(&texts)?;
        println!(
            "texts, batch of {}: {:.1} ms/text, peak RSS {:.0} MB",
            texts.len(),
            t.elapsed().as_secs_f64() * 1000.0 / texts.len() as f64,
            peak_rss_mb().unwrap_or(0.0)
        );
    }
    Ok(())
}
