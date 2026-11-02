//! Procedural test photos at any size (no camera files): `N` JPEGs of the demo scenes.
//! `cargo run --release -p lightcraft-engine --example scene_jpegs -- OUT_DIR N [WIDTHxHEIGHT]`
//! (default 9504x6336, 60 MP, as a 61 MP Sony body).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lightcraft_codecs::encode::{ChromaSubsampling, EncodeImage, EncodeMeta, encode_jpeg};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = std::path::PathBuf::from(&args[0]);
    let n: usize = args[1].parse().unwrap();
    let (w, h) = args
        .get(2)
        .map_or("9504x6336", String::as_str)
        .split_once('x')
        .map(|(a, b)| (a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap()))
        .unwrap();
    std::fs::create_dir_all(&out).unwrap();
    let scenes = lightcraft_scenes::demo_library();
    for i in 0..n {
        let path = out.join(format!("scene-{:02}.jpg", i + 1));
        if path.exists() {
            continue;
        }
        let s = &scenes[i % scenes.len()];
        // a different seed per copy, so no two files are the same picture
        let img = lightcraft_scenes::Scene { seed: s.seed.wrapping_add(i as u32 * 7919), ..s.clone() }.render(w, h).to_srgb8();
        let bytes = encode_jpeg(&EncodeImage::rgba8(&img), 92, ChromaSubsampling::S420, &EncodeMeta::default()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        eprintln!("{} ({} MB)", path.display(), std::fs::metadata(&path).unwrap().len() >> 20);
    }
}
