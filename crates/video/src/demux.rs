//! Containers: MP4/MOV, Matroska/WebM, MPEG-TS/M2TS/MTS (and MPEG program/elementary streams),
//! MXF. Each is reduced to a [`Clip`]: stream facts, the codec to decode with, and a sample
//! [`Table`] in one shape, so seeking and decoding are container-independent.
//!
//! Matroska and TS/MXF video mapping follow `filmcraft-codecs` (MIT OR Apache-2.0).

use std::path::Path;

use filmcraft_isobmff::CodecConfig;

use crate::colour::{self, Codes};
use crate::decode::{self, Codec};
use crate::source::FileSource;
use crate::{VideoError, VideoInfo};

/// Bound on open-GOP back-steps when looking for a decodable start (leading pictures).
const MAX_GOP_STEPS: usize = 8;

/// The video samples in decode order, with presentation times in units of `tb` seconds.
pub struct Table {
    /// Presentation time of each sample; samples that are never shown (leading pictures before
    /// the first random-access point) have a unique negative value below every shown one.
    pub pts: Vec<i64>,
    pub key: Vec<bool>,
    /// Shown samples sorted by presentation time.
    pub order: Vec<usize>,
    /// Seconds per pts unit, as (num, den).
    pub tb: (u64, u64),
}

impl Table {
    fn new(pts: Vec<i64>, key: Vec<bool>, tb: (u64, u64), shown: impl Fn(usize) -> bool) -> Self {
        let mut order: Vec<usize> = (0..pts.len()).filter(|&i| shown(i)).collect();
        order.sort_by_key(|&i| (pts.get(i).copied().unwrap_or(0), i));
        let tb = if tb.0 == 0 || tb.1 == 0 { (1, 30) } else { tb };
        Table { pts, key, order, tb }
    }

    pub fn seconds(&self, pts: i64) -> f64 {
        pts as f64 * self.tb.0 as f64 / self.tb.1 as f64
    }

    pub fn units(&self, t: f64) -> i64 {
        let u = (t * self.tb.1 as f64 / self.tb.0 as f64).round();
        if u.is_finite() { u.clamp(-(i64::MAX as f64) / 2.0, i64::MAX as f64 / 2.0) as i64 } else { 0 }
    }

    pub fn pts(&self, i: usize) -> Option<i64> {
        self.pts.get(i).copied()
    }

    /// The shown sample displayed at `u` (the last one starting at or before it; the first one
    /// for earlier times).
    pub fn sample_at(&self, u: i64) -> Option<usize> {
        let k = self.order.partition_point(|&i| self.pts.get(i).is_some_and(|&p| p <= u));
        self.order.get(k.saturating_sub(1)).copied()
    }

    /// The sample decoding must start from to reach sample `i`: the preceding sync sample, or an
    /// earlier one when `i` is a leading picture of an open GOP (shown before its sync sample).
    pub fn sync_before(&self, i: usize) -> usize {
        let mut i = i.min(self.key.len().saturating_sub(1));
        let target = self.pts(i);
        for _ in 0..MAX_GOP_STEPS {
            let key = (0..=i).rev().find(|&k| self.key.get(k).copied().unwrap_or(false)).unwrap_or(0);
            match (target, self.pts(key)) {
                (Some(t), Some(kp)) if key > 0 && t < kp => i = key - 1,
                _ => return key,
            }
        }
        (0..=i).rev().find(|&k| self.key.get(k).copied().unwrap_or(false)).unwrap_or(0)
    }

    pub fn len(&self) -> usize {
        self.pts.len()
    }
}

pub enum Container {
    Mp4 {
        file: Box<filmcraft_isobmff::Mp4File>,
        track: usize,
    },
    Mkv {
        file: Box<filmcraft_matroska::MkvFile>,
        track: usize,
    },
    /// `units[i]` is the stream unit of sample i.
    Ts {
        file: Box<filmcraft_mpegts::File>,
        stream: usize,
        units: Vec<usize>,
    },
    /// MPEG-1/2 video elementary stream, read whole (capped) at open.
    Es {
        data: Vec<u8>,
        units: Vec<std::ops::Range<usize>>,
    },
    Mxf {
        file: Box<filmcraft_mxf::MxfFile>,
        track: usize,
    },
}

pub struct Clip {
    pub src: FileSource,
    pub container: Container,
    pub info: VideoInfo,
    /// The container's colour description, overriding the stream's.
    pub colr: Option<Codes>,
    /// None when the video can't be decoded; `unsupported` says why.
    pub codec: Option<Codec>,
    pub unsupported: Option<String>,
    pub table: Table,
}

impl Clip {
    pub fn read(&self, i: usize) -> Result<Vec<u8>, VideoError> {
        let invalid = |e: String| VideoError::Invalid(e);
        match &self.container {
            Container::Mp4 { file, track } => file.read_sample(&self.src, *track, i).map_err(demux_error),
            Container::Mkv { file, track } => file.read_sample(&self.src, *track, i).map_err(|e| invalid(e.to_string())),
            Container::Ts { file, stream, units } => {
                let u = *units.get(i).ok_or_else(|| invalid("sample index".into()))?;
                file.read_unit(&self.src, *stream, u).map_err(|e| invalid(e.to_string()))
            }
            Container::Es { data, units } => {
                let r = units.get(i).ok_or_else(|| invalid("sample index".into()))?;
                data.get(r.clone()).map(<[u8]>::to_vec).ok_or_else(|| invalid("access unit out of range".into()))
            }
            Container::Mxf { file, track } => file.read_sample(&self.src, *track, i).map_err(|e| invalid(e.to_string())),
        }
    }
}

pub fn demux_error(e: filmcraft_isobmff::Error) -> VideoError {
    match e {
        filmcraft_isobmff::Error::Io(e) => VideoError::Io(e.to_string()),
        e => VideoError::Invalid(e.to_string()),
    }
}

fn io(path: &Path, e: std::io::Error) -> VideoError {
    VideoError::Io(format!("{}: {e}", path.display()))
}

/// Largest MPEG video elementary stream read into memory (they have no index to seek with).
const MAX_ES_BYTES: u64 = 512 << 20;

pub fn open(path: &Path) -> Result<Clip, VideoError> {
    let src = FileSource::open(path).map_err(|e| io(path, e))?;
    let mut head = vec![0u8; src.len().min(64 << 10) as usize];
    src.read(0, &mut head).map_err(|e| io(path, e))?;
    let mut clip = open_container(src, &head)?;
    pixel_aspect(&mut clip);
    Ok(clip)
}

fn open_container(src: FileSource, head: &[u8]) -> Result<Clip, VideoError> {
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        open_mkv(src)
    } else if filmcraft_mxf::sniff(head) {
        open_mxf(src)
    } else if filmcraft_mpegts::sniff(head).is_some() {
        open_ts(src)
    } else if head.starts_with(&[0, 0, 1, 0xB3]) {
        open_es(src)
    } else {
        open_mp4(src)
    }
}

/// Reduce a pixel aspect ratio; None for 1:1, zero or absurd values (outside 1:8 .. 8:1).
pub fn reduce_par(n: u64, d: u64) -> Option<(u32, u32)> {
    if n == 0 || d == 0 || n == d || n > d.saturating_mul(8) || d > n.saturating_mul(8) {
        return None;
    }
    let g = gcd(n, d);
    Some((u32::try_from(n / g).ok()?, u32::try_from(d / g).ok()?))
}

/// The container's pixel aspect ratio (MP4 `pasp`, Matroska display size, MXF aspect ratio).
fn container_par(clip: &Clip, coded: (u32, u32)) -> Option<(u32, u32)> {
    match &clip.container {
        Container::Mp4 { file, track } => {
            let e = file.tracks.get(*track)?.entries.first()?;
            let (h, v) = e.video.as_ref()?.pixel_aspect?;
            reduce_par(h.into(), v.into())
        }
        Container::Mkv { file, track } => {
            let (n, d) = file.tracks.get(*track)?.video.as_ref()?.pixel_aspect();
            reduce_par(n.into(), d.into())
        }
        Container::Mxf { file, track } => {
            let ar = file.tracks.get(*track)?.picture.as_ref()?.aspect_ratio;
            let (n, d) = (u64::try_from(ar.num).ok()?, u64::try_from(ar.den).ok()?);
            // display aspect → PAR = DAR · h / w
            reduce_par(n.checked_mul(coded.1.into())?, d.checked_mul(coded.0.into())?)
        }
        Container::Ts { .. } | Container::Es { .. } => None,
    }
}

/// The bitstream's sample aspect ratio (H.264 / HEVC VUI, MPEG-2 sequence header) and whether the
/// sequence is interlaced (MPEG-2 only).
fn bitstream_par(clip: &Clip) -> (Option<(u32, u32)>, bool) {
    let first = || clip.table.order.first().and_then(|&i| clip.read(i).ok()).or_else(|| clip.read(0).ok());
    let vui_sar = |sar: (u16, u16)| reduce_par(sar.0.into(), sar.1.into());
    let h264 = |nal: &[u8]| -> Option<(u32, u32)> {
        let sps = filmcraft_h264::params::Sps::parse(&filmcraft_bitstream::unescape_rbsp(nal.get(1..)?)).ok()?;
        vui_sar(sps.vui?.sar)
    };
    let hevc = |nal: &[u8]| -> Option<(u32, u32)> {
        let sps = filmcraft_hevc::params::Sps::parse(&filmcraft_bitstream::unescape_rbsp(nal.get(2..)?)).ok()?;
        vui_sar(sps.vui?.sar)
    };
    match &clip.codec {
        Some(Codec::H264 { avcc: Some(a) }) => (filmcraft_isobmff::AvcConfig::parse(a).ok().and_then(|c| c.sps.first().and_then(|n| h264(n))), false),
        Some(Codec::H264 { avcc: None }) => {
            let sample = first().unwrap_or_default();
            let nal = filmcraft_bitstream::annexb_nals(&sample).into_iter().find(|n| n.first().is_some_and(|h| h & 0x1f == 7)).map(<[u8]>::to_vec);
            (nal.and_then(|n| h264(&n)), false)
        }
        Some(Codec::Hevc { hvcc: Some(c) }) => {
            let cfg = filmcraft_isobmff::HevcConfig::parse(c).ok();
            let nal = cfg.and_then(|c| c.arrays.into_iter().find(|a| a.nal_type == 33).and_then(|a| a.nalus.into_iter().next()));
            (nal.and_then(|n| hevc(&n)), false)
        }
        Some(Codec::Hevc { hvcc: None }) => {
            let sample = first().unwrap_or_default();
            let nal =
                filmcraft_bitstream::annexb_nals(&sample).into_iter().find(|n| n.first().is_some_and(|h| (h >> 1) & 0x3f == 33)).map(<[u8]>::to_vec);
            (nal.and_then(|n| hevc(&n)), false)
        }
        Some(Codec::Mpeg2 { header }) => {
            let seq = filmcraft_mpeg2v::probe(header).or_else(|| first().and_then(|s| filmcraft_mpeg2v::probe(&s)));
            match seq {
                Some(q) => (reduce_par(q.sar.0.into(), q.sar.1.into()), q.mpeg2 && !q.progressive_sequence),
                None => (None, false),
            }
        }
        _ => (None, false),
    }
}

/// Display size of a `w × h` picture with pixel aspect `par`: the wider-than-square axis is
/// stretched (anamorphic 1440×1080 at 4:3 → 1920×1080; 720×480 at 10:11 → 720×528).
pub fn display_size(w: u32, h: u32, par: (u32, u32)) -> (u32, u32) {
    let (n, d) = (u64::from(par.0.max(1)), u64::from(par.1.max(1)));
    let scale = |v: u32, a: u64, b: u64| u32::try_from((u64::from(v) * a + b / 2) / b).unwrap_or(v).max(1);
    if n > d {
        (scale(w, n, d), h)
    } else if n < d {
        (w, scale(h, d, n))
    } else {
        (w, h)
    }
}

/// Fill `par_*`, `coded_*` and `interlaced` and apply the pixel aspect to the display size.
fn pixel_aspect(clip: &mut Clip) {
    let rot = clip.info.rotation % 2 == 1;
    let (w, h) = if rot { (clip.info.height, clip.info.width) } else { (clip.info.width, clip.info.height) };
    let (bits, interlaced) = bitstream_par(clip);
    // the container wins (it is what players honour), then the bitstream
    let par = container_par(clip, (w, h)).or(bits).unwrap_or((1, 1));
    let (dw, dh) = display_size(w, h, par);
    let i = &mut clip.info;
    (i.par_num, i.par_den, i.interlaced) = (par.0, par.1, interlaced);
    (i.coded_width, i.coded_height) = if rot { (h, w) } else { (w, h) };
    (i.width, i.height) = if rot { (dh, dw) } else { (dw, dh) };
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

/// A frame rate as a reduced (num, den), never zero.
fn rate(num: u64, den: u64) -> (u32, u32) {
    if num == 0 || den == 0 {
        return (30, 1);
    }
    let g = gcd(num, den);
    let (n, d) = (num / g, den / g);
    match (u32::try_from(n), u32::try_from(d)) {
        (Ok(n), Ok(d)) => (n, d),
        _ => {
            let f = n as f64 / d as f64;
            (((f * 1000.0).round().clamp(1.0, u32::MAX as f64)) as u32, 1000)
        }
    }
}

/// The most common positive step between consecutive presentation times (frame duration).
fn common_step(pts: &[i64], order: &[usize]) -> Option<u64> {
    let mut steps: Vec<u64> =
        order.windows(2).filter_map(|w| pts.get(*w.get(1)?)?.checked_sub(*pts.get(*w.first()?)?)).filter(|&d| d > 0).map(|d| d as u64).collect();
    steps.sort_unstable();
    let mut best = (0u64, 0usize);
    let mut i = 0;
    while let Some(&d) = steps.get(i) {
        let run = steps.get(i..).map_or(0, |r| r.iter().take_while(|&&x| x == d).count()).max(1);
        if run > best.1 {
            best = (d, run);
        }
        i += run;
    }
    (best.0 > 0).then_some(best.0)
}

#[allow(clippy::too_many_arguments)]
fn info(
    duration: f64,
    fps: (u32, u32),
    (w, h): (u32, u32),
    rotation: u8,
    codec: String,
    has_audio: bool,
    created: Option<String>,
    gps: Option<(f64, f64)>,
    hdr: bool,
    decodable: bool,
) -> VideoInfo {
    let (width, height) = if rotation % 2 == 1 { (h, w) } else { (w, h) };
    let duration = if duration.is_finite() && duration > 0.0 { duration } else { 0.0 };
    VideoInfo {
        duration,
        fps_num: fps.0,
        fps_den: fps.1,
        width,
        height,
        codec,
        has_audio,
        created,
        gps,
        rotation,
        hdr,
        decodable,
        coded_width: width,
        coded_height: height,
        par_num: 1,
        par_den: 1,
        interlaced: false,
    }
}

fn hdr_of(w: u32, h: u32, colr: Option<Codes>) -> bool {
    colr.is_some_and(|c| colour::is_hdr(&colour::resolve(w, h, None, Some(c))))
}

// ---------------------------------------------------------------------------------------------
// MP4 / MOV

fn mp4_codec(c: &CodecConfig, fourcc: [u8; 4]) -> (String, Result<Codec, String>) {
    match c {
        CodecConfig::Avc(a) => {
            let r = match decode::h264_avcc_unsupported(a) {
                Some(VideoError::Unsupported(w)) => Err(w),
                _ => Ok(Codec::H264 { avcc: Some(a.to_bytes()) }),
            };
            ("h264".into(), r)
        }
        CodecConfig::Hevc(c) => ("hevc".into(), Ok(Codec::Hevc { hvcc: Some(c.to_bytes()) })),
        CodecConfig::Av1(c) => ("av1".into(), Ok(Codec::Av1 { obus: c.config_obus.clone() })),
        CodecConfig::Vp9(_) => ("vp9".into(), Ok(Codec::Vp9)),
        CodecConfig::ProRes { .. } => ("prores".into(), Ok(Codec::ProRes)),
        CodecConfig::Apv(_) => ("apv".into(), Ok(Codec::Apv)),
        CodecConfig::Dnx { .. } => ("dnxhd".into(), Ok(Codec::Dnx)),
        _ if is_mpeg2_fourcc(&fourcc) => ("mpeg2".into(), Ok(Codec::Mpeg2 { header: Vec::new() })),
        _ => {
            let name = String::from_utf8_lossy(&fourcc).trim().to_string();
            let why = format!("{name} video");
            (name, Err(why))
        }
    }
}

/// QuickTime / MP4 sample entries carrying MPEG-1/2 video (XDCAM, IMX, HDV, generic).
fn is_mpeg2_fourcc(f: &[u8; 4]) -> bool {
    matches!(
        f,
        b"mp2v" | b"mpg2" | b"m2v1" | b"mpg1" | b"m1v " | b"m1v1" | b"mx3n" | b"mx4n" | b"mx5n" | b"mx3p" | b"mx4p" | b"mx5p" | b"xdhd" | b"xdh2"
    ) || matches!(f, [b'x', b'd', b'v' | b'5', _] | [b'h', b'd', b'v', _])
}

fn mp4_colr(c: &filmcraft_isobmff::ColorInfo) -> Option<Codes> {
    match *c {
        filmcraft_isobmff::ColorInfo::Nclx { primaries, transfer, matrix, full_range } => {
            Some(Codes { primaries, transfer, matrix, full_range: Some(full_range) })
        }
        filmcraft_isobmff::ColorInfo::Nclc { primaries, transfer, matrix } => Some(Codes { primaries, transfer, matrix, full_range: None }),
        filmcraft_isobmff::ColorInfo::Icc { .. } => None,
    }
}

fn open_mp4(src: FileSource) -> Result<Clip, VideoError> {
    use filmcraft_isobmff::TrackKind;
    let file = filmcraft_isobmff::open(&src).map_err(demux_error)?;
    let track = file.track_of_kind(TrackKind::Video).ok_or_else(|| VideoError::Invalid("no video track".into()))?;
    let t = file.tracks.get(track).ok_or_else(|| VideoError::Invalid("no video track".into()))?;
    let entry = t.entries.first().ok_or_else(|| VideoError::Invalid("video track without a sample description".into()))?;
    let (name, codec) = mp4_codec(&entry.codec, entry.format.0);
    let vp = entry.video.as_ref();
    // VP9 carries its transfer and primaries in `vpcC`, which a `colr` box overrides
    let vpc = match &entry.codec {
        CodecConfig::Vp9(v) => Some(Codes {
            primaries: v.colour_primaries.into(),
            transfer: v.transfer_characteristics.into(),
            matrix: v.matrix_coefficients.into(),
            full_range: Some(v.full_range),
        }),
        _ => None,
    };
    let colr = vp.and_then(|v| v.color.as_ref()).and_then(mp4_colr).or(vpc);
    let (mut w, mut h) = vp.map_or((t.width, t.height), |v| (u32::from(v.width), u32::from(v.height)));
    if w == 0 || h == 0 {
        (w, h) = (t.width, t.height);
    }
    let rotation = t.display_rotation().unwrap_or(0);
    let duration = if file.timescale > 0 && file.duration > 0 {
        file.duration as f64 / f64::from(file.timescale)
    } else if t.timescale > 0 {
        t.duration as f64 / f64::from(t.timescale)
    } else {
        0.0
    };
    let pts: Vec<i64> = (0..t.samples.len()).map(|i| t.presentation_pts(i).or_else(|| t.samples.get(i).map(|s| s.pts)).unwrap_or(0)).collect();
    let key: Vec<bool> = t.samples.iter().map(|s| s.is_sync).collect();
    let table = Table::new(pts, key, (1, u64::from(t.timescale)), |_| true);
    let fps = mp4_rate(t);
    let md = &file.metadata;
    let created = md.get("com.apple.quicktime.creationdate").or_else(|| md.get("©day")).or_else(|| md.get("date")).map(str::to_string);
    let gps = md.get("com.apple.quicktime.location.ISO6709").or_else(|| md.get("©xyz")).and_then(crate::parse_iso6709);
    let has_audio = file.track_of_kind(TrackKind::Audio).is_some();
    let (codec, unsupported) = split(codec, table.len());
    let info = info(duration, fps, (w, h), rotation, name, has_audio, created, gps, hdr_of(w, h, colr), codec.is_some());
    Ok(Clip { src, container: Container::Mp4 { file: Box::new(file), track }, info, colr, codec, unsupported, table })
}

/// Frame rate from the track timescale and the most common sample duration.
fn mp4_rate(t: &filmcraft_isobmff::Track) -> (u32, u32) {
    let durs: Vec<i64> = t.samples.iter().map(|s| i64::from(s.duration)).collect();
    // reuse the step histogram: cumulative durations are steps
    let mut acc = 0i64;
    let mut pts = Vec::with_capacity(durs.len() + 1);
    pts.push(0);
    for d in durs {
        acc = acc.saturating_add(d);
        pts.push(acc);
    }
    let order: Vec<usize> = (0..pts.len()).collect();
    match common_step(&pts, &order) {
        Some(step) => rate(u64::from(t.timescale), step),
        None => (30, 1),
    }
}

/// A codec with no samples to decode is unsupported too.
fn split(codec: Result<Codec, String>, samples: usize) -> (Option<Codec>, Option<String>) {
    match codec {
        Ok(_) if samples == 0 => (None, Some("the video track has no samples".into())),
        Ok(c) => (Some(c), None),
        Err(why) => (None, Some(why)),
    }
}

// ---------------------------------------------------------------------------------------------
// Matroska / WebM

fn mkv_codec(c: &filmcraft_matroska::Codec, private: &[u8]) -> (String, Result<Codec, String>) {
    use filmcraft_matroska::Codec as M;
    match c {
        M::Avc { avcc } => {
            let r = match filmcraft_isobmff::AvcConfig::parse(avcc) {
                Ok(a) => match decode::h264_avcc_unsupported(&a) {
                    Some(VideoError::Unsupported(w)) => Err(w),
                    _ => Ok(Codec::H264 { avcc: Some(avcc.clone()) }),
                },
                Err(e) => Err(format!("H.264 configuration: {e}")),
            };
            ("h264".into(), r)
        }
        M::Hevc { hvcc } => ("hevc".into(), Ok(Codec::Hevc { hvcc: Some(hvcc.clone()) })),
        M::Vp9 { .. } => ("vp9".into(), Ok(Codec::Vp9)),
        M::Av1 { av1c } => {
            let obus = filmcraft_isobmff::Av1Config::parse(av1c).map(|c| c.config_obus).unwrap_or_default();
            ("av1".into(), Ok(Codec::Av1 { obus }))
        }
        M::Apv { .. } => ("apv".into(), Ok(Codec::Apv)),
        M::ProRes { .. } => ("prores".into(), Ok(Codec::ProRes)),
        M::Other(id) if id == "V_MPEG2" || id == "V_MPEG1" => {
            ("mpeg2".into(), Ok(Codec::Mpeg2 { header: mpeg2_sequence_header(private).unwrap_or_default() }))
        }
        M::Vp8 => ("vp8".into(), Err("VP8 video".into())),
        M::Mjpeg => ("mjpeg".into(), Err("Motion JPEG video".into())),
        other => {
            let name = other.name().to_string();
            let why = format!("{name} video");
            (name, Err(why))
        }
    }
}

/// The track's `Colour` element as H.273 code points (unset fields are "unspecified").
fn mkv_colr(v: Option<&filmcraft_matroska::VideoInfo>) -> Option<Codes> {
    let col = v?.colour.as_ref()?;
    let code = |c: Option<u64>| c.and_then(|c| u16::try_from(c).ok()).unwrap_or(2);
    let full_range = match col.range {
        Some(1) => Some(false),
        Some(2) => Some(true),
        _ => None,
    };
    Some(Codes { primaries: code(col.primaries), transfer: code(col.transfer_characteristics), matrix: code(col.matrix_coefficients), full_range })
}

/// RFC 3339 for a Matroska `DateUTC` (nanoseconds since 2001-01-01T00:00:00Z).
fn mkv_date(ns: i64) -> Option<String> {
    let secs = ns.div_euclid(1_000_000_000).checked_add(978_307_200)?;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // civil from days (Howard Hinnant's algorithm)
    let z = days.checked_add(719_468)?;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (0..=9999).contains(&y).then(|| format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60))
}

fn open_mkv(src: FileSource) -> Result<Clip, VideoError> {
    use filmcraft_matroska::TrackKind;
    let file = filmcraft_matroska::open(&src).map_err(|e| VideoError::Invalid(format!("Matroska: {e}")))?;
    let track = file
        .tracks
        .iter()
        .position(|t| t.kind == TrackKind::Video && !t.samples.is_empty())
        .or_else(|| file.track_of_kind(TrackKind::Video))
        .ok_or_else(|| VideoError::Invalid("no video track".into()))?;
    let t = file.tracks.get(track).ok_or_else(|| VideoError::Invalid("no video track".into()))?;
    let v = t.video.as_ref();
    let (w, h) = v.map_or((0, 0), |v| (v.pixel_width, v.pixel_height));
    let rotation = v.and_then(|v| v.display_rotation()).unwrap_or(0);
    let (name, codec) = mkv_codec(&t.codec, &t.codec_private);
    let colr = mkv_colr(v);
    let (n, d) = t.timebase;
    let pts: Vec<i64> = t.samples.iter().map(|s| s.pts).collect();
    let key: Vec<bool> = t.samples.iter().map(|s| s.keyframe).collect();
    let table = Table::new(pts, key, (n, d), |_| true);
    let fps = match t.default_duration_ns {
        Some(ns) if ns > 0 => rate(1_000_000_000, ns),
        _ => match common_step(&table.pts, &table.order) {
            // step × n/d seconds per frame
            Some(step) => rate(d, step.saturating_mul(n)),
            None => (30, 1),
        },
    };
    let first = table.order.first().and_then(|&i| table.pts(i)).unwrap_or(0);
    let last = table.order.last().and_then(|&i| table.pts(i)).unwrap_or(0);
    let frame = f64::from(fps.1) / f64::from(fps.0.max(1));
    let duration = match file.duration_ns() {
        Some(ns) => ns as f64 / 1e9 - table.seconds(first),
        None => table.seconds(last.saturating_sub(first)) + frame,
    };
    let has_audio = file.tracks.iter().any(|t| t.kind == TrackKind::Audio && !t.samples.is_empty());
    let created = file.info.date_utc.and_then(mkv_date);
    let (codec, unsupported) = split(codec, table.len());
    let info = info(duration, fps, (w, h), rotation, name, has_audio, created, None, hdr_of(w, h, colr), codec.is_some());
    Ok(Clip { src, container: Container::Mkv { file: Box::new(file), track }, info, colr, codec, unsupported, table })
}

// ---------------------------------------------------------------------------------------------
// MPEG transport / program streams and MPEG video elementary streams

/// The sequence header with its extensions at the start of an MPEG video sample.
pub fn mpeg2_sequence_header(sample: &[u8]) -> Option<Vec<u8>> {
    let codes = filmcraft_mpeg2v::start_codes(sample);
    let (start, _) = *codes.iter().find(|c| c.1 == 0xB3)?;
    let end = codes.iter().find(|c| c.0 > start && matches!(c.1, 0x00 | 0xB8 | 0x01..=0xAF)).map_or(sample.len(), |c| c.0);
    sample.get(start..end).map(<[u8]>::to_vec)
}

/// Display positions for samples in decode order from `keys` (decode order, from a
/// random-access unit): samples shown before the first (open-GOP leading pictures) get unique
/// negative positions.
fn positions(keys: &[i64]) -> Vec<i64> {
    let k0 = keys.first().copied().unwrap_or(0);
    let mut idx: Vec<usize> = (0..keys.len()).filter(|&i| keys.get(i).is_some_and(|&k| k >= k0)).collect();
    idx.sort_by_key(|&i| (keys.get(i).copied().unwrap_or(0), i));
    let mut pts: Vec<i64> = (0..keys.len()).map(|i| -(i as i64) - 1).collect();
    for (d, &i) in idx.iter().enumerate() {
        if let Some(p) = pts.get_mut(i) {
            *p = d as i64;
        }
    }
    pts
}

/// The MPEG video units as a table with display positions at `fps` (TS / PS / ES).
fn mpeg_table(keys: &[i64], key: Vec<bool>, fps: (u32, u32)) -> Table {
    let pts = positions(keys);
    let shown: Vec<bool> = pts.iter().map(|&p| p >= 0).collect();
    Table::new(pts, key, (u64::from(fps.1), u64::from(fps.0)), |i| shown.get(i).copied().unwrap_or(false))
}

fn open_ts(src: FileSource) -> Result<Clip, VideoError> {
    use filmcraft_mpegts::{Codec as T, Kind};
    let file = filmcraft_mpegts::open(&src).map_err(|e| match e {
        filmcraft_mpegts::Error::NotMpeg => VideoError::Invalid("not an MPEG stream".into()),
        other => VideoError::Invalid(format!("MPEG-TS: {other}")),
    })?;
    let has_audio = file.streams.iter().any(|s| s.kind() == Kind::Audio && !s.units.is_empty());
    let stream = file
        .find(Kind::Video, |c| matches!(c, T::Mpeg1Video | T::Mpeg2Video | T::H264 | T::Hevc))
        .or_else(|| file.find(Kind::Video, |_| true))
        .ok_or_else(|| VideoError::Invalid(format!("{}: no video stream", file.format.name())))?;
    let st = file.streams.get(stream).ok_or_else(|| VideoError::Invalid("no video stream".into()))?;
    let first_key = st.units.iter().position(|u| u.key);
    let sel = first_key.and_then(|k| st.units.get(k..)).unwrap_or(&[]);
    let mpeg = matches!(st.codec, T::Mpeg1Video | T::Mpeg2Video);
    // display keys: PTS when every unit has one, else (MPEG video) GOP start + temporal reference
    let keys: Vec<i64> = if sel.iter().all(|u| u.pts.is_some()) {
        sel.iter().map(|u| u.pts.unwrap_or(0)).collect()
    } else if mpeg {
        let mut base = 0i64;
        sel.iter()
            .enumerate()
            .map(|(i, u)| {
                if let Some(p) = u.picture
                    && (p.gop || i == 0)
                {
                    base = i as i64;
                }
                (base + u.picture.map_or(0, |p| i64::from(p.temporal_reference))) * 4
            })
            .collect()
    } else {
        (0..sel.len() as i64).collect()
    };
    let units: Vec<usize> = first_key.map_or(Vec::new(), |k| (k..st.units.len()).collect());
    let key: Vec<bool> = sel.iter().map(|u| u.key).collect();
    let mut clip_codec: Result<Codec, String>;
    let name: String;
    let (mut w, mut h) = (0, 0);
    let mut coded_rate = None;
    let first = match first_key {
        Some(k) => file.read_unit(&src, stream, k).ok(),
        None => None,
    };
    match st.codec {
        T::Mpeg1Video | T::Mpeg2Video => {
            name = "mpeg2".into();
            let info = first.as_deref().and_then(filmcraft_mpeg2v::probe);
            if let Some(i) = &info {
                (w, h) = (i.width, i.height);
                coded_rate = i.frame_rate;
            }
            let header = first.as_deref().and_then(mpeg2_sequence_header).unwrap_or_default();
            clip_codec = Ok(Codec::Mpeg2 { header });
        }
        T::H264 => {
            name = "h264".into();
            clip_codec = Ok(Codec::H264 { avcc: None });
            if let Some((nal, sw, sh)) = first.as_deref().and_then(decode::h264_annexb_sps) {
                (w, h) = (sw, sh);
                if let Some(VideoError::Unsupported(why)) = decode::h264_sps_unsupported(&nal) {
                    clip_codec = Err(why);
                }
            }
        }
        T::Hevc => {
            name = "hevc".into();
            clip_codec = Ok(Codec::Hevc { hvcc: None });
        }
        ref other => {
            name = other.name().to_lowercase();
            clip_codec = Err(format!("{} video in {}", other.name(), file.format.name()));
        }
    }
    if first_key.is_none() && clip_codec.is_ok() {
        clip_codec = Err("no random-access picture in the video stream".into());
    }
    // rate: the coded one unless the timestamps say otherwise (3:2 pulldown, field-rate coding)
    let pts_rate = {
        let mut shown: Vec<i64> = sel.iter().filter_map(|u| u.pts).collect();
        shown.sort_unstable();
        match (shown.first(), shown.last()) {
            (Some(&a), Some(&b)) if shown.len() >= 2 && b > a && shown.len() == sel.len() => {
                Some((shown.len() - 1) as f64 * 90_000.0 / (b - a) as f64)
            }
            _ => None,
        }
    };
    let fps = match (coded_rate, pts_rate) {
        (Some((n, d)), Some(p)) if d > 0 && ((f64::from(n) / f64::from(d)) - p).abs() / p > 0.01 => rate((p * 1000.0).round() as u64, 1000),
        (Some((n, d)), _) => rate(n.into(), d.into()),
        (None, Some(p)) => rate((p * 1000.0).round() as u64, 1000),
        (None, None) => (25, 1),
    };
    let table = mpeg_table(&keys, key, fps);
    let duration = table.order.len() as f64 * f64::from(fps.1) / f64::from(fps.0.max(1));
    let mut clip = Clip {
        src,
        container: Container::Ts { file: Box::new(file), stream, units },
        info: info(duration, fps, (w, h), 0, name, has_audio, None, None, false, false),
        colr: None,
        codec: None,
        unsupported: None,
        table,
    };
    (clip.codec, clip.unsupported) = split(clip_codec, clip.table.len());
    clip.info.decodable = clip.codec.is_some();
    if clip.info.width == 0 && clip.codec.is_some() {
        size_from_first_picture(&mut clip);
    }
    Ok(clip)
}

/// A raw MPEG-1/2 video elementary stream (no timestamps: GOP / temporal reference order).
fn open_es(src: FileSource) -> Result<Clip, VideoError> {
    let len = src.len();
    if len > MAX_ES_BYTES {
        return Err(VideoError::Unsupported(format!("MPEG video elementary stream larger than {} MB", MAX_ES_BYTES >> 20)));
    }
    let mut data = vec![0u8; len as usize];
    src.read(0, &mut data).map_err(|e| VideoError::Io(e.to_string()))?;
    let aus = filmcraft_mpeg2v::access_units(&data);
    let infos: Vec<filmcraft_mpeg2v::AccessUnitInfo> =
        aus.iter().map(|r| filmcraft_mpeg2v::scan_access_unit(data.get(r.clone()).unwrap_or(&[]))).collect();
    let first_key = infos.iter().position(|a| a.is_intra()).ok_or_else(|| VideoError::Invalid("no I picture in the stream".into()))?;
    let sel = infos.get(first_key..).unwrap_or(&[]);
    let mut keys = Vec::with_capacity(sel.len());
    let mut base = 0i64;
    for (k, a) in sel.iter().enumerate() {
        if a.gop.is_some() || k == 0 {
            base = k as i64;
        }
        keys.push((base + a.pictures.first().map_or(0, |p| i64::from(p.0.temporal_reference))) * 4);
    }
    let first = aus.get(first_key).and_then(|r| data.get(r.clone())).unwrap_or(&[]);
    let seq = filmcraft_mpeg2v::probe(first).ok_or_else(|| VideoError::Invalid("no MPEG video sequence header at the first I picture".into()))?;
    let header = mpeg2_sequence_header(first).unwrap_or_default();
    let fps = seq.frame_rate.map_or((25, 1), |(n, d)| rate(n.into(), d.into()));
    let table = mpeg_table(&keys, sel.iter().map(|a| a.is_intra()).collect(), fps);
    let units: Vec<std::ops::Range<usize>> = aus.get(first_key..).map(<[_]>::to_vec).unwrap_or_default();
    let duration = table.order.len() as f64 * f64::from(fps.1) / f64::from(fps.0.max(1));
    let (codec, unsupported) = split(Ok(Codec::Mpeg2 { header }), table.len());
    let info = info(duration, fps, (seq.width, seq.height), 0, "mpeg2".into(), false, None, None, false, codec.is_some());
    Ok(Clip { src, container: Container::Es { data, units }, info, colr: None, codec, unsupported, table })
}

/// Size (and HDR) of streams that only say so in their pictures (HEVC in TS): decode the first.
fn size_from_first_picture(clip: &mut Clip) {
    let Some(codec) = clip.codec.clone() else { return };
    let Ok(mut dec) = decode::Decoder::new(&codec) else { return };
    for i in 0..clip.table.len().min(32) {
        let Ok(data) = clip.read(i) else { return };
        let pics = match dec.decode(&data, i as i64, clip.colr) {
            Ok(p) => p,
            Err(VideoError::Unsupported(why)) => {
                clip.codec = None;
                clip.unsupported = Some(why);
                clip.info.decodable = false;
                return;
            }
            Err(_) => continue,
        };
        if let Some(p) = pics.first() {
            clip.info.width = p.frame.width;
            clip.info.height = p.frame.height;
            clip.info.hdr = colour::is_hdr(&p.frame.color);
            return;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// MXF

fn open_mxf(src: FileSource) -> Result<Clip, VideoError> {
    use filmcraft_mxf::{Codec as X, TrackKind};
    let file = filmcraft_mxf::open(&src).map_err(|e| match e {
        filmcraft_mxf::Error::Unsupported(s) => VideoError::Unsupported(format!("MXF: {s}")),
        other => VideoError::Invalid(format!("MXF: {other}")),
    })?;
    let track = file.track_of_kind(TrackKind::Picture).ok_or_else(|| VideoError::Invalid("MXF: no picture track".into()))?;
    let t = file.tracks.get(track).ok_or_else(|| VideoError::Invalid("MXF: no picture track".into()))?;
    let has_audio = file.tracks.iter().any(|t| t.kind == TrackKind::Sound && !t.chunks.is_empty());
    let p = t.picture.clone().unwrap_or_default();
    let first = file.read_sample(&src, track, 0).ok();
    let (mut w, mut h) = (p.frame_width(), p.frame_height());
    let (name, codec): (String, Result<Codec, String>) = match t.codec {
        X::Avc { .. } => {
            let mut c = Ok(Codec::H264 { avcc: None });
            if let Some((nal, sw, sh)) = first.as_deref().and_then(decode::h264_annexb_sps) {
                (w, h) = (sw, sh);
                if let Some(VideoError::Unsupported(why)) = decode::h264_sps_unsupported(&nal) {
                    c = Err(why);
                }
            }
            if t.needs_reorder {
                // B pictures without temporal offsets: presentation order would need the slice headers
                c = c.and(Err("H.264 in MXF with B pictures and no index temporal offsets".into()));
            }
            ("h264".into(), c)
        }
        X::Vc3 => ("dnxhd".into(), Ok(Codec::Dnx)),
        X::ProRes { .. } => ("prores".into(), Ok(Codec::ProRes)),
        X::Mpeg2 => {
            if let Some(i) = first.as_deref().and_then(filmcraft_mpeg2v::probe) {
                (w, h) = (i.width, i.height);
            }
            ("mpeg2".into(), Ok(Codec::Mpeg2 { header: first.as_deref().and_then(mpeg2_sequence_header).unwrap_or_default() }))
        }
        ref other => (other.name().to_lowercase(), Err(format!("{} video in MXF", other.name()))),
    };
    let er = t.edit_rate;
    let fps = match (u64::try_from(er.num), u64::try_from(er.den)) {
        (Ok(n), Ok(d)) if n > 0 && d > 0 => rate(n, d),
        _ => (25, 1),
    };
    // display positions from the first edit unit, for the track's duration
    let vfirst = t.first_edit_unit().max(0);
    let avail = (t.display_order.len() as i64).saturating_sub(vfirst);
    let vcount = t.duration.map_or(avail, |d| d.min(avail)).max(0);
    let n = t.samples.len();
    let mut pts: Vec<i64> = (0..n).map(|i| -(i as i64) - 1).collect();
    for (pos, &s) in t.display_order.iter().enumerate() {
        let d = pos as i64 - vfirst;
        if (0..vcount).contains(&d)
            && let Some(p) = pts.get_mut(s)
        {
            *p = d;
        }
    }
    let shown: Vec<bool> = pts.iter().map(|&p| p >= 0).collect();
    let key: Vec<bool> = t.samples.iter().map(|s| s.key).collect();
    let table = Table::new(pts, key, (u64::from(fps.1), u64::from(fps.0)), |i| shown.get(i).copied().unwrap_or(false));
    let mut colr = None;
    if p.full_range() {
        colr = Some(Codes { primaries: 2, transfer: 2, matrix: 2, full_range: Some(true) });
    }
    let duration = vcount as f64 * f64::from(fps.1) / f64::from(fps.0.max(1));
    let (codec, unsupported) = split(codec, table.order.len());
    let info = info(duration, fps, (w, h), 0, name, has_audio, None, None, false, codec.is_some());
    Ok(Clip { src, container: Container::Mxf { file: Box::new(file), track }, info, colr, codec, unsupported, table })
}

#[cfg(test)]
mod tests {
    #[test]
    fn pixel_aspect_and_display_size() {
        use super::*;
        assert_eq!(reduce_par(4, 3), Some((4, 3)));
        assert_eq!(reduce_par(16, 12), Some((4, 3)));
        assert_eq!(reduce_par(1, 1), None);
        assert_eq!(reduce_par(0, 3), None);
        assert_eq!(reduce_par(100, 1), None);
        assert_eq!(display_size(1440, 1080, (4, 3)), (1920, 1080));
        assert_eq!(display_size(720, 480, (10, 11)), (720, 528));
        assert_eq!(display_size(1280, 1080, (3, 2)), (1920, 1080));
        assert_eq!(display_size(1920, 1080, (1, 1)), (1920, 1080));
        assert_eq!(display_size(u32::MAX, 1, (8, 1)), (u32::MAX, 1));
        assert_eq!(display_size(0, 0, (0, 0)), (0, 0));
    }

    use super::*;

    #[test]
    fn matroska_dates() {
        assert_eq!(mkv_date(0).as_deref(), Some("2001-01-01T00:00:00Z"));
        // 2024-02-29T12:34:56Z
        let secs = 1_709_210_096i64 - 978_307_200;
        assert_eq!(mkv_date(secs * 1_000_000_000).as_deref(), Some("2024-02-29T12:34:56Z"));
        assert!(mkv_date(i64::MAX).is_some_and(|d| d.starts_with("2293-")));
        assert!(mkv_date(i64::MIN).is_some_and(|d| d.starts_with("1708-")));
    }

    #[test]
    fn table_lookup_and_open_gops() {
        // decode order: I0(pts 2) B(0) B(1) P(5) B(3) B(4) | I(8) B(6) B(7)
        let pts = vec![2, 0, 1, 5, 3, 4, 8, 6, 7];
        let key = vec![true, false, false, false, false, false, true, false, false];
        let t = Table::new(pts, key, (1, 30), |_| true);
        assert_eq!(t.sample_at(0), Some(1));
        assert_eq!(t.sample_at(-10), Some(1));
        assert_eq!(t.sample_at(5), Some(3));
        assert_eq!(t.sample_at(99), Some(6));
        // the B pictures after the second I are shown before it: start from the first GOP
        assert_eq!(t.sync_before(7), 0);
        assert_eq!(t.sync_before(6), 6);
        assert_eq!(t.sync_before(4), 0);
        assert_eq!(t.units(1.0), 30);
        assert_eq!(t.units(f64::NAN), 0);
        assert!((t.seconds(15) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn rates_reduce() {
        assert_eq!(rate(30000, 1001), (30000, 1001));
        assert_eq!(rate(90000, 3003), (30000, 1001));
        assert_eq!(rate(0, 5), (30, 1));
        assert_eq!(rate(1_000_000_000, 41_708_333), (1_000_000_000, 41_708_333));
        assert_eq!(rate(u64::MAX, 3), (u32::MAX, 1000));
        assert_eq!(positions(&[8, 0, 4, 12]), vec![0, -2, -3, 1]);
    }
}
