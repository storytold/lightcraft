//! The flat-field panel's placement reads: the header only, never the whole raw, and in the app off the UI thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lcc_catalog::PhotoId;
use lcc_color::Mat3;
use lcc_geom::Orientation;
use lcc_meta::Metadata;
use lcc_raw::{BlackLevel, Cfa, ColorData, DngWriteOptions, OpcodeLists, RawData, RawFormat, RawImage, Rect};

use super::{Lookup, Placements, read};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lcc-ffplace-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A Bayer DNG of `w` × `h` 16-bit samples (about `2·w·h` bytes), with a default crop inside its active area.
fn dng(w: usize, h: usize) -> Vec<u8> {
    let data = (0..w * h).map(|i| (1000 + (i * 7919) % 2000) as u16).collect();
    let raw = RawImage {
        format: RawFormat::Dng,
        width: w,
        height: h,
        cpp: 1,
        data: RawData::U16(data),
        cfa: Some(Cfa::bayer("RGGB").unwrap()),
        bits: 14,
        black: BlackLevel::uniform(64.0),
        white: vec![16383.0],
        active_area: Rect::new(0, 0, w, h),
        crop: Rect::new(8, 6, w - 16, h - 12),
        orientation: Orientation::Rotate90,
        color: ColorData {
            illuminant: [21, 0],
            color_matrix: [Some(Mat3::IDENTITY), None],
            as_shot_neutral: Some([1.0, 1.0, 1.0]),
            ..Default::default()
        },
        wb_multipliers: None,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: OpcodeLists::default(),
        metadata: Metadata { make: Some("Test".into()), model: Some("Flat Cam".into()), ..Default::default() },
        lens_metadata: Default::default(),
    };
    lcc_raw::write_dng(&raw, &DngWriteOptions::default()).unwrap()
}

/// The Optics panel used to read the whole raw (tens of MB from a slow drive) on the UI thread for every photo it
/// showed. Now only the header is read, and it places the picture exactly as the whole file does.
#[test]
fn the_placement_comes_from_the_header_alone() {
    let d = temp_dir("header");
    let bytes = dng(3000, 2000);
    let path = d.join("big.dng");
    std::fs::write(&path, &bytes).unwrap();
    let whole = crate::files::placement_of(&bytes);
    assert!(whole.is_some(), "the fixture places");
    let r = read(path.to_str().unwrap()).unwrap();
    assert_eq!(r.placed, whole, "the header gives what the whole file gives");
    assert_eq!(r.total, bytes.len() as u64);
    assert!(r.read * 20 < r.total, "read {} of {} bytes: more than the header", r.read, r.total);
    // not a raw, and no file: unplaced, nothing read beyond the start
    let text = d.join("notes.dng");
    std::fs::write(&text, vec![b'x'; 3 << 20]).unwrap();
    let r = read(text.to_str().unwrap()).unwrap();
    assert_eq!(r.placed, None);
    assert!(r.read <= super::PREFIXES[1], "{}", r.read);
    assert!(read(d.join("gone.dng").to_str().unwrap()).is_none());
    let _ = std::fs::remove_dir_all(&d);
}

/// In the app the caller never waits for the file: the first ask is pending, the worker reads the header, wakes the
/// UI and moves the generation on, and the answer is then known (without reading again).
#[test]
fn the_app_reads_placements_in_the_background() {
    let d = temp_dir("background");
    let bytes = dng(1200, 800);
    let path = d.join("a.dng");
    std::fs::write(&path, &bytes).unwrap();
    let path = path.to_str().unwrap().to_string();
    let woken = Arc::new(AtomicUsize::new(0));
    let mut p = Placements::default();
    let w = woken.clone();
    p.run_in_background(Arc::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    }));
    let id = PhotoId(7);
    assert_eq!(p.get(id, &path), Lookup::Pending);
    let g0 = p.generation();
    let start = Instant::now();
    while woken.load(Ordering::SeqCst) == 0 {
        assert!(start.elapsed() < Duration::from_secs(20), "the worker never answered");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(p.generation() > g0);
    assert_eq!(p.get(id, &path), Lookup::Known(crate::files::placement_of(&bytes)));
    // the same photo now at another path (another library): asked again, not taken from the old file
    let other = d.join("b.dng");
    std::fs::write(&other, b"not a raw").unwrap();
    assert_eq!(p.get(id, other.to_str().unwrap()), Lookup::Pending);
    let _ = std::fs::remove_dir_all(&d);
}

/// Everywhere else (CLI, MCP, tests) the answer is read at once, as before.
#[test]
fn without_a_worker_the_answer_is_read_at_once() {
    let d = temp_dir("sync");
    let bytes = dng(1200, 800);
    let path = d.join("a.dng");
    std::fs::write(&path, &bytes).unwrap();
    let p = Placements::default();
    assert_eq!(p.get(PhotoId(1), path.to_str().unwrap()), Lookup::Known(crate::files::placement_of(&bytes)));
    assert_eq!(p.get(PhotoId(2), d.join("gone.dng").to_str().unwrap()), Lookup::Known(None));
    let _ = std::fs::remove_dir_all(&d);
}

/// A measurement over real raws (not in CI): `LCC_PLACEMENT_DIRS="dir;dir" cargo test -p lcc-engine --lib
/// placement_of_real_raws -- --ignored --nocapture`. Every raw the whole file places must be placed the same from
/// its header, and the bytes read are printed.
#[test]
#[ignore]
fn placement_of_real_raws() {
    let dirs = std::env::var("LCC_PLACEMENT_DIRS").unwrap_or_default();
    let (mut files, mut same, mut read_total, mut size_total) = (0, 0, 0u64, 0u64);
    for dir in dirs.split(';').filter(|d| !d.is_empty()) {
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        entries.sort();
        for p in entries {
            let Ok(bytes) = std::fs::read(&p) else { continue };
            let Some(whole) = crate::files::placement_of(&bytes) else { continue };
            files += 1;
            let start = Instant::now();
            let r = read(p.to_str().unwrap()).unwrap();
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            let ok = r.placed == Some(whole);
            same += usize::from(ok);
            read_total += r.read;
            size_total += r.total;
            println!(
                "{} {:<44} read {:>9} of {:>10} bytes ({:.2} %) in {ms:.1} ms",
                if ok { "same" } else { "DIFFERS" },
                p.file_name().unwrap().to_string_lossy(),
                r.read,
                r.total,
                r.read as f64 * 100.0 / r.total as f64
            );
            if !ok {
                println!("    whole {whole:?}\n    header {:?}", r.placed);
            }
        }
    }
    println!("{same} of {files} placed the same; read {read_total} of {size_total} bytes");
    assert_eq!(same, files);
}
