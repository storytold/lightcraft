//! Audio export from a non-MP4 source: a 440 Hz stereo AAC tone in Matroska (FilmCraft has no
//! Opus or AC-3 encoder, so those codecs are covered by `lcc_video`'s decode tests; the export
//! path after decode is codec-independent). Exports a trimmed range to MP4 (AAC-LC) and ProRes
//! MOV (16-bit PCM) and checks the output's length, the tone's pitch and A/V sync.
#![cfg(not(target_arch = "wasm32"))]

use std::f32::consts::TAU;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use lcc_video_export::{ClipExportOptions, ClipFormat, export_clip};

const W: u32 = 64;
const H: u32 = 48;
const FPS: u32 = 25;
const FRAMES: u32 = 50;
const SR: u32 = 48_000;
const HZ: f32 = 440.0;
const TRIM: (f64, f64) = (0.4, 1.6);

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lcc-video-export-audio-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tone(t: f64) -> f32 {
    (t as f32 * HZ * TAU).sin() * 0.5
}

/// 2 s of grey H.264 plus the stereo AAC tone, in Matroska (codec delay signalled).
fn mkv_clip(path: &Path) {
    use filmcraft_matroska::{MkvWriter, MuxOptions, TrackKind, TrackSpec};
    let mut cfg = filmcraft_h264enc::EncoderConfig::new(W, H, FPS, 1);
    cfg.format = filmcraft_h264enc::PacketFormat::LengthPrefixed;
    cfg.aud = false;
    cfg.keyint = 10;
    let mut enc = filmcraft_h264enc::Encoder::new(cfg).unwrap();
    let (y, c) = (vec![126u8; (W * H) as usize], vec![128u8; (W * H / 4) as usize]);
    let mut ps = Vec::new();
    for i in 0..FRAMES {
        let f = filmcraft_h264enc::YuvFrame { y: &y, u: &c, v: &c, y_stride: W as usize, uv_stride: (W / 2) as usize };
        ps.extend(enc.encode(&f, i64::from(i)).unwrap());
    }
    ps.extend(enc.flush());
    let mut a = filmcraft_aac::Encoder::new(filmcraft_aac::EncoderConfig::cbr(SR, 2, 160_000)).unwrap();
    let s: Vec<f32> = (0..(SR * FRAMES / FPS) as usize).map(|k| tone(k as f64 / f64::from(SR))).collect();
    let mut aus = Vec::new();
    for chunk in s.chunks(1024) {
        aus.extend(a.encode(&[chunk, chunk]));
    }
    aus.extend(a.flush());

    let frame_ns = 1_000_000_000 / u64::from(FPS);
    let mut v = TrackSpec::new(TrackKind::Video, "V_MPEG4/ISO/AVC");
    v.codec_private = enc.avcc();
    v.video_size = Some((W, H));
    v.default_duration_ns = Some(frame_ns);
    let mut at = TrackSpec::new(TrackKind::Audio, "A_AAC");
    at.codec_private = a.audio_specific_config();
    at.audio = Some((f64::from(SR), 2, None));
    let delay = u64::from(a.priming_samples()) * 1_000_000_000 / u64::from(SR);
    at.codec_delay_ns = delay;
    let mut w = MkvWriter::new(Cursor::new(Vec::new()), vec![v, at], MuxOptions::default()).unwrap();
    let au_ns = |k: usize| (k as u64 * 1024 * 1_000_000_000 / u64::from(SR)) as i64;
    let (mut vi, mut ai) = (0, 0);
    while vi < ps.len() || ai < aus.len() {
        let vt = ps.get(vi).map(|p| p.dts * frame_ns as i64);
        if vt.is_some_and(|t| ai >= aus.len() || t <= au_ns(ai)) {
            let p = &ps[vi];
            w.write_frame(0, p.pts * frame_ns as i64, p.keyframe, &p.data, Some(frame_ns)).unwrap();
            vi += 1;
        } else {
            w.write_frame(1, au_ns(ai) - delay as i64, true, &aus[ai], None).unwrap();
            ai += 1;
        }
    }
    std::fs::write(path, w.finish().unwrap().into_inner()).unwrap();
}

fn export(src: &Path, format: ClipFormat, dir: &Path) -> PathBuf {
    let out = dir.join(format!("out.{}", format.extension()));
    let opts = ClipExportOptions { format, quality: 60, max_side: None, out: out.clone() };
    let rep = export_clip(src, (TRIM.0, Some(TRIM.1)), &|_| {}, &opts, &AtomicBool::new(false), &|_| {}).unwrap();
    assert!(rep.has_audio);
    out
}

/// Pitch, length and sync of the exported left channel (output time 0 = source time TRIM.0).
fn check(left: &[f32]) {
    let len = left.len() as f64 / f64::from(SR);
    assert!((len - (TRIM.1 - TRIM.0)).abs() < 1.0 / f64::from(FPS), "audio length {len}");
    // pitch: zero crossings over the middle second
    let mid = &left[SR as usize / 10..SR as usize * 11 / 10];
    let crossings = mid.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
    assert!((crossings as f32 - 2.0 * HZ).abs() <= 4.0, "{crossings} zero crossings in 1 s");
    // sync: best lag against the source tone at the in point, well under one frame (1920 samples)
    let corr = |lag: i32| {
        let (mut xy, mut xx, mut yy) = (0f32, 0f32, 0f32);
        for (k, &x) in mid.iter().enumerate() {
            let y = tone(TRIM.0 + (k as f64 + f64::from(SR / 10) + f64::from(lag)) / f64::from(SR));
            xy += x * y;
            xx += x * x;
            yy += y * y;
        }
        xy / (xx.sqrt() * yy.sqrt()).max(1e-9)
    };
    // the tone's period is ~109 samples, so search half a period around 0 (a 1 ms Matroska
    // timestamp rounding is within it)
    let (lag, c) = (-54..=54).map(|l| (l, corr(l))).fold((0, -1.0), |a, b| if b.1 > a.1 { b } else { a });
    assert!(c > 0.95, "correlation {c} at lag {lag}");
    assert!(lag.abs() <= 48, "audio is {lag} samples off");
}

#[test]
fn matroska_audio_exports_to_mp4_as_aac_in_sync() {
    let dir = scratch("mp4");
    let src = dir.join("in.mkv");
    mkv_clip(&src);
    let out = export(&src, ClipFormat::H264Mp4, &dir);
    let mut r = lcc_video::AudioReader::open(&out).unwrap();
    assert_eq!(r.info().codec, "aac");
    assert_eq!(r.info().channels, 2);
    let pcm = r.read_range(0.0, None).unwrap();
    let left: Vec<f32> = pcm.iter().step_by(2).copied().collect();
    check(&left);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn matroska_audio_exports_to_prores_mov_as_pcm_in_sync() {
    let dir = scratch("mov");
    let src = dir.join("in.mkv");
    mkv_clip(&src);
    let out = export(&src, ClipFormat::ProResMov, &dir);
    let f = std::fs::File::open(&out).unwrap();
    let file = filmcraft_isobmff::open(&f).unwrap();
    let ti = file.track_of_kind(filmcraft_isobmff::TrackKind::Audio).unwrap();
    let t = &file.tracks[ti];
    let Some(filmcraft_isobmff::CodecConfig::Pcm(p)) = t.codec() else { panic!("not PCM") };
    assert_eq!((p.bits, p.channels, p.sample_rate as u32), (16, 2, SR));
    assert_eq!(t.edit_offset, 0);
    let mut bytes = Vec::new();
    for i in 0..t.samples.len() {
        bytes.extend(file.read_sample(&f, ti, i).unwrap());
    }
    let left: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0).collect();
    check(&left);
    std::fs::remove_dir_all(&dir).ok();
}
