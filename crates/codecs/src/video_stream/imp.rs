//! The decoding half of [`super`]: demuxing with craft-media, decoding with craft-video.

use std::collections::VecDeque;

use craft_media::isobmff::{self, ByteSource, CodecConfig, ColorInfo, Mp4File, TrackKind};
use craft_video::{h264, hevc};

use super::{AudioTrackInfo, ClipSource, Codec, Frame, FramePlanes, SeekMode, SeekPlan, StreamInfo, StreamOptions, VideoError, matrix_orientation};
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
        isobmff::Error::Unsupported(m) => VideoError::Unsupported(format!("this clip's container: {m}")),
        isobmff::Error::TooLarge(m) => VideoError::TooLarge(m.to_string()),
        isobmff::Error::NoMoov => VideoError::Malformed("no movie header (moov box)".into()),
        e => VideoError::Malformed(e.to_string()),
    }
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

fn h264_error(e: h264::decode::Error) -> VideoError {
    match e {
        h264::decode::Error::Unsupported(m) => VideoError::Unsupported(format!("this H.264 video can't be decoded yet ({m})")),
        e => VideoError::Malformed(format!("H.264: {e}")),
    }
}

fn hevc_error(e: hevc::decode::Error) -> VideoError {
    match e {
        hevc::decode::Error::Unsupported(m) => VideoError::Unsupported(format!("this HEVC video can't be decoded yet ({m})")),
        e => VideoError::Malformed(format!("HEVC: {e}")),
    }
}

impl Decoder {
    fn new(codec: Codec, config: &[u8], opts: &StreamOptions) -> Result<Decoder, VideoError> {
        let threads = opts.threads.max(1);
        Ok(match codec {
            Codec::H264 => {
                let mut d = h264::decode::Decoder::with_threads(threads);
                d.set_memory_budget(opts.memory_budget);
                d.configure_avcc(config).map_err(h264_error)?;
                Decoder::H264(Box::new(d))
            }
            Codec::Hevc => {
                let mut d = hevc::decode::Decoder::with_threads(threads);
                d.set_memory_budget(opts.memory_budget);
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

impl Decoded {
    fn pts(&self) -> i64 {
        match self {
            Decoded::H264(p) => p.pts,
            Decoded::Hevc(p) => p.pts,
        }
    }

    fn into_frame(self, container: Option<ColourDescription>, timescale: u32) -> Result<Frame, VideoError> {
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
        // The bitstream's description first; the container's `colr` fills what it leaves open (an
        // encoder that writes no VUI colour leaves it all unspecified), and a full-range flag in
        // either counts (a stream without a VUI reads as limited range).
        let colour = match container {
            Some(c) => ColourDescription { full_range: colour.full_range || c.full_range, ..colour.or(c) },
            None => colour,
        };
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

fn container_colour(c: &ColorInfo) -> Option<ColourDescription> {
    let code = |v: u16| u8::try_from(v).unwrap_or(yuv::UNSPECIFIED);
    match c {
        ColorInfo::Nclx { primaries, transfer, matrix, full_range } => Some(vui(*full_range, code(*primaries), code(*transfer), code(*matrix))),
        ColorInfo::Nclc { primaries, transfer, matrix } => Some(vui(false, code(*primaries), code(*transfer), code(*matrix))),
        ColorInfo::Icc { .. } => None,
    }
}

/// The reason a sample entry's codec isn't decoded.
fn unsupported_codec(codec: &CodecConfig, format: [u8; 4]) -> VideoError {
    let name = match codec {
        CodecConfig::Av1(_) => "AV1".to_string(),
        CodecConfig::Vp9(_) => "VP9".to_string(),
        CodecConfig::ProRes { .. } => "Apple ProRes".to_string(),
        CodecConfig::Jpeg { .. } => "Motion JPEG".to_string(),
        CodecConfig::Dnx { .. } => "Avid DNxHD / DNxHR".to_string(),
        CodecConfig::Apv(_) => "APV".to_string(),
        _ => {
            let tag: String = format.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { char::from(b) } else { '?' }).collect();
            format!("'{}'", tag.trim_end())
        }
    };
    VideoError::Unsupported(format!("{name} video can't be decoded yet"))
}

/// The decoder for sample entry `index` (1-based): its codec and configuration record.
fn entry_codec(track: &isobmff::Track, index: u32) -> Result<(Codec, Vec<u8>, Option<ColourDescription>), VideoError> {
    let entry = index
        .checked_sub(1)
        .and_then(|i| track.entries.get(i as usize))
        .ok_or_else(|| VideoError::Malformed(format!("sample description {index} is missing")))?;
    let colour = entry.video.as_ref().and_then(|v| v.color.as_ref()).and_then(container_colour);
    match &entry.codec {
        CodecConfig::Avc(c) => Ok((Codec::H264, c.to_bytes(), colour)),
        CodecConfig::Hevc(c) => Ok((Codec::Hevc, c.to_bytes(), colour)),
        other => Err(unsupported_codec(other, entry.format.0)),
    }
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
        Err(VideoError::Unsupported("the file has no video track".into()))
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
    container_colour: Option<ColourDescription>,
    /// Next sample to feed, in decode order.
    next_sample: usize,
    /// Pictures shown before this time are dropped (a seek's target; open-GOP leading pictures).
    min_pts: i64,
    /// Pictures shown before this time are no stand-ins either (open-GOP leading pictures).
    keep_from: i64,
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
        let file = isobmff::open(&src).map_err(demux_error)?;
        let track = pick_track(&file)?;
        let t = file.tracks.get(track).ok_or_else(|| VideoError::Malformed("no video track".into()))?;
        let description = t.samples.first().map_or(1, |s| s.description_index);
        let (codec, config, container_colour) = entry_codec(t, description)?;
        let decoder = Decoder::new(codec, &config, opts)?;
        let (w, h) = t.video().map_or((t.width, t.height), |v| (u32::from(v.width), u32::from(v.height)));
        let start = (0..t.samples.len()).filter_map(|i| t.presentation_pts(i)).min().unwrap_or(0);
        let end = (0..t.samples.len())
            .filter_map(|i| Some(t.presentation_pts(i)?.saturating_add(i64::from(t.samples.get(i)?.duration))))
            .max()
            .unwrap_or(start);
        let frame = median_duration(t).map(|d| seconds(i64::from(d), t.timescale)).filter(|d| *d > 0.0);
        let info = StreamInfo {
            frame_duration_seconds: frame,
            frame_rate: frame.map(|d| 1.0 / d),
            audio: audio_info(&file),
            codec,
            width: w,
            height: h,
            orientation: matrix_orientation(&t.matrix),
            container_colour,
            timescale: t.timescale,
            start_seconds: seconds(start, t.timescale),
            duration_seconds: seconds(end.saturating_sub(start), t.timescale),
            sample_count: t.samples.len(),
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
            min_pts: i64::MIN,
            keep_from: i64::MIN,
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

    fn track(&self) -> Result<&isobmff::Track, VideoError> {
        self.file.tracks.get(self.track).ok_or_else(|| VideoError::Malformed("no video track".into()))
    }

    /// (sync sample, target sample) for an exact seek to `seconds`.
    fn seek_points(&self, seconds: f64) -> Result<(usize, usize), VideoError> {
        let t = self.track()?;
        let ticks = if seconds.is_finite() { (seconds * f64::from(t.timescale)).round() } else { 0.0 };
        let ticks = ticks.clamp(i64::MIN as f64, i64::MAX as f64) as i64;
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
        let min_pts = t.presentation_pts(shown).unwrap_or(i64::MIN);
        let keep_from = t.presentation_pts(sync).unwrap_or(i64::MIN);
        let description = t.samples.get(sync).map_or(1, |s| s.description_index);
        self.restart(description)?;
        self.next_sample = sync;
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
        let (codec, config, colour) = entry_codec(self.track()?, description)?;
        match &mut self.decoder {
            Decoder::H264(d) if codec == Codec::H264 => {
                d.reset();
                d.configure_avcc(&config).map_err(h264_error)?;
            }
            _ => self.decoder = Decoder::new(codec, &config, &self.opts)?,
        }
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
            let len = self.track()?.samples.len();
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
