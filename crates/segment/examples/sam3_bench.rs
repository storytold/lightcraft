//! Times SAM 3 on this machine (a synthetic gradient image; the first run includes compiling
//! the GPU kernels): `cargo run -p lightcraft-segment --release --example sam3_bench -- <model dir> [<Neural Engine cache dir>]`.
//! With a cache dir the Neural Engine's backbone is set up in the background and the bench waits for it before its
//! second run: the second photo is the one checked against candle, the third and later use it.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().ok_or("usage: sam3_bench <model dir>")?);
    let t = std::time::Instant::now();
    let mut model = lightcraft_segment::Sam3::load(&dir)?;
    eprintln!("load: {:?} on {:?}", t.elapsed(), model.device());
    let (w, h) = (1200, 800);
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| ((i / 3 % w) * 255 / w) as u8).collect();
    let cache = args.next().map(std::path::PathBuf::from);
    if let Some(cache) = &cache {
        model.start_neural_engine(cache);
    }
    for run in 0..5 {
        if run == 1 && cache.is_some() {
            let t = std::time::Instant::now();
            while model.neural_engine() == lightcraft_segment::ane::Status::Building {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            eprintln!("neural engine: {:?} after waiting {:?}", model.neural_engine(), t.elapsed());
        }
        let t = std::time::Instant::now();
        let mut enc = model.encode(&rgb, w, h)?;
        lightcraft_segment::sync(&model)?;
        let e = t.elapsed();
        let t = std::time::Instant::now();
        let p = model.segment_clicks(&mut enc, &[lightcraft_segment::Click { x: 0.5, y: 0.5, positive: true }])?;
        let c = t.elapsed();
        let t = std::time::Instant::now();
        let d = model.segment_text(&mut enc, "sky", 0.5)?;
        eprintln!(
            "run {run} ({:?}): encode {e:?}, click {c:?} (score {:.2}), text {:?} ({})",
            model.neural_engine(),
            p.score,
            t.elapsed(),
            d.is_some()
        );
    }
    Ok(())
}
