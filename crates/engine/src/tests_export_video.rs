//! M14.8: video clips through `lcc_engine::export` (synthetic clips generated in-test).

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use filmcraft_isobmff::{AvcConfig, Brand, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use lcc_catalog::{MediaKind, Op, Photo, PhotoId, Source, VideoMeta};
use serde_json::json;

use crate::Session;
use crate::activity::Cancel;
use crate::export::{Destination, ExportOptions, prepare_batch, run_batch, set_video_hooks};
use crate::export_video::VideoHooks;

const W: u32 = 64;
const H: u32 = 48;
const FPS: u32 = 25;
const FRAMES: u32 = 50;
/// Luma of the synthetic clip: sRGB ≈ 0.38, linear ≈ 0.12 (room to double without clipping).
const LUMA: u8 = 100;

/// The test's folder in Temp, removed when the test ends.
fn scratch(name: &str) -> lcc_testkit::TempDir {
    lcc_testkit::TempDir::new(&format!("lcc-engine-video-{name}"))
}

/// A 2 s, 25 fps, 64×48 flat grey H.264 clip.
fn make_clip(path: &Path) {
    let mut cfg = filmcraft_h264enc::EncoderConfig::new(W, H, FPS, 1);
    cfg.format = filmcraft_h264enc::PacketFormat::LengthPrefixed;
    cfg.aud = false;
    cfg.keyint = 10;
    let mut enc = filmcraft_h264enc::Encoder::new(cfg).unwrap();
    let avc = AvcConfig::parse(&enc.avcc()).unwrap();
    let mut mux = Mp4Writer::new(BufWriter::new(File::create(path).unwrap()), WriterOptions::new(Brand::Mp4)).unwrap();
    let mut vt = TrackConfig::new(SampleEntry::avc(avc, W as u16, H as u16), FPS);
    if enc.delay() > 0 {
        vt.media_start = Some(1);
    }
    let vt = mux.add_track(vt).unwrap();
    let y = vec![LUMA; (W * H) as usize];
    let c = vec![128u8; (W * H / 4) as usize];
    let write = |mux: &mut Mp4Writer<_>, ps: Vec<filmcraft_h264enc::Packet>| {
        for p in ps {
            mux.write_sample(vt, WriteSample { data: &p.data, duration: 1, composition_offset: (p.pts - p.dts) as i32, is_sync: p.keyframe })
                .unwrap();
        }
    };
    for i in 0..FRAMES {
        let ps = enc
            .encode(&filmcraft_h264enc::YuvFrame { y: &y, u: &c, v: &c, y_stride: W as usize, uv_stride: (W / 2) as usize }, i64::from(i))
            .unwrap();
        write(&mut mux, ps);
    }
    let ps = enc.flush();
    write(&mut mux, ps);
    mux.finish().unwrap();
}

fn add_clip(s: &mut Session, path: &Path, trim: (f64, Option<f64>)) -> PhotoId {
    let id = s.catalog.alloc_photo_id();
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    let mut p = Photo::new(id, Source::File { path: path.to_string_lossy().to_string() }, &name, "mp4", W, H, "2026-10-12T00:00:00");
    p.kind = MediaKind::Video;
    p.video = Some(VideoMeta {
        duration: 2.0,
        fps_num: FPS,
        fps_den: 1,
        width: W,
        height: H,
        codec: "h264".into(),
        has_audio: false,
        trim_in: trim.0,
        trim_out: trim.1,
        poster_time: None,
    });
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

fn mean_luma(path: &Path, t: f64) -> f32 {
    let f = lcc_video::FrameReader::open(path).unwrap().frame_at(t, None).unwrap();
    f.image.data.iter().map(|p| (p[0] + p[1] + p[2]) / 3.0).sum::<f32>() / f.image.data.len() as f32
}

fn export(s: &mut Session, ids: &[PhotoId], o: &ExportOptions, dir: &Path, hooks: &VideoHooks) -> Result<Vec<serde_json::Value>, String> {
    let mut items = prepare_batch(s, ids, o)?;
    set_video_hooks(&mut items, hooks);
    let to = Destination { dir: dir.to_string_lossy().to_string(), exact: None };
    run_batch(items, o, &to, &mut crate::export::write_file, &|p| Path::new(p).exists(), false, &mut |_, _| true)
}

#[test]
fn clip_exports_trimmed_graded_and_cropped() {
    let dir = scratch("graded");
    let src = dir.join("clip.mp4");
    make_clip(&src);
    let mut s = Session::new();
    let id = add_clip(&mut s, &src, (0.4, Some(1.4)));
    let out = dir.join("out");
    // the default grade first (identity), then +1 EV
    let o = ExportOptions::from_params(&json!({"videoQuality": 95})).unwrap();
    let files = export(&mut s, &[id], &o, &out.join("plain"), &VideoHooks::default()).unwrap();
    let plain = PathBuf::from(files[0]["path"].as_str().unwrap());
    assert_eq!(plain.extension().unwrap(), "mp4", "{files:?}");
    assert_eq!(files[0]["video"], true);
    let mut d = (*s.develop_of(id).unwrap()).clone();
    d.light.exposure = 1.0;
    s.catalog.apply(Op::SetDevelop { id, settings: std::sync::Arc::new(d), label: "Exposure".into(), edited: None }).unwrap();
    let files = export(&mut s, &[id], &o, &out.join("bright"), &VideoHooks::default()).unwrap();
    let bright = PathBuf::from(files[0]["path"].as_str().unwrap());

    let info = lcc_video::probe(&bright).unwrap();
    assert!((info.duration - 1.0).abs() < 0.05, "trimmed to 1 s: {}", info.duration);
    assert_eq!(lcc_video::FrameReader::open(&bright).unwrap().frame_times().len(), 25);
    let (src_y, plain_y, bright_y) = (mean_luma(&src, 0.5), mean_luma(&plain, 0.5), mean_luma(&bright, 0.5));
    assert!((plain_y / src_y - 1.0).abs() < 0.05, "a default grade keeps the clip: {src_y} → {plain_y}");
    assert!((bright_y / plain_y - 2.0).abs() < 0.12, "+1 EV doubles linear light: {plain_y} → {bright_y}");

    // the export carries the clip's crop together with its trim and grade: the left half, full height (32×48 of 64×48)
    let mut d = (*s.develop_of(id).unwrap()).clone();
    d.crop.geometry.rect = lcc_geom::Rect { x0: 0.0, y0: 0.0, x1: 0.5, y1: 1.0 };
    s.catalog.apply(Op::SetDevelop { id, settings: std::sync::Arc::new(d), label: "Crop".into(), edited: None }).unwrap();
    let files = export(&mut s, &[id], &o, &out.join("cropped"), &VideoHooks::default()).unwrap();
    let cropped = PathBuf::from(files[0]["path"].as_str().unwrap());
    let info = lcc_video::probe(&cropped).unwrap();
    assert_eq!((info.width, info.height), (W / 2, H), "cropped");
    assert!((info.duration - 1.0).abs() < 0.05, "cropped and trimmed to 1 s: {}", info.duration);
    let cropped_y = mean_luma(&cropped, 0.5);
    assert!((cropped_y / src_y - 2.0).abs() < 0.15, "cropped and +1 EV graded: {src_y} → {cropped_y}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The full-quality linear grade (no 8-bit step): +1 EV doubles a mid-tone exactly.
#[test]
fn linear_grade_doubles_on_plus_one_ev() {
    let mut f = lcc_raster::Rgb32f::from_fn(32, 24, |_, _| [0.12, 0.12, 0.12]);
    // the default grade is the identity (export skips the pipeline for it)
    let mut same = f.clone();
    crate::export_video::grade_linear(&mut same, &lcc_develop::DevelopSettings::default()).unwrap();
    assert!(same.data.iter().all(|p| (p[1] - 0.12).abs() < 0.004), "{:?}", same.data[0]);
    let mut d = lcc_develop::DevelopSettings::default();
    d.light.exposure = 1.0;
    crate::export_video::grade_linear(&mut f, &d).unwrap();
    let m = f.data.iter().map(|p| p[1]).sum::<f32>() / f.data.len() as f32;
    assert!((m / 0.12 - 2.0).abs() < 0.06, "{m}");
}

#[test]
fn mixed_batch_and_prores() {
    let dir = scratch("mixed");
    let src = dir.join("clip.mp4");
    make_clip(&src);
    let mut s = Session::with_demo();
    let photo = s.visible()[0];
    let clip = add_clip(&mut s, &src, (0.0, None));
    let o = ExportOptions::from_params(&json!({"format": "jpeg", "longEdge": 64, "videoFormat": "prores", "videoMaxSide": 32})).unwrap();
    let files = export(&mut s, &[photo, clip], &o, &dir.join("out"), &VideoHooks::default()).unwrap();
    assert_eq!(files.len(), 2, "{files:?}");
    assert!(files[0]["path"].as_str().unwrap().ends_with(".jpg"), "{files:?}");
    let mov = files[1]["path"].as_str().unwrap();
    assert!(mov.ends_with("clip.mov"), "{files:?}");
    assert_eq!((files[1]["width"].as_u64(), files[1]["height"].as_u64()), (Some(32), Some(24)));
    assert!(std::fs::metadata(mov).unwrap().len() > 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancel_through_the_activity_row() {
    let dir = scratch("cancel");
    let src = dir.join("clip.mp4");
    make_clip(&src);
    let mut s = Session::new();
    let id = add_clip(&mut s, &src, (0.0, None));
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let guard = s.activity.start("export", "Exporting", Cancel::Flag(cancel.clone()));
    let (activity, row) = (s.activity.clone(), guard.id());
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let hooks = VideoHooks {
        cancel,
        progress: Some(std::sync::Arc::new(move |name: &str, x: f64| {
            seen2.lock().unwrap().push((name.to_string(), x));
            if x > 0.2 {
                activity.cancel(row).unwrap();
            }
        })),
    };
    let out = dir.join("out");
    let files = export(&mut s, &[id], &ExportOptions::default(), &out, &hooks).unwrap();
    assert!(files[0]["error"].as_str().unwrap().contains("cancelled"), "{files:?}");
    assert!(seen.lock().unwrap().iter().all(|(n, _)| n == "clip.mp4"));
    let left: Vec<_> = std::fs::read_dir(&out).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    assert!(left.is_empty(), "no partial movie left: {left:?}");
    drop(guard);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hostile_video_options() {
    for bad in [
        json!({"videoFormat": "av1"}),
        json!({"videoFormat": 3}),
        json!({"videoQuality": 0}),
        json!({"videoQuality": 101}),
        json!({"videoQuality": 50.5}),
        json!({"videoMaxSide": 8}),
        json!({"videoMaxSide": 100000}),
        json!({"videoMaxSide": -1}),
        json!({"videoMaxSide": "big"}),
        json!({"videoFormats": "h264"}),
    ] {
        assert!(ExportOptions::from_params(&bad).is_err(), "{bad}");
    }
    let o = ExportOptions::from_params(&json!({"videoFormat": "ProRes", "videoQuality": 60, "videoMaxSide": 1280})).unwrap();
    assert_eq!(ExportOptions::from_json(&o.to_json()), o, "to_json round trip");
    assert_eq!(o.to_json()["videoFormat"], "prores");
    let full = ExportOptions::from_params(&json!({"videoMaxSide": 0})).unwrap();
    assert_eq!(full.video_max_side, None);
    // a clip whose file is missing or garbage is that clip's error, not a crash
    let dir = scratch("hostile");
    let mut s = Session::new();
    let gone = add_clip(&mut s, &dir.join("gone.mp4"), (0.0, None));
    std::fs::write(dir.join("junk.mp4"), b"not a movie at all").unwrap();
    let junk = add_clip(&mut s, &dir.join("junk.mp4"), (0.0, None));
    let files = export(&mut s, &[gone, junk], &ExportOptions::default(), &dir.join("out"), &VideoHooks::default()).unwrap();
    assert!(files.iter().all(|f| f["error"].is_string()), "{files:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The GPU grade of an exported (H.264) frame matches the CPU float grade within 8-bit rounding: the export grade
/// (GPU when it can) with a LUT profile, with a feathered mask in the edit (a video grade leaves masks out), and the
/// GPU grade called directly.
#[test]
fn gpu_grade_matches_cpu_grade() {
    if !lcc_gpu::available() {
        lcc_testkit::skip!(Gpu, "no GPU adapter ({})", lcc_gpu::unavailable_reason().unwrap_or_default());
        return;
    }
    // a clip with a LUT profile exports with the LUT applied
    let n = 5;
    let mut cube = format!("LUT_3D_SIZE {n}\n");
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let f = |i: usize| i as f32 / (n - 1) as f32;
                // channel swap: far from the identity, so a skipped LUT shows
                cube.push_str(&format!("{} {} {}\n", f(b), f(g), 1.0 - f(r)));
            }
        }
    }
    lcc_pipeline::lut::register("lut:video-gpu-parity", lcc_pipeline::lut::Lut3d::parse_cube(&cube).unwrap());
    let mut d = lcc_develop::DevelopSettings::default();
    d.profile.id = "lut:video-gpu-parity".into();
    d.profile.amount = 100.0;
    assert_export_grade_matches_cpu(&lcc_pipeline::grade::video_settings(&d));

    // a clip whose edit has a feathered mask
    {
        use lcc_develop::{Mask, MaskComponent, MaskOp, MaskShape};
        let mut d = lcc_develop::DevelopSettings::default();
        d.masks.push(Mask {
            visible: true,
            feather: 60.0,
            components: vec![MaskComponent {
                anchor: None,
                name: None,
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::Radial { center: lcc_geom::Point::new(0.5, 0.5), rx: 0.3, ry: 0.3, angle: 0.0, feather: 20.0, invert: false },
            }],
            adjust: lcc_develop::LocalAdjustments { exposure: 1.0, ..Default::default() },
            ..Default::default()
        });
        assert_export_grade_matches_cpu(&lcc_pipeline::grade::video_settings(&d));
    }

    // the GPU grade itself
    let mut d = lcc_develop::DevelopSettings::default();
    d.light.exposure = 0.5;
    d.light.contrast = 20.0;
    d.color.vibrance = 15.0;
    let vs = lcc_pipeline::grade::video_settings(&d);
    let (mut cpu, mut gpu) = (graded_frame(), graded_frame());
    crate::export_video::grade_linear(&mut cpu, &vs).unwrap();
    if crate::export_video::grade_gpu(&mut gpu, &vs).is_none() {
        lcc_testkit::skip!(Gpu, "the GPU did not render");
        return;
    }
    let enc = |v: f32| lcc_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0;
    let mut worst = 0f32;
    for (a, b) in cpu.data.iter().zip(&gpu.data) {
        for k in 0..3 {
            worst = worst.max((enc(a[k]) - enc(b[k])).abs());
        }
    }
    assert!(worst <= 3.0, "GPU vs CPU differ by {worst} 8-bit codes");
}

fn graded_frame() -> lcc_raster::Rgb32f {
    lcc_raster::Rgb32f::from_fn(96, 64, |x, y| {
        let (u, v) = (x as f32 / 95.0, y as f32 / 63.0);
        [u * u, v * 0.8, (1.0 - u) * 0.5 * v]
    })
}

/// The export grade (GPU when it can) matches the CPU float grade for `s` within 8-bit rounding.
fn assert_export_grade_matches_cpu(s: &lcc_develop::DevelopSettings) {
    let (mut cpu, mut out) = (graded_frame(), graded_frame());
    crate::export_video::grade_linear(&mut cpu, s).unwrap();
    crate::export_video::grade_export(&mut out, s, true).unwrap();
    let enc = |v: f32| lcc_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0;
    let mut worst = 0f32;
    for (a, b) in cpu.data.iter().zip(&out.data) {
        for k in 0..3 {
            worst = worst.max((enc(a[k]) - enc(b[k])).abs());
        }
    }
    assert!(worst <= 1.0, "export grade vs CPU differ by {worst} 8-bit codes");
}
