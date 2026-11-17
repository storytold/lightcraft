//! A clip's frames for [`crate::video_play`]: the codecs crate's [`VideoStream`] (MP4/MOV demuxing,
//! H.264 / HEVC decoding) and its Y′CbCr to display-sRGB [`Converter`], run on the decode thread.
//!
//! Builds without the codecs' `video-frames` feature compile this too: opening a clip then fails
//! with the stream's "no video decoding in this build" reason, which the loupe shows.

use lightcraft_codecs::video_stream::{Frame, SeekMode, StreamOptions, VideoError, VideoStream};
use lightcraft_codecs::yuv::{ColourDescription, Converter, Picture, Plane, Samples};
use rayon::prelude::*;

use crate::video_play::{FrameSource, Next, SourceInfo, VideoFrame};

/// Decoder threads for playback: enough for 4K HEVC Main 10 at several times real time on a
/// laptop, leaving cores for the UI and the GPU driver.
const PLAYBACK_THREADS: usize = 6;
/// Decoded pictures a playback decoder may hold (references and reordering), per clip.
const DECODER_MEMORY: u64 = 768 << 20;
/// The largest coded picture read (a 4K intra picture at a high bit rate is a few MB).
const MAX_SAMPLE_BYTES: u32 = 64 << 20;
/// While playing, pictures larger than this (pixels) are shown at half size: converting a 4K
/// picture to sRGB takes 50 to 65 ms on a 14-core laptop, more than a frame lasts, and the loupe
/// shows it at about half size anyway. Half of 4K is 1920 × 1080; a paused frame is full size.
const HALVE_ABOVE: usize = 2_600_000;

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
    let convert = Convert { converter: None, draft: false, half: [Vec::new(), Vec::new(), Vec::new()], start };
    Ok(CodecSource { stream, info, convert, ahead: None })
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

/// [`FrameSource`] over a [`VideoStream`]. While one picture is converted to sRGB (on a scoped
/// thread), the next is decoded: at 4K each takes 20 to 30 ms, so in turn they would not keep up.
pub struct CodecSource {
    stream: VideoStream,
    info: SourceInfo,
    /// Converts pictures: its colour converter and half-size planes.
    convert: Convert,
    /// The picture decoded ahead, not yet converted; or why the stream has no more (its end, a
    /// stop, an error), reported once the picture before it is out.
    ahead: Option<Ahead>,
}

enum Ahead {
    Picture(Frame),
    End,
    Stopped,
    Failed(String),
}

/// The conversion side of a [`CodecSource`] (separate so it can run beside the decoder).
struct Convert {
    /// The converter for the current colour description, bit depth and size.
    converter: Option<((ColourDescription, u8, usize, usize), Converter)>,
    /// Playing: big pictures are converted at half size ([`HALVE_ABOVE`]).
    draft: bool,
    /// The half-size planes (Y, Cb, Cr), reused from frame to frame.
    half: [Vec<u16>; 3],
    /// When the first picture is shown on the stream's timeline: playback time 0.
    start: f64,
}

impl Convert {
    fn frame(&mut self, f: &Frame, mut buffer: Vec<u8>) -> Result<VideoFrame, String> {
        let full = f.picture();
        let halve = self.draft && f.width.saturating_mul(f.height) > HALVE_ABOVE && f.width >= 2 && f.height >= 2;
        let pic = if halve { half_size(&full, &mut self.half) } else { full };
        let key = (f.colour, f.bit_depth, pic.width, pic.height);
        if self.converter.as_ref().is_none_or(|(k, _)| *k != key) {
            // (the full size picks the defaults for unspecified code points)
            let c = Converter::new(f.colour, f.bit_depth, f.width, f.height).map_err(|e| describe(&e.into()))?;
            self.converter = Some((key, c));
        }
        let Some((_, conv)) = &self.converter else {
            return Err("no colour converter".into());
        };
        let len = pic.width.checked_mul(pic.height).and_then(|n| n.checked_mul(4)).ok_or("picture too large")?;
        buffer.clear();
        buffer.resize(len, 0);
        conv.to_srgb8(&pic, bytemuck::cast_slice_mut(&mut buffer)).map_err(|e| describe(&e.into()))?;
        Ok(VideoFrame {
            pts: f.seconds - self.start,
            width: u32::try_from(pic.width).map_err(|_| "picture too wide")?,
            height: u32::try_from(pic.height).map_err(|_| "picture too tall")?,
            rgba: buffer,
            // a half-size frame is replaced by the full picture when playback pauses on it
            draft: f.draft || halve,
        })
    }
}

/// The stream's next picture, or why there is none.
fn decode(stream: &mut VideoStream, stop: &mut dyn FnMut() -> bool) -> Ahead {
    match stream.next_frame_until(stop) {
        Ok(Some(f)) => Ahead::Picture(f),
        Ok(None) => Ahead::End,
        Err(VideoError::Cancelled) => Ahead::Stopped,
        Err(e) => Ahead::Failed(describe(&e)),
    }
}

impl FrameSource for CodecSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn seek(&mut self, seconds: f64) -> Result<(), String> {
        self.ahead = None;
        self.stream.seek(seconds + self.convert.start, SeekMode::Exact).map_err(|e| describe(&e))
    }

    fn next_frame(&mut self, stop: &mut dyn FnMut() -> bool, buffer: Vec<u8>) -> Result<Next, String> {
        let current = match self.ahead.take() {
            Some(a) => a,
            None => decode(&mut self.stream, stop),
        };
        let f = match current {
            Ahead::Picture(f) => f,
            Ahead::End => return Ok(Next::End),
            Ahead::Stopped => return Ok(Next::Stopped),
            Ahead::Failed(e) => return Err(e),
        };
        // a stop already asked for: no need to decode ahead
        if stop() {
            return self.convert.frame(&f, buffer).map(Next::Frame);
        }
        let (convert, stream) = (&mut self.convert, &mut self.stream);
        let (converted, ahead) = std::thread::scope(|scope| {
            let worker = std::thread::Builder::new().name("lc-video-convert".into()).spawn_scoped(scope, || convert.frame(&f, buffer));
            let ahead = decode(stream, stop);
            let converted = match worker {
                Ok(h) => h.join().unwrap_or_else(|_| Err("the colour conversion failed".to_string())),
                Err(e) => Err(format!("no thread for the colour conversion: {e}")),
            };
            (converted, ahead)
        });
        self.ahead = Some(ahead);
        converted.map(Next::Frame)
    }

    fn set_draft(&mut self, on: bool) {
        self.convert.draft = on;
        self.stream.set_draft(on);
    }
}

/// `pic` at half its width and height (rounded down), each plane averaged over 2 × 2 samples, in
/// `planes` (Y, Cb, Cr; reused). Samples past an odd edge repeat the last row or column.
fn half_size<'a>(pic: &Picture<'_>, planes: &'a mut [Vec<u16>; 3]) -> Picture<'a> {
    let (w, h) = ((pic.width / 2).max(1), (pic.height / 2).max(1));
    let (cw, ch) = pic.chroma.plane_size(w, h);
    let (scw, sch) = pic.chroma.plane_size(pic.width, pic.height);
    let [y, cb, cr] = planes;
    halve_plane(&pic.y, (pic.width, pic.height), (w, h), y);
    halve_plane(&pic.cb, (scw, sch), (cw, ch), cb);
    halve_plane(&pic.cr, (scw, sch), (cw, ch), cr);
    let plane = |p: &'a Vec<u16>, stride: usize| Plane { samples: Samples::U16(p), stride };
    Picture { width: w, height: h, bit_depth: pic.bit_depth, chroma: pic.chroma, y: plane(y, w), cb: plane(cb, cw), cr: plane(cr, cw) }
}

/// One plane of `src_size` samples averaged 2 × 2 into `out` (`size`, rows packed), rows in parallel.
fn halve_plane(src: &Plane<'_>, (sw, sh): (usize, usize), (w, h): (usize, usize), out: &mut Vec<u16>) {
    out.clear();
    out.resize(w.saturating_mul(h), 0);
    if w == 0 || sw == 0 || sh == 0 {
        return;
    }
    let at = |x: usize, y: usize| -> u32 {
        let i = y.min(sh - 1).saturating_mul(src.stride).saturating_add(x.min(sw - 1));
        match src.samples {
            Samples::U8(s) => s.get(i).map_or(0, |&v| u32::from(v)),
            Samples::U16(s) => s.get(i).map_or(0, |&v| u32::from(v)),
        }
    };
    out.par_chunks_mut(w).enumerate().for_each(|(row, line)| {
        let (y0, y1) = (2 * row, 2 * row + 1);
        for (x, o) in line.iter_mut().enumerate() {
            let (x0, x1) = (2 * x, 2 * x + 1);
            let sum = at(x0, y0) + at(x1, y0) + at(x0, y1) + at(x1, y1);
            *o = u16::try_from((sum + 2) / 4).unwrap_or(u16::MAX);
        }
    });
}

#[cfg(test)]
#[path = "video_source_tests.rs"]
mod tests;
