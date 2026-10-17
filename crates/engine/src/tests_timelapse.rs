//! Upstream #497: `export.timelapse` from synthetic photos, re-decoded with `lcc-video`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use lcc_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{Selection, Session};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lcc-engine-timelapse-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A flat grey PNG photo (`w × h`, sRGB code value `v`) in the catalog.
fn add_photo(s: &mut Session, dir: &Path, name: &str, w: u32, h: u32, v: u8) -> PhotoId {
    let img = lcc_raster::Rgba8::filled(w as usize, h as usize, [v, v, v, 255]);
    let bytes = lcc_codecs::encode::encode_png(&lcc_codecs::encode::EncodeImage::rgba8(&img), &Default::default()).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    let id = s.catalog.alloc_photo_id();
    let p = Photo::new(id, Source::File { path: path.to_string_lossy().to_string() }, name, "png", w, h, "2026-10-12T00:00:00");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

fn set_exposure(s: &mut Session, id: PhotoId, ev: f64) {
    let mut d = (*s.develop_of(id).unwrap()).clone();
    d.light.exposure = ev;
    s.catalog.apply(Op::SetDevelop { id, settings: std::sync::Arc::new(d), label: "Exposure".into(), edited: None }).unwrap();
}

/// Six photos, same grey, exposures −1.5 … +1 EV.
fn six(s: &mut Session, dir: &Path) -> Vec<PhotoId> {
    (0..6)
        .map(|i| {
            let id = add_photo(s, dir, &format!("p{i}.png"), 64, 48, 90);
            set_exposure(s, id, -1.5 + 0.5 * f64::from(i));
            id
        })
        .collect()
}

fn luma(r: &mut lcc_video::FrameReader, t: f64) -> f32 {
    let f = r.frame_at(t, None).unwrap();
    f.image.data.iter().map(|p| (p[0] + p[1] + p[2]) / 3.0).sum::<f32>() / f.image.data.len() as f32
}

fn ids_json(ids: &[PhotoId]) -> serde_json::Value {
    json!(ids.iter().map(|i| i.0).collect::<Vec<_>>())
}

#[test]
fn frames_hold_crossfade_and_brightness_follow_each_photo() {
    let dir = scratch("basic");
    let mut s = Session::new().with_fs();
    let ids = six(&mut s, &dir);
    let out = dir.join("tl.mp4");
    let r = s
        .execute(
            "export.timelapse",
            &json!({"path": out, "ids": ids_json(&ids), "fps": 25, "size": "64x48", "hold": 3, "crossfade": 2, "quality": 95}),
        )
        .unwrap();
    assert_eq!(r["frames"], 6 * 3 + 5 * 2, "{r}");
    assert_eq!((r["width"].as_u64(), r["height"].as_u64(), r["photos"].as_u64()), (Some(64), Some(48), Some(6)));
    assert!(!dir.join("tl.mp4.partial").exists());
    let mut rd = lcc_video::FrameReader::open(&out).unwrap();
    assert_eq!(rd.frame_times().len(), 28);
    // photo i's held frames start at frame i × (hold + crossfade); sample the middle one
    let ys: Vec<f32> = (0..6).map(|i| luma(&mut rd, f64::from(i * 5 + 1) / 25.0)).collect();
    assert!(ys.windows(2).all(|w| w[1] > w[0] * 1.2), "brighter with each +0.5 EV: {ys:?}");
    // a cross-fade frame sits between its neighbours
    let mid = luma(&mut rd, 3.0 / 25.0);
    assert!(mid > ys[0] && mid < ys[1], "{} < {mid} < {}", ys[0], ys[1]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn order_follows_custom_order_and_prores() {
    let dir = scratch("order");
    let mut s = Session::new().with_fs();
    let ids = six(&mut s, &dir);
    let a = s.execute("album.create", &json!({"name": "TL"})).unwrap()["id"].as_u64().unwrap();
    s.selection = Selection::single(ids[0]);
    s.execute("album.addPhotos", &json!({"id": a, "ids": ids_json(&ids)})).unwrap();
    s.execute("library.source", &json!({"kind": "album", "id": a})).unwrap();
    s.execute("library.sort", &json!({"key": "custom"})).unwrap();
    let rev: Vec<PhotoId> = ids.iter().rev().copied().collect();
    s.execute("album.setPhotoOrder", &json!({"album": a, "ids": ids_json(&rev)})).unwrap();
    // selected in creation order; the movie follows the shown (custom) order
    s.selection = Selection::single(ids[0]);
    s.selection.ids = ids.clone();
    let out = dir.join("tl.mov");
    let r = s.execute("export.timelapse", &json!({"path": out, "size": [32, 24], "fps": 24})).unwrap();
    assert_eq!(r["frames"], 6);
    assert_eq!(&std::fs::read(&out).unwrap()[8..12], b"qt  ", "a .mov is ProRes in QuickTime");
    let mut rd = lcc_video::FrameReader::open(&out).unwrap();
    let ys: Vec<f32> = (0..6).map(|i| luma(&mut rd, f64::from(i) / 24.0)).collect();
    assert!(ys.windows(2).all(|w| w[1] < w[0]), "darker each frame (reversed order): {ys:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fit_letterboxes_and_fill_crops() {
    let dir = scratch("fit");
    let mut s = Session::new().with_fs();
    let wide = add_photo(&mut s, &dir, "wide.png", 64, 32, 200);
    for (fit, black) in [("fit", true), ("fill", false)] {
        let out = dir.join(format!("{fit}.mp4"));
        s.execute("export.timelapse", &json!({"path": out, "ids": [wide.0], "size": "32x32", "fit": fit, "quality": 95})).unwrap();
        let f = lcc_video::FrameReader::open(&out).unwrap().frame_at(0.0, None).unwrap().image;
        assert_eq!((f.width, f.height), (32, 32));
        let top = f.data[2 * 32 + 16][1];
        let centre = f.data[16 * 32 + 16][1];
        assert!(centre > 0.4, "{fit}: {centre}");
        assert_eq!(top < 0.05, black, "{fit}: top row {top}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancel_leaves_no_file() {
    let dir = scratch("cancel");
    let mut s = Session::new().with_fs();
    let ids = six(&mut s, &dir);
    let out = dir.join("tl.mp4");
    let p = crate::timelapse::prepare(&mut s, &json!({"path": out, "ids": ids_json(&ids), "size": "32x32"})).unwrap();
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
    let mut s = Session::new().with_fs();
    let id = add_photo(&mut s, &dir, "a.png", 32, 32, 100);
    let out = dir.join("x.mp4").to_string_lossy().to_string();
    for bad in [
        json!({"ids": [id.0]}),
        json!({"path": "", "ids": [id.0]}),
        json!({"path": out, "ids": [id.0], "fps": 0}),
        json!({"path": out, "ids": [id.0], "fps": 1000}),
        json!({"path": out, "ids": [id.0], "fps": 24.5}),
        json!({"path": out, "ids": [id.0], "fps": "24"}),
        json!({"path": out, "ids": [id.0], "size": "8x8"}),
        json!({"path": out, "ids": [id.0], "size": "99999x100"}),
        json!({"path": out, "ids": [id.0], "size": "axb"}),
        json!({"path": out, "ids": [id.0], "size": [-1, 100]}),
        json!({"path": out, "ids": [id.0], "size": 3}),
        json!({"path": out, "ids": [id.0], "fit": "stretch"}),
        json!({"path": out, "ids": [id.0], "hold": 0}),
        json!({"path": out, "ids": [id.0], "crossfade": -1}),
        json!({"path": out, "ids": [id.0], "crossfade": 1_000_000}),
        json!({"path": out, "ids": [id.0], "format": "av1"}),
        json!({"path": out, "ids": [id.0], "quality": 0}),
        json!({"path": out, "ids": [id.0], "quality": 101}),
        json!({"path": out, "ids": [id.0], "bogus": 1}),
        json!({"path": out, "ids": "all"}),
        json!({"path": out, "ids": [-3]}),
        json!({"path": out, "ids": [999_999]}),
        json!({"path": out, "ids": []}),
        json!([1, 2]),
    ] {
        assert!(s.execute("export.timelapse", &bad).is_err(), "{bad}");
    }
    assert!(!Path::new(&out).exists());
    // a photo whose file is gone fails cleanly, with no movie left
    let gone = add_photo(&mut s, &dir, "gone.png", 32, 32, 100);
    std::fs::remove_file(dir.join("gone.png")).unwrap();
    assert!(s.execute("export.timelapse", &json!({"path": out, "ids": [id.0, gone.0], "size": "32x32"})).is_err());
    assert!(!Path::new(&out).exists() && !Path::new(&format!("{out}.partial")).exists());
    let _ = std::fs::remove_dir_all(&dir);
}
