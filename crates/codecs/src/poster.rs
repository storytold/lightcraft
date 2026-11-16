//! Poster frames: the picture a video tile shows, decoded from the clip (decoding needs feature
//! `video-frames` on a native target; without it [`poster_frame`] answers
//! [`VideoError::Unsupported`]).
//!
//! [`poster_frame`] opens the clip's video track ([`crate::video_stream`]), picks the picture shown
//! about [`PosterOptions::target_seconds`] in (the first picture of a clip shorter than
//! [`PosterOptions::short_clip_seconds`]: a fade-in rarely lasts a second, a short clip may not
//! reach it), decodes from the sync picture before it, and returns it in linear Rec.2020, turned
//! upright, no larger than [`PosterOptions::max_edge`].
//!
//! Bounded: the bytes read from the file (header and pictures) stop at
//! [`PosterOptions::max_read_bytes`], at most [`PosterOptions::max_frames`] pictures are decoded
//! (a target further from its sync picture shows the sync picture instead), and `cancel` is
//! checked between pictures. A clip damaged after its sync picture shows the last picture that
//! decoded.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use lightcraft_raster::Rgb32f;
use lightcraft_raster::resample::{Filter, fit};

use crate::video_stream::{ClipSource, Codec, Frame, SeekMode, StreamOptions, VideoError, VideoStream};
use crate::yuv::{ColourDescription, Converter};

/// What a poster frame decodes, and how much it may cost.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PosterOptions {
    /// The time shown, in seconds after the first picture.
    pub target_seconds: f64,
    /// Clips shorter than this show their first picture.
    pub short_clip_seconds: f64,
    /// Longest edge of the result (upright); 0 keeps the decoded size.
    pub max_edge: usize,
    /// Bytes read from the file, its headers included.
    pub max_read_bytes: u64,
    /// Coded pictures decoded.
    pub max_frames: usize,
    /// Decoder limits.
    pub stream: StreamOptions,
}

impl Default for PosterOptions {
    fn default() -> Self {
        PosterOptions {
            target_seconds: 1.0,
            short_clip_seconds: 2.0,
            max_edge: 512,
            max_read_bytes: 256 << 20,
            max_frames: 240,
            stream: StreamOptions { threads: 1, memory_budget: 1 << 30, max_sample_bytes: 64 << 20 },
        }
    }
}

/// A decoded poster frame.
#[derive(Clone, Debug)]
pub struct PosterFrame {
    /// Linear Rec.2020, 1.0 = SDR white, upright.
    pub image: Rgb32f,
    pub codec: Codec,
    /// The colour description it was converted with (defaults filled in).
    pub colour: ColourDescription,
    /// HDR (PQ / HLG), tone mapped for the picture.
    pub hdr: bool,
    pub bit_depth: u8,
    /// When the picture is shown, in seconds of the clip's timeline.
    pub seconds: f64,
    /// The target was too far from its sync picture (or after damage): an earlier picture shows.
    pub fallback: bool,
    pub frames_decoded: usize,
    pub bytes_read: u64,
}

/// A source that refuses reads past a byte budget, so a hostile header (a `moov` claiming a
/// gigabyte) or a long run of pictures can't make a thumbnail read the whole clip.
struct Budgeted<S> {
    inner: S,
    left: AtomicU64,
    used: AtomicU64,
    exhausted: AtomicBool,
}

impl<S: ClipSource> ClipSource for Budgeted<S> {
    fn len(&self) -> u64 {
        self.inner.len()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let n = buf.len() as u64;
        let ok = self.left.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| left.checked_sub(n)).is_ok();
        if !ok {
            self.exhausted.store(true, Ordering::Relaxed);
            return Err(io::Error::other("the poster frame's read budget is spent"));
        }
        self.used.fetch_add(n, Ordering::Relaxed);
        self.inner.read_at(offset, buf)
    }
}

/// The poster frame of the clip in `src`. `cancel` stops the work at the next picture
/// ([`VideoError::Cancelled`]).
pub fn poster_frame<S: ClipSource + 'static>(src: S, opts: &PosterOptions, cancel: &AtomicBool) -> Result<PosterFrame, VideoError> {
    let budget =
        Arc::new(Budgeted { inner: src, left: AtomicU64::new(opts.max_read_bytes), used: AtomicU64::new(0), exhausted: AtomicBool::new(false) });
    let r = decode(budget.clone(), opts, cancel);
    match r {
        Err(VideoError::Io(_)) | Err(VideoError::Malformed(_)) if budget.exhausted.load(Ordering::Relaxed) => {
            Err(VideoError::TooLarge(format!("reading the picture needs more than {} MB of the clip", opts.max_read_bytes >> 20)))
        }
        Ok(mut p) => {
            p.bytes_read = budget.used.load(Ordering::Relaxed);
            Ok(p)
        }
        e => e,
    }
}

fn decode<S: ClipSource + 'static>(src: Arc<Budgeted<S>>, opts: &PosterOptions, cancel: &AtomicBool) -> Result<PosterFrame, VideoError> {
    let mut stream = VideoStream::open(src, &opts.stream)?;
    let info = stream.info().clone();
    let at = if info.duration_seconds >= opts.short_clip_seconds { info.start_seconds + opts.target_seconds } else { info.start_seconds };
    let plan = stream.seek_plan(at)?;
    let sample_budget = opts.max_read_bytes / 2;
    let keyframe_only = plan.frames > opts.max_frames.max(1) || plan.bytes > sample_budget;
    stream.seek(at, if keyframe_only { SeekMode::Keyframe } else { SeekMode::Exact })?;
    let (frame, damaged) = first_frame(&mut stream, cancel)?;
    let fallback = keyframe_only || damaged || (frame.seconds - plan.target_seconds).abs() > 1e-6;
    let conv = Converter::new(frame.colour, frame.bit_depth, frame.width, frame.height)?;
    let mut lin = vec![[0f32; 3]; frame.width.saturating_mul(frame.height)];
    conv.to_linear_rec2020(&frame.picture(), &mut lin)?;
    let image = Rgb32f { width: frame.width, height: frame.height, data: lin };
    drop(frame.planes);
    let image = if opts.max_edge > 0 { fit(&image, opts.max_edge, opts.max_edge, Filter::Lanczos3) } else { image };
    Ok(PosterFrame {
        image: image.into_oriented(info.orientation),
        codec: info.codec,
        colour: conv.resolved(),
        hdr: conv.is_hdr(),
        bit_depth: frame.bit_depth,
        seconds: frame.seconds,
        fallback,
        frames_decoded: stream.frames_decoded(),
        bytes_read: 0,
    })
}

/// The first picture after the seek. When the run ends early (a picture that fails to read or
/// decode, a track cut short), the latest picture decoded on the way stands in (`true`).
fn first_frame(stream: &mut VideoStream, cancel: &AtomicBool) -> Result<(Frame, bool), VideoError> {
    match stream.next_frame_until(|| cancel.load(Ordering::Relaxed)) {
        Ok(Some(f)) => Ok((f, false)),
        Err(e @ (VideoError::Cancelled | VideoError::Unsupported(_))) => Err(e),
        Ok(None) => stream.take_skipped().map(|f| (f, true)).ok_or_else(|| VideoError::Malformed("no picture decoded".into())),
        Err(e) => stream.take_skipped().map(|f| (f, true)).ok_or(e),
    }
}
