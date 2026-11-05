//! Issue #497: `export.timelapse` from synthetic photos, decoded again to check the frames.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use lightcraft_video_encode::probe::{Luma, h264_lumas};
use serde_json::{Value, json};

use crate::Session;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lightcraft-engine-timelapse-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Grey PNGs (`w × h`, sRGB code `v`) imported into a fresh library; their ids in order. The
/// bottom-right pixel carries the file name's first byte, so equal greys are not duplicates.
fn import(s: &mut Session, dir: &Path, files: &[(&str, u32, u32, u8)]) -> Vec<u64> {
    let mut paths = Vec::new();
    for (name, w, h, v) in files {
        let mut img = lightcraft_raster::Rgba8::filled(*w as usize, *h as usize, [*v, *v, *v, 255]);
        let tag = name.bytes().take(2).fold(0u8, |a, b| a.wrapping_add(b));
        *img.data.last_mut().unwrap() = [tag, *v, *v, 255];
        let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        paths.push(path.to_string_lossy().to_string());
    }
    let r = s.execute("library.import", &json!({"paths": paths})).unwrap();
    let ids: Vec<u64> = r["imported"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    assert_eq!(ids.len(), files.len(), "{r}");
    ids
}

fn session(dir: &Path) -> Session {
    let mut s = Session::new().with_fs();
    s.open_library(dir.join("lib"), false).unwrap();
    s
}

/// Six grey photos, exposures −1.5 … +1 EV in import order.
fn six(s: &mut Session, dir: &Path) -> Vec<u64> {
    let files: Vec<(String, u32, u32, u8)> = (0..6).map(|i| (format!("p{i}.png"), 64, 48, 90)).collect();
    let refs: Vec<(&str, u32, u32, u8)> = files.iter().map(|f| (f.0.as_str(), f.1, f.2, f.3)).collect();
    let ids = import(s, dir, &refs);
    for (i, id) in ids.iter().enumerate() {
        s.execute("library.select", &json!({"ids": [id]})).unwrap();
        s.execute("develop.set", &json!({"control": "light.exposure", "value": -1.5 + 0.5 * i as f64})).unwrap();
    }
    ids
}

fn means(out: &Path) -> Vec<f32> {
    h264_lumas(out).unwrap().iter().map(Luma::mean).collect()
}

#[test]
fn frames_hold_crossfade_and_brightness_follow_each_photo() {
    let dir = scratch("basic");
    let mut s = session(&dir);
    let ids = six(&mut s, &dir);
    let out = dir.join("tl.mp4");
    let r = s
        .execute("export.timelapse", &json!({"path": out, "ids": ids, "fps": 25, "size": "64x48", "hold": 3, "crossfade": 2, "quality": 95}))
        .unwrap();
    assert_eq!(r["frames"], 6 * 3 + 5 * 2, "{r}");
    assert_eq!((r["width"].as_u64(), r["height"].as_u64(), r["photos"].as_u64()), (Some(64), Some(48), Some(6)));
    assert!(!dir.join("tl.mp4.partial").exists());
    assert!(s.activity.list().is_empty(), "the row goes when the call returns");
    let ys = means(&out);
    assert_eq!(ys.len(), 28);
    // photo i's held frames start at frame i × (hold + crossfade); sample the middle one
    let held: Vec<f32> = (0..6).map(|i| ys[i * 5 + 1]).collect();
    assert!(held.windows(2).all(|w| w[1] > w[0] + 3.0), "brighter with each +0.5 EV: {held:?}");
    // a cross-fade frame sits between its neighbours
    assert!(ys[3] > held[0] && ys[3] < held[1], "{} < {} < {}", held[0], ys[3], held[1]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn selection_follows_the_shown_order_and_mov_is_prores() {
    let dir = scratch("order");
    let mut s = session(&dir);
    // imported (and selected) darkest first; named so that the file-name sort puts them brightest first
    let ids = import(&mut s, &dir, &[("c.png", 32, 24, 40), ("b.png", 32, 24, 120), ("a.png", 32, 24, 220)]);
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true})).unwrap();
    s.execute("library.select", &json!({"ids": ids})).unwrap();
    let out = dir.join("tl.mp4");
    let r = s.execute("export.timelapse", &json!({"path": out, "size": [32, 24], "fps": 24, "quality": 95})).unwrap();
    assert_eq!(r["frames"], 3);
    let ys = means(&out);
    assert!(ys.windows(2).all(|w| w[1] < w[0]), "a, b, c as shown, not the selection order: {ys:?}");
    let mov = dir.join("tl.mov");
    s.execute("export.timelapse", &json!({"path": mov, "size": [32, 24]})).unwrap();
    assert_eq!(&std::fs::read(&mov).unwrap()[8..12], b"qt  ", "a .mov is ProRes in QuickTime");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fit_letterboxes_and_fill_crops() {
    let dir = scratch("fit");
    let mut s = session(&dir);
    let wide = import(&mut s, &dir, &[("wide.png", 64, 32, 200)])[0];
    for (fit, black) in [("fit", true), ("fill", false)] {
        let out = dir.join(format!("{fit}.mp4"));
        s.execute("export.timelapse", &json!({"path": out, "ids": [wide], "size": "32x32", "fit": fit, "quality": 95})).unwrap();
        let f = &h264_lumas(&out).unwrap()[0];
        assert_eq!((f.width, f.height), (32, 32));
        let (top, centre) = (f.at(16, 2), f.at(16, 16));
        assert!(centre > 150, "{fit}: centre {centre}");
        assert_eq!(top < 30, black, "{fit}: top row {top}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancel_leaves_no_file() {
    let dir = scratch("cancel");
    let mut s = session(&dir);
    let ids = six(&mut s, &dir);
    let out = dir.join("tl.mp4");
    let p = crate::timelapse::prepare(&mut s, &json!({"path": out, "ids": ids, "size": "32x32"})).unwrap();
    assert_eq!(p.frames(), 6);
    let cancel = AtomicBool::new(false);
    let r = p.run(&cancel, &mut |done, _| {
        if done == 3 {
            cancel.store(true, Ordering::Relaxed);
        }
    });
    assert!(r.unwrap_err().contains("cancelled"));
    let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.to_string_lossy().contains("tl.mp4")).collect();
    assert!(left.is_empty(), "{left:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hostile_params_are_errors() {
    let dir = scratch("hostile");
    let mut s = session(&dir);
    let id = import(&mut s, &dir, &[("a.png", 32, 32, 100)])[0];
    let out = dir.join("x.mp4").to_string_lossy().to_string();
    let bad: Vec<Value> = vec![
        json!({"ids": [id]}),
        json!({"path": "", "ids": [id]}),
        json!({"path": out, "ids": [id], "fps": 0}),
        json!({"path": out, "ids": [id], "fps": 1000}),
        json!({"path": out, "ids": [id], "fps": 24.5}),
        json!({"path": out, "ids": [id], "fps": "24"}),
        json!({"path": out, "ids": [id], "size": "8x8"}),
        json!({"path": out, "ids": [id], "size": "99999x100"}),
        json!({"path": out, "ids": [id], "size": "axb"}),
        json!({"path": out, "ids": [id], "size": [-1, 100]}),
        json!({"path": out, "ids": [id], "size": 3}),
        json!({"path": out, "ids": [id], "fit": "stretch"}),
        json!({"path": out, "ids": [id], "hold": 0}),
        json!({"path": out, "ids": [id], "crossfade": -1}),
        json!({"path": out, "ids": [id], "crossfade": 1_000_000}),
        json!({"path": out, "ids": [id], "format": "av1"}),
        json!({"path": out, "ids": [id], "quality": 0}),
        json!({"path": out, "ids": [id], "quality": 101}),
        json!({"path": out, "ids": [id], "bogus": 1}),
        json!({"path": out, "ids": "all"}),
        json!({"path": out, "ids": [-3]}),
        json!({"path": out, "ids": [999_999]}),
        json!({"path": out, "ids": []}),
        json!([1, 2]),
    ];
    for p in bad {
        assert!(s.execute("export.timelapse", &p).is_err(), "{p}");
    }
    assert!(!Path::new(&out).exists());
    // an original is never the output
    let orig = dir.join("a.png").to_string_lossy().to_string();
    assert!(s.execute("export.timelapse", &json!({"path": orig, "ids": [id]})).is_err());
    // a photo whose file is gone fails cleanly, with no movie left
    let gone = import(&mut s, &dir, &[("gone.png", 32, 32, 100)])[0];
    std::fs::remove_file(dir.join("gone.png")).unwrap();
    assert!(s.execute("export.timelapse", &json!({"path": out, "ids": [id, gone], "size": "32x32"})).is_err());
    assert!(!Path::new(&out).exists() && !Path::new(&format!("{out}.partial")).exists());
    let _ = std::fs::remove_dir_all(&dir);
}
