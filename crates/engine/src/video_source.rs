//! A clip's frames for [`crate::video_play`]: the codecs crate's [`VideoStream`] (MP4/MOV demuxing,
//! H.264 / HEVC decoding) and its Y′CbCr to display-sRGB [`Converter`], run on the decode thread.
//!
//! Builds without the codecs' `video-frames` feature compile this too: opening a clip then fails
//! with the stream's "no video decoding in this build" reason, which the loupe shows.

use lightcraft_codecs::video_stream::{Frame, SeekMode, StreamOptions, VideoError, VideoStream};
use lightcraft_codecs::yuv::{ColourDescription, Converter, Picture, Plane, Samples};
use web_time::Instant;

use rayon::prelude::*;

use crate::video_play::{FrameSource, Next, SourceInfo, VideoFrame};

/// Decoder threads for playback: enough for 4K HEVC Main 10 at several times real time on a
/// laptop, leaving cores for the UI and the GPU driver.
const PLAYBACK_THREADS: usize = 6;
/// Pictures at least this big (pixels) may be shown at half size while playback falls behind
/// ([`Adapt`]); smaller ones always play at full size.
const HALVE_MIN: usize = 1_000_000;
/// Back to full size once it would take at most this share of a frame's time…
const FULL_FITS: f64 = 0.6;
/// …for this many frames in a row (doubled each time full size fell behind again, up to
/// [`FULL_AFTER_MAX`], so a machine on the edge does not flip between the two every second).
const FULL_AFTER: u32 = 30;
const FULL_AFTER_MAX: u32 = 960;
/// After the decoder was the slower part, half size returns (playback still behind) only once a
/// full-size conversion takes this many times the decode, so timings that hover around each other
/// don't switch sizes every frame.
const HELPS_AGAIN: f64 = 1.5;

/// When frames are converted at half size: only while playback falls behind (the player drops
/// late frames) and only while that helps. Decoding and converting run side by side, so the slower
/// of the two sets the pace; half size averages 2 × 2 samples and makes the conversion about four
/// times cheaper, which speeds playback only when the conversion is the slower one. Back to full
/// size once the decoder and a full-size conversion fit in a frame's time with room to spare, or
/// at once when the decoder alone is the slower one (half size would only cost picture). Paused,
/// stepped and scrubbed frames are always full size.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Adapt {
    /// Converting at half size now.
    half: bool,
    /// The player says playback is falling behind.
    behind: bool,
    /// Frames in a row a full-size conversion would have fitted.
    fits: u32,
    /// How many of those it takes to go back to full size.
    need: u32,
    /// Full size fell behind since the last time it was chosen.
    tried_full: bool,
    /// The latest decode time and (estimated) full-size conversion time, in seconds.
    decode: f64,
    full: f64,
}

impl Default for Adapt {
    fn default() -> Adapt {
        Adapt { half: false, behind: false, fits: 0, need: FULL_AFTER, tried_full: false, decode: 0.0, full: 0.0 }
    }
}

impl Adapt {
    /// Half size would speed playback up: the conversion, not the decoder, sets the pace (or
    /// nothing has been measured yet).
    fn helps(&self) -> bool {
        self.full > self.decode || (self.full == 0.0 && self.decode == 0.0)
    }

    fn set_behind(&mut self, behind: bool) {
        self.behind = behind;
        if behind && !self.half && self.helps() {
            self.halve();
        }
    }

    fn halve(&mut self) {
        self.half = true;
        self.fits = 0;
        // full size was tried and fell behind again: wait longer before the next try
        if self.tried_full {
            self.need = self.need.saturating_mul(2).min(FULL_AFTER_MAX);
        }
    }

    /// One frame went through: `decode` and `convert` seconds it took (side by side; `convert`
    /// is the colour conversion alone, without the 2 × 2 averaging), whether it was `halved`, and
    /// how long a `frame` lasts.
    fn after(&mut self, decode: f64, convert: f64, halved: bool, frame: f64) {
        self.decode = decode;
        // a full-size conversion costs about four times a half-size one
        self.full = if halved { convert * 4.0 } else { convert };
        if !self.half {
            // still behind, and the conversion is clearly the slower part again (the player says
            // "behind" only when that changes): half size helps once more
            if self.behind && self.full > self.decode * HELPS_AGAIN {
                self.halve();
            }
            return;
        }
        if !self.helps() {
            // the decoder is the limit: half size can't rescue playback, show the full picture
            // (a later return to half size counts as full size having fallen behind: back-off)
            self.half = false;
            self.fits = 0;
            self.tried_full = true;
            return;
        }
        if !self.behind && decode.max(self.full) <= frame * FULL_FITS {
            self.fits = self.fits.saturating_add(1);
        } else {
            self.fits = 0;
        }
        if self.fits >= self.need {
            self.half = false;
            self.fits = 0;
            self.tried_full = true;
        }
    }
}

/// Why nothing plays in a build without video decoding (for people; the UI translates it).
pub use lightcraft_codecs::video_stream::NO_VIDEO_DECODING;

/// Whether this build can decode video at all (the codecs' `video-frames` feature).
pub fn available() -> bool {
    lightcraft_codecs::video_stream::video_decoding_available()
}

/// Why a clip can't be played, in the words the poster card uses (plain sentences the UI
/// translates; a damaged clip's technical detail goes to the log).
fn reason(e: &VideoError) -> String {
    crate::files::video_reason(e)
}

/// The clip at `path` (a file on this computer), ready to play. Why it can't be opened is worded
/// as the poster card words it (translated in the UI, no path: the bar shows it); the log keeps
/// the system's own message.
#[cfg(not(target_arch = "wasm32"))]
pub fn open_file(path: &str) -> Result<Box<dyn FrameSource>, String> {
    let file = lightcraft_codecs::video_stream::FileSource::open(path).map_err(|e| {
        log::warn!("video playback: {path}: {e}");
        crate::files::open_failure(e.kind()).to_string()
    })?;
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
    open_with(src, &options(threads))
}

/// The stream options playback decodes with on `threads` decoder threads. The decoder's memory
/// is what the clip's sequence header asks for on them, up to the codecs' ceiling (one figure
/// for posters and playback); over it the clip is too large to decode, never damaged.
fn options(threads: usize) -> StreamOptions {
    StreamOptions { threads, ..StreamOptions::default() }
}

/// [`open`] with the stream's options given (tests fix the decoder threads and the ceiling).
fn open_with(src: impl lightcraft_codecs::video_stream::ClipSource + 'static, opts: &StreamOptions) -> Result<CodecSource, String> {
    let stream = VideoStream::open(src, opts).map_err(|e| reason(&e))?;
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
        decoder_bytes: si.decoder_bytes,
    };
    let start = if si.start_seconds.is_finite() { si.start_seconds } else { 0.0 };
    let convert = Convert { converter: None, draft: false, adapt: Adapt::default(), max_side: 0, half: [Vec::new(), Vec::new(), Vec::new()], start };
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
    /// Playing (frames may be approximate).
    draft: bool,
    /// Whether to convert at half size while playing.
    adapt: Adapt,
    /// The largest side the frames may have (the GPU's texture limit; 0: no limit).
    max_side: usize,
    /// The half-size planes (Y, Cb, Cr), reused from frame to frame.
    half: [Vec<u16>; 3],
    /// When the first picture is shown on the stream's timeline: playback time 0.
    start: f64,
}

/// How one picture is converted: `halve` it, and whether the frame is a `draft` for that (half
/// size while playing; a picture halved to fit the GPU is the best this machine can show, not a
/// draft).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Size {
    halve: bool,
    draft: bool,
}

impl Convert {
    /// What to do with a `width × height` picture now: half size while playing and falling
    /// behind (only big pictures), or when it is bigger than the GPU's textures.
    fn size(&self, width: usize, height: usize) -> Size {
        let can = width >= 2 && height >= 2;
        let too_big = self.max_side > 0 && (width > self.max_side || height > self.max_side);
        let playing_half = self.draft && self.adapt.half && width.saturating_mul(height) >= HALVE_MIN;
        Size { halve: can && (too_big || playing_half), draft: can && playing_half && !too_big }
    }

    /// The frame of `f`, and how long its colour conversion took (seconds, without the 2 × 2
    /// averaging).
    fn frame(&mut self, f: &Frame, mut buffer: Vec<u8>) -> Result<(VideoFrame, f64), String> {
        let full = f.picture();
        let size = self.size(f.width, f.height);
        let pic = if size.halve { half_size(&full, &mut self.half) } else { full };
        if self.max_side > 0 && (pic.width > self.max_side || pic.height > self.max_side) {
            return Err(format!(
                "The clip's pictures are {} × {} pixels, more than this graphics card can show (at most {} per side)",
                f.width, f.height, self.max_side
            ));
        }
        let key = (f.colour, f.bit_depth, pic.width, pic.height);
        if self.converter.as_ref().is_none_or(|(k, _)| *k != key) {
            // (the full size picks the defaults for unspecified code points)
            let c = Converter::new(f.colour, f.bit_depth, f.width, f.height).map_err(|e| reason(&e.into()))?;
            self.converter = Some((key, c));
        }
        let Some((_, conv)) = &self.converter else {
            return Err("no colour converter".into());
        };
        let len = pic.width.checked_mul(pic.height).and_then(|n| n.checked_mul(4)).ok_or("picture too large")?;
        buffer.clear();
        buffer.resize(len, 0);
        let t = Instant::now();
        conv.to_srgb8(&pic, bytemuck::cast_slice_mut(&mut buffer)).map_err(|e| reason(&e.into()))?;
        let secs = t.elapsed().as_secs_f64();
        let frame = VideoFrame {
            pts: f.seconds - self.start,
            width: u32::try_from(pic.width).map_err(|_| "picture too wide")?,
            height: u32::try_from(pic.height).map_err(|_| "picture too tall")?,
            rgba: buffer,
            // a half-size frame is replaced by the full picture when playback stops on it
            draft: f.draft || size.draft,
        };
        Ok((frame, secs))
    }
}

/// The stream's next picture, or why there is none.
fn decode(stream: &mut VideoStream, stop: &mut dyn FnMut() -> bool) -> Ahead {
    match stream.next_frame_until(stop) {
        Ok(Some(f)) => Ahead::Picture(f),
        Ok(None) => Ahead::End,
        Err(VideoError::Cancelled) => Ahead::Stopped,
        Err(e) => Ahead::Failed(reason(&e)),
    }
}

impl FrameSource for CodecSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn seek(&mut self, seconds: f64) -> Result<(), String> {
        self.ahead = None;
        self.stream.seek(seconds + self.convert.start, SeekMode::Exact).map_err(|e| reason(&e))
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
            return self.convert.frame(&f, buffer).map(|(v, _)| Next::Frame(v));
        }
        let (convert, stream) = (&mut self.convert, &mut self.stream);
        let (converted, ahead, decode_secs) = std::thread::scope(|scope| {
            let worker = std::thread::Builder::new().name("lc-video-convert".into()).spawn_scoped(scope, || convert.frame(&f, buffer));
            let t = Instant::now();
            let ahead = decode(stream, stop);
            let decode_secs = t.elapsed().as_secs_f64();
            let converted = match worker {
                Ok(h) => h.join().unwrap_or_else(|_| Err("the colour conversion failed".to_string())),
                Err(e) => Err(format!("no thread for the colour conversion: {e}")),
            };
            (converted, ahead, decode_secs)
        });
        self.ahead = Some(ahead);
        let (frame, convert_secs) = converted?;
        if self.convert.draft {
            let halved = self.convert.size(f.width, f.height).halve;
            self.convert.adapt.after(decode_secs, convert_secs, halved, self.info.frame_duration);
        }
        Ok(Next::Frame(frame))
    }

    fn set_draft(&mut self, on: bool) {
        self.convert.draft = on;
        self.stream.set_draft(on);
    }

    fn set_behind(&mut self, behind: bool) {
        self.convert.adapt.set_behind(behind);
    }

    fn set_max_side(&mut self, pixels: usize) {
        self.convert.max_side = pixels;
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
