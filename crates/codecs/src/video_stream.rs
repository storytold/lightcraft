//! Video streams: the video track of an MP4 / MOV clip demuxed (`craft-media`'s ISO BMFF reader)
//! and decoded picture by picture (`craft-video`'s H.264 and HEVC decoders), for poster frames and
//! playback.
//!
//! The types here exist in every build; the decoding needs feature `video-frames` on a native
//! target ([`video_decoding_available`]). Without it [`VideoStream::open`] and
//! [`crate::poster::poster_frame`] answer [`VideoError::Unsupported`], so callers need no `cfg`.
//!
//! ```no_run
//! # fn main() -> Result<(), lightcraft_codecs::video_stream::VideoError> {
//! use lightcraft_codecs::video_stream::{FileSource, SeekMode, StreamOptions, VideoError, VideoStream};
//! use lightcraft_codecs::yuv::Converter;
//! // a file read in place (or a Vec<u8>, an Arc<[u8]>, any other ClipSource)
//! let file = FileSource::open("clip.mov").map_err(|e| VideoError::Io(e.to_string()))?;
//! let mut stream = VideoStream::open(file, &StreamOptions::default())?;
//! stream.seek(1.0, SeekMode::Exact)?;
//! if let Some(frame) = stream.next_frame()? {
//!     let conv = Converter::new(frame.colour, frame.bit_depth, frame.width, frame.height)?;
//!     let mut rgba = vec![[0u8; 4]; frame.width * frame.height];
//!     conv.to_srgb8(&frame.picture(), &mut rgba)?;
//! }
//! # Ok(()) }
//! ```
//!
//! Frames come out in presentation order. Times are the track's presentation times (its edit list
//! applied) in seconds; [`StreamInfo::start_seconds`] is when the first picture is shown.
//!
//! Everything read from the file is hostile: a sample larger than
//! [`StreamOptions::max_sample_bytes`] is refused before it is read, the decoders' picture memory
//! is capped by [`StreamOptions::memory_budget`], and no input panics. A caller that must bound the
//! bytes read in all (a thumbnail) wraps its source (see [`crate::poster`]).

use std::sync::Arc;

use lightcraft_geom::Orientation;

use crate::yuv::{self, Chroma, ColourDescription};

#[cfg(all(feature = "video-frames", not(target_arch = "wasm32")))]
#[path = "video_stream/imp.rs"]
mod imp;

/// Whether this build decodes video (feature `video-frames`, native targets).
pub fn video_decoding_available() -> bool {
    cfg!(all(feature = "video-frames", not(target_arch = "wasm32")))
}

/// What [`VideoStream::open`] says in a build without video decoding.
pub const NO_VIDEO_DECODING: &str = "this build has no video decoding (feature video-frames)";

/// Random-access bytes of a clip: a file read in place, or a buffer. Reads go through `&self`
/// from the decoding thread, so a source is `Send + Sync`.
pub trait ClipSource: Send + Sync {
    /// Total length in bytes.
    fn len(&self) -> u64;
    /// Fill `buf` with the bytes at `offset`; an error (`UnexpectedEof`) past the end.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()>;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn eof() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "read past the end of the clip")
}

impl ClipSource for [u8] {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| eof())?;
        let end = start.checked_add(buf.len()).ok_or_else(eof)?;
        buf.copy_from_slice(self.get(start..end).ok_or_else(eof)?);
        Ok(())
    }
}

impl ClipSource for Vec<u8> {
    fn len(&self) -> u64 {
        self.as_slice().len() as u64
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.as_slice().read_at(offset, buf)
    }
}

impl<T: ClipSource + ?Sized> ClipSource for Arc<T> {
    fn len(&self) -> u64 {
        (**self).len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        (**self).read_at(offset, buf)
    }
}

impl<T: ClipSource + ?Sized> ClipSource for Box<T> {
    fn len(&self) -> u64 {
        (**self).len()
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        (**self).read_at(offset, buf)
    }
}

/// A file read in place, its length taken once when it is wrapped (a clip being written is read
/// as it was when opened).
#[cfg(not(target_arch = "wasm32"))]
pub struct FileSource {
    file: std::fs::File,
    len: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileSource {
    pub fn new(file: std::fs::File) -> std::io::Result<FileSource> {
        let len = file.metadata()?.len();
        Ok(FileSource { file, len })
    }

    pub fn open(path: impl AsRef<std::path::Path>) -> std::io::Result<FileSource> {
        FileSource::new(std::fs::File::open(path)?)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ClipSource for FileSource {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        if offset.checked_add(buf.len() as u64).is_none_or(|end| end > self.len) {
            return Err(eof());
        }
        read_exact_at(&self.file, offset, buf)
    }
}

#[cfg(all(unix, not(target_arch = "wasm32")))]
fn read_exact_at(f: &std::fs::File, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(f, buf, offset)
}

#[cfg(windows)]
fn read_exact_at(f: &std::fs::File, mut offset: u64, mut buf: &mut [u8]) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_read(buf, offset)?;
        if n == 0 {
            return Err(eof());
        }
        offset += n as u64;
        buf = &mut buf[n..];
    }
    Ok(())
}

#[cfg(not(any(unix, windows, target_arch = "wasm32")))]
fn read_exact_at(f: &std::fs::File, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = f;
    f.seek(SeekFrom::Start(offset))?;
    f.read_exact(buf)
}

/// Why a clip's pictures can't be had.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VideoError {
    /// A codec, profile or coding tool the decoders do not handle (yet): the reason is meant for
    /// the person looking at the clip ("Apple ProRes video can't be decoded yet").
    #[error("{0}")]
    Unsupported(String),
    /// The file is damaged or not what it claims.
    #[error("damaged clip: {0}")]
    Malformed(String),
    /// A size in the file is over a limit this reader keeps.
    #[error("too large: {0}")]
    TooLarge(String),
    #[error("{0}")]
    Io(String),
    /// The caller stopped the work.
    #[error("cancelled")]
    Cancelled,
}

impl From<yuv::YuvError> for VideoError {
    fn from(e: yuv::YuvError) -> Self {
        match e {
            yuv::YuvError::Unsupported(m) => VideoError::Unsupported(m),
            yuv::YuvError::Layout(m) => VideoError::Malformed(m),
        }
    }
}

/// The video codecs a stream decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    H264,
    Hevc,
}

impl Codec {
    /// The name people know it by.
    pub fn name(self) -> &'static str {
        match self {
            Codec::H264 => "H.264",
            Codec::Hevc => "HEVC",
        }
    }
}

/// Limits and settings for a [`VideoStream`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamOptions {
    /// Decoder worker threads (1 decodes on the calling thread).
    pub threads: usize,
    /// Bytes of decoded pictures a decoder may hold (current, references, reordering).
    pub memory_budget: u64,
    /// The largest single sample (one coded picture) read; a larger one is an error.
    pub max_sample_bytes: u32,
}

impl Default for StreamOptions {
    fn default() -> Self {
        StreamOptions { threads: 1, memory_budget: 1 << 30, max_sample_bytes: 64 << 20 }
    }
}

/// What a stream holds, from the container.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamInfo {
    pub codec: Codec,
    /// Stored picture size from the sample entry (before the display orientation).
    pub width: u32,
    pub height: u32,
    /// How the stored pictures turn upright: the track header's matrix when it is a pure quarter
    /// turn (a mirrored, scaled or sheared matrix shows the stored picture, as the catalog does).
    pub orientation: Orientation,
    /// The sample entry's `colr` box (`nclx` / `nclc`), when it has one.
    pub container_colour: Option<ColourDescription>,
    /// Media ticks per second.
    pub timescale: u32,
    /// When the first picture is shown and how long the track runs, in seconds.
    pub start_seconds: f64,
    pub duration_seconds: f64,
    /// Coded pictures in the track.
    pub sample_count: usize,
    /// How long one picture is shown (the median sample duration) and its inverse; `None` when
    /// the track gives no durations.
    pub frame_duration_seconds: Option<f64>,
    pub frame_rate: Option<f64>,
    /// The clip's first audio track, described from its sample entry (whether or not anything here
    /// can play it).
    pub audio: Option<AudioTrackInfo>,
}

/// An audio track as its sample entry describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioTrackInfo {
    /// The codec's short name (`aac`, `pcm`, `alac`, `opus`, `ac3`, …).
    pub codec: &'static str,
    pub sample_rate: f64,
    pub channels: u32,
}

/// Decoded samples of one plane, as the decoder made them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FramePlanes {
    /// 8-bit pictures: Y, Cb, Cr.
    U8([Vec<u8>; 3]),
    /// Deeper pictures: Y, Cb, Cr, values below `1 << bit_depth`.
    U16([Vec<u16>; 3]),
}

/// One decoded picture, in presentation order.
#[derive(Clone, Debug)]
pub struct Frame {
    /// Presentation time in media ticks ([`StreamInfo::timescale`]) and in seconds.
    pub pts: i64,
    pub seconds: f64,
    /// A sync (key) picture.
    pub key: bool,
    /// Visible (cropped) luma size.
    pub width: usize,
    pub height: usize,
    pub bit_depth: u8,
    pub chroma: Chroma,
    pub planes: FramePlanes,
    /// Row strides in samples.
    pub y_stride: usize,
    pub uv_stride: usize,
    /// The colour description to convert with: the container's `colr` where it specifies a code
    /// point (ISO/IEC 14496-12 gives it precedence), the bitstream's VUI for the rest; the range
    /// flag is the `nclx` box's, else the bitstream's.
    pub colour: ColourDescription,
    /// Decoded in draft mode ([`VideoStream::set_draft`]): approximate, never for a still.
    pub draft: bool,
}

impl Frame {
    /// The picture for [`yuv::Converter`].
    pub fn picture(&self) -> yuv::Picture<'_> {
        let plane = |i: usize, stride: usize| yuv::Plane {
            samples: match &self.planes {
                FramePlanes::U8(p) => yuv::Samples::U8(&p[i]),
                FramePlanes::U16(p) => yuv::Samples::U16(&p[i]),
            },
            stride,
        };
        yuv::Picture {
            width: self.width,
            height: self.height,
            bit_depth: self.bit_depth,
            chroma: self.chroma,
            y: plane(0, self.y_stride),
            cb: plane(1, self.uv_stride),
            cr: plane(2, self.uv_stride),
        }
    }
}

/// How [`VideoStream::seek`] lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekMode {
    /// The picture shown at the time: decoding starts at the sync sample before it and the pictures
    /// in between are dropped.
    Exact,
    /// The sync picture at or before the time: one picture to decode.
    Keyframe,
}

/// What an exact seek to a time costs, before doing it ([`VideoStream::seek_plan`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SeekPlan {
    /// Coded pictures to decode, the sync picture's included.
    pub frames: usize,
    /// Their bytes.
    pub bytes: u64,
    /// When the sync picture and the target picture are shown.
    pub keyframe_seconds: f64,
    pub target_seconds: f64,
}

/// The track header matrix as an orientation, by the rules `lightcraft-meta` catalogues a clip
/// with: a pure quarter turn (16.16 fixed-point entries within 1/128 of 0 and ±1), else upright.
pub fn matrix_orientation(matrix: &[i32; 9]) -> Orientation {
    let [a, b, _, c, d, _, _, _, _] = *matrix;
    let one = |v: i32| v.checked_sub(0x10000).is_some_and(|d| d.unsigned_abs() < 0x200);
    let neg_one = |v: i32| v.checked_add(0x10000).is_some_and(|d| d.unsigned_abs() < 0x200);
    let zero = |v: i32| v.unsigned_abs() < 0x200;
    if zero(a) && zero(d) && one(b) && neg_one(c) {
        Orientation::Rotate90
    } else if neg_one(a) && neg_one(d) && zero(b) && zero(c) {
        Orientation::Rotate180
    } else if zero(a) && zero(d) && neg_one(b) && one(c) {
        Orientation::Rotate270
    } else {
        Orientation::Normal
    }
}

#[cfg(all(feature = "video-frames", not(target_arch = "wasm32")))]
pub use imp::VideoStream;

/// A clip's video track, decoded picture by picture. This build has no video decoding:
/// [`VideoStream::open`] says so and no stream is ever made.
#[cfg(not(all(feature = "video-frames", not(target_arch = "wasm32"))))]
pub struct VideoStream {
    never: std::convert::Infallible,
}

#[cfg(not(all(feature = "video-frames", not(target_arch = "wasm32"))))]
impl VideoStream {
    pub fn open(_src: impl ClipSource + 'static, _opts: &StreamOptions) -> Result<VideoStream, VideoError> {
        Err(VideoError::Unsupported(NO_VIDEO_DECODING.into()))
    }
    pub fn info(&self) -> &StreamInfo {
        match self.never {}
    }
    pub fn frames_decoded(&self) -> usize {
        match self.never {}
    }
    pub fn bytes_read(&self) -> u64 {
        match self.never {}
    }
    pub fn set_draft(&mut self, _on: bool) {
        match self.never {}
    }
    pub fn set_feed_limit(&mut self, _limit: Option<usize>) {
        match self.never {}
    }
    pub fn seek_plan(&self, _seconds: f64) -> Result<SeekPlan, VideoError> {
        match self.never {}
    }
    pub fn seek(&mut self, _seconds: f64, _mode: SeekMode) -> Result<(), VideoError> {
        match self.never {}
    }
    pub fn next_frame(&mut self) -> Result<Option<Frame>, VideoError> {
        match self.never {}
    }
    pub fn next_frame_until(&mut self, _stop: impl FnMut() -> bool) -> Result<Option<Frame>, VideoError> {
        match self.never {}
    }
    pub fn take_skipped(&mut self) -> Option<Frame> {
        match self.never {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_without_decoding_says_so() {
        let r = VideoStream::open(vec![0u8; 16], &StreamOptions::default());
        if video_decoding_available() {
            assert!(matches!(r, Err(VideoError::Malformed(_))), "16 zero bytes are no clip");
        } else {
            assert!(matches!(r, Err(VideoError::Unsupported(m)) if m == NO_VIDEO_DECODING));
        }
    }

    #[test]
    fn slices_read_in_bounds_only() {
        let bytes = vec![1u8, 2, 3, 4];
        let mut buf = [0u8; 2];
        assert!(bytes.read_at(2, &mut buf).is_ok());
        assert_eq!(buf, [3, 4]);
        assert!(bytes.read_at(3, &mut buf).is_err());
        assert!(bytes.read_at(u64::MAX, &mut buf).is_err());
    }

    #[test]
    fn matrices_map_like_the_catalog() {
        let m = |a: i32, b: i32, c: i32, d: i32| [a, b, 0, c, d, 0, 0, 0, 0x4000_0000];
        assert_eq!(matrix_orientation(&m(0x10000, 0, 0, 0x10000)), Orientation::Normal);
        assert_eq!(matrix_orientation(&m(0, 0x10000, -0x10000, 0)), Orientation::Rotate90);
        assert_eq!(matrix_orientation(&m(-0x10000, 0, 0, -0x10000)), Orientation::Rotate180);
        assert_eq!(matrix_orientation(&m(0, -0x10000, 0x10000, 0)), Orientation::Rotate270);
        // mirrored or extreme: upright, never a panic
        assert_eq!(matrix_orientation(&m(-0x10000, 0, 0, 0x10000)), Orientation::Normal);
        assert_eq!(matrix_orientation(&m(i32::MIN, i32::MAX, i32::MIN, i32::MAX)), Orientation::Normal);
    }
}
