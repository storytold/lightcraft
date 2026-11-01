//! The White Balance Selector (test notes 2026-10-25 note 13): a spot with no colour left to read
//! (clipped, near white, near black) is refused with a plain reason and changes nothing; the
//! sample's size is the caller's.

use serde_json::{Value, json};

use crate::Session;

/// A photo of four bands, left to right: pure white, a warm grey, near black, and a warm grey
/// with a white speck every other pixel (more than a quarter of it clipped).
fn bands() -> (std::path::PathBuf, Session) {
    // one folder per call: the tests run in parallel and each clears its own
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let call = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("lc-wbpick-{}-{call}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h) = (256usize, 128usize);
    let data: Vec<[u8; 4]> = (0..w * h)
        .map(|i| match (i % w) / 64 {
            0 => [255, 255, 255, 255],
            1 => [150, 120, 100, 255],
            2 => [1, 1, 1, 255],
            _ if (i % w + i / w) % 2 == 0 => [255, 255, 255, 255],
            _ => [150, 120, 100, 255],
        })
        .collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    let path = dir.join("bands.png");
    std::fs::write(&path, bytes).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    let id = r["imported"][0].as_u64().unwrap_or_else(|| r["imported"][0]["id"].as_u64().unwrap());
    s.execute("library.select", &json!({"ids": [id], "active": id})).unwrap();
    (dir, s)
}

fn wb(s: &Session) -> (f64, f64) {
    let d = s.develop_of(s.active().unwrap()).unwrap();
    (d.wb.temp, d.wb.tint)
}

#[test]
fn clipped_white_and_black_spots_are_refused_and_change_nothing() {
    let (dir, mut s) = bands();
    let before = wb(&s);
    for (x, why) in [(0.12, "bright"), (0.62, "dark"), (0.88, "bright")] {
        let r: Value = s.execute("develop.wbPick", &json!({"x": x, "y": 0.5})).unwrap();
        assert_eq!(r["applied"], false, "{x}: {r}");
        assert_eq!(r["refused"], why, "{x}: {r}");
        assert!(r["message"].as_str().is_some_and(|m| m.contains(if why == "dark" { "too dark" } else { "too bright" })), "{r}");
        assert_eq!(wb(&s), before, "{x}: the white balance is untouched");
    }
    // a neutral (here warm-grey) spot is read: the photo is cooled
    let r = s.execute("develop.wbPick", &json!({"x": 0.37, "y": 0.5})).unwrap();
    assert_eq!(r["applied"], true, "{r}");
    assert_ne!(wb(&s), before, "the grey spot sets a white balance");
    let _ = std::fs::remove_dir_all(dir);
}

/// The sample's size decides what is read: a small one on the grey band beside the white one reads
/// the grey; a large one there takes in the white and is refused.
#[test]
fn the_sample_size_is_the_callers() {
    let (dir, mut s) = bands();
    let small = s.execute("develop.wbPick", &json!({"x": 0.27, "y": 0.5, "size": 0.01})).unwrap();
    assert_eq!(small["applied"], true, "{small}");
    let large = s.execute("develop.wbPick", &json!({"x": 0.27, "y": 0.5, "size": 0.2})).unwrap();
    assert_eq!(large["applied"], false, "{large}");
    for bad in [json!(-1.0), json!("big"), json!(f64::MAX)] {
        // hostile sizes fall back or clamp; never a panic
        let _ = s.execute("develop.wbPick", &json!({"x": 0.37, "y": 0.5, "size": bad}));
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// The brightest part of a photo that isn't clipped is still read (a grey card in a dim room); a
/// scene-referred source's bright, unclipped colour too.
#[test]
fn bright_but_unclipped_spots_are_read() {
    use crate::cmd::develop::{WbSample, wb_sample};
    let sample = |img: &lightcraft_raster::Rgb32f, x, y, size| match wb_sample(img, x, y, size) {
        WbSample::Neutral(_) => Some(true),
        WbSample::TooBright => Some(false),
        WbSample::TooDark => None,
    };
    let dim = lightcraft_raster::Rgb32f::filled(32, 32, [0.3, 0.25, 0.2]);
    assert!(matches!(sample(&dim, 0.5, 0.5, 0.1), Some(true)), "a dim photo's grey is read");
    let mut hdr = lightcraft_raster::Rgb32f::filled(32, 32, [1.0, 0.65, 0.1]);
    for c in hdr.data.iter_mut().take(64) {
        *c = [1.3, 1.7, 1.4];
    }
    assert!(matches!(sample(&hdr, 0.5, 0.9, 0.1), Some(true)), "a value over 1 below the photo's top is read");
    assert!(matches!(sample(&hdr, 0.1, 0.0, 0.05), Some(false)), "the photo's top is clipped");
}

/// The photo's top is read on every pixel (a scene-referred highlight on an odd pixel set it too),
/// and exactly a quarter of the sample clipped is refused.
#[test]
fn the_top_reads_every_pixel_and_a_quarter_clipped_is_refused() {
    use crate::cmd::develop::{WbSample, wb_sample};
    // one bright pixel at an odd index: the top is 2.0, so a 0.95 grey elsewhere is read, not
    // refused as near white
    let mut img = lightcraft_raster::Rgb32f::new(8, 1);
    img.data.fill([0.95, 0.95, 0.95]);
    img.data[1] = [2.0, 2.0, 2.0];
    assert!(matches!(wb_sample(&img, 6.5 / 8.0, 0.5, 1.0 / 8.0), WbSample::Neutral(_)));
    // 4 × 4, a quarter of it white (the photo's top) and the rest dark grey: refused
    let mut img = lightcraft_raster::Rgb32f::new(4, 4);
    img.data.fill([0.2, 0.2, 0.2]);
    for i in 0..4 {
        img.data[i] = [1.0, 1.0, 1.0];
    }
    assert!(matches!(wb_sample(&img, 0.5, 0.5, 1.0), WbSample::TooBright));
}
