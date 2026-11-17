//! Playback of clips made here: encoded with craft-video's H.264 and HEVC encoders and muxed with
//! craft-media's MP4 writer (no media is committed). They run with the codecs' `video-frames`
//! feature (`cargo xtask ci` runs them so) and skip without it.
#![cfg(not(target_arch = "wasm32"))]

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use craft_media::isobmff::{AvcConfig, Brand, HevcConfig, Mp4Writer, PcmConfig, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use lightcraft_catalog::PhotoId;

use super::*;
use crate::video_play::{Clock, VideoPlayer};

const FPS: u32 = 30;
const TICK: u32 = 1000;
const W: usize = 64;
const H: usize = 48;

/// Luma of frame `i`: a ramp, so a frame's grey tells which picture it is.
fn luma_of(i: usize) -> u8 {
    (16 + 5 * i).min(235) as u8
}

fn planes(i: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    (vec![luma_of(i); W * H], vec![128; W * H / 4], vec![128; W * H / 4])
}

fn mux(entry: SampleEntry, samples: &[(Vec<u8>, i64, i64, bool)], media_start: Option<i64>, audio: bool) -> Vec<u8> {
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
    let mut cfg = TrackConfig::new(entry, FPS * TICK);
    cfg.media_start = media_start;
    let v = w.add_track(cfg).unwrap();
    for (data, pts, dts, key) in samples {
        let off = i32::try_from((pts - dts) * i64::from(TICK)).unwrap();
        w.write_sample(v, WriteSample { data, duration: TICK, composition_offset: off, is_sync: *key }).unwrap();
    }
    if audio {
        let pcm = PcmConfig { bits: 16, float: false, big_endian: false, signed: true, channels: 2, sample_rate: 48000.0 };
        let a = w.add_track(TrackConfig::new(SampleEntry::pcm(pcm), 48000)).unwrap();
        w.write_sample(a, WriteSample { data: &[0; 400], duration: 0, composition_offset: 0, is_sync: true }).unwrap();
    }
    w.finish_faststart().unwrap().into_inner()
}

/// An H.264 clip of `frames` pictures, a sync picture every `keyint`, with B-frames.
fn h264_clip(frames: usize, keyint: u32, audio: bool) -> Vec<u8> {
    use craft_video::h264::{Encoder, EncoderConfig, PacketFormat, RateControl, YuvFrame};
    let mut cfg = EncoderConfig::new(W as u32, H as u32, FPS, 1);
    cfg.format = PacketFormat::LengthPrefixed;
    cfg.rate = RateControl::Qp(18);
    cfg.bframes = 2;
    cfg.keyint = keyint;
    cfg.scenecut = false;
    cfg.threads = 1;
    let mut enc = Encoder::new(cfg).unwrap();
    let mut packets = Vec::new();
    for i in 0..frames {
        let (y, u, v) = planes(i);
        packets.extend(enc.encode(&YuvFrame { y: &y, u: &u, v: &v, y_stride: W, uv_stride: W / 2 }, i as i64).unwrap());
    }
    packets.extend(enc.flush());
    let entry = SampleEntry::avc(AvcConfig::parse(&enc.avcc()).unwrap(), W as u16, H as u16);
    let samples: Vec<_> = packets.into_iter().map(|p| (p.data, p.pts, p.dts, p.keyframe)).collect();
    // B-frames delay the first picture shown: an edit list starts the track there, as muxers do
    let start = samples.iter().enumerate().map(|(k, s)| k as i64 + s.1 - s.2).min().filter(|&d| d != 0).map(|d| d * i64::from(TICK));
    mux(entry, &samples, start, audio)
}

/// An HEVC Main 10 clip.
fn hevc10_clip(frames: usize, keyint: u32) -> Vec<u8> {
    use craft_video::hevc::{Encoder, EncoderConfig, Frame, Profile, RateControl};
    let mut cfg = EncoderConfig::new(W as u32, H as u32, FPS, 1);
    cfg.rate = RateControl::ConstantQp(20);
    cfg.keyint = keyint;
    cfg.profile = Profile::Main10;
    let mut enc = Encoder::new(cfg).unwrap();
    let mut samples = Vec::new();
    for i in 0..frames {
        let (y, u, v) = planes(i);
        let wide = |p: &[u8]| p.iter().map(|&s| u16::from(s) * 4).collect::<Vec<_>>();
        let (y, u, v) = (wide(&y), wide(&u), wide(&v));
        let p = enc.encode(&Frame { y: &y, u: &u, v: &v, y_stride: W, uv_stride: W / 2 }).unwrap();
        samples.push((p.data, i as i64, i as i64, p.keyframe));
    }
    let entry = SampleEntry::hevc(HevcConfig::parse(&enc.hvcc()).unwrap(), W as u16, H as u16);
    mux(entry, &samples, None, false)
}

/// Skip (and say so) without the decoder.
fn decoder_here() -> bool {
    if !available() {
        lightcraft_testkit::skip!(Feature, "no video decoding in this build (lightcraft-codecs/video-frames)");
    }
    available()
}

/// Every frame of a source, in order: (time, grey level at the centre).
fn all_frames(src: &mut dyn FrameSource) -> Vec<(f64, u8)> {
    let mut out = Vec::new();
    loop {
        match src.next_frame(&mut || false, Vec::new()).unwrap() {
            Next::Frame(f) => out.push((f.pts, grey(&f))),
            Next::End => return out,
            Next::Stopped => panic!("stopped without a stop"),
        }
    }
}

fn grey(f: &VideoFrame) -> u8 {
    let i = ((f.height as usize / 2) * f.width as usize + f.width as usize / 2) * 4;
    f.rgba[i + 1]
}

#[derive(Default)]
struct ManualClock(AtomicU64);

impl Clock for ManualClock {
    fn now(&self) -> f64 {
        self.0.load(Ordering::SeqCst) as f64 / 1e6
    }
}

fn wait(p: &mut VideoPlayer, done: impl Fn(&VideoPlayer) -> bool) {
    for _ in 0..10_000 {
        p.tick();
        if done(p) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("timed out: {}", p.state());
}

#[test]
fn video_source_without_the_decoder_says_why() {
    if available() {
        return;
    }
    let e = open(h264_clip(4, 30, false)).err().unwrap();
    assert!(e.contains("no video decoding"), "{e}");
}

#[test]
fn video_source_h264_with_b_frames_in_presentation_order() {
    if !decoder_here() {
        return;
    }
    let mut src = open(h264_clip(20, 10, false)).unwrap();
    let info = src.info().clone();
    assert_eq!((info.width, info.height, info.codec.as_str()), (W as u32, H as u32, "H.264"));
    assert!((info.frame_duration - 1.0 / 30.0).abs() < 1e-6);
    assert!((info.duration - 20.0 / 30.0).abs() < 1e-3, "{}", info.duration);
    assert_eq!(info.audio, None);
    let frames = all_frames(&mut src);
    assert_eq!(frames.len(), 20);
    // starts at 0 (the edit list's delay is taken off), times rise by one frame, greys rise
    assert!(frames[0].0.abs() < 1e-6, "{}", frames[0].0);
    for w in frames.windows(2) {
        assert!((w[1].0 - w[0].0 - 1.0 / 30.0).abs() < 1e-6);
        assert!(w[1].1 > w[0].1, "{frames:?}");
    }
}

#[test]
fn video_source_seek_lands_on_the_frame_shown_at_a_time() {
    if !decoder_here() {
        return;
    }
    let clip = h264_clip(40, 12, false);
    let reference = all_frames(&mut open(clip.clone()).unwrap());
    let mut p = VideoPlayer::start(PhotoId(1), Box::new(open(clip).unwrap()), Arc::new(ManualClock::default())).unwrap();
    for k in [0usize, 13, 25, 39, 7] {
        // a little into frame k: still frame k
        p.seek(reference[k].0 + 0.01);
        wait(&mut p, |p| p.frame().is_some_and(|(f, _)| (f.pts - reference[k].0).abs() < 1e-6));
        let (f, _) = p.frame().unwrap();
        assert_eq!(grey(f), reference[k].1, "frame {k}");
        assert!(!f.draft);
    }
    // stepping back across a sync picture (frame 12) decodes from the one before
    p.seek(reference[12].0);
    wait(&mut p, |p| p.frame().is_some_and(|(f, _)| (f.pts - reference[12].0).abs() < 1e-6));
    p.step(-1);
    wait(&mut p, |p| p.frame().is_some_and(|(f, _)| (f.pts - reference[11].0).abs() < 1e-6));
    assert_eq!(grey(p.frame().unwrap().0), reference[11].1);
}

#[test]
fn video_source_plays_a_clip_to_the_end() {
    if !decoder_here() {
        return;
    }
    let clock = Arc::new(ManualClock::default());
    let mut p = VideoPlayer::start(PhotoId(1), Box::new(open(h264_clip(30, 10, false)).unwrap()), clock.clone()).unwrap();
    wait(&mut p, |p| p.frame().is_some());
    p.play();
    for _ in 0..200 {
        clock.0.fetch_add(1_000_000 / 30, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(2));
        if p.tick().is_none() {
            break;
        }
    }
    assert!(!p.playing(), "{}", p.state());
    assert_eq!(p.state()["ended"], true);
    assert!(p.error().is_none());
    let shown = p.stats().shown + p.stats().dropped;
    assert!(shown >= 29, "{}", p.state());
}

#[test]
fn video_source_hevc_main10() {
    if !decoder_here() {
        return;
    }
    let mut src = open(hevc10_clip(12, 6)).unwrap();
    assert_eq!(src.info().codec, "HEVC");
    let frames = all_frames(&mut src);
    assert_eq!(frames.len(), 12);
    for w in frames.windows(2) {
        assert!(w[1].1 > w[0].1, "{frames:?}");
    }
}

#[test]
fn video_source_describes_audio_it_does_not_play() {
    if !decoder_here() {
        return;
    }
    let src = open(h264_clip(4, 30, true)).unwrap();
    assert_eq!(src.info().audio.as_deref(), Some("PCM, 48 kHz, stereo"));
}

#[test]
fn video_source_damaged_clips_never_panic() {
    if !decoder_here() {
        return;
    }
    let clip = h264_clip(30, 10, false);
    // cut short: the pictures that are there play, then the clip ends or reports the damage
    for keep in [clip.len() * 3 / 4, clip.len() / 2, 200, 16, 0] {
        let Some(bytes) = clip.get(..keep) else { continue };
        match open(bytes.to_vec()) {
            Err(e) => assert!(!e.is_empty()),
            Ok(src) => {
                let mut p = VideoPlayer::start(PhotoId(1), Box::new(src), Arc::new(ManualClock::default())).unwrap();
                p.seek(0.5);
                for _ in 0..200 {
                    p.tick();
                    std::thread::sleep(Duration::from_millis(1));
                }
                let _ = p.state();
            }
        }
    }
}

#[test]
fn video_source_halves_big_pictures_by_averaging() {
    use lightcraft_codecs::yuv::{Chroma, Picture, Plane, Samples};
    // 5 × 3 luma (odd on both sides), 4:2:0 chroma 3 × 2, 10-bit
    let y: Vec<u16> = (0..15).map(|i| i * 10).collect();
    let cb: Vec<u16> = vec![100, 200, 300, 400, 500, 600];
    let cr: Vec<u16> = vec![512; 6];
    fn plane(s: &[u16], stride: usize) -> Plane<'_> {
        Plane { samples: Samples::U16(s), stride }
    }
    let pic = Picture { width: 5, height: 3, bit_depth: 10, chroma: Chroma::C420, y: plane(&y, 5), cb: plane(&cb, 3), cr: plane(&cr, 3) };
    let mut planes = [Vec::new(), Vec::new(), Vec::new()];
    let half = half_size(&pic, &mut planes);
    assert_eq!((half.width, half.height, half.bit_depth), (2, 1, 10));
    // (0 + 10 + 50 + 60) / 4 = 30; (20 + 30 + 70 + 80) / 4 = 50
    assert_eq!(planes[0], vec![30, 50]);
    // half picture 2 × 1 at 4:2:0: chroma 1 × 1, the average of the first 2 × 2 chroma samples
    assert_eq!(planes[1], vec![(100 + 200 + 400 + 500 + 2) / 4]);
    assert_eq!(planes[2], vec![512]);
    // 8-bit samples come out as u16 the converter takes
    let y8: Vec<u8> = vec![16; 16];
    let c8: Vec<u8> = vec![128; 4];
    let p8 = Picture {
        width: 4,
        height: 4,
        bit_depth: 8,
        chroma: Chroma::C420,
        y: Plane { samples: Samples::U8(&y8), stride: 4 },
        cb: Plane { samples: Samples::U8(&c8), stride: 2 },
        cr: Plane { samples: Samples::U8(&c8), stride: 2 },
    };
    let half = half_size(&p8, &mut planes);
    let conv = Converter::new(ColourDescription { primaries: 1, transfer: 1, matrix: 1, full_range: false }, 8, 4, 4).unwrap();
    let mut out = vec![[0u8; 4]; half.width * half.height];
    conv.to_srgb8(&half, &mut out).unwrap();
    assert_eq!(out, vec![[0, 0, 0, 255]; 4], "video black stays black");
}

#[test]
fn video_source_keeps_small_pictures_full_size_while_playing() {
    if !decoder_here() {
        return;
    }
    let clip = h264_clip(6, 3, false);
    let mut src = open(clip).unwrap();
    src.set_draft(true);
    let Next::Frame(f) = src.next_frame(&mut || false, Vec::new()).unwrap() else { panic!("no frame") };
    assert_eq!((f.width as usize, f.height as usize), (W, H), "small pictures are never halved");
}
