//! A clip's frames for [`crate::video_play`]: the codecs crate's [`VideoStream`] (MP4/MOV demuxing,
//! H.264 / HEVC decoding) and its Y′CbCr to display-sRGB [`Converter`], run on the decode thread.
//!
//! Builds without the codecs' `video-frames` feature compile this too: opening a clip then fails
//! with the stream's "no video decoding in this build" reason, which the loupe shows.

use lightcraft_codecs::video_stream::{SeekMode, StreamOptions, VideoError, VideoStream};
use lightcraft_codecs::yuv::{ColourDescription, Converter};

use crate::video_play::{FrameSource, Next, SourceInfo, VideoFrame};

/// Decoder threads for playback: enough for 4K HEVC Main 10 at several times real time on a
/// laptop, leaving cores for the UI and the GPU driver.
const PLAYBACK_THREADS: usize = 6;
/// Decoded pictures a playback decoder may hold (references and reordering), per clip.
const DECODER_MEMORY: u64 = 768 << 20;
/// The largest coded picture read (a 4K intra picture at a high bit rate is a few MB).
const MAX_SAMPLE_BYTES: u32 = 64 << 20;

/// Whether this build can decode video at all (the codecs' `video-frames` feature).
pub fn available() -> bool {
    lightcraft_codecs::video_stream::video_decoding_available()
}

/// A sentence for people about why a clip can't be played.
pub fn describe(e: &VideoError) -> String {
    match e {
        VideoError::Unsupported(why) => why.clone(),
        VideoError::Malformed(why) => format!("The clip is damaged: {why}"),
        VideoError::TooLarge(why) => format!("The clip is too large to play: {why}"),
        VideoError::Io(why) => format!("The clip could not be read: {why}"),
        VideoError::Cancelled => "Stopped".to_string(),
    }
}

/// The clip at `path` (a file on this computer), ready to play.
#[cfg(not(target_arch = "wasm32"))]
pub fn open_file(path: &str) -> Result<Box<dyn FrameSource>, String> {
    let file = lightcraft_codecs::video_stream::FileSource::open(path).map_err(|e| format!("The clip could not be opened: {e}"))?;
    open(file).map(|s| Box::new(s) as Box<dyn FrameSource>)
}

/// Playback is native only: the web build has no video decoding.
#[cfg(target_arch = "wasm32")]
pub fn open_file(_path: &str) -> Result<Box<dyn FrameSource>, String> {
    Err(lightcraft_codecs::video_stream::NO_VIDEO_DECODING.to_string())
}

/// A clip from any byte source (tests open clips held in memory).
pub fn open(src: impl lightcraft_codecs::video_stream::ClipSource + 'static) -> Result<CodecSource, String> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, PLAYBACK_THREADS);
    let opts = StreamOptions { threads, memory_budget: DECODER_MEMORY, max_sample_bytes: MAX_SAMPLE_BYTES };
    let stream = VideoStream::open(src, &opts).map_err(|e| describe(&e))?;
    let si = stream.info();
    let audio = si.audio.as_ref().map(|a| {
        let channels = match a.channels {
            1 => "mono".to_string(),
            2 => "stereo".to_string(),
            n => format!("{n} channels"),
        };
        format!("{}, {:.1} kHz, {channels}", audio_codec_name(a.codec), a.sample_rate / 1000.0).replace(".0 kHz", " kHz")
    });
    let info = SourceInfo {
        duration: if si.duration_seconds.is_finite() { si.duration_seconds.max(0.0) } else { 0.0 },
        width: si.width,
        height: si.height,
        frame_duration: si.frame_duration_seconds.filter(|d| d.is_finite() && *d > 0.0).unwrap_or(1.0 / 30.0),
        orientation: si.orientation,
        codec: si.codec.name().to_string(),
        audio,
    };
    let start = if si.start_seconds.is_finite() { si.start_seconds } else { 0.0 };
    Ok(CodecSource { stream, info, start, converter: None })
}

fn audio_codec_name(codec: &str) -> String {
    match codec {
        "aac" => "AAC".to_string(),
        "pcm" | "lpcm" => "PCM".to_string(),
        "alac" => "Apple Lossless".to_string(),
        "ac3" => "AC-3".to_string(),
        "eac3" => "E-AC-3".to_string(),
        "opus" => "Opus".to_string(),
        "mp3" => "MP3".to_string(),
        other => other.to_uppercase(),
    }
}

/// [`FrameSource`] over a [`VideoStream`].
pub struct CodecSource {
    stream: VideoStream,
    info: SourceInfo,
    /// When the first picture is shown on the stream's timeline: playback time 0.
    start: f64,
    /// The converter for the current colour description, bit depth and size.
    converter: Option<((ColourDescription, u8, usize, usize), Converter)>,
}

impl CodecSource {
    fn convert(&mut self, f: &lightcraft_codecs::video_stream::Frame, mut buffer: Vec<u8>) -> Result<VideoFrame, String> {
        let key = (f.colour, f.bit_depth, f.width, f.height);
        if self.converter.as_ref().is_none_or(|(k, _)| *k != key) {
            let c = Converter::new(f.colour, f.bit_depth, f.width, f.height).map_err(|e| describe(&e.into()))?;
            self.converter = Some((key, c));
        }
        let Some((_, conv)) = &self.converter else {
            return Err("no colour converter".into());
        };
        let len = f.width.checked_mul(f.height).and_then(|n| n.checked_mul(4)).ok_or("picture too large")?;
        buffer.clear();
        buffer.resize(len, 0);
        conv.to_srgb8(&f.picture(), bytemuck::cast_slice_mut(&mut buffer)).map_err(|e| describe(&e.into()))?;
        Ok(VideoFrame {
            pts: f.seconds - self.start,
            width: u32::try_from(f.width).map_err(|_| "picture too wide")?,
            height: u32::try_from(f.height).map_err(|_| "picture too tall")?,
            rgba: buffer,
            draft: f.draft,
        })
    }
}

impl FrameSource for CodecSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn seek(&mut self, seconds: f64) -> Result<(), String> {
        self.stream.seek(seconds + self.start, SeekMode::Exact).map_err(|e| describe(&e))
    }

    fn next_frame(&mut self, stop: &mut dyn FnMut() -> bool, buffer: Vec<u8>) -> Result<Next, String> {
        match self.stream.next_frame_until(stop) {
            Ok(Some(f)) => self.convert(&f, buffer).map(Next::Frame),
            Ok(None) => Ok(Next::End),
            Err(VideoError::Cancelled) => Ok(Next::Stopped),
            Err(e) => Err(describe(&e)),
        }
    }

    fn set_draft(&mut self, on: bool) {
        self.stream.set_draft(on);
    }
}

#[cfg(test)]
#[path = "video_source_tests.rs"]
mod tests;
