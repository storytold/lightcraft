//! The CC0 / CC-BY clips from `cargo xtask corpus --download` (`corpus/video/`, gitignored): probe
//! fields per container and codec, decoded frames checked against the clip's own neighbours and
//! against the same scene decoded by the other codecs (the Big Buck Bunny excerpts are one source
//! encoded four ways), AVCHD 1080i reported as interlaced, and truncations that never panic.
//! Every test skips when its clip is absent.
#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;

use lcc_raster::Rgb32f;
use lcc_video::{FrameReader, VideoError, poster, probe};

fn clip(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/video").join(name);
    if p.exists() {
        Some(p)
    } else {
        eprintln!("skip: {} not present (cargo xtask corpus --download)", p.display());
        None
    }
}

/// PSNR in dB of two same-size linear images, on their sRGB-encoded values (as viewed).
fn psnr(a: &Rgb32f, b: &Rgb32f) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let enc = |v: f32| if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    let mut se = 0f64;
    for (p, q) in a.data.iter().zip(&b.data) {
        for c in 0..3 {
            let d = f64::from(enc(p[c].clamp(0.0, 1.0)) - enc(q[c].clamp(0.0, 1.0)));
            se += d * d;
        }
    }
    let mse = se / (a.data.len() * 3).max(1) as f64;
    if mse <= 0.0 { 99.0 } else { 10.0 * (1.0 / mse).log10() }
}

fn spread(img: &Rgb32f) -> f32 {
    let g: Vec<f32> = img.data.iter().map(|p| p[1]).collect();
    let max = g.iter().cloned().fold(f32::MIN, f32::max);
    let min = g.iter().cloned().fold(f32::MAX, f32::min);
    max - min
}

struct Expect {
    name: &'static str,
    codec: &'static str,
    size: (u32, u32),
    fps: f64,
    duration: f64,
    audio: bool,
}

const CLIPS: &[Expect] = &[
    Expect { name: "h264-aac-720p-nps-elk.mp4", codec: "h264", size: (1280, 720), fps: 29.97, duration: 15.0, audio: true },
    Expect { name: "h264-1080p-bbb.mp4", codec: "h264", size: (1920, 1080), fps: 60.0, duration: 10.0, audio: false },
    Expect { name: "hevc-1080p-bbb.mp4", codec: "hevc", size: (1920, 1080), fps: 30.0, duration: 10.0, audio: false },
    Expect { name: "vp9-1080p-bbb.webm", codec: "vp9", size: (1920, 1080), fps: 30.0, duration: 10.0, audio: false },
    Expect { name: "av1-1080p-bbb.webm", codec: "av1", size: (1920, 1080), fps: 30.0, duration: 10.0, audio: false },
];

#[test]
fn corpus_clips_probe_and_decode() {
    for e in CLIPS {
        let Some(path) = clip(e.name) else { continue };
        let info = probe(&path).expect("probe");
        assert_eq!(info.codec, e.codec, "{}", e.name);
        assert_eq!((info.width, info.height), e.size, "{}", e.name);
        let fps = f64::from(info.fps_num) / f64::from(info.fps_den);
        assert!((fps - e.fps).abs() < 0.05, "{}: {fps} fps", e.name);
        assert!((info.duration - e.duration).abs() < 0.2, "{}: {} s", e.name, info.duration);
        assert_eq!(info.has_audio, e.audio, "{}", e.name);
        assert!(info.decodable && !info.hdr, "{}", e.name);

        let mut rd = FrameReader::open(&path).expect("open");
        let times = rd.frame_times();
        let expect_frames = (e.duration * e.fps).round() as usize;
        assert!(times.len().abs_diff(expect_frames) <= 2, "{}: {} frames", e.name, times.len());
        assert!(times.windows(2).all(|w| w[1] > w[0]), "{}: frame times not increasing", e.name);
        // two neighbouring frames are near-identical, a frame 3 s away is not, and none are flat
        let step = 1.0 / e.fps;
        let started = std::time::Instant::now();
        let a = rd.frame_at(2.0, Some(480)).expect("frame");
        eprintln!("{}: first frame at 2 s in {:?}", e.name, started.elapsed());
        let b = rd.frame_at(2.0 + step, Some(480)).expect("next frame");
        let far = rd.frame_at(5.0, Some(480)).expect("far frame");
        assert!((a.time - 2.0).abs() < step, "{}: time {}", e.name, a.time);
        assert!(b.time > a.time);
        let (near, away) = (psnr(&a.image, &b.image), psnr(&a.image, &far.image));
        assert!(near > 22.0 && near > away + 3.0, "{}: neighbour {near:.1} dB, far {away:.1} dB", e.name);
        assert!(spread(&a.image) > 0.3, "{}: flat frame", e.name);
        // a backwards seek gives the same picture as before
        let again = rd.frame_at(2.0, Some(480)).expect("seek back");
        assert!(psnr(&a.image, &again.image) > 60.0, "{}: seek back differs", e.name);
        let full = rd.frame_at(1.0, None).expect("full size");
        assert_eq!((full.image.width as u32, full.image.height as u32), e.size);
        assert!(poster(&path, None, 256).expect("poster").image.width <= 256);
    }
}

/// The Big Buck Bunny excerpts are one source: H.264, HEVC, VP9 and AV1 decode the same picture.
#[test]
fn bbb_decoders_agree() {
    let names = ["h264-1080p-bbb.mp4", "hevc-1080p-bbb.mp4", "vp9-1080p-bbb.webm", "av1-1080p-bbb.webm"];
    let mut frames = Vec::new();
    for n in names {
        let Some(path) = clip(n) else { return };
        let mut rd = FrameReader::open(&path).expect("open");
        frames.push((n, rd.frame_at(4.0, Some(640)).expect("frame").image));
    }
    let (n0, ref f0) = frames[0];
    for (n, f) in &frames[1..] {
        let p = psnr(f0, f);
        eprintln!("{n0} vs {n}: {p:.1} dB");
        assert!(p > 25.0, "{n0} vs {n}: {p:.1} dB");
    }
}

#[test]
fn avchd_interlaced_is_reported_unsupported() {
    let Some(path) = clip("avchd-interlaced-1080i-jvc.mts") else { return };
    let info = probe(&path).expect("probe");
    assert_eq!(info.codec, "h264");
    assert_eq!((info.width, info.height), (1920, 1080));
    assert!(info.has_audio);
    assert!(!info.decodable);
    match FrameReader::open(&path) {
        Err(VideoError::Unsupported(why)) => assert!(why.starts_with("interlaced"), "{why}"),
        Err(e) => panic!("wrong error: {e}"),
        Ok(_) => panic!("interlaced H.264 opened"),
    }
    assert!(matches!(poster(&path, None, 64), Err(VideoError::Unsupported(_))));
}

/// Prefixes of every corpus clip: probe and decode return errors or frames, never panic.
#[test]
fn corpus_truncations_never_panic() {
    let dir = std::env::temp_dir().join(format!("lcc-video-corpus-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch");
    for name in CLIPS.iter().map(|e| e.name).chain(["avchd-interlaced-1080i-jvc.mts"]) {
        let Some(path) = clip(name) else { continue };
        let bytes = std::fs::read(&path).expect("read");
        let out = dir.join(name);
        for frac in [0.001, 0.01, 0.1, 0.37, 0.93] {
            let n = (bytes.len() as f64 * frac) as usize;
            std::fs::write(&out, &bytes[..n]).expect("write");
            let _ = probe(&out);
            if let Ok(mut r) = FrameReader::open(&out) {
                let _ = r.frame_at(0.0, Some(64));
                // the last frame the prefix holds (decoding the clip's tail is the corrupt case)
                if let Some(&t) = r.frame_times().last() {
                    let _ = r.frame_at(t, Some(64));
                }
            }
        }
        // a run of corrupted bytes in the middle of the file
        let mut b = bytes.clone();
        let mid = b.len() / 2;
        for (k, x) in b[mid..mid + 4096].iter_mut().enumerate() {
            *x = (k as u8).wrapping_mul(151);
        }
        std::fs::write(&out, &b).expect("write");
        if let Ok(mut r) = FrameReader::open(&out) {
            let d = r.info().duration;
            for t in [d * 0.5, d * 0.55] {
                let _ = r.frame_at(t, Some(64));
            }
        }
        let _ = std::fs::remove_file(&out);
    }
}
