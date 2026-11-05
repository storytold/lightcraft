//! The writer and the encoders (native targets only).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use filmcraft_color::{Matrix, from_linear, rgb_to_ycbcr};
use filmcraft_isobmff::{AvcConfig, Brand, FourCc, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use lightcraft_raster::Rgb32f;

/// Largest frame side (H.264 level 6.2 tops out at 8192×4320).
pub const MAX_SIDE: u32 = 8192;
/// Most frames one movie holds (24 h at 60 fps).
pub const MAX_FRAMES: i64 = 24 * 3600 * 60;

/// Output format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VideoFormat {
    /// MPEG-4: H.264 High 8-bit 4:2:0.
    #[default]
    H264Mp4,
    /// QuickTime: ProRes 422 10-bit.
    ProResMov,
}

impl VideoFormat {
    /// The file extension (without the dot).
    pub fn extension(self) -> &'static str {
        match self {
            VideoFormat::H264Mp4 => "mp4",
            VideoFormat::ProResMov => "mov",
        }
    }
}

/// What a writer wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoReport {
    pub frames: u64,
    /// Seconds (frames / frame rate).
    pub duration: f64,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    #[error("I/O error: {0}")]
    Io(String),
    #[error("invalid options: {0}")]
    BadOptions(String),
    #[error("encode error: {0}")]
    Encode(String),
    #[error("internal error while writing the video")]
    Internal,
}

pub type Result<T> = std::result::Result<T, VideoError>;

fn io(e: impl std::fmt::Display) -> VideoError {
    VideoError::Io(e.to_string())
}
fn enc(e: impl std::fmt::Display) -> VideoError {
    VideoError::Encode(e.to_string())
}

/// `<out>.<pid>.<attempt>.partial`, next to the output: a name no other writer shares.
fn partial_path(out: &Path, attempt: u32) -> Result<PathBuf> {
    let name = out.file_name().ok_or_else(|| VideoError::BadOptions("the output path has no file name".into()))?;
    let mut n = name.to_os_string();
    n.push(format!(".{}.{attempt}.partial", std::process::id()));
    Ok(out.with_file_name(n))
}

/// Create a fresh partial file next to `out`. `create_new` never opens, follows or truncates an
/// existing file or link, so the writer owns exactly the file it removes on failure.
fn create_partial(out: &Path) -> Result<(PathBuf, File)> {
    for attempt in 0..64 {
        let path = partial_path(out, attempt)?;
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io(e)),
        }
    }
    Err(VideoError::BadOptions("could not reserve a temporary file next to the output".into()))
}

/// Move the finished partial to `out`. With `keep_existing`, a hard link (which fails when `out`
/// exists, even if it appeared a moment ago) takes the name, so nothing is ever replaced; volumes
/// without hard links fall back to a rename after an existence check.
fn move_into_place(partial: &Path, out: &Path, keep_existing: bool) -> Result<()> {
    if !keep_existing {
        return std::fs::rename(partial, out).map_err(io);
    }
    match std::fs::hard_link(partial, out) {
        Ok(()) => std::fs::remove_file(partial).map_err(io),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(io(format!("{} already exists", out.display()))),
        Err(_) => {
            if std::fs::symlink_metadata(out).is_ok() {
                return Err(io(format!("{} already exists", out.display())));
            }
            std::fs::rename(partial, out).map_err(io)
        }
    }
}

/// Writes frames of one fixed size at a fixed rate to an H.264 MP4 or ProRes MOV (no audio).
pub struct SequenceWriter {
    mux: Option<Mp4Writer<BufWriter<File>>>,
    video: VideoOut,
    track: usize,
    partial: PathBuf,
    out: PathBuf,
    /// Never replace a file at `out`: it did not exist when the caller looked.
    keep_existing: bool,
    frames: i64,
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
}

impl SequenceWriter {
    /// A writer for `width × height` frames (even, 16..=[`MAX_SIDE`]) at `fps_num / fps_den`
    /// frames per second (0.1..=240). `quality` 1–100: H.264 CRF 40…14, or the ProRes flavour.
    pub fn create(out: &Path, format: VideoFormat, quality: u8, width: u32, height: u32, fps_num: u32, fps_den: u32) -> Result<Self> {
        let bad = |m: String| Err(VideoError::BadOptions(m));
        if !(16..=MAX_SIDE).contains(&width) || !(16..=MAX_SIDE).contains(&height) || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return bad(format!("frame size {width}×{height} must be even and 16–{MAX_SIDE}"));
        }
        if fps_num == 0 || fps_den == 0 || f64::from(fps_num) / f64::from(fps_den) > 240.0 || f64::from(fps_num) / f64::from(fps_den) < 0.1 {
            return bad(format!("frame rate {fps_num}/{fps_den} is out of range"));
        }
        let brand = match format {
            VideoFormat::H264Mp4 => Brand::Mp4,
            VideoFormat::ProResMov => Brand::Mov,
        };
        let video = VideoOut::new(format, quality.clamp(1, 100), width, height, fps_num, fps_den)?;
        let (partial, file) = create_partial(out)?;
        let made = (|| {
            let mut mux = Mp4Writer::new(BufWriter::new(file), WriterOptions::new(brand)).map_err(enc)?;
            let track = mux.add_track(video.track_config()).map_err(enc)?;
            Ok::<_, VideoError>((mux, track))
        })();
        let (mux, track) = match made {
            Ok(v) => v,
            Err(e) => {
                let _ = std::fs::remove_file(&partial);
                return Err(e);
            }
        };
        Ok(Self { mux: Some(mux), video, track, partial, out: out.to_path_buf(), keep_existing: false, frames: 0, width, height, fps_num, fps_den })
    }

    /// Finish without replacing anything: fails with an I/O error when a file (an original that
    /// appeared since the caller's check, say) is at the output path by then.
    #[must_use]
    pub fn keep_existing(mut self) -> Self {
        self.keep_existing = true;
        self
    }

    /// Frames written so far.
    pub fn frames(&self) -> u64 {
        self.frames.max(0) as u64
    }

    /// Append one frame of **encoded** R'G'B' values (0..=1, e.g. an sRGB render divided by 255),
    /// stored as BT.709 video without a transfer change. Its size must be the writer's.
    pub fn push_encoded(&mut self, frame: &Rgb32f) -> Result<()> {
        self.push_inner(frame, false)
    }

    /// Append one frame of linear light (Rec.709 primaries; encoded with the BT.709 curve).
    pub fn push_linear(&mut self, frame: &Rgb32f) -> Result<()> {
        self.push_inner(frame, true)
    }

    fn push_inner(&mut self, frame: &Rgb32f, linear: bool) -> Result<()> {
        if frame.width != self.width as usize || frame.height != self.height as usize || frame.data.len() != frame.width * frame.height {
            return Err(VideoError::BadOptions(format!("frame is {}×{}, the movie {}×{}", frame.width, frame.height, self.width, self.height)));
        }
        if self.frames >= MAX_FRAMES {
            return Err(VideoError::BadOptions("the movie is too long".into()));
        }
        let mux = self.mux.as_mut().ok_or(VideoError::Internal)?;
        for p in self.video.encode_ycbcr(&ycbcr(frame, linear), self.frames)? {
            mux.write_sample(self.track, WriteSample { data: &p.data, duration: self.fps_den, composition_offset: p.cto, is_sync: p.key })
                .map_err(enc)?;
        }
        self.frames += 1;
        Ok(())
    }

    /// Flush the encoder, finish the file and move it to its place.
    pub fn finish(mut self) -> Result<VideoReport> {
        if self.frames == 0 {
            return Err(VideoError::BadOptions("no frames".into()));
        }
        let finished = (|| {
            let mut mux = self.mux.take().ok_or(VideoError::Internal)?;
            for p in self.video.flush() {
                mux.write_sample(self.track, WriteSample { data: &p.data, duration: self.fps_den, composition_offset: p.cto, is_sync: p.key })
                    .map_err(enc)?;
            }
            let mut out = mux.finish().map_err(enc)?;
            out.flush().map_err(io)?;
            let file = out.into_inner().map_err(|e| io(e.error()))?;
            file.sync_all().map_err(io)?;
            drop(file);
            move_into_place(&self.partial, &self.out, self.keep_existing)
        })();
        if let Err(e) = finished {
            let _ = std::fs::remove_file(&self.partial);
            return Err(e);
        }
        let frames = self.frames();
        Ok(VideoReport {
            frames,
            duration: frames as f64 * f64::from(self.fps_den) / f64::from(self.fps_num),
            path: self.out.clone(),
            width: self.width,
            height: self.height,
        })
    }
}

impl Drop for SequenceWriter {
    fn drop(&mut self) {
        // unfinished (error, cancel): never leave a truncated movie behind
        if self.mux.take().is_some() {
            let _ = std::fs::remove_file(&self.partial);
        }
    }
}

/// Per pixel normalised Y'CbCr; `linear` = the frame is linear light (else already encoded R'G'B').
fn ycbcr(frame: &Rgb32f, linear: bool) -> Vec<[f32; 3]> {
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

/// Limited-range code value of a normalised luma / chroma sample at `bits` (8 or 10).
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

struct Packet {
    data: Vec<u8>,
    key: bool,
    cto: i32,
}

enum VideoOut {
    H264 { enc: Box<filmcraft_h264enc::Encoder>, w: u32, h: u32, den: u32 },
    ProRes { enc: filmcraft_prores::Encoder, profile: filmcraft_prores::Profile, w: u32, h: u32, num: u32 },
}

impl VideoOut {
    fn new(format: VideoFormat, quality: u8, w: u32, h: u32, num: u32, den: u32) -> Result<Self> {
        Ok(match format {
            VideoFormat::H264Mp4 => {
                let mut cfg = filmcraft_h264enc::EncoderConfig::new(w, h, num, den);
                cfg.format = filmcraft_h264enc::PacketFormat::LengthPrefixed;
                cfg.aud = false;
                cfg.keyint = ((f64::from(num) / f64::from(den)) * 2.0).round().clamp(1.0, 600.0) as u32;
                cfg.rate = filmcraft_h264enc::RateControl::Crf(40.0 - f32::from(quality.saturating_sub(1)) * 26.0 / 99.0);
                let enc = filmcraft_h264enc::Encoder::new(cfg).map_err(enc)?;
                VideoOut::H264 { enc: Box::new(enc), w, h, den }
            }
            VideoFormat::ProResMov => {
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

    fn track_config(&self) -> TrackConfig {
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

    /// Encode one frame given as normalised Y'CbCr.
    fn encode_ycbcr(&mut self, yc: &[[f32; 3]], index: i64) -> Result<Vec<Packet>> {
        match self {
            VideoOut::H264 { enc, w, h, den } => {
                let (w, h) = (*w as usize, *h as usize);
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
                let yuv = filmcraft_h264enc::YuvFrame { y: &y, u: &u, v: &v, y_stride: w, uv_stride: cw };
                let ps = enc.try_encode(&yuv, index.saturating_mul(i64::from(*den))).map_err(self::enc)?;
                Ok(h264_packets(ps))
            }
            VideoOut::ProRes { enc, w, h, .. } => {
                let mut fr = filmcraft_prores::Frame::new(*w, *h, filmcraft_prores::ChromaFormat::Yuv422, 10, false);
                let (w, h) = (*w as usize, *h as usize);
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
                let data = enc.encode(&fr).map_err(self::enc)?;
                Ok(vec![Packet { data, key: true, cto: 0 }])
            }
        }
    }

    fn flush(&mut self) -> Vec<Packet> {
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

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lightcraft-video-encode-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Mean luma code value of every decoded frame of an H.264 MP4, in presentation order.
    fn h264_lumas(path: &Path) -> Vec<f32> {
        crate::probe::h264_lumas(path).unwrap().iter().map(crate::probe::Luma::mean).collect()
    }

    #[test]
    fn keep_existing_never_replaces_a_file_that_appeared_meanwhile() {
        let dir = scratch("keep");
        let out = dir.join("t.mp4");
        let mut w = SequenceWriter::create(&out, VideoFormat::H264Mp4, 80, 32, 16, 24, 1).unwrap().keep_existing();
        w.push_encoded(&Rgb32f::from_fn(32, 16, |_, _| [0.5; 3])).unwrap();
        std::fs::write(&out, b"precious").unwrap();
        assert!(w.finish().is_err());
        assert_eq!(std::fs::read(&out).unwrap(), b"precious");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "the partial is removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_frames_in_order_and_rejects_bad_options() {
        let dir = scratch("seq");
        let out = dir.join("t.mp4");
        assert!(SequenceWriter::create(&out, VideoFormat::H264Mp4, 80, 15, 16, 24, 1).is_err());
        assert!(SequenceWriter::create(&out, VideoFormat::H264Mp4, 80, 16, 16, 0, 1).is_err());
        assert!(SequenceWriter::create(&out, VideoFormat::H264Mp4, 80, 16, 16, 1000, 1).is_err());
        assert!(SequenceWriter::create(&out, VideoFormat::H264Mp4, 80, 99_999, 16, 24, 1).is_err());
        let mut w = SequenceWriter::create(&out, VideoFormat::H264Mp4, 95, 32, 16, 24, 1).unwrap();
        assert!(w.push_encoded(&Rgb32f::from_fn(16, 16, |_, _| [0.5; 3])).is_err(), "wrong size");
        for i in 0..5 {
            w.push_encoded(&Rgb32f::from_fn(32, 16, |_, _| [0.1 + i as f32 * 0.2; 3])).unwrap();
        }
        let r = w.finish().unwrap();
        assert_eq!((r.frames, r.width, r.height), (5, 32, 16));
        assert!((r.duration - 5.0 / 24.0).abs() < 1e-9);
        assert!(no_partials(&dir));
        let ys = h264_lumas(&out);
        assert_eq!(ys.len(), 5);
        assert!(ys.windows(2).all(|w| w[1] > w[0] + 20.0), "brighter each frame: {ys:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prores_mov_and_unfinished_writers_leave_nothing() {
        let dir = scratch("prores");
        let out = dir.join("t.mov");
        let mut w = SequenceWriter::create(&out, VideoFormat::ProResMov, 50, 32, 16, 25, 1).unwrap();
        for _ in 0..3 {
            w.push_linear(&Rgb32f::from_fn(32, 16, |_, _| [0.2; 3])).unwrap();
        }
        assert_eq!(w.finish().unwrap().frames, 3);
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(&bytes[8..12], b"qt  ", "QuickTime brand");
        let file = filmcraft_isobmff::open(bytes.as_slice()).unwrap();
        let t = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).unwrap();
        assert_eq!(file.tracks[t].samples.len(), 3);
        // dropped unfinished: neither the movie nor the partial file
        let out2 = dir.join("u.mp4");
        let mut w = SequenceWriter::create(&out2, VideoFormat::H264Mp4, 50, 32, 16, 25, 1).unwrap();
        w.push_encoded(&Rgb32f::from_fn(32, 16, |_, _| [0.5; 3])).unwrap();
        drop(w);
        assert!(!out2.exists() && no_partials(&dir));
        // no frames is an error, and also leaves nothing
        let w = SequenceWriter::create(&out2, VideoFormat::H264Mp4, 50, 32, 16, 25, 1).unwrap();
        assert!(w.finish().is_err());
        assert!(!out2.exists() && no_partials(&dir));
        // hostile values are clamped, not trusted
        let mut w = SequenceWriter::create(&out2, VideoFormat::H264Mp4, 0, 16, 16, 25, 1).unwrap();
        w.push_encoded(&Rgb32f::from_fn(16, 16, |x, _| [f32::NAN, f32::INFINITY, -(x as f32)])).unwrap();
        w.finish().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn no_partials(dir: &Path) -> bool {
        std::fs::read_dir(dir).is_ok_and(|d| d.flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".partial")))
    }

    #[test]
    fn partial_path_sits_next_to_the_output() {
        let p = partial_path(Path::new("/a/b.mp4"), 3).unwrap();
        assert_eq!(p.parent(), Some(Path::new("/a")));
        assert!(p.file_name().unwrap().to_string_lossy().starts_with("b.mp4."));
        assert!(p.to_string_lossy().ends_with(".3.partial"));
        assert!(partial_path(Path::new("/"), 0).is_err());
    }

    #[test]
    fn partial_file_never_truncates_an_existing_file_or_link() {
        let dir = std::env::temp_dir().join(format!("lc-partial-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("o.mp4");
        // an occupied first candidate (a regular file) is left alone and the next name is taken
        let first = partial_path(&out, 0).unwrap();
        std::fs::write(&first, b"keep").unwrap();
        let (second, file) = create_partial(&out).unwrap();
        drop(file);
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&first).unwrap(), b"keep");
        // a link in the way (to a catalogued original) is not followed either
        #[cfg(unix)]
        {
            let original = dir.join("orig.bin");
            std::fs::write(&original, b"original").unwrap();
            let third = partial_path(&out, 2).unwrap();
            let _ = std::fs::remove_file(&second);
            std::os::unix::fs::symlink(&original, &third).unwrap();
            let (fourth, _f) = create_partial(&out).unwrap();
            assert_ne!(third, fourth);
            assert_eq!(std::fs::read(&original).unwrap(), b"original");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
