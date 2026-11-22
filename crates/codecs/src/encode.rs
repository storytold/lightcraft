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

/// The APP segments [`encode_jpeg`] writes for `meta` (marker byte, payload): JFIF density, EXIF,
/// XMP and the ICC profile in chunks, in that order.
pub(crate) fn jpeg_app_segments(meta: &EncodeMeta) -> Result<Vec<(u8, Vec<u8>)>> {
    craft_image_codecs::raw_metadata::jpeg_segments(craft_image_codecs::raw_metadata::JpegMetadata {
        exif: meta.exif,
        xmp: meta.xmp,
        icc: meta.icc,
        ppi: meta.ppi,
    })
    .map_err(|e| Error::Encode(e.to_string()))
}

/// An APP1 XMP payload (namespace + packet), refusing packets larger than one segment.
pub(crate) fn xmp_segment(xmp: &str) -> Result<Vec<u8>> {
    craft_image_codecs::raw_metadata::jpeg_xmp(xmp).map_err(|e| Error::Encode(e.to_string()))
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

/// `img` (already validated) as a `craft-image-codecs` image. `refuse_float` is the error for
/// 32-bit float samples, for a format that cannot hold them; `None` converts them too.
fn shared_image(img: &EncodeImage, refuse_float: Option<&str>) -> Result<craft_image_codecs::Image> {
    let n = img.width as usize * img.height as usize * img.channels as usize;
    let layout = match img.channels {
        1 => craft_image_codecs::ChannelLayout::Gray,
        2 => craft_image_codecs::ChannelLayout::GrayA,
        3 => craft_image_codecs::ChannelLayout::Rgb,
        _ => craft_image_codecs::ChannelLayout::Rgba,
    };
    match (img.samples, refuse_float) {
        (Samples::U8(s), _) => craft_image_codecs::Image::from_u8(img.width, img.height, layout, s[..n].to_vec()),
        (Samples::U16(s), _) => craft_image_codecs::Image::from_u16(img.width, img.height, layout, &s[..n]),
        (Samples::F32(_), Some(refusal)) => return Err(Error::Encode(refusal.into())),
        (Samples::F32(s), None) => craft_image_codecs::Image::from_f32(img.width, img.height, layout, &s[..n]),
    }
    .map_err(|e| Error::Encode(e.to_string()))
}

/// Encode PNG (8- or 16-bit; float input is rejected — quantize first).
pub fn encode_png(img: &EncodeImage, meta: &EncodeMeta) -> Result<Vec<u8>> {
    img.validate()?;
    let mut ic_img = shared_image(img, Some("PNG needs 8- or 16-bit samples"))?;
    ic_img.icc = meta.icc.map(|b| b.to_vec());
    ic_img.meta.exif = meta.exif.map(|b| b.to_vec());
    ic_img.meta.xmp = meta.xmp.map(|x| x.to_string());
    ic_img.meta.dpi = meta.ppi.filter(|p| *p > 0).map(|p| (p as f32, p as f32));
    let ic_opts = craft_image_codecs::EncodeOptions { png_compression: craft_image_codecs::PngCompression::Fast, ..Default::default() };
    let mut out = craft_image_codecs::encode(&ic_img, craft_image_codecs::Format::Png, &ic_opts).map_err(|e| Error::Encode(e.to_string()))?;
    if let Some(exif) = meta.exif {
        craft_image_codecs::raw_metadata::replace_png_exif(&mut out, exif).map_err(|e| Error::Encode(e.to_string()))?;
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
    let mut ic_img = shared_image(img, None)?;
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
        craft_image_codecs::raw_metadata::replace_webp_exif(&mut out, exif).map_err(|e| Error::Encode(e.to_string()))?;
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
        let mut ic_img = shared_image(img, Some("AVIF encoder needs 8- or 16-bit samples"))?;
        ic_img.meta.exif = meta.exif.map(|e| e.to_vec());
        craft_image_codecs::encode_avif_with_speed(&ic_img, quality, speed).map_err(|e| Error::Encode(e.to_string()))
    }
    #[cfg(not(all(feature = "avif", not(target_arch = "wasm32"))))]
    {
        let _ = (quality, speed, meta);
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
        let _ = (w, h, n);
        let t = match transfer {
            HdrTransfer::Pq => craft_image_codecs::HdrTransfer::Pq,
            HdrTransfer::Hlg => craft_image_codecs::HdrTransfer::Hlg,
        };
        craft_image_codecs::encode_avif_hdr(width, height, rgb, quality, speed, meta.exif, t).map_err(|e| Error::Encode(e.to_string()))
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

    #[test]
    fn jpeg_app_segments_order_and_webp_riff_padding_parity() {
        let exif = b"II*\0\x08\0\0\0\0\0\0\0\0\0\x01"; // 15 bytes (odd)
        let meta = EncodeMeta { ppi: Some(300), exif: Some(exif), xmp: Some("<x/>"), icc: Some(b"icc") };
        let segs = jpeg_app_segments(&meta).unwrap();
        let markers: Vec<u8> = segs.iter().map(|(m, _)| *m).collect();
        assert_eq!(markers, vec![0xE0, 0xE1, 0xE1, 0xE2]);
        assert!(segs[0].1.starts_with(b"JFIF\0"));
        assert!(segs[1].1.starts_with(b"Exif\0\0II*"));
        assert!(segs[2].1.starts_with(b"http://ns.adobe.com/xap/1.0/\0<x/>"));
        assert!(segs[3].1.starts_with(b"ICC_PROFILE\0"));

        let px = [10u8, 20, 30, 255];
        let img = EncodeImage::new(1, 1, 4, Samples::U8(&px));
        let webp = encode_webp_lossless(&img, &EncodeMeta { exif: Some(exif), ..Default::default() }).unwrap();
        assert_eq!(&webp[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(webp[4..8].try_into().unwrap()) as usize + 8, webp.len());
        let pos = webp.windows(4).position(|w| w == b"EXIF").expect("EXIF chunk");
        let chunk_len = u32::from_le_bytes(webp[pos + 4..pos + 8].try_into().unwrap()) as usize;
        assert_eq!(chunk_len, exif.len());
        assert_eq!(chunk_len % 2, 1);
        assert_eq!(webp[pos + 8 + chunk_len], 0, "odd WebP EXIF chunk must be zero-padded");
        let padded_end = pos + 8 + chunk_len + 1;
        assert!(padded_end <= webp.len());
        assert_eq!(crate::sniff(&webp), Some(crate::Format::WebP));
        let header = crate::read_header(&webp).unwrap();
        assert_eq!((header.width, header.height, header.orientation), (1, 1, 1));
        let decoded = crate::decode(&webp, crate::DecodeOptions::default()).unwrap();
        assert_eq!(decoded.exif.as_deref(), Some(exif.as_slice()));
    }

    #[test]
    fn jpeg_export_preserves_app_order_original_exif_and_orientation() {
        let pixels = [10u8, 120, 230].repeat(4);
        let img = EncodeImage::new(2, 2, 3, Samples::U8(&pixels));
        let exif = crate::exif::minimal_exif(6);
        let xmp = "<xmp/>";
        let icc = b"icc-profile";
        let meta = EncodeMeta { ppi: Some(300), exif: Some(&exif), xmp: Some(xmp), icc: Some(icc) };
        let jpeg = encode_jpeg(&img, 90, ChromaSubsampling::S444, &meta).unwrap();

        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
        let mut pos = 2usize;
        let mut apps = Vec::new();
        while pos + 4 <= jpeg.len() && jpeg[pos] == 0xFF {
            let marker = jpeg[pos + 1];
            if marker == 0xDA || marker == 0xD9 {
                break;
            }
            let len = u16::from_be_bytes([jpeg[pos + 2], jpeg[pos + 3]]) as usize;
            let payload = &jpeg[pos + 4..pos + 2 + len];
            if (0xE0..=0xEF).contains(&marker) {
                apps.push((marker, payload));
            }
            pos += 2 + len;
        }
        let markers: Vec<u8> = apps.iter().map(|(m, _)| *m).collect();
        assert_eq!(markers, vec![0xE0, 0xE1, 0xE1, 0xE2]);
        assert!(apps[0].1.starts_with(b"JFIF\0"));
        assert_eq!(apps[1].1, [b"Exif\0\0".as_slice(), exif.as_slice()].concat());
        assert_eq!(apps[2].1, [b"http://ns.adobe.com/xap/1.0/\0".as_slice(), xmp.as_bytes()].concat());
        assert_eq!(apps[3].1, [b"ICC_PROFILE\0\x01\x01".as_slice(), icc.as_slice()].concat());

        assert_eq!(crate::sniff(&jpeg), Some(crate::Format::Jpeg));
        let header = crate::read_header(&jpeg).unwrap();
        assert_eq!((header.format, header.width, header.height, header.orientation), (crate::Format::Jpeg, 2, 2, 6));
        let decoded = crate::decode(&jpeg, crate::DecodeOptions::default()).unwrap();
        assert_eq!(decoded.orientation, 6);
        assert_eq!(decoded.exif.as_deref(), Some(exif.as_slice()));
        assert_eq!(decoded.xmp.as_deref(), Some(xmp));
        assert_eq!(decoded.icc.as_deref(), Some(icc.as_slice()));
    }

    #[test]
    fn png_export_preserves_original_exif_orientation_and_crc() {
        let pixels = [10u8, 120, 230, 80, 40, 20];
        let img = EncodeImage::new(2, 1, 3, Samples::U8(&pixels));
        let exif = crate::exif::minimal_exif(6);
        let xmp = "<xmp/>";
        let meta = EncodeMeta { exif: Some(&exif), xmp: Some(xmp), ..Default::default() };
        let png = encode_png(&img, &meta).unwrap();

        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let mut chunks = &png[8..];
        let mut exif_chunks = 0usize;
        while !chunks.is_empty() {
            let len = u32::from_be_bytes(chunks[..4].try_into().unwrap()) as usize;
            let (chunk, rest) = chunks.split_at(12 + len);
            if &chunk[4..8] == b"eXIf" {
                assert_eq!(&chunk[8..8 + len], exif.as_slice());
                let mut crc = 0xFFFF_FFFFu32;
                for &b in &chunk[4..8 + len] {
                    crc ^= u32::from(b);
                    for _ in 0..8 {
                        crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
                    }
                }
                assert_eq!(&chunk[8 + len..12 + len], &(!crc).to_be_bytes());
                exif_chunks += 1;
            }
            chunks = rest;
        }
        assert_eq!(exif_chunks, 1);

        assert_eq!(crate::sniff(&png), Some(crate::Format::Png));
        let header = crate::read_header(&png).unwrap();
        assert_eq!((header.format, header.width, header.height, header.orientation), (crate::Format::Png, 2, 1, 6));
        let decoded = crate::decode(&png, crate::DecodeOptions::default()).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 1));
        assert_eq!(decoded.orientation, 6);
        assert_eq!(decoded.exif.as_deref(), Some(exif.as_slice()));
        assert_eq!(decoded.xmp.as_deref(), Some(xmp));
        assert_eq!(decoded.to_srgb8().data, [[10, 120, 230, 255], [80, 40, 20, 255]]);
    }

    #[test]
    fn metadata_and_encoder_error_limits_unchanged() {
        let ok_exif = vec![0u8; 65_527];
        assert!(jpeg_app_segments(&EncodeMeta { exif: Some(&ok_exif), ..Default::default() }).is_ok());
        let too_big_exif = vec![0u8; 65_528];
        assert!(jpeg_app_segments(&EncodeMeta { exif: Some(&too_big_exif), ..Default::default() }).is_err());

        let ok_xmp = "a".repeat(65_504);
        assert!(xmp_segment(&ok_xmp).is_ok());
        let too_big_xmp = "a".repeat(65_505);
        assert!(xmp_segment(&too_big_xmp).is_err());
        assert!(jpeg_app_segments(&EncodeMeta { xmp: Some(&too_big_xmp), ..Default::default() }).is_err());

        let too_big_icc = vec![0u8; 255 * 65_519 + 1];
        assert!(jpeg_app_segments(&EncodeMeta { icc: Some(&too_big_icc), ..Default::default() }).is_err());

        let u16_img = EncodeImage::new(1, 1, 3, Samples::U16(&[0; 3]));
        assert!(encode_jpeg(&u16_img, 85, ChromaSubsampling::S444, &EncodeMeta::default()).is_err());
        assert!(encode_webp_lossless(&u16_img, &EncodeMeta::default()).is_err());

        let f32_img = EncodeImage::new(1, 1, 3, Samples::F32(&[0.0; 3]));
        assert!(encode_png(&f32_img, &EncodeMeta::default()).is_err());

        let wide_pixels = vec![0u8; (u16::MAX as usize + 1) * 3];
        let wide_img = EncodeImage::new(u16::MAX as u32 + 1, 1, 3, Samples::U8(&wide_pixels));
        assert!(encode_jpeg(&wide_img, 85, ChromaSubsampling::S444, &EncodeMeta::default()).is_err());
    }
}
