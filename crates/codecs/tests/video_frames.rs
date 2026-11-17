//! Poster frames and video streams on clips made here: encoded with craft-video's H.264 and HEVC
//! encoders and muxed with craft-media's MP4 writer (no media is committed).
#![cfg(all(feature = "video-frames", not(target_arch = "wasm32")))]

use std::io::Cursor;
use std::sync::atomic::AtomicBool;

use craft_media::isobmff::{
    self, AvcConfig, Brand, ColorInfo, FourCc, HevcConfig, Mp4Writer, PcmConfig, SampleEntry, TrackConfig, WriteSample, WriterOptions,
};
use lightcraft_codecs::poster::{PosterOptions, open_poster, poster_frame};
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
/// top-left quarter (to see the rotation); `noisy` adds a different pattern to every frame, so
/// each coded picture is about as large as a sync picture.
fn planes(w: usize, h: usize, i: usize, noisy: bool) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let noise = |k: usize| {
        let x = (k as u32).wrapping_mul(2_654_435_761).wrapping_add(i as u32 * 40_503) >> 27;
        x as i32 - 16
    };
    let y = (0..w * h).map(|k| if noisy { (i32::from(luma_of(i)) + noise(k)).clamp(0, 255) as u8 } else { luma_of(i) }).collect();
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
    /// An explicit edit list (segment durations in the writer's movie timescale, 1000 per second).
    edits: Vec<isobmff::Edit>,
    /// The last picture's duration in ticks (every other one lasts [`TICK`]).
    last_duration: Option<u32>,
    noisy: bool,
    /// The HEVC encoder signals full range in its VUI.
    vui_full_range: bool,
    /// The movie timescale (the writer's default 1000 when `None`).
    movie_timescale: Option<u32>,
}

impl Clip {
    fn new(w: usize, h: usize, frames: usize) -> Clip {
        Clip {
            w,
            h,
            frames,
            bframes: 0,
            keyint: 250,
            brand: Brand::Mp4,
            colour: None,
            audio: false,
            edits: Vec::new(),
            last_duration: None,
            noisy: false,
            vui_full_range: false,
            movie_timescale: None,
        }
    }
}

fn mux(entry: SampleEntry, samples: &[(Vec<u8>, i64, i64, bool)], c: &Clip, media_start: Option<i64>) -> Vec<u8> {
    let (brand, audio) = (c.brand, c.audio);
    let mut wo = WriterOptions::new(brand);
    if let Some(t) = c.movie_timescale {
        wo.movie_timescale = t;
    }
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), wo).unwrap();
    let mut cfg = TrackConfig::new(entry, FPS * TICK);
    cfg.media_start = media_start;
    cfg.edits.clone_from(&c.edits);
    let v = w.add_track(cfg).unwrap();
    for (k, (data, pts, dts, key)) in samples.iter().enumerate() {
        let off = i32::try_from((pts - dts) * i64::from(TICK)).unwrap();
        let duration = if k + 1 == samples.len() { c.last_duration.unwrap_or(TICK) } else { TICK };
        w.write_sample(v, WriteSample { data, duration, composition_offset: off, is_sync: *key }).unwrap();
    }
    if audio {
        let pcm = PcmConfig { bits: 16, float: false, big_endian: false, signed: true, channels: 2, sample_rate: 48000.0 };
        let a = w.add_track(TrackConfig::new(SampleEntry::pcm(pcm), 48000)).unwrap();
        w.write_sample(a, WriteSample { data: &[0; 400], duration: 0, composition_offset: 0, is_sync: true }).unwrap();
    }
    w.finish_faststart().unwrap().into_inner()
}

fn h264_clip(c: &Clip) -> Vec<u8> {
    let (entry, samples, start) = h264_samples(c);
    mux(entry, &samples, c, start)
}

/// A clip's H.264 sample entry, its samples (data, pts, dts, sync) and the edit start muxers write.
fn h264_samples(c: &Clip) -> (SampleEntry, Vec<(Vec<u8>, i64, i64, bool)>, Option<i64>) {
    use craft_video::h264::{Encoder, EncoderConfig, PacketFormat, RateControl, YuvFrame};
    let mut cfg = EncoderConfig::new(c.w as u32, c.h as u32, FPS, 1);
    cfg.format = PacketFormat::LengthPrefixed;
    cfg.rate = RateControl::Qp(12);
    cfg.bframes = c.bframes;
    cfg.keyint = c.keyint;
    cfg.scenecut = false;
    cfg.threads = 1;
    let mut enc = Encoder::new(cfg).unwrap();
    let mut packets = Vec::new();
    for i in 0..c.frames {
        let (y, u, v) = planes(c.w, c.h, i, c.noisy);
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
    (entry, samples, start)
}

fn hevc_clip(c: &Clip, ten_bit: bool) -> Vec<u8> {
    use craft_video::hevc::{Encoder, EncoderConfig, Frame, Profile, RateControl};
    let mut cfg = EncoderConfig::new(c.w as u32, c.h as u32, FPS, 1);
    cfg.rate = RateControl::ConstantQp(22);
    cfg.keyint = c.keyint;
    cfg.full_range = c.vui_full_range;
    if ten_bit {
        cfg.profile = Profile::Main10;
    }
    let mut enc = Encoder::new(cfg).unwrap();
    let scale = if ten_bit { 4u16 } else { 1 };
    let mut samples = Vec::new();
    for i in 0..c.frames {
        let (y, u, v) = planes(c.w, c.h, i, c.noisy);
        let wide = |p: &[u8]| p.iter().map(|&s| u16::from(s) * scale).collect::<Vec<_>>();
        let (y, u, v) = (wide(&y), wide(&u), wide(&v));
        let p = enc.encode(&Frame { y: &y, u: &u, v: &v, y_stride: c.w, uv_stride: c.w / 2 }).unwrap();
        samples.push((p.data, i as i64, i as i64, p.keyframe));
    }
    let mut entry = SampleEntry::hevc(HevcConfig::parse(&enc.hvcc()).unwrap(), c.w as u16, c.h as u16);
    if let Some(v) = entry.video.as_mut() {
        v.color = c.colour.clone();
    }
    mux(entry, &samples, c, None)
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

/// The picture shows frame `i`: neighbouring frames are 2.33 display levels apart, the clips are
/// coded finely enough (QP 12) that a frame lands within 0.1 of its own level.
fn assert_shows_frame(img: &lightcraft_raster::Rgb32f, i: usize) {
    let got = grey_level(img);
    let want = level_of(luma_of(i));
    assert!((got - want).abs() < 0.6, "shows level {got}, frame {i} has {want}");
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

/// ISO/IEC 14496-12: colour in the `colr` box takes precedence over the bitstream's (AVFoundation
/// reads it so too). The encoder writes BT.709 limited range in the VUI throughout.
#[test]
fn container_colour_wins_over_the_bitstream_and_hdr_is_tone_mapped() {
    let mut c = Clip::new(64, 48, 45);
    c.colour = Some(ColorInfo::Nclx { primaries: 9, transfer: 18, matrix: 9, full_range: false });
    let p = poster(&hevc_clip(&c, true), &opts()).unwrap();
    assert_eq!((p.colour.primaries, p.colour.transfer, p.colour.matrix), (9, 18, 9));
    assert!(p.hdr);
    assert!(p.image.data.iter().flatten().all(|v| v.is_finite() && *v <= 1.0 + 1e-4));
}

#[test]
fn the_bitstream_fills_what_the_container_leaves_unspecified() {
    let resolved = |colour: ColorInfo, vui_full_range: bool| {
        let mut c = Clip::new(64, 48, 10);
        (c.colour, c.vui_full_range) = (Some(colour), vui_full_range);
        let p = poster(&hevc_clip(&c, false), &opts()).unwrap();
        (p.colour.primaries, p.colour.transfer, p.colour.matrix, p.colour.full_range)
    };
    // primaries from the box, transfer and matrix (unspecified there) from the VUI
    assert_eq!(resolved(ColorInfo::Nclx { primaries: 9, transfer: 2, matrix: 2, full_range: false }, false), (9, 1, 1, false));
    // `nclx` carries a range flag: the box's wins either way, never or-ed with the VUI's
    assert_eq!(resolved(ColorInfo::Nclx { primaries: 1, transfer: 1, matrix: 1, full_range: true }, false), (1, 1, 1, true));
    assert_eq!(resolved(ColorInfo::Nclx { primaries: 1, transfer: 1, matrix: 1, full_range: false }, true), (1, 1, 1, false));
    // QuickTime's `nclc` has no range flag: the VUI's stands
    assert_eq!(resolved(ColorInfo::Nclc { primaries: 9, transfer: 1, matrix: 9 }, true), (9, 1, 9, true));
    assert_eq!(resolved(ColorInfo::Nclc { primaries: 9, transfer: 1, matrix: 9 }, false), (9, 1, 9, false));
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
    let bytes = mux(entry, &[(pkt.data, 0, 0, true)], &Clip { brand: Brand::Mov, ..Clip::new(0, 0, 0) }, None);
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
    let bytes = mux(entry, &samples, &Clip::new(0, 0, 0), None);
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
    let bytes = mux(entry, &[(vec![0u8; 100], 0, 0, true)], &Clip { brand: Brand::Mov, ..Clip::new(0, 0, 0) }, None);
    match poster(&bytes, &opts()) {
        Err(VideoError::Unsupported(m)) => assert_eq!(m, "Apple ProRes video can't be decoded yet"),
        r => panic!("{r:?}"),
    }
}

#[test]
fn empty_and_zero_length_clips_are_errors() {
    assert!(matches!(poster(&[], &opts()), Err(VideoError::Malformed(_))));
    let entry = SampleEntry::avc(AvcConfig::new(vec![vec![0x67, 0x64, 0, 0x1e]], vec![vec![0x68, 0xce]], 4), 64, 48);
    let bytes = mux(entry, &[], &Clip::new(0, 0, 0), None);
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

/// Opening a clip for its poster reads its headers through the poster's read budget and gives
/// its picture size; the decode goes on from there, the budget shared, the headers not read again.
#[test]
fn opening_for_the_poster_reads_the_headers_within_the_budget() {
    let bytes = h264_clip(&Clip::new(64, 48, 90));
    let clip = open_poster(bytes.clone(), &opts()).unwrap();
    assert_eq!(clip.picture_size(), (64, 48));
    let headers = clip.bytes_read();
    assert!(headers > 0 && headers < bytes.len() as u64, "{headers}");
    let p = clip.decode(&AtomicBool::new(false)).unwrap();
    assert_shows_frame(&p.image, 30);
    assert_eq!(p.bytes_read, poster(&bytes, &opts()).unwrap().bytes_read, "the headers counted once");
    // headers larger than the budget: refused while opening
    let tight = PosterOptions { max_read_bytes: headers - 1, ..opts() };
    assert!(matches!(open_poster(bytes, &tight).err(), Some(VideoError::TooLarge(_))));
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

/// An edit list that cuts the clip's first pictures (and its end) hides them: the stream starts
/// and ends where the edit does, seeks stay inside it, and the poster comes from what is shown.
/// The cut pictures still decode as references for the first ones shown (YouTube's MP4s start
/// their edit seconds into the media).
#[test]
fn an_edit_list_that_cuts_the_start_and_end_hides_those_pictures() {
    let mut c = Clip::new(64, 48, 120);
    c.keyint = 30;
    // shown: pictures 40..85 (1.5 s); 40 needs its sync picture 30, which is cut
    c.edits = vec![isobmff::Edit { segment_duration: 1500, media_time: 40 * i64::from(TICK), media_rate: 0x0001_0000 }];
    let bytes = h264_clip(&c);
    let mut s = VideoStream::open(bytes.clone(), &StreamOptions::default()).unwrap();
    let info = s.info().clone();
    assert!(info.start_seconds.abs() < 1e-9, "start {}", info.start_seconds);
    assert!((info.duration_seconds - 1.5).abs() < 1e-9, "duration {}", info.duration_seconds);
    // which picture a frame is: its centre luma sample, nearest on the `luma_of` ramp
    let which = |f: &lightcraft_codecs::video_stream::Frame| match &f.planes {
        lightcraft_codecs::video_stream::FramePlanes::U8(p) => p[0]
            .get(f.height / 2 * f.y_stride + f.width / 2)
            .map(|&v| (0..120).min_by_key(|&i| (i32::from(luma_of(i)) - i32::from(v)).abs()).unwrap_or(0)),
        lightcraft_codecs::video_stream::FramePlanes::U16(_) => None,
    };
    let first = s.next_frame().unwrap().unwrap();
    assert!(first.seconds.abs() < 1e-9, "the first picture shown is at {}", first.seconds);
    assert_eq!(which(&first), Some(40), "picture 40 comes first");
    let mut shown = 1;
    let mut last = first;
    while let Some(f) = s.next_frame().unwrap() {
        shown += 1;
        last = f;
    }
    assert_eq!(shown, 45, "only the edit's pictures");
    assert!(s.frames_decoded() <= 85, "nothing past the edit is decoded: {}", s.frames_decoded());
    assert_eq!(which(&last), Some(84));
    for (t, frame) in [(0.5, 55usize), (-3.0, 40), (99.0, 84), (0.0, 40)] {
        s.seek(t, SeekMode::Exact).unwrap();
        let f = s.next_frame().unwrap().unwrap();
        assert_eq!(which(&f), Some(frame), "exact seek to {t}");
        assert!((f.seconds - (frame as f64 - 40.0) / 30.0).abs() < 1e-9, "seek {t}: {}", f.seconds);
    }
    // the sync picture before 0.5 s is cut: a keyframe seek lands on the first picture shown
    s.seek(0.5, SeekMode::Keyframe).unwrap();
    assert_eq!(which(&s.next_frame().unwrap().unwrap()), Some(40));
    // under two seconds shown: the poster is the first picture shown, not the cut picture 0
    let p = poster(&bytes, &opts()).unwrap();
    assert!(p.seconds.abs() < 1e-9, "{}", p.seconds);
    assert_shows_frame(&p.image, 40);

    // an edit that runs to the end (as muxers write it): the poster is one second into what is shown
    let mut c = Clip::new(64, 48, 120);
    c.keyint = 30;
    c.edits = vec![isobmff::Edit { segment_duration: 3000, media_time: 30 * i64::from(TICK), media_rate: 0x0001_0000 }];
    let bytes = h264_clip(&c);
    let s = VideoStream::open(bytes.clone(), &StreamOptions::default()).unwrap();
    assert!((s.info().duration_seconds - 3.0).abs() < 1e-9, "{}", s.info().duration_seconds);
    let p = poster(&bytes, &opts()).unwrap();
    assert!((p.seconds - 1.0).abs() < 1e-9, "{}", p.seconds);
    assert_shows_frame(&p.image, 60);
}

/// An edit list that starts far past the only sync picture: even a keyframe seek would decode
/// every picture up to the first one shown. The poster stops at `max_frames` and shows the latest
/// picture it decoded (a cut one, as a fallback) instead of decoding 301 pictures.
#[test]
fn a_keyframe_seek_to_an_edit_far_past_its_sync_picture_stops_at_max_frames() {
    let mut c = Clip::new(64, 48, 360);
    c.keyint = 1000;
    c.edits = vec![isobmff::Edit { segment_duration: 1500, media_time: 300 * i64::from(TICK), media_rate: 0x0001_0000 }];
    let bytes = h264_clip(&c);
    let p = poster(&bytes, &PosterOptions { max_frames: 240, ..opts() }).unwrap();
    assert!(p.frames_decoded <= 240, "{} pictures decoded", p.frames_decoded);
    assert!(p.fallback);
    assert_shows_frame(&p.image, p.frames_decoded - 1);
    // without the limit, a stream still reaches the first picture shown
    let mut s = VideoStream::open(bytes, &StreamOptions::default()).unwrap();
    s.seek(0.0, SeekMode::Keyframe).unwrap();
    assert!(s.next_frame().unwrap().unwrap().seconds.abs() < 1e-9);
    assert_eq!(s.frames_decoded(), 301);
}

/// A sample with no duration (some muxers write the last one so) is still shown, for the usual
/// picture duration.
#[test]
fn a_last_picture_without_a_duration_is_shown() {
    let mut c = Clip::new(64, 48, 1);
    c.last_duration = Some(0);
    let p = poster(&h264_clip(&c), &opts()).unwrap();
    assert_shows_frame(&p.image, 0);
    let mut c = Clip::new(64, 48, 10);
    c.last_duration = Some(0);
    let mut s = VideoStream::open(h264_clip(&c), &StreamOptions::default()).unwrap();
    assert!((s.info().duration_seconds - 10.0 / 30.0).abs() < 1e-9, "{}", s.info().duration_seconds);
    let mut n = 0;
    while s.next_frame().unwrap().is_some() {
        n += 1;
    }
    assert_eq!(n, 10);
}

/// An edit whose segment rescales to no media ticks (a movie timescale finer than the track's)
/// still shows the picture at its start.
#[test]
fn an_edit_shorter_than_a_tick_still_shows_a_picture() {
    let mut c = Clip::new(64, 48, 10);
    // one microsecond at the track's 30000 ticks per second is 0.03 ticks
    c.movie_timescale = Some(1_000_000);
    c.edits = vec![isobmff::Edit { segment_duration: 1, media_time: 9 * i64::from(TICK), media_rate: 0x0001_0000 }];
    let mut s = VideoStream::open(h264_clip(&c), &StreamOptions::default()).unwrap();
    assert!(s.next_frame().unwrap().is_some());
}

/// The sample-bytes budget alone (pictures within `max_frames`, but more bytes to the target than
/// half the read budget) shows the sync picture.
#[test]
fn too_many_bytes_to_the_target_shows_the_sync_picture() {
    let mut c = Clip::new(64, 48, 60);
    c.noisy = true;
    let bytes = h264_clip(&c);
    let f = isobmff::open(bytes.as_slice()).unwrap();
    let to_target: u64 = f.tracks[0].samples[..=30].iter().map(|s| u64::from(s.size)).sum();
    let full = poster(&bytes, &opts()).unwrap();
    assert!(!full.fallback && full.frames_decoded == 31);
    // half the read budget is one byte short of the run, yet the header and sync picture fit
    let p = poster(&bytes, &PosterOptions { max_read_bytes: 2 * (to_target - 1), ..opts() }).unwrap();
    assert!(p.fallback);
    assert_eq!((p.seconds, p.frames_decoded), (0.0, 1));
}

/// An open GOP: pictures that follow a sync picture in decode order but are shown before it need
/// the previous group, so a target among them decodes from the sync picture and shows it. (A
/// sample table crafted from a B-frame clip, with a P picture marked sync: craft-video's encoders
/// write closed GOPs only.)
#[test]
fn an_open_gop_leading_picture_seeks_to_its_sync_picture() {
    let mut c = Clip::new(64, 48, 30);
    c.bframes = 2;
    c.keyint = 1000;
    let (entry, mut samples, start) = h264_samples(&c);
    // when sample `k` is shown, in frames from the first picture shown (as `mux` writes them)
    let shown = |k: usize| k as i64 + samples[k].1 - samples[k].2;
    let first = (0..samples.len()).map(shown).min().unwrap();
    let at = |k: usize| (shown(k) - first) as f64 / 30.0;
    // the first P picture after the IDR, with B pictures after it in decode order shown before it
    let p = (1..samples.len() - 1).find(|&k| shown(k + 1) < shown(k)).unwrap();
    let (sync_at, leading_at) = (at(p), at(p + 1));
    samples[p].3 = true;
    let bytes = mux(entry, &samples, &c, start);
    let s = VideoStream::open(bytes, &StreamOptions::default()).unwrap();
    let plan = s.seek_plan(leading_at).unwrap();
    assert!((plan.keyframe_seconds - sync_at).abs() < 1e-9, "{plan:?}");
    assert!((plan.target_seconds - plan.keyframe_seconds).abs() < 1e-9, "the sync picture stands in: {plan:?}");
    assert_eq!(plan.frames, 1);
}

/// A picture over the conversion budget converts straight to a smaller one (block means in linear
/// light), never to a full-size float copy.
#[test]
fn a_picture_over_the_conversion_budget_converts_smaller() {
    let bytes = h264_clip(&Clip::new(128, 96, 40));
    let full = poster(&bytes, &opts()).unwrap();
    assert_eq!((full.image.width, full.image.height), (128, 96));
    let p = poster(&bytes, &PosterOptions { max_convert_pixels: 64 * 48, ..opts() }).unwrap();
    assert_eq!((p.image.width, p.image.height), (64, 48));
    assert_shows_frame(&p.image, 0);
    let p = poster(&bytes, &PosterOptions { max_convert_pixels: 64 * 48 - 1, ..opts() }).unwrap();
    assert_eq!((p.image.width, p.image.height), (43, 32), "factor 3, partial blocks at the edges");
}

/// A file with no movie header that is a valid clip of a kind not read here (an AVCHD camcorder's
/// MPEG transport stream, `.mts` / `.m2ts`, or a bare H.264 / HEVC stream with no container) is
/// unsupported, not a damaged clip; anything else without a movie header still is damaged.
#[test]
fn a_transport_stream_or_a_bare_stream_is_unsupported_not_damaged() {
    let packets = |prefix: usize| -> Vec<u8> {
        (0..8)
            .flat_map(|i| {
                let mut p = vec![0u8; prefix + 188];
                p[..prefix].fill(i as u8);
                p[prefix] = 0x47;
                p
            })
            .collect()
    };
    let annex_b = [0u8, 0, 0, 1, 0x09, 0x10, 0, 0, 0, 1, 0x67, 0x64, 0, 0x28].repeat(4);
    let short_start_code = [0u8, 0, 1, 0x40, 0x01, 0x0c, 0x01].repeat(4);
    for (what, bytes, says) in [
        ("transport stream", packets(0), "transport stream"),
        ("timestamped transport stream", packets(4), "transport stream"),
        ("bare stream", annex_b.clone(), "container"),
        ("bare stream, 3-byte start code", short_start_code, "container"),
    ] {
        match VideoStream::open(bytes, &StreamOptions::default()) {
            Err(VideoError::Unsupported(m)) => assert!(m.contains(says), "{what}: {m}"),
            other => panic!("{what}: {:?}", other.err()),
        }
    }
    // (one sync byte, or a start code past the beginning, is no evidence)
    let mut junk = vec![0x55u8; 600];
    junk[0] = 0x47;
    junk[300..304].copy_from_slice(&[0, 0, 0, 1]);
    assert!(matches!(VideoStream::open(junk, &StreamOptions::default()), Err(VideoError::Malformed(_))));
    assert!(matches!(VideoStream::open(Vec::new(), &StreamOptions::default()), Err(VideoError::Malformed(_) | VideoError::Io(_))));
}

/// A clip's decoder gets the memory its sequence header asks for, not a fixed amount: an 8K HEVC
/// clip on 8 decoder threads (some 2 GB of pictures, over the 1 GiB a decoder had) decodes within
/// the default ceiling, and the stream and the poster say what it may hold. Over the ceiling the
/// clip is too large to decode: unsupported, not damaged.
#[test]
fn the_decoder_gets_the_memory_the_clip_asks_for_within_a_ceiling() {
    use lightcraft_codecs::video_stream::reason;
    let bytes = hevc_clip(&Clip::new(7680, 4320, 1), false);
    let opts = StreamOptions { threads: 8, ..StreamOptions::default() };
    let mut s = VideoStream::open(bytes.clone(), &opts).unwrap();
    let need = s.info().decoder_bytes;
    // (whole pictures of 16-bit 4:2:0 samples)
    let picture = 7680 * 4320 * 3;
    assert!(need > 1 << 30 && need <= opts.memory_budget && need.is_multiple_of(picture), "{need}");
    s.seek(0.0, SeekMode::Exact).unwrap();
    let f = s.next_frame().unwrap().unwrap();
    assert_eq!((f.width, f.height), (7680, 4320));
    match VideoStream::open(bytes.clone(), &StreamOptions { memory_budget: need - 1, ..opts }) {
        Err(VideoError::Unsupported(m)) => assert_eq!(m, reason::TOO_LARGE_TO_DECODE),
        other => panic!("{:?}", other.err()),
    }
    let o = PosterOptions { max_edge: 64, stream: opts, ..PosterOptions::default() };
    let clip = open_poster(bytes, &o).unwrap();
    assert_eq!(clip.decoder_bytes(), need, "the poster weighs what the decoder may hold");
    assert!(clip.decode(&AtomicBool::new(false)).is_ok());
}
