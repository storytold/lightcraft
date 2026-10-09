//! HDR encoders. Input is display-linear RGB where 1.0 is SDR (reference) white and brighter
//! values are HDR highlights.
//!
//! - **AVIF**: 10-bit, PQ (SMPTE ST 2084) with BT.2020 or Display P3 primaries, tagged in both
//!   the AV1 bitstream and the `colr` box, plus content light levels (`clli`).
//! - **PNG**: 16-bit PQ with a `cICP` chunk (PNG third edition) and `cLLI`.
//! - **JPEG with a gain map** (the Ultra HDR layout: an SDR JPEG every viewer shows, plus a
//!   greyscale gain map JPEG, linked by a Multi-Picture Format index): HDR displays apply the gain
//!   map, everyone else sees the SDR base. The gain map is described twice, as `hdrgm` XMP and as
//!   ISO 21496-1 binary metadata (APP2), so readers of either find it.
//!
//! SDR white sits at 203 cd/m² in PQ (ITU-R BT.2408's HDR reference white).

use crate::encode::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
use crate::{Error, Result};

/// cd/m² of SDR white in PQ files (ITU-R BT.2408).
pub const SDR_WHITE_NITS: f32 = 203.0;

/// The primaries of an HDR file (ITU-T H.273 codes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HdrPrimaries {
    /// ITU-R BT.2020 (code 9).
    Bt2020,
    /// Display P3 (SMPTE EG 432-1, D65; code 12).
    DisplayP3,
}

impl HdrPrimaries {
    pub fn code(self) -> u8 {
        match self {
            HdrPrimaries::Bt2020 => 9,
            HdrPrimaries::DisplayP3 => 12,
        }
    }
}

/// A display-linear HDR image: interleaved RGB, 1.0 = SDR white.
#[derive(Clone, Copy, Debug)]
pub struct HdrImage<'a> {
    pub width: u32,
    pub height: u32,
    pub rgb: &'a [f32],
    pub primaries: HdrPrimaries,
}

impl HdrImage<'_> {
    fn pixels(&self) -> Result<usize> {
        if self.width == 0 || self.height == 0 {
            return Err(Error::Encode("zero dimension".into()));
        }
        let n = (self.width as usize).checked_mul(self.height as usize).ok_or_else(|| Error::Encode("image too large".into()))?;
        if self.rgb.len() < n.saturating_mul(3) {
            return Err(Error::Encode(format!("sample buffer too short: {} < {}", self.rgb.len(), n.saturating_mul(3))));
        }
        Ok(n)
    }

    fn px(&self, n: usize) -> impl Iterator<Item = [f32; 3]> + '_ {
        self.rgb.as_chunks::<3>().0.iter().take(n).map(|c| c.map(clean))
    }
}

/// A sample made safe: NaN → 0, negatives → 0, infinities → a large finite value.
#[inline]
fn clean(v: f32) -> f32 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1e4) }
}

/// SMPTE ST 2084 inverse EOTF: absolute luminance (cd/m²) → PQ signal 0..1.
#[inline]
pub fn pq_encode(nits: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 4096.0 * 128.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 4096.0 * 32.0;
    const C3: f32 = 2392.0 / 4096.0 * 32.0;
    let l = (clean(nits) / 10_000.0).min(1.0);
    let p = l.powf(M1);
    ((C1 + C2 * p) / (1.0 + C3 * p)).powf(M2)
}

/// SMPTE ST 2084 EOTF: PQ signal 0..1 → cd/m².
#[inline]
pub fn pq_decode(e: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 4096.0 * 128.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 4096.0 * 32.0;
    const C3: f32 = 2392.0 / 4096.0 * 32.0;
    let p = clean(e).min(1.0).powf(1.0 / M2);
    10_000.0 * ((p - C1).max(0.0) / (C2 - C3 * p)).powf(1.0 / M1)
}

/// Content light levels (CEA-861.3): the brightest channel anywhere and its frame average, cd/m².
fn content_light(img: &HdrImage, n: usize) -> (u16, u16) {
    let (mut max, mut sum) = (0.0f32, 0.0f64);
    for c in img.px(n) {
        let m = c[0].max(c[1]).max(c[2]) * SDR_WHITE_NITS;
        max = max.max(m);
        sum += m as f64;
    }
    let avg = sum / n.max(1) as f64;
    (max.round().clamp(0.0, 10_000.0) as u16, avg.round().clamp(0.0, 10_000.0) as u16)
}

/// Encode a 10-bit PQ AVIF (4:4:4, BT.2020 non-constant-luminance YCbCr, full range).
/// `quality` 1..=100, `speed` 1 (slow) ..= 10. Returns [`Error::Encode`] on wasm32 or without the
/// `avif` feature.
pub fn encode_avif_hdr(img: &HdrImage, quality: u8, speed: u8, exif: Option<&[u8]>) -> Result<Vec<u8>> {
    let n = img.pixels()?;
    #[cfg(all(feature = "avif", not(target_arch = "wasm32")))]
    {
        use rav1e::prelude::*;
        let (w, h) = (img.width as usize, img.height as usize);
        let (cll, fall) = content_light(img, n);
        const KR: f32 = 0.2627;
        const KB: f32 = 0.0593;
        let ycc: Vec<[u16; 3]> = img
            .px(n)
            .map(|c| {
                let [r, g, b] = c.map(|v| pq_encode(v * SDR_WHITE_NITS) * 1023.0);
                let y = KR * r + (1.0 - KR - KB) * g + KB * b;
                let cb = (b - y) / (2.0 * (1.0 - KB)) + 512.0;
                let cr = (r - y) / (2.0 * (1.0 - KR)) + 512.0;
                [y, cb, cr].map(|v| v.round().clamp(0.0, 1023.0) as u16)
            })
            .collect();
        let primaries = match img.primaries {
            HdrPrimaries::Bt2020 => ColorPrimaries::BT2020,
            HdrPrimaries::DisplayP3 => ColorPrimaries::SMPTE432,
        };
        // ravif's quality → quantizer mapping, so HDR and SDR AVIFs of one quality look alike
        let q = (quality.clamp(1, 100) as f32) / 100.0;
        let x = if q >= 0.82 {
            (1.0 - q) * 2.6
        } else if q > 0.25 {
            q.mul_add(-0.5, 1.0 - 0.125)
        } else {
            1.0 - q
        };
        let quantizer = (x * 255.0).round().clamp(0.0, 255.0) as usize;
        let cfg = Config::new().with_encoder_config(EncoderConfig {
            width: w,
            height: h,
            bit_depth: 10,
            chroma_sampling: ChromaSampling::Cs444,
            pixel_range: PixelRange::Full,
            color_description: Some(ColorDescription {
                color_primaries: primaries,
                transfer_characteristics: TransferCharacteristics::SMPTE2084,
                matrix_coefficients: MatrixCoefficients::BT2020NCL,
            }),
            content_light: Some(ContentLight { max_content_light_level: cll, max_frame_average_light_level: fall }),
            still_picture: true,
            quantizer,
            min_quantizer: quantizer.min(255) as u8,
            speed_settings: SpeedSettings::from_preset(speed.clamp(1, 10)),
            ..EncoderConfig::default()
        });
        let e = |e: &dyn std::fmt::Display| Error::Encode(format!("AVIF: {e}"));
        let mut ctx: Context<u16> = cfg.new_context().map_err(|x| e(&x))?;
        let mut frame = ctx.new_frame();
        {
            let mut planes = frame.planes.iter_mut();
            let (Some(py), Some(pu), Some(pv)) = (planes.next(), planes.next(), planes.next()) else {
                return Err(Error::Encode("AVIF: encoder frame has no planes".into()));
            };
            let (mut y, mut u, mut v) = (py.mut_slice(Default::default()), pu.mut_slice(Default::default()), pv.mut_slice(Default::default()));
            let mut src = ycc.iter();
            for ((ry, ru), rv) in y.rows_iter_mut().zip(u.rows_iter_mut()).zip(v.rows_iter_mut()).take(h) {
                for ((dy, du), dv) in ry.iter_mut().zip(ru.iter_mut()).zip(rv.iter_mut()).take(w) {
                    let Some(p) = src.next() else { break };
                    (*dy, *du, *dv) = (p[0], p[1], p[2]);
                }
            }
        }
        ctx.send_frame(frame).map_err(|x| e(&x))?;
        ctx.flush();
        let mut av1 = Vec::new();
        loop {
            match ctx.receive_packet() {
                Ok(mut p) => {
                    if p.frame_type == FrameType::KEY {
                        av1.append(&mut p.data);
                    }
                }
                Err(EncoderStatus::Encoded | EncoderStatus::LimitReached) => break,
                Err(x) => return Err(e(&x)),
            }
        }
        let mut mux = avif_serialize::Aviffy::new();
        use avif_serialize::constants as k;
        mux.set_color_primaries(match img.primaries {
            HdrPrimaries::Bt2020 => k::ColorPrimaries::Bt2020,
            HdrPrimaries::DisplayP3 => k::ColorPrimaries::DisplayP3,
        })
        .set_transfer_characteristics(k::TransferCharacteristics::Smpte2084)
        .set_matrix_coefficients(k::MatrixCoefficients::Bt2020Ncl)
        .set_full_color_range(true)
        .set_content_light_level(cll, fall);
        if let Some(exif) = exif {
            mux.set_exif(exif.to_vec());
        }
        Ok(mux.to_vec(&av1, None, img.width, img.height, 10))
    }
    #[cfg(not(all(feature = "avif", not(target_arch = "wasm32"))))]
    {
        let _ = (n, quality, speed, exif, content_light(img, 0));
        Err(Error::Encode("AVIF encoding is not available in this build".into()))
    }
}

/// Encode a 16-bit PQ PNG tagged with `cICP` (and `cLLI`). EXIF and XMP are embedded; an ICC
/// profile is not (`cICP` describes the colour, and readers prefer it).
pub fn encode_png_hdr(img: &HdrImage, meta: &EncodeMeta) -> Result<Vec<u8>> {
    let n = img.pixels()?;
    let (cll, fall) = content_light(img, n);
    let bytes: Vec<u8> =
        img.px(n).flat_map(|c| c.map(|v| (pq_encode(v * SDR_WHITE_NITS) * 65535.0 + 0.5) as u16)).flat_map(u16::to_be_bytes).collect();
    let mut info = png::Info::with_size(img.width, img.height);
    info.color_type = png::ColorType::Rgb;
    info.bit_depth = png::BitDepth::Sixteen;
    info.exif_metadata = meta.exif.map(|b| b.to_vec().into());
    let mut out = Vec::new();
    let e = |e: png::EncodingError| Error::Encode(e.to_string());
    {
        let mut enc = png::Encoder::with_info(&mut out, info).map_err(e)?;
        enc.set_compression(png::Compression::Fast);
        if let Some(xmp) = meta.xmp {
            enc.add_itxt_chunk("XML:com.adobe.xmp".into(), xmp.into()).map_err(e)?;
        }
        let mut w = enc.write_header().map_err(e)?;
        // primaries, PQ, RGB (no matrix), full range
        w.write_chunk(png::chunk::ChunkType(*b"cICP"), &[img.primaries.code(), 16, 0, 1]).map_err(e)?;
        // cLLI: MaxCLL and MaxFALL in 0.0001 cd/m²
        let clli = [(cll as u32 * 10_000).to_be_bytes(), (fall as u32 * 10_000).to_be_bytes()].concat();
        w.write_chunk(png::chunk::ChunkType(*b"cLLI"), &clli).map_err(e)?;
        w.write_image_data(&bytes).map_err(e)?;
        w.finish().map_err(e)?;
    }
    Ok(out)
}

/// What a gain map JPEG needs besides the HDR image.
#[derive(Clone, Copy)]
pub struct GainMapInput<'a> {
    /// The SDR base: interleaved 8-bit RGB, encoded with `base_trc` (e.g. sRGB), same size and
    /// primaries as the HDR image.
    pub sdr: &'a [u8],
    /// Encoded base value (0..1) → linear, for the gain each pixel needs.
    pub base_decode: &'a dyn Fn(f32) -> f32,
    /// Luminance weights of the base's primaries.
    pub luma: [f32; 3],
    pub quality: u8,
}

/// The gain map's `hdrgm` parameters (log2 values, as written to XMP).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GainMapParams {
    pub min_log2: f32,
    pub max_log2: f32,
    pub offset: f32,
}

/// `hdrgm:OffsetSDR` / `OffsetHDR`: keeps the ratio finite near black.
const GAIN_OFFSET: f32 = 1.0 / 64.0;

/// Encode a JPEG with a gain map (Ultra HDR layout): the SDR base from `g.sdr` (with `meta`'s ICC,
/// EXIF and XMP; the gain map's container description is merged into the XMP) followed by a
/// half-resolution greyscale gain map recovering `hdr` from it.
pub fn encode_jpeg_gainmap(hdr: &HdrImage, g: &GainMapInput, meta: &EncodeMeta) -> Result<Vec<u8>> {
    let n = hdr.pixels()?;
    if g.sdr.len() < n.saturating_mul(3) {
        return Err(Error::Encode("SDR base shorter than the HDR image".into()));
    }
    let (w, h) = (hdr.width as usize, hdr.height as usize);
    // per-pixel log2 gain from base to HDR (luminance), on a half-resolution grid
    let lum = |c: [f32; 3]| g.luma[0] * c[0] + g.luma[1] * c[1] + g.luma[2] * c[2];
    let (gw, gh) = (w.div_ceil(2), h.div_ceil(2));
    let mut gain = vec![0.0f32; gw * gh];
    let mut count = vec![0u8; gw * gh];
    for (i, c) in hdr.px(n).enumerate() {
        let (x, y) = (i % w, i / w);
        let Some(s) = g.sdr.get(i * 3..i * 3 + 3) else { break };
        let sdr = [s[0], s[1], s[2]].map(|v| (g.base_decode)(v as f32 / 255.0));
        let k = ((lum(c) + GAIN_OFFSET) / (lum(sdr) + GAIN_OFFSET)).log2();
        let j = (y / 2) * gw + x / 2;
        if let (Some(a), Some(m)) = (gain.get_mut(j), count.get_mut(j)) {
            *a += if k.is_finite() { k } else { 0.0 };
            *m += 1;
        }
    }
    for (a, m) in gain.iter_mut().zip(&count) {
        *a /= (*m).max(1) as f32;
    }
    let lo = gain.iter().copied().fold(f32::INFINITY, f32::min).min(0.0);
    let hi = gain.iter().copied().fold(f32::NEG_INFINITY, f32::max).max(lo + 1.0 / 64.0);
    let params = GainMapParams { min_log2: lo, max_log2: hi, offset: GAIN_OFFSET };
    let map: Vec<u8> = gain.iter().map(|k| (((k - lo) / (hi - lo)).clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect();
    let gm_xmp = gain_map_xmp(&params);
    let gm_meta = EncodeMeta { xmp: Some(&gm_xmp), ..Default::default() };
    let gain_jpeg = encode_jpeg(&EncodeImage::new(gw as u32, gh as u32, 1, Samples::U8(&map)), g.quality, ChromaSubsampling::S444, &gm_meta)?;
    let gain_jpeg = with_segment(&gain_jpeg, 0xE2, &iso_metadata(&params))?;

    let base_xmp = primary_xmp(meta.xmp, gain_jpeg.len());
    let base_meta = EncodeMeta { xmp: Some(&base_xmp), ..*meta };
    let base = encode_jpeg(&EncodeImage::new(hdr.width, hdr.height, 3, Samples::U8(g.sdr)), g.quality, ChromaSubsampling::S444, &base_meta)?;
    let base = with_segment(&base, 0xE2, &iso_version())?;
    let base = with_mpf(&base, gain_jpeg.len())?;
    Ok([base, gain_jpeg].concat())
}

/// The ISO 21496-1 APP2 identifier.
const ISO_URN: &[u8] = b"urn:iso:std:iso:ts:21496:-1\0";

/// Denominator of the ISO 21496-1 fractions we write.
const ISO_DENOM: u32 = 1_000_000;

/// The primary image's ISO 21496-1 segment: the identifier and the versions (minimum version
/// a reader needs, writer version: both 0), announcing a gain map.
fn iso_version() -> Vec<u8> {
    [ISO_URN, &0u16.to_be_bytes(), &0u16.to_be_bytes()].concat()
}

/// The gain map image's ISO 21496-1 metadata (big-endian): versions, flags (one channel, the
/// base image's colour space), then each value as its own numerator / denominator pair (the
/// spec's optional common-denominator form is not read by every decoder: Skia, so Chromium,
/// reads only this one): the base and alternate HDR headroom (log2; the base is SDR), then the
/// channel's gain map min and max (log2), gamma and the base / alternate offsets — what `hdrgm`
/// says in XMP.
fn iso_metadata(p: &GainMapParams) -> Vec<u8> {
    const USE_BASE_COLOUR_SPACE: u8 = 1 << 6;
    let num = |v: f32| (f64::from(v) * f64::from(ISO_DENOM)).round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    let signed = |v: f32| [num(v).to_be_bytes(), ISO_DENOM.to_be_bytes()].concat();
    let positive = |v: f32| [(num(v.max(0.0)).max(0) as u32).to_be_bytes(), ISO_DENOM.to_be_bytes()].concat();
    let mut out = iso_version();
    out.push(USE_BASE_COLOUR_SPACE);
    out.extend(positive(0.0)); // base HDR headroom: an SDR base
    out.extend(positive(p.max_log2)); // alternate (HDR) headroom
    out.extend(signed(p.min_log2));
    out.extend(signed(p.max_log2));
    out.extend(positive(1.0)); // gamma
    out.extend(signed(p.offset));
    out.extend(signed(p.offset));
    out
}

/// The gain map image's XMP: its `hdrgm` parameters.
fn gain_map_xmp(p: &GainMapParams) -> String {
    format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/" hdrgm:Version="1.0" hdrgm:GainMapMin="{lo}" hdrgm:GainMapMax="{hi}" hdrgm:Gamma="1" hdrgm:OffsetSDR="{o}" hdrgm:OffsetHDR="{o}" hdrgm:HDRCapacityMin="0" hdrgm:HDRCapacityMax="{cap}" hdrgm:BaseRenditionIsHDR="False"/></rdf:RDF></x:xmpmeta>"#,
        lo = p.min_log2,
        hi = p.max_log2,
        o = p.offset,
        cap = p.max_log2.max(0.0),
    )
}

/// The base image's XMP: `existing` (the export's metadata, if any) with the gain map container
/// description added (`hdrgm:Version` and the `Container:Directory` naming the gain map's length).
fn primary_xmp(existing: Option<&str>, gain_len: usize) -> String {
    let desc = format!(
        r#"<rdf:Description rdf:about="" xmlns:Container="http://ns.google.com/photos/1.0/container/" xmlns:Item="http://ns.google.com/photos/1.0/container/item/" xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/" hdrgm:Version="1.0"><Container:Directory><rdf:Seq><rdf:li rdf:parseType="Resource"><Container:Item Item:Semantic="Primary" Item:Mime="image/jpeg"/></rdf:li><rdf:li rdf:parseType="Resource"><Container:Item Item:Semantic="GainMap" Item:Mime="image/jpeg" Item:Length="{gain_len}"/></rdf:li></rdf:Seq></Container:Directory></rdf:Description>"#
    );
    match existing.and_then(|x| x.rfind("</rdf:RDF>").map(|i| (x, i))) {
        Some((x, i)) => format!("{}{desc}{}", x.get(..i).unwrap_or(""), x.get(i..).unwrap_or("")),
        None => format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">{desc}</rdf:RDF></x:xmpmeta>"#
        ),
    }
}

/// Where new APPn segments go in `jpeg`: after SOI and the APPn segments the encoder wrote.
fn after_app_segments(jpeg: &[u8]) -> Result<usize> {
    if jpeg.get(..2) != Some(&[0xFF, 0xD8]) {
        return Err(Error::Encode("not a JPEG".into()));
    }
    let mut at = 2usize;
    while let (Some(&0xFF), Some(&m)) = (jpeg.get(at), jpeg.get(at + 1)) {
        if !(0xE0..=0xEF).contains(&m) {
            break;
        }
        let len =
            jpeg.get(at + 2..at + 4).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize).ok_or_else(|| Error::Encode("truncated JPEG".into()))?;
        at = at.checked_add(2 + len).filter(|e| *e <= jpeg.len()).ok_or_else(|| Error::Encode("truncated JPEG".into()))?;
    }
    Ok(at)
}

/// `jpeg` with an APPn segment (`marker` 0xE0..=0xEF, payload `body`) after its APPn segments.
fn with_segment(jpeg: &[u8], marker: u8, body: &[u8]) -> Result<Vec<u8>> {
    let at = after_app_segments(jpeg)?;
    let len = u16::try_from(body.len() + 2).map_err(|_| Error::Encode("APP segment too large".into()))?;
    Ok([jpeg.get(..at).unwrap_or_default(), &[0xFF, marker], &len.to_be_bytes(), body, jpeg.get(at..).unwrap_or_default()].concat())
}

/// `jpeg` with a Multi-Picture Format index (APP2 `MPF`) inserted after its leading APPn
/// segments: two images, this one (primary) and a `second_len`-byte one appended right after it.
fn with_mpf(jpeg: &[u8], second_len: usize) -> Result<Vec<u8>> {
    let at = after_app_segments(jpeg)?;
    // APP2: "MPF\0", big-endian TIFF header, one IFD (version, count, entries), two 16-byte entries
    let mut body: Vec<u8> = Vec::with_capacity(86);
    body.extend_from_slice(b"MPF\0");
    body.extend_from_slice(b"MM\0\x2a\0\0\0\x08");
    body.extend_from_slice(&3u16.to_be_bytes());
    let entry =
        |tag: u16, ty: u16, count: u32, value: [u8; 4]| [tag.to_be_bytes().as_slice(), &ty.to_be_bytes(), &count.to_be_bytes(), &value].concat();
    body.extend(entry(0xB000, 7, 4, *b"0100"));
    body.extend(entry(0xB001, 4, 1, 2u32.to_be_bytes()));
    body.extend(entry(0xB002, 7, 32, 50u32.to_be_bytes())); // entries follow the IFD: 8 + 2 + 36 + 4
    body.extend_from_slice(&0u32.to_be_bytes());
    let seg_len = 2 + body.len() + 32;
    let total = jpeg.len() + 2 + seg_len;
    // offsets of later images count from the TIFF header (marker 2 + length 2 + "MPF\0" 4)
    let header_at = at + 8;
    let second_offset = u32::try_from(total - header_at).map_err(|_| Error::Encode("JPEG too large for MPF".into()))?;
    let (total32, second32) = (
        u32::try_from(total).map_err(|_| Error::Encode("JPEG too large for MPF".into()))?,
        u32::try_from(second_len).map_err(|_| Error::Encode("gain map too large for MPF".into()))?,
    );
    // primary: JPEG baseline primary image (type 0x030000), offset 0; the gain map: offset after it
    body.extend([0x0003_0000u32.to_be_bytes(), total32.to_be_bytes(), 0u32.to_be_bytes()].concat());
    body.extend_from_slice(&[0, 0, 0, 0]);
    body.extend([0u32.to_be_bytes(), second32.to_be_bytes(), second_offset.to_be_bytes()].concat());
    body.extend_from_slice(&[0, 0, 0, 0]);
    let seg_len16 = u16::try_from(seg_len).map_err(|_| Error::Encode("MPF segment too large".into()))?;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(jpeg.get(..at).unwrap_or_default());
    out.extend_from_slice(&[0xFF, 0xE2]);
    out.extend_from_slice(&seg_len16.to_be_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(jpeg.get(at..).unwrap_or_default());
    debug_assert_eq!(out.len(), total);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: u32, h: u32) -> Vec<f32> {
        (0..w * h)
            .flat_map(|i| {
                let v = 0.01 * 1.12f32.powi((i % w) as i32);
                [v, v * 0.8, v * 0.6]
            })
            .collect()
    }

    #[test]
    fn pq_round_trips_and_pins_known_points() {
        assert!(pq_encode(0.0) < 1e-6);
        assert!((pq_encode(10_000.0) - 1.0).abs() < 1e-5);
        assert!((pq_encode(100.0) - 0.508).abs() < 0.002, "{}", pq_encode(100.0));
        assert!((pq_encode(203.0) - 0.58).abs() < 0.005, "{}", pq_encode(203.0));
        for nits in [0.1, 1.0, 50.0, 203.0, 1000.0, 3248.0] {
            assert!((pq_decode(pq_encode(nits)) - nits).abs() / nits < 1e-3, "{nits}");
        }
        assert_eq!(pq_encode(f32::NAN), pq_encode(0.0));
        assert!(pq_encode(f32::INFINITY) <= 1.0);
    }

    #[test]
    fn png_is_16_bit_pq_with_cicp() {
        let rgb = ramp(64, 4);
        let img = HdrImage { width: 64, height: 4, rgb: &rgb, primaries: HdrPrimaries::Bt2020 };
        let bytes = encode_png_hdr(&img, &EncodeMeta::default()).unwrap();
        let dec = png::Decoder::new(std::io::Cursor::new(bytes.as_slice()));
        let mut r = dec.read_info().unwrap();
        let cicp = r.info().coding_independent_code_points.unwrap();
        assert_eq!((cicp.color_primaries, cicp.transfer_function), (9, 16));
        let mut buf = vec![0; r.output_buffer_size().unwrap()];
        let f = r.next_frame(&mut buf).unwrap();
        assert_eq!(f.bit_depth, png::BitDepth::Sixteen);
        // the brightest red sample decodes back to its luminance
        let last = (63 * 3) * 2;
        let e = u16::from_be_bytes([buf[last], buf[last + 1]]) as f32 / 65535.0;
        let want = rgb[63 * 3] * SDR_WHITE_NITS;
        assert!((pq_decode(e) - want).abs() / want < 0.01, "{} vs {want}", pq_decode(e));
        assert!(bytes.windows(4).any(|w| w == b"cLLI"));
    }

    #[test]
    fn avif_is_tagged_pq_bt2020() {
        let rgb = ramp(32, 16);
        let img = HdrImage { width: 32, height: 16, rgb: &rgb, primaries: HdrPrimaries::Bt2020 };
        let bytes = encode_avif_hdr(&img, 70, 10, None);
        if !cfg!(feature = "avif") {
            assert!(bytes.is_err());
            return;
        }
        let bytes = bytes.unwrap();
        let at = bytes.windows(8).position(|w| w == b"colrnclx").expect("colr nclx box");
        let b = &bytes[at + 8..at + 15];
        assert_eq!((u16::from_be_bytes([b[0], b[1]]), u16::from_be_bytes([b[2], b[3]]), u16::from_be_bytes([b[4], b[5]])), (9, 16, 9));
        assert_eq!(b[6] & 0x80, 0x80, "full range");
        assert!(bytes.windows(4).any(|w| w == b"clli"));
        let p3 = encode_avif_hdr(&HdrImage { primaries: HdrPrimaries::DisplayP3, ..img }, 70, 10, None).unwrap();
        let at = p3.windows(8).position(|w| w == b"colrnclx").unwrap();
        assert_eq!(u16::from_be_bytes([p3[at + 8], p3[at + 9]]), 12);
    }

    #[test]
    fn gain_map_jpeg_recovers_the_hdr_image() {
        let (w, h) = (64u32, 8u32);
        let rgb = ramp(w, h);
        let img = HdrImage { width: w, height: h, rgb: &rgb, primaries: HdrPrimaries::Bt2020 };
        use lightcraft_color::transfer::{linear_to_srgb, srgb_to_linear};
        let sdr: Vec<u8> = rgb.iter().map(|v| (linear_to_srgb(v.min(1.0)) * 255.0 + 0.5) as u8).collect();
        let g = GainMapInput { sdr: &sdr, base_decode: &srgb_to_linear, luma: [0.2126, 0.7152, 0.0722], quality: 95 };
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/"/></rdf:RDF></x:xmpmeta>"#;
        let bytes = encode_jpeg_gainmap(&img, &g, &EncodeMeta { xmp: Some(xmp), ..Default::default() }).unwrap();
        // the MPF index points at the gain map JPEG appended after the base
        let mpf = bytes.windows(4).position(|w| w == b"MPF\0").unwrap();
        let header = mpf + 4;
        let e2 = header + 50 + 16;
        let rd = |o: usize| u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]) as usize;
        let (base_len, gm_len, gm_off) = (rd(header + 50 + 4), rd(e2 + 4), rd(e2 + 8));
        assert_eq!(base_len + gm_len, bytes.len());
        assert_eq!(header + gm_off, base_len);
        assert_eq!(&bytes[base_len..base_len + 2], &[0xFF, 0xD8]);
        let base = String::from_utf8_lossy(&bytes[..base_len]);
        assert!(base.contains(r#"hdrgm:Version="1.0""#) && base.contains(&format!(r#"Item:Length="{gm_len}""#)) && base.contains("xmlns:dc"));
        let gm = String::from_utf8_lossy(&bytes[base_len..]);
        let max: f32 = gm.split(r#"hdrgm:GainMapMax=""#).nth(1).unwrap().split('"').next().unwrap().parse().unwrap();
        let brightest = rgb[(w as usize - 1) * 3..(w as usize) * 3].iter().map(|v| v * 0.3).sum::<f32>();
        assert!(max > 1.0 && max < (brightest / 0.3 * 2.0).log2() + 1.0, "{max}");
        // ISO 21496-1: the primary announces it (versions only), the gain map carries the metadata
        let urn = |b: &[u8]| b.windows(ISO_URN.len()).position(|w| w == ISO_URN);
        let p = urn(&bytes[..base_len]).expect("ISO 21496-1 in the primary");
        assert_eq!(&bytes[p + ISO_URN.len()..p + ISO_URN.len() + 4], &[0, 0, 0, 0], "versions 0 / 0");
        let seg_len = u16::from_be_bytes([bytes[p - 2], bytes[p - 1]]) as usize;
        assert_eq!(seg_len, 2 + ISO_URN.len() + 4, "versions only");
        let g0 = base_len + urn(&bytes[base_len..]).expect("ISO 21496-1 in the gain map") + ISO_URN.len();
        // read back as Skia (Chromium) does: every value a numerator / denominator pair
        let m = &bytes[g0..];
        let be32 = |o: usize| u32::from_be_bytes([m[o], m[o + 1], m[o + 2], m[o + 3]]);
        let frac = |o: usize, signed: bool| {
            let d = be32(o + 4);
            assert_ne!(d, 0, "denominators are non-zero");
            (if signed { be32(o) as i32 as f64 } else { be32(o) as f64 } / d as f64) as f32
        };
        assert_eq!(&m[..4], &[0, 0, 0, 0]);
        assert_eq!(m[4], 0x40, "one channel, base colour space");
        assert_eq!(frac(5, false), 0.0, "SDR base");
        let alt = frac(13, false);
        let (gmin, gmax) = (frac(21, true), frac(29, true));
        assert!((alt - max).abs() < 1e-4 && (gmax - max).abs() < 1e-4 && gmin <= 0.0, "{alt} {gmin} {gmax} vs {max}");
        assert_eq!(frac(37, false), 1.0, "gamma");
        assert!((frac(45, true) - GAIN_OFFSET).abs() < 1e-5 && (frac(53, true) - GAIN_OFFSET).abs() < 1e-5);
        let seg = u16::from_be_bytes([bytes[g0 - ISO_URN.len() - 2], bytes[g0 - ISO_URN.len() - 1]]) as usize;
        assert_eq!(seg, 2 + ISO_URN.len() + 5 + 7 * 8, "nothing after the last value");
        // both images decode; applying the gain map to the base restores the HDR brightness
        let dec = |b: &[u8]| {
            let mut d = jpeg_decoder::Decoder::new(std::io::Cursor::new(b));
            let px = d.decode().unwrap();
            (d.info().unwrap().width as usize, px)
        };
        let ((bw, base_px), (gw, gm_px)) = (dec(&bytes[..base_len]), dec(&bytes[base_len..]));
        assert_eq!((bw, gw), (64, 32));
        let x = w as usize - 2;
        let sdr_y = srgb_to_linear(base_px[(4 * bw + x) * 3 + 1] as f32 / 255.0);
        let k = gm_px[2 * gw + x / 2] as f32 / 255.0;
        let gain = (k * max).exp2();
        let restored = (sdr_y + GAIN_OFFSET) * gain - GAIN_OFFSET;
        let want = rgb[x * 3 + 1];
        assert!((restored - want).abs() / want < 0.15, "{restored} vs {want}");
    }

    #[test]
    fn hostile_inputs_are_errors_not_panics() {
        let img = HdrImage { width: 4, height: 4, rgb: &[1.0; 3], primaries: HdrPrimaries::Bt2020 };
        assert!(encode_png_hdr(&img, &EncodeMeta::default()).is_err());
        assert!(encode_avif_hdr(&img, 50, 10, None).is_err());
        let g = GainMapInput { sdr: &[], base_decode: &lightcraft_color::transfer::srgb_to_linear, luma: [0.3, 0.6, 0.1], quality: 90 };
        let ok = vec![1.0; 48];
        assert!(encode_jpeg_gainmap(&HdrImage { rgb: &ok, ..img }, &g, &EncodeMeta::default()).is_err(), "short SDR base");
        assert!(with_mpf(b"nope", 10).is_err());
        assert!(with_mpf(&[0xFF, 0xD8, 0xFF, 0xE0, 0xFF, 0xFF], 10).is_err(), "truncated APP0");
        let nan = vec![f32::NAN; 48];
        assert!(encode_png_hdr(&HdrImage { rgb: &nan, ..img }, &EncodeMeta::default()).is_ok());
    }
}
