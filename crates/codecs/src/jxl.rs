//! JPEG XL decode via `jxl-oxide` (pure Rust). Enum-encoded images are rendered straight into linear
//! Rec.2020; ICC-tagged ones are rendered in their own space and interpreted with our ICC path.
//! XYB (lossy) images are rendered to linear sRGB primaries and converted to Rec.2020 here:
//! jxl-oxide 0.12 desaturates colours outside sRGB when it renders XYB to wider primaries.
//! jxl-oxide applies the codestream orientation itself, so `orientation` is reported as 1.
//!
//! Alpha is the extra channel typed Alpha, wherever it sits among the extra channels. With associated
//! (premultiplied) alpha the colour is divided by alpha in the space it was coded in, before any curve or
//! matrix: non-XYB images are rendered in their own encoding (enum or ICC) for that and converted here.
//! Two cases still divide after linearizing, an approximation at partial alpha: XYB (lossy) images,
//! whose colour jxl-oxide hands back already converted, and enum encodings with a PQ, HLG or unknown
//! curve, which only jxl-oxide converts.
//!
//! CMYK JPEG XL (a Black extra channel) isn't supported: such files are refused.

use crate::convert::{Buf, Meta, Model, Raw, check_size, finish};
use crate::space::{NamedSpace, SourceSpace, SpaceOrigin, Trc, rgb_to_xyz_d50};
use crate::{DecodeOptions, Decoded, Error, Format, Result};
use jxl_oxide::color::{ColourEncoding, ColourSpace, Customxy, EnumColourEncoding, Primaries, RenderingIntent, TransferFunction, WhitePoint};
use jxl_oxide::{AllocTracker, AuxBoxData, ExtraChannelType, InitializeResult, JxlImage};

const F: Format = Format::Jxl;

fn err(e: impl std::fmt::Display) -> Error {
    Error::Malformed(F, e.to_string())
}

/// Tracked allocation budget per allowed pixel for frames, extra channels and modular trees.
/// This bounds allocations covered by `AllocTracker`, not every allocation made by the decoder (#1517).
const TRACKED_BYTES_PER_PIXEL: u64 = 4 * 4 * 8;

/// The allocation budget never drops below this, so small images keep the decoder's fixed
/// overhead (group buffers, entropy tables) whatever `max_pixels` is.
const MIN_TRACKED_BYTES: u64 = 64 << 20;

/// Bytes handed to the decoder at a time (the size `JxlImageBuilder::read` reads in).
const INPUT_CHUNK: usize = 4096;

/// Initializes from bounded input, checks dimensions before rendering, then loads remaining frames
/// within a tracked allocation budget.
fn open(bytes: &[u8], opts: &DecodeOptions) -> Result<JxlImage> {
    let budget = opts.max_pixels.saturating_mul(TRACKED_BYTES_PER_PIXEL).max(MIN_TRACKED_BYTES);
    let tracker = AllocTracker::with_limit(usize::try_from(budget).unwrap_or(usize::MAX));
    let mut uninit = JxlImage::builder().alloc_tracker(tracker).build_uninit();
    // Initialization can load already-buffered frame data, so small feeds limit work before
    // dimension checking.
    let mut rest = bytes;
    let mut window = INPUT_CHUNK;
    let mut img = loop {
        let consumed = uninit.feed_bytes(rest.get(..window).unwrap_or(rest)).map_err(err)?;
        rest = rest.get(consumed..).unwrap_or_default();
        match uninit.try_init().map_err(err)? {
            InitializeResult::Initialized(img) => break img,
            InitializeResult::NeedMoreData(more) => uninit = more,
        }
        if consumed == 0 {
            // the parser wants more than the window holds at once
            if window >= rest.len() {
                return Err(err("truncated JPEG XL header"));
            }
            window = window.saturating_mul(2);
        }
    };
    check_size(F, img.width() as u64, img.height() as u64, opts)?;
    // Match `JxlImageBuilder::read`: bounded feeds until the final frame, not merely the first
    // keyframe. `feed_bytes` copies codestream data into an untracked Vec, including any tail
    // after the last frame; feeding the remaining file at once bypasses the allocation budget.
    while !img.is_loading_done() && !rest.is_empty() {
        let consumed = img.feed_bytes(rest.get(..INPUT_CHUNK).unwrap_or(rest)).map_err(err)?;
        if consumed == 0 {
            break;
        }
        rest = rest.get(consumed..).unwrap_or_default();
    }
    img.finalize().map_err(err)?;
    Ok(img)
}

/// Dimensions from the codestream (orientation already applied by jxl-oxide, so 1, as a decode
/// reports it) without rendering a frame: the frames are parsed within the decode's allocation
/// budget, and a file without a complete first keyframe (truncated) is refused, as a decode would.
pub(crate) fn header(bytes: &[u8]) -> Result<(u32, u32, u16)> {
    let img = open(bytes, &DecodeOptions::default())?;
    if img.num_loaded_keyframes() == 0 {
        return Err(err("truncated: no complete frame"));
    }
    Ok((img.width(), img.height(), 1))
}

pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Decoded> {
    let mut img = open(bytes, opts)?;
    let (w, h) = (img.width(), img.height());
    let header = img.image_header();
    let bits = header.metadata.bit_depth.bits_per_sample() as u8;
    let float = matches!(header.metadata.bit_depth, jxl_oxide::image::BitDepth::FloatSample { .. });
    let gray = header.metadata.grayscale();
    let ec: Vec<ExtraChannelType> = header.metadata.ec_info.iter().map(|e| e.ty).collect();
    if has_black(&ec) || img.pixel_format().has_black() {
        return Err(Error::Unsupported(F, "CMYK JPEG XL isn't supported yet"));
    }
    let alpha = pick_alpha(&ec);
    let icc = img.original_icc().map(|v| v.to_vec());
    let xyb = header.metadata.xyb_encoded;
    let mut meta = Meta { orientation: Some(1), ..Default::default() };
    // linear sRGB primaries, unclamped, then our own matrix to Rec.2020 (see the module docs)
    let via_srgb = icc.is_none() && !gray && xyb;
    // Associated alpha on a non-XYB image: the samples come back in the file's own encoding (an ICC
    // space, or an enum one we model), divided by alpha here, then converted by `finish`.
    let own_space = match &header.metadata.colour_encoding {
        // (with a linear curve, dividing after jxl-oxide's conversion is the same thing, so that path stays)
        ColourEncoding::Enum(e) if icc.is_none() && !xyb && e.tf != TransferFunction::Linear => enum_space(e),
        _ => None,
    };
    let associated = alpha.is_some_and(|a| a.associated);
    let unpremultiply_here = associated && !xyb && (icc.is_some() || own_space.is_some());
    if unpremultiply_here && icc.is_none() {
        // rendered in its own encoding (jxl-oxide's default for an enum image), described to `finish`
        meta.hint = own_space;
    } else if icc.is_none() {
        img.request_color_encoding(EnumColourEncoding {
            colour_space: if gray { ColourSpace::Grey } else { ColourSpace::Rgb },
            white_point: WhitePoint::D65,
            primaries: if via_srgb { Primaries::Srgb } else { Primaries::Bt2100 },
            tf: TransferFunction::Linear,
            rendering_intent: RenderingIntent::Relative,
        });
        let mut s = SourceSpace::named(NamedSpace::Rec2020, SpaceOrigin::Container);
        s.trc = Some([Trc::Linear, Trc::Linear, Trc::Linear]);
        meta.already_linear = Some(s);
    } else {
        meta.icc = icc;
    }
    if let Ok(AuxBoxData::Data(ex)) = img.aux_boxes().first_exif() {
        // The box declares where the TIFF header starts inside its payload; skip any prefix before it.
        meta.exif = usize::try_from(ex.tiff_header_offset()).ok().and_then(|offset| ex.payload().get(offset..)).map(<[u8]>::to_vec);
    }
    if let AuxBoxData::Data(x) = img.aux_boxes().first_xml() {
        meta.xmp = Some(String::from_utf8_lossy(x).trim_end_matches('\0').to_string());
    }
    let render = img.render_frame(0).map_err(err)?;
    let fb = render.image_all_channels();
    let (fw, fh, ch) = (fb.width(), fb.height(), fb.channels());
    let n_color = if gray { 1 } else { 3 };
    // colour channels first, then every extra channel in header order
    let color_ch = ch.checked_sub(ec.len()).filter(|&c| c >= n_color).ok_or_else(|| err("too few channels"))?;
    let alpha_at = alpha.map(|a| color_ch + a.index);
    let keep = n_color + alpha_at.is_some() as usize;
    let src = fb.buf();
    let mut v = Vec::with_capacity(crate::checked_samples(fw, fh, keep)?);
    let to_2020 = lcc_color::SRGB.to_space(&lcc_color::REC2020).to_f32();
    for px in src.chunks_exact(ch) {
        let start = v.len();
        v.extend_from_slice(&px[..n_color]);
        if let Some(i) = alpha_at {
            let a = px[i];
            if unpremultiply_here && a.clamp(0.0, 1.0) > 0.0 {
                let a = a.clamp(0.0, 1.0);
                for c in &mut v[start..] {
                    *c /= a;
                }
            }
            v.push(a);
        }
        if via_srgb && let Some(rgb) = v.get_mut(start..start + 3) {
            let [r, g, b] = [rgb[0], rgb[1], rgb[2]];
            for (o, m) in rgb.iter_mut().zip(&to_2020) {
                *o = m[0] * r + m[1] * g + m[2] * b;
            }
        }
    }
    let raw = Raw {
        width: fw,
        height: fh,
        model: if gray { Model::Gray } else { Model::Rgb },
        alpha: alpha.is_some(),
        premultiplied: associated && !unpremultiply_here,
        buf: Buf::F32(v),
        bit_depth: bits,
    };
    let mut d = finish(F, raw, meta, (w, h), opts)?;
    d.float = float;
    Ok(d)
}

/// The space of an enum colour encoding whose curve this crate models (not PQ, HLG or unknown), with
/// its primaries and white point; `None` otherwise.
fn enum_space(e: &EnumColourEncoding) -> Option<SourceSpace> {
    if !matches!(e.colour_space, ColourSpace::Rgb | ColourSpace::Grey) {
        return None;
    }
    let trc = match e.tf {
        TransferFunction::Linear => Trc::Linear,
        TransferFunction::Srgb => Trc::Srgb,
        TransferFunction::Bt709 => Trc::Rec709,
        TransferFunction::Dci => Trc::Gamma(2.6),
        // the codestream's gamma is the encoding exponent (scaled by 1e7): decoding raises to its inverse
        TransferFunction::Gamma { g, inverted } if g > 0 => Trc::Gamma(if inverted { 1e7 / g as f32 } else { g as f32 / 1e7 }),
        _ => return None,
    };
    let xy = |c: Customxy| {
        let [x, y] = c.as_float();
        lcc_color::Xy::new(f64::from(x), f64::from(y))
    };
    let white = match e.white_point {
        WhitePoint::D65 => lcc_color::D65,
        WhitePoint::E => lcc_color::Xy::new(1.0 / 3.0, 1.0 / 3.0),
        WhitePoint::Dci => lcc_color::Xy::new(0.314, 0.351),
        WhitePoint::Custom(c) => xy(c),
    };
    let named = |s: lcc_color::RgbSpace| (s.r, s.g, s.b);
    let (r, g, b) = match e.primaries {
        Primaries::Srgb => named(lcc_color::SRGB),
        Primaries::Bt2100 => named(lcc_color::REC2020),
        Primaries::P3 => named(lcc_color::DISPLAY_P3),
        Primaries::Custom { red, green, blue } => (xy(red), xy(green), xy(blue)),
    };
    let to_xyz_d50 = rgb_to_xyz_d50(&lcc_color::RgbSpace { name: "JPEG XL", r, g, b, white });
    Some(SourceSpace {
        named: NamedSpace::recognize(&to_xyz_d50),
        to_xyz_d50,
        trc: Some([trc.clone(), trc.clone(), trc]),
        origin: SpaceOrigin::Container,
    })
}

/// The alpha channel among the extra channels: its index in header order and whether the colour
/// channels are premultiplied by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AlphaChannel {
    index: usize,
    associated: bool,
}

/// The first extra channel typed Alpha. Black, spot colour, depth and the other types never count
/// as alpha, wherever they sit in the list.
fn pick_alpha(ec: &[ExtraChannelType]) -> Option<AlphaChannel> {
    ec.iter().enumerate().find_map(|(index, ty)| match *ty {
        ExtraChannelType::Alpha { alpha_associated } => Some(AlphaChannel { index, associated: alpha_associated }),
        _ => None,
    })
}

/// A Black extra channel marks a CMYK image (its colour channels hold C, M and Y).
fn has_black(ec: &[ExtraChannelType]) -> bool {
    ec.iter().any(|ty| matches!(ty, ExtraChannelType::Black))
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRAIGHT: ExtraChannelType = ExtraChannelType::Alpha { alpha_associated: false };
    const ASSOCIATED: ExtraChannelType = ExtraChannelType::Alpha { alpha_associated: true };
    const SPOT: ExtraChannelType = ExtraChannelType::SpotColour { red: 1.0, green: 0.0, blue: 0.0, solidity: 1.0 };

    #[test]
    fn alpha_is_picked_by_type() {
        assert_eq!(pick_alpha(&[SPOT, STRAIGHT]), Some(AlphaChannel { index: 1, associated: false }));
        assert_eq!(pick_alpha(&[STRAIGHT, SPOT]), Some(AlphaChannel { index: 0, associated: false }));
        assert_eq!(pick_alpha(&[ExtraChannelType::Black, STRAIGHT]), Some(AlphaChannel { index: 1, associated: false }));
        assert_eq!(pick_alpha(&[ExtraChannelType::Depth, ASSOCIATED, STRAIGHT]), Some(AlphaChannel { index: 1, associated: true }));
        assert_eq!(pick_alpha(&[]), None);
        assert_eq!(pick_alpha(&[ExtraChannelType::Black, SPOT, ExtraChannelType::Depth]), None);
        // associated and straight alpha
        {
            assert_eq!(pick_alpha(&[ASSOCIATED]).map(|a| a.associated), Some(true));
            assert_eq!(pick_alpha(&[STRAIGHT]).map(|a| a.associated), Some(false));
        }
    }

    #[test]
    fn black_marks_cmyk() {
        assert!(has_black(&[ExtraChannelType::Black, STRAIGHT]));
        assert!(has_black(&[STRAIGHT, ExtraChannelType::Black]));
        assert!(!has_black(&[SPOT, STRAIGHT]));
        assert!(!has_black(&[]));
    }

    /// The same 8-bit samples encoded twice, with the associated-alpha bit off and on: the
    /// associated decode, multiplied back by alpha, matches the straight one.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn associated_alpha_is_unpremultiplied() {
        let (w, h) = (16usize, 8usize);
        let px: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let a = (i * 255 / (w * h - 1)) as u8;
                let c = |k: usize| ((i * k) % 256) as u8 & a;
                [c(37), c(91), c(13), a]
            })
            .collect();
        let enc = |associated: bool| {
            let m = jixel::FlMeta { alpha_premultiplied: associated, ..jixel::FlMeta::linear() };
            let b = jixel::encode_fast_lossless(&px, w, h, jixel::ColorSpace::Rgb, true, &m).unwrap();
            decode(&b, &DecodeOptions::default()).unwrap()
        };
        let (straight, assoc) = (enc(false), enc(true));
        let (sa, aa) = (straight.alpha.as_ref().unwrap(), assoc.alpha.as_ref().unwrap());
        assert_eq!(sa.data, aa.data);
        let mut checked = 0;
        for ((s, p), &a) in straight.image.data.iter().zip(&assoc.image.data).zip(&aa.data) {
            if a > 0.0 {
                for (sc, pc) in s.iter().zip(p) {
                    assert!((pc * a - sc).abs() < 1e-4, "{pc} * {a} != {sc}");
                }
                checked += 1;
            }
        }
        assert!(checked > w * h / 2);
        // and the straight decode keeps colour where alpha is partial
        assert!(straight.image.data.iter().zip(&sa.data).any(|(s, &a)| a > 0.0 && a < 1.0 && s.iter().any(|&c| c > 0.0)));
    }

    /// Semi-transparent sRGB pixels stored premultiplied (each 8-bit colour sample is round(s·a) of
    /// its straight value s), with the enum sRGB encoding and with an embedded sRGB ICC profile: the
    /// decode divides by alpha before the sRGB curve, so the colour is the straight value's. Dividing
    /// after linearizing would be far off at partial alpha (mid-grey at half alpha comes out ~2× too dark).
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn associated_alpha_in_srgb_is_divided_before_the_curve() {
        let (w, h) = (16usize, 8usize);
        let straight: Vec<[u8; 4]> = (0..w * h)
            .map(|i| [(i * 37 % 256) as u8, (i * 91 % 256) as u8, (40 + i * 13 % 216) as u8, 128 + (i * 127 / (w * h - 1)) as u8])
            .collect();
        let premultiplied: Vec<u8> = straight
            .iter()
            .flat_map(|p| {
                let a = f32::from(p[3]) / 255.0;
                [0, 1, 2].map(|k| (f32::from(p[k]) * a).round() as u8).into_iter().chain([p[3]])
            })
            .collect();
        let srgb = |v: f32| Trc::Srgb.to_linear(v / 255.0);
        for icc in [None, Some(crate::icc::write_named(NamedSpace::Srgb))] {
            let from_icc = icc.is_some();
            let m = jixel::FlMeta { alpha_premultiplied: true, icc, ..jixel::FlMeta::srgb() };
            let b = jixel::encode_fast_lossless(&premultiplied, w, h, jixel::ColorSpace::Rgb, true, &m).unwrap();
            let d = decode(&b, &DecodeOptions::default()).unwrap();
            assert_eq!(d.space.named, Some(NamedSpace::Srgb), "icc {from_icc}");
            assert_eq!(d.space.origin, if from_icc { SpaceOrigin::IccMatrixTrc } else { SpaceOrigin::Container });
            let alpha = d.alpha.as_ref().unwrap();
            let mut far_if_divided_after = 0;
            for (i, (got, p)) in d.image.data.iter().zip(&straight).enumerate() {
                let a = f32::from(p[3]) / 255.0;
                assert!((alpha.data[i] - a).abs() < 1e-6);
                for k in 0..3 {
                    // round(s·a)/a is within 1/255 of s for a ≥ 1/2: under 0.01 in linear light
                    let want = srgb(f32::from(p[k]));
                    assert!((got[k] - want).abs() < 0.01, "icc {from_icc} pixel {i} channel {k}: {} vs {want}", got[k]);
                    let after = srgb(premultiplied[i * 4 + k] as f32) / a;
                    far_if_divided_after += ((after - want).abs() > 0.05) as usize;
                }
            }
            assert!(far_if_divided_after > w * h / 2, "{far_if_divided_after}");
        }
    }

    /// A file without alpha decodes with none: no plane, colour as coded.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_file_without_alpha_has_none() {
        let (w, h) = (8usize, 4usize);
        let px: Vec<u8> = (0..w * h * 3).map(|i| (i * 29 % 256) as u8).collect();
        let b = jixel::encode_fast_lossless(&px, w, h, jixel::ColorSpace::Rgb, false, &jixel::FlMeta::linear()).unwrap();
        let d = decode(&b, &DecodeOptions::default()).unwrap();
        assert!(d.alpha.is_none() && !d.has_alpha);
        assert_eq!((d.width, d.height), (w as u32, h as u32));
        // linear sRGB primaries in, linear Rec.2020 out: grey stays the same grey
        let grey = jixel::encode_fast_lossless(&[100u8; 3 * 4], 2, 2, jixel::ColorSpace::Rgb, false, &jixel::FlMeta::linear()).unwrap();
        let g = decode(&grey, &DecodeOptions::default()).unwrap();
        assert!(g.alpha.is_none());
        assert!(
            g.image.data.iter().all(|p| (p[0] - 100.0 / 255.0).abs() < 1e-3 && (p[1] - p[0]).abs() < 1e-3 && (p[2] - p[0]).abs() < 1e-3),
            "{:?}",
            g.image.data
        );
    }

    /// Enum encodings map to the spaces they name; curves this crate does not model (PQ, HLG) give none.
    #[test]
    fn enum_encodings_map_to_spaces() {
        let enc = |primaries, tf| EnumColourEncoding {
            colour_space: ColourSpace::Rgb,
            white_point: WhitePoint::D65,
            primaries,
            tf,
            rendering_intent: RenderingIntent::Relative,
        };
        let srgb = enum_space(&enc(Primaries::Srgb, TransferFunction::Srgb)).unwrap();
        assert_eq!((srgb.named, srgb.trc.as_ref().map(|t| t[0].clone())), (Some(NamedSpace::Srgb), Some(Trc::Srgb)));
        assert_eq!(enum_space(&enc(Primaries::P3, TransferFunction::Srgb)).unwrap().named, Some(NamedSpace::DisplayP3));
        assert_eq!(enum_space(&enc(Primaries::Bt2100, TransferFunction::Bt709)).unwrap().named, Some(NamedSpace::Rec2020));
        // a codestream gamma of 0.4545455 (as coded, inverted) decodes with exponent 2.2
        let g = enum_space(&enc(Primaries::Srgb, TransferFunction::Gamma { g: 4_545_455, inverted: true })).unwrap();
        assert!(matches!(g.trc.as_ref().map(|t| &t[0]), Some(Trc::Gamma(e)) if (e - 2.2).abs() < 1e-4));
        assert!(enum_space(&enc(Primaries::Bt2100, TransferFunction::Pq)).is_none());
        assert!(enum_space(&enc(Primaries::Bt2100, TransferFunction::Hlg)).is_none());
    }
}
