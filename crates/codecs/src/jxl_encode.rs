//! JPEG XL encode via `jixel` (pure Rust, BSD-3-Clause OR Apache-2.0; native targets with the `jxl`
//! feature: it spawns threads of its own). Lossy files are VarDCT in XYB, tagged with an enum colour
//! encoding the decoder renders back to; lossless files are Modular and keep their samples as they
//! are, tagged with the ICC profile when one is given. EXIF and XMP go in `Exif` / `xml ` boxes.

use crate::encode::{EncodeImage, EncodeMeta, HDR_REFERENCE_WHITE_NITS, Samples, pq_encode};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// Encoder effort: time spent searching for a smaller file at the same quality. jixel's slowest
/// search (about 20 % smaller again, five times slower) is not offered: its files are valid, but
/// jxl-oxide, our decoder, renders blocks of them wrongly, so the app could not read back its own
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
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&pixels, img.width as usize, img.height as usize, o, meta, None)))
            .unwrap_or_else(|_| Err(Error::Encode("JPEG XL encoder panicked".into())))
    }
    #[cfg(not(all(feature = "jxl", not(target_arch = "wasm32"))))]
    {
        let _ = (o, meta);
        Err(Error::Encode("JPEG XL encoding is not available in this build".into()))
    }
}

/// Encode an ISO 21496-1 gain-map JPEG XL: `base` (8-bit SDR) is an ordinary JPEG XL codestream in
/// a container, and a `jhgm` box next to it holds the gain map as libjxl defines the box
/// (`JxlGainMapBundle`): `jhgm_version` (0), the size and bytes of the ISO 21496-1 metadata
/// (without the URN that names the JPEG APP2 payload), the size and bytes of the colour encoding
/// of the alternate (HDR) image, the size and bytes of its ICC profile (none here), then the gain
/// map as a bare JPEG XL codestream (grayscale for a single-channel map). libjxl 0.13 reads and
/// preserves the box (`jxlinfo` lists it; it does not render HDR from it yet). Readers without
/// gain map support show the base.
///
/// The gain map is encoded by jixel at the same quality as the base, lossless when the base is.
/// Returns [`Error::Encode`] for a 16-bit or float base, the HDR colour [`JxlColour::Rec2020Pq`],
/// a map whose channel count disagrees with `m`, or unusable metadata.
pub fn encode_jxl_gain_map(
    base: &EncodeImage,
    gain: &crate::gainmap::GainImage,
    m: &crate::gainmap::GainMapMeta,
    o: &JxlOptions,
    meta: &EncodeMeta,
) -> Result<Vec<u8>> {
    base.validate()?;
    #[cfg(all(feature = "jxl", not(target_arch = "wasm32")))]
    {
        let gm = jixel_gain_map(gain, m, o)?;
        if o.colour == JxlColour::Rec2020Pq {
            return Err(Error::Encode("the base image of a gain map JPEG XL is an SDR image".into()));
        }
        let pixels = Pixels::from(base, o.colour)?;
        if !matches!(pixels, Pixels::U8 { .. }) {
            return Err(Error::Encode("a gain map JPEG XL needs an 8-bit base image".into()));
        }
        // jixel asserts internally; a panic there is an export error, not a crash
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&pixels, base.width as usize, base.height as usize, o, meta, Some(gm))))
            .unwrap_or_else(|_| Err(Error::Encode("JPEG XL encoder panicked".into())))
    }
    #[cfg(not(all(feature = "jxl", not(target_arch = "wasm32"))))]
    {
        let _ = (gain, m, o, meta);
        Err(Error::Encode("JPEG XL encoding is not available in this build".into()))
    }
}

/// jixel's description of the gain map.
#[cfg(all(feature = "jxl", not(target_arch = "wasm32")))]
fn jixel_gain_map(gain: &crate::gainmap::GainImage, m: &crate::gainmap::GainMapMeta, o: &JxlOptions) -> Result<jixel::GainMap> {
    use jixel::{ColorEncoding, GainMap, IsoGainMap, TransferFunction};
    if !m.is_valid() || m.multichannel != (gain.channels == 3) {
        return Err(Error::Encode("gain map metadata doesn't match the map".into()));
    }
    let n = gain
        .width
        .checked_mul(gain.height)
        .and_then(|n| n.checked_mul(gain.channels))
        .ok_or_else(|| Error::Encode("gain map image is too large".into()))?;
    if gain.width == 0 || gain.height == 0 || gain.data.len() < n {
        return Err(Error::Encode("gain map image is malformed".into()));
    }
    // the metadata's rationals use the same fixed denominator as the JPEG payload
    const D: u32 = 1_000_000;
    let unsigned = |v: f32| -> Result<u32> { Ok((f64::from(v.max(0.0)) * f64::from(D)).round().clamp(0.0, f64::from(u32::MAX)) as u32) };
    let signed = |v: f32| -> Result<i32> { Ok((f64::from(v) * f64::from(D)).round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32) };
    let (mut min_n, mut max_n, mut gamma_n, mut base_n, mut alt_n) = ([0i32; 3], [0i32; 3], [0u32; 3], [0i32; 3], [0i32; 3]);
    for c in 0..3 {
        // a single-channel map repeats its one value, as jixel's single-channel form requires
        let k = if m.multichannel { c } else { 0 };
        min_n[c] = signed(m.min[k])?;
        max_n[c] = signed(m.max[k])?;
        gamma_n[c] = unsigned(m.gamma[k])?;
        base_n[c] = signed(m.base_offset[k])?;
        alt_n[c] = signed(m.alternate_offset[k])?;
    }
    let iso = IsoGainMap {
        gain_map_min_n: min_n,
        gain_map_min_d: [D; 3],
        gain_map_max_n: max_n,
        gain_map_max_d: [D; 3],
        gain_map_gamma_n: gamma_n,
        gain_map_gamma_d: [D; 3],
        base_offset_n: base_n,
        base_offset_d: [D; 3],
        alternate_offset_n: alt_n,
        alternate_offset_d: [D; 3],
        base_hdr_headroom_n: unsigned(m.base_headroom)?,
        base_hdr_headroom_d: D,
        alternate_hdr_headroom_n: unsigned(m.alternate_headroom)?,
        alternate_hdr_headroom_d: D,
        backward_direction: false,
        use_base_color_space: m.use_base_colour_space,
    };
    // The alternate image is the base's linear light with the headroom the map adds, in the base
    // primaries (the gain applies in the base colour space).
    let alternate = ColorEncoding { transfer: TransferFunction::Linear, ..ColorEncoding::srgb() };
    let map = match gain.channels {
        3 => GainMap::rgb8(gain.data[..n].to_vec(), gain.width, gain.height, iso),
        _ => GainMap::gray8(gain.data[..n].to_vec(), gain.width, gain.height, iso),
    };
    let map = if o.lossless { map.with_lossless(true) } else { map.with_distance(jixel::distance_from_quality(f32::from(o.quality.clamp(1, 100)))) };
    Ok(map.with_alternate_color_encoding(alternate))
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
fn run(px: &Pixels, w: usize, h: usize, o: &JxlOptions, meta: &EncodeMeta, gain: Option<jixel::GainMap>) -> Result<Vec<u8>> {
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
    if let Some(gain) = gain {
        c = c.with_gain_map(gain);
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

#[cfg(all(test, feature = "jxl", not(target_arch = "wasm32")))]
mod gain_map_tests {
    use super::*;
    use crate::gainmap::{self, test_support::fixture};
    use crate::{DecodeOptions, Format, decode};

    fn build(w: usize, h: usize, o: &JxlOptions) -> Result<(Vec<u8>, Vec<f32>, Vec<u8>, gainmap::GainImage, gainmap::GainMapMeta)> {
        let (base, hdr, gain, gm) = fixture(w, h)?;
        let img = EncodeImage::new(w as u32, h as u32, 3, Samples::U8(&base));
        Ok((encode_jxl_gain_map(&img, &gain, &gm, o, &EncodeMeta::default())?, hdr, base, gain, gm))
    }

    #[test]
    fn the_gain_map_rebuilds_the_hdr_rendition() {
        let (w, h) = (64usize, 40usize);
        for o in [
            JxlOptions::default(),
            JxlOptions { lossless: true, ..JxlOptions::default() },
            JxlOptions { effort: JxlEffort::Fast, ..JxlOptions::default() },
        ] {
            let (file, _, base, gain, gm) = build(w, h, &o).unwrap();
            // an ordinary JPEG XL to every reader: the base image
            let d = decode(&file, DecodeOptions::default()).unwrap();
            assert_eq!((d.format, d.width as usize, d.height as usize), (Format::Jxl, w, h));
            let shown: Vec<u8> = d.to_srgb8().data.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
            let diff = shown.iter().zip(&base).map(|(&x, &y)| (f64::from(x) - f64::from(y)).abs()).sum::<f64>() / base.len() as f64;
            assert!(diff < 4.0, "{o:?}: base mean difference {diff}");
            // the gain map and its metadata come back
            let back = gainmap::read_jxl(&file).expect("the gain map is found");
            assert_eq!((back.gain.width, back.gain.height, back.gain.channels), (gain.width, gain.height, 1));
            assert!((back.meta.min[0] - gm.min[0]).abs() < 1e-4 && (back.meta.max[0] - gm.max[0]).abs() < 1e-4, "{o:?}");
            assert!((back.meta.alternate_headroom - gm.alternate_headroom).abs() < 1e-4, "{o:?}");
            let codes =
                back.gain.data.iter().zip(&gain.data).map(|(&x, &y)| (f64::from(x) - f64::from(y)).abs()).sum::<f64>() / gain.data.len() as f64;
            assert!(codes < 4.0, "{o:?}: gain map mean difference {codes}");
            // applied to the decoded base, the HDR rendition is rebuilt (linear, SDR white = 1)
            let mut lin: Vec<[f32; 3]> =
                decode(&file, DecodeOptions::default()).unwrap().to_working().data.iter().map(|p| [p[0], p[1], p[2]]).collect();
            gainmap::apply(&mut lin, w, h, &back.gain, &back.meta, 1.0).unwrap();
            let at = |y: usize, x: usize| lin[y * w + x][1];
            assert!((at(h / 8, 2) / 4.0 - 1.0).abs() < 0.15, "{o:?}: highlight {}", at(h / 8, 2));
            let mid = lcc_color::transfer::srgb_to_linear(0.5);
            assert!((at(h * 3 / 4, w / 2) / mid - 1.0).abs() < 0.08, "{o:?}: midtone {}", at(h * 3 / 4, w / 2));
            assert!(lin.iter().flatten().all(|v| v.is_finite()));
        }
    }

    #[test]
    fn bad_inputs_are_errors() {
        let (base, _, gain, gm) = fixture(16, 16).unwrap();
        let img = EncodeImage::new(16, 16, 3, Samples::U8(&base));
        let em = EncodeMeta::default();
        // the base of a gain map is SDR
        assert!(encode_jxl_gain_map(&img, &gain, &gm, &JxlOptions { colour: JxlColour::Rec2020Pq, ..JxlOptions::default() }, &em).is_err());
        // a 16-bit base
        let deep: Vec<u16> = base.iter().map(|&v| u16::from(v) * 257).collect();
        assert!(encode_jxl_gain_map(&EncodeImage::new(16, 16, 3, Samples::U16(&deep)), &gain, &gm, &JxlOptions::default(), &em).is_err());
        // the map and its metadata disagree
        let g = |width, height, channels, data: Vec<u8>| gainmap::GainImage { width, height, channels, data };
        let three = gainmap::GainMapMeta { multichannel: true, ..gm };
        assert!(encode_jxl_gain_map(&img, &g(16, 16, 3, vec![0; 768]), &three, &JxlOptions::default(), &em).is_ok());
        assert!(encode_jxl_gain_map(&img, &g(16, 16, 1, vec![0; 256]), &three, &JxlOptions::default(), &em).is_err());
        assert!(encode_jxl_gain_map(&img, &g(16, 16, 3, vec![0; 768]), &gm, &JxlOptions::default(), &em).is_err());
        // short or empty map, unusable metadata
        for map in [g(16, 16, 1, vec![0; 255]), g(0, 16, 1, vec![0; 256]), g(16, 16, 1, Vec::new())] {
            assert!(encode_jxl_gain_map(&img, &map, &gm, &JxlOptions::default(), &em).is_err());
        }
        let bad_m = gainmap::GainMapMeta { gamma: [0.0; 3], ..gm };
        assert!(encode_jxl_gain_map(&img, &gain, &bad_m, &JxlOptions::default(), &em).is_err());
    }

    #[test]
    fn hostile_files_are_errors_not_panics() {
        let (file, ..) = build(16, 16, &JxlOptions::default()).unwrap();
        let opts = DecodeOptions::default();
        for n in 0..file.len() {
            let _ = decode(&file[..n], opts);
            let _ = gainmap::read_jxl(&file[..n]);
        }
        for i in 0..file.len() {
            for x in [0x01u8, 0x80, 0xff] {
                let mut m = file.clone();
                m[i] ^= x;
                let _ = decode(&m, opts);
                let _ = gainmap::read_jxl(&m);
            }
        }
    }
}
