//! X-Trans demosaic benchmark on real RAF files: timing + JPEG visual comparison.
//!
//! `cargo run --release -p lightcraft-raw --example xtrans_raf_bench -- <raf_dir> [count] [out_dir] [csv_out]`
//!
//! - `raf_dir`:  directory containing `.RAF` files
//! - `count`:    how many files to process (default 4, evenly spaced)
//! - `out_dir`:  directory for output JPEGs (default `raf_bench_out`)
//! - `csv_out`:  optional CSV file path

use lightcraft_codecs::encode::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
use lightcraft_color::transfer::encode_srgb8;
use lightcraft_raw::demosaic::{Options, demosaic_with};
use lightcraft_raw::{Method, Rgb32f};
use std::io::Write;
use std::path::Path;
use std::time::Instant;

struct Variant {
    name: &'static str,
    method: Method,
}

/// X-Trans: `Ppg` is the directional interpolation, `Ahd` the classified LMMSE + residual interpolation.
const VARIANTS: [Variant; 3] = [
    Variant { name: "bilinear", method: Method::Bilinear },
    Variant { name: "directional", method: Method::Ppg },
    Variant { name: "xtrans-ri", method: Method::Ahd },
];

fn save_jpeg(path: &Path, img: &Rgb32f, wb: [f32; 3]) {
    let mut srgb = vec![0u8; img.width * img.height * 3];
    for (i, px) in img.data.iter().enumerate() {
        srgb[i * 3] = encode_srgb8(px[0] * wb[0]);
        srgb[i * 3 + 1] = encode_srgb8(px[1] * wb[1]);
        srgb[i * 3 + 2] = encode_srgb8(px[2] * wb[2]);
    }
    let enc = EncodeImage::new(img.width as u32, img.height as u32, 3, Samples::U8(&srgb));
    let meta = EncodeMeta::default();
    let bytes = encode_jpeg(&enc, 92, ChromaSubsampling::S444, &meta).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let raf_dir = Path::new(args.get(1).map(String::as_str).unwrap_or("."));
    let count: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(4);
    let out_dir = Path::new(args.get(3).map(String::as_str).unwrap_or("raf_bench_out"));
    let csv_path = args.get(4);

    let mut rafs: Vec<_> = std::fs::read_dir(raf_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("raf")))
        .collect();
    rafs.sort();

    if rafs.is_empty() {
        eprintln!("no RAF files found in {}", raf_dir.display());
        return;
    }

    // Pick evenly spaced files (one or none: the first, as spacing divides by `count - 1`).
    let selected: Vec<_> = if rafs.len() <= count {
        rafs.clone()
    } else if count <= 1 {
        rafs.iter().take(count).cloned().collect()
    } else {
        (0..count).map(|i| rafs[i * (rafs.len() - 1) / (count - 1)].clone()).collect()
    };

    eprintln!("selected {} of {} RAF files", selected.len(), rafs.len());
    std::fs::create_dir_all(out_dir).unwrap();

    let mut csv = String::from("file,variant,width,height,megapixels,wb_r,wb_b,ms\n");
    let mut totals = vec![0.0f64; VARIANTS.len()];
    let mut file_count = 0usize;

    for path in &selected {
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        eprintln!("loading {stem}...");

        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("  skip: {e}");
                continue;
            }
        };
        let raw = match lightcraft_raw::decode(&bytes) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("  skip (decode): {e}");
                continue;
            }
        };
        let wb = raw.wb_multipliers.unwrap_or([1.0, 1.0, 1.0]);
        let normalized = match raw.normalized() {
            Ok(n) => n,
            Err(e) => {
                eprintln!("  skip (normalize): {e}");
                continue;
            }
        };

        let mp = (normalized.width * normalized.height) as f64 / 1e6;
        eprintln!("  {}x{} ({mp:.1} MP), WB gains R={:.3} B={:.3}", normalized.width, normalized.height, wb[0], wb[2]);

        file_count += 1;

        for (vi, v) in VARIANTS.iter().enumerate() {
            let opts = Options { balance: Some(wb) };
            let t0 = Instant::now();
            let out = demosaic_with(&normalized, v.method, opts);
            let ms = t0.elapsed().as_secs_f64() * 1e3;

            totals[vi] += ms;

            csv.push_str(&format!("{stem},{},{},{},{mp:.2},{:.3},{:.3},{ms:.0}\n", v.name, normalized.width, normalized.height, wb[0], wb[2]));

            save_jpeg(&out_dir.join(format!("{stem}.{}.jpg", v.name.replace('+', "p"))), &out, wb);
            eprintln!("  {:<12} {ms:>8.0} ms", v.name);
        }
        eprintln!();
    }

    if file_count > 0 {
        println!("\nMean demosaic time over {} files:\n", file_count);
        for (vi, v) in VARIANTS.iter().enumerate() {
            println!("  {:<12} {:>8.0} ms", v.name, totals[vi] / file_count as f64);
        }
        println!();
    }

    if let Some(p) = csv_path {
        std::fs::File::create(p).unwrap().write_all(csv.as_bytes()).unwrap();
        eprintln!("wrote {p}");
    }
}
