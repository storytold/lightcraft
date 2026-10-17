//! The export loop (native targets only).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use filmcraft_color::{Matrix, from_linear, rgb_to_ycbcr};
use filmcraft_isobmff::{AvcConfig, Brand, FourCc, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use lcc_raster::Rgb32f;
use lcc_video::FrameReader;

/// Largest output side (H.264 level 6.2 tops out at 8192×4320).
pub const MAX_SIDE: u32 = 8192;
/// Most frames one export writes (24 h at 60 fps): guards against a hostile duration.
pub const MAX_FRAMES: i64 = 24 * 3600 * 60;

/// The frame type the grade callback receives: [`lcc_raster::Rgb32f`] as `lcc_video` decodes it,
/// **linear light, Rec.709 primaries, D65, display-referred** (1.0 = the source's SDR peak white).
/// The grade may write any value; afterwards values are clamped to 0..=1 and re-encoded with the
/// sRGB curve (the inverse of the decode), so an identity grade round-trips. The grade must not
/// change the frame size.
pub type GradeFrame = Rgb32f;

/// Output format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClipFormat {
    /// MPEG-4: H.264 High 8-bit 4:2:0 + AAC-LC.
    #[default]
    H264Mp4,
    /// QuickTime: ProRes 422 (HQ at quality ≥ 80, Standard ≥ 50, else LT) 10-bit + 16-bit PCM.
    ProResMov,
}

impl ClipFormat {
    /// The file extension (without the dot).
    pub fn extension(self) -> &'static str {
        match self {
            ClipFormat::H264Mp4 => "mp4",
            ClipFormat::ProResMov => "mov",
        }
    }
}

/// Export options.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipExportOptions {
    pub format: ClipFormat,
    /// 1–100 (clamped): maps to H.264 CRF 40…14 or the ProRes flavour.
    pub quality: u8,
    /// Longest output side in pixels (`None` = source size). The other side keeps the aspect;
    /// both are rounded down to even. Never upscales.
    pub max_side: Option<u32>,
    /// Destination file. Overwritten on success; untouched on failure.
    pub out: PathBuf,
}

/// What an export wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipReport {
    pub frames: u64,
    /// Seconds (frames / frame rate).
    pub duration: f64,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub has_audio: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportClipError {
    #[error("I/O error: {0}")]
    Io(String),
    #[error("cannot open the video: {0}")]
    Open(String),
    #[error("the file has no video stream")]
    NoVideo,
    #[error("invalid trim: {0}")]
    BadTrim(String),
    #[error("invalid options: {0}")]
    BadOptions(String),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("encode error: {0}")]
    Encode(String),
    #[error("export cancelled")]
    Cancelled,
    #[error("internal error while exporting the video")]
    Internal,
}

pub(crate) type Result<T> = std::result::Result<T, ExportClipError>;

pub(crate) fn io(e: impl std::fmt::Display) -> ExportClipError {
    ExportClipError::Io(e.to_string())
}
pub(crate) fn enc(e: impl std::fmt::Display) -> ExportClipError {
    ExportClipError::Encode(e.to_string())
}

/// A geometric step before the grade (crop, straighten, flips): `sample(frame, w, h)` draws the
/// decoded source frame into a `w × h` output. `size` is that output's size at the source's own
/// resolution, which sets the export's aspect and (with `max_side`) its pixel size.
pub struct Reframe<'a> {
    pub size: (f64, f64),
    pub sample: &'a (dyn Fn(&Rgb32f, usize, usize) -> Rgb32f + Sync),
}

/// Export `src` from `trim.0` to `trim.1` (seconds; `None` = end of clip) to `opts.out`.
///
/// `grade` runs on every output frame, on several frames at once on worker threads (decode, grade
/// and encode are pipelined; frames are encoded in order). `progress` receives 0..=1. Setting `cancel` stops within one frame with
/// [`ExportClipError::Cancelled`] and removes the partial file.
pub fn export_clip(
    src: &Path,
    trim: (f64, Option<f64>),
    grade: &(dyn Fn(&mut Rgb32f) + Sync),
    opts: &ClipExportOptions,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<ClipReport> {
    export_clip_framed(src, trim, None, grade, opts, cancel, progress)
}

/// [`export_clip`] with an optional [`Reframe`] (crop) applied to every frame before the grade.
pub fn export_clip_framed(
    src: &Path,
    trim: (f64, Option<f64>),
    reframe: Option<&Reframe<'_>>,
    grade: &(dyn Fn(&mut Rgb32f) + Sync),
    opts: &ClipExportOptions,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<ClipReport> {
    let partial = partial_path(&opts.out)?;
    let r =
        catch_unwind(AssertUnwindSafe(|| run(src, trim, reframe, grade, opts, cancel, progress, &partial))).unwrap_or(Err(ExportClipError::Internal));
    match r {
        Ok(mut rep) => {
            rep.path = opts.out.clone();
            std::fs::rename(&partial, &opts.out).map_err(|e| {
                let _ = std::fs::remove_file(&partial);
                io(e)
            })?;
            Ok(rep)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            Err(e)
        }
    }
}

pub(crate) fn partial_path(out: &Path) -> Result<PathBuf> {
    let name = out.file_name().ok_or_else(|| ExportClipError::BadOptions("the output path has no file name".into()))?;
    let mut n = name.to_os_string();
    n.push(".partial");
    Ok(out.with_file_name(n))
}

/// The frames to export: indices `[k0, k1)` into the clip's presentation-ordered frame times. The
/// first is the frame displayed at `trim.0` (which may sit mid-GOP: the reader decodes from the
/// previous sync sample); the last is the final frame that starts before the out point.
fn select_frames(times: &[f64], duration: f64, trim: (f64, Option<f64>)) -> Result<(usize, usize)> {
    let (tin, tout) = (trim.0, trim.1.unwrap_or(duration));
    if !tin.is_finite() || !tout.is_finite() || !duration.is_finite() || tin < 0.0 {
        return Err(ExportClipError::BadTrim(format!("in {tin} / out {tout}")));
    }
    let tout = tout.min(duration);
    if tout <= tin {
        return Err(ExportClipError::BadTrim(format!("out ({tout:.3} s) must be after in ({tin:.3} s)")));
    }
    const EPS: f64 = 1e-6;
    let k0 = times.iter().rposition(|t| *t <= tin + EPS).unwrap_or(0);
    let k1 = times.iter().position(|t| *t >= tout - EPS).unwrap_or(times.len()).max(k0.saturating_add(1)).min(times.len());
    if k0 >= k1 {
        return Err(ExportClipError::BadTrim("no frames in the range".into()));
    }
    if (k1 - k0) as i64 > MAX_FRAMES {
        return Err(ExportClipError::BadTrim("the range is too long".into()));
    }
    Ok((k0, k1))
}

/// Output size: fit the longest side in `max_side`, keep the aspect, round down to even.
fn output_size(w: u32, h: u32, max_side: Option<u32>) -> Result<(u32, u32)> {
    if w < 2 || h < 2 {
        return Err(ExportClipError::Decode(format!("frame size {w}×{h} is too small")));
    }
    let long = w.max(h);
    let limit = max_side.unwrap_or(long).clamp(2, MAX_SIDE).min(long);
    let s = f64::from(limit) / f64::from(long);
    let even = |v: u32| -> u32 { ((f64::from(v) * s).round() as u32).max(2) & !1 };
    Ok((even(w), even(h)))
}

fn video_err(e: lcc_video::VideoError) -> ExportClipError {
    match e {
        lcc_video::VideoError::Io(m) => ExportClipError::Io(m),
        lcc_video::VideoError::Decode(m) => ExportClipError::Decode(m),
        other => ExportClipError::Open(other.to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    src: &Path,
    trim: (f64, Option<f64>),
    reframe: Option<&Reframe<'_>>,
    grade: &(dyn Fn(&mut Rgb32f) + Sync),
    opts: &ClipExportOptions,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
    partial: &Path,
) -> Result<ClipReport> {
    let mut reader = FrameReader::open(src).map_err(video_err)?;
    let info = reader.info().clone();
    if !info.decodable {
        return Err(ExportClipError::Open(format!("{} video cannot be decoded", info.codec)));
    }
    let (fnum, fden) = (info.fps_num, info.fps_den);
    if fnum == 0 || fden == 0 {
        return Err(ExportClipError::Decode("invalid frame rate".into()));
    }
    let times = reader.frame_times();
    let (k0, k1) = select_frames(&times, info.duration, trim)?;
    let total = k1 - k0;
    let t0 = times.get(k0).copied().unwrap_or(0.0);
    let (w, h) = match reframe {
        // a crop's own size, never above the source's pixels
        Some(r) if r.size.0.is_finite() && r.size.1.is_finite() && r.size.0 >= 2.0 && r.size.1 >= 2.0 => {
            output_size(r.size.0.min(f64::from(MAX_SIDE)).round() as u32, r.size.1.min(f64::from(MAX_SIDE)).round() as u32, opts.max_side)?
        }
        Some(_) => return Err(ExportClipError::BadOptions("the crop is empty".into())),
        None => output_size(info.width, info.height, opts.max_side)?,
    };
    // a reframed export samples the frame at the source's size (the crop picks a part of it)
    let decode_long = if reframe.is_some() { info.width.max(info.height) } else { w.max(h) };
    let quality = opts.quality.clamp(1, 100);

    let file = File::create(partial).map_err(io)?;
    let brand = match opts.format {
        ClipFormat::H264Mp4 => Brand::Mp4,
        ClipFormat::ProResMov => Brand::Mov,
    };
    let mut mux = Mp4Writer::new(BufWriter::new(file), WriterOptions::new(brand)).map_err(enc)?;
    let mut video = VideoOut::new(opts.format, quality, w, h, fnum, fden)?;
    let vtrack = mux.add_track(video.track_config()).map_err(enc)?;
    let end = t0 + total as f64 * f64::from(fden) / f64::from(fnum);
    let audio = crate::audio::encode_audio_range(src, t0, end, opts.format, cancel)?;
    let atrack = match &audio {
        Some(a) => Some(mux.add_track(a.track_config()).map_err(enc)?),
        None => None,
    };

    let threads = grade_threads();
    // Decode, grade and encode run on separate threads joined by bounded queues; grading may run
    // on several frames at once, and the encoder takes the frames back in order.
    let stop = AtomicBool::new(false);
    let halted = || cancel.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed);
    let (dtx, drx) = std::sync::mpsc::sync_channel::<Result<(usize, Rgb32f)>>(QUEUE);
    let (gtx, grx) = std::sync::mpsc::sync_channel::<Result<(usize, Prepared)>>(QUEUE + threads);
    let stages = Stages::default();
    let started = Instant::now();
    let prep = video.prep();
    let drx = std::sync::Mutex::new(drx);
    let encoded: Result<()> = std::thread::scope(|sc| {
        let (halted, drx, stages, prep) = (&halted, &drx, &stages, &prep);
        let mut handles = Vec::with_capacity(threads + 1);
        handles.push(sc.spawn(move || {
            for (i, k) in (k0..k1).enumerate() {
                if halted() {
                    return;
                }
                let t = times.get(k).copied().unwrap_or(t0);
                let at = Instant::now();
                let r = reader.frame_at(t, Some(decode_long)).map_err(video_err).map(|d| {
                    let f = match reframe {
                        Some(rf) => (rf.sample)(&d.image, w as usize, h as usize),
                        None => fit(&d.image, w as usize, h as usize),
                    };
                    (i, f)
                });
                Stages::add(&stages.decode, at);
                let failed = r.is_err();
                if dtx.send(r).is_err() || failed {
                    return;
                }
            }
        }));
        for _ in 0..threads {
            let gtx = gtx.clone();
            handles.push(sc.spawn(move || {
                loop {
                    // ends when the decoder has stopped; once halted, frames are drained (so a
                    // decoder blocked on a full queue sees the halt) and dropped
                    let next = drx.lock().unwrap_or_else(std::sync::PoisonError::into_inner).recv();
                    let Ok(item) = next else { return };
                    if halted() {
                        continue;
                    }
                    let item = item.and_then(|(i, mut frame)| {
                        let at = Instant::now();
                        grade(&mut frame);
                        Stages::add(&stages.grade, at);
                        if frame.width != w as usize || frame.height != h as usize || frame.data.len() != w as usize * h as usize {
                            return Err(ExportClipError::Encode("the crop or grade changed the frame size".into()));
                        }
                        let at = Instant::now();
                        let p = prep.prepare(&frame);
                        Stages::add(&stages.convert, at);
                        Ok((i, p?))
                    });
                    // The encoder has gone (it stops the pipeline before dropping its end): keep
                    // draining, not leaving, until the decoder stops. A grader that left here
                    // could strand the decoder on a full queue, and the scope then never joins.
                    let _ = gtx.send(item);
                }
            }));
        }
        drop(gtx);
        let r = (|| {
            let mut pending = std::collections::BTreeMap::new();
            let mut i = 0usize;
            while i < total {
                if cancel.load(Ordering::Relaxed) {
                    return Err(ExportClipError::Cancelled);
                }
                let at = Instant::now();
                let frame = match pending.remove(&i) {
                    Some(f) => f,
                    None => match grx.recv() {
                        Ok(item) => {
                            let (j, f) = item?;
                            if j != i {
                                pending.insert(j, f);
                                continue;
                            }
                            f
                        }
                        // every stage ended early: cancelled, or a thread failed
                        Err(_) if cancel.load(Ordering::Relaxed) => return Err(ExportClipError::Cancelled),
                        Err(_) => return Err(ExportClipError::Internal),
                    },
                };
                Stages::add(&stages.wait, at);
                let at = Instant::now();
                for p in video.encode(frame, i as i64)? {
                    mux.write_sample(vtrack, WriteSample { data: &p.data, duration: fden, composition_offset: p.cto, is_sync: p.key })
                        .map_err(enc)?;
                }
                Stages::add(&stages.encode, at);
                i += 1;
                progress(i as f64 / total as f64);
            }
            Ok(())
        })();
        // stop the other stages, then wait for them (their queues are drained by dropping them)
        stop.store(true, Ordering::Relaxed);
        drop(grx);
        let mut joined = Ok(());
        for hd in handles {
            if hd.join().is_err() {
                joined = Err(ExportClipError::Internal);
            }
        }
        r.and(joined)
    });
    encoded?;
    stages.report(total, started.elapsed().as_secs_f64(), threads);
    if cancel.load(Ordering::Relaxed) {
        return Err(ExportClipError::Cancelled);
    }
    for p in video.flush() {
        mux.write_sample(vtrack, WriteSample { data: &p.data, duration: fden, composition_offset: p.cto, is_sync: p.key }).map_err(enc)?;
    }
    if let (Some(a), Some(tr)) = (&audio, atrack) {
        for p in &a.packets {
            mux.write_sample(tr, WriteSample { data: &p.data, duration: p.duration, composition_offset: 0, is_sync: true }).map_err(enc)?;
        }
    }
    let mut out = mux.finish().map_err(enc)?;
    out.flush().map_err(io)?;
    let file = out.into_inner().map_err(|e| io(e.error()))?;
    file.sync_all().map_err(io)?;
    let frames = total as u64;
    Ok(ClipReport {
        frames,
        duration: frames as f64 * f64::from(fden) / f64::from(fnum),
        path: PathBuf::new(),
        width: w,
        height: h,
        has_audio: audio.is_some(),
    })
}

/// Per-stage busy time (nanoseconds, summed over threads), printed under `{ENV_PREFIX}_PROFILE`.
#[derive(Default)]
struct Stages {
    decode: AtomicU64,
    grade: AtomicU64,
    /// Y'CbCr conversion + plane packing (+ the ProRes encode, which runs on the workers).
    convert: AtomicU64,
    /// H.264 encode + mux on the calling thread.
    encode: AtomicU64,
    /// The calling thread waiting for the next frame.
    wait: AtomicU64,
}

impl Stages {
    fn add(slot: &AtomicU64, since: Instant) {
        slot.fetch_add(u64::try_from(since.elapsed().as_nanos()).unwrap_or(u64::MAX), Ordering::Relaxed);
    }
    fn report(&self, frames: usize, wall: f64, workers: usize) {
        if !lcc_brand::env_is_set("PROFILE") {
            return;
        }
        let ms = |a: &AtomicU64| a.load(Ordering::Relaxed) as f64 / 1e6 / frames.max(1) as f64;
        eprintln!(
            "clip export: {frames} frames in {wall:.1} s ({:.1} fps), {workers} workers; per frame: decode {:.1} ms, grade {:.1} ms, convert {:.1} ms, encode+mux {:.1} ms, encoder waiting {:.1} ms",
            frames as f64 / wall.max(1e-9),
            ms(&self.decode),
            ms(&self.grade),
            ms(&self.convert),
            ms(&self.encode),
            ms(&self.wait)
        );
    }
}

/// Frames waiting between two stages.
const QUEUE: usize = 4;

/// Frames graded at once: a few, as the grade itself is parallel inside.
fn grade_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get()).div_ceil(8).clamp(1, 4)
}

/// Resample to exactly `w × h` (bilinear; a copy when the size already matches).
fn fit(img: &Rgb32f, w: usize, h: usize) -> Rgb32f {
    if img.width == w && img.height == h {
        return img.clone();
    }
    let (sx, sy) = (img.width.max(1) as f32 / w.max(1) as f32, img.height.max(1) as f32 / h.max(1) as f32);
    Rgb32f::from_fn(w, h, |x, y| img.sample_bilinear((x as f32 + 0.5) * sx, (y as f32 + 0.5) * sy))
}

/// Linear → encoded R'G'B' (0..1) → normalised Y'CbCr, per pixel.
fn ycbcr(frame: &Rgb32f) -> Vec<[f32; 3]> {
    ycbcr_with(frame, true)
}

/// Per pixel Y'CbCr; `linear` = the frame is linear light (else already encoded R'G'B').
pub(crate) fn ycbcr_with(frame: &Rgb32f, linear: bool) -> Vec<[f32; 3]> {
    frame
        .data
        .iter()
        .map(|p| {
            let e = |v: f32| {
                let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
                if linear { from_linear(v, filmcraft_color::Transfer::Bt709) } else { v }
            };
            rgb_to_ycbcr(e(p[0]), e(p[1]), e(p[2]), Matrix::Bt709)
        })
        .collect()
}

/// Limited-range code value of a normalised luma / chroma sample at `bits`.
fn code_y(v: f32, bits: u32) -> u16 {
    let s = (1u32 << (bits - 8)) as f32;
    let max = ((1u32 << bits) - 1) as f32;
    (16.0 * s + v.clamp(0.0, 1.0) * 219.0 * s).round().clamp(0.0, max) as u16
}
fn code_c(v: f32, bits: u32) -> u16 {
    let s = (1u32 << (bits - 8)) as f32;
    let max = ((1u32 << bits) - 1) as f32;
    (128.0 * s + v.clamp(-0.5, 0.5) * 224.0 * s).round().clamp(0.0, max) as u16
}

/// Turns a graded frame into encoder input. H.264 is stateful (references, rate control) and
/// stays on one thread; ProRes frames are intra-only and its encoder keeps no state between
/// frames, so whole frames are encoded here, in parallel, and muxed in order (same bytes as a
/// sequential encode).
enum Prep {
    H264 { w: usize, h: usize },
    ProRes { enc: filmcraft_prores::Encoder, w: usize, h: usize },
}

enum Prepared {
    Yuv420 { y: Vec<u8>, u: Vec<u8>, v: Vec<u8> },
    Coded(Packet),
}

impl Prep {
    fn prepare(&self, frame: &Rgb32f) -> Result<Prepared> {
        self.prepare_ycbcr(&ycbcr(frame))
    }

    fn prepare_ycbcr(&self, yc: &[[f32; 3]]) -> Result<Prepared> {
        match self {
            Prep::H264 { w, h } => {
                let (w, h) = (*w, *h);
                let (cw, ch) = (w / 2, h / 2);
                let y: Vec<u8> = yc.iter().map(|p| code_y(p[0], 8) as u8).collect();
                let (mut u, mut v) = (vec![0u8; cw * ch], vec![0u8; cw * ch]);
                for cy in 0..ch {
                    for cx in 0..cw {
                        let (mut cb, mut cr) = (0.0, 0.0);
                        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                            if let Some(p) = yc.get((cy * 2 + dy) * w + cx * 2 + dx) {
                                cb += p[1];
                                cr += p[2];
                            }
                        }
                        let i = cy * cw + cx;
                        if let (Some(u), Some(v)) = (u.get_mut(i), v.get_mut(i)) {
                            *u = code_c(cb / 4.0, 8) as u8;
                            *v = code_c(cr / 4.0, 8) as u8;
                        }
                    }
                }
                Ok(Prepared::Yuv420 { y, u, v })
            }
            Prep::ProRes { enc, w, h } => {
                let (w, h) = (*w, *h);
                let mut fr = filmcraft_prores::Frame::new(w as u32, h as u32, filmcraft_prores::ChromaFormat::Yuv422, 10, false);
                let cw = w / 2;
                for (d, p) in fr.y.iter_mut().zip(yc) {
                    *d = code_y(p[0], 10);
                }
                for y in 0..h {
                    for cx in 0..cw {
                        let a = yc.get(y * w + cx * 2);
                        let b = yc.get(y * w + cx * 2 + 1).or(a);
                        let (Some(a), Some(b)) = (a, b) else { continue };
                        let i = y * cw + cx;
                        if let Some(d) = fr.cb.get_mut(i) {
                            *d = code_c((a[1] + b[1]) / 2.0, 10);
                        }
                        if let Some(d) = fr.cr.get_mut(i) {
                            *d = code_c((a[2] + b[2]) / 2.0, 10);
                        }
                    }
                }
                // a clone per frame: the encoder is a config, and `encode` needs `&mut`
                let data = enc.clone().encode(&fr).map_err(enc_err)?;
                Ok(Prepared::Coded(Packet { data, key: true, cto: 0 }))
            }
        }
    }
}

fn enc_err(e: impl std::fmt::Display) -> ExportClipError {
    enc(e)
}

pub(crate) struct Packet {
    pub(crate) data: Vec<u8>,
    pub(crate) key: bool,
    pub(crate) cto: i32,
}

pub(crate) enum VideoOut {
    H264 { enc: Box<filmcraft_h264enc::Encoder>, w: u32, h: u32, den: u32 },
    ProRes { enc: filmcraft_prores::Encoder, profile: filmcraft_prores::Profile, w: u32, h: u32, num: u32 },
}

impl VideoOut {
    pub(crate) fn new(format: ClipFormat, quality: u8, w: u32, h: u32, num: u32, den: u32) -> Result<Self> {
        Ok(match format {
            ClipFormat::H264Mp4 => {
                let mut cfg = filmcraft_h264enc::EncoderConfig::new(w, h, num, den);
                cfg.format = filmcraft_h264enc::PacketFormat::LengthPrefixed;
                cfg.aud = false;
                cfg.keyint = ((f64::from(num) / f64::from(den)) * 2.0).round().clamp(1.0, 600.0) as u32;
                cfg.rate = filmcraft_h264enc::RateControl::Crf(40.0 - f32::from(quality - 1) * 26.0 / 99.0);
                let enc = filmcraft_h264enc::Encoder::new(cfg).map_err(enc)?;
                VideoOut::H264 { enc: Box::new(enc), w, h, den }
            }
            ClipFormat::ProResMov => {
                use filmcraft_prores::Profile;
                let profile = match quality {
                    80.. => Profile::Hq,
                    50.. => Profile::Standard,
                    _ => Profile::Lt,
                };
                let mut cfg = filmcraft_prores::EncoderConfig::new(profile, w, h);
                cfg.encode_alpha = false;
                VideoOut::ProRes { enc: filmcraft_prores::Encoder::with_config(cfg), profile, w, h, num }
            }
        })
    }

    pub(crate) fn track_config(&self) -> TrackConfig {
        match self {
            VideoOut::H264 { enc, w, h, den } => {
                let cfg = AvcConfig::parse(&enc.avcc()).unwrap_or_else(|_| {
                    let (sps, pps) = enc.sps_pps();
                    AvcConfig::new(vec![sps], vec![pps], 4)
                });
                let mut t = TrackConfig::new(SampleEntry::avc(cfg, *w as u16, *h as u16), enc.config().fps_num);
                // with B-frames the first DTS is one frame before the first PTS
                if enc.delay() > 0 {
                    t.media_start = Some(i64::from(*den));
                }
                t
            }
            VideoOut::ProRes { profile, w, h, num, .. } => {
                TrackConfig::new(SampleEntry::prores(FourCc(profile.fourcc()), *w as u16, *h as u16), *num)
            }
        }
    }

    /// The per-frame work that can run on the grade workers.
    fn prep(&self) -> Prep {
        match self {
            VideoOut::H264 { w, h, .. } => Prep::H264 { w: *w as usize, h: *h as usize },
            VideoOut::ProRes { enc, w, h, .. } => Prep::ProRes { enc: enc.clone(), w: *w as usize, h: *h as usize },
        }
    }

    /// Encode one frame given as normalised Y'CbCr (the sequential path, used by the sequence writer).
    pub(crate) fn encode_ycbcr(&mut self, yc: &[[f32; 3]], index: i64) -> Result<Vec<Packet>> {
        let p = self.prep().prepare_ycbcr(yc)?;
        self.encode(p, index)
    }

    fn encode(&mut self, frame: Prepared, index: i64) -> Result<Vec<Packet>> {
        match (self, frame) {
            (VideoOut::H264 { enc, w, den, .. }, Prepared::Yuv420 { y, u, v }) => {
                let w = *w as usize;
                let yuv = filmcraft_h264enc::YuvFrame { y: &y, u: &u, v: &v, y_stride: w, uv_stride: w / 2 };
                let ps = enc.try_encode(&yuv, index.saturating_mul(i64::from(*den))).map_err(self::enc)?;
                Ok(h264_packets(ps))
            }
            (VideoOut::ProRes { .. }, Prepared::Coded(p)) => Ok(vec![p]),
            _ => Err(ExportClipError::Internal),
        }
    }

    pub(crate) fn flush(&mut self) -> Vec<Packet> {
        match self {
            VideoOut::H264 { enc, .. } => h264_packets(enc.flush()),
            VideoOut::ProRes { .. } => Vec::new(),
        }
    }
}

fn h264_packets(ps: Vec<filmcraft_h264enc::Packet>) -> Vec<Packet> {
    ps.into_iter().map(|p| Packet { data: p.data, key: p.keyframe, cto: i32::try_from(p.pts.saturating_sub(p.dts)).unwrap_or(0) }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_selection_is_frame_accurate_and_validated() {
        let times: Vec<f64> = (0..100).map(|i| f64::from(i) / 25.0).collect();
        assert_eq!(select_frames(&times, 4.0, (1.0, Some(2.0))).unwrap(), (25, 50));
        assert_eq!(select_frames(&times, 4.0, (0.0, None)).unwrap(), (0, 100));
        assert_eq!(select_frames(&times, 4.0, (1.01, Some(1.05))).unwrap(), (25, 27));
        assert_eq!(select_frames(&times, 4.0, (3.9, Some(99.0))).unwrap(), (97, 100));
        assert!(select_frames(&times, 4.0, (2.0, Some(1.0))).is_err());
        assert!(select_frames(&times, 4.0, (f64::NAN, None)).is_err());
        assert!(select_frames(&times, 4.0, (-1.0, None)).is_err());
        assert!(select_frames(&times, 4.0, (5.0, None)).is_err());
        assert!(select_frames(&times, 4.0, (0.0, Some(f64::INFINITY))).is_err());
        assert!(select_frames(&[], 4.0, (0.0, None)).is_err());
    }

    #[test]
    fn output_size_fits_keeps_aspect_and_is_even() {
        assert_eq!(output_size(1920, 1080, None).unwrap(), (1920, 1080));
        assert_eq!(output_size(1920, 1080, Some(1280)).unwrap(), (1280, 720));
        assert_eq!(output_size(1080, 1920, Some(1280)).unwrap(), (720, 1280));
        assert_eq!(output_size(641, 481, None).unwrap(), (640, 480));
        assert_eq!(output_size(640, 480, Some(4000)).unwrap(), (640, 480));
        assert_eq!(output_size(640, 480, Some(0)).unwrap(), (2, 2));
        assert!(output_size(1, 480, None).is_err());
    }

    #[test]
    fn partial_path_sits_next_to_the_output() {
        assert_eq!(partial_path(Path::new("/a/b.mp4")).unwrap(), PathBuf::from("/a/b.mp4.partial"));
        assert!(partial_path(Path::new("/")).is_err());
    }
}
