//! Uncompressed and MP3 clip audio: a 440 Hz stereo tone written in-test as PCM (16/24/32-bit
//! integer and 32-bit float, both byte orders) beside an H.264 track in MP4/MOV, Matroska and MXF,
//! and synthetic silent MP3 frames in Matroska. Checks format, exact range lengths, sample-exact
//! timing and seeking; damaged copies never panic.
#![cfg(not(target_arch = "wasm32"))]

use std::f32::consts::TAU;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use filmcraft_isobmff::{AvcConfig, Brand, CodecConfig, FourCc, Mp4Writer, PcmConfig, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use lcc_video::AudioReader;

const W: u32 = 64;
const H: u32 = 48;
const FPS: u32 = 25;
const FRAMES: u32 = 50;
const SR: u32 = 48_000;
const HZ: f32 = 440.0;
const N: usize = (SR * FRAMES / FPS) as usize;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lcc-video-pcm-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn tone(k: usize) -> f32 {
    (k as f32 / SR as f32 * HZ * TAU).sin() * 0.5
}

fn video(format: filmcraft_h264enc::PacketFormat) -> (Vec<filmcraft_h264enc::Packet>, Vec<u8>) {
    let mut cfg = filmcraft_h264enc::EncoderConfig::new(W, H, FPS, 1);
    cfg.format = format;
    cfg.bframes = 0;
    cfg.keyint = 10;
    let mut enc = filmcraft_h264enc::Encoder::new(cfg).unwrap();
    let y = vec![126u8; (W * H) as usize];
    let c = vec![128u8; (W * H / 4) as usize];
    let mut ps = Vec::new();
    for i in 0..FRAMES {
        let f = filmcraft_h264enc::YuvFrame { y: &y, u: &c, v: &c, y_stride: W as usize, uv_stride: (W / 2) as usize };
        ps.extend(enc.encode(&f, i64::from(i)).unwrap());
    }
    ps.extend(enc.flush());
    (ps, enc.avcc())
}

#[derive(Clone, Copy)]
struct Fmt {
    bits: u32,
    float: bool,
    be: bool,
}

/// Interleaved stereo bytes of tone frames `range` (right channel = -left).
fn pcm(f: Fmt, range: std::ops::Range<usize>) -> Vec<u8> {
    let mut out = Vec::new();
    for k in range {
        for x in [tone(k), -tone(k)] {
            let mut b = if f.float {
                x.to_bits().to_be_bytes().to_vec()
            } else {
                let v = (f64::from(x) * f64::from(1u32 << (f.bits - 1))).round() as i64;
                v.to_be_bytes()[8 - f.bits as usize / 8..].to_vec()
            };
            if !f.be {
                b.reverse();
            }
            out.extend(b);
        }
    }
    out
}

fn mp4_clip(path: &Path, f: Fmt, brand: Brand, fourcc: Option<&[u8; 4]>) {
    let (ps, avcc) = video(filmcraft_h264enc::PacketFormat::LengthPrefixed);
    let mut mux = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(brand)).unwrap();
    let vt = mux.add_track(TrackConfig::new(SampleEntry::avc(AvcConfig::parse(&avcc).unwrap(), W as u16, H as u16), FPS)).unwrap();
    let cfg = PcmConfig { bits: f.bits as u16, float: f.float, big_endian: f.be, signed: true, channels: 2, sample_rate: f64::from(SR) };
    let entry = match fourcc {
        Some(cc) => SampleEntry::audio(FourCc(*cc), CodecConfig::Pcm(cfg), 2, f64::from(SR), f.bits as u16),
        None => SampleEntry::pcm(cfg),
    };
    let at = mux.add_track(TrackConfig::new(entry, SR)).unwrap();
    for p in &ps {
        mux.write_sample(vt, WriteSample { data: &p.data, duration: 1, composition_offset: 0, is_sync: p.keyframe }).unwrap();
    }
    // uneven chunks
    let mut k = 0;
    while k < N {
        let n = 3001.min(N - k);
        mux.write_sample(at, WriteSample { data: &pcm(f, k..k + n), duration: 0, composition_offset: 0, is_sync: true }).unwrap();
        k += n;
    }
    std::fs::write(path, mux.finish().unwrap().into_inner()).unwrap();
}

fn mkv_clip(path: &Path, f: Fmt) {
    use filmcraft_matroska::{MkvWriter, MuxOptions, TrackKind, TrackSpec};
    let (ps, avcc) = video(filmcraft_h264enc::PacketFormat::LengthPrefixed);
    let frame_ns = 1_000_000_000 / u64::from(FPS);
    let mut v = TrackSpec::new(TrackKind::Video, "V_MPEG4/ISO/AVC");
    v.codec_private = avcc;
    v.video_size = Some((W, H));
    v.default_duration_ns = Some(frame_ns);
    let id = match (f.float, f.be) {
        (true, _) => "A_PCM/FLOAT/IEEE",
        (false, true) => "A_PCM/INT/BIG",
        (false, false) => "A_PCM/INT/LIT",
    };
    let mut a = TrackSpec::new(TrackKind::Audio, id);
    a.audio = Some((f64::from(SR), 2, Some(u64::from(f.bits))));
    let mut w = MkvWriter::new(Cursor::new(Vec::new()), vec![v, a], MuxOptions::default()).unwrap();
    let block = 1920; // one video frame of audio
    for (i, p) in ps.iter().enumerate() {
        w.write_frame(0, p.pts * frame_ns as i64, p.keyframe, &p.data, Some(frame_ns)).unwrap();
        let k = i * block;
        if k < N {
            w.write_frame(1, (k as u64 * 1_000_000_000 / u64::from(SR)) as i64, true, &pcm(f, k..(k + block).min(N)), None).unwrap();
        }
    }
    std::fs::write(path, w.finish().unwrap().into_inner()).unwrap();
}

/// OP1a MXF: H.264 pictures with 24-bit stereo sound interleaved per edit unit.
fn mxf_clip(path: &Path) {
    use filmcraft_mxf::{FrameInfo, MxfWriter, PackageIds, Pattern, PictureCoding, PictureDesc, Rational, SoundDesc, WriterConfig};
    let (ps, _) = video(filmcraft_h264enc::PacketFormat::AnnexB);
    let mut cfg = WriterConfig::new(Pattern::Op1a, Rational::new(FPS as i32, 1), PackageIds::from_seed("test", "Tone"));
    cfg.picture = Some(PictureDesc::new(PictureCoding::Avc { profile_idc: 100, intra: false }, W, H));
    cfg.sound = vec![SoundDesc { sample_rate: SR, channels: 2, bits: 24 }];
    let mut w = MxfWriter::new(Cursor::new(Vec::new()), cfg).unwrap();
    let f = Fmt { bits: 24, float: false, be: false };
    let block = (SR / FPS) as usize;
    for (i, p) in ps.iter().enumerate() {
        let info = if p.keyframe { FrameInfo::intra() } else { FrameInfo::default() };
        w.push_picture(&p.data, info).unwrap();
        w.push_sound(0, &pcm(f, i * block..(i + 1) * block)).unwrap();
    }
    std::fs::write(path, w.finish().unwrap().into_inner()).unwrap();
}

fn correlation(left: &[f32], k0: usize) -> f32 {
    let (mut xy, mut xx, mut yy) = (0f32, 0f32, 0f32);
    for (k, &x) in left.iter().enumerate() {
        let y = tone(k0 + k);
        xy += x * y;
        xx += x * x;
        yy += y * y;
    }
    xy / (xx.sqrt() * yy.sqrt()).max(1e-9)
}

fn check(path: &Path, tol: f32) {
    let mut r = AudioReader::open(path).unwrap();
    assert_eq!(r.info().codec, "pcm", "{}", path.display());
    assert_eq!(r.info().sample_rate, SR);
    assert_eq!(r.info().channels, 2);
    let got = r.read_range(0.5, Some(1.0)).unwrap();
    assert_eq!(got.len(), (SR / 2) as usize * 2, "{}", path.display());
    // sample-exact: the decoded values are the written ones
    for (k, s) in got.chunks(2).enumerate() {
        let want = tone(SR as usize / 2 + k);
        assert!((s[0] - want).abs() <= tol && (s[1] + want).abs() <= tol, "{}: frame {k}: {s:?} vs {want}", path.display());
    }
    assert!(correlation(&got.iter().step_by(2).copied().collect::<Vec<_>>(), SR as usize / 2) > 0.999);
    let b = r.samples_from(1.25).next().unwrap().unwrap();
    assert!((b.time - 1.25).abs() < 1e-6, "first block at {}", b.time);
    let all = r.read_range(0.0, None).unwrap();
    assert_eq!(all.len(), N * 2, "{}", path.display());
}

const S16LE: Fmt = Fmt { bits: 16, float: false, be: false };
const S16BE: Fmt = Fmt { bits: 16, float: false, be: true };
const S24BE: Fmt = Fmt { bits: 24, float: false, be: true };
const S24LE: Fmt = Fmt { bits: 24, float: false, be: false };
const S32BE: Fmt = Fmt { bits: 32, float: false, be: true };
const F32LE: Fmt = Fmt { bits: 32, float: true, be: false };
const F32BE: Fmt = Fmt { bits: 32, float: true, be: true };

#[test]
fn pcm_in_mp4_and_mov() {
    let cases: [(&str, Fmt, Brand, Option<&[u8; 4]>, f32); 7] = [
        ("lpcm16.mp4", S16LE, Brand::Mp4, None, 1e-4),
        ("lpcm24.mov", S24BE, Brand::Mov, None, 1e-6),
        ("sowt.mov", S16LE, Brand::Mov, Some(b"sowt"), 1e-4),
        ("twos.mov", S16BE, Brand::Mov, Some(b"twos"), 1e-4),
        ("in24.mov", S24BE, Brand::Mov, Some(b"in24"), 1e-6),
        ("in32.mov", S32BE, Brand::Mov, Some(b"in32"), 1e-6),
        ("fl32.mov", F32BE, Brand::Mov, Some(b"fl32"), 0.0),
    ];
    for (name, f, brand, cc, tol) in cases {
        let p = scratch(name);
        mp4_clip(&p, f, brand, cc);
        check(&p, tol);
    }
}

#[test]
fn pcm_in_matroska() {
    for (name, f, tol) in [("lit16.mkv", S16LE, 1e-4), ("big24.mkv", S24BE, 1e-6), ("lit24.mkv", S24LE, 1e-6), ("float.mkv", F32LE, 0.0)] {
        let p = scratch(name);
        mkv_clip(&p, f);
        check(&p, tol);
    }
}

#[test]
fn pcm_in_mxf() {
    let p = scratch("tone.mxf");
    mxf_clip(&p);
    check(&p, 1e-6);
}

/// A silent MPEG-1 Layer III frame: 128 kbit/s, 48 kHz, stereo (384 bytes, zero side info).
fn silent_mp3_frame() -> Vec<u8> {
    let mut f = vec![0u8; 384];
    f[..4].copy_from_slice(&[0xFF, 0xFB, 0x94, 0x00]);
    f
}

#[test]
fn mp3_in_matroska() {
    use filmcraft_matroska::{MkvWriter, MuxOptions, TrackKind, TrackSpec};
    let (ps, avcc) = video(filmcraft_h264enc::PacketFormat::LengthPrefixed);
    let frame_ns = 1_000_000_000 / u64::from(FPS);
    let mut v = TrackSpec::new(TrackKind::Video, "V_MPEG4/ISO/AVC");
    v.codec_private = avcc;
    v.video_size = Some((W, H));
    v.default_duration_ns = Some(frame_ns);
    let mut a = TrackSpec::new(TrackKind::Audio, "A_MPEG/L3");
    a.audio = Some((f64::from(SR), 2, None));
    let mut w = MkvWriter::new(Cursor::new(Vec::new()), vec![v, a], MuxOptions::default()).unwrap();
    for p in &ps {
        w.write_frame(0, p.pts * frame_ns as i64, p.keyframe, &p.data, Some(frame_ns)).unwrap();
    }
    let frames = 80; // 1.92 s
    for k in 0..frames {
        w.write_frame(1, (k as u64 * 1152 * 1_000_000_000 / u64::from(SR)) as i64, true, &silent_mp3_frame(), None).unwrap();
    }
    let path = scratch("silence.mkv");
    std::fs::write(&path, w.finish().unwrap().into_inner()).unwrap();
    let mut r = AudioReader::open(&path).unwrap();
    assert_eq!(r.info().codec, "mp3");
    assert_eq!(r.info().sample_rate, SR);
    assert_eq!(r.info().channels, 2);
    let got = r.read_range(0.5, Some(1.0)).unwrap();
    assert_eq!(got.len(), (SR / 2) as usize * 2);
    assert!(got.iter().all(|s| s.abs() < 1e-6));
    assert_eq!(r.read_range(0.0, None).unwrap().len(), frames * 1152 * 2);
}

#[test]
fn damaged_pcm_clips_never_panic() {
    let p = scratch("damaged-src.mov");
    mp4_clip(&p, S24BE, Brand::Mov, Some(b"in24"));
    let m = scratch("damaged-src.mxf");
    mxf_clip(&m);
    for src in [p, m] {
        let bytes = std::fs::read(&src).unwrap();
        for cut in [bytes.len() / 3, bytes.len() / 2, bytes.len() - 7] {
            let d = scratch("damaged.bin");
            let mut b = bytes[..cut].to_vec();
            for i in (cut / 2..cut).step_by(997) {
                b[i] ^= 0x5A;
            }
            std::fs::write(&d, &b).unwrap();
            if let Ok(mut r) = AudioReader::open(&d) {
                let _ = r.read_range(0.0, None);
                let _ = r.read_range(1.0, Some(1.5));
            }
        }
    }
}
