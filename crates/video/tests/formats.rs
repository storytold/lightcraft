//! Synthetic clips in every container / intra codec FilmCraft can write, generated in-test:
//! ProRes, DNxHR and APV in MOV/MP4, ProRes and DNxHR in MXF, H.264 (with B-frames) in Matroska
//! and in an MPEG transport stream. Each frame k is a flat grey of known level, so a decoded
//! frame identifies itself; seeking, frame times and probe fields are checked per container, and
//! truncated / corrupted copies never panic.
#![cfg(not(target_arch = "wasm32"))]

use std::io::Cursor;
use std::path::{Path, PathBuf};

use filmcraft_isobmff::{Brand, FourCc, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use lcc_video::{FrameReader, VideoError, poster, probe};

const FRAMES: usize = 12;
const W: u32 = 64;
const H: u32 = 48;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lcc-video-formats-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir.join(name)
}

fn save(name: &str, bytes: &[u8]) -> PathBuf {
    let p = scratch(name);
    std::fs::write(&p, bytes).expect("write clip");
    p
}

/// 8-bit video-range luma of frame k (distinct, well inside the range).
fn level8(k: usize) -> u16 {
    40 + 12 * k as u16
}

/// Frame index from a decoded frame's mean green (inverting sRGB and the video range).
fn which(img: &lcc_raster::Rgb32f) -> usize {
    let n = img.data.len().max(1) as f32;
    let g = img.data.iter().map(|p| p[1]).sum::<f32>() / n;
    let v = if g <= 0.0031308 { g * 12.92 } else { 1.055 * g.powf(1.0 / 2.4) - 0.055 };
    let y = 16.0 + 219.0 * v;
    ((y - 40.0) / 12.0).round().max(0.0) as usize
}

fn prores_frames() -> Vec<Vec<u8>> {
    use filmcraft_prores::{ChromaFormat, Encoder, Frame, Profile};
    (0..FRAMES)
        .map(|k| {
            let mut f = Frame::new(W, H, ChromaFormat::Yuv422, 10, false);
            f.y.iter_mut().for_each(|y| *y = level8(k) << 2);
            Encoder::new(Profile::Hq, W, H).encode(&f).expect("prores")
        })
        .collect()
}

fn dnx_frames() -> Vec<Vec<u8>> {
    use filmcraft_dnx::{ChromaFormat, Encoder, Frame, Profile};
    let mut enc = Encoder::new(Profile::Hq, W, H).expect("dnx encoder");
    (0..FRAMES)
        .map(|k| {
            let mut f = Frame::new(W, H, ChromaFormat::Yuv422, 8, false);
            f.y.iter_mut().for_each(|y| *y = level8(k));
            enc.encode(&f).expect("dnx")
        })
        .collect()
}

fn apv_frames() -> Vec<Vec<u8>> {
    use filmcraft_apv::{ChromaFormat, Encoder, Frame, Profile};
    let mut enc = Encoder::new(Profile::P422_10, W, H).expect("apv encoder");
    (0..FRAMES)
        .map(|k| {
            let mut f = Frame::new(W, H, ChromaFormat::Yuv422, 10, false);
            f.y.iter_mut().for_each(|y| *y = level8(k) << 2);
            enc.encode(&f).expect("apv")
        })
        .collect()
}

/// Intra frames at 25 fps in a MOV/MP4 track.
fn mp4_intra(entry: SampleEntry, frames: &[Vec<u8>]) -> Vec<u8> {
    let mut mux = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).expect("writer");
    let t = mux.add_track(TrackConfig::new(entry, 25)).expect("track");
    for d in frames {
        mux.write_sample(t, WriteSample { data: d, duration: 1, composition_offset: 0, is_sync: true }).expect("sample");
    }
    mux.finish().expect("finish").into_inner()
}

fn mxf_intra(coding: filmcraft_mxf::PictureCoding, frames: &[Vec<u8>]) -> Vec<u8> {
    use filmcraft_mxf::{FrameInfo, MxfWriter, PackageIds, Pattern, PictureDesc, Rational, WriterConfig};
    let mut cfg = WriterConfig::new(Pattern::Op1a, Rational::new(25, 1), PackageIds::from_seed("test", "clip"));
    cfg.picture = Some(PictureDesc::new(coding, W, H));
    let mut w = MxfWriter::new(Cursor::new(Vec::new()), cfg).expect("mxf writer");
    for d in frames {
        w.push_picture(d, FrameInfo::intra()).expect("picture");
    }
    w.finish().expect("finish").into_inner()
}

/// H.264 packets (decode order) of FRAMES flat frames at 25 fps, keyint 5, 2 B-frames; pts in
/// frame units. Returns (packets, avcC).
fn h264_packets(format: filmcraft_h264enc::PacketFormat) -> (Vec<filmcraft_h264enc::Packet>, Vec<u8>) {
    use filmcraft_h264enc::{Encoder, EncoderConfig, RateControl, YuvFrame};
    let mut cfg = EncoderConfig::new(W, H, 25, 1);
    cfg.format = format;
    cfg.bframes = 2;
    cfg.keyint = 5;
    cfg.scenecut = false;
    cfg.rate = RateControl::Crf(12.0);
    cfg.threads = 1;
    let mut enc = Encoder::new(cfg).expect("encoder");
    let (w, h) = (W as usize, H as usize);
    let mut packets = Vec::new();
    for k in 0..FRAMES {
        let y = vec![level8(k) as u8; w * h];
        let c = vec![128u8; w * h / 4];
        let f = YuvFrame { y: &y, u: &c, v: &c, y_stride: w, uv_stride: w / 2 };
        packets.extend(enc.encode(&f, k as i64).expect("encode"));
    }
    packets.extend(enc.flush());
    (packets, enc.avcc())
}

fn mkv_h264() -> Vec<u8> {
    use filmcraft_matroska::{MkvWriter, MuxOptions, TrackKind, TrackSpec};
    let (packets, avcc) = h264_packets(filmcraft_h264enc::PacketFormat::LengthPrefixed);
    let mut spec = TrackSpec::new(TrackKind::Video, "V_MPEG4/ISO/AVC");
    spec.codec_private = avcc;
    spec.video_size = Some((W, H));
    spec.default_duration_ns = Some(40_000_000);
    let mut w = MkvWriter::new(Cursor::new(Vec::new()), vec![spec], MuxOptions::default()).expect("mkv writer");
    for p in &packets {
        w.write_frame(0, p.pts * 40_000_000, p.keyframe, &p.data, Some(40_000_000)).expect("frame");
    }
    w.finish().expect("finish").into_inner()
}

/// A minimal MPEG-2 transport stream: PAT, PMT and PES packets of one H.264 stream (PID 0x101).
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
        assert_eq!(p.len(), 188);
        self.out.extend(p);
        n
    }

    fn section(&mut self, pid: u16, body: &[u8]) {
        let mut payload = vec![0];
        payload.extend_from_slice(body);
        payload.extend(crc32_mpeg(body).to_be_bytes());
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

fn crc32_mpeg(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
    }
    crc
}

fn ts_stamp(prefix: u8, v: i64) -> [u8; 5] {
    [
        (prefix << 4) | (((v >> 30) & 7) as u8) << 1 | 1,
        (v >> 22) as u8,
        (((v >> 15) & 0x7F) as u8) << 1 | 1,
        (v >> 7) as u8,
        ((v & 0x7F) as u8) << 1 | 1,
    ]
}

fn ts_h264() -> Vec<u8> {
    let (packets, _) = h264_packets(filmcraft_h264enc::PacketFormat::AnnexB);
    let mut m = TsMux { out: Vec::new(), cc: [0; 0x2000] };
    m.section(0, &[0x00, 0xB0, 13, 0, 1, 0xC1, 0, 0, 0, 1, 0xE1, 0x00]);
    // PMT: PCR PID 0x101, one stream of type 0x1B (H.264) on PID 0x101
    m.section(0x100, &[0x02, 0xB0, 18, 0, 1, 0xC1, 0, 0, 0xE1, 0x01, 0xF0, 0, 0x1B, 0xE1, 0x01, 0xF0, 0]);
    // dts runs two frames behind (B-frame delay); 90 kHz, 3600 per frame, offset 1 s
    for (i, p) in packets.iter().enumerate() {
        let pts = 90_000 + p.pts * 3600 + 7200;
        let dts = 90_000 + i as i64 * 3600;
        let mut pes = vec![0, 0, 1, 0xE0, 0, 0, 0x80, 0xC0, 10];
        pes.extend(ts_stamp(3, pts));
        pes.extend(ts_stamp(1, dts));
        pes.extend_from_slice(&p.data);
        m.pes(0x101, &pes);
    }
    m.out
}

/// Probe, frame times, seeking in random order and sequential play on a FRAMES-frame 25 fps clip.
fn check(name: &str, path: &Path, codec: &str) {
    let info = probe(path).unwrap_or_else(|e| panic!("{name}: probe: {e}"));
    assert_eq!(info.codec, codec, "{name}");
    assert_eq!((info.width, info.height), (W, H), "{name}");
    assert_eq!((info.fps_num, info.fps_den), (25, 1), "{name}");
    assert!((info.duration - FRAMES as f64 / 25.0).abs() < 0.05, "{name}: duration {}", info.duration);
    assert!(info.decodable && !info.has_audio, "{name}");
    let mut rd = FrameReader::open(path).unwrap_or_else(|e| panic!("{name}: open: {e}"));
    let times = rd.frame_times();
    assert_eq!(times.len(), FRAMES, "{name}: {times:?}");
    for (k, t) in times.iter().enumerate() {
        assert!((t - k as f64 / 25.0).abs() < 1e-3, "{name}: frame {k} at {t}");
    }
    for k in [7usize, 2, 11, 0, 5, 4, 6, 9] {
        let f = rd.frame_at(k as f64 / 25.0 + 0.01, None).unwrap_or_else(|e| panic!("{name}: frame {k}: {e}"));
        assert_eq!(which(&f.image), k, "{name}: seek to {k}");
        assert!((f.time - k as f64 / 25.0).abs() < 1e-3, "{name}: time {}", f.time);
        assert_eq!((f.image.width, f.image.height), (W as usize, H as usize));
    }
    for k in 0..FRAMES {
        let f = rd.frame_at(k as f64 / 25.0, Some(16)).unwrap_or_else(|e| panic!("{name}: play {k}: {e}"));
        assert_eq!(which(&f.image), k, "{name}: play {k}");
        assert!(f.image.width <= 16);
    }
    assert_eq!(which(&rd.frame_at(99.0, None).expect("end").image), FRAMES - 1, "{name}");
    assert!(poster(path, None, 32).is_ok(), "{name}");
}

fn no_panic(path: &Path) {
    let _ = probe(path);
    if let Ok(mut r) = FrameReader::open(path) {
        let _ = r.frame_at(0.0, Some(16));
        let _ = r.frame_at(0.3, None);
        let _ = r.frame_at(0.1, None);
    }
}

/// Truncations and random byte corruption of a clip never panic.
fn fuzz(name: &str, good: &[u8]) {
    let p = scratch(&format!("fuzz-{name}"));
    let mut cuts: Vec<usize> = (0..good.len()).step_by((good.len() / 60).max(1)).collect();
    cuts.extend([1, 4, 188, 189, good.len() - 1]);
    for n in cuts {
        std::fs::write(&p, &good[..n.min(good.len())]).expect("write");
        no_panic(&p);
    }
    let mut s = 0x2545_f491_4f6c_dd1du64;
    let mut rnd = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    for _ in 0..150 {
        let mut b = good.to_vec();
        for _ in 0..1 + rnd() % 8 {
            let i = (rnd() as usize) % b.len();
            b[i] = rnd() as u8;
        }
        std::fs::write(&p, &b).expect("write");
        no_panic(&p);
    }
}

#[test]
fn prores_in_mov() {
    let b = mp4_intra(SampleEntry::prores(FourCc(*b"apch"), W as u16, H as u16), &prores_frames());
    check("prores.mov", &save("prores.mov", &b), "prores");
    fuzz("prores.mov", &b);
}

#[test]
fn dnxhr_in_mov() {
    let b = mp4_intra(SampleEntry::dnx(FourCc(*b"AVdh"), W as u16, H as u16), &dnx_frames());
    check("dnx.mov", &save("dnx.mov", &b), "dnxhd");
}

#[test]
fn apv_in_mp4() {
    let b = mp4_intra(SampleEntry::apv(Default::default(), W as u16, H as u16), &apv_frames());
    check("apv.mp4", &save("apv.mp4", &b), "apv");
}

#[test]
fn prores_and_dnxhr_in_mxf() {
    let b = mxf_intra(filmcraft_mxf::PictureCoding::ProRes { profile: 4 }, &prores_frames());
    check("prores.mxf", &save("prores.mxf", &b), "prores");
    fuzz("prores.mxf", &b);
    let b = mxf_intra(filmcraft_mxf::PictureCoding::Vc3 { cid: 1272 }, &dnx_frames());
    check("dnx.mxf", &save("dnx.mxf", &b), "dnxhd");
}

#[test]
fn h264_in_matroska() {
    let b = mkv_h264();
    check("h264.mkv", &save("h264.mkv", &b), "h264");
    fuzz("h264.mkv", &b);
}

#[test]
fn h264_in_transport_stream() {
    let b = ts_h264();
    check("h264.ts", &save("h264.ts", &b), "h264");
    fuzz("h264.ts", &b);
}

#[test]
fn unknown_and_empty_streams_are_errors() {
    // a TS with only a PAT/PMT and no PES is invalid, never a panic
    let mut m = TsMux { out: Vec::new(), cc: [0; 0x2000] };
    m.section(0, &[0x00, 0xB0, 13, 0, 1, 0xC1, 0, 0, 0, 1, 0xE1, 0x00]);
    m.section(0x100, &[0x02, 0xB0, 18, 0, 1, 0xC1, 0, 0, 0xE1, 0x01, 0xF0, 0, 0x1B, 0xE1, 0x01, 0xF0, 0]);
    let p = save("empty.ts", &m.out);
    assert!(probe(&p).is_err() || probe(&p).is_ok_and(|i| !i.decodable));
    assert!(FrameReader::open(&p).is_err());
    // an EBML header with nothing after it
    let p = save("empty.mkv", &[0x1A, 0x45, 0xDF, 0xA3, 0x80]);
    assert!(matches!(probe(&p), Err(VideoError::Invalid(_))));
}
