//! Encoders: JPEG (`craft-jpeg`), PNG (`png`), TIFF (`tiff`), flat PSD (our own), lossless WebP (`image-webp`) and,
//! on native targets with the `avif` feature, AVIF (`ravif`/`rav1e`); JPEG XL is in `jxl_encode`.
//! Metadata (ICC/EXIF/XMP) is embedded where the format allows.

use crate::{Error, Result};
use lightcraft_raster::Rgba8;
use serde::{Deserialize, Serialize};

/// Interleaved sample data. Integer samples are display-encoded (0..=MAX); float samples are
/// written as-is.
#[derive(Clone, Copy, Debug)]
pub enum Samples<'a> {
    U8(&'a [u8]),
    U16(&'a [u16]),
    F32(&'a [f32]),
}

/// An image to encode: `channels` = 1 (gray), 2 (gray + alpha), 3 (RGB) or 4 (RGBA), straight alpha.
#[derive(Clone, Copy, Debug)]
pub struct EncodeImage<'a> {
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub samples: Samples<'a>,
}

impl<'a> EncodeImage<'a> {
    pub fn rgba8(img: &'a Rgba8) -> Self {
        EncodeImage { width: img.width as u32, height: img.height as u32, channels: 4, samples: Samples::U8(bytemuck::cast_slice(&img.data)) }
    }
    pub fn new(width: u32, height: u32, channels: u8, samples: Samples<'a>) -> Self {
        EncodeImage { width, height, channels, samples }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if !(1..=4).contains(&self.channels) {
            return Err(Error::Encode(format!("unsupported channel count {}", self.channels)));
        }
        if self.width == 0 || self.height == 0 {
            return Err(Error::Encode("zero dimension".into()));
        }
        // u128: 2^32 x 2^32 x 4 does not fit u64, and every later `width * height * channels` is bounded by this
        let need = self.width as u128 * self.height as u128 * self.channels as u128;
        if need > usize::MAX as u128 {
            return Err(Error::Encode("image too large".into()));
        }
        let have = match self.samples {
            Samples::U8(s) => s.len(),
            Samples::U16(s) => s.len(),
            Samples::F32(s) => s.len(),
        } as u128;
        if have < need {
            return Err(Error::Encode(format!("sample buffer too short: {have} < {need}")));
        }
        Ok(())
    }

    fn has_alpha(&self) -> bool {
        self.channels == 2 || self.channels == 4
    }
}

/// Metadata to embed.
#[derive(Clone, Copy, Debug, Default)]
pub struct EncodeMeta<'a> {
    pub icc: Option<&'a [u8]>,
    /// TIFF-structured EXIF (starting at `II`/`MM`, no `Exif\0\0` prefix).
    pub exif: Option<&'a [u8]>,
    pub xmp: Option<&'a str>,
    /// Print resolution in pixels per inch (JPEG JFIF density, PNG `pHYs`, TIFF X/YResolution).
    pub ppi: Option<u16>,
}

/// JPEG chroma subsampling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChromaSubsampling {
    /// No subsampling (best quality).
    #[default]
    S444,
    S422,
    S420,
}

fn metadata_error(error: craft_image_codecs::CodecError) -> Error {
    match error {
        craft_image_codecs::CodecError::Encode { message, .. } => Error::Encode(message),
        other => Error::Encode(other.to_string()),
    }
}

pub(crate) fn jpeg_app_segments(meta: &EncodeMeta) -> Result<Vec<(u8, Vec<u8>)>> {
    craft_image_codecs::raw_metadata::jpeg_segments(craft_image_codecs::raw_metadata::JpegMetadata {
        exif: meta.exif,
        xmp: meta.xmp,
        icc: meta.icc,
        ppi: meta.ppi,
    })
    .map_err(metadata_error)
}

pub(crate) fn xmp_segment(xmp: &str) -> Result<Vec<u8>> {
    craft_image_codecs::raw_metadata::jpeg_xmp(xmp).map_err(metadata_error)
}

/// Encode baseline JPEG (8-bit; alpha is dropped). `quality` 1..=100.
pub fn encode_jpeg(img: &EncodeImage, quality: u8, subsampling: ChromaSubsampling, meta: &EncodeMeta) -> Result<Vec<u8>> {
    img.validate()?;
    let Samples::U8(data) = img.samples else {
        return Err(Error::Encode("JPEG needs 8-bit samples".into()));
    };
    if img.width > u16::MAX as u32 || img.height > u16::MAX as u32 {
        return Err(Error::Encode("JPEG dimensions are limited to 65535".into()));
    }
    let pixels = img.width as usize * img.height as usize;
    let (data, channels): (std::borrow::Cow<'_, [u8]>, usize) = match img.channels {
        1 => (data[..pixels].into(), 1),
        2 => (data.as_chunks::<2>().0.iter().take(pixels).map(|c| c[0]).collect::<Vec<_>>().into(), 1),
        3 => (data[..pixels * 3].into(), 3),
        _ => (data[..pixels * 4].into(), 4),
    };
    let subsampling = match subsampling {
        ChromaSubsampling::S444 => craft_jpeg::ChromaSubsampling::S444,
        ChromaSubsampling::S422 => craft_jpeg::ChromaSubsampling::S422,
        ChromaSubsampling::S420 => craft_jpeg::ChromaSubsampling::S420,
    };
    craft_jpeg::encode_baseline(&data, img.width as usize, img.height as usize, channels, quality, subsampling, &jpeg_app_segments(meta)?)
        .map_err(|e| Error::Encode(e.to_string()))
}

/// Encode PNG (8- or 16-bit; float input is rejected — quantize first).
pub fn encode_png(img: &EncodeImage, meta: &EncodeMeta) -> Result<Vec<u8>> {
    img.validate()?;
    let n = img.width as usize * img.height as usize * img.channels as usize;
    let layout = match img.channels {
        1 => craft_image_codecs::ChannelLayout::Gray,
        2 => craft_image_codecs::ChannelLayout::GrayA,
        3 => craft_image_codecs::ChannelLayout::Rgb,
        _ => craft_image_codecs::ChannelLayout::Rgba,
    };
    let mut ic_img = match img.samples {
        Samples::U8(s) => craft_image_codecs::Image::from_u8(img.width, img.height, layout, s[..n].to_vec()),
        Samples::U16(s) => craft_image_codecs::Image::from_u16(img.width, img.height, layout, &s[..n]),
        Samples::F32(_) => return Err(Error::Encode("PNG needs 8- or 16-bit samples".into())),
    }
    .map_err(|e| Error::Encode(e.to_string()))?;
    ic_img.icc = meta.icc.map(|b| b.to_vec());
    ic_img.meta.exif = meta.exif.map(|b| b.to_vec());
    ic_img.meta.xmp = meta.xmp.map(|x| x.to_string());
    ic_img.meta.dpi = meta.ppi.filter(|p| *p > 0).map(|p| (p as f32, p as f32));
    let ic_opts = craft_image_codecs::EncodeOptions { png_compression: craft_image_codecs::PngCompression::Fast, ..Default::default() };
    let mut out = craft_image_codecs::encode(&ic_img, craft_image_codecs::Format::Png, &ic_opts).map_err(|e| Error::Encode(e.to_string()))?;
    if let Some(exif) = meta.exif {
        craft_image_codecs::raw_metadata::replace_png_exif(&mut out, exif).map_err(metadata_error)?;
    }
    Ok(out)
}

/// TIFF compression.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TiffCompression {
    None,
    Lzw,
    #[default]
    Deflate,
    PackBits,
}

/// Encode TIFF: 8/16-bit integer or 32-bit float, gray/RGB with optional (unassociated) alpha.
/// ICC and XMP are embedded; EXIF is not (write it through the TIFF writer in `craft-tiff`).
pub fn encode_tiff(img: &EncodeImage, compression: TiffCompression, meta: &EncodeMeta) -> Result<Vec<u8>> {
    img.validate()?;
    let n = img.width as usize * img.height as usize * img.channels as usize;
    let layout = match img.channels {
        1 => craft_image_codecs::ChannelLayout::Gray,
        2 => craft_image_codecs::ChannelLayout::GrayA,
        3 => craft_image_codecs::ChannelLayout::Rgb,
        _ => craft_image_codecs::ChannelLayout::Rgba,
    };
    let mut ic_img = match img.samples {
        Samples::U8(s) => craft_image_codecs::Image::from_u8(img.width, img.height, layout, s[..n].to_vec()),
        Samples::U16(s) => craft_image_codecs::Image::from_u16(img.width, img.height, layout, &s[..n]),
        Samples::F32(s) => craft_image_codecs::Image::from_f32(img.width, img.height, layout, &s[..n]),
    }
    .map_err(|e| Error::Encode(e.to_string()))?;
    ic_img.icc = meta.icc.map(|b| b.to_vec());
    ic_img.meta.xmp = meta.xmp.map(|x| x.to_string());
    ic_img.meta.dpi = meta.ppi.filter(|p| *p > 0).map(|p| (p as f32, p as f32));
    let tiff_compression = match compression {
        TiffCompression::None => craft_image_codecs::TiffCompression::None,
        TiffCompression::Lzw => craft_image_codecs::TiffCompression::Lzw,
        TiffCompression::Deflate => craft_image_codecs::TiffCompression::Deflate,
        TiffCompression::PackBits => craft_image_codecs::TiffCompression::PackBits,
    };
    let ic_opts = craft_image_codecs::EncodeOptions { tiff_compression, ..Default::default() };
    craft_image_codecs::encode(&ic_img, craft_image_codecs::Format::Tiff, &ic_opts).map_err(|e| Error::Encode(e.to_string()))
}

/// Encode a flat PSD (8/16-bit gray or RGB, no layers; alpha dropped): the hand-off an image
/// editor such as PhotoCraft saves back to in place. ICC, EXIF, XMP and resolution are embedded.
pub fn encode_psd(img: &EncodeImage, meta: &EncodeMeta) -> Result<Vec<u8>> {
    crate::psd::encode(img, meta)
}

/// Encode lossless WebP (8-bit). ICC/EXIF/XMP are embedded in the extended (VP8X) container.
pub fn encode_webp_lossless(img: &EncodeImage, meta: &EncodeMeta) -> Result<Vec<u8>> {
    img.validate()?;
    let Samples::U8(s) = img.samples else {
        return Err(Error::Encode("WebP needs 8-bit samples".into()));
    };
    let n = img.width as usize * img.height as usize * img.channels as usize;
    let layout = match img.channels {
        1 => craft_image_codecs::ChannelLayout::Gray,
        2 => craft_image_codecs::ChannelLayout::GrayA,
        3 => craft_image_codecs::ChannelLayout::Rgb,
        _ => craft_image_codecs::ChannelLayout::Rgba,
    };
    let mut ic_img = craft_image_codecs::Image::from_u8(img.width, img.height, layout, s[..n].to_vec()).map_err(|e| Error::Encode(e.to_string()))?;
    ic_img.icc = meta.icc.map(|b| b.to_vec());
    ic_img.meta.exif = meta.exif.map(|b| b.to_vec());
    ic_img.meta.xmp = meta.xmp.map(|x| x.to_string());
    let ic_opts = craft_image_codecs::EncodeOptions { webp_lossless: true, ..Default::default() };
    let mut out = craft_image_codecs::encode(&ic_img, craft_image_codecs::Format::WebP, &ic_opts).map_err(|e| Error::Encode(e.to_string()))?;
    if let Some(exif) = meta.exif {
        craft_image_codecs::raw_metadata::replace_webp_exif(&mut out, exif).map_err(metadata_error)?;
    }
    Ok(out)
}

/// Encode AVIF (sRGB, 4:4:4, 10-bit AV1) with `ravif` (rav1e). 8-bit samples are widened by the
/// encoder; 16-bit samples keep 10 bits of precision (BT.601 full-range YCbCr computed here).
/// `quality` 1..=100, `speed` 1 (slow) ..= 10. EXIF is embedded; ICC is not supported by the muxer
/// (the file is tagged sRGB via `nclx`). Returns [`Error::Encode`] for float samples, on wasm32 or
/// when built without the `avif` feature.
pub fn encode_avif(img: &EncodeImage, quality: u8, speed: u8, meta: &EncodeMeta) -> Result<Vec<u8>> {
    img.validate()?;
    #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
    {
        let mut enc = ravif::Encoder::new()
            .with_quality(quality.clamp(1, 100) as f32)
            .with_alpha_quality(quality.clamp(1, 100) as f32)
            .with_speed(speed.clamp(1, 10));
        if let Some(exif) = meta.exif {
            enc = enc.with_exif(exif.to_vec());
        }
        let s = match img.samples {
            Samples::U8(s) => s,
            Samples::U16(s) => return avif_10bit(&enc, img, s),
            Samples::F32(_) => return Err(Error::Encode("AVIF encoder needs 8- or 16-bit samples".into())),
        };
        let n = img.width as usize * img.height as usize;
        let ch = img.channels as usize;
        let px: Vec<ravif::RGBA8> = s
            .chunks_exact(ch)
            .take(n)
            .map(|c| match ch {
                1 => ravif::RGBA8::new(c[0], c[0], c[0], 255),
                2 => ravif::RGBA8::new(c[0], c[0], c[0], c[1]),
                3 => ravif::RGBA8::new(c[0], c[1], c[2], 255),
                _ => ravif::RGBA8::new(c[0], c[1], c[2], c[3]),
            })
            .collect();
        let r = enc.encode_rgba(ravif::Img::new(&px[..], img.width as usize, img.height as usize)).map_err(|e| Error::Encode(e.to_string()))?;
        let _ = img.has_alpha();
        Ok(r.avif_file)
    }
    #[cfg(not(all(feature = "avif", not(target_arch = "wasm32"))))]
    {
        let _ = (quality, speed, meta, img.has_alpha());
        Err(Error::Encode("AVIF encoding is not available in this build".into()))
    }
}

/// Absolute luminance of SDR white in an HDR file (ITU-R BT.2408 reference white), cd/m².
pub const HDR_REFERENCE_WHITE_NITS: f32 = 203.0;

/// SMPTE ST 2084 (PQ) inverse EOTF: luminance in cd/m² → signal 0..1.
pub fn pq_encode(nits: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 4096.0 * 128.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 4096.0 * 32.0;
    const C3: f32 = 2392.0 / 4096.0 * 32.0;
    let y = (nits / 10_000.0).clamp(0.0, 1.0).powf(M1);
    ((C1 + C2 * y) / (1.0 + C3 * y)).powf(M2)
}

/// SMPTE ST 2084 (PQ) EOTF: signal 0..1 → luminance in cd/m².
pub fn pq_decode(e: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 4096.0 * 128.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 4096.0 * 32.0;
    const C3: f32 = 2392.0 / 4096.0 * 32.0;
    let p = e.clamp(0.0, 1.0).powf(1.0 / M2);
    ((p - C1).max(0.0) / (C2 - C3 * p)).powf(1.0 / M1) * 10_000.0
}

/// Transfer curve of an HDR file: SMPTE ST 2084 (PQ, absolute) or ITU-R BT.2100 HLG (relative).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HdrTransfer {
    #[default]
    Pq,
    Hlg,
}

/// Nominal peak of the HLG reference display, cd/m² (ITU-R BT.2100 / BT.2408).
pub const HLG_REFERENCE_PEAK_NITS: f32 = 1000.0;

/// Display-referred linear RGB (SDR white = 1.0, Rec. 2020) → HLG signal 0..1, per BT.2100:
/// display light normalised to the 1000 cd/m² reference display, the inverse OOTF (system gamma
/// 1.2, applied on luminance so hue is kept), then the HLG OETF. SDR white lands at signal 0.75,
/// as BT.2408 asks. Non-finite or negative inputs count as 0.
pub fn hlg_signal(rgb: [f32; 3]) -> [f32; 3] {
    const K: [f32; 3] = [0.2627, 0.6780, 0.0593];
    const GAMMA: f32 = 1.2;
    let fd = rgb.map(|v| (if v.is_finite() { v.max(0.0) } else { 0.0 }) * (HDR_REFERENCE_WHITE_NITS / HLG_REFERENCE_PEAK_NITS));
    let yd = (K[0] * fd[0] + K[1] * fd[1] + K[2] * fd[2]).min(1.0);
    let scale = if yd > 0.0 { yd.powf((1.0 - GAMMA) / GAMMA) } else { 0.0 };
    fd.map(|v| lightcraft_color::transfer::hlg_encode(f64::from((v * scale).clamp(0.0, 1.0))).clamp(0.0, 1.0) as f32)
}

/// Encode an HDR AVIF with the PQ curve (see [`encode_avif_hdr`]).
pub fn encode_avif_pq(width: u32, height: u32, rgb: &[f32], quality: u8, speed: u8, meta: &EncodeMeta) -> Result<Vec<u8>> {
    encode_avif_hdr(width, height, rgb, quality, speed, meta, HdrTransfer::Pq)
}

/// Encode an HDR AVIF: `rgb` is linear light in **Rec. 2020** primaries with SDR white at 1.0
/// (`width × height × 3`). Written as 10-bit 4:4:4 BT.2020 non-constant-luminance YCbCr with the
/// `transfer` curve: PQ with SDR white at [`HDR_REFERENCE_WHITE_NITS`] and a content light level
/// box, or HLG ([`hlg_signal`], no light level box: HLG is relative to the display).
/// `quality` 1..=100, `speed` 1..=10. Native targets with the `avif` feature only.
pub fn encode_avif_hdr(width: u32, height: u32, rgb: &[f32], quality: u8, speed: u8, meta: &EncodeMeta, transfer: HdrTransfer) -> Result<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    let n = w.checked_mul(h).ok_or_else(|| Error::Encode("image too large".into()))?;
    if n == 0 || n.checked_mul(3).is_none_or(|need| rgb.len() < need) {
        return Err(Error::Encode("HDR AVIF: sample buffer too short".into()));
    }
    #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
    {
        use rav1e::prelude::*;
        const K: [f32; 3] = [0.2627, 0.6780, 0.0593]; // BT.2020 NCL
        let mut max_nits = 0f32;
        let mut sum_nits = 0f64;
        let planes: Vec<[u16; 3]> = rgb
            .as_chunks::<3>()
            .0
            .iter()
            .take(n)
            .map(|c| {
                let nits = c.map(|v| (if v.is_finite() { v.max(0.0) } else { 0.0 }) * HDR_REFERENCE_WHITE_NITS);
                let y_nits = K[0] * nits[0] + K[1] * nits[1] + K[2] * nits[2];
                max_nits = max_nits.max(nits[0].max(nits[1]).max(nits[2]));
                sum_nits += y_nits as f64;
                let [r, g, b] = match transfer {
                    HdrTransfer::Pq => nits.map(pq_encode),
                    HdrTransfer::Hlg => hlg_signal([c[0], c[1], c[2]]),
                };
                let y = K[0] * r + K[1] * g + K[2] * b;
                let cb = (b - y) / (2.0 * (1.0 - K[2])) + 0.5;
                let cr = (r - y) / (2.0 * (1.0 - K[0])) + 0.5;
                [y, cb, cr].map(|v| (v * 1023.0).round().clamp(0.0, 1023.0) as u16)
            })
            .collect();
        let q = quality.clamp(1, 100) as f32 / 100.0;
        // ravif's quality → quantizer curve, so qualities match the SDR AVIF path
        let x = if q >= 0.82 {
            (1.0 - q) * 2.6
        } else if q > 0.25 {
            1.0 - 0.125 - q * 0.5
        } else {
            1.0 - q
        };
        let quantizer = (x * 255.0).round() as usize;
        let mut ec = EncoderConfig::with_speed_preset(speed.clamp(1, 10));
        ec.width = w;
        ec.height = h;
        ec.time_base = Rational::new(1, 1);
        ec.bit_depth = 10;
        ec.chroma_sampling = ChromaSampling::Cs444;
        ec.pixel_range = PixelRange::Full;
        ec.color_description = Some(ColorDescription {
            color_primaries: ColorPrimaries::BT2020,
            transfer_characteristics: match transfer {
                HdrTransfer::Pq => TransferCharacteristics::SMPTE2084,
                HdrTransfer::Hlg => TransferCharacteristics::HLG,
            },
            matrix_coefficients: MatrixCoefficients::BT2020NCL,
        });
        let cll = (max_nits.ceil().clamp(1.0, 10_000.0) as u16, ((sum_nits / n as f64).ceil().clamp(1.0, 10_000.0)) as u16);
        ec.content_light =
            (transfer == HdrTransfer::Pq).then_some(ContentLight { max_content_light_level: cll.0, max_frame_average_light_level: cll.1 });
        ec.still_picture = true;
        ec.quantizer = quantizer;
        ec.min_quantizer = quantizer.min(255) as u8;
        ec.speed_settings.multiref = false;
        ec.speed_settings.rdo_lookahead_frames = 1;
        ec.speed_settings.scene_detection_mode = SceneDetectionSpeed::None;
        let e = |e: &dyn std::fmt::Display| Error::Encode(format!("AV1: {e}"));
        let mut ctx: Context<u16> = Config::new().with_encoder_config(ec).new_context().map_err(|x| e(&x))?;
        let mut frame = ctx.new_frame();
        {
            let mut it = planes.iter();
            let mut fp = frame.planes.iter_mut();
            let (Some(py), Some(pu), Some(pv)) = (fp.next(), fp.next(), fp.next()) else {
                return Err(Error::Encode("AV1: frame has no planes".into()));
            };
            let (mut sy, mut su, mut sv) = (py.mut_slice(Default::default()), pu.mut_slice(Default::default()), pv.mut_slice(Default::default()));
            for ((ry, ru), rv) in sy.rows_iter_mut().zip(su.rows_iter_mut()).zip(sv.rows_iter_mut()).take(h) {
                for ((y, u), v) in ry.iter_mut().zip(ru.iter_mut()).zip(rv.iter_mut()).take(w) {
                    let Some(p) = it.next() else { break };
                    (*y, *u, *v) = (p[0], p[1], p[2]);
                }
            }
        }
        ctx.send_frame(frame).map_err(|x| e(&x))?;
        ctx.flush();
        let mut av1 = Vec::new();
        loop {
            match ctx.receive_packet() {
                Ok(mut p) if p.frame_type == FrameType::KEY => av1.append(&mut p.data),
                Ok(_) => {}
                Err(EncoderStatus::Encoded | EncoderStatus::LimitReached) => break,
                Err(x) => return Err(e(&x)),
            }
        }
        use avif_serialize::constants as c;
        let mut a = avif_serialize::Aviffy::new();
        a.set_matrix_coefficients(c::MatrixCoefficients::Bt2020Ncl)
            .set_transfer_characteristics(match transfer {
                HdrTransfer::Pq => c::TransferCharacteristics::Smpte2084,
                HdrTransfer::Hlg => c::TransferCharacteristics::Hlg,
            })
            .set_color_primaries(c::ColorPrimaries::Bt2020)
            .set_full_color_range(true);
        if transfer == HdrTransfer::Pq {
            a.set_content_light_level(cll.0, cll.1);
        }
        if let Some(exif) = meta.exif {
            a.set_exif(exif.to_vec());
        }
        Ok(a.to_vec(&av1, None, width, height, 10))
    }
    #[cfg(not(all(feature = "avif", not(target_arch = "wasm32"))))]
    {
        let _ = (quality, speed, meta, transfer);
        Err(Error::Encode("AVIF encoding is not available in this build".into()))
    }
}

/// Encode an HDR PNG: `rgb` is linear light in **Rec. 2020** primaries with SDR white at 1.0
/// (`width × height × 3`), as for [`encode_avif_pq`]. Written as 16-bit RGB with the PQ curve
/// (SDR white at [`HDR_REFERENCE_WHITE_NITS`]), described by a `cICP` chunk (PNG third edition:
/// BT.2020 primaries, PQ, RGB, full range) and a `cLLI` content light level chunk (the same
/// levels the HDR AVIF records). EXIF and XMP are embedded; an ICC profile is not (`cICP`
/// describes the colour, and readers prefer it). Readers without `cICP` support show the PQ
/// samples as if they were sRGB (flat, washed out): expected for a PQ PNG, there is no fallback.
pub fn encode_png_pq(width: u32, height: u32, rgb: &[f32], meta: &EncodeMeta) -> Result<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    let n = w.checked_mul(h).ok_or_else(|| Error::Encode("image too large".into()))?;
    let samples = n.checked_mul(3).ok_or_else(|| Error::Encode("image too large".into()))?;
    if n == 0 || rgb.len() < samples {
        return Err(Error::Encode("HDR PNG: sample buffer too short".into()));
    }
    let mut image = craft_image_codecs::Image::from_f32(
        width,
        height,
        craft_image_codecs::ChannelLayout::Rgb,
        rgb.get(..samples).ok_or_else(|| Error::Encode("HDR PNG: sample buffer too short".into()))?,
    )
    .map_err(|e| Error::Encode(e.to_string()))?;
    image.meta.exif = meta.exif.map(ToOwned::to_owned);
    image.meta.xmp = meta.xmp.map(ToOwned::to_owned);
    image.meta.dpi = meta.ppi.filter(|&ppi| ppi > 0).map(|ppi| (f32::from(ppi), f32::from(ppi)));
    let options = craft_image_codecs::EncodeOptions { png_compression: craft_image_codecs::PngCompression::Fast, ..Default::default() };
    craft_image_codecs::encode_png_pq(&image, &options).map_err(|e| Error::Encode(e.to_string()))
}

/// 16-bit RGB(A) → 10-bit BT.601 full-range YCbCr planes (the matrix `ravif` uses for 8-bit input).
#[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
fn avif_10bit(enc: &ravif::Encoder, img: &EncodeImage, s: &[u16]) -> Result<Vec<u8>> {
    const K: [f32; 3] = [0.299, 0.587, 0.114];
    let n = img.width as usize * img.height as usize;
    let ch = img.channels as usize;
    let rgb = |c: &[u16]| -> [f32; 3] {
        let v = |x: u16| x as f32 * (1023.0 / 65535.0);
        match ch {
            1 | 2 => [v(c[0]); 3],
            _ => [v(c[0]), v(c[1]), v(c[2])],
        }
    };
    let planes = s.chunks_exact(ch).take(n).map(|c| {
        let [r, g, b] = rgb(c);
        let y = K[0] * r + K[1] * g + K[2] * b;
        let cb = (b - y) * (0.5 / (1.0 - K[2])) + 512.0;
        let cr = (r - y) * (0.5 / (1.0 - K[0])) + 512.0;
        [y, cb, cr].map(|v| v.round().clamp(0.0, 1023.0) as u16)
    });
    let alpha_at = |c: &[u16]| if img.has_alpha() { c[ch - 1] } else { u16::MAX };
    let has_alpha = s.chunks_exact(ch).take(n).any(|c| alpha_at(c) != u16::MAX);
    let alpha = has_alpha.then(|| s.chunks_exact(ch).take(n).map(move |c| ((alpha_at(c) as u32 * 1023 + 32767) / 65535) as u16));
    let r = enc
        .encode_raw_planes_10_bit(img.width as usize, img.height as usize, planes, alpha, ravif::PixelRange::Full, ravif::MatrixCoefficients::BT601)
        .map_err(|e| Error::Encode(e.to_string()))?;
    Ok(r.avif_file)
}

#[cfg(test)]
mod hlg_tests {
    use super::*;

    #[test]
    fn dimensions_whose_sample_count_wraps_are_refused() {
        // 2^31 x 2^31 x 4 is 2^64 samples: zero when wrapped, which an empty buffer would satisfy
        let img = EncodeImage::new(1 << 31, 1 << 31, 4, Samples::U8(&[]));
        assert!(img.validate().is_err());
    }

    #[test]
    fn hlg_reference_white_and_peak() {
        // BT.2408: SDR (HDR reference) white sits at 75 % HLG signal
        for v in hlg_signal([1.0; 3]) {
            assert!((v - 0.75).abs() < 0.005, "{v}");
        }
        // the 1000 cd/m² peak (≈ 4.9× SDR white) reaches full signal; above it clips
        let peak = HLG_REFERENCE_PEAK_NITS / HDR_REFERENCE_WHITE_NITS;
        assert!(hlg_signal([peak; 3]).iter().all(|v| (v - 1.0).abs() < 1e-3));
        assert_eq!(hlg_signal([100.0; 3]), [1.0; 3]);
        // black, negatives and non-finite values are 0, never NaN
        assert_eq!(hlg_signal([0.0, -1.0, f32::NAN]), [0.0; 3]);
        assert!(hlg_signal([f32::INFINITY, 0.0, 0.0]).iter().all(|v| v.is_finite()));
        // monotonic in brightness
        let mut prev = 0.0;
        for i in 1..50 {
            let v = hlg_signal([i as f32 * 0.1; 3])[1];
            assert!(v >= prev);
            prev = v;
        }
    }

    #[test]
    fn hdr_avif_rejects_short_buffers() {
        let m = EncodeMeta::default();
        assert!(encode_avif_hdr(4, 4, &[0.5; 10], 80, 10, &m, HdrTransfer::Hlg).is_err());
        assert!(encode_avif_hdr(0, 4, &[], 80, 10, &m, HdrTransfer::Hlg).is_err());
    }
}
