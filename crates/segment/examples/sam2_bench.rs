//! Times SAM 2.1 on this machine (a synthetic gradient image):
//! `cargo run -p lightcraft-segment --release --example sam2_bench -- <model dir>`.

use lightcraft_segment::{Click, Sam2};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).ok_or("usage: sam2_bench <model dir>")?);
    let t = std::time::Instant::now();
    let model = Sam2::load(&dir)?;
    eprintln!("load: {:?} on {:?}", t.elapsed(), model.device());
    let (w, h) = (1600, 1067);
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| (((i / 3 % w) * 255 / w) ^ ((i / 3 / w) * 255 / h)) as u8).collect();
    let mut enc_ms = Vec::new();
    let mut dec_ms = Vec::new();
    for _ in 0..5 {
        let t = std::time::Instant::now();
        let enc = model.encode(&rgb, w, h)?;
        enc_ms.push(t.elapsed().as_millis());
        for clicks in
            [&[Click { x: 0.5, y: 0.5, positive: true }][..], &[Click { x: 0.5, y: 0.5, positive: true }, Click { x: 0.2, y: 0.8, positive: false }]]
        {
            let t = std::time::Instant::now();
            let _ = model.segment_clicks(&enc, clicks)?;
            dec_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
    }
    let enc = model.encode(&rgb, w, h)?;
    let mut best = f64::MAX;
    for _ in 0..30 {
        let t = std::time::Instant::now();
        let _ = model.segment_clicks(&enc, &[Click { x: 0.5, y: 0.5, positive: true }])?;
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
    }
    eprintln!("decode best of 30: {best:.1} ms");
    eprintln!("encode ms {enc_ms:?}");
    eprintln!("decode ms {:?}", dec_ms.iter().map(|d| (d * 10.0).round() / 10.0).collect::<Vec<_>>());
    Ok(())
}
