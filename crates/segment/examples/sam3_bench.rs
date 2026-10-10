//! Times SAM 3 on this machine (a synthetic gradient image; the first run includes compiling
//! the GPU kernels): `cargo run -p dac-segment --release --example sam3_bench -- <model dir>`.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().ok_or("usage: sam3_bench <model dir>")?);
    let t = std::time::Instant::now();
    let mut model = dac_segment::Sam3::load(&dir)?;
    eprintln!("load: {:?} on {:?}", t.elapsed(), model.device());
    let (w, h) = (1200, 800);
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| ((i / 3 % w) * 255 / w) as u8).collect();
    for run in 0..3 {
        let t = std::time::Instant::now();
        let mut enc = model.encode(&rgb, w, h)?;
        dac_segment::sync(&model)?;
        let e = t.elapsed();
        let t = std::time::Instant::now();
        let p = model.segment_clicks(&mut enc, &[dac_segment::Click { x: 0.5, y: 0.5, positive: true }])?;
        let c = t.elapsed();
        let t = std::time::Instant::now();
        let d = model.segment_text(&mut enc, "sky", 0.5)?;
        eprintln!("run {run}: encode {e:?}, click {c:?} (score {:.2}), text {:?} ({})", p.score, t.elapsed(), d.is_some());
    }
    Ok(())
}
