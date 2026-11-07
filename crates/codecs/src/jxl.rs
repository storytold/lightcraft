//! JPEG XL decode via `jxl-oxide` (pure Rust). Enum-encoded images are rendered straight into linear
//! Rec.2020; ICC-tagged ones are rendered in their own space and interpreted with our ICC path.
//! XYB (lossy) images are rendered to linear sRGB primaries and converted to Rec.2020 here:
//! jxl-oxide 0.12 desaturates colours outside sRGB when it renders XYB to wider primaries.
//! jxl-oxide applies the codestream orientation itself, so `orientation` is reported as 1.

use crate::convert::{Buf, Meta, Model, Raw, check_size, finish};
use crate::space::{NamedSpace, SourceSpace, SpaceOrigin, Trc};
use crate::{DecodeOptions, Decoded, Error, Format, Result};
use jxl_oxide::color::{ColourSpace, EnumColourEncoding, Primaries, RenderingIntent, TransferFunction, WhitePoint};
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
    let mut meta = Meta { orientation: Some(1), ..Default::default() };
    // linear sRGB primaries, unclamped, then our own matrix to Rec.2020 (see the module docs)
    let via_srgb = icc.is_none() && !gray && header.metadata.xyb_encoded;
    if icc.is_none() {
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
        meta.exif = Some(ex.payload().to_vec());
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
    let mut v = Vec::with_capacity(fw * fh * keep);
    let to_2020 = lightcraft_color::SRGB.to_space(&lightcraft_color::REC2020).to_f32();
    for px in src.chunks_exact(ch) {
        let start = v.len();
        v.extend_from_slice(&px[..n_color]);
        if let Some(i) = alpha_at {
            v.push(px[i]);
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
        premultiplied: alpha.is_some_and(|a| a.associated),
        buf: Buf::F32(v),
        bit_depth: bits,
    };
    let mut d = finish(F, raw, meta, (w, h), opts)?;
    d.float = float;
    Ok(d)
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
    }

    #[test]
    fn associated_and_straight_alpha() {
        assert_eq!(pick_alpha(&[ASSOCIATED]).map(|a| a.associated), Some(true));
        assert_eq!(pick_alpha(&[STRAIGHT]).map(|a| a.associated), Some(false));
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
}
