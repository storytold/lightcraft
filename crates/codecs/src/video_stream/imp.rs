//! The decoding half of [`super`]: demuxing with craft-media, decoding with craft-video.

use std::collections::VecDeque;

use craft_media::isobmff::{self, ByteSource, CodecConfig, ColorInfo, Mp4File, TrackKind};
use craft_video::{h264, hevc};

use super::{
    AudioTrackInfo, ClipSource, Codec, Frame, FramePlanes, SeekMode, SeekPlan, StreamInfo, StreamOptions, VideoError, matrix_orientation, reason,
};
use crate::yuv::{self, Chroma, ColourDescription};

/// A [`ClipSource`] as the demuxer reads it.
struct Src(Box<dyn ClipSource>);

impl ByteSource for Src {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.0.read_at(offset, buf)
    }
}

fn demux_error(e: isobmff::Error) -> VideoError {
    match e {
        isobmff::Error::Io(e) => VideoError::Io(e.to_string()),
        isobmff::Error::Unsupported(m) if m.contains("cmov") => VideoError::Unsupported(reason::COMPRESSED_HEADER.into()),
        isobmff::Error::Unsupported(_) => VideoError::Unsupported(reason::OTHER_CONTAINER.into()),
        isobmff::Error::TooLarge(m) => VideoError::TooLarge(m.to_string()),
        isobmff::Error::NoMoov => VideoError::Malformed("no movie header (moov box)".into()),
        e => VideoError::Malformed(e.to_string()),
    }
}

/// Bytes [`not_a_movie`] looks at.
const SNIFF_BYTES: usize = 4096;

/// Why a file that isn't read as a movie has no pictures here, when it is a valid clip of a kind
/// not read (an AVCHD camcorder's MPEG transport stream, a bare H.264 / HEVC stream): unsupported,
/// not damaged. `None` for anything else (the container's own error stands). Told by its first
/// bytes, whatever the container reader made of them (an `.m2ts` packet's timestamp can read as a
/// box too small for its header):
/// - a transport stream: the sync byte at the start of three consecutive 188-byte packets, or of
///   three 192-byte ones after the 4-byte timestamp `.m2ts` puts before each;
/// - a bare stream: a start code at the very start, then a valid H.264 or HEVC NAL unit header
///   and a second start code; not when bytes 4 to 8 read as a box type (a box of 256 bytes or
///   more, or a 64-bit `mdat` with no movie header, starts `00 00 01` / `00 00 00 01` too).
fn not_a_movie(src: &Src) -> Option<VideoError> {
    let mut head = vec![0u8; SNIFF_BYTES];
    let n = src.len().min(SNIFF_BYTES as u64) as usize;
    let head = head.get_mut(..n)?;
    src.read_at(0, head).ok()?;
    let head = &*head;
    let sync = |at: usize, stride: usize| (0..3).all(|k| head.get(at + k * stride) == Some(&0x47));
    if sync(0, 188) || sync(4, 192) {
        return Some(VideoError::Unsupported(reason::TRANSPORT_STREAM.into()));
    }
    bare_stream(head).then(|| VideoError::Unsupported(reason::BARE_STREAM.into()))
}

/// `head` starts like a bare H.264 / HEVC stream (see [`not_a_movie`]).
fn bare_stream(head: &[u8]) -> bool {
    let box_type = head.get(4..8).is_some_and(|t| t.iter().all(|b| b.is_ascii_alphanumeric() || *b == b' '));
    let nal = if head.starts_with(&[0, 0, 0, 1]) {
        4
    } else if head.starts_with(&[0, 0, 1]) {
        3
    } else {
        return false;
    };
    let (Some(&a), Some(&b)) = (head.get(nal), head.get(nal + 1)) else { return false };
    // H.264: forbidden bit 0, a type 1 to 23. HEVC: forbidden bit 0, a type 0 to 40, a temporal
    // id plus one that isn't 0.
    let h264 = a & 0x80 == 0 && (1..=23).contains(&(a & 0x1f));
    let hevc = a & 0x80 == 0 && (a >> 1) & 0x3f <= 40 && b & 0x07 != 0;
    let second = head.get(nal + 1..).is_some_and(|rest| rest.windows(3).any(|w| w == [0, 0, 1]));
    !box_type && (h264 || hevc) && second
}

/// The first audio track's description.
fn audio_info(file: &Mp4File) -> Option<AudioTrackInfo> {
    let t = file.tracks.iter().find(|t| t.kind == TrackKind::Audio)?;
    let entry = t.entries.first()?;
    let (sample_rate, channels) = match (&entry.codec, &entry.audio) {
        (CodecConfig::Pcm(c), _) => (c.sample_rate, c.channels),
        (_, Some(a)) => (a.sample_rate, a.channels),
        _ => (0.0, 0),
    };
    Some(AudioTrackInfo { codec: entry.codec.name(), sample_rate: if sample_rate.is_finite() { sample_rate } else { 0.0 }, channels })
}

/// Samples looked at for the median picture duration (spread over the track; a hostile track may
/// list millions).
const DURATION_PROBES: usize = 4096;

/// The median of the track's sample durations, in ticks; `None` when they are all 0.
fn median_duration(t: &isobmff::Track) -> Option<u32> {
    let n = t.samples.len();
    let step = n.div_ceil(DURATION_PROBES).max(1);
    let mut d: Vec<u32> = t.samples.iter().step_by(step).map(|s| s.duration).filter(|&d| d > 0).collect();
    d.sort_unstable();
    d.get(d.len() / 2).copied()
}

enum Decoder {
    H264(Box<h264::decode::Decoder>),
    Hevc(Box<hevc::decode::Decoder>),
}

/// A decoder refusing pictures over its memory budget (craft-video says so in an `Invalid` error,
/// or as unsupported picture dimensions): the clip is valid, only too large.
fn over_memory(m: &str) -> bool {
    m.contains("memory budget") || m.contains("memory limit")
}

fn h264_error(e: h264::decode::Error) -> VideoError {
    use h264::decode::Error;
    match e {
        Error::Unsupported(m) | Error::Invalid(m) if over_memory(&m) => VideoError::Unsupported(reason::TOO_LARGE_TO_DECODE.into()),
        Error::Unsupported(m) => VideoError::Unsupported(h264_unsupported(&m).into()),
        e => VideoError::Malformed(format!("H.264: {e}")),
    }
}

fn hevc_error(e: hevc::decode::Error) -> VideoError {
    use hevc::decode::Error;
    match e {
        Error::Unsupported(m) | Error::Invalid(m) if over_memory(&m) => VideoError::Unsupported(reason::TOO_LARGE_TO_DECODE.into()),
        Error::Unsupported(m) => VideoError::Unsupported(hevc_unsupported(&m).into()),
        e => VideoError::Malformed(format!("HEVC: {e}")),
    }
}

/// The plain reason for what the H.264 decoder doesn't do (its own words are the bitstream's:
/// `chroma_format_idc 2`, `MBAFF`, `FMO`).
fn h264_unsupported(m: &str) -> &'static str {
    if m.contains("chroma_format_idc 2") {
        reason::H264_422
    } else if m.contains("bit depth") {
        reason::H264_DEEP
    } else if m.contains("interlaced") || m.contains("field") {
        reason::H264_INTERLACED
    } else {
        reason::H264_OTHER
    }
}

/// The plain reason for what the HEVC decoder doesn't do.
fn hevc_unsupported(m: &str) -> &'static str {
    if m.starts_with("4:4:4") || m.contains("chroma_format_idc 3") {
        reason::HEVC_444
    } else if m.contains("bit depth") {
        reason::HEVC_DEEP
    } else {
        reason::HEVC_OTHER
    }
}

/// The most decoder threads [`decoder_plan`] considers (a larger request decodes on this many).
const MAX_DECODER_THREADS: usize = 64;

/// What a clip's decoder is set up with: its threads and memory budget, and the bytes of
/// pictures a caller weighing memory should count ([`StreamInfo::decoder_bytes`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DecoderPlan {
    threads: usize,
    budget: u64,
    weight: u64,
}

/// The decoder plan for a clip whose pictures are `sizing` ([`picture_sizing`]): the most threads
/// up to [`StreamOptions::threads`] whose pictures fit within the ceiling
/// [`StreamOptions::memory_budget`] (each thread holds pictures in flight, so an 8K clip may
/// decode on fewer threads than asked), and the bytes those pictures take, as budget and weight.
/// A clip whose pictures don't fit even on one thread is too large to decode.
///
/// When the stream may bring sequence headers of its own (an `avc3` / `hev1` sample entry, or a
/// record with no sequence header this reads, sized from the sample entry), its pictures may grow
/// past the estimate, legally: the decoder keeps the whole ceiling and its own check, and the
/// estimate picks the threads and is the weight.
fn decoder_plan(sizing: Option<Sizing>, opts: &StreamOptions) -> Result<DecoderPlan, VideoError> {
    let asked = opts.threads.clamp(1, MAX_DECODER_THREADS);
    let Some(sizing) = sizing else {
        return Ok(DecoderPlan { threads: asked, budget: opts.memory_budget, weight: opts.memory_budget });
    };
    let (threads, bytes) = (1..=asked)
        .rev()
        .find_map(|threads| sizing.store(threads).filter(|bytes| *bytes <= opts.memory_budget).map(|bytes| (threads, bytes)))
        .ok_or_else(|| VideoError::Unsupported(reason::TOO_LARGE_TO_DECODE.into()))?;
    let budget = if sizing.exact { bytes } else { opts.memory_budget };
    Ok(DecoderPlan { threads, budget, weight: bytes })
}

/// Decoded pictures the decoders have in flight per thread setting: craft-video 5c91598 decodes
/// `threads + 2` at once.
fn in_flight(threads: usize) -> u64 {
    threads.max(1) as u64 + 2
}

/// How big a clip's decoded pictures are and how many its decoder holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sizing {
    /// Bytes of the largest decoded picture.
    picture: u64,
    /// Reference pictures (the most any sequence header asks for).
    references: u64,
    /// Pictures held per picture in flight, counting those in flight (see [`Sizing::store`]).
    per_flight: u64,
    /// The record's sequence headers are all the stream has (an `avc1` / `hvc1` sample entry
    /// that carries them): the decoder needs no more than [`Sizing::store`].
    exact: bool,
}

/// Reference pictures assumed when no sequence header says: the most either codec allows.
const MAX_REFERENCES: u64 = 16;

impl Sizing {
    /// Bytes of decoded pictures the decoder may hold on `threads` threads, as craft-video
    /// 5c91598 counts them: the reference pictures, the one being decoded, those in flight, and
    /// the output waiting to be taken (at most twice the pictures in flight). H.264 charges the
    /// larger of the pictures in flight and that output, HEVC both.
    fn store(&self, threads: usize) -> Option<u64> {
        let pictures = self.references.saturating_add(1).saturating_add(in_flight(threads).saturating_mul(self.per_flight));
        self.picture.checked_mul(pictures)
    }

    /// Pictures held per picture in flight by `codec`'s decoder ([`Sizing::store`]).
    fn per_flight(codec: Codec) -> u64 {
        if codec == Codec::Hevc { 3 } else { 2 }
    }

    /// Sized from the sample entry's `width` × `height` when no sequence header can be read: 4:2:0
    /// pictures padded to 64-sample blocks, 2 bytes a sample for HEVC, [`MAX_REFERENCES`].
    fn of_entry(codec: Codec, width: u16, height: u16) -> Option<Sizing> {
        let pad = |n: u16| u64::from(n).div_ceil(64).saturating_mul(64);
        let shape = craft_video::budget::PictureShape {
            width: pad(width),
            height: pad(height),
            shift_x: 1,
            shift_y: 1,
            chroma_planes: 2,
            bytes_per_sample: if codec == Codec::Hevc { 2 } else { 1 },
        };
        let picture = shape.bytes().filter(|b| *b > 0)?;
        Some(Sizing { picture, references: MAX_REFERENCES, per_flight: Sizing::per_flight(codec), exact: false })
    }
}

/// How the decoder for `codec` sizes its pictures, from every sequence header in the record (the
/// largest picture, the most reference pictures). `None` when the record has none this reads (the
/// decoder then says why).
fn picture_sizing(codec: &CodecConfig) -> Option<Sizing> {
    use craft_media::bitstream::unescape_rbsp;
    let sizes: Vec<(craft_video::budget::PictureShape, u64)> = match codec {
        CodecConfig::Avc(c) => c
            .sps
            .iter()
            .filter_map(|nal| h264::decode::params::Sps::parse(&unescape_rbsp(nal.get(1..)?)).ok())
            .map(|sps| {
                let chroma = sps.chroma_array_type();
                let shape = craft_video::budget::PictureShape {
                    width: u64::from(sps.width()),
                    height: u64::from(sps.height()),
                    shift_x: u32::from(chroma != 3),
                    shift_y: u32::from(chroma == 1),
                    chroma_planes: if chroma == 0 { 0 } else { 2 },
                    bytes_per_sample: 1,
                };
                (shape, sps.max_dpb_frames() as u64)
            })
            .collect(),
        CodecConfig::Hevc(c) => c
            .arrays
            .iter()
            .filter(|a| a.nal_type == 33)
            .flat_map(|a| a.nalus.iter())
            .filter_map(|nal| hevc::decode::params::Sps::parse(&unescape_rbsp(nal.get(2..)?)).ok())
            .map(|sps| {
                let chroma = sps.chroma_array_type();
                let shape = craft_video::budget::PictureShape {
                    width: u64::from(sps.width),
                    height: u64::from(sps.height),
                    shift_x: u32::from(sps.sub_width_c() == 2),
                    shift_y: u32::from(sps.sub_height_c() == 2),
                    chroma_planes: if chroma == 0 { 0 } else { 2 },
                    bytes_per_sample: 2,
                };
                (shape, u64::from(sps.max_dec_pic_buffering))
            })
            .collect(),
        _ => return None,
    };
    let codec = if matches!(codec, CodecConfig::Hevc(_)) { Codec::Hevc } else { Codec::H264 };
    // (a picture size that overflows counts as the largest)
    let picture = sizes.iter().map(|(shape, _)| shape.bytes().unwrap_or(u64::MAX)).max()?;
    let references = sizes.iter().map(|(_, refs)| *refs).max()?;
    Some(Sizing { picture, references, per_flight: Sizing::per_flight(codec), exact: true })
}

impl Decoder {
    /// A decoder for `codec` on `threads` threads whose pictures may take `budget` bytes.
    fn new(codec: Codec, config: &[u8], threads: usize, budget: u64) -> Result<Decoder, VideoError> {
        let threads = threads.max(1);
        Ok(match codec {
            Codec::H264 => {
                let mut d = h264::decode::Decoder::with_threads(threads);
                d.set_memory_budget(budget);
                d.configure_avcc(config).map_err(h264_error)?;
                Decoder::H264(Box::new(d))
            }
            Codec::Hevc => {
                let mut d = hevc::decode::Decoder::with_threads(threads);
                d.set_memory_budget(budget);
                d.configure_hvcc(config).map_err(hevc_error)?;
                Decoder::Hevc(Box::new(d))
            }
        })
    }

    fn set_draft(&mut self, on: bool) {
        match self {
            Decoder::H264(d) => d.set_draft(on),
            Decoder::Hevc(d) => d.set_draft(on),
        }
    }

    fn decode(&mut self, data: &[u8], pts: i64, out: &mut Vec<Decoded>) -> Result<(), VideoError> {
        match self {
            Decoder::H264(d) => out.extend(d.decode(data, pts).map_err(h264_error)?.into_iter().map(Decoded::H264)),
            Decoder::Hevc(d) => out.extend(d.decode(data, pts).map_err(hevc_error)?.into_iter().map(Decoded::Hevc)),
        }
        Ok(())
    }

    fn flush(&mut self, out: &mut Vec<Decoded>) -> Result<(), VideoError> {
        match self {
            Decoder::H264(d) => {
                out.extend(d.flush().into_iter().map(Decoded::H264));
                d.take_error().map_or(Ok(()), |e| Err(h264_error(e)))
            }
            Decoder::Hevc(d) => {
                out.extend(d.flush().into_iter().map(Decoded::Hevc));
                d.take_error().map_or(Ok(()), |e| Err(hevc_error(e)))
            }
        }
    }
}

enum Decoded {
    H264(h264::decode::Picture),
    Hevc(hevc::decode::Picture),
}

/// The chroma layout a decoder's plane sizes describe.
fn chroma_of(width: u32, height: u32, cw: u32, ch: u32) -> Result<Chroma, VideoError> {
    for c in [Chroma::C420, Chroma::C422, Chroma::C444] {
        if c.plane_size(width as usize, height as usize) == (cw as usize, ch as usize) {
            return Ok(c);
        }
    }
    Err(VideoError::Malformed(format!("chroma planes {cw}×{ch} for a {width}×{height} picture")))
}

fn vui(full_range: bool, primaries: u8, transfer: u8, matrix: u8) -> ColourDescription {
    ColourDescription { primaries, transfer, matrix, full_range }
}

/// A sample entry's `colr` box: its code points, and its range flag when it has one (`nclx` does,
/// QuickTime's `nclc` does not).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ContainerColour {
    desc: ColourDescription,
    full_range: Option<bool>,
}

impl ContainerColour {
    /// The colour a picture is read with: ISO/IEC 14496-12 (the `colr` box) gives the box
    /// precedence over the bitstream where both describe the colour, as AVFoundation does. So each
    /// code point the box specifies wins, the bitstream's VUI fills those it leaves unspecified,
    /// and the range flag is the box's when it carries one, else the bitstream's.
    fn over(self, bitstream: ColourDescription) -> ColourDescription {
        ColourDescription { full_range: self.full_range.unwrap_or(bitstream.full_range), ..self.desc.or(bitstream) }
    }
}

impl Decoded {
    fn pts(&self) -> i64 {
        match self {
            Decoded::H264(p) => p.pts,
            Decoded::Hevc(p) => p.pts,
        }
    }

    fn into_frame(self, container: Option<ContainerColour>, timescale: u32) -> Result<Frame, VideoError> {
        let (width, height, chroma, bit_depth, planes, y_stride, uv_stride, colour, key, draft, pts) = match self {
            Decoded::H264(p) => {
                let c = p.color;
                (
                    p.width,
                    p.height,
                    chroma_of(p.width, p.height, p.chroma_width, p.chroma_height)?,
                    8,
                    FramePlanes::U8([p.y, p.u, p.v]),
                    p.y_stride,
                    p.uv_stride,
                    vui(c.full_range, c.primaries, c.transfer, c.matrix),
                    p.key,
                    p.draft,
                    p.pts,
                )
            }
            Decoded::Hevc(p) => {
                let c = p.color;
                let planes = match (p.y, p.u, p.v) {
                    (hevc::decode::Plane::U8(y), hevc::decode::Plane::U8(u), hevc::decode::Plane::U8(v)) => FramePlanes::U8([y, u, v]),
                    (hevc::decode::Plane::U16(y), hevc::decode::Plane::U16(u), hevc::decode::Plane::U16(v)) => FramePlanes::U16([y, u, v]),
                    _ => return Err(VideoError::Malformed("HEVC picture with mixed sample sizes".into())),
                };
                let chroma = match p.chroma {
                    hevc::decode::ChromaFormat::C420 => Chroma::C420,
                    hevc::decode::ChromaFormat::C422 => Chroma::C422,
                };
                let bit_depth = u8::try_from(p.bit_depth).map_err(|_| VideoError::Malformed(format!("bit depth {}", p.bit_depth)))?;
                (
                    p.width,
                    p.height,
                    chroma,
                    bit_depth,
                    planes,
                    p.y_stride,
                    p.uv_stride,
                    vui(c.full_range, c.primaries, c.transfer, c.matrix),
                    p.key,
                    p.draft,
                    p.pts,
                )
            }
        };
        let colour = container.map_or(colour, |c| c.over(colour));
        Ok(Frame {
            pts,
            seconds: seconds(pts, timescale),
            key,
            width: width as usize,
            height: height as usize,
            bit_depth,
            chroma,
            planes,
            y_stride,
            uv_stride,
            colour,
            draft,
        })
    }
}

fn seconds(ticks: i64, timescale: u32) -> f64 {
    if timescale == 0 { 0.0 } else { ticks as f64 / f64::from(timescale) }
}

fn container_colour(c: &ColorInfo) -> Option<ContainerColour> {
    let code = |v: u16| u8::try_from(v).unwrap_or(yuv::UNSPECIFIED);
    match c {
        ColorInfo::Nclx { primaries, transfer, matrix, full_range } => {
            Some(ContainerColour { desc: vui(*full_range, code(*primaries), code(*transfer), code(*matrix)), full_range: Some(*full_range) })
        }
        ColorInfo::Nclc { primaries, transfer, matrix } => {
            Some(ContainerColour { desc: vui(false, code(*primaries), code(*transfer), code(*matrix)), full_range: None })
        }
        ColorInfo::Icc { .. } => None,
    }
}

/// The reason a sample entry's codec isn't decoded.
fn unsupported_codec(codec: &CodecConfig, format: [u8; 4]) -> VideoError {
    let why = match codec {
        CodecConfig::Av1(_) => reason::AV1,
        CodecConfig::Vp9(_) => reason::VP9,
        CodecConfig::ProRes { .. } => reason::PRORES,
        CodecConfig::Jpeg { .. } => reason::MOTION_JPEG,
        CodecConfig::Dnx { .. } => reason::DNX,
        CodecConfig::Apv(_) => reason::APV,
        _ => match &format {
            b"CFHD" => reason::CINEFORM,
            b"rle " => reason::ANIMATION,
            _ => reason::OTHER_CODEC,
        },
    };
    VideoError::Unsupported(why.into())
}

/// The decoder for sample entry `index` (1-based): its codec and configuration record.
/// Also how its decoder sizes its pictures ([`picture_sizing`]).
fn entry_codec(track: &isobmff::Track, index: u32) -> Result<(Codec, Vec<u8>, Option<ContainerColour>, Option<Sizing>), VideoError> {
    let entry = index
        .checked_sub(1)
        .and_then(|i| track.entries.get(i as usize))
        .ok_or_else(|| VideoError::Malformed(format!("sample description {index} is missing")))?;
    let colour = entry.video.as_ref().and_then(|v| v.color.as_ref()).and_then(container_colour);
    let (codec, config) = match &entry.codec {
        CodecConfig::Avc(c) => (Codec::H264, c.to_bytes()),
        CodecConfig::Hevc(c) => (Codec::Hevc, c.to_bytes()),
        other => return Err(unsupported_codec(other, entry.format.0)),
    };
    // (`avc3` / `hev1` streams may carry sequence headers in their samples, beyond the record's)
    let in_band = matches!(&entry.format.0, b"avc3" | b"hev1");
    let sizing = picture_sizing(&entry.codec).map(|s| Sizing { exact: s.exact && !in_band, ..s }).or_else(|| {
        let (w, h) = entry.video.as_ref().map_or((0, 0), |v| (v.width, v.height));
        Sizing::of_entry(codec, w, h)
    });
    Ok((codec, config, colour, sizing))
}

/// The track to show: the first enabled video track with pictures, else any video track with
/// pictures.
fn pick_track(file: &Mp4File) -> Result<usize, VideoError> {
    let video = |t: &&isobmff::Track| t.kind == TrackKind::Video;
    let with_samples = |enabled: bool| file.tracks.iter().position(|t| video(&t) && !t.samples.is_empty() && (t.enabled || !enabled));
    if let Some(i) = with_samples(true).or_else(|| with_samples(false)) {
        return Ok(i);
    }
    if file.tracks.iter().any(|t| video(&t)) {
        Err(VideoError::Malformed("the video track holds no pictures".into()))
    } else {
        Err(VideoError::Unsupported(reason::NO_VIDEO_TRACK.into()))
    }
}

/// The part of the track a clip shows, in presentation ticks: its edit list's first non-empty
/// edit (`media_time` to `media_time` + the segment's duration), else every picture. Pictures the
/// edit cuts still decode (the first ones shown may refer to them) but are never shown: a YouTube
/// MP4 starts its edit five seconds into the media, and those five seconds are not the clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Window {
    /// When the clip starts and ends.
    start: i64,
    end: i64,
    /// Presentation time of the picture on screen at `start` (at or before it).
    first: i64,
    /// Samples to feed (decode order): through the last one shown before `end`. Those after it
    /// are shown only past the end, and no picture shown refers to a later one.
    feed_end: usize,
}

impl Window {
    fn of(t: &isobmff::Track, movie_timescale: u32) -> Window {
        let pts = |i: usize| t.presentation_pts(i);
        // (a picture without a duration, often the last one, is shown for the usual time: with
        // none, a one-picture clip would show nothing)
        let usual = median_duration(t).map_or(1, i64::from);
        let shown_for = |d: u32| if d == 0 { usual } else { i64::from(d) };
        let all_start = (0..t.samples.len()).filter_map(pts).min().unwrap_or(0);
        let all_end =
            (0..t.samples.len()).filter_map(|i| Some(pts(i)?.saturating_add(shown_for(t.samples.get(i)?.duration)))).max().unwrap_or(all_start);
        // craft-media's `edit_offset` places the first non-empty edit's `media_time` after the
        // leading empty edits (empty: `media_time` -1, as craft-media reads them): that is where
        // the clip starts
        let (start, end) = match t.edits.iter().find(|e| e.media_time != -1) {
            Some(e) => {
                let start = e.media_time.saturating_add(t.edit_offset).clamp(all_start, all_end);
                // (a zero-length segment, or no movie timescale, says nothing about the end)
                let length = (e.segment_duration > 0 && movie_timescale > 0)
                    .then(|| i128::from(e.segment_duration) * i128::from(t.timescale) / i128::from(movie_timescale))
                    .map(|l| i64::try_from(l).unwrap_or(i64::MAX));
                (start, length.map_or(all_end, |l| start.saturating_add(l).min(all_end)))
            }
            None => (all_start, all_end),
        };
        let first = t.sample_at_presentation_time(start).and_then(pts).filter(|&p| p <= start).unwrap_or(start);
        // (a track with pictures always shows one: a segment that rescales to no ticks still
        // shows the picture at its start)
        let end = if t.samples.is_empty() { end.max(start) } else { end.max(start.saturating_add(1)) };
        let feed_end = (0..t.samples.len()).rev().find(|&i| pts(i).is_some_and(|p| p < end)).map_or(t.samples.len(), |i| i + 1);
        Window { start, end, first, feed_end }
    }
}

/// A clip's video track, decoded picture by picture.
pub struct VideoStream {
    src: Src,
    file: Mp4File,
    track: usize,
    info: StreamInfo,
    opts: StreamOptions,
    decoder: Decoder,
    /// The sample description the decoder is configured for, and its colour box.
    description: u32,
    container_colour: Option<ContainerColour>,
    /// Next sample to feed, in decode order.
    next_sample: usize,
    /// Samples fed after a seek at most ([`Self::set_feed_limit`]), and the sample feeding stops
    /// before.
    feed_limit: Option<usize>,
    feed_stop: usize,
    /// Pictures shown before this time are dropped (a seek's target; open-GOP leading pictures).
    min_pts: i64,
    /// Pictures shown before this time are no stand-ins either (open-GOP leading pictures).
    keep_from: i64,
    /// What the clip shows ([`Window`]): pictures outside it decode (as references) but are
    /// never returned.
    window: Window,
    /// The latest picture a seek skipped (shown at or after its sync picture): what a caller can
    /// show when the target itself never decodes ([`Self::take_skipped`]).
    skipped: Option<Frame>,
    ready: VecDeque<Frame>,
    decoded: Vec<Decoded>,
    flushed: bool,
    draft: bool,
    frames_fed: usize,
    bytes_read: u64,
}

impl VideoStream {
    /// Reads the clip's headers (its `moov`) and gets the decoder ready for the first picture.
    pub fn open(src: impl ClipSource + 'static, opts: &StreamOptions) -> Result<VideoStream, VideoError> {
        let src = Src(Box::new(src));
        let file = isobmff::open(&src).map_err(|e| not_a_movie(&src).unwrap_or_else(|| demux_error(e)))?;
        let track = pick_track(&file)?;
        let t = file.tracks.get(track).ok_or_else(|| VideoError::Malformed("no video track".into()))?;
        let description = t.samples.first().map_or(1, |s| s.description_index);
        let (codec, config, container_colour, sizing) = entry_codec(t, description)?;
        let plan = decoder_plan(sizing, opts)?;
        let decoder = Decoder::new(codec, &config, plan.threads, plan.budget)?;
        let (w, h) = t.video().map_or((t.width, t.height), |v| (u32::from(v.width), u32::from(v.height)));
        let window = Window::of(t, file.timescale);
        let (start, end) = (window.start, window.end);
        let frame = median_duration(t).map(|d| seconds(i64::from(d), t.timescale)).filter(|d| *d > 0.0);
        let info = StreamInfo {
            frame_duration_seconds: frame,
            frame_rate: frame.map(|d| 1.0 / d),
            audio: audio_info(&file),
            codec,
            width: w,
            height: h,
            orientation: matrix_orientation(&t.matrix),
            container_colour: container_colour.map(|c| c.desc),
            timescale: t.timescale,
            start_seconds: seconds(start, t.timescale),
            duration_seconds: seconds(end.saturating_sub(start), t.timescale),
            sample_count: t.samples.len(),
            decoder_bytes: plan.weight,
            decoder_threads: plan.threads,
        };
        if t.timescale == 0 {
            return Err(VideoError::Malformed("the video track's timescale is 0".into()));
        }
        Ok(VideoStream {
            src,
            file,
            track,
            info,
            opts: *opts,
            decoder,
            description,
            container_colour,
            next_sample: 0,
            feed_limit: None,
            feed_stop: usize::MAX,
            min_pts: window.first,
            keep_from: window.first,
            window,
            skipped: None,
            ready: VecDeque::new(),
            decoded: Vec::new(),
            flushed: false,
            draft: false,
            frames_fed: 0,
            bytes_read: 0,
        })
    }

    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    /// Coded pictures fed to the decoder so far, and the sample bytes read for them.
    pub fn frames_decoded(&self) -> usize {
        self.frames_fed
    }

    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Draft decoding for playback at reduced quality (non-reference pictures skip in-loop
    /// filtering; see the decoders' `set_draft`). Off by default; never for a still.
    pub fn set_draft(&mut self, on: bool) {
        self.draft = on;
        self.decoder.set_draft(on);
    }

    /// At most `limit` coded pictures are fed from here and after each later seek (`None`: no
    /// limit). Reaching it ends the run as the track's end does: the pictures still in the
    /// decoder come out, and [`Self::take_skipped`] holds the latest one decoded on the way.
    /// A poster frame sets it, so a seek that never reaches a shown picture (an edit list that
    /// cuts the only sync picture far before the clip starts) stays as cheap as its plan.
    pub fn set_feed_limit(&mut self, limit: Option<usize>) {
        self.feed_limit = limit;
        self.feed_stop = limit.map_or(usize::MAX, |l| self.next_sample.saturating_add(l));
    }

    fn track(&self) -> Result<&isobmff::Track, VideoError> {
        self.file.tracks.get(self.track).ok_or_else(|| VideoError::Malformed("no video track".into()))
    }

    /// (sync sample, target sample) for an exact seek to `seconds`.
    fn seek_points(&self, seconds: f64) -> Result<(usize, usize), VideoError> {
        let t = self.track()?;
        let ticks = if seconds.is_finite() { (seconds * f64::from(t.timescale)).round() } else { 0.0 };
        // (inside what the clip shows: before it the first picture shown, past it the last)
        let w = &self.window;
        let ticks = (ticks.clamp(i64::MIN as f64, i64::MAX as f64) as i64).clamp(w.start, w.end.saturating_sub(1).max(w.start));
        let target = t.sample_at_presentation_time(ticks).ok_or_else(|| VideoError::Malformed("the video track holds no pictures".into()))?;
        let sync = t.sync_sample_before(target);
        // a picture shown before its sync picture is an open-GOP leading picture: it needs the
        // previous group, so the sync picture stands in for it
        let target = match (t.presentation_pts(target), t.presentation_pts(sync)) {
            (Some(a), Some(b)) if a < b => sync,
            _ => target,
        };
        Ok((sync, target.max(sync)))
    }

    /// What an exact seek to `seconds` would decode.
    pub fn seek_plan(&self, seconds: f64) -> Result<SeekPlan, VideoError> {
        let (sync, target) = self.seek_points(seconds)?;
        let t = self.track()?;
        let bytes = t.samples.get(sync..=target).map_or(0, |s| s.iter().map(|s| u64::from(s.size)).sum());
        let at = |i: usize| t.presentation_pts(i).map_or(0.0, |p| self::seconds(p, t.timescale));
        Ok(SeekPlan { frames: target - sync + 1, bytes, keyframe_seconds: at(sync), target_seconds: at(target) })
    }

    /// Positions the stream so [`Self::next_frame`] returns the picture shown at `seconds`
    /// ([`SeekMode::Exact`]) or the sync picture before it ([`SeekMode::Keyframe`]). Any number of
    /// times on one stream; a time past the end lands on the last picture, one before the start on
    /// the first.
    pub fn seek(&mut self, seconds: f64, mode: SeekMode) -> Result<(), VideoError> {
        let (sync, target) = self.seek_points(seconds)?;
        let shown = if mode == SeekMode::Exact { target } else { sync };
        let t = self.track()?;
        // (a sync picture the edit list cuts is decoded, not shown: the first picture shown is;
        // it and the cut pictures after it can still stand in when nothing shown decodes)
        let first = self.window.first;
        let min_pts = t.presentation_pts(shown).unwrap_or(i64::MIN).max(first);
        let keep_from = t.presentation_pts(sync).unwrap_or(i64::MIN);
        let description = t.samples.get(sync).map_or(1, |s| s.description_index);
        self.restart(description)?;
        self.next_sample = sync;
        self.feed_stop = self.feed_limit.map_or(usize::MAX, |l| sync.saturating_add(l));
        self.min_pts = min_pts;
        self.keep_from = keep_from;
        self.skipped = None;
        Ok(())
    }

    /// The decoder ready for sample description `description` from a sync picture; pending
    /// pictures are dropped. The H.264 decoder is reset in place (it keeps its worker threads and
    /// picture buffers, which matters for a scrub that seeks on every move); the HEVC decoder has
    /// no reset, so it is made anew.
    fn restart(&mut self, description: u32) -> Result<(), VideoError> {
        let (codec, config, colour, sizing) = entry_codec(self.track()?, description)?;
        let DecoderPlan { threads, budget, weight } = decoder_plan(sizing, &self.opts)?;
        match &mut self.decoder {
            // (a reset keeps the decoder's threads: another description on the same count only)
            Decoder::H264(d) if codec == Codec::H264 && threads == self.info.decoder_threads => {
                d.reset();
                d.set_memory_budget(budget);
                d.configure_avcc(&config).map_err(h264_error)?;
            }
            _ => self.decoder = Decoder::new(codec, &config, threads, budget)?,
        }
        (self.info.decoder_threads, self.info.decoder_bytes) = (threads, weight);
        self.decoder.set_draft(self.draft);
        self.description = description;
        self.container_colour = colour;
        self.ready.clear();
        self.decoded.clear();
        self.flushed = false;
        Ok(())
    }

    /// The next picture in presentation order; `None` at the end of the track. A sample that
    /// can't be read or decoded is an error; the stream goes on with the next one when called
    /// again (a damaged picture's dependants may then be wrong until the next sync picture).
    pub fn next_frame(&mut self) -> Result<Option<Frame>, VideoError> {
        self.next_frame_until(|| false)
    }

    /// [`Self::next_frame`], asking `stop` before each coded picture is fed: `true` ends the
    /// call with [`VideoError::Cancelled`] (a superseded scrub, a cancelled thumbnail). The
    /// stream stays usable.
    pub fn next_frame_until(&mut self, mut stop: impl FnMut() -> bool) -> Result<Option<Frame>, VideoError> {
        loop {
            if let Some(f) = self.ready.pop_front() {
                return Ok(Some(f));
            }
            if stop() {
                return Err(VideoError::Cancelled);
            }
            // (past the last picture shown, the track's remaining samples are not fed; nor past
            // the feed limit)
            let len = self.track()?.samples.len().min(self.window.feed_end).min(self.feed_stop);
            if self.next_sample >= len {
                if self.flushed {
                    return Ok(None);
                }
                self.flushed = true;
                let r = self.decoder.flush(&mut self.decoded);
                self.take_decoded()?;
                r?;
                continue;
            }
            self.feed_one()?;
        }
    }

    /// Feeds the next sample to the decoder.
    fn feed_one(&mut self) -> Result<(), VideoError> {
        let i = self.next_sample;
        self.next_sample += 1;
        let t = self.track()?;
        let s = *t.samples.get(i).ok_or_else(|| VideoError::Malformed(format!("sample {i} is missing")))?;
        let pts = t.presentation_pts(i).unwrap_or(s.pts);
        if s.size > self.opts.max_sample_bytes {
            return Err(VideoError::TooLarge(format!("picture {i} is {} bytes", s.size)));
        }
        if s.description_index != self.description {
            // the clip switches to another sample description: finish the old stream first
            let r = self.decoder.flush(&mut self.decoded);
            self.take_decoded()?;
            r?;
            let ready = std::mem::take(&mut self.ready);
            self.restart(s.description_index)?;
            self.ready = ready;
        }
        let data = self.file.read_sample(&self.src, self.track, i).map_err(|e| match e {
            isobmff::Error::Truncated(_) => VideoError::Malformed(format!("picture {i} lies past the end of the file")),
            e => demux_error(e),
        })?;
        self.bytes_read = self.bytes_read.saturating_add(data.len() as u64);
        self.frames_fed += 1;
        let r = self.decoder.decode(&data, pts, &mut self.decoded);
        self.take_decoded()?;
        r
    }

    /// The latest picture the last seek skipped on its way to the target (shown at or after the
    /// sync picture it started from), if any: a stand-in when the target never decodes.
    pub fn take_skipped(&mut self) -> Option<Frame> {
        self.skipped.take()
    }

    /// Moves decoded pictures to the output; those shown before the seek target are skipped (the
    /// latest of them kept as [`Self::take_skipped`]'s stand-in).
    fn take_decoded(&mut self) -> Result<(), VideoError> {
        for d in std::mem::take(&mut self.decoded) {
            let pts = d.pts();
            if pts >= self.window.end {
                // past what the clip shows (an edit list that ends before the media)
                continue;
            }
            if pts < self.min_pts {
                if pts >= self.keep_from && self.skipped.as_ref().is_none_or(|s| s.pts < pts) {
                    self.skipped = Some(d.into_frame(self.container_colour, self.info.timescale)?);
                }
                continue;
            }
            self.ready.push_back(d.into_frame(self.container_colour, self.info.timescale)?);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An 8-bit 4:2:0 grey picture of `w` × `h`.
    fn grey(w: usize, h: usize) -> (Vec<u8>, Vec<u8>) {
        (vec![100; w * h], vec![128; w * h / 4])
    }

    /// An H.264 stream of one `w` × `h` picture: its avcC record and the picture's sample.
    fn h264_stream(w: usize, h: usize) -> (Vec<u8>, Vec<u8>) {
        use craft_video::h264::{Encoder, EncoderConfig, PacketFormat, YuvFrame};
        let mut cfg = EncoderConfig::new(w as u32, h as u32, 30, 1);
        cfg.format = PacketFormat::LengthPrefixed;
        cfg.threads = 1;
        let mut enc = Encoder::new(cfg).unwrap();
        let (y, c) = grey(w, h);
        let mut packets = enc.encode(&YuvFrame { y: &y, u: &c, v: &c, y_stride: w, uv_stride: w / 2 }, 0).unwrap();
        packets.extend(enc.flush());
        (enc.avcc(), packets.remove(0).data)
    }

    /// An HEVC stream of one `w` × `h` picture: its hvcC record and the picture's sample.
    fn hevc_stream(w: usize, h: usize) -> (Vec<u8>, Vec<u8>) {
        use craft_video::hevc::{Encoder, EncoderConfig, Frame};
        let mut enc = Encoder::new(EncoderConfig::new(w as u32, h as u32, 30, 1)).unwrap();
        let (y, c) = grey(w, h);
        let wide = |p: &[u8]| p.iter().map(|&s| u16::from(s)).collect::<Vec<_>>();
        let (y, c) = (wide(&y), wide(&c));
        let p = enc.encode(&Frame { y: &y, u: &c, v: &c, y_stride: w, uv_stride: w / 2 }).unwrap();
        (enc.hvcc(), p.data)
    }

    /// The real decoders' memory refusal, whatever its wording, reads as a clip too large to
    /// decode (the test breaks, not the behaviour, if craft-video rewords it).
    #[test]
    fn a_decoder_refusing_for_memory_is_too_large_to_decode() {
        for (codec, (config, sample)) in [(Codec::H264, h264_stream(64, 64)), (Codec::Hevc, hevc_stream(64, 64))] {
            let config = match codec {
                Codec::H264 => isobmff::AvcConfig::parse(&config).unwrap().to_bytes(),
                _ => isobmff::HevcConfig::parse(&config).unwrap().to_bytes(),
            };
            let mut d = Decoder::new(codec, &config, 1, 1000).unwrap();
            let mut out = Vec::new();
            let e = d.decode(&sample, 0, &mut out).and_then(|()| d.flush(&mut out)).unwrap_err();
            assert_eq!(e, VideoError::Unsupported(reason::TOO_LARGE_TO_DECODE.into()), "{codec:?}");
        }
    }

    /// The budget covers what craft-video 5c91598 may charge at worst: the reference pictures, the
    /// one being decoded, those in flight and the output waiting to be taken (at most twice those
    /// in flight). The HEVC decoder adds the in-flight pictures and that output; H.264 charges the
    /// larger of them.
    #[test]
    fn the_budget_covers_the_output_waiting_beside_the_pictures_in_flight() {
        for threads in 1..=16 {
            let flight = in_flight(threads);
            let at = |codec| Sizing { picture: 1, references: 4, per_flight: Sizing::per_flight(codec), exact: true }.store(threads);
            assert_eq!(at(Codec::Hevc), Some(4 + 1 + flight + 2 * flight), "{threads}");
            assert_eq!(at(Codec::H264), Some(4 + 1 + flight.max(2 * flight)), "{threads}");
        }
    }

    /// Every sequence header in the record counts: the largest picture and the most reference
    /// pictures, not just the first header's.
    #[test]
    fn every_sequence_header_counts() {
        let (small, _) = h264_stream(64, 64);
        let (big, _) = h264_stream(640, 480);
        let mut record = isobmff::AvcConfig::parse(&small).unwrap();
        record.sps.extend(isobmff::AvcConfig::parse(&big).unwrap().sps);
        let sizing = picture_sizing(&CodecConfig::Avc(record)).unwrap();
        assert_eq!(sizing.picture, 640 * 480 * 3 / 2);
        let (small, _) = hevc_stream(64, 64);
        let (big, _) = hevc_stream(640, 480);
        let mut record = isobmff::HevcConfig::parse(&small).unwrap();
        record.arrays.extend(isobmff::HevcConfig::parse(&big).unwrap().arrays.into_iter().filter(|a| a.nal_type == 33));
        let sizing = picture_sizing(&CodecConfig::Hevc(record)).unwrap();
        assert_eq!(sizing.picture, 640 * 480 * 3);
    }

    /// With no sequence header to read, the weight comes from the sample entry's picture size, not
    /// the whole ceiling; the decoder keeps the ceiling and its own check.
    #[test]
    fn without_a_sequence_header_the_entry_size_weighs_the_decode() {
        let opts = StreamOptions { threads: 2, ..StreamOptions::default() };
        let plan = decoder_plan(Sizing::of_entry(Codec::Hevc, 1920, 1080), &opts).unwrap();
        // (1920 × 1088 samples, 4:2:0, 2 bytes each; 16 references, 1, 4 in flight, 8 waiting)
        assert_eq!(plan, DecoderPlan { threads: 2, budget: opts.memory_budget, weight: 1920 * 1088 * 3 * (16 + 1 + 4 * 3) });
        assert_eq!(Sizing::of_entry(Codec::H264, 0, 1080), None);
    }

    /// The decoders' own words name bitstream syntax; the reason a person reads doesn't, and a
    /// decoder refusing pictures over its memory budget means a clip too large, not a damaged one.
    #[test]
    fn decoder_refusals_read_in_plain_words() {
        let h = |m: &str| h264_error(h264::decode::Error::Unsupported(m.into()));
        let says = |e: VideoError, why: &str| assert_eq!(e, VideoError::Unsupported(why.into()));
        says(h("chroma_format_idc 2 (only 4:2:0 is implemented)"), reason::H264_422);
        says(h("bit depth luma 10 chroma 10 (only 8-bit is implemented)"), reason::H264_DEEP);
        says(h("interlaced (field / MBAFF) coding"), reason::H264_INTERLACED);
        says(h("field pictures"), reason::H264_INTERLACED);
        says(h("slice groups (FMO)"), reason::H264_OTHER);
        says(h("picture dimensions exceed the decoder memory limit"), reason::TOO_LARGE_TO_DECODE);
        let v = |m: &str| hevc_error(hevc::decode::Error::Unsupported(m.into()));
        says(v("4:4:4 (only 4:2:0 and 4:2:2)"), reason::HEVC_444);
        says(v("bit depth 14/14"), reason::HEVC_DEEP);
        says(v("SPS range extension tools"), reason::HEVC_OTHER);
        let budget = "7680x4320 pictures with 10 in the decoder and 0 bytes queued exceed the memory budget";
        says(hevc_error(hevc::decode::Error::Invalid(budget.into())), reason::TOO_LARGE_TO_DECODE);
        says(h264_error(h264::decode::Error::Invalid(budget.into())), reason::TOO_LARGE_TO_DECODE);
        // (anything else invalid is still damage)
        assert!(matches!(hevc_error(hevc::decode::Error::Invalid("slice_qp_delta out of range".into())), VideoError::Malformed(_)));
        for why in [reason::H264_422, reason::H264_DEEP, reason::HEVC_444, reason::TOO_LARGE_TO_DECODE] {
            assert!(reason::ALL.contains(&why));
            assert!(!why.contains("idc") && !why.contains('_'), "{why}");
        }
    }
}
