//! Video clips (L3): probe a file, read frames at a time, pick a poster frame.
//!
//! - [`probe`]: duration, frame rate, size, codec, audio presence, creation date and GPS.
//! - [`FrameReader`]: decodes the frame shown at a time, optionally decimated to a maximum side
//!   (draft decode for playback and thumbnails), as linear Rec.709 RGB ([`lcc_raster::Rgb32f`],
//!   the type the pipeline takes for JPEG/TIFF input).
//! - [`AudioReader`]: the first audio track as interleaved f32 PCM (AAC, Opus, AC-3); [`resample`].
//! - [`poster`]: one frame for grid thumbnails (default 10 % into the clip).
//!
//! Containers and codecs come from FilmCraft (pure Rust, clean-room): the MP4/MOV, Matroska/WebM,
//! MPEG-TS/PS and MXF demuxers; the H.264, HEVC, VP9, AV1, ProRes, DNxHD/DNxHR, APV and MPEG-1/2
//! decoders; and its colour maths (`filmcraft-color`, `filmcraft-frame`). Each container becomes
//! one sample table and each codec one decoder (`demux`, `decode`), so seeking is shared. Y'CbCr
//! is converted honouring the stream's colour description (the container first, then the
//! bitstream, then size-based defaults); HLG and PQ are tone-mapped to SDR (BT.2390 EETF) and
//! wide-gamut primaries are converted to Rec.709 for v1. Interlaced H.264 is reported as
//! [`VideoError::Unsupported`] rather than decoded wrongly.
//!
//! Never-crash: every input-derived number is checked, and every call into FilmCraft runs under
//! `catch_unwind`, so a malformed file is a [`VideoError`], never a panic.
//!
//! Native only: on wasm32 the API exists but every call returns [`VideoError::Unsupported`].
//!
//! Sources: ISO/IEC 14496-12 (MP4), Matroska (RFC 9559), ISO/IEC 13818-1 (TS), SMPTE ST 377-1 (MXF)
//! and ITU-T H.273 colour code points as implemented by FilmCraft; the per-codec frame mapping follows
//! `filmcraft-codecs` (MIT OR Apache-2.0);
//! ITU-R BT.2100 (HLG OOTF, PQ) and BT.2390 (tone mapping) through `filmcraft-color`; ISO 6709
//! location strings; frame selection and seeking are own design.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::Path;

use lcc_raster::Rgb32f;

pub use audio::{AudioBlock, AudioInfo, AudioReader, MAX_CHANNELS, MAX_RANGE_SECONDS, resample};

mod audio;
#[cfg(not(target_arch = "wasm32"))]
mod audio_native;
#[cfg(not(target_arch = "wasm32"))]
mod colour;
#[cfg(not(target_arch = "wasm32"))]
mod decode;
#[cfg(not(target_arch = "wasm32"))]
mod demux;
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
mod source;
#[cfg(all(feature = "test-util", not(target_arch = "wasm32")))]
pub mod test_util;

/// What went wrong reading a video.
#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    #[error("I/O error: {0}")]
    Io(String),
    /// The file is not a container we read, or is malformed.
    #[error("invalid video: {0}")]
    Invalid(String),
    /// A valid file using something we can't decode (codec, interlacing, 4:2:2, the web build…).
    #[error("unsupported video: {0}")]
    Unsupported(String),
    /// The decoder rejected the bitstream.
    #[error("decode error: {0}")]
    Decode(String),
    /// A FilmCraft call panicked; caught at this crate's boundary.
    #[error("internal decoder failure: {0}")]
    Internal(String),
}

/// What [`probe`] learns about a clip.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoInfo {
    /// Seconds.
    pub duration: f64,
    pub fps_num: u32,
    pub fps_den: u32,
    /// Display size: after the pixel aspect ratio (anamorphic clips are stretched, never
    /// squeezed) and the container's rotation. Frames come out at this aspect.
    pub width: u32,
    pub height: u32,
    /// `h264`, `hevc`, `vp9`, `av1`, `prores`, `dnxhd`, `apv`, `mpeg2`; for codecs we don't decode,
    /// the container's name for them (MP4 fourcc, Matroska / TS codec name).
    pub codec: String,
    pub has_audio: bool,
    /// Creation date as written in the file (QuickTime `creationdate` / `©day`), RFC 3339 when it is.
    pub created: Option<String>,
    /// (latitude, longitude) in degrees, from an ISO 6709 location.
    pub gps: Option<(f64, f64)>,
    /// Clockwise quarter turns applied for display.
    pub rotation: u8,
    /// HLG or PQ transfer (frames are tone-mapped to SDR).
    pub hdr: bool,
    /// Whether [`FrameReader`] can decode this clip's video.
    pub decodable: bool,
    /// Stored (coded, cropped) picture size, after rotation; differs from `width`/`height` for
    /// non-square pixels.
    pub coded_width: u32,
    pub coded_height: u32,
    /// Pixel (sample) aspect ratio, reduced; 1:1 for square pixels. From the container (MP4
    /// `pasp`, Matroska `DisplayWidth`/`DisplayHeight`, MXF aspect ratio), else the bitstream
    /// (H.264 / HEVC VUI, MPEG-2 sequence header).
    pub par_num: u32,
    pub par_den: u32,
    /// Interlaced MPEG-2 sequence: frames are deinterlaced (linear, first field kept) instead of
    /// showing woven combing.
    pub interlaced: bool,
}

/// One decoded frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Linear Rec.709 RGB.
    pub image: Rgb32f,
    /// Presentation time of this frame, seconds.
    pub time: f64,
}

/// Probe a clip's container and stream headers (no decoding).
pub fn probe(path: &Path) -> Result<VideoInfo, VideoError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        native::probe(path)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path;
        Err(unsupported_web())
    }
}

/// The time [`poster`] uses when none is given: 10 % of the duration.
pub fn default_poster_time(duration: f64) -> f64 {
    if duration.is_finite() && duration > 0.0 { duration * 0.1 } else { 0.0 }
}

/// A poster frame at `t` seconds (None = [`default_poster_time`]), at most `max_side` pixels.
pub fn poster(path: &Path, t: Option<f64>, max_side: u32) -> Result<Frame, VideoError> {
    let mut r = FrameReader::open(path)?;
    let t = t.unwrap_or_else(|| default_poster_time(r.info().duration));
    r.frame_at(t, Some(max_side))
}

/// Sequential/random-access frame decoding for one clip. Reuses the decoder while times move
/// forward inside a GOP, so playback and frame stepping don't re-decode from the keyframe.
pub struct FrameReader {
    #[cfg(not(target_arch = "wasm32"))]
    inner: native::Reader,
    #[cfg(target_arch = "wasm32")]
    info: VideoInfo,
}

impl FrameReader {
    /// Open a clip for decoding. Fails for files without a decodable video track.
    pub fn open(path: &Path) -> Result<Self, VideoError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Ok(Self { inner: native::Reader::open(path)? })
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = path;
            Err(unsupported_web())
        }
    }

    pub fn info(&self) -> &VideoInfo {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.info()
        }
        #[cfg(target_arch = "wasm32")]
        {
            &self.info
        }
    }

    /// Times of every frame in presentation order, seconds (for frame stepping).
    pub fn frame_times(&self) -> Vec<f64> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.frame_times()
        }
        #[cfg(target_arch = "wasm32")]
        {
            Vec::new()
        }
    }

    /// Move the playback cursor so [`FrameReader::next_frame`] returns the frame shown at `t`
    /// seconds next (clamped to the clip). Decoding happens on the next call, not here.
    pub fn seek(&mut self, t: f64) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.seek(t)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = t;
        }
    }

    /// The next frame in presentation order (after the last [`FrameReader::frame_at`] /
    /// [`FrameReader::next_frame`], or from [`FrameReader::seek`]); `None` at the end of the clip.
    /// Decodes forward without re-seeking, so a decode-ahead playback worker costs one decode
    /// per frame. `max_side` as in [`FrameReader::frame_at`].
    pub fn next_frame(&mut self, max_side: Option<u32>) -> Result<Option<Frame>, VideoError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.next_frame(max_side)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = max_side;
            Err(unsupported_web())
        }
    }

    /// [`FrameReader::seek`] to `t`, then iterate [`FrameReader::next_frame`] to the end of the
    /// clip (stops after the first error).
    pub fn frames_from(&mut self, t: f64, max_side: Option<u32>) -> impl Iterator<Item = Result<Frame, VideoError>> + '_ {
        self.seek(t);
        let mut failed = false;
        std::iter::from_fn(move || {
            if failed {
                return None;
            }
            let r = self.next_frame(max_side).transpose();
            failed = matches!(r, Some(Err(_)));
            r
        })
    }

    /// The frame displayed at `t` seconds (clamped to the clip), decimated so its longer side is
    /// at most `max_side` when given.
    pub fn frame_at(&mut self, t: f64, max_side: Option<u32>) -> Result<Frame, VideoError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.frame_at(t, max_side)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (t, max_side);
            Err(unsupported_web())
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn unsupported_web() -> VideoError {
    VideoError::Unsupported("video is not supported in the web build".into())
}

/// Parse an ISO 6709 point such as `+37.3317-122.0307+010.000/` into (lat, lon).
pub fn parse_iso6709(s: &str) -> Option<(f64, f64)> {
    let s = s.trim().trim_end_matches('/');
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if i > 0 && (c == '+' || c == '-') {
            parts.push(s.get(start..i)?);
            start = i;
        }
    }
    parts.push(s.get(start..)?);
    let lat: f64 = parts.first()?.parse().ok()?;
    let lon: f64 = parts.get(1)?.parse().ok()?;
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso6709() {
        assert_eq!(parse_iso6709("+37.3317-122.0307+010.000/"), Some((37.3317, -122.0307)));
        assert_eq!(parse_iso6709("-33.8688+151.2093/"), Some((-33.8688, 151.2093)));
        assert_eq!(parse_iso6709("+99.0+0.0/"), None);
        assert_eq!(parse_iso6709("garbage"), None);
        assert_eq!(parse_iso6709(""), None);
        assert_eq!(parse_iso6709("+é-1"), None);
    }
}
