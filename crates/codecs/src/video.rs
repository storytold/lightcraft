//! Video: AV1 encoding of RGBA frames (`craft-video`, native targets with the `avif` feature)
//! and MP4 / WebM muxing over `craft-media`:
//!
//! - **MP4** (`craft_media::isobmff::mux::Mp4Writer`): `ftyp`, `mdat` holding the samples, then
//!   `moov` with one video track whose sample entry is `av01` + `av1C` + `colr` (`nclx`) + `pasp`.
//! - **WebM** (`craft_media::matroska::mux::MkvWriter`, codec mapping `V_AV1`): EBML header, Segment
//!   with SeekHead, Info, Tracks, Clusters of SimpleBlocks (the last frame a BlockGroup with its
//!   duration) and Cues.
//!
//! Frames may last several frame periods: a picture held on screen is one sample with a long
//! duration (variable frame rate), so a still slide costs one encoded frame however long it
//! stays up. Durations are counted in frame periods of the nominal rate.
//!
//! Samples are AV1 temporal units without temporal delimiter OBUs (both bindings ask for that);
//! the configuration record carries the sequence header OBU. No audio track.

use std::io::{Seek, Write};

use craft_media::isobmff::{Av1Config, Brand, ColorInfo, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use craft_media::matroska::{MkvWriter, MuxOptions, TrackKind, TrackSpec};

use crate::{Error, Result};

/// The container a video is written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Mp4,
    WebM,
}

impl Container {
    pub fn id(self) -> &'static str {
        match self {
            Container::Mp4 => "mp4",
            Container::WebM => "webm",
        }
    }

    pub fn parse(s: &str) -> Option<Container> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mp4" => Some(Container::Mp4),
            "webm" => Some(Container::WebM),
            _ => None,
        }
    }
}

/// One encoded picture: an AV1 temporal unit (no temporal delimiter), whether it is a key frame
/// (a sync sample), and how many frame periods it stays on screen (≥ 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sample {
    pub data: Vec<u8>,
    pub key: bool,
    pub duration: u64,
}

/// The longest video the muxers write, in frame periods (keeps every timestamp in range).
pub const MAX_PERIODS: u64 = 60 * 60 * 24 * 120;

fn bad(msg: impl Into<String>) -> Error {
    Error::Encode(format!("video: {}", msg.into()))
}

fn media_err(e: impl std::fmt::Display) -> Error {
    bad(e.to_string())
}

// ---------------------------------------------------------------- AV1 OBUs

/// OBU types (AV1 specification §6.2.2).
pub const OBU_SEQUENCE_HEADER: u8 = 1;
pub const OBU_TEMPORAL_DELIMITER: u8 = 2;
pub const OBU_PADDING: u8 = 15;

/// Read a LEB128 value at `pos` (at most 8 bytes); returns (value, bytes read).
fn leb128(data: &[u8], pos: usize) -> Result<(u64, usize)> {
    let mut v = 0u64;
    for i in 0..8 {
        let b = *data.get(pos + i).ok_or_else(|| bad("truncated OBU size"))?;
        v |= u64::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Ok((v, i + 1));
        }
    }
    Err(bad("OBU size longer than 8 bytes"))
}

/// The OBUs of a temporal unit: (type, the whole OBU's byte range). Every OBU but the last must
/// carry its size (as the low-overhead bitstream format requires).
pub fn obus(data: &[u8]) -> Result<Vec<(u8, std::ops::Range<usize>)>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while let Some(&header) = data.get(pos) {
        if header & 0x80 != 0 {
            return Err(bad("OBU forbidden bit set"));
        }
        let kind = (header >> 3) & 0x0f;
        let ext = header & 0x04 != 0;
        let has_size = header & 0x02 != 0;
        let mut p = pos + 1 + usize::from(ext);
        let end = if has_size {
            let (size, n) = leb128(data, p)?;
            p += n;
            let size = usize::try_from(size).map_err(|_| bad("OBU too large"))?;
            p.checked_add(size).filter(|e| *e <= data.len()).ok_or_else(|| bad("OBU runs past the end of the packet"))?
        } else {
            data.len()
        };
        if p > end {
            return Err(bad("truncated OBU header"));
        }
        out.push((kind, pos..end));
        pos = end;
    }
    Ok(out)
}

/// A temporal unit as a sample: temporal delimiters and padding dropped. Also returns its
/// sequence header OBU, if it has one.
pub fn to_sample_data(tu: &[u8]) -> Result<(Vec<u8>, Option<Vec<u8>>)> {
    let mut data = Vec::with_capacity(tu.len());
    let mut seq = None;
    for (kind, r) in obus(tu)? {
        let Some(bytes) = tu.get(r) else { continue };
        match kind {
            OBU_TEMPORAL_DELIMITER | OBU_PADDING => {}
            _ => {
                if kind == OBU_SEQUENCE_HEADER && seq.is_none() {
                    seq = Some(bytes.to_vec());
                }
                data.extend_from_slice(bytes);
            }
        }
    }
    Ok((data, seq))
}

/// The `av1C` configuration record (AV1-ISOBMFF §2.3) for 8-bit 4:2:0 main profile, followed by
/// `seq_header` (the sequence header OBU, may be empty). Matroska's `CodecPrivate` is the same.
pub fn av1_config(seq_header: &[u8]) -> Vec<u8> {
    Av1Config {
        seq_profile: 0,
        seq_level_idx_0: 31,
        chroma_subsampling_x: true,
        chroma_subsampling_y: true,
        config_obus: seq_header.to_vec(),
        ..Default::default()
    }
    .to_bytes()
}

// ---------------------------------------------------------------- MP4

/// Track ticks per frame period (ffmpeg's habit: 512 per frame).
const TICKS: u64 = 512;

fn av1_sample_entry(cfg: Av1Config, width: u16, height: u16) -> SampleEntry {
    let mut entry = SampleEntry::av1(cfg, width, height);
    if let Some(v) = entry.video.as_mut() {
        v.compressor_name = "AOM Coding".into();
        v.color = Some(ColorInfo::Nclx { primaries: 1, transfer: 1, matrix: 1, full_range: false });
        v.pixel_aspect = Some((1, 1));
    }
    entry
}

/// An MP4 writer: [`Mp4Writer::add`] streams each sample into `mdat`; [`Mp4Writer::finish`]
/// patches its size and appends `moov`.
pub struct Mp4Writer<W: Write + Seek> {
    inner: craft_media::isobmff::Mp4Writer<W>,
    track: usize,
    width: u16,
    height: u16,
    periods: u64,
    config: Option<Vec<u8>>,
}

impl<W: Write + Seek> Mp4Writer<W> {
    pub fn new(w: W, width: u32, height: u32, fps: u32) -> Result<Self> {
        let (width, height) = (u16::try_from(width).map_err(|_| bad("too wide"))?, u16::try_from(height).map_err(|_| bad("too tall"))?);
        if width == 0 || height == 0 || !(1..=240).contains(&fps) {
            return Err(bad("bad size or frame rate"));
        }
        let timescale = u32::try_from(u64::from(fps) * TICKS).map_err(|_| bad("bad frame rate"))?;
        let mut inner = craft_media::isobmff::Mp4Writer::new(w, WriterOptions::new(Brand::Mp4)).map_err(media_err)?;
        let mut cfg = TrackConfig::new(av1_sample_entry(Av1Config::default(), width, height), timescale);
        cfg.handler_name = Some("LightCraft video".into());
        let track = inner.add_track(cfg).map_err(media_err)?;
        Ok(Mp4Writer { inner, track, width, height, periods: 0, config: None })
    }

    /// The `av1C` record ([`av1_config`]); needed before [`Self::finish`].
    pub fn set_config(&mut self, config: Vec<u8>) {
        self.config = Some(config);
    }

    pub fn add(&mut self, s: &Sample) -> Result<()> {
        let _ = u32::try_from(s.data.len()).map_err(|_| bad("sample too large"))?;
        if s.duration == 0 {
            return Err(bad("a sample lasts at least one frame"));
        }
        if self.periods == 0 && !s.key {
            return Err(bad("the first sample must be a key frame"));
        }
        let duration = s.duration.checked_mul(TICKS).and_then(|d| u32::try_from(d).ok()).ok_or_else(|| bad("video too long"))?;
        let sample = WriteSample { data: &s.data, duration, composition_offset: 0, is_sync: s.key };
        self.inner.write_sample(self.track, sample).map_err(media_err)?;
        self.periods = self.periods.saturating_add(s.duration);
        Ok(())
    }

    /// Frame periods written so far.
    pub fn periods(&self) -> u64 {
        self.periods
    }

    pub fn finish(mut self) -> Result<W> {
        if self.periods == 0 {
            return Err(bad("no frames"));
        }
        let config = self.config.take().ok_or_else(|| bad("no AV1 configuration"))?;
        if self.periods > MAX_PERIODS {
            return Err(bad("video too long"));
        }
        let av1c = Av1Config::parse(&config).map_err(media_err)?;
        self.inner.set_sample_entry(self.track, av1_sample_entry(av1c, self.width, self.height)).map_err(media_err)?;
        self.inner.finish().map_err(media_err)
    }
}

// ---------------------------------------------------------------- WebM

enum WebmState<W: Write + Seek> {
    Pending { w: W, config: Option<Vec<u8>> },
    Active(MkvWriter<W>),
}

/// A WebM writer (see the module docs). Timestamps are in milliseconds.
pub struct WebmWriter<W: Write + Seek> {
    state: Option<WebmState<W>>,
    width: u32,
    height: u32,
    fps: u32,
    /// Frame periods before the pending sample.
    periods: u64,
    /// The sample not written yet (the last one becomes a BlockGroup with its duration).
    pending: Option<(Sample, u64)>,
}

impl<W: Write + Seek> WebmWriter<W> {
    pub fn new(w: W, width: u32, height: u32, fps: u32) -> Result<Self> {
        if width == 0 || height == 0 || width > 65_535 || height > 65_535 || !(1..=240).contains(&fps) {
            return Err(bad("bad size or frame rate"));
        }
        Ok(WebmWriter {
            state: Some(WebmState::Pending { w, config: None }),
            width,
            height,
            fps,
            periods: 0,
            pending: None,
        })
    }

    /// The `CodecPrivate` (the `av1C` record, [`av1_config`]); needed before the first sample.
    pub fn set_config(&mut self, config: Vec<u8>) {
        if let Some(WebmState::Pending { config: slot, .. }) = &mut self.state {
            *slot = Some(config);
        }
    }

    fn ms(&self, periods: u64) -> u64 {
        periods.saturating_mul(1000) / u64::from(self.fps)
    }

    fn ensure_active(&mut self, first_key: bool) -> Result<&mut MkvWriter<W>> {
        if matches!(self.state, Some(WebmState::Pending { .. })) {
            if !first_key {
                return Err(bad("the first sample must be a key frame"));
            }
            if matches!(self.state, Some(WebmState::Pending { config: None, .. })) {
                return Err(bad("no AV1 configuration"));
            }
            let Some(WebmState::Pending { w, config: Some(config) }) = self.state.take() else {
                unreachable!();
            };
            let mut spec = TrackSpec::new(TrackKind::Video, "V_AV1");
            spec.codec_private = config;
            spec.default_duration_ns = Some(1_000_000_000 / u64::from(self.fps));
            spec.video_size = Some((self.width, self.height));
            let opts = MuxOptions {
                doc_type: "webm".into(),
                cluster_duration_ns: 0,
                writing_app: "LightCraft".into(),
                ..Default::default()
            };
            let mkv = MkvWriter::new(w, vec![spec], opts).map_err(media_err)?;
            self.state = Some(WebmState::Active(mkv));
        }
        match self.state.as_mut() {
            Some(WebmState::Active(mkv)) => Ok(mkv),
            _ => Err(bad("writer already finished")),
        }
    }

    pub fn add(&mut self, s: &Sample) -> Result<()> {
        if s.duration == 0 {
            return Err(bad("a sample lasts at least one frame"));
        }
        self.ensure_active(s.key)?;
        if let Some((p, start)) = self.pending.take() {
            let pts_ns = i64::try_from(self.ms(start).saturating_mul(1_000_000)).map_err(|_| bad("timestamp overflow"))?;
            let Some(WebmState::Active(mkv)) = self.state.as_mut() else {
                return Err(bad("writer already finished"));
            };
            mkv.write_frame(0, pts_ns, p.key, &p.data, None).map_err(media_err)?;
        }
        let start = self.periods;
        self.periods = self.periods.saturating_add(s.duration);
        if self.periods > MAX_PERIODS {
            return Err(bad("video too long"));
        }
        self.pending = Some((s.clone(), start));
        Ok(())
    }

    pub fn periods(&self) -> u64 {
        self.periods
    }

    pub fn finish(mut self) -> Result<W> {
        let (last, start) = self.pending.take().ok_or_else(|| bad("no frames"))?;
        let pts_ms = self.ms(start);
        let pts_ns = i64::try_from(pts_ms.saturating_mul(1_000_000)).map_err(|_| bad("timestamp overflow"))?;
        let dur_ms = self.ms(self.periods).saturating_sub(pts_ms).max(1);
        let dur_ns = dur_ms.saturating_mul(1_000_000);
        let Some(WebmState::Active(mut mkv)) = self.state.take() else {
            return Err(bad("no tracks"));
        };
        mkv.write_frame(0, pts_ns, last.key, &last.data, Some(dur_ns)).map_err(media_err)?;
        mkv.finish().map_err(media_err)
    }
}

// ---------------------------------------------------------------- either container

/// An MP4 or WebM writer.
pub enum VideoWriter<W: Write + Seek> {
    Mp4(Mp4Writer<W>),
    WebM(WebmWriter<W>),
}

impl<W: Write + Seek> VideoWriter<W> {
    pub fn new(w: W, container: Container, width: u32, height: u32, fps: u32) -> Result<Self> {
        Ok(match container {
            Container::Mp4 => VideoWriter::Mp4(Mp4Writer::new(w, width, height, fps)?),
            Container::WebM => VideoWriter::WebM(WebmWriter::new(w, width, height, fps)?),
        })
    }

    pub fn set_config(&mut self, config: Vec<u8>) {
        match self {
            VideoWriter::Mp4(m) => m.set_config(config),
            VideoWriter::WebM(m) => m.set_config(config),
        }
    }

    pub fn add(&mut self, s: &Sample) -> Result<()> {
        match self {
            VideoWriter::Mp4(m) => m.add(s),
            VideoWriter::WebM(m) => m.add(s),
        }
    }

    pub fn periods(&self) -> u64 {
        match self {
            VideoWriter::Mp4(m) => m.periods(),
            VideoWriter::WebM(m) => m.periods(),
        }
    }

    pub fn finish(self) -> Result<W> {
        match self {
            VideoWriter::Mp4(m) => m.finish(),
            VideoWriter::WebM(m) => m.finish(),
        }
    }
}

// ---------------------------------------------------------------- AV1 encoder

/// How to encode: frame size (even numbers; odd ones are rounded down), nominal frame rate,
/// quality 1–100 and rav1e speed 1 (slow) – 10 (fast).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub quality: u8,
    pub speed: u8,
}

/// Is AV1 video encoding in this build (native, `avif` feature)?
pub const AV1_AVAILABLE: bool = cfg!(all(feature = "avif", not(target_arch = "wasm32")));

/// BT.709 limited-range 4:2:0 planes of an sRGB RGBA image (alpha ignored), `w × h` (even).
pub fn yuv420(rgba: &[[u8; 4]], w: usize, h: usize) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    if w == 0 || h == 0 || !w.is_multiple_of(2) || !h.is_multiple_of(2) || w.checked_mul(h).is_none_or(|n| rgba.len() < n) {
        return Err(bad("frame size"));
    }
    const KR: f32 = 0.2126;
    const KB: f32 = 0.0722;
    const KG: f32 = 1.0 - KR - KB;
    let mut y = vec![0u8; w * h];
    let (cw, ch) = (w / 2, h / 2);
    let mut u = vec![0u8; cw * ch];
    let mut v = vec![0u8; cw * ch];
    let luma = |p: [u8; 4]| KR * f32::from(p[0]) + KG * f32::from(p[1]) + KB * f32::from(p[2]);
    for (i, out) in y.iter_mut().enumerate() {
        let p = rgba.get(i).copied().unwrap_or([0; 4]);
        *out = (16.0 + luma(p) * (219.0 / 255.0)).round().clamp(0.0, 255.0) as u8;
    }
    for cy in 0..ch {
        for cx in 0..cw {
            let mut acc = [0f32; 3];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = rgba.get((cy * 2 + dy) * w + cx * 2 + dx).copied().unwrap_or([0; 4]);
                for (a, c) in acc.iter_mut().zip(p) {
                    *a += f32::from(c) / 4.0;
                }
            }
            let l = KR * acc[0] + KG * acc[1] + KB * acc[2];
            let cb = (acc[2] - l) / (2.0 * (1.0 - KB));
            let cr = (acc[0] - l) / (2.0 * (1.0 - KR));
            let i = cy * cw + cx;
            if let Some(o) = u.get_mut(i) {
                *o = (128.0 + cb * (224.0 / 255.0)).round().clamp(0.0, 255.0) as u8;
            }
            if let Some(o) = v.get_mut(i) {
                *o = (128.0 + cr * (224.0 / 255.0)).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Ok((y, u, v))
}

/// The quantizer for `quality` (ravif's curve, so qualities match the AVIF export).
pub fn quantizer(quality: u8) -> usize {
    let q = f32::from(quality.clamp(1, 100)) / 100.0;
    let x = if q >= 0.82 {
        (1.0 - q) * 2.6
    } else if q > 0.25 {
        1.0 - 0.125 - q * 0.5
    } else {
        1.0 - q
    };
    (x * 255.0).round().clamp(0.0, 255.0) as usize
}

/// An AV1 encoder fed RGBA frames, each with how many frame periods it stays up; it hands back
/// the samples as they come out (in order: no frame reordering).
pub struct Av1Encoder {
    #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
    enc: craft_video::av1::LargeEncoder,
    #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
    durations: Vec<u64>,
    #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
    returned: usize,
    #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
    failed: bool,
    width: usize,
    height: usize,
}

impl Av1Encoder {
    pub fn new(c: &VideoConfig) -> Result<Self> {
        let (w, h) = ((c.width & !1) as usize, (c.height & !1) as usize);
        if w < 16 || h < 16 || w > 8192 || h > 8192 || !(1..=240).contains(&c.fps) {
            return Err(bad("unsupported video size or frame rate"));
        }
        #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
        {
            use craft_video::av1::{EncoderConfig, RateControl};
            let mut ec = EncoderConfig::new(w as u32, h as u32, c.fps, 1);
            ec.rate = RateControl::ConstantQ(quantizer(c.quality) as u8);
            ec.keyint = 120;
            let enc = craft_video::av1::LargeEncoder::with_exact_quantizer(&ec, c.speed).map_err(|e| bad(format!("AV1: {e}")))?;
            Ok(Av1Encoder { enc, durations: Vec::new(), returned: 0, failed: false, width: w, height: h })
        }
        #[cfg(not(all(feature = "avif", not(target_arch = "wasm32"))))]
        {
            let _ = (w, h);
            Err(bad("AV1 video encoding is not available in this build"))
        }
    }

    /// The frame size actually encoded (even numbers).
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// The configuration record for the container (`av1C`), once the first sample has come out.
    pub fn config_record(&self) -> Option<Vec<u8>> {
        #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
        {
            self.enc.av1c()
        }
        #[cfg(not(all(feature = "avif", not(target_arch = "wasm32"))))]
        {
            None
        }
    }

    /// Encode `rgba` (`width × height` of [`Self::size`]) shown for `periods` frame periods;
    /// `key` forces a key frame (a picture that doesn't lean on earlier ones: a held slide is
    /// then clean of the slide before it, and seeking lands on it).
    pub fn push(&mut self, rgba: &[[u8; 4]], periods: u64, key: bool) -> Result<Vec<Sample>> {
        let (y, u, v) = yuv420(rgba, self.width, self.height)?;
        #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
        {
            if self.failed {
                return Err(bad("AV1 encoder failed earlier"));
            }
            let y16: Vec<u16> = y.into_iter().map(u16::from).collect();
            let u16_: Vec<u16> = u.into_iter().map(u16::from).collect();
            let v16: Vec<u16> = v.into_iter().map(u16::from).collect();
            let frame = craft_video::av1::Frame { y: &y16, u: &u16_, v: &v16, y_stride: self.width, uv_stride: self.width / 2 };
            self.durations.push(periods.max(1));
            let packets = self.enc.push(&frame, key).map_err(|e| {
                self.failed = true;
                bad(format!("AV1: {e}"))
            })?;
            self.samples(packets)
        }
        #[cfg(not(all(feature = "avif", not(target_arch = "wasm32"))))]
        {
            let _ = (y, u, v, periods, key);
            Err(bad("AV1 video encoding is not available in this build"))
        }
    }

    /// Flush the encoder: the samples still inside.
    pub fn finish(&mut self) -> Result<Vec<Sample>> {
        #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
        {
            if self.failed {
                return Err(bad("AV1 encoder failed earlier"));
            }
            let packets = self.enc.finish().map_err(|e| {
                self.failed = true;
                bad(format!("AV1: {e}"))
            })?;
            let samples = self.samples(packets)?;
            if self.returned != self.durations.len() {
                self.failed = true;
                return Err(bad("AV1 encoder did not return every input frame"));
            }
            Ok(samples)
        }
        #[cfg(not(all(feature = "avif", not(target_arch = "wasm32"))))]
        Err(bad("AV1 video encoding is not available in this build"))
    }

    #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
    fn samples(&mut self, packets: Vec<craft_video::av1::BufferedPacket>) -> Result<Vec<Sample>> {
        let mut samples = Vec::with_capacity(packets.len());
        for packet in packets {
            let index = usize::try_from(packet.input_frame_number).ok();
            let duration = index.filter(|&i| i == self.returned).and_then(|i| self.durations.get(i)).copied();
            let Some(duration) = duration else {
                self.failed = true;
                return Err(bad("AV1 encoder returned an unknown or out-of-order frame"));
            };
            self.returned += 1;
            samples.push(Sample { data: packet.packet.data, key: packet.packet.keyframe, duration });
        }
        Ok(samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// The boxes of an MP4 byte range: (type, body range).
    pub fn boxes(d: &[u8], range: std::ops::Range<usize>) -> Vec<([u8; 4], std::ops::Range<usize>)> {
        let mut out = Vec::new();
        let mut p = range.start;
        while p + 8 <= range.end {
            let size32 = u32::from_be_bytes(d[p..p + 4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = d[p + 4..p + 8].try_into().unwrap();
            let (hdr, size) = if size32 == 1 { (16, u64::from_be_bytes(d[p + 8..p + 16].try_into().unwrap()) as usize) } else { (8, size32) };
            assert!(size >= hdr && p + size <= range.end, "box {} overruns", String::from_utf8_lossy(&kind));
            out.push((kind, p + hdr..p + size));
            p += size;
        }
        assert_eq!(p, range.end, "trailing bytes");
        out
    }

    fn find(d: &[u8], r: std::ops::Range<usize>, path: &[&[u8; 4]]) -> std::ops::Range<usize> {
        let mut r = r;
        for k in path {
            r = boxes(d, r).into_iter().find(|(t, _)| t == *k).unwrap_or_else(|| panic!("no {}", String::from_utf8_lossy(*k))).1;
        }
        r
    }

    fn samples() -> Vec<Sample> {
        // fake AV1 data: an OBU_FRAME (type 6) with a size field
        let frame = |b: u8| vec![0x32, 2, b, b];
        vec![
            Sample { data: frame(1), key: true, duration: 30 },
            Sample { data: frame(2), key: false, duration: 1 },
            Sample { data: frame(3), key: false, duration: 1 },
            Sample { data: frame(4), key: true, duration: 58 },
        ]
    }

    #[test]
    fn obus_split_and_temporal_delimiters_go() {
        // TD, sequence header (3 bytes), frame (2 bytes)
        let tu = [0x12, 0, 0x0a, 3, 1, 2, 3, 0x32, 2, 9, 9];
        let o = obus(&tu).unwrap();
        assert_eq!(o.iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![2, 1, 6]);
        let (data, seq) = to_sample_data(&tu).unwrap();
        assert_eq!(data, &tu[2..]);
        assert_eq!(seq.unwrap(), &tu[2..7]);
        // hostile: sizes past the end, forbidden bit, endless LEB128
        assert!(obus(&[0x32, 9, 1]).is_err());
        assert!(obus(&[0x80]).is_err());
        assert!(obus(&[0x32, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).is_err());
        assert!(obus(&[0x36]).is_err(), "extension byte missing");
        assert_eq!(av1_config(&[1, 2])[..], [0x81, 31, 0x0c, 0, 1, 2]);
    }

    #[test]
    fn mp4_box_tree_samples_and_durations() {
        let mut m = Mp4Writer::new(Cursor::new(Vec::new()), 64, 36, 30).unwrap();
        assert!(m.add(&Sample { data: vec![1], key: false, duration: 1 }).is_err(), "must start on a key frame");
        for s in samples() {
            m.add(&s).unwrap();
        }
        assert!(Mp4Writer::new(Cursor::new(Vec::new()), 64, 36, 30).unwrap().finish().is_err(), "no frames");
        m.set_config(av1_config(&[0x0a, 1, 0]));
        let d = m.finish().unwrap().into_inner();
        let top = boxes(&d, 0..d.len());
        assert_eq!(top.iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![*b"ftyp", *b"free", *b"mdat", *b"moov"]);
        assert_eq!(&d[top[2].1.clone()], &[0x32, 2, 1, 1, 0x32, 2, 2, 2, 0x32, 2, 3, 3, 0x32, 2, 4, 4]);
        let stbl = find(&d, 0..d.len(), &[b"moov", b"trak", b"mdia", b"minf", b"stbl"]);
        let kinds: Vec<_> = boxes(&d, stbl.clone()).into_iter().map(|(k, _)| k).collect();
        assert_eq!(kinds, vec![*b"stsd", *b"stts", *b"stss", *b"stsc", *b"stsz", *b"stco"]);
        let be = |r: usize| u32::from_be_bytes(d[r..r + 4].try_into().unwrap());
        let stsz = find(&d, stbl.clone(), &[b"stsz"]);
        assert_eq!(be(stsz.start + 8), 4, "sample count");
        let stts = find(&d, stbl.clone(), &[b"stts"]);
        assert_eq!(be(stts.start + 4), 3, "runs: 30, 1×2, 58");
        let total: u32 = (0..3).map(|i| be(stts.start + 8 + i * 8) * be(stts.start + 12 + i * 8)).sum();
        assert_eq!(total, 90 * 512);
        let stss = find(&d, stbl.clone(), &[b"stss"]);
        assert_eq!((be(stss.start + 4), be(stss.start + 8), be(stss.start + 12)), (2, 1, 4));
        let stco = find(&d, stbl.clone(), &[b"stco"]);
        assert_eq!(be(stco.start + 8) as usize, top[2].1.start, "chunk offset = mdat data");
        let stsd = find(&d, stbl, &[b"stsd"]);
        let av01 = boxes(&d, stsd.start + 8..stsd.end);
        assert_eq!(av01[0].0, *b"av01");
        let entry = av01[0].1.clone();
        assert_eq!(u16::from_be_bytes([d[entry.start + 24], d[entry.start + 25]]), 64);
        let inner: Vec<_> = boxes(&d, entry.start + 78..entry.end).into_iter().map(|(k, _)| k).collect();
        assert_eq!(inner, vec![*b"av1C", *b"colr", *b"pasp"]);
        let mdhd = find(&d, 0..d.len(), &[b"moov", b"trak", b"mdia", b"mdhd"]);
        assert_eq!((be(mdhd.start + 12), be(mdhd.start + 16)), (30 * 512, 90 * 512));
        let mvhd = find(&d, 0..d.len(), &[b"moov", b"mvhd"]);
        assert_eq!(be(mvhd.start + 16), 3000, "3 s in ms");
    }

    /// The EBML elements of a byte range: (id, body range).
    fn elements(d: &[u8], range: std::ops::Range<usize>) -> Vec<(u32, std::ops::Range<usize>)> {
        let vint = |p: usize| -> (u64, usize) {
            let len = d[p].leading_zeros() as usize + 1;
            let mut v = u64::from(d[p]) & (0xff >> len);
            for i in 1..len {
                v = (v << 8) | u64::from(d[p + i]);
            }
            (v, len)
        };
        let mut out = Vec::new();
        let mut p = range.start;
        while p < range.end {
            let idlen = d[p].leading_zeros() as usize + 1;
            let id = d[p..p + idlen].iter().fold(0u32, |a, b| (a << 8) | u32::from(*b));
            let (size, n) = vint(p + idlen);
            let start = p + idlen + n;
            let end = start + size as usize;
            assert!(end <= range.end, "element {id:x} overruns");
            out.push((id, start..end));
            p = end;
        }
        out
    }

    #[test]
    fn webm_element_tree_blocks_and_duration() {
        use craft_media::matroska::element_ids::{
            BLOCK, BLOCK_DURATION, CLUSTER, CUES, DURATION, EBML, INFO, SEEK_HEAD, SEGMENT, SIMPLE_BLOCK, TIMESTAMP, TRACKS, VOID,
        };
        let mut m = WebmWriter::new(Cursor::new(Vec::new()), 64, 36, 30).unwrap();
        assert!(m.add(&Sample { data: vec![0x32, 0], key: true, duration: 1 }).is_err(), "configuration first");
        let mut m = WebmWriter::new(Cursor::new(Vec::new()), 64, 36, 30).unwrap();
        m.set_config(av1_config(&[0x0a, 1, 0]));
        for s in samples() {
            m.add(&s).unwrap();
        }
        let d = m.finish().unwrap().into_inner();
        let top = elements(&d, 0..d.len());
        assert_eq!(top.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![EBML, SEGMENT]);
        let seg = elements(&d, top[1].1.clone());
        let ids: Vec<_> = seg.iter().map(|(i, _)| *i).collect();
        assert_eq!(ids, vec![SEEK_HEAD, VOID, INFO, TRACKS, CLUSTER, CLUSTER, CUES]);
        let info = elements(&d, seg[2].1.clone());
        let dur = info.iter().find(|(i, _)| *i == DURATION).unwrap().1.clone();
        assert_eq!(f64::from_be_bytes(d[dur].try_into().unwrap()), 3000.0);
        let track = elements(&d, elements(&d, seg[3].1.clone())[0].1.clone());
        let codec = track.iter().find(|(i, _)| *i == 0x86).unwrap().1.clone();
        assert_eq!(&d[codec], b"V_AV1");
        let first: Vec<_> = elements(&d, seg[4].1.clone()).into_iter().map(|(i, _)| i).collect();
        assert_eq!(first, vec![TIMESTAMP, SIMPLE_BLOCK, SIMPLE_BLOCK, SIMPLE_BLOCK]);
        let second = elements(&d, seg[5].1.clone());
        assert_eq!(d[second[0].1.clone()], [0x04, 0x2a], "the second cluster starts at 1066 ms (32 periods)");
        let group = elements(&d, second[1].1.clone());
        assert_eq!(group.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![BLOCK, BLOCK_DURATION]);
        // the SeekHead points at Info, Tracks and Cues
        let seg_start = top[1].1.start;
        for seek in elements(&d, seg[0].1.clone()) {
            let e = elements(&d, seek.1);
            let pos = d[e[1].1.clone()].iter().fold(0usize, |a, b| (a << 8) | usize::from(*b));
            let id = d[e[0].1.clone()].iter().fold(0u32, |a, b| (a << 8) | u32::from(*b));
            assert!(seg.iter().any(|(i, r)| *i == id && r.start > seg_start + pos), "{id:x}");
        }
    }

    #[test]
    fn hostile_sizes_are_refused() {
        assert!(Mp4Writer::new(Cursor::new(Vec::new()), 0, 10, 30).is_err());
        assert!(Mp4Writer::new(Cursor::new(Vec::new()), 70_000, 10, 30).is_err());
        assert!(WebmWriter::new(Cursor::new(Vec::new()), 10, 10, 0).is_err());
        assert!(yuv420(&[[0; 4]; 3], 2, 2).is_err());
        assert!(yuv420(&[[0; 4]; 9], 3, 3).is_err());
        let (y, u, v) = yuv420(&[[255; 4]; 4], 2, 2).unwrap();
        assert_eq!((y, u, v), (vec![235; 4], vec![128], vec![128]));
        assert!(quantizer(100) < quantizer(50) && quantizer(50) < quantizer(1));
        assert!(Av1Encoder::new(&VideoConfig { width: 8, height: 8, fps: 30, quality: 50, speed: 10 }).is_err());
    }

    #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
    #[test]
    fn encodes_av1_into_a_playable_mp4() {
        let c = VideoConfig { width: 64, height: 48, fps: 24, quality: 60, speed: 10 };
        let mut e = Av1Encoder::new(&c).unwrap();
        let mut out = Vec::new();
        for (i, periods) in [(0u8, 24u64), (80, 1), (160, 1), (240, 30)] {
            let img = vec![[i, 255 - i, 128, 255]; 64 * 48];
            out.extend(e.push(&img, periods, periods > 1).unwrap());
        }
        out.extend(e.finish().unwrap());
        assert_eq!(out.len(), 4, "one sample per frame sent");
        assert!(out[0].key);
        assert!(out[3].key, "requested key frame");
        assert_eq!(out.iter().map(|s| s.duration).collect::<Vec<_>>(), [24, 1, 1, 30]);
        for s in &out {
            assert!(obus(&s.data).unwrap().iter().all(|(k, _)| *k != OBU_TEMPORAL_DELIMITER));
        }
        let config = e.config_record().unwrap();
        assert_eq!(obus(&config[4..]).unwrap()[0].0, OBU_SEQUENCE_HEADER);
        let mut m = VideoWriter::new(Cursor::new(Vec::new()), Container::Mp4, 64, 48, 24).unwrap();
        m.set_config(config);
        for s in &out {
            m.add(s).unwrap();
        }
        let d = m.finish().unwrap().into_inner();
        assert_eq!(boxes(&d, 0..d.len()).iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![*b"ftyp", *b"free", *b"mdat", *b"moov"]);
    }

    #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
    #[test]
    fn av1_buffers_a_large_frame_and_preserves_its_duration() {
        let c = VideoConfig { width: 4112, height: 16, fps: 30, quality: 60, speed: 10 };
        let mut encoder = Av1Encoder::new(&c).unwrap();
        let mut samples = encoder.push(&vec![[80, 120, 180, 255]; 4112 * 16], 7, true).unwrap();
        samples.extend(encoder.finish().unwrap());
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].duration, 7);
        assert!(samples[0].key);
        assert!(encoder.config_record().is_some());
    }

    #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
    #[test]
    fn av1_preserves_frame_sizes_above_the_old_area_limit() {
        for (width, height) in [(5120, 2160), (4096, 4096)] {
            let cfg = VideoConfig { width, height, fps: 30, quality: 60, speed: 10 };
            let mut encoder = Av1Encoder::new(&cfg).unwrap();
            let mut samples = encoder.push(&vec![[80, 120, 180, 255]; width as usize * height as usize], 7, true).unwrap();
            samples.extend(encoder.finish().unwrap());
            assert_eq!(samples.len(), 1, "{width}x{height}");
            assert!(!samples[0].data.is_empty());
            assert_eq!(samples[0].duration, 7);
            assert!(samples[0].key);
            assert!(encoder.config_record().is_some());
        }
    }

    #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
    #[test]
    fn av1_quality_100_preserves_the_exact_zero_quantizer() {
        assert_eq!(quantizer(100), 0);
        let image: Vec<[u8; 4]> = (0..64 * 48).map(|i| [(i * 13) as u8, (i * 41) as u8, (i * 73) as u8, 255]).collect();
        let cfg = VideoConfig { width: 64, height: 48, fps: 24, quality: 100, speed: 10 };
        let mut actual = Av1Encoder::new(&cfg).unwrap();
        let mut samples = actual.push(&image, 7, true).unwrap();
        samples.extend(actual.finish().unwrap());

        let (y, u, v) = yuv420(&image, 64, 48).unwrap();
        let y: Vec<u16> = y.into_iter().map(u16::from).collect();
        let u: Vec<u16> = u.into_iter().map(u16::from).collect();
        let v: Vec<u16> = v.into_iter().map(u16::from).collect();
        let frame = craft_video::av1::Frame { y: &y, u: &u, v: &v, y_stride: 64, uv_stride: 32 };
        let mut ec = craft_video::av1::EncoderConfig::new(64, 48, 24, 1);
        ec.rate = craft_video::av1::RateControl::ConstantQ(0);
        ec.keyint = 120;
        let mut exact = craft_video::av1::LargeEncoder::with_exact_quantizer(&ec, 10).unwrap();
        let mut expected = exact.push(&frame, true).unwrap();
        expected.extend(exact.finish().unwrap());
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].data, expected[0].packet.data);
        assert_eq!(samples[0].duration, 7);
        assert!(samples[0].key);
    }

    #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
    #[test]
    fn av1_speed_affects_normal_sized_frames() {
        let image: Vec<[u8; 4]> = (0..64 * 48).map(|i| [(i * 13) as u8, (i * 41) as u8, (i * 73) as u8, 255]).collect();
        let encode = |speed| {
            let c = VideoConfig { width: 64, height: 48, fps: 24, quality: 60, speed };
            let mut encoder = Av1Encoder::new(&c).unwrap();
            let mut samples = encoder.push(&image, 1, true).unwrap();
            samples.extend(encoder.finish().unwrap());
            samples.remove(0).data
        };
        assert_ne!(encode(1), encode(10), "AV1 speed must reach the shared encoder");
    }
}
