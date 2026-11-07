//! Procedural window benchmark for #1146. Run with `cargo run --release -p
//! lightcraft-gpu --example window_context`; N controls samples (default 5).
use std::{sync::Arc, time::Instant};

use lightcraft_develop::{DevelopSettings, Process};
use lightcraft_pipeline::{PixelWindow, RenderRequest, SourceInfo, StageCache};
use lightcraft_raster::Rgb32f;

fn blocks(w: usize, h: usize) -> Arc<Rgb32f> {
    Arc::new(Rgb32f::from_fn(w, h, |x, y| {
        let cell = |size: usize, k: usize| ((x / size * 7 + y / size * 13 + k) % 5) as f32 / 4.0;
        let l = 0.02 + 0.5 * cell(97, 1) + 0.25 * cell(31, 2) + 0.1 * cell(11, 3);
        [l * 1.1, l, l * 0.8]
    }))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let n = std::env::var("N").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(5).max(1);
    let info = SourceInfo::default();
    if std::env::var_os("FULL_PIXELS").is_some() {
        let src = blocks(600, 400);
        for process in [Process::V1, Process::V2] {
            for (name, edit) in [
                ("dehaze", [100., 0., 0., 0.]),
                ("clarity", [0., 100., 0., 0.]),
                ("texture", [0., 0., 100., 0.]),
                ("tone", [0., 0., 0., -100.]),
                ("combined", [80., 80., 60., -80.]),
            ] {
                let mut s = DevelopSettings { process: process.version(), ..Default::default() };
                s.effects.dehaze = edit[0];
                s.effects.clarity = edit[1];
                s.effects.texture = edit[2];
                s.light.highlights = edit[3];
                s.light.shadows = -edit[3];
                let r = lightcraft_pipeline::render(&src, &info, &s, &RenderRequest::fit(2400, 1600)).unwrap();
                let bytes: Vec<u8> = r.image.data.iter().flatten().copied().collect();
                let path = format!("{}/{process:?}-{name}.rgba", std::env::var("FULL_PIXELS").unwrap_or_default());
                std::fs::write(path, bytes)?;
            }
        }
        return Ok(());
    }
    let has_gpu = lightcraft_gpu::available();
    for (fw, fh) in [(6000, 4000), (9504, 6336)] {
        let src = blocks(fw, fh);
        for zoom in [1, 2] {
            // 2800 by 1700 physical-pixel canvas; the current loupe uses native
            // pixels at both zooms. Mirror region::window_for's snap and margin.
            let (vw, vh) = (2800 / zoom, 1700 / zoom);
            let margin = ((fw as f64 * 0.045).min(768.) as usize).div_ceil(256).max(1) * 256;
            let (x, y) = (((fw - vw) / 2).saturating_sub(margin) / 256 * 256, ((fh - vh) / 2).saturating_sub(margin) / 256 * 256);
            let (end_x, end_y) = (((fw + vw) / 2 + margin).div_ceil(256) * 256, ((fh + vh) / 2 + margin).div_ceil(256) * 256);
            let win = PixelWindow { x, y, w: end_x.min(fw) - x, h: end_y.min(fh) - y };
            for combined in [false, true] {
                let mut s = DevelopSettings::default();
                s.effects.dehaze = 100.;
                if combined {
                    s.effects.clarity = 80.;
                    s.effects.texture = 60.;
                    s.light.highlights = -80.;
                    s.light.shadows = 80.;
                }
                let req = RenderRequest { window: Some(win), ..RenderRequest::fit(fw, fh) };
                for gpu in [false, true] {
                    if gpu && !has_gpu {
                        continue;
                    }
                    let cpu_cache = StageCache::default();
                    let gpu_cache = StageCache::default();
                    let mut times = Vec::new();
                    for i in 0..=n {
                        // First render is cold; subsequent Exposure edits reuse the planes.
                        s.light.exposure = i as f64 * 0.01;
                        let start = Instant::now();
                        let r = if gpu {
                            lightcraft_gpu::render(&src, &info, &s, &req, Some(&gpu_cache)).ok_or("hardware GPU render failed")?
                        } else {
                            lightcraft_pipeline::render_cached(&src, &info, &s, &req, &cpu_cache).unwrap()
                        };
                        std::hint::black_box(r);
                        let ms = start.elapsed().as_secs_f64() * 1000.;
                        if i == 0 {
                            println!("{fw}x{fh} zoom={zoom} combined={combined} gpu={gpu} window={}x{} cold_ms={ms:.3}", win.w, win.h);
                        } else {
                            times.push(ms);
                        }
                    }
                    times.sort_by(f64::total_cmp);
                    println!("{fw}x{fh} zoom={zoom} combined={combined} gpu={gpu} warm_median_ms={:.3} samples={times:?}", times[n / 2]);
                }
            }
        }
    }
    Ok(())
}
