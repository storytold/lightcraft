//! Clip audio decode: a 440 Hz stereo AAC tone synthesised in-test (FilmCraft's AAC encoder)
//! alongside an H.264 track in MP4, Matroska and an MPEG transport stream (ADTS). Checks the
//! format, the trimmed ranges' length, timing against the tone's phase (priming handled),
//! seeking, resampling; the corpus clip when present; truncated / corrupted copies never panic.
#![cfg(not(target_arch = "wasm32"))]

use std::f32::consts::TAU;
use std::io::{BufWriter, Cursor};
use std::path::{Path, PathBuf};

use filmcraft_isobmff::{AvcConfig, Brand, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use lcc_video::{AudioReader, VideoError, resample};

const W: u32 = 64;
const H: u32 = 48;
const FPS: u32 = 25;
const FRAMES: u32 = 50;
const SR: u32 = 48_000;
const HZ: f32 = 440.0;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lcc-video-audio-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn tone(t: f64) -> f32 {
    (t as f32 * HZ * TAU).sin() * 0.5
}

/// H.264 packets of FRAMES grey frames (decode order) and the avcC.
fn video() -> (Vec<filmcraft_h264enc::Packet>, Vec<u8>) {
    let mut cfg = filmcraft_h264enc::EncoderConfig::new(W, H, FPS, 1);
    cfg.format = filmcraft_h264enc::PacketFormat::LengthPrefixed;
    cfg.aud = false;
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

/// Two seconds of the stereo tone as AAC access units, plus the ASC and the encoder priming.
fn aac() -> (Vec<Vec<u8>>, Vec<u8>, u32) {
    let mut a = filmcraft_aac::Encoder::new(filmcraft_aac::EncoderConfig::cbr(SR, 2, 160_000)).unwrap();
    let n = (SR * FRAMES / FPS) as usize;
    let s: Vec<f32> = (0..n).map(|k| tone(k as f64 / f64::from(SR))).collect();
    let mut aus = Vec::new();
    for chunk in s.chunks(1024) {
        aus.extend(a.encode(&[chunk, chunk]));
    }
    aus.extend(a.flush());
    (aus, a.audio_specific_config(), a.priming_samples())
}

fn mp4_clip(path: &Path) {
    let (ps, avcc) = video();
    let (aus, asc, priming) = aac();
    let mut mux = Mp4Writer::new(BufWriter::new(std::fs::File::create(path).unwrap()), WriterOptions::new(Brand::Mp4)).unwrap();
    let mut vt = TrackConfig::new(SampleEntry::avc(AvcConfig::parse(&avcc).unwrap(), W as u16, H as u16), FPS);
    if ps.iter().any(|p| p.pts != p.dts) {
        vt.media_start = Some(1);
    }
    let vt = mux.add_track(vt).unwrap();
    let mut at = TrackConfig::new(SampleEntry::aac(asc, 2, SR), SR);
    at.media_start = Some(i64::from(priming));
    let at = mux.add_track(at).unwrap();
    for p in &ps {
        mux.write_sample(vt, WriteSample { data: &p.data, duration: 1, composition_offset: (p.pts - p.dts) as i32, is_sync: p.keyframe }).unwrap();
    }
    for au in &aus {
        mux.write_sample(at, WriteSample { data: au, duration: 1024, composition_offset: 0, is_sync: true }).unwrap();
    }
    mux.finish().unwrap();
}

fn mkv_clip(path: &Path) {
    use filmcraft_matroska::{MkvWriter, MuxOptions, TrackKind, TrackSpec};
    let (ps, avcc) = video();
    let (aus, asc, priming) = aac();
    let frame_ns = 1_000_000_000 / u64::from(FPS);
    let mut v = TrackSpec::new(TrackKind::Video, "V_MPEG4/ISO/AVC");
    v.codec_private = avcc;
    v.video_size = Some((W, H));
    v.default_duration_ns = Some(frame_ns);
    let mut a = TrackSpec::new(TrackKind::Audio, "A_AAC");
    a.codec_private = asc;
    a.audio = Some((f64::from(SR), 2, None));
    let delay = u64::from(priming) * 1_000_000_000 / u64::from(SR);
    a.codec_delay_ns = delay;
    let mut w = MkvWriter::new(Cursor::new(Vec::new()), vec![v, a], MuxOptions::default()).unwrap();
    let au_ns = |k: usize| (k as u64 * 1024 * 1_000_000_000 / u64::from(SR)) as i64;
    let (mut vi, mut ai) = (0, 0);
    // interleave by time, as a muxer would
    while vi < ps.len() || ai < aus.len() {
        let vt = ps.get(vi).map(|p| p.dts * frame_ns as i64);
        if vt.is_some_and(|t| ai >= aus.len() || t <= au_ns(ai)) {
            let p = &ps[vi];
            w.write_frame(0, p.pts * frame_ns as i64, p.keyframe, &p.data, Some(frame_ns)).unwrap();
            vi += 1;
        } else {
            // presentation time: the muxer stores it plus the codec delay
            w.write_frame(1, au_ns(ai) - delay as i64, true, &aus[ai], None).unwrap();
            ai += 1;
        }
    }
    std::fs::write(path, w.finish().unwrap().into_inner()).unwrap();
}

struct TsMux {
    out: Vec<u8>,
    cc: [u8; 0x2000],
}

impl TsMux {
    fn packet(&mut self, pid: u16, pusi: bool, payload: &[u8]) -> usize {
        let cc = &mut self.cc[pid as usize];
        let mut p = vec![0x47, (pusi as u8) << 6 | (pid >> 8) as u8, pid as u8];
        let n = payload.len().min(184);
        if n < 184 {
            let al = 183 - n;
            p.push(0x30 | *cc);
            p.push(al as u8);
            if al > 0 {
                p.push(0);
                p.extend(std::iter::repeat_n(0xFF, al - 1));
            }
        } else {
            p.push(0x10 | *cc);
        }
        *cc = (*cc + 1) & 15;
        p.extend_from_slice(&payload[..n]);
        self.out.extend(p);
        n
    }

    fn section(&mut self, pid: u16, body: &[u8]) {
        let mut payload = vec![0];
        payload.extend_from_slice(body);
        let mut crc = 0xFFFF_FFFFu32;
        for &b in body {
            crc ^= u32::from(b) << 24;
            for _ in 0..8 {
                crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
            }
        }
        payload.extend(crc.to_be_bytes());
        self.packet(pid, true, &payload);
    }

    fn pes(&mut self, pid: u16, data: &[u8]) {
        let mut rest = data;
        let mut first = true;
        while !rest.is_empty() {
            let n = self.packet(pid, first, rest);
            rest = &rest[n..];
            first = false;
        }
    }
}

fn stamp(prefix: u8, v: i64) -> [u8; 5] {
    [
        (prefix << 4) | (((v >> 30) & 7) as u8) << 1 | 1,
        (v >> 22) as u8,
        (((v >> 15) & 0x7F) as u8) << 1 | 1,
        (v >> 7) as u8,
        ((v & 0x7F) as u8) << 1 | 1,
    ]
}

/// H.264 on PID 0x101 and ADTS AAC on PID 0x102, the tone starting with the first shown frame.
fn ts_clip(path: &Path) {
    let mut cfg = filmcraft_h264enc::EncoderConfig::new(W, H, FPS, 1);
    cfg.format = filmcraft_h264enc::PacketFormat::AnnexB;
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
    let (aus, _, priming) = aac();
    let mut m = TsMux { out: Vec::new(), cc: [0; 0x2000] };
    m.section(0, &[0x00, 0xB0, 13, 0, 1, 0xC1, 0, 0, 0, 1, 0xE1, 0x00]);
    m.section(0x100, &[0x02, 0xB0, 23, 0, 1, 0xC1, 0, 0, 0xE1, 0x01, 0xF0, 0, 0x1B, 0xE1, 0x01, 0xF0, 0, 0x0F, 0xE1, 0x02, 0xF0, 0]);
    let base = 90_000i64;
    let tick = 90_000 / i64::from(FPS);
    for (i, p) in ps.iter().enumerate() {
        let mut pes = vec![0, 0, 1, 0xE0, 0, 0, 0x80, 0xC0, 10];
        pes.extend(stamp(3, base + p.pts * tick + tick));
        pes.extend(stamp(1, base + i as i64 * tick));
        pes.extend_from_slice(&p.data);
        m.pes(0x101, &pes);
    }
    for (k, au) in aus.iter().enumerate() {
        let pts = base + tick + ((k as i64 * 1024 - i64::from(priming)) * 90_000) / i64::from(SR);
        let mut frame = filmcraft_aac::AdtsHeader::write(2, 3, 2, au.len(), 0x7FF).to_vec();
        frame.extend_from_slice(au);
        let len = (frame.len() + 8) as u16;
        let mut pes = vec![0, 0, 1, 0xC0, (len >> 8) as u8, len as u8, 0x80, 0x80, 5];
        pes.extend(stamp(2, pts));
        pes.extend(frame);
        m.pes(0x102, &pes);
    }
    std::fs::write(path, m.out).unwrap();
}

/// Normalised correlation of a decoded left channel with the tone from `t0`.
fn correlation(left: &[f32], t0: f64) -> f32 {
    let (mut xy, mut xx, mut yy) = (0f32, 0f32, 0f32);
    for (k, &x) in left.iter().enumerate() {
        let y = tone(t0 + k as f64 / f64::from(SR));
        xy += x * y;
        xx += x * x;
        yy += y * y;
    }
    xy / (xx.sqrt() * yy.sqrt()).max(1e-9)
}

fn check(path: &Path) {
    let mut r = AudioReader::open(path).unwrap();
    assert_eq!(r.info().codec, "aac");
    assert_eq!(r.info().sample_rate, SR);
    assert_eq!(r.info().channels, 2);
    // a trimmed range has exactly its length and lines up with the tone
    let pcm = r.read_range(0.5, Some(1.0)).unwrap();
    assert_eq!(pcm.len(), (SR / 2) as usize * 2, "{}", path.display());
    let left: Vec<f32> = pcm.iter().step_by(2).copied().collect();
    let peak = left.iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!((0.4..0.6).contains(&peak), "peak {peak}");
    // within 1 ms of the tone (Matroska timestamps are whole milliseconds; MP4 and TS are exact)
    let (best, c) =
        (-48..=48).map(|o| (o, correlation(&left, 0.5 + f64::from(o) / f64::from(SR)))).fold((0, -1.0), |a, b| if b.1 > a.1 { b } else { a });
    assert!(c > 0.95, "{}: correlation {c} at {best} samples (timing off)", path.display());
    // seeking back and blocks in order from the target
    let mut last = 0.0;
    let mut n = 0;
    for (i, b) in r.samples_from(1.25).enumerate() {
        let b = b.unwrap();
        if i == 0 {
            assert!((b.time - 1.25).abs() < 1e-3, "first block at {}", b.time);
        } else {
            assert!(b.time > last);
        }
        last = b.time;
        n += b.samples.len() / 2;
    }
    let tail = n as f64 / f64::from(SR);
    assert!((0.7..0.85).contains(&tail), "{tail} s after 1.25 s");
    // the start of the clip
    let head = r.read_range(0.0, Some(0.1)).unwrap();
    assert_eq!(head.len(), 4800 * 2);
    // ranges past the end are short or empty, inverted ones are empty
    assert!(r.read_range(5.0, Some(6.0)).unwrap().is_empty());
    assert!(r.read_range(1.0, Some(0.5)).unwrap().is_empty());
    assert!(r.read_range(f64::NAN, Some(f64::INFINITY)).unwrap().len() >= SR as usize * 2 * 19 / 10);
}

#[test]
fn aac_in_mp4() {
    let p = scratch("tone.mp4");
    mp4_clip(&p);
    check(&p);
}

#[test]
fn aac_in_matroska() {
    let p = scratch("tone.mkv");
    mkv_clip(&p);
    check(&p);
}

#[test]
fn adts_aac_in_transport_stream() {
    let p = scratch("tone.ts");
    ts_clip(&p);
    check(&p);
}

#[test]
fn resampled_tone_keeps_its_pitch() {
    let p = scratch("resample.mp4");
    mp4_clip(&p);
    let pcm = AudioReader::open(&p).unwrap().read_range(0.5, Some(1.0)).unwrap();
    let out = resample(&pcm, 2, SR, 44_100);
    assert_eq!(out.len(), 22_050 * 2);
    // zero crossings of the left channel: 2 per cycle, 220 in 0.5 s at either rate
    let crossings = out.iter().step_by(2).collect::<Vec<_>>().windows(2).filter(|w| (*w[0] < 0.0) != (*w[1] < 0.0)).count();
    assert!((436..=444).contains(&crossings), "{crossings} crossings");
}

#[test]
fn clips_without_audio_are_unsupported() {
    use filmcraft_matroska::{MkvWriter, MuxOptions, TrackKind, TrackSpec};
    let (ps, avcc) = video();
    let mut v = TrackSpec::new(TrackKind::Video, "V_MPEG4/ISO/AVC");
    v.codec_private = avcc;
    v.video_size = Some((W, H));
    let mut w = MkvWriter::new(Cursor::new(Vec::new()), vec![v], MuxOptions::default()).unwrap();
    for p in &ps {
        w.write_frame(0, p.pts * 40_000_000, p.keyframe, &p.data, Some(40_000_000)).unwrap();
    }
    let p = scratch("silent.mkv");
    std::fs::write(&p, w.finish().unwrap().into_inner()).unwrap();
    assert!(matches!(AudioReader::open(&p), Err(VideoError::Unsupported(_))));
    assert!(AudioReader::open(&scratch("missing.mp4")).is_err());
}

fn no_panic(p: &Path) {
    if let Ok(mut r) = AudioReader::open(p) {
        let _ = r.read_range(0.0, Some(0.3));
        let _ = r.read_range(1.5, None);
        for b in r.samples_from(0.9).take(8) {
            let _ = b;
        }
    }
}

/// Truncations and random corruption of each container never panic.
#[test]
fn damaged_clips_never_panic() {
    let mut s = 0x2545_f491_4f6c_dd1du64;
    let mut rnd = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    for (name, make) in [("f.mp4", mp4_clip as fn(&Path)), ("f.mkv", mkv_clip), ("f.ts", ts_clip)] {
        let p = scratch(&format!("good-{name}"));
        make(&p);
        let good = std::fs::read(&p).unwrap();
        let q = scratch(&format!("bad-{name}"));
        for n in (0..good.len()).step_by((good.len() / 25).max(1)) {
            std::fs::write(&q, &good[..n]).unwrap();
            no_panic(&q);
        }
        for _ in 0..40 {
            let mut b = good.clone();
            for _ in 0..1 + rnd() % 16 {
                let i = (rnd() as usize) % b.len();
                b[i] = rnd() as u8;
            }
            std::fs::write(&q, &b).unwrap();
            no_panic(&q);
        }
    }
}

#[test]
fn corpus_clip_audio() {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/video/h264-aac-720p-nps-elk.mp4");
    if !p.exists() {
        eprintln!("skip: {} not present (cargo xtask corpus --download)", p.display());
        return;
    }
    let mut r = AudioReader::open(&p).unwrap();
    assert_eq!(r.info().codec, "aac");
    let (rate, ch) = (r.info().sample_rate, r.info().channels);
    assert!(rate >= 22_050 && (1..=2).contains(&ch), "{rate} Hz {ch} ch");
    let pcm = r.read_range(1.0, Some(3.0)).unwrap();
    assert_eq!(pcm.len(), 2 * rate as usize * ch);
    let rms = (pcm.iter().map(|v| v * v).sum::<f32>() / pcm.len() as f32).sqrt();
    assert!(rms > 1e-4 && pcm.iter().all(|v| v.is_finite() && v.abs() <= 2.0), "rms {rms}");
}
