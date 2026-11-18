//! Shared tail of every decoder: interpret device samples (with ICC / container hints), linearize,
//! split alpha, and optionally downscale in linear light.

use crate::icc::{self, IccColorModel, IccKind};
use crate::space::{NamedSpace, SourceSpace, SpaceOrigin, Trc};
use crate::{DecodeOptions, Decoded, Error, Format, Result, exif};
use lcc_raster::resample::{Filter, fit};
use lcc_raster::{Plane, Rgb32f, par_rows};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Model {
    Gray,
    Rgb,
    /// Subtractive CMYK, 0 = no ink.
    Cmyk,
}

impl Model {
    pub fn channels(self) -> usize {
        match self {
            Model::Gray => 1,
            Model::Rgb => 3,
            Model::Cmyk => 4,
        }
    }
}

pub(crate) enum Buf {
    U8(Vec<u8>),
    U16(Vec<u16>),
    F32(Vec<f32>),
}

impl Buf {
    pub(crate) fn len(&self) -> usize {
        match self {
            Buf::U8(v) => v.len(),
            Buf::U16(v) => v.len(),
            Buf::F32(v) => v.len(),
        }
    }
    #[inline]
    pub(crate) fn norm(&self, i: usize) -> f32 {
        match self {
            Buf::U8(v) => v[i] as f32 / 255.0,
            Buf::U16(v) => v[i] as f32 / 65535.0,
            Buf::F32(v) => normalize_float(v[i]),
        }
    }
}

/// Divide associated alpha out of the colour samples of interleaved pixels of `ch` channels, the
/// last of which is alpha (integer colour is capped at full scale; zero alpha leaves the colour).
fn unpremultiply(buf: &mut Buf, ch: usize) {
    fn each<T: Copy>(v: &mut [T], ch: usize, f: impl Fn(T, T) -> T) {
        if ch < 2 {
            return;
        }
        for px in v.chunks_exact_mut(ch) {
            if let Some((&mut a, colour)) = px.split_last_mut() {
                for c in colour {
                    *c = f(*c, a);
                }
            }
        }
    }
    match buf {
        Buf::U8(v) => each(v, ch, |c, a| if a == 0 { c } else { ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8 }),
        Buf::U16(v) => each(v, ch, |c, a| if a == 0 { c } else { ((c as u64 * 65535 + a as u64 / 2) / a as u64).min(65535) as u16 }),
        // (non-finite samples clipped first, as the colour conversion and the alpha plane clip them)
        Buf::F32(v) => each(v, ch, |c, a| {
            let (c, a) = (normalize_float(c), normalize_float(a).clamp(0.0, 1.0));
            if a > 0.0 { c / a } else { c }
        }),
    }
}

/// Clip non-finite device samples before colour conversion, preserving finite extended range.
#[inline]
fn normalize_float(value: f32) -> f32 {
    if value == f32::INFINITY {
        1.0
    } else if value.is_finite() {
        value
    } else {
        0.0
    }
}

/// Interleaved device samples as produced by a format decoder.
pub(crate) struct Raw {
    pub width: usize,
    pub height: usize,
    pub model: Model,
    /// One extra (last) channel of alpha.
    pub alpha: bool,
    /// Alpha is premultiplied into the colour channels.
    pub premultiplied: bool,
    pub buf: Buf,
    pub bit_depth: u8,
}

/// Metadata and colour hints gathered from the container.
#[derive(Default)]
pub(crate) struct Meta {
    pub icc: Option<Vec<u8>>,
    /// An ICC profile was declared by the container but its sequence was invalid.
    pub icc_invalid: bool,
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<String>,
    /// Orientation from the container itself (e.g. TIFF tag); EXIF blob is consulted otherwise.
    pub orientation: Option<u16>,
    /// Container-level colour description (PNG cHRM/gAMA/sRGB/cICP, JXL enum encoding, …).
    pub hint: Option<SourceSpace>,
    /// `image` already went through a CMS/transform producing this space (skip interpretation).
    pub already_linear: Option<SourceSpace>,
    /// A film scanner's infrared pass (see [`crate::Decoded::infrared`]), the image's size, 0..1.
    pub infrared: Option<Plane>,
}

/// [`check_size`], and the decoder's own sample buffer of `bytes_per_pixel` a pixel against
/// [`DecodeOptions::max_bytes`].
pub(crate) fn check_budget(format: Format, w: u64, h: u64, bytes_per_pixel: u64, opts: &DecodeOptions) -> Result<()> {
    check_size(format, w, h, opts)?;
    if w.saturating_mul(h).saturating_mul(bytes_per_pixel) > opts.max_bytes {
        return Err(Error::TooLarge(w, h));
    }
    Ok(())
}

/// `n` copies of `v`, or [`Error::TooLarge`] when the allocation fails: the full-size buffers of a
/// decode are sized by the file's header, so failing to get one is an error, not an abort.
pub(crate) fn try_filled<T: Clone>(n: usize, v: T, dims: (usize, usize)) -> Result<Vec<T>> {
    let mut out = Vec::new();
    out.try_reserve_exact(n).map_err(|_| Error::TooLarge(dims.0 as u64, dims.1 as u64))?;
    out.resize(n, v);
    Ok(out)
}

/// A zeroed full-size image, allocated fallibly ([`try_filled`]).
pub(crate) fn try_image<T: Copy + Default>(w: usize, h: usize) -> Result<lcc_raster::Image<T>> {
    let n = w.checked_mul(h).ok_or(Error::TooLarge(w as u64, h as u64))?;
    Ok(lcc_raster::Image { width: w, height: h, data: try_filled(n, T::default(), (w, h))? })
}

pub(crate) fn check_size(format: Format, w: u64, h: u64, opts: &DecodeOptions) -> Result<()> {
    if w == 0 || h == 0 {
        return Err(Error::Malformed(format, "zero dimension".into()));
    }
    if w.saturating_mul(h) > opts.max_pixels {
        return Err(Error::TooLarge(w, h));
    }
    Ok(())
}

pub(crate) fn finish(format: Format, mut raw: Raw, meta: Meta, source_dims: (u32, u32), opts: &DecodeOptions) -> Result<Decoded> {
    let ch = raw.model.channels() + raw.alpha as usize;
    let n = crate::checked_samples(raw.width, raw.height, 1)?;
    if n == 0 || raw.buf.len() < crate::checked_samples(raw.width, raw.height, ch)? {
        return Err(Error::Malformed(format, "sample buffer too short".into()));
    }
    // Associated alpha is multiplied into the stored (encoded) colour values, as image editors and
    // our TIFF export write it: divide it out before those values are decoded to linear light.
    if raw.premultiplied && raw.alpha {
        unpremultiply(&mut raw.buf, ch);
    }
    let float = matches!(raw.buf, Buf::F32(_));

    // Alpha plane (straight).
    let alpha = if raw.alpha {
        let mut a: Plane = try_image(raw.width, raw.height)?;
        for (i, o) in a.data.iter_mut().enumerate() {
            *o = raw.buf.norm(i * ch + ch - 1).clamp(0.0, 1.0);
        }
        Some(a)
    } else {
        None
    };

    let parsed_icc = meta.icc.as_deref().and_then(icc::parse);
    let (image, space) = if let Some(space) = meta.already_linear.clone() {
        (to_rgb_passthrough(&raw, ch)?, space)
    } else if raw.model == Model::Cmyk {
        cmyk(&raw, ch, meta.icc.as_deref(), parsed_icc.as_ref(), meta.icc_invalid)?
    } else {
        rgb_or_gray(&raw, ch, &meta, parsed_icc.as_ref(), float)?
    };

    let summary = meta.exif.as_deref().map(exif::summarize).unwrap_or_default();
    let orientation = meta.orientation.or(summary.orientation).filter(|o| (1..=8).contains(o)).unwrap_or(1);

    let mut infrared = meta.infrared.filter(|p| p.width == image.width && p.height == image.height);
    let (image, alpha) = match opts.max_size {
        Some((mw, mh)) if (image.width as u32 > mw || image.height as u32 > mh) && mw > 0 && mh > 0 => {
            let factor = (image.width as f64 / mw as f64).max(image.height as f64 / mh as f64);
            // (Lanczos at 2:1 and more as well: a box average there halves the contrast of
            // detail near the output's resolution limit and aliases what is finer)
            let f = if factor >= 2.0 { Filter::Lanczos3 } else { Filter::Mitchell };
            match alpha {
                // with transparency, the colour is fitted weighted by its alpha (`c·a` and `a`, then
                // divided): straight colour fitted on its own pulls the colour under transparent
                // pixels (often black) into every edge
                Some(a) if a.data.iter().any(|&v| v < 1.0) => {
                    let mut weighted = image;
                    for (p, &av) in weighted.data.iter_mut().zip(&a.data) {
                        *p = p.map(|c| c * av);
                    }
                    let mut img = fit(&weighted, mw as usize, mh as usize, f);
                    let mut a = fit(&a, mw as usize, mh as usize, f);
                    for (p, av) in img.data.iter_mut().zip(a.data.iter_mut()) {
                        // (no colour to recover where there's next to no alpha: it stays the weighted value, ~0)
                        // (clamped at 0: a negative lobe next to an edge, divided by a small alpha, undershoots)
                        if av.is_finite() && *av >= 1.0 / 1024.0 {
                            *p = p.map(|c| (c / *av).max(0.0));
                        }
                        // the filter rings a little past 0 and 1: the plane is straight alpha 0..1, as
                        // `to_working` flattens with it and an export carries it
                        *av = if av.is_finite() { av.clamp(0.0, 1.0) } else { 0.0 };
                    }
                    (img, Some(a))
                }
                alpha => (fit(&image, mw as usize, mh as usize, f), alpha.map(|a| fit(&a, mw as usize, mh as usize, f))),
            }
        }
        _ => (image, alpha),
    };
    // the infrared pass is fitted with the pixels (a dust speck stays where its colour is)
    if let Some(p) = infrared.take() {
        let mut p = if (p.width, p.height) != (image.width, image.height) { fit(&p, image.width, image.height, Filter::Mitchell) } else { p };
        for v in &mut p.data {
            *v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        }
        infrared = Some(p);
    }

    Ok(Decoded {
        format,
        width: image.width as u32,
        height: image.height as u32,
        image,
        has_alpha: alpha.is_some(),
        alpha,
        infrared,
        space,
        bit_depth: raw.bit_depth,
        float,
        grayscale: raw.model == Model::Gray,
        cmyk: raw.model == Model::Cmyk,
        orientation,
        exif: meta.exif,
        xmp: meta.xmp,
        icc: meta.icc,
        source_width: source_dims.0,
        source_height: source_dims.1,
    })
}

fn to_rgb_passthrough(raw: &Raw, ch: usize) -> Result<Rgb32f> {
    let gray = raw.model == Model::Gray;
    let mut img: Rgb32f = try_image(raw.width, raw.height)?;
    par_rows(&mut img.data, raw.width, |y, row| {
        for (x, o) in row.iter_mut().enumerate() {
            let i = (y * raw.width + x) * ch;
            *o = if gray {
                let v = raw.buf.norm(i);
                [v, v, v]
            } else {
                [raw.buf.norm(i), raw.buf.norm(i + 1), raw.buf.norm(i + 2)]
            };
        }
    });
    Ok(img)
}

/// Per-channel linearization via LUTs (8/16-bit) or direct evaluation (float).
fn linearize(raw: &Raw, ch: usize, trc: &[Trc; 3]) -> Result<Rgb32f> {
    let gray = raw.model == Model::Gray;
    let mut img: Rgb32f = try_image(raw.width, raw.height)?;
    let w = raw.width;
    match &raw.buf {
        Buf::U8(v) => {
            let luts: Vec<Vec<f32>> = trc.iter().map(|t| t.lut8()).collect();
            par_rows(&mut img.data, w, |y, row| {
                for (x, o) in row.iter_mut().enumerate() {
                    let i = (y * w + x) * ch;
                    *o = if gray {
                        let l = luts[1][v[i] as usize];
                        [l, l, l]
                    } else {
                        [luts[0][v[i] as usize], luts[1][v[i + 1] as usize], luts[2][v[i + 2] as usize]]
                    };
                }
            });
        }
        Buf::U16(v) => {
            let same = trc[0] == trc[1] && trc[1] == trc[2];
            let l0 = trc[1].lut16();
            let (l1, l2) = if same || gray { (None, None) } else { (Some(trc[0].lut16()), Some(trc[2].lut16())) };
            let lr = l1.as_ref().unwrap_or(&l0);
            let lb = l2.as_ref().unwrap_or(&l0);
            par_rows(&mut img.data, w, |y, row| {
                for (x, o) in row.iter_mut().enumerate() {
                    let i = (y * w + x) * ch;
                    *o = if gray {
                        let l = l0[v[i] as usize];
                        [l, l, l]
                    } else {
                        [lr[v[i] as usize], l0[v[i + 1] as usize], lb[v[i + 2] as usize]]
                    };
                }
            });
        }
        Buf::F32(v) => {
            let lin = trc.iter().all(|t| t.is_linear());
            par_rows(&mut img.data, w, |y, row| {
                for (x, o) in row.iter_mut().enumerate() {
                    let i = (y * w + x) * ch;
                    let s = |c: usize| normalize_float(v[i + c]);
                    *o = if gray {
                        let l = if lin { s(0) } else { trc[1].to_linear(s(0)) };
                        [l, l, l]
                    } else if lin {
                        [s(0), s(1), s(2)]
                    } else {
                        [trc[0].to_linear(s(0)), trc[1].to_linear(s(1)), trc[2].to_linear(s(2))]
                    };
                }
            });
        }
    }
    Ok(img)
}

fn device_samples(raw: &Raw, ch: usize, n_color: usize) -> Result<Vec<f32>> {
    let n = raw.width * raw.height;
    let too_large = || Error::TooLarge(raw.width as u64, raw.height as u64);
    let mut out = Vec::new();
    out.try_reserve_exact(n.checked_mul(n_color).ok_or_else(too_large)?).map_err(|_| too_large())?;
    for p in 0..n {
        for c in 0..n_color {
            out.push(raw.buf.norm(p * ch + c).clamp(0.0, 1.0));
        }
    }
    Ok(out)
}

fn from_vec(w: usize, h: usize, data: Vec<[f32; 3]>) -> Rgb32f {
    Rgb32f { width: w, height: h, data }
}

fn rgb_or_gray(raw: &Raw, ch: usize, meta: &Meta, info: Option<&icc::IccInfo>, float: bool) -> Result<(Rgb32f, SourceSpace)> {
    let gray = raw.model == Model::Gray;
    if let (Some(bytes), Some(info)) = (meta.icc.as_deref(), info) {
        match (&info.kind, gray) {
            (IccKind::MatrixTrc { to_xyz_d50, trc }, false) => {
                let space = SourceSpace { named: info.named, to_xyz_d50: *to_xyz_d50, trc: Some(trc.clone()), origin: SpaceOrigin::IccMatrixTrc };
                return Ok((linearize(raw, ch, trc)?, space));
            }
            (IccKind::MatrixTrc { to_xyz_d50, trc }, true) => {
                // Gray data with an RGB profile: neutral axis only needs the green curve.
                let t = [trc[1].clone(), trc[1].clone(), trc[1].clone()];
                let space = SourceSpace { named: info.named, to_xyz_d50: *to_xyz_d50, trc: Some(t.clone()), origin: SpaceOrigin::IccMatrixTrc };
                return Ok((linearize(raw, ch, &t)?, space));
            }
            (IccKind::Gray { trc }, true) => {
                let t = [trc.clone(), trc.clone(), trc.clone()];
                let mut space = SourceSpace::named(NamedSpace::Srgb, SpaceOrigin::IccMatrixTrc);
                space.trc = Some(t.clone());
                return Ok((linearize(raw, ch, &t)?, space));
            }
            (IccKind::NeedsCms, _) => {
                let wanted = if gray { IccColorModel::Gray } else { IccColorModel::Rgb };
                if info.model == wanted {
                    let nc = raw.model.channels();
                    let samples = device_samples(raw, ch, nc)?;
                    if let Some(px) = icc::cms_to_linear_rec2020(bytes, &samples, nc) {
                        let mut s = SourceSpace::named(NamedSpace::Rec2020, SpaceOrigin::IccCms);
                        s.trc = None;
                        return Ok((from_vec(raw.width, raw.height, px), s));
                    }
                }
            }
            _ => {}
        }
    }
    if meta.icc.is_some() || meta.icc_invalid {
        // Present but unusable (malformed, mismatched model, CMS failure): assume sRGB.
        let s = SourceSpace::named(NamedSpace::Srgb, SpaceOrigin::IccUnsupported);
        let t = NamedSpace::Srgb.trc();
        return Ok((linearize(raw, ch, &[t.clone(), t.clone(), t])?, s));
    }
    if let Some(h) = &meta.hint {
        let t = h.trc.clone().unwrap_or([Trc::Srgb, Trc::Srgb, Trc::Srgb]);
        return Ok((linearize(raw, ch, &t)?, h.clone()));
    }
    let adobe = meta.exif.as_deref().map(exif::summarize).is_some_and(|s| s.adobe_rgb_hint);
    let mut s = if adobe {
        SourceSpace::named(NamedSpace::AdobeRgb, SpaceOrigin::Container)
    } else {
        SourceSpace::named(NamedSpace::Srgb, SpaceOrigin::Untagged)
    };
    if float {
        // Untagged float data is conventionally scene-linear.
        s.trc = Some([Trc::Linear, Trc::Linear, Trc::Linear]);
    }
    let t = s.trc.clone().unwrap_or([Trc::Srgb, Trc::Srgb, Trc::Srgb]);
    Ok((linearize(raw, ch, &t)?, s))
}

fn cmyk(raw: &Raw, ch: usize, icc_bytes: Option<&[u8]>, info: Option<&icc::IccInfo>, icc_invalid: bool) -> Result<(Rgb32f, SourceSpace)> {
    let samples = device_samples(raw, ch, 4)?;
    if let (Some(bytes), Some(info)) = (icc_bytes, info)
        && info.model == IccColorModel::Cmyk
        && let Some(px) = icc::cms_to_linear_rec2020(bytes, &samples, 4)
    {
        let mut s = SourceSpace::named(NamedSpace::Rec2020, SpaceOrigin::IccCms);
        s.trc = None;
        return Ok((from_vec(raw.width, raw.height, px), s));
    }
    // Naive: RGB = (1 - C)(1 - K) … treated as sRGB-encoded.
    let mut px = try_filled(samples.len() / 4, [0.0; 3], (raw.width, raw.height))?;
    for (o, c) in px.iter_mut().zip(samples.as_chunks::<4>().0) {
        let k = 1.0 - c[3];
        *o = [(1.0 - c[0]) * k, (1.0 - c[1]) * k, (1.0 - c[2]) * k].map(lcc_color::transfer::srgb_to_linear);
    }
    let origin = if icc_bytes.is_some() || icc_invalid { SpaceOrigin::IccUnsupported } else { SpaceOrigin::Naive };
    Ok((from_vec(raw.width, raw.height, px), SourceSpace::named(NamedSpace::Srgb, origin)))
}
