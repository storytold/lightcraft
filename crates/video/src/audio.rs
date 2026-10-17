//! Clip audio: decode the first audio track to interleaved f32 PCM at its own rate
//! ([`AudioReader`]), and convert rates for an output device ([`resample`]).
//!
//! Codecs: AAC-LC (MP4/MOV, Matroska, ADTS in MPEG-TS; the HE-AAC SBR band is not
//! reconstructed, so HE-AAC plays as its band-limited core at half rate), Opus (MP4, Matroska),
//! AC-3 / E-AC-3 (MP4, Matroska, MPEG-TS), all FilmCraft decoders (MIT OR Apache-2.0, no
//! symphonia); MP3 Layer III (MP4, Matroska, MPEG-TS/PS) through `puremp3`; PCM: QuickTime / ISO
//! `lpcm`-family FourCCs, Matroska `A_PCM/*`, Blu-ray and 16-bit DVD LPCM, MXF PCM / AES3.
//! FLAC, ALAC and elementary-stream audio are not read.
//!
//! Sources: ISO/IEC 14496-3 (AAC, ADTS), RFC 6716 / RFC 7845 (Opus), ATSC A/52 (AC-3, E-AC-3) as
//! implemented by FilmCraft; ISO/IEC 11172-3 (MP3) as implemented by `puremp3`; Apple QuickTime
//! File Format and Matroska codec-ID prose specs (PCM layouts); the Blu-ray LPCM header layout from
//! public format notes; packet selection, pre-roll, timing, PCM unpacking and the resampler are own design.

use std::path::Path;

use crate::VideoError;

/// Most channels a clip's audio may have (7.1).
pub const MAX_CHANNELS: usize = 8;
/// Most seconds [`AudioReader::read_range`] returns in one call.
pub const MAX_RANGE_SECONDS: f64 = 3600.0;

/// The decoded audio track's format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioInfo {
    /// `aac`, `opus`, `ac3`, `eac3`.
    pub codec: String,
    /// Source rate in Hz (48 000 for Opus).
    pub sample_rate: u32,
    /// Interleaved channels per frame, in the decoder's order (WAV order for AC-3).
    pub channels: usize,
}

/// Decoded audio: `samples` interleaved, starting at `time` seconds from the clip's first video frame.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioBlock {
    pub time: f64,
    pub samples: Vec<f32>,
}

/// Sequential audio decoding for one clip.
pub struct AudioReader {
    #[cfg(not(target_arch = "wasm32"))]
    inner: crate::audio_native::Reader,
    #[cfg(target_arch = "wasm32")]
    info: AudioInfo,
}

impl AudioReader {
    /// Open the clip's first decodable audio track. [`VideoError::Unsupported`] when it has none.
    pub fn open(path: &Path) -> Result<Self, VideoError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Ok(Self { inner: crate::audio_native::Reader::open(path)? })
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = path;
            Err(VideoError::Unsupported("video is not available in the web build".into()))
        }
    }

    pub fn info(&self) -> &AudioInfo {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.info()
        }
        #[cfg(target_arch = "wasm32")]
        {
            &self.info
        }
    }

    /// Position so the next block starts at `t` seconds.
    pub fn seek(&mut self, t: f64) {
        #[cfg(not(target_arch = "wasm32"))]
        self.inner.seek(t);
        #[cfg(target_arch = "wasm32")]
        let _ = t;
    }

    /// The next decoded block; None at the end of the track. Undecodable packets are skipped
    /// (a long run of them is an error).
    pub fn next_block(&mut self) -> Result<Option<AudioBlock>, VideoError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.next_block()
        }
        #[cfg(target_arch = "wasm32")]
        {
            Ok(None)
        }
    }

    /// Blocks from `t` seconds on, to the end of the track.
    pub fn samples_from(&mut self, t: f64) -> impl Iterator<Item = Result<AudioBlock, VideoError>> + '_ {
        self.seek(t);
        let mut done = false;
        std::iter::from_fn(move || {
            if done {
                return None;
            }
            let r = self.next_block().transpose();
            done = !matches!(r, Some(Ok(_)));
            r
        })
    }

    /// Interleaved samples of `[start, end)` seconds (a trimmed range; `end` None = to the end of the
    /// track, at most [`MAX_RANGE_SECONDS`]). Gaps in the track are not filled: the result is
    /// the decoded samples from `start` on.
    pub fn read_range(&mut self, start: f64, end: Option<f64>) -> Result<Vec<f32>, VideoError> {
        let start = if start.is_finite() { start.max(0.0) } else { 0.0 };
        let end = end.filter(|e| e.is_finite()).unwrap_or(f64::INFINITY).min(start + MAX_RANGE_SECONDS);
        if end <= start {
            return Ok(Vec::new());
        }
        let (rate, ch) = (f64::from(self.info().sample_rate), self.info().channels.max(1));
        let frames = ((end - start) * rate).round();
        let want = if frames.is_finite() && frames > 0.0 { (frames as usize).saturating_mul(ch) } else { 0 };
        let mut out = Vec::with_capacity(want.min(1 << 24));
        for b in self.samples_from(start) {
            let b = b?;
            let room = want.saturating_sub(out.len());
            out.extend(b.samples.iter().take(room));
            if out.len() >= want {
                break;
            }
        }
        Ok(out)
    }
}

/// Convert interleaved `input` at `from` Hz to `to` Hz (linear interpolation; enough for preview
/// playback). Returns `input` unchanged when the rates match or are invalid.
pub fn resample(input: &[f32], channels: usize, from: u32, to: u32) -> Vec<f32> {
    if channels == 0 || from == 0 || to == 0 || from == to {
        return input.to_vec();
    }
    let frames = input.len() / channels;
    if frames == 0 {
        return Vec::new();
    }
    let out_frames = ((frames as u128 * u128::from(to)) / u128::from(from)).min(1 << 31) as usize;
    let step = f64::from(from) / f64::from(to);
    let mut out = Vec::with_capacity(out_frames.saturating_mul(channels));
    for k in 0..out_frames {
        let pos = k as f64 * step;
        let i = pos as usize;
        let f = (pos - i as f64) as f32;
        let j = (i + 1).min(frames - 1);
        for c in 0..channels {
            let a = input.get(i * channels + c).copied().unwrap_or(0.0);
            let b = input.get(j * channels + c).copied().unwrap_or(a);
            out.push(a + (b - a) * f);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_doubles_and_halves() {
        let x: Vec<f32> = (0..100).flat_map(|i| [i as f32, -(i as f32)]).collect();
        let up = resample(&x, 2, 24_000, 48_000);
        assert_eq!(up.len(), 400);
        assert!((up[2] - 0.5).abs() < 1e-6 && (up[3] + 0.5).abs() < 1e-6);
        let down = resample(&x, 2, 48_000, 24_000);
        assert_eq!(down.len(), 100);
        assert_eq!(down[2], 2.0);
        assert_eq!(resample(&x, 2, 48_000, 48_000), x);
        assert!(resample(&[], 2, 1, 2).is_empty());
        assert_eq!(resample(&x, 0, 1, 2), x);
        // odd tail and huge ratios don't panic
        assert!(resample(&[1.0, 2.0, 3.0], 2, 8_000, 192_000).len() >= 2);
    }
}
