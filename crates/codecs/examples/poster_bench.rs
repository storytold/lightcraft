//! Time poster frames of real clips and write them as PNGs (feature `video-frames`):
//! `cargo run --release -p lightcraft-codecs --features video-frames --example poster_bench -- [--threads N] [--edge PX] [--out DIR] clip...`
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use lightcraft_codecs::poster::{PosterOptions, poster_frame};
use lightcraft_codecs::video_stream::FileSource;
use lightcraft_codecs::{EncodeImage, EncodeMeta, Samples, encode_png};

fn main() {
    let mut opts = PosterOptions::default();
    let mut out = None;
    let mut clips = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--threads" => opts.stream.threads = args.next().and_then(|v| v.parse().ok()).unwrap_or(1),
            "--edge" => opts.max_edge = args.next().and_then(|v| v.parse().ok()).unwrap_or(512),
            "--out" => out = args.next(),
            _ => clips.push(a),
        }
    }
    let m = lightcraft_color::REC2020.to_space(&lightcraft_color::SRGB);
    for clip in clips {
        let name = std::path::Path::new(&clip).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if let Err(e) = FileSource::open(&clip) {
            println!("{name}: {e}");
            continue;
        }
        // best of three (the first warms the file cache)
        let mut best = f64::MAX;
        let mut last = None;
        for _ in 0..3 {
            let Ok(src) = FileSource::open(&clip) else { break };
            let t = Instant::now();
            let r = poster_frame(src, &opts, &AtomicBool::new(false));
            best = best.min(t.elapsed().as_secs_f64() * 1e3);
            last = Some(r);
        }
        match last {
            Some(Ok(p)) => {
                println!(
                    "{name}: {:.1} ms, {:?} {}-bit {}x{} at {:.3} s, {} pictures, {:.1} MB read{}{}",
                    best,
                    p.codec,
                    p.bit_depth,
                    p.image.width,
                    p.image.height,
                    p.seconds,
                    p.frames_decoded,
                    p.bytes_read as f64 / 1e6,
                    if p.hdr { ", HDR tone mapped" } else { "" },
                    if p.fallback { ", fallback" } else { "" },
                );
                if let Some(dir) = &out {
                    let px: Vec<u8> = p
                        .image
                        .data
                        .iter()
                        .flat_map(|v| {
                            let s = m.apply_f32(*v);
                            s.map(lightcraft_color::transfer::encode_srgb8)
                        })
                        .collect();
                    let img = EncodeImage::new(p.image.width as u32, p.image.height as u32, 3, Samples::U8(&px));
                    match encode_png(&img, &EncodeMeta::default()) {
                        Ok(png) => {
                            let _ = std::fs::write(std::path::Path::new(dir).join(format!("{name}.png")), png);
                        }
                        Err(e) => println!("  png: {e}"),
                    }
                }
            }
            Some(Err(e)) => println!("{name}: {best:.1} ms, {e}"),
            None => {}
        }
    }
}
