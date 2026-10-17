//! Synthetic H.264 MP4 clips generated in-test (filmcraft-h264enc + Mp4Writer): probe fields,
//! frame colour, seek accuracy, and malformed files that must never panic.
#![cfg(not(target_arch = "wasm32"))]

use std::io::Cursor;
use std::path::{Path, PathBuf};

use filmcraft_h264enc::{ColorConfig, Encoder, EncoderConfig, PacketFormat, RateControl, YuvFrame};
use filmcraft_isobmff::{AvcConfig, Brand, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use lcc_video::{FrameReader, VideoError, poster, probe};

struct Clip {
    w: u32,
    h: u32,
    frames: usize,
    fps: (u32, u32),
    bframes: u8,
    keyint: u32,
    color: ColorConfig,
    /// VUI sample aspect ratio.
    sar: (u16, u16),
}

impl Clip {
    fn new(w: u32, h: u32, frames: usize) -> Self {
        Clip { w, h, frames, fps: (30, 1), bframes: 0, keyint: 250, color: ColorConfig::default(), sar: (1, 1) }
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lcc-video-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir.join(name)
}

/// Encode `clip`, frame k's planes given by `pic(k) -> (y, u, v)` (full planes), into an MP4.
fn write_clip(clip: &Clip, pic: impl Fn(usize) -> (Vec<u8>, Vec<u8>, Vec<u8>)) -> Vec<u8> {
    let mut cfg = EncoderConfig::new(clip.w, clip.h, clip.fps.0, clip.fps.1);
    cfg.format = PacketFormat::LengthPrefixed;
    cfg.bframes = clip.bframes;
    cfg.keyint = clip.keyint;
    cfg.scenecut = false;
    cfg.rate = RateControl::Crf(12.0);
    cfg.color = clip.color;
    cfg.threads = 1;
    cfg.sar = clip.sar;
    let mut enc = Encoder::new(cfg).expect("encoder");
    let den = clip.fps.1 as i64;
    let mut packets = Vec::new();
    for k in 0..clip.frames {
        let (y, u, v) = pic(k);
        let f = YuvFrame { y: &y, u: &u, v: &v, y_stride: clip.w as usize, uv_stride: clip.w as usize / 2 };
        packets.extend(enc.encode(&f, k as i64 * den).expect("encode"));
    }
    packets.extend(enc.flush());
    let avcc = AvcConfig::parse(&enc.avcc()).expect("avcC");
    let entry = SampleEntry::avc(avcc, clip.w as u16, clip.h as u16);
    let mut mux = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).expect("writer");
    let shift = clip.bframes as i64 * den;
    let mut tc = TrackConfig::new(entry, clip.fps.0);
    if shift > 0 {
        tc.media_start = Some(shift);
    }
    let t = mux.add_track(tc).expect("track");
    for (k, p) in packets.iter().enumerate() {
        let off = p.pts - k as i64 * den + shift;
        mux.write_sample(t, WriteSample { data: &p.data, duration: clip.fps.1, composition_offset: off as i32, is_sync: p.keyframe })
            .expect("sample");
    }
    mux.finish().expect("finish").into_inner()
}

fn flat(clip: &Clip, y: u8, u: u8, v: u8) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let (w, h) = (clip.w as usize, clip.h as usize);
    (vec![y; w * h], vec![u; w * h / 4], vec![v; w * h / 4])
}

/// BT.709 limited-range Y'CbCr of an sRGB-encoded colour (0..1).
fn ycbcr709(r: f32, g: f32, b: f32) -> (u8, u8, u8) {
    let (kr, kb) = (0.2126f32, 0.0722f32);
    let y = kr * r + (1.0 - kr - kb) * g + kb * b;
    let cb = (b - y) / (2.0 * (1.0 - kb));
    let cr = (r - y) / (2.0 * (1.0 - kr));
    let q = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    (q(16.0 + 219.0 * y), q(128.0 + 224.0 * cb), q(128.0 + 224.0 * cr))
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn mean(img: &lcc_raster::Rgb32f) -> [f32; 3] {
    let n = img.data.len().max(1) as f32;
    let s = img.data.iter().fold([0f32; 3], |a, p| [a[0] + p[0], a[1] + p[1], a[2] + p[2]]);
    s.map(|v| v / n)
}

fn save(name: &str, bytes: &[u8]) -> PathBuf {
    let p = scratch(name);
    std::fs::write(&p, bytes).expect("write clip");
    p
}

#[test]
fn probe_reports_the_stream() {
    let clip = Clip { fps: (30000, 1001), ..Clip::new(160, 96, 12) };
    let path = save("probe.mp4", &write_clip(&clip, |_| flat(&clip, 120, 128, 128)));
    let info = probe(&path).expect("probe");
    assert_eq!((info.width, info.height), (160, 96));
    assert_eq!((info.fps_num, info.fps_den), (30000, 1001));
    assert_eq!(info.codec, "h264");
    assert!(!info.has_audio);
    assert!(info.decodable && !info.hdr);
    assert_eq!(info.rotation, 0);
    let expect = 12.0 * 1001.0 / 30000.0;
    assert!((info.duration - expect).abs() < 1e-3, "{}", info.duration);
}

#[test]
fn flat_colours_decode_to_their_linear_values() {
    let colours = [(0.8f32, 0.2f32, 0.1f32), (0.1, 0.6, 0.3), (0.5, 0.5, 0.5), (0.2, 0.3, 0.9)];
    let clip = Clip { keyint: 1, ..Clip::new(64, 64, colours.len()) };
    let path = save(
        "flat.mp4",
        &write_clip(&clip, |k| {
            let (r, g, b) = colours[k];
            let (y, u, v) = ycbcr709(r, g, b);
            flat(&clip, y, u, v)
        }),
    );
    let mut rd = FrameReader::open(&path).expect("open");
    for (k, &(r, g, b)) in colours.iter().enumerate() {
        let f = rd.frame_at(k as f64 / 30.0, None).expect("frame");
        assert_eq!((f.image.width, f.image.height), (64, 64));
        let m = mean(&f.image);
        let want = [r, g, b].map(srgb_to_linear);
        for c in 0..3 {
            assert!((m[c] - want[c]).abs() < 0.02, "frame {k} channel {c}: {} vs {}", m[c], want[c]);
        }
    }
}

#[test]
fn gradient_is_monotonic_and_scaled_decode_bounds_the_side() {
    let clip = Clip::new(256, 128, 3);
    let path = save(
        "ramp.mp4",
        &write_clip(&clip, |_| {
            let (w, h) = (clip.w as usize, clip.h as usize);
            let y: Vec<u8> = (0..w * h).map(|i| (16 + (i % w) * 219 / (w - 1)) as u8).collect();
            (y, vec![128; w * h / 4], vec![128; w * h / 4])
        }),
    );
    let mut rd = FrameReader::open(&path).expect("open");
    let f = rd.frame_at(0.0, None).expect("frame");
    let row: Vec<f32> = (0..256).step_by(16).map(|x| f.image.data[64 * 256 + x][1]).collect();
    assert!(row.windows(2).all(|p| p[1] >= p[0] - 0.005), "{row:?}");
    assert!(row[0] < 0.01 && *row.last().expect("row") > 0.8, "{row:?}");
    let small = rd.frame_at(0.0, Some(64)).expect("draft");
    assert!(small.image.width <= 64 && small.image.height <= 64);
    assert_eq!(small.image.width, 64);
    let p = poster(&path, None, 32).expect("poster");
    assert!(p.image.width <= 32);
}

/// Each frame k has luma 30 + 6k: the decoded mean identifies which frame came back.
fn which(img: &lcc_raster::Rgb32f) -> usize {
    let g = mean(img)[1];
    // invert the sRGB curve and the limited-range mapping
    let v = if g <= 0.0031308 { g * 12.92 } else { 1.055 * g.powf(1.0 / 2.4) - 0.055 };
    let y = 16.0 + 219.0 * v;
    ((y - 30.0) / 6.0).round().max(0.0) as usize
}

fn seek_clip(bframes: u8, name: &str) -> PathBuf {
    let clip = Clip { bframes, keyint: 10, ..Clip::new(64, 48, 30) };
    save(name, &write_clip(&clip, |k| flat(&clip, 30 + 6 * k as u8, 128, 128)))
}

#[test]
fn seeking_returns_the_frame_at_the_time() {
    for bframes in [0u8, 2] {
        let path = seek_clip(bframes, &format!("seek{bframes}.mp4"));
        let mut rd = FrameReader::open(&path).expect("open");
        assert_eq!(rd.frame_times().len(), 30);
        // random order, backwards, across GOPs, mid-frame times
        for k in [17usize, 3, 29, 0, 10, 9, 11, 25, 24, 5] {
            let t = k as f64 / 30.0 + 0.01;
            let f = rd.frame_at(t, None).expect("frame");
            assert_eq!(which(&f.image), k, "bframes {bframes}, t {t}");
            assert!((f.time - k as f64 / 30.0).abs() < 1e-6, "time {}", f.time);
        }
        // sequential playback reuses the decoder
        for k in 0..30 {
            let f = rd.frame_at(k as f64 / 30.0, Some(32)).expect("play");
            assert_eq!(which(&f.image), k, "bframes {bframes} play");
        }
        // next_frame continues after frame_at without re-seeking, across GOPs, in order
        rd.frame_at(7.0 / 30.0, None).expect("frame 7");
        for k in 8..30 {
            let f = rd.next_frame(Some(32)).expect("next").expect("a frame");
            assert_eq!(which(&f.image), k, "bframes {bframes} next");
        }
        assert!(rd.next_frame(None).expect("end").is_none());
        // seek + frames_from: backwards and forwards
        let got: Vec<usize> = rd.frames_from(12.0 / 30.0 + 0.01, Some(16)).map(|f| which(&f.expect("frame").image)).collect();
        assert_eq!(got, (12..30).collect::<Vec<_>>(), "bframes {bframes}");
        rd.seek(2.0 / 30.0);
        assert_eq!(which(&rd.next_frame(None).expect("2").expect("frame").image), 2);
        rd.seek(f64::NAN);
        assert_eq!(which(&rd.next_frame(None).expect("0").expect("frame").image), 0);
        // out of range times clamp
        assert_eq!(which(&rd.frame_at(99.0, None).expect("end").image), 29);
        assert_eq!(which(&rd.frame_at(-5.0, None).expect("start").image), 0);
        assert_eq!(which(&rd.frame_at(f64::NAN, None).expect("nan").image), 0);
    }
}

fn no_panic(path: &Path) {
    let _ = probe(path);
    if let Ok(mut r) = FrameReader::open(path) {
        let _ = r.frame_at(0.0, Some(32));
        let _ = r.frame_at(0.5, None);
    }
    let _ = poster(path, Some(0.2), 16);
}

#[test]
fn truncated_and_garbage_files_never_panic() {
    let good =
        write_clip(&Clip { keyint: 5, ..Clip::new(64, 48, 10) }, |k| (vec![40 + k as u8 * 10; 64 * 48], vec![128; 64 * 12], vec![100; 64 * 12]));
    let p = scratch("fuzz.mp4");
    // truncations
    let mut cuts: Vec<usize> = (0..good.len()).step_by(97).collect();
    cuts.extend([0, 1, 7, 8, 16, good.len() - 1]);
    for n in cuts {
        std::fs::write(&p, &good[..n]).expect("write");
        no_panic(&p);
    }
    // byte corruption (deterministic xorshift)
    let mut s = 0x9e37_79b9_7f4a_7c15u64;
    let mut rnd = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    for _ in 0..200 {
        let mut b = good.clone();
        for _ in 0..1 + rnd() % 8 {
            let i = (rnd() as usize) % b.len();
            b[i] = rnd() as u8;
        }
        std::fs::write(&p, &b).expect("write");
        no_panic(&p);
    }
    // pure garbage and empty files
    for len in [0usize, 4, 64, 4096] {
        let b: Vec<u8> = (0..len).map(|_| rnd() as u8).collect();
        std::fs::write(&p, &b).expect("write");
        assert!(probe(&p).is_err());
        no_panic(&p);
    }
    // a missing file is an I/O error
    assert!(matches!(probe(&scratch("missing.mp4")), Err(VideoError::Io(_))));
}

#[test]
fn anamorphic_vui_sar_is_shown_at_display_aspect() {
    // 4:3 pixels (HDV-style 1440 → 1920): 96×64 stored, 128×64 shown
    let clip = Clip { sar: (4, 3), ..Clip::new(96, 64, 3) };
    let path = save("sar.mp4", &write_clip(&clip, |_| flat(&clip, 120, 128, 128)));
    let info = probe(&path).expect("probe");
    assert_eq!((info.par_num, info.par_den), (4, 3));
    assert_eq!((info.coded_width, info.coded_height), (96, 64));
    assert_eq!((info.width, info.height), (128, 64));
    assert!(!info.interlaced);
    let mut rd = FrameReader::open(&path).expect("open");
    let f = rd.frame_at(0.0, None).expect("frame");
    assert_eq!((f.image.width, f.image.height), (128, 64));
    let f = rd.frame_at(0.0, Some(64)).expect("small");
    assert_eq!((f.image.width, f.image.height), (64, 32));
    // square pixels are untouched
    let sq = Clip::new(96, 64, 2);
    let path = save("square.mp4", &write_clip(&sq, |_| flat(&sq, 120, 128, 128)));
    let info = probe(&path).expect("probe");
    assert_eq!((info.par_num, info.par_den, info.width, info.coded_width), (1, 1, 96, 96));
}
