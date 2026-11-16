//! Poster frames and video streams on clips made here: encoded with craft-video's H.264 and HEVC
//! encoders and muxed with craft-media's MP4 writer (no media is committed).
#![cfg(all(feature = "video-frames", not(target_arch = "wasm32")))]

use std::io::Cursor;
use std::sync::atomic::AtomicBool;

use craft_media::isobmff::{
    self, AvcConfig, Brand, ColorInfo, FourCc, HevcConfig, Mp4Writer, PcmConfig, SampleEntry, TrackConfig, WriteSample, WriterOptions,
};
use lightcraft_codecs::poster::{PosterOptions, poster_frame};
use lightcraft_codecs::video_stream::{Codec, SeekMode, StreamOptions, VideoError, VideoStream};
use proptest::prelude::*;

const FPS: u32 = 30;
/// Media ticks per frame (timescale FPS × TICK).
const TICK: u32 = 1000;

/// Luma of frame `i`: a ramp, so the frame shown tells which picture was picked.
fn luma_of(i: usize) -> u8 {
    (20 + 2 * i).min(230) as u8
}

/// Planes of frame `i` (8-bit 4:2:0): flat grey of [`luma_of`], with a red patch in the
/// top-left quarter (to see the rotation).
fn planes(w: usize, h: usize, i: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let y = vec![luma_of(i); w * h];
    let (cw, ch) = (w / 2, h / 2);
    let cb = vec![128u8; cw * ch];
    let cr: Vec<u8> = (0..cw * ch).map(|k| if k % cw < cw / 2 && k / cw < ch / 2 { 220 } else { 128 }).collect();
    (y, cb, cr)
}

struct Clip {
    w: usize,
    h: usize,
    frames: usize,
    bframes: u8,
    keyint: u32,
    brand: Brand,
    colour: Option<ColorInfo>,
    audio: bool,
}

impl Clip {
    fn new(w: usize, h: usize, frames: usize) -> Clip {
        Clip { w, h, frames, bframes: 0, keyint: 250, brand: Brand::Mp4, colour: None, audio: false }
    }
}

fn mux(entry: SampleEntry, samples: &[(Vec<u8>, i64, i64, bool)], brand: Brand, media_start: Option<i64>, audio: bool) -> Vec<u8> {
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(brand)).unwrap();
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

fn h264_clip(c: &Clip) -> Vec<u8> {
    use craft_video::h264::{Encoder, EncoderConfig, PacketFormat, RateControl, YuvFrame};
    let mut cfg = EncoderConfig::new(c.w as u32, c.h as u32, FPS, 1);
    cfg.format = PacketFormat::LengthPrefixed;
    cfg.rate = RateControl::Qp(20);
    cfg.bframes = c.bframes;
    cfg.keyint = c.keyint;
    cfg.scenecut = false;
    cfg.threads = 1;
    let mut enc = Encoder::new(cfg).unwrap();
    let mut packets = Vec::new();
    for i in 0..c.frames {
        let (y, u, v) = planes(c.w, c.h, i);
        let f = YuvFrame { y: &y, u: &u, v: &v, y_stride: c.w, uv_stride: c.w / 2 };
        packets.extend(enc.encode(&f, i as i64).unwrap());
    }
    packets.extend(enc.flush());
    let mut entry = SampleEntry::avc(AvcConfig::parse(&enc.avcc()).unwrap(), c.w as u16, c.h as u16);
    if let Some(v) = entry.video.as_mut() {
        v.color = c.colour.clone();
    }
    let samples: Vec<_> = packets.into_iter().map(|p| (p.data, p.pts, p.dts, p.keyframe)).collect();
    // B-frames delay the first picture shown: an edit list starts the track there, as muxers do
    let start = samples.iter().enumerate().map(|(k, s)| k as i64 + s.1 - s.2).min().filter(|&d| d != 0).map(|d| d * i64::from(TICK));
    mux(entry, &samples, c.brand, start, c.audio)
}

fn hevc_clip(c: &Clip, ten_bit: bool) -> Vec<u8> {
    use craft_video::hevc::{Encoder, EncoderConfig, Frame, Profile, RateControl};
    let mut cfg = EncoderConfig::new(c.w as u32, c.h as u32, FPS, 1);
    cfg.rate = RateControl::ConstantQp(22);
    cfg.keyint = c.keyint;
    if ten_bit {
        cfg.profile = Profile::Main10;
    }
    let mut enc = Encoder::new(cfg).unwrap();
    let scale = if ten_bit { 4u16 } else { 1 };
    let mut samples = Vec::new();
    for i in 0..c.frames {
        let (y, u, v) = planes(c.w, c.h, i);
        let wide = |p: &[u8]| p.iter().map(|&s| u16::from(s) * scale).collect::<Vec<_>>();
        let (y, u, v) = (wide(&y), wide(&u), wide(&v));
        let p = enc.encode(&Frame { y: &y, u: &u, v: &v, y_stride: c.w, uv_stride: c.w / 2 }).unwrap();
        samples.push((p.data, i as i64, i as i64, p.keyframe));
    }
    let mut entry = SampleEntry::hevc(HevcConfig::parse(&enc.hvcc()).unwrap(), c.w as u16, c.h as u16);
    if let Some(v) = entry.video.as_mut() {
        v.color = c.colour.clone();
    }
    mux(entry, &samples, c.brand, None, c.audio)
}

fn opts() -> PosterOptions {
    PosterOptions { max_edge: 0, ..PosterOptions::default() }
}

fn poster(bytes: &[u8], o: &PosterOptions) -> Result<lightcraft_codecs::poster::PosterFrame, VideoError> {
    poster_frame(bytes.to_vec(), o, &AtomicBool::new(false))
}

/// Mean display value (sRGB 0..255) of the picture's green channel over its bottom-right quarter
/// (away from the red patch).
fn grey_level(img: &lightcraft_raster::Rgb32f) -> f32 {
    let (w, h) = (img.width, img.height);
    let mut sum = 0.0;
    let mut n = 0.0;
    for y in h / 2..h {
        for x in w / 2..w {
            sum += lightcraft_color::transfer::linear_to_srgb(img.data[y * w + x][1]);
            n += 1.0;
        }
    }
    sum / n * 255.0
}

/// The display level limited-range luma `y` has (grey: R = G = B).
fn level_of(y: u8) -> f32 {
    (f32::from(y) - 16.0) / 219.0 * 255.0
}

fn assert_shows_frame(img: &lightcraft_raster::Rgb32f, i: usize) {
    let got = grey_level(img);
    let want = level_of(luma_of(i));
    assert!((got - want).abs() < 2.5, "shows level {got}, frame {i} has {want}");
}

#[test]
fn h264_poster_is_the_picture_one_second_in() {
    let bytes = h264_clip(&Clip::new(64, 48, 90));
    let p = poster(&bytes, &opts()).unwrap();
    assert_eq!(p.codec, Codec::H264);
    assert_eq!((p.image.width, p.image.height), (64, 48));
    assert!((p.seconds - 1.0).abs() < 1e-9, "{}", p.seconds);
    assert!(!p.fallback && !p.hdr && p.bit_depth == 8);
    assert_eq!(p.frames_decoded, 31, "from the IDR at 0 through frame 30");
    assert_shows_frame(&p.image, 30);
    assert!(p.bytes_read > 0 && p.bytes_read < bytes.len() as u64 + 1);
}

#[test]
fn b_frames_are_reordered_and_the_target_picked_by_time() {
    let mut c = Clip::new(64, 48, 90);
    c.bframes = 2;
    c.keyint = 15;
    let bytes = h264_clip(&c);
    let p = poster(&bytes, &opts()).unwrap();
    assert!((p.seconds - 1.0).abs() < 1e-9, "{}", p.seconds);
    assert!(!p.fallback);
    assert_shows_frame(&p.image, 30);
    // the IDR at frame 30 starts the run: a target on a sync picture decodes that one alone
    // (B-frames reference only within the group)
    assert!(p.frames_decoded <= 3, "{}", p.frames_decoded);
    // every frame comes out once, in presentation order, through the edit list's start
    let mut s = VideoStream::open(bytes.clone(), &StreamOptions::default()).unwrap();
    assert_eq!(s.info().start_seconds, 0.0);
    let mut seen = Vec::new();
    while let Some(f) = s.next_frame().unwrap() {
        seen.push(f.seconds);
    }
    assert_eq!(seen.len(), 90);
    assert!(seen.windows(2).all(|w| w[0] < w[1]), "presentation order");
}

#[test]
fn a_target_between_sync_pictures_decodes_from_the_one_before() {
    let mut c = Clip::new(64, 48, 90);
    c.bframes = 2;
    c.keyint = 20;
    let bytes = h264_clip(&c);
    let p = poster(&bytes, &opts()).unwrap();
    assert_shows_frame(&p.image, 30);
    assert!(p.frames_decoded >= 11, "IDR at 20 through 30: {}", p.frames_decoded);
}

#[test]
fn too_many_pictures_to_the_target_shows_the_sync_picture() {
    let bytes = h264_clip(&Clip::new(64, 48, 90));
    let p = poster(&bytes, &PosterOptions { max_frames: 5, ..opts() }).unwrap();
    assert!(p.fallback);
    assert_eq!(p.seconds, 0.0);
    assert_eq!(p.frames_decoded, 1);
    assert_shows_frame(&p.image, 0);
}

#[test]
fn a_short_clip_shows_its_first_picture() {
    let bytes = h264_clip(&Clip::new(64, 48, 40));
    let p = poster(&bytes, &opts()).unwrap();
    assert_eq!(p.seconds, 0.0);
    assert_shows_frame(&p.image, 0);
}

/// Writes the track header matrix of the first `tkhd` (a quarter turn per `rot`).
fn set_rotation(bytes: &mut [u8], rot: u8) {
    let at = bytes.windows(4).position(|w| w == b"tkhd").unwrap();
    let version = bytes[at + 4];
    let matrix = at - 4 + if version == 1 { 60 } else { 48 };
    let (a, b, c, d): (i32, i32, i32, i32) = match rot {
        1 => (0, 0x10000, -0x10000, 0),
        2 => (-0x10000, 0, 0, -0x10000),
        3 => (0, -0x10000, 0x10000, 0),
        _ => (0x10000, 0, 0, 0x10000),
    };
    for (k, v) in [(0, a), (4, b), (12, c), (16, d)] {
        bytes[matrix + k..matrix + k + 4].copy_from_slice(&v.to_be_bytes());
    }
}

fn redness(img: &lightcraft_raster::Rgb32f, x: usize, y: usize) -> f32 {
    let p = img.data[y * img.width + x];
    p[0] - p[1]
}

#[test]
fn a_rotated_clip_turns_upright() {
    let mut bytes = h264_clip(&Clip::new(64, 48, 10));
    set_rotation(&mut bytes, 1);
    let s = VideoStream::open(bytes.clone(), &StreamOptions::default()).unwrap();
    assert_eq!(s.info().orientation, lightcraft_geom::Orientation::Rotate90);
    let p = poster(&bytes, &opts()).unwrap();
    assert_eq!((p.image.width, p.image.height), (48, 64), "portrait after a quarter turn");
    // the stored top-left patch is now top-right
    assert!(redness(&p.image, 40, 8) > 0.2, "{}", redness(&p.image, 40, 8));
    assert!(redness(&p.image, 8, 8) < 0.05);
    for (rot, o) in [(2, lightcraft_geom::Orientation::Rotate180), (3, lightcraft_geom::Orientation::Rotate270)] {
        set_rotation(&mut bytes, rot);
        assert_eq!(VideoStream::open(bytes.clone(), &StreamOptions::default()).unwrap().info().orientation, o);
    }
}

#[test]
fn hevc_8_and_10_bit_posters() {
    let bytes = hevc_clip(&Clip::new(64, 48, 75), false);
    let p = poster(&bytes, &opts()).unwrap();
    assert_eq!((p.codec, p.bit_depth), (Codec::Hevc, 8));
    assert_shows_frame(&p.image, 30);
    let mut c = Clip::new(64, 48, 75);
    c.brand = Brand::Mov;
    let bytes = hevc_clip(&c, true);
    let p = poster(&bytes, &opts()).unwrap();
    assert_eq!((p.codec, p.bit_depth), (Codec::Hevc, 10));
    assert_shows_frame(&p.image, 30);
}

#[test]
fn container_colour_fills_what_the_stream_leaves_open_and_hdr_is_tone_mapped() {
    let mut c = Clip::new(64, 48, 45);
    // the encoder writes BT.709 in the VUI; the container's HLG transfer must not override it
    c.colour = Some(ColorInfo::Nclx { primaries: 9, transfer: 18, matrix: 9, full_range: false });
    let p = poster(&hevc_clip(&c, true), &opts()).unwrap();
    assert_eq!((p.colour.primaries, p.colour.transfer, p.colour.matrix), (1, 1, 1));
    assert!(!p.hdr);
}

#[test]
fn hdr_vui_is_tone_mapped() {
    use craft_video::hevc::{Encoder, EncoderConfig, Frame, Profile};
    let mut cfg = EncoderConfig::new(64, 48, FPS, 1);
    cfg.profile = Profile::Main10;
    (cfg.colour_primaries, cfg.transfer_characteristics, cfg.matrix_coeffs) = (9, 18, 9);
    let mut enc = Encoder::new(cfg).unwrap();
    let (y, c) = (vec![900u16; 64 * 48], vec![512u16; 32 * 24]);
    let pkt = enc.encode(&Frame { y: &y, u: &c, v: &c, y_stride: 64, uv_stride: 32 }).unwrap();
    let entry = SampleEntry::hevc(HevcConfig::parse(&enc.hvcc()).unwrap(), 64, 48);
    let bytes = mux(entry, &[(pkt.data, 0, 0, true)], Brand::Mov, None, false);
    let p = poster(&bytes, &opts()).unwrap();
    assert!(p.hdr);
    assert_eq!(p.colour.transfer, 18);
    assert!(p.image.data.iter().flatten().all(|v| v.is_finite() && *v <= 1.0 + 1e-4));
}

#[test]
fn ten_bit_h264_is_unsupported_with_a_reason() {
    use craft_video::h264::{Encoder, EncoderConfig, PacketFormat, Profile, YuvFrame16};
    let mut cfg = EncoderConfig::new(64, 48, FPS, 1);
    (cfg.format, cfg.profile, cfg.bit_depth, cfg.bframes, cfg.threads) = (PacketFormat::LengthPrefixed, Profile::High10, 10, 0, 1);
    let mut enc = Encoder::new(cfg).unwrap();
    let (y, c) = (vec![400u16; 64 * 48], vec![512u16; 32 * 24]);
    let mut pk = enc.encode16(&YuvFrame16 { y: &y, u: &c, v: &c, y_stride: 64, uv_stride: 32 }, 0).unwrap();
    pk.extend(enc.flush());
    let entry = SampleEntry::avc(AvcConfig::parse(&enc.avcc()).unwrap(), 64, 48);
    let samples: Vec<_> = pk.into_iter().map(|p| (p.data, p.pts, p.dts, p.keyframe)).collect();
    let bytes = mux(entry, &samples, Brand::Mp4, None, false);
    match poster(&bytes, &opts()) {
        Err(VideoError::Unsupported(m)) => assert!(m.contains("H.264"), "{m}"),
        // a future decoder that handles it is fine too
        Ok(p) => assert_eq!(p.bit_depth, 10),
        Err(e) => panic!("expected unsupported, got {e:?}"),
    }
}

#[test]
fn an_unsupported_codec_says_which() {
    let entry = SampleEntry::prores(FourCc(*b"apcn"), 64, 48);
    let bytes = mux(entry, &[(vec![0u8; 100], 0, 0, true)], Brand::Mov, None, false);
    match poster(&bytes, &opts()) {
        Err(VideoError::Unsupported(m)) => assert_eq!(m, "Apple ProRes video can't be decoded yet"),
        r => panic!("{r:?}"),
    }
}

#[test]
fn empty_and_zero_length_clips_are_errors() {
    assert!(matches!(poster(&[], &opts()), Err(VideoError::Malformed(_))));
    let entry = SampleEntry::avc(AvcConfig::new(vec![vec![0x67, 0x64, 0, 0x1e]], vec![vec![0x68, 0xce]], 4), 64, 48);
    let bytes = mux(entry, &[], Brand::Mp4, None, false);
    match poster(&bytes, &opts()) {
        Err(VideoError::Malformed(m)) => assert!(m.contains("no pictures"), "{m}"),
        r => panic!("{r:?}"),
    }
    // an audio-only file has no video track
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
    let pcm = PcmConfig { bits: 16, float: false, big_endian: false, signed: true, channels: 1, sample_rate: 8000.0 };
    let a = w.add_track(TrackConfig::new(SampleEntry::pcm(pcm), 8000)).unwrap();
    w.write_sample(a, WriteSample { data: &[0; 40], duration: 0, composition_offset: 0, is_sync: true }).unwrap();
    let bytes = w.finish().unwrap().into_inner();
    assert!(matches!(poster(&bytes, &opts()), Err(VideoError::Unsupported(m)) if m.contains("no video track")));
}

#[test]
fn a_clip_cut_short_shows_the_last_picture_that_decoded() {
    let bytes = h264_clip(&Clip::new(64, 48, 90));
    let f = isobmff::open(bytes.as_slice()).unwrap();
    // cut inside picture 20: the target (30) is gone, 0..=19 decode
    let cut = f.tracks[0].samples[20].offset as usize + 3;
    let p = poster(&bytes[..cut], &opts()).unwrap();
    assert!(p.fallback);
    assert!((p.seconds - 19.0 / 30.0).abs() < 1e-9, "{}", p.seconds);
    assert_shows_frame(&p.image, 19);
    // cut inside the first picture: nothing to show
    let cut = f.tracks[0].samples[0].offset as usize + 3;
    assert!(poster(&bytes[..cut], &opts()).is_err());
}

#[test]
fn a_read_budget_and_cancel_stop_the_work() {
    let bytes = h264_clip(&Clip::new(64, 48, 90));
    assert!(matches!(poster(&bytes, &PosterOptions { max_read_bytes: 64, ..opts() }), Err(VideoError::TooLarge(_))));
    assert!(matches!(poster_frame(bytes.clone(), &opts(), &AtomicBool::new(true)), Err(VideoError::Cancelled)));
    // a sample over the per-picture limit
    let o = PosterOptions { stream: StreamOptions { max_sample_bytes: 4, ..StreamOptions::default() }, ..opts() };
    assert!(matches!(poster(&bytes, &o), Err(VideoError::TooLarge(_))));
}

#[test]
fn the_poster_fits_max_edge() {
    let bytes = h264_clip(&Clip::new(128, 96, 10));
    let p = poster(&bytes, &PosterOptions { max_edge: 32, ..opts() }).unwrap();
    assert_eq!((p.image.width, p.image.height), (32, 24));
}

#[test]
fn hostile_avcc_is_an_error_not_a_panic() {
    let clean = h264_clip(&Clip::new(64, 48, 10));
    let at = clean.windows(4).position(|w| w == b"avcC").unwrap() + 4;
    // SPS count, SPS length, then garbage in the SPS and a PPS count past the end
    for (k, v) in [(5usize, 0x1fu8), (6, 0xff), (7, 0xff), (9, 0x00), (12, 0xff), (4, 0xff)] {
        let mut bytes = clean.clone();
        bytes[at + k] = v;
        let _ = poster(&bytes, &opts());
    }
    // every byte of the record flipped in turn
    for k in 0..40 {
        let mut bytes = clean.clone();
        bytes[at + k] ^= 0xa5;
        let _ = poster(&bytes, &opts());
    }
}

#[test]
fn stream_info_seeks_and_audio() {
    let mut c = Clip::new(64, 48, 60);
    c.audio = true;
    c.bframes = 1;
    let bytes = h264_clip(&c);
    let mut s = VideoStream::open(bytes.clone(), &StreamOptions::default()).unwrap();
    let info = s.info().clone();
    assert_eq!((info.width, info.height, info.sample_count), (64, 48, 60));
    assert!((info.frame_rate.unwrap() - 30.0).abs() < 1e-9);
    assert!((info.duration_seconds - 2.0).abs() < 1e-9);
    let audio = info.audio.unwrap();
    assert_eq!((audio.codec, audio.channels), ("pcm", 2));
    assert_eq!(audio.sample_rate, 48000.0);
    // seek back and forth on one stream
    for (t, frame) in [(1.0, 30usize), (0.2, 6), (1.5, 45), (0.0, 0), (99.0, 59), (-5.0, 0)] {
        s.seek(t, SeekMode::Exact).unwrap();
        let f = s.next_frame().unwrap().unwrap();
        assert!((f.seconds - frame as f64 / 30.0).abs() < 1e-9, "seek {t}: {}", f.seconds);
        let n = s.next_frame().unwrap();
        assert_eq!(n.is_some(), frame < 59, "frames follow the target");
    }
    s.seek(1.0, SeekMode::Keyframe).unwrap();
    assert_eq!(s.next_frame().unwrap().unwrap().seconds, 0.0);
    let plan = s.seek_plan(1.0).unwrap();
    assert_eq!((plan.keyframe_seconds, plan.target_seconds), (0.0, 1.0));
}

#[test]
fn hevc_streams_seek_repeatedly() {
    let mut c = Clip::new(64, 48, 40);
    c.keyint = 10;
    let bytes = hevc_clip(&c, false);
    let mut s = VideoStream::open(bytes.clone(), &StreamOptions::default()).unwrap();
    for t in [1.0, 0.1, 1.2, 0.5] {
        s.seek(t, SeekMode::Exact).unwrap();
        let f = s.next_frame().unwrap().unwrap();
        assert!((f.seconds - (t * 30.0).round() / 30.0).abs() < 1e-9);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Any bytes changed anywhere in a clip: an answer, never a panic.
    #[test]
    fn corrupted_clips_never_panic(flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..8), cut in any::<prop::sample::Index>(), truncate in any::<bool>()) {
        static CLIP: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        let clean = CLIP.get_or_init(|| { let mut c = Clip::new(32, 32, 12); c.bframes = 2; h264_clip(&c) });
        let mut bytes = clean.clone();
        for (i, v) in flips {
            let k = i.index(bytes.len());
            bytes[k] ^= v;
        }
        if truncate {
            bytes.truncate(cut.index(bytes.len()));
        }
        let _ = poster(&bytes, &PosterOptions { max_frames: 20, ..opts() });
        if let Ok(mut s) = VideoStream::open(bytes.clone(), &StreamOptions::default()) {
            let _ = s.seek(0.3, SeekMode::Exact);
            for _ in 0..20 {
                if let Ok(None) = s.next_frame() {
                    break;
                }
            }
        }
    }
}

#[test]
fn a_track_without_an_edit_list_counts_from_its_first_picture() {
    // B-frames shift the first picture shown to one frame in; without an edit list the poster's
    // "one second in" counts from there
    let mut c = Clip::new(64, 48, 90);
    c.bframes = 1;
    let mut bytes = h264_clip(&c);
    // drop the edit list: rename `edts` to a box readers skip
    let at = bytes.windows(4).position(|w| w == b"edts").unwrap();
    bytes[at..at + 4].copy_from_slice(b"free");
    let s = VideoStream::open(bytes.clone(), &StreamOptions::default()).unwrap();
    assert!((s.info().start_seconds - 1.0 / 30.0).abs() < 1e-9);
    let p = poster(&bytes, &opts()).unwrap();
    assert!((p.seconds - 31.0 / 30.0).abs() < 1e-9, "{}", p.seconds);
    assert_shows_frame(&p.image, 30);
}

#[test]
fn streams_go_to_a_decode_thread() {
    fn send<T: Send>() {}
    send::<VideoStream>();
    assert!(lightcraft_codecs::video_stream::video_decoding_available());
    // a file read in place
    let dir = std::env::temp_dir().join(format!("lc-video-frames-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clip.mp4");
    std::fs::write(&path, h264_clip(&Clip::new(64, 48, 40))).unwrap();
    let src = lightcraft_codecs::video_stream::FileSource::open(&path).unwrap();
    let p = poster_frame(src, &opts(), &AtomicBool::new(false)).unwrap();
    assert_eq!(p.seconds, 0.0);
    std::fs::remove_dir_all(&dir).unwrap();
}
