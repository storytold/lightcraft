//! JPEG XL encode via `jixel` (pure Rust, BSD-3-Clause OR Apache-2.0; native targets with the `jxl`
//! feature: it spawns threads of its own). Lossy files are VarDCT in XYB, tagged with an enum colour
//! encoding the decoder renders back to; lossless files are Modular and keep their samples as they
//! are, tagged with the ICC profile when one is given. EXIF and XMP go in `Exif` / `xml ` boxes.

use crate::encode::{EncodeImage, EncodeMeta, HDR_REFERENCE_WHITE_NITS, Samples, pq_encode};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// Encoder effort: time spent searching for a smaller file at the same quality. jixel's slowest
/// search (about 20 % smaller again, five times slower) is not offered: its files are valid, but
/// jxl-oxide, our decoder, renders blocks of them wrongly, so LightCraft could not read back its own
/// exports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JxlEffort {
    /// Plain 8×8 transforms only: quickest, largest (about a third larger than `Normal`).
    Fast,
    #[default]
    Normal,
}

impl JxlEffort {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "fast" => Self::Fast,
            "normal" => Self::Normal,
            _ => return None,
        })
    }
}

/// The colour encoding a JPEG XL file declares. Integer samples are display-encoded with its curve.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JxlColour {
    #[default]
    Srgb,
    /// P3 primaries, D65, sRGB curve.
    DisplayP3,
    /// Rec. 2020 primaries with the BT.709 curve.
    Rec2020,
    /// HDR: Rec. 2020 primaries with the PQ curve (SMPTE ST 2084). Float samples are linear light
    /// with SDR white at 1.0 (written with SDR white at [`HDR_REFERENCE_WHITE_NITS`]); 16-bit
    /// samples are already PQ-encoded.
    Rec2020Pq,
}

/// JPEG XL encoder settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JxlOptions {
    /// 1..=100 on a libjpeg-like scale (90 ≈ visually lossless). Ignored when `lossless`.
    pub quality: u8,
    /// Mathematically lossless (Modular) instead of lossy (VarDCT).
    pub lossless: bool,
    pub effort: JxlEffort,
    /// Lossy: two passes, a coarse image first; lossless: the Squeeze transform (a low-resolution
    /// preview that refines to the exact image).
    pub progressive: bool,
    pub colour: JxlColour,
}

impl Default for JxlOptions {
    fn default() -> Self {
        Self { quality: 90, lossless: false, effort: JxlEffort::Normal, progressive: false, colour: JxlColour::Srgb }
    }
}

/// Encode JPEG XL from 8- or 16-bit samples (float samples only with [`JxlColour::Rec2020Pq`]).
/// Gray is widened to RGB; alpha is kept only when some pixel is not opaque. The ICC profile in
/// `meta` is embedded in lossless files (lossy ones carry the enum encoding of `o.colour`); `ppi`
/// has no place in the format. Returns [`Error::Encode`] on wasm32 or without the `jxl` feature.
pub fn encode_jxl(img: &EncodeImage, o: &JxlOptions, meta: &EncodeMeta) -> Result<Vec<u8>> {
    img.validate()?;
    #[cfg(all(feature = "jxl", not(target_arch = "wasm32")))]
    {
        let pixels = Pixels::from(img, o.colour)?;
        // jixel asserts internally; a panic there is an export error, not a crash
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&pixels, img.width as usize, img.height as usize, o, meta)))
            .unwrap_or_else(|_| Err(Error::Encode("JPEG XL encoder panicked".into())))
    }
    #[cfg(not(all(feature = "jxl", not(target_arch = "wasm32"))))]
    {
        let _ = (o, meta);
        Err(Error::Encode("JPEG XL encoding is not available in this build".into()))
    }
}

/// Interleaved RGB or RGBA samples for jixel.
#[cfg(all(feature = "jxl", not(target_arch = "wasm32")))]
enum Pixels {
    U8 { data: Vec<u8>, alpha: bool },
    U16 { data: Vec<u16>, alpha: bool },
}

#[cfg(all(feature = "jxl", not(target_arch = "wasm32")))]
impl Pixels {
    fn from(img: &EncodeImage, colour: JxlColour) -> Result<Self> {
        let n = (img.width as usize).checked_mul(img.height as usize).ok_or_else(|| Error::Encode("image too large".into()))?;
        let ch = img.channels as usize;
        // keep alpha only when it says something
        fn rgba<T: Copy>(s: &[T], ch: usize, n: usize, opaque: impl Fn(T) -> bool) -> (Vec<T>, bool) {
            let px = s.chunks_exact(ch).take(n);
            let alpha = (ch == 2 || ch == 4) && s.chunks_exact(ch).take(n).any(|c| c.last().is_some_and(|a| !opaque(*a)));
            let out_ch = if alpha { 4 } else { 3 };
            let mut v = Vec::with_capacity(n * out_ch);
            for c in px {
                match c {
                    [g] => v.extend_from_slice(&[*g, *g, *g]),
                    [g, a] => v.extend_from_slice(&[*g, *g, *g, *a][..out_ch]),
                    [r, g, b] => v.extend_from_slice(&[*r, *g, *b]),
                    [r, g, b, a, ..] => v.extend_from_slice(&[*r, *g, *b, *a][..out_ch]),
                    [] => {}
                }
            }
            (v, alpha)
        }
        Ok(match img.samples {
            Samples::U8(s) => {
                let (data, alpha) = rgba(s, ch, n, |a| a == u8::MAX);
                Pixels::U8 { data, alpha }
            }
            Samples::U16(s) => {
                let (data, alpha) = rgba(s, ch, n, |a| a == u16::MAX);
                Pixels::U16 { data, alpha }
            }
            Samples::F32(s) if colour == JxlColour::Rec2020Pq => {
                let pq = |v: f32| (pq_encode(if v.is_finite() { v.max(0.0) } else { 0.0 } * HDR_REFERENCE_WHITE_NITS) * 65535.0).round() as u16;
                let alpha = |a: f32| (if a.is_finite() { a.clamp(0.0, 1.0) } else { 1.0 } * 65535.0).round() as u16;
                let s: Vec<u16> = s
                    .chunks_exact(ch)
                    .take(n)
                    .flat_map(|c| match c {
                        [g] => [pq(*g), pq(*g), pq(*g), u16::MAX],
                        [g, a] => [pq(*g), pq(*g), pq(*g), alpha(*a)],
                        [r, g, b] => [pq(*r), pq(*g), pq(*b), u16::MAX],
                        [r, g, b, a, ..] => [pq(*r), pq(*g), pq(*b), alpha(*a)],
                        [] => [0; 4],
                    })
                    .collect();
                let (data, alpha) = rgba(&s, 4, n, |a| a == u16::MAX);
                Pixels::U16 { data, alpha }
            }
            Samples::F32(_) => return Err(Error::Encode("JPEG XL float samples are written as HDR (Rec. 2020 PQ) only".into())),
        })
    }
}

#[cfg(all(feature = "jxl", not(target_arch = "wasm32")))]
fn run(px: &Pixels, w: usize, h: usize, o: &JxlOptions, meta: &EncodeMeta) -> Result<Vec<u8>> {
    use jixel::{ColorEncoding, EncodeConfig, Primaries, Speed, TransferFunction};
    let colour = match o.colour {
        JxlColour::Srgb => ColorEncoding::srgb(),
        JxlColour::DisplayP3 => ColorEncoding::display_p3(),
        JxlColour::Rec2020 => ColorEncoding { primaries: Primaries::Bt2020, transfer: TransferFunction::Bt709, ..ColorEncoding::srgb() },
        JxlColour::Rec2020Pq => ColorEncoding::bt2020_pq(),
    };
    let mut c = EncodeConfig::default()
        .with_quality(f32::from(o.quality.clamp(1, 100)))
        .with_color_encoding(colour)
        .with_lossless(o.lossless)
        .with_progressive(o.progressive)
        .with_speed(match o.effort {
            JxlEffort::Fast => Speed::Fastest,
            JxlEffort::Normal => Speed::Fast,
        });
    if o.colour == JxlColour::Rec2020Pq {
        // PQ code values are absolute: 1.0 = 10 000 cd/m²
        c = c.with_intensity_target(10_000.0);
    }
    if o.lossless
        && let Some(icc) = meta.icc
    {
        c = c.with_icc_profile(icc.to_vec());
    }
    if let Some(exif) = meta.exif {
        c = c.with_exif(exif.to_vec());
    }
    if let Some(xmp) = meta.xmp {
        c = c.with_xmp(xmp.as_bytes().to_vec());
    }
    let r = match px {
        Pixels::U8 { data, alpha: false } => jixel::encode_image(data, w, h, &c),
        Pixels::U8 { data, alpha: true } => jixel::encode_image_with_alpha(data, w, h, &c),
        Pixels::U16 { data, alpha: false } => jixel::encode_image_16bit(data, w, h, &c),
        Pixels::U16 { data, alpha: true } => jixel::encode_image_with_alpha_16bit(data, w, h, &c),
    };
    r.map_err(|e| Error::Encode(format!("JPEG XL: {e}")))
}
