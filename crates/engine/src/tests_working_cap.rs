//! A host's working-resolution cap (`MediaCache::max_working_pixels`, set by phones): originals
//! larger than it are decoded and exported at the largest size within it, and the export says so.

use std::path::Path;

use serde_json::json;

use crate::Session;

fn write_png(path: &Path, w: usize, h: usize) {
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w % 256) as u8, (i / w % 256) as u8, 90, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn session_with(dir: &Path, cap: usize) -> Session {
    let mut s = Session::new().with_fs();
    s.media.max_working_pixels = cap;
    s.execute("library.import", &json!({"paths": [dir.join("big.png").to_string_lossy()]})).unwrap();
    s
}

fn export(s: &mut Session, out: &Path) -> serde_json::Value {
    let id = s.catalog.photos().next().unwrap().id;
    let o = crate::export::ExportOptions::from_json(&json!({"format": "png"}));
    let to = crate::export::Destination { dir: out.to_string_lossy().to_string(), exact: None };
    let files = crate::export::export_batch(s, &[id], &o, &to, &mut crate::export::write_file, &|p| Path::new(p).exists()).unwrap();
    files[0].clone()
}

#[test]
fn a_capped_host_exports_large_originals_smaller_and_says_so() {
    let dir = std::env::temp_dir().join(format!("lc-cap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    write_png(&dir.join("big.png"), 2000, 1500);
    // 1 MP cap: the 3 MP original exports at half size (a power of two: decoders reduce while
    // decoding)
    let mut s = session_with(&dir, 1_000_000);
    let f = export(&mut s, &dir.join("capped"));
    let (w, h) = (f["width"].as_u64().unwrap(), f["height"].as_u64().unwrap());
    assert_eq!((w, h), (1000, 750));
    assert_eq!(f["reducedFrom"], json!([2000, 1500]), "{f}");
    // the decoded original was within the cap too (not 3 MP decoded then shrunk)
    let full = s.memory_report().full_source;
    assert!(full.bytes <= 1_000_000 * 16, "decoded at {} bytes", full.bytes);
    // no cap (the desktop): full size, nothing reported
    let mut s = session_with(&dir, 0);
    let f = export(&mut s, &dir.join("uncapped"));
    assert_eq!((f["width"].as_u64(), f["height"].as_u64()), (Some(2000), Some(1500)));
    assert!(f.get("reducedFrom").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_full_size_export_over_the_cap_is_rendered_in_tiles() {
    let dir = std::env::temp_dir().join(format!("lc-tiled-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    write_png(&dir.join("big.png"), 2000, 1500);
    let decode = |f: &serde_json::Value| {
        let bytes = std::fs::read(f["path"].as_str().unwrap()).unwrap();
        lightcraft_codecs::decode(&bytes, lightcraft_codecs::DecodeOptions::default()).unwrap().to_srgb8()
    };
    let edit = |s: &mut Session| {
        s.execute("develop.set", &json!({"values": {"light.shadows": 40, "effects.clarity": 30, "effects.dehaze": 20}})).unwrap();
    };
    let mut whole = session_with(&dir, 0);
    edit(&mut whole);
    let reference = decode(&export(&mut whole, &dir.join("whole")));
    // 1 MP working cap, but 4 MP tiled: the 3 MP original exports at full size, as the uncapped
    // render does (tiles on the CPU; then on the GPU, when there is one, within its 3 LSB)
    for (gpu_px, tolerance) in [(0, 1), (4_000_000, 3)] {
        let mut s = session_with(&dir, 1_000_000);
        s.media.max_tiled_pixels = 4_000_000;
        s.media.max_gpu_tiled_pixels = gpu_px;
        edit(&mut s);
        let f = export(&mut s, &dir.join("tiled"));
        assert_eq!((f["width"].as_u64(), f["height"].as_u64()), (Some(2000), Some(1500)));
        assert!(f.get("reducedFrom").is_none(), "{f}");
        let tiled = decode(&f);
        let worst = reference.data.iter().zip(&tiled.data).map(|(a, b)| (0..3).map(|k| a[k].abs_diff(b[k])).max().unwrap()).max().unwrap();
        assert!(worst <= tolerance, "tiled export (GPU tiles up to {gpu_px} px) differs by {worst}");
    }
    let mut s = session_with(&dir, 1_000_000);
    s.media.max_tiled_pixels = 4_000_000;
    edit(&mut s);
    export(&mut s, &dir.join("tiled2"));
    // the full-size original isn't kept in memory afterwards
    assert!(s.memory_report().full_source.bytes <= 1_000_000 * 16);
    // edits that can't be tiled (spot removal) still export smaller
    let spot = lightcraft_develop::Spot { points: vec![lightcraft_geom::Point::new(0.5, 0.5)], ..Default::default() };
    s.execute("develop.merge", &json!({"settings": {"spots": [spot]}})).unwrap();
    let f = export(&mut s, &dir.join("spots"));
    assert_eq!(f["reducedFrom"], json!([2000, 1500]), "{f}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn capped_edge_keeps_within_the_pixel_count() {
    use crate::media::capped_edge;
    assert_eq!(capped_edge(8000, 6000, 0), usize::MAX, "no cap");
    assert_eq!(capped_edge(4000, 3000, 12_000_000), usize::MAX, "fits");
    assert_eq!(capped_edge(8000, 6000, 12_000_000), 4000, "half");
    assert_eq!(capped_edge(8000, 6000, 16_000_000), 4000, "half, not 4618: a power of two");
    assert_eq!(capped_edge(8000, 6000, 10_000_000), 2000, "half is 12 MP: a quarter");
    assert_eq!(capped_edge(8000, 6000, 2_000_000), 1000, "an eighth");
    assert_eq!(capped_edge(6000, 8000, 12_000_000), 4000, "portrait: the long edge");
}
