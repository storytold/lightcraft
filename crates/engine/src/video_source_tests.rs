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

/// An all-intra H.264 clip of `frames` flat grey pictures of `w × h` (big enough to be halved).
fn h264_big(frames: usize, w: usize, h: usize) -> Vec<u8> {
    use craft_video::h264::{Encoder, EncoderConfig, PacketFormat, RateControl, YuvFrame};
    let mut cfg = EncoderConfig::new(w as u32, h as u32, FPS, 1);
    cfg.format = PacketFormat::LengthPrefixed;
    cfg.rate = RateControl::Qp(30);
    cfg.bframes = 0;
    cfg.keyint = 1;
    cfg.scenecut = false;
    cfg.threads = 1;
    let mut enc = Encoder::new(cfg).unwrap();
    let (y, c) = (vec![120u8; w * h], vec![128u8; w * h / 4]);
    let mut packets = Vec::new();
    for i in 0..frames {
        packets.extend(enc.encode(&YuvFrame { y: &y, u: &c, v: &c, y_stride: w, uv_stride: w / 2 }, i as i64).unwrap());
    }
    packets.extend(enc.flush());
    let entry = SampleEntry::avc(AvcConfig::parse(&enc.avcc()).unwrap(), w as u16, h as u16);
    let samples: Vec<_> = packets.into_iter().map(|p| (p.data, p.pts, p.dts, p.keyframe)).collect();
    mux(entry, &samples, None, false)
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
            // a plain reason the UI translates, the technical detail in the log
            Err(e) => assert!(crate::files::video_reasons().contains(&e.as_str()), "{e}"),
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

#[test]
fn video_source_halves_only_while_playback_falls_behind() {
    let frame = 1.0 / 30.0;
    let mut a = Adapt::default();
    // keeping up: full size, however slow a frame looked
    a.after(0.010, 0.030, false, frame);
    assert!(!a.half);
    // the player drops late frames: half size at once
    a.set_behind(true);
    assert!(a.half);
    // still behind: stays half
    for _ in 0..100 {
        a.after(0.005, 0.004, true, frame);
    }
    assert!(a.half);
    // caught up, and a full-size conversion (4 × 4 ms) would fit: back to full after 30 frames
    a.set_behind(false);
    for _ in 0..29 {
        a.after(0.005, 0.004, true, frame);
    }
    assert!(a.half);
    a.after(0.005, 0.004, true, frame);
    assert!(!a.half, "{a:?}");
    // full size falls behind again: half, and twice as long before the next try
    a.set_behind(true);
    a.set_behind(false);
    for _ in 0..59 {
        a.after(0.005, 0.004, true, frame);
    }
    assert!(a.half);
    a.after(0.005, 0.004, true, frame);
    assert!(!a.half);
}

#[test]
fn video_source_stays_half_only_while_it_helps() {
    let frame = 1.0 / 30.0;
    // the conversion is the slow part: half size takes 8 ms, full would take about 32 ms, more
    // than 60 % of 33 ms: stays half
    let mut a = Adapt::default();
    a.set_behind(true);
    a.set_behind(false);
    for _ in 0..1000 {
        a.after(0.005, 0.008, true, frame);
    }
    assert!(a.half);
    // the decoder is the slow part (30 ms; a full conversion 8 ms): half size can't rescue
    // playback, so the full picture comes back at once
    let mut b = Adapt::default();
    b.set_behind(true);
    assert!(b.half);
    b.after(0.030, 0.002, true, frame);
    assert!(!b.half, "{b:?}");
    // and falling behind again doesn't halve while the decoder is what's slow
    b.after(0.030, 0.008, false, frame);
    b.set_behind(true);
    assert!(!b.half, "{b:?}");
    // once the conversion is the slow part again, it does
    b.after(0.010, 0.040, false, frame);
    b.set_behind(true);
    assert!(b.half);
    // the wait is bounded however often full size fails
    let mut c = Adapt::default();
    for _ in 0..40 {
        c.set_behind(true);
        c.set_behind(false);
        for _ in 0..FULL_AFTER_MAX {
            c.after(0.0, 0.001, true, frame);
        }
    }
    assert_eq!(c.need, FULL_AFTER_MAX);
}

fn convert(draft: bool, half: bool, max_side: usize) -> Convert {
    let adapt = Adapt { half, ..Adapt::default() };
    Convert { converter: None, draft, adapt, max_side, half: [Vec::new(), Vec::new(), Vec::new()], start: 0.0 }
}

#[test]
fn video_source_shows_paused_stepped_and_scrubbed_frames_full_size() {
    let (w, h) = (3840, 2160);
    // paused, stepped or scrubbed (not draft): full size even while the adaptive rule says half
    assert_eq!(convert(false, true, 0).size(w, h), Size { halve: false, draft: false });
    // playing and falling behind: half size, marked draft so a pause replaces it
    assert_eq!(convert(true, true, 0).size(w, h), Size { halve: true, draft: true });
    // playing and keeping up: full size
    assert_eq!(convert(true, false, 0).size(w, h), Size { halve: false, draft: false });
    // small pictures are never halved for speed
    assert_eq!(convert(true, true, 0).size(640, 360), Size { halve: false, draft: false });
    // bigger than the GPU's textures: halved whatever happens, and not a draft (it is the best
    // this machine can show)
    assert_eq!(convert(false, false, 2048).size(w, h), Size { halve: true, draft: false });
    assert_eq!(convert(true, true, 2048).size(w, h), Size { halve: true, draft: false });
    assert_eq!(convert(false, false, 4096).size(w, h), Size { halve: false, draft: false });
}

#[test]
fn video_source_fits_frames_to_the_gpu_or_says_why() {
    if !decoder_here() {
        return;
    }
    let mut src = open(h264_clip(6, 3, false)).unwrap();
    src.set_max_side(40);
    let Next::Frame(f) = src.next_frame(&mut || false, Vec::new()).unwrap() else { panic!("no frame") };
    assert_eq!((f.width as usize, f.height as usize, f.draft), (W / 2, H / 2, false));
    // even half size is too big: a reason, not a frame the GPU would refuse
    let mut src = open(h264_clip(6, 3, false)).unwrap();
    src.set_max_side(20);
    let e = src.next_frame(&mut || false, Vec::new()).unwrap_err();
    assert!(e.contains("more than this graphics card can show"), "{e}");
}

#[test]
fn video_source_marks_half_size_frames_draft_and_shows_paused_ones_full() {
    if !decoder_here() {
        return;
    }
    let (w, h) = (1280, 800);
    let mut src = open(h264_big(3, w, h)).unwrap();
    // playing and falling behind: half size, a draft the pause replaces
    src.set_draft(true);
    src.set_behind(true);
    let Next::Frame(f) = src.next_frame(&mut || false, Vec::new()).unwrap() else { panic!("no frame") };
    assert_eq!((f.width as usize, f.height as usize, f.draft), (w / 2, h / 2, true));
    // paused on it: the same picture again, full size and exact
    src.set_draft(false);
    src.seek(0.0).unwrap();
    let Next::Frame(f) = src.next_frame(&mut || false, Vec::new()).unwrap() else { panic!("no frame") };
    assert_eq!((f.width as usize, f.height as usize, f.draft), (w, h, false));
}

#[test]
fn a_clip_that_cannot_be_opened_says_why_without_its_path() {
    // (a name in the temp folder nobody creates: nothing is written)
    let path = std::env::temp_dir().join(format!("lc-gone-clip-{}.mp4", std::process::id()));
    let path = path.to_string_lossy();
    let Err(e) = open_file(&path) else { panic!("a missing clip opened") };
    // the poster card's wording (translated in every catalog), never the path
    assert_eq!(e, "the clip's file is missing");
    assert!(!e.contains("gone-clip"), "{e}");
}

#[test]
fn video_source_halves_again_while_behind_once_the_conversion_is_slow_again() {
    let frame = 1.0 / 30.0;
    let mut a = Adapt::default();
    a.set_behind(true);
    assert!(a.half);
    // the decoder is the slow part: full size at once
    a.after(0.030, 0.002, true, frame);
    assert!(!a.half);
    // still behind (the player says so only when it changes): timings close to each other keep
    // full size, so the size doesn't switch every frame
    a.after(0.020, 0.024, false, frame);
    assert!(!a.half, "{a:?}");
    // the conversion is clearly the slow part again: half size, with the longer wait to go back
    a.after(0.010, 0.040, false, frame);
    assert!(a.half, "{a:?}");
    assert_eq!(a.need, FULL_AFTER * 2);
    // not behind: full size stays full whatever the timings
    let mut b = Adapt::default();
    b.after(0.010, 0.040, false, frame);
    assert!(!b.half);
}

/// An 8K clip on playback's decoder threads asks for more picture memory than the fixed 768 MiB
/// playback once gave its decoder: it plays, the decoder holding what the clip asks for, up to the
/// codecs' ceiling. Found with an 8K drone clip that played as "the clip is damaged".
#[test]
fn video_source_plays_an_8k_clip_on_playbacks_decoder_threads() {
    if !decoder_here() {
        return;
    }
    let mut src = open_with(h264_big(1, 7680, 4320), &options(PLAYBACK_THREADS)).unwrap_or_else(|e| panic!("the 8K clip was refused: {e}"));
    assert!(src.info().decoder_bytes > 768 << 20, "the case this guards: {} MiB", src.info().decoder_bytes >> 20);
    assert_eq!(all_frames(&mut src).len(), 1);
}

/// A clip whose decoder would need more than the ceiling is too large to decode, never damaged.
#[test]
fn video_source_says_a_clip_over_the_ceiling_is_too_large_not_damaged() {
    if !decoder_here() {
        return;
    }
    let opts = StreamOptions { memory_budget: 64 << 20, ..options(PLAYBACK_THREADS) };
    let e = open_with(h264_big(1, 7680, 4320), &opts).err();
    assert_eq!(e.as_deref(), Some(lightcraft_codecs::video_stream::reason::TOO_LARGE_TO_DECODE));
}
