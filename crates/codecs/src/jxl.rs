//! JPEG XL decode via `jxl-oxide` (pure Rust). Enum-encoded images are rendered straight into linear
//! Rec.2020; ICC-tagged ones are rendered in their own space and interpreted with our ICC path.
//! XYB (lossy) images are rendered to linear sRGB primaries and converted to Rec.2020 here:
//! jxl-oxide 0.12 desaturates colours outside sRGB when it renders XYB to wider primaries.
//! jxl-oxide applies the codestream orientation itself, so `orientation` is reported as 1.

use crate::convert::{Buf, Meta, Model, Raw, check_size, finish};
use crate::space::{NamedSpace, SourceSpace, SpaceOrigin, Trc};
use crate::{DecodeOptions, Decoded, Error, Format, Result};
use jxl_oxide::color::{ColourSpace, EnumColourEncoding, Primaries, RenderingIntent, TransferFunction, WhitePoint};
use jxl_oxide::{AllocTracker, AuxBoxData, InitializeResult, JxlImage};

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
    let has_alpha = img.pixel_format().has_alpha();
    if ch < n_color {
        return Err(err("too few channels"));
    }
    let keep = n_color + has_alpha as usize;
    let src = fb.buf();
    let mut v = Vec::with_capacity(fw * fh * keep);
    let to_2020 = lightcraft_color::SRGB.to_space(&lightcraft_color::REC2020).to_f32();
    for px in src.chunks_exact(ch) {
        let start = v.len();
        v.extend_from_slice(&px[..keep.min(ch)]);
        if keep > ch {
            v.push(1.0);
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
        alpha: has_alpha,
        premultiplied: false,
        buf: Buf::F32(v),
        bit_depth: bits,
    };
    let mut d = finish(F, raw, meta, (w, h), opts)?;
    d.float = float;
    Ok(d)
}
