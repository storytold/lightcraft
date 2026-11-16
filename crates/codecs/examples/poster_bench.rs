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
    if std::env::var_os("POSTER_STAGES").is_some() {
        for clip in &clips {
            stages(clip, &opts);
        }
        return;
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

/// Time each stage of one poster: decoding to the target, colour conversion, fitting.
fn stages(clip: &str, opts: &PosterOptions) {
    use lightcraft_codecs::video_stream::{SeekMode, VideoStream};
    use lightcraft_codecs::yuv::Converter;
    let Ok(src) = FileSource::open(clip) else { return };
    let t = Instant::now();
    let Ok(mut s) = VideoStream::open(src, &opts.stream) else { return };
    let start = s.info().start_seconds;
    let _ = s.seek(start + 1.0, SeekMode::Exact);
    let Ok(Some(f)) = s.next_frame() else { return };
    let decode = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let Ok(conv) = Converter::new(f.colour, f.bit_depth, f.width, f.height) else { return };
    let mut lin = vec![[0f32; 3]; f.width * f.height];
    let _ = conv.to_linear_rec2020(&f.picture(), &mut lin);
    let convert = t.elapsed().as_secs_f64() * 1e3;
    let img = lightcraft_raster::Rgb32f { width: f.width, height: f.height, data: lin };
    let t = Instant::now();
    let _ = lightcraft_raster::resample::fit(&img, opts.max_edge, opts.max_edge, lightcraft_raster::resample::Filter::Lanczos3);
    let fit = t.elapsed().as_secs_f64() * 1e3;
    println!(
        "{clip}: decode {decode:.1} ms ({} pictures), convert {convert:.1} ms, fit {fit:.1} ms, colour {:?}",
        s.frames_decoded(),
        conv.resolved()
    );
}
