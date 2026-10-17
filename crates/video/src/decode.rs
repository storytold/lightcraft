//! One decoder per codec, all producing [`VideoFrame`]s with the stream's colour description.
//!
//! The per-codec conversions follow `filmcraft-codecs` (MIT OR Apache-2.0), which can't be used
//! as a dependency (it pulls symphonia, MPL-2.0); here every plane access is bounds-checked.

use std::sync::Arc;

use filmcraft_color::{ColorInfo, Matrix, Primaries, Range, Transfer};
use filmcraft_frame::{Chroma, PixelData, VideoFrame};

use crate::VideoError;
use crate::colour::{self, Codes};

/// What a decoder needs to start, from the container's codec configuration.
#[derive(Clone, Debug)]
pub enum Codec {
    /// `avcC` record, or None for Annex B byte streams with in-band parameter sets (TS, MXF).
    H264 {
        avcc: Option<Vec<u8>>,
    },
    Hevc {
        hvcc: Option<Vec<u8>>,
    },
    /// VP9 carries neither transfer nor primaries: they come from the container.
    Vp9,
    /// Configuration OBUs (sequence header) from `av1C`, fed before the first sample.
    Av1 {
        obus: Vec<u8>,
    },
    ProRes,
    Apv,
    Dnx,
    /// Sequence header (with extensions) to prime the decoder after a seek.
    Mpeg2 {
        header: Vec<u8>,
    },
}

/// A decoded picture with its pts (the value fed with its sample).
pub struct Pic {
    /// Interlaced picture (both fields woven into one frame): Some(top field first).
    pub field: Option<bool>,
    pub pts: i64,
    pub frame: VideoFrame,
}

pub enum Decoder {
    H264(Box<filmcraft_h264::Decoder>),
    Hevc(Box<filmcraft_hevc::Decoder>),
    Vp9(Box<filmcraft_vp9::Decoder>),
    Av1 { dec: Box<filmcraft_av1::Decoder>, obus: Vec<u8>, primed: bool },
    ProRes,
    Apv,
    Dnx,
    Mpeg2(Box<filmcraft_mpeg2v::Decoder>),
}

/// An H.264 "unsupported" error; interlaced coding gets a message of its own (AVCHD 1080i).
pub fn h264_unsupported(what: &str) -> VideoError {
    if what.contains("interlaced") || what.contains("field") {
        VideoError::Unsupported(format!("interlaced H.264 ({what}) is not decoded yet"))
    } else {
        VideoError::Unsupported(format!("H.264: {what}"))
    }
}

fn h264_error(e: filmcraft_h264::Error) -> VideoError {
    match e {
        filmcraft_h264::Error::Unsupported(w) => h264_unsupported(&w),
        e => VideoError::Decode(format!("H.264: {e}")),
    }
}

fn hevc_error(e: filmcraft_hevc::Error) -> VideoError {
    match e {
        filmcraft_hevc::Error::Unsupported(w) => VideoError::Unsupported(format!("HEVC: {w}")),
        e => VideoError::Decode(format!("HEVC: {e}")),
    }
}

fn decode_error(codec: &str, e: impl std::fmt::Display) -> VideoError {
    VideoError::Decode(format!("{codec}: {e}"))
}

/// Why an `avcC` stream can't be decoded (interlaced, 4:2:2, 10-bit…), from its first SPS.
pub fn h264_avcc_unsupported(avcc: &filmcraft_isobmff::AvcConfig) -> Option<VideoError> {
    let sps = avcc.sps.first()?;
    h264_sps_unsupported(sps)
}

/// Why an H.264 stream with this SPS NAL unit (header byte included) can't be decoded.
pub fn h264_sps_unsupported(nal: &[u8]) -> Option<VideoError> {
    let body = nal.get(1..)?;
    let sps = filmcraft_h264::params::Sps::parse(&filmcraft_bitstream::unescape_rbsp(body)).ok()?;
    match sps.check_supported() {
        Err(filmcraft_h264::Error::Unsupported(w)) => Some(h264_unsupported(&w)),
        _ => None,
    }
}

/// The first SPS of an Annex B access unit, with the (cropped) frame size it codes.
pub fn h264_annexb_sps(sample: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let nal = filmcraft_bitstream::annexb_nals(sample).into_iter().find(|n| n.first().is_some_and(|h| h & 0x1f == 7))?;
    let sps = filmcraft_h264::params::Sps::parse(&filmcraft_bitstream::unescape_rbsp(nal.get(1..)?)).ok()?;
    let (w, h) = match sps.frame_crop {
        Some((l, r, t, b)) => {
            let (cx, cy) = sps.crop_unit();
            let crop = |a: u32, b: u32, unit: u32| a.saturating_add(b).saturating_mul(unit);
            (sps.width().saturating_sub(crop(l, r, cx)), sps.height().saturating_sub(crop(t, b, cy)))
        }
        None => (sps.width(), sps.height()),
    };
    Some((nal.to_vec(), w, h))
}

impl Decoder {
    pub fn new(codec: &Codec) -> Result<Self, VideoError> {
        Ok(match codec {
            Codec::H264 { avcc: Some(a) } => Decoder::H264(Box::new(filmcraft_h264::Decoder::from_avcc(a).map_err(h264_error)?)),
            Codec::H264 { avcc: None } => Decoder::H264(Box::new(filmcraft_h264::Decoder::new())),
            Codec::Hevc { hvcc: Some(c) } => Decoder::Hevc(Box::new(filmcraft_hevc::Decoder::from_hvcc(c).map_err(hevc_error)?)),
            Codec::Hevc { hvcc: None } => Decoder::Hevc(Box::new(filmcraft_hevc::Decoder::new())),
            Codec::Vp9 => Decoder::Vp9(Box::new(filmcraft_vp9::Decoder::new())),
            Codec::Av1 { obus } => Decoder::Av1 { dec: Box::new(filmcraft_av1::Decoder::new()), obus: obus.clone(), primed: false },
            Codec::ProRes => Decoder::ProRes,
            Codec::Apv => Decoder::Apv,
            Codec::Dnx => Decoder::Dnx,
            Codec::Mpeg2 { header } => {
                let mut d = filmcraft_mpeg2v::Decoder::new();
                if !header.is_empty() {
                    // a header that doesn't parse leaves the decoder waiting for the stream's own
                    let _ = d.decode(header, 0);
                }
                Decoder::Mpeg2(Box::new(d))
            }
        })
    }

    /// Feed one sample (decode order); pictures come out in presentation order.
    pub fn decode(&mut self, data: &[u8], pts: i64, colr: Option<Codes>) -> Result<Vec<Pic>, VideoError> {
        let pics = match self {
            Decoder::H264(d) => d.decode(data, pts).map_err(h264_error)?.into_iter().map(h264_pic).collect(),
            Decoder::Hevc(d) => d.decode(data, pts).map_err(hevc_error)?.into_iter().map(hevc_pic).collect(),
            Decoder::Vp9(d) => d.decode(data, pts).map_err(|e| decode_error("VP9", e))?.into_iter().filter_map(vp9_pic).collect(),
            Decoder::Av1 { dec, obus, primed } => {
                if !*primed {
                    *primed = true;
                    if !obus.is_empty() {
                        dec.decode(obus).map_err(|e| decode_error("AV1", e))?;
                    }
                }
                dec.decode_pts(data, pts).map_err(|e| decode_error("AV1", e))?.into_iter().map(av1_pic).collect()
            }
            Decoder::ProRes => {
                vec![Pic { field: None, pts, frame: prores_frame(filmcraft_prores::decode_frame(data).map_err(|e| decode_error("ProRes", e))?) }]
            }
            Decoder::Apv => vec![Pic { field: None, pts, frame: apv_frame(filmcraft_apv::decode_frame(data).map_err(|e| decode_error("APV", e))?) }],
            Decoder::Dnx => {
                vec![Pic { field: None, pts, frame: dnx_frame(filmcraft_dnx::decode_frame(data).map_err(|e| decode_error("DNxHD", e))?) }]
            }
            Decoder::Mpeg2(d) => d.decode(data, pts).map_err(|e| decode_error("MPEG-2", e))?.into_iter().map(mpeg2_pic).collect(),
        };
        Ok(with_container_colour(pics, colr))
    }

    /// Drain pictures held for reordering.
    pub fn flush(&mut self, colr: Option<Codes>) -> Vec<Pic> {
        let pics = match self {
            Decoder::H264(d) => d.flush().into_iter().map(h264_pic).collect(),
            Decoder::Hevc(d) => d.flush().into_iter().map(hevc_pic).collect(),
            Decoder::Vp9(d) => d.flush().into_iter().filter_map(vp9_pic).collect(),
            Decoder::Av1 { dec, .. } => dec.flush().into_iter().map(av1_pic).collect(),
            Decoder::Mpeg2(d) => d.flush().into_iter().map(mpeg2_pic).collect(),
            Decoder::ProRes | Decoder::Apv | Decoder::Dnx => Vec::new(),
        };
        with_container_colour(pics, colr)
    }
}

/// The container's colour description (`colr`, Matroska `Colour`, `vpcC`) overrides the stream's.
fn with_container_colour(mut pics: Vec<Pic>, colr: Option<Codes>) -> Vec<Pic> {
    if let Some(c) = colr {
        for p in &mut pics {
            colour::apply(&mut p.frame.color, c);
        }
    }
    pics
}

pub fn tight<T: Copy>(plane: &[T], stride: usize, w: usize, h: usize) -> Vec<T> {
    if stride == w && plane.len() >= w.saturating_mul(h) {
        return plane.get(..w * h).map(<[T]>::to_vec).unwrap_or_default();
    }
    let mut out = Vec::with_capacity(w.saturating_mul(h));
    for y in 0..h {
        let start = y.saturating_mul(stride);
        match plane.get(start..start.saturating_add(w)) {
            Some(row) => out.extend_from_slice(row),
            None => break,
        }
    }
    out
}

fn frame(width: u32, height: u32, data: PixelData, color: ColorInfo) -> VideoFrame {
    VideoFrame { width, height, data, color, par: (1, 1), pts: filmcraft_time::Tick::ZERO }
}

fn vui(primaries: u8, transfer: u8, matrix: u8, full_range: bool) -> Codes {
    Codes { primaries: primaries.into(), transfer: transfer.into(), matrix: matrix.into(), full_range: Some(full_range) }
}

/// Size-based defaults overridden by in-stream H.273 code points.
fn coded_colour(width: u32, height: u32, codes: Codes) -> ColorInfo {
    colour::resolve(width, height, Some(codes), None)
}

fn to8(p: Vec<u16>) -> Arc<Vec<u8>> {
    Arc::new(p.into_iter().map(|v| v.min(255) as u8).collect())
}

fn h264_pic(p: filmcraft_h264::Picture) -> Pic {
    let (w, h, cw, ch) = (p.width as usize, p.height as usize, p.chroma_width as usize, p.chroma_height as usize);
    let c = p.color;
    let color = coded_colour(p.width, p.height, vui(c.primaries, c.transfer, c.matrix, c.full_range));
    let planes = [tight(&p.y, p.y_stride, w, h), tight(&p.u, p.uv_stride, cw, ch), tight(&p.v, p.uv_stride, cw, ch)].map(Arc::new);
    Pic { field: None, pts: p.pts, frame: frame(p.width, p.height, PixelData::Yuv8 { planes, chroma: Chroma::C420, alpha: None }, color) }
}

fn hevc_pic(p: filmcraft_hevc::Picture) -> Pic {
    use filmcraft_hevc::Plane;
    let (w, h, cw, ch) = (p.width as usize, p.height as usize, p.chroma_width as usize, p.chroma_height as usize);
    let c = p.color;
    let color = coded_colour(p.width, p.height, vui(c.primaries, c.transfer, c.matrix, c.full_range));
    let (ys, uvs) = (p.y_stride, p.uv_stride);
    let wide = |pl: &Plane| -> Vec<u16> {
        match pl {
            Plane::U16(v) => v.clone(),
            Plane::U8(v) => v.iter().map(|&b| u16::from(b)).collect(),
        }
    };
    let data = match (&p.y, &p.u, &p.v) {
        (Plane::U8(y), Plane::U8(u), Plane::U8(v)) => PixelData::Yuv8 {
            planes: [tight(y, ys, w, h), tight(u, uvs, cw, ch), tight(v, uvs, cw, ch)].map(Arc::new),
            chroma: Chroma::C420,
            alpha: None,
        },
        (y, u, v) => PixelData::Yuv16 {
            planes: [tight(&wide(y), ys, w, h), tight(&wide(u), uvs, cw, ch), tight(&wide(v), uvs, cw, ch)].map(Arc::new),
            chroma: Chroma::C420,
            bits: p.bit_depth,
            alpha: None,
        },
    };
    Pic { field: None, pts: p.pts, frame: frame(p.width, p.height, data, color) }
}

fn vp9_pic(p: filmcraft_vp9::Picture) -> Option<Pic> {
    use filmcraft_vp9::Plane;
    let (w, h) = (p.width as usize, p.height as usize);
    let (cw, ch) = (p.chroma_width as usize, p.chroma_height as usize);
    // color_space (VP9 §7.2.2): 1 BT.601, 2 BT.709, 3 SMPTE-170, 4 SMPTE-240, 5 BT.2020, 7 sRGB
    let mut color = ColorInfo { matrix: filmcraft_frame::default_matrix(p.width, p.height), ..ColorInfo::REC709 };
    match p.color.color_space {
        1 | 3 => color.matrix = Matrix::Bt601,
        2 | 4 => color.matrix = Matrix::Bt709,
        5 => {
            color.matrix = Matrix::Bt2020Ncl;
            color.primaries = Primaries::Bt2020;
        }
        _ => {}
    }
    if p.color.full_range {
        color.range = Range::Full;
    }
    if p.color.color_space == 7 {
        // RGB (profiles 1 / 3, 4:4:4): the planes carry G, B, R
        let shift = p.bit_depth.saturating_sub(8);
        let mut rgba = Vec::with_capacity(w.saturating_mul(h).saturating_mul(4));
        for y in 0..h {
            for x in 0..w {
                let i = y.saturating_mul(p.y_stride).saturating_add(x);
                let j = y.saturating_mul(p.uv_stride).saturating_add(x);
                let s = |v: u16| (v >> shift).min(255) as u8;
                rgba.extend_from_slice(&[s(p.v.get(j)), s(p.y.get(i)), s(p.u.get(j)), 255]);
            }
        }
        let mut f = VideoFrame::rgba8(p.width, p.height, rgba);
        f.color = ColorInfo { range: Range::Full, transfer: Transfer::Srgb, ..color };
        return Some(Pic { field: None, pts: p.pts, frame: f });
    }
    // 4:4:0 has no frame format of its own: chroma rows are repeated to 4:4:4
    let (chroma, rows_440) = match (p.subsampling_x, p.subsampling_y) {
        (true, true) => (Chroma::C420, false),
        (true, false) => (Chroma::C422, false),
        (false, false) => (Chroma::C444, false),
        (false, true) => (Chroma::C444, true),
    };
    let uvs = p.uv_stride;
    fn expand<T: Copy>(v: &[T], stride: usize, cw: usize, ch: usize, h: usize, rows_440: bool) -> Vec<T> {
        let t = tight(v, stride, cw, ch);
        if !rows_440 {
            return t;
        }
        let mut out = Vec::with_capacity(cw.saturating_mul(h));
        for y in 0..h {
            let s = (y >> 1).saturating_mul(cw);
            match t.get(s..s.saturating_add(cw)) {
                Some(row) => out.extend_from_slice(row),
                None => break,
            }
        }
        out
    }
    let data = match (&p.y, &p.u, &p.v) {
        (Plane::U8(y), Plane::U8(u), Plane::U8(v)) => PixelData::Yuv8 {
            planes: [tight(y, p.y_stride, w, h), expand(u, uvs, cw, ch, h, rows_440), expand(v, uvs, cw, ch, h, rows_440)].map(Arc::new),
            chroma,
            alpha: None,
        },
        (Plane::U16(y), Plane::U16(u), Plane::U16(v)) => PixelData::Yuv16 {
            planes: [tight(y, p.y_stride, w, h), expand(u, uvs, cw, ch, h, rows_440), expand(v, uvs, cw, ch, h, rows_440)].map(Arc::new),
            chroma,
            bits: p.bit_depth,
            alpha: None,
        },
        _ => return None,
    };
    Some(Pic { field: None, pts: p.pts, frame: frame(p.width, p.height, data, color) })
}

fn av1_pic(p: filmcraft_av1::Picture) -> Pic {
    let (w, h) = (p.width as usize, p.height as usize);
    let mut color = ColorInfo::REC709;
    color.matrix = Matrix::from_code(p.matrix_coefficients).unwrap_or_else(|| filmcraft_frame::default_matrix(p.width, p.height));
    if let Some(t) = Transfer::from_code(p.transfer_characteristics) {
        color.transfer = t;
    }
    color.primaries = match p.color_primaries {
        9 => Primaries::Bt2020,
        12 => Primaries::P3D65,
        5 => Primaries::Bt601_625,
        6 => Primaries::Bt601_525,
        _ => Primaries::Bt709,
    };
    if p.full_range {
        color.range = Range::Full;
    }
    let (sx, sy) = (usize::from(p.subsampling_x.min(1)), usize::from(p.subsampling_y.min(1)));
    let chroma = match (sx, sy) {
        (1, 1) => Chroma::C420,
        (1, 0) => Chroma::C422,
        _ => Chroma::C444,
    };
    let cw = (w + sx) >> sx;
    let ch = (h + sy) >> sy;
    let [y, mut u, mut v] = p.planes;
    if p.mono_chrome {
        u = vec![1u16 << p.bit_depth.clamp(1, 16).saturating_sub(1); cw.saturating_mul(ch)];
        v = u.clone();
    }
    let data = if p.bit_depth <= 8 {
        PixelData::Yuv8 { planes: [to8(y), to8(u), to8(v)], chroma, alpha: None }
    } else {
        PixelData::Yuv16 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma, bits: u32::from(p.bit_depth), alpha: None }
    };
    Pic { field: None, pts: p.pts, frame: frame(p.width, p.height, data, color) }
}

fn prores_frame(f: filmcraft_prores::Frame) -> VideoFrame {
    let chroma = match f.chroma {
        filmcraft_prores::ChromaFormat::Yuv422 => Chroma::C422,
        filmcraft_prores::ChromaFormat::Yuv444 => Chroma::C444,
    };
    let mut color = ColorInfo::REC709;
    if let Some(m) = Matrix::from_code(f.color.matrix) {
        color.matrix = m;
    }
    if let Some(t) = Transfer::from_code(f.color.transfer) {
        color.transfer = t;
    }
    let data = PixelData::Yuv16 { planes: [Arc::new(f.y), Arc::new(f.cb), Arc::new(f.cr)], chroma, bits: u32::from(f.bit_depth), alpha: None };
    frame(f.width, f.height, data, color)
}

fn apv_frame(f: filmcraft_apv::Frame) -> VideoFrame {
    use filmcraft_apv::ChromaFormat as C;
    let chroma = match f.chroma {
        C::Yuv422 => Chroma::C422,
        C::Monochrome | C::Yuv444 | C::Yuv4444 => Chroma::C444,
    };
    let c = f.color;
    let color = coded_colour(f.width, f.height, vui(c.primaries, c.transfer, c.matrix, c.full_range));
    let neutral = 1u16 << u32::from(f.bit_depth).clamp(1, 16).saturating_sub(1);
    let (cb, cr) = if f.chroma == C::Monochrome {
        let u = vec![neutral; (f.width as usize).saturating_mul(f.height as usize)];
        (u.clone(), u)
    } else {
        (f.cb, f.cr)
    };
    let data = if f.bit_depth <= 8 {
        PixelData::Yuv8 { planes: [to8(f.y), to8(cb), to8(cr)], chroma, alpha: None }
    } else {
        PixelData::Yuv16 { planes: [Arc::new(f.y), Arc::new(cb), Arc::new(cr)], chroma, bits: u32::from(f.bit_depth), alpha: None }
    };
    frame(f.width, f.height, data, color)
}

fn dnx_frame(f: filmcraft_dnx::Frame) -> VideoFrame {
    let chroma = match f.chroma {
        filmcraft_dnx::ChromaFormat::Yuv420 => Chroma::C420,
        filmcraft_dnx::ChromaFormat::Yuv422 => Chroma::C422,
        filmcraft_dnx::ChromaFormat::Yuv444 => Chroma::C444,
    };
    let mut color = ColorInfo::REC709;
    if matches!(f.color_volume, filmcraft_dnx::ColorVolume::Bt2020Ncl | filmcraft_dnx::ColorVolume::Bt2020Cl) {
        color.matrix = Matrix::Bt2020Ncl;
        color.primaries = Primaries::Bt2020;
    }
    let bits = u32::from(f.bit_depth).clamp(8, 16);
    let (mut y, mut cb, mut cr) = (f.y, f.cb, f.cr);
    if f.rgb {
        // planes hold G, B, R (video range): derive Y'CbCr with the stream's matrix
        let (kr, kb) = color.matrix.kr_kb();
        let kg = 1.0 - kr - kb;
        let s = (1u32 << (bits - 8)) as f32;
        let max = ((1u32 << bits) - 1) as f32;
        let c = 224.0 / 219.0;
        for ((yy, u), v) in y.iter_mut().zip(cb.iter_mut()).zip(cr.iter_mut()) {
            let (g, b, r) = (f32::from(*yy), f32::from(*u), f32::from(*v));
            let l = kr * r + kg * g + kb * b;
            let pb = (b - l) / (2.0 * (1.0 - kb)) * c + 128.0 * s;
            let pr = (r - l) / (2.0 * (1.0 - kr)) * c + 128.0 * s;
            *yy = (l + 0.5).clamp(0.0, max) as u16;
            *u = (pb + 0.5).clamp(0.0, max) as u16;
            *v = (pr + 0.5).clamp(0.0, max) as u16;
        }
    }
    let data = if bits == 8 {
        PixelData::Yuv8 { planes: [to8(y), to8(cb), to8(cr)], chroma, alpha: None }
    } else {
        PixelData::Yuv16 { planes: [Arc::new(y), Arc::new(cb), Arc::new(cr)], chroma, bits, alpha: None }
    };
    frame(f.width, f.height, data, color)
}

fn mpeg2_pic(p: filmcraft_mpeg2v::Picture) -> Pic {
    let info = &p.info;
    let mut color = ColorInfo { matrix: filmcraft_frame::default_matrix(p.width, p.height), ..ColorInfo::REC709 };
    if info.width <= 1024 && info.height <= 576 {
        color.primaries = if info.height == 576 || info.height == 608 { Primaries::Bt601_625 } else { Primaries::Bt601_525 };
    }
    if let Some((prim, transfer, matrix)) = info.colour {
        colour::apply(&mut color, Codes { primaries: prim.into(), transfer: transfer.into(), matrix: matrix.into(), full_range: None });
    }
    let chroma = match p.chroma {
        filmcraft_mpeg2v::ChromaFormat::Yuv420 => Chroma::C420,
        filmcraft_mpeg2v::ChromaFormat::Yuv422 => Chroma::C422,
        filmcraft_mpeg2v::ChromaFormat::Yuv444 => Chroma::C444,
    };
    let field = (!p.progressive_frame).then_some(p.top_field_first);
    let data = PixelData::Yuv8 { planes: [Arc::new(p.y), Arc::new(p.cb), Arc::new(p.cr)], chroma, alpha: None };
    Pic { field, pts: p.pts, frame: frame(p.width, p.height, data, color) }
}

/// Linear (bob) deinterlace of a woven 8-bit frame in place: the first field's rows are kept,
/// the other field's rows are rebuilt from the rows above and below. Each plane is treated alike
/// (4:2:0 interlaced chroma alternates fields row by row too). Other pixel formats are left as is.
pub fn deinterlace(f: &mut VideoFrame, top_first: bool) {
    let (w, h) = (f.width as usize, f.height as usize);
    let PixelData::Yuv8 { planes, chroma, .. } = &mut f.data else { return };
    let (cw, ch) = match chroma {
        Chroma::C420 => (w.div_ceil(2), h.div_ceil(2)),
        Chroma::C422 => (w.div_ceil(2), h),
        _ => (w, h),
    };
    for (k, plane) in planes.iter_mut().enumerate() {
        let (pw, ph) = if k == 0 { (w, h) } else { (cw, ch) };
        if pw == 0 || ph < 2 || plane.len() < pw.saturating_mul(ph) {
            continue;
        }
        let p = Arc::make_mut(plane);
        let skip = usize::from(top_first);
        for y in (skip..ph).step_by(2) {
            let above = y.checked_sub(1);
            let below = (y + 1 < ph).then_some(y + 1);
            let (a, b) = match (above, below) {
                (Some(a), Some(b)) => (a, b),
                (Some(a), None) => (a, a),
                (None, Some(b)) => (b, b),
                (None, None) => continue,
            };
            for x in 0..pw {
                let (Some(&va), Some(&vb)) = (p.get(a * pw + x), p.get(b * pw + x)) else { continue };
                if let Some(o) = p.get_mut(y * pw + x) {
                    *o = (u16::from(va) + u16::from(vb)).div_ceil(2) as u8;
                }
            }
        }
    }
}

/// Whether a frame's planes hold as many samples as its size and chroma format say (the
/// converter indexes them unchecked).
pub fn complete(f: &VideoFrame) -> bool {
    let (w, h) = (f.width as usize, f.height as usize);
    let sizes = |chroma: &Chroma| -> (usize, usize) {
        match chroma {
            Chroma::C420 => (w.div_ceil(2), h.div_ceil(2)),
            Chroma::C422 => (w.div_ceil(2), h),
            Chroma::C444 => (w, h),
        }
    };
    let n = w.saturating_mul(h);
    match &f.data {
        PixelData::Yuv8 { planes, chroma, alpha } => {
            let (cw, ch) = sizes(chroma);
            let c = cw.saturating_mul(ch);
            planes[0].len() >= n && planes[1].len() >= c && planes[2].len() >= c && alpha.as_ref().is_none_or(|a| a.len() >= n)
        }
        PixelData::Yuv16 { planes, chroma, alpha, .. } => {
            let (cw, ch) = sizes(chroma);
            let c = cw.saturating_mul(ch);
            planes[0].len() >= n && planes[1].len() >= c && planes[2].len() >= c && alpha.as_ref().is_none_or(|a| a.len() >= n)
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deinterlace_rebuilds_the_second_field() {
        // 2×4 luma woven from fields 10 (top) and 200 (bottom); chroma 1×2
        let y = vec![10, 10, 200, 200, 10, 10, 200, 200];
        let data = PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(vec![128, 60]), Arc::new(vec![128, 60])], chroma: Chroma::C420, alpha: None };
        let mut f = frame(2, 4, data.clone(), ColorInfo::REC709);
        deinterlace(&mut f, true);
        let PixelData::Yuv8 { planes, .. } = &f.data else { panic!("format") };
        assert_eq!(*planes[0], vec![10, 10, 10, 10, 10, 10, 10, 10]);
        assert_eq!(*planes[1], vec![128, 128]);
        let mut f = frame(2, 4, data, ColorInfo::REC709);
        deinterlace(&mut f, false);
        let PixelData::Yuv8 { planes, .. } = &f.data else { panic!("format") };
        assert_eq!(*planes[0], vec![200, 200, 200, 200, 200, 200, 200, 200]);
        // short planes are left alone
        let mut f = frame(
            4,
            4,
            PixelData::Yuv8 { planes: [Arc::new(vec![1]), Arc::new(vec![]), Arc::new(vec![])], chroma: Chroma::C420, alpha: None },
            ColorInfo::REC709,
        );
        deinterlace(&mut f, true);
    }
}
