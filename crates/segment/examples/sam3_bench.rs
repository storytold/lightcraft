//! Times SAM 3 on this machine (a synthetic gradient image; the first run includes compiling
//! the GPU kernels and loading the heads): `cargo run -p lightcraft-segment --release --example
//! sam3_bench -- <model dir>`. Each run times the backbone, the click features (tracker neck),
//! a first and a further click, and a first and a second description of the same image (the
//! first one computes the detector's features).

use std::time::{Duration, Instant};

fn ms(d: Duration) -> String {
    format!("{:.0} ms", d.as_secs_f64() * 1e3)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().ok_or("usage: sam3_bench <model dir> [runs]")?);
    let runs: usize = args.next().map(|s| s.parse()).transpose()?.unwrap_or(3);
    let t = Instant::now();
    let mut model = lightcraft_segment::Sam3::load(&dir)?;
    eprintln!("load: {} on {}", ms(t.elapsed()), model.backbone_device());
    let (w, h) = (1200, 800);
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| ((i / 3 % w) * 255 / w) as u8).collect();
    let click = |x, y| lightcraft_segment::Click { x, y, positive: true };
    for run in 0..runs {
        let t = Instant::now();
        let mut enc = model.encode(&rgb, w, h)?;
        lightcraft_segment::sync(&model)?;
        let encode = t.elapsed();
        let t = Instant::now();
        model.prepare_clicks(&mut enc)?;
        lightcraft_segment::sync(&model)?;
        let features = t.elapsed();
        let t = Instant::now();
        let p = model.segment_clicks(&mut enc, &[click(0.5, 0.5)])?;
        let click1 = t.elapsed();
        let t = Instant::now();
        model.segment_clicks(&mut enc, &[click(0.5, 0.5), click(0.3, 0.6)])?;
        let click2 = t.elapsed();
        let t = Instant::now();
        let d = model.segment_text(&mut enc, "sky", 0.5)?;
        let text1 = t.elapsed();
        let t = Instant::now();
        model.segment_text(&mut enc, "the red car", 0.5)?;
        let text2 = t.elapsed();
        eprintln!(
            "run {run}: encode {}, click features {}, click {} (score {:.2}), further click {}, first describe {} ({}), second describe {}",
            ms(encode),
            ms(features),
            ms(click1),
            p.score,
            ms(click2),
            ms(text1),
            d.is_some(),
            ms(text2)
        );
    }
    Ok(())
}
