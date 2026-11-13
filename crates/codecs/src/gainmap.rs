//! Gain map JPEGs (ISO 21496-1): an SDR base image plus a gain map that a decoder uses to rebuild
//! the HDR rendition, so the file looks right everywhere and lights up on HDR displays.
//!
//! The metadata formats and the arithmetic ([`compute`], [`apply`], [`GainMapMeta`]) are
//! `craft-gainmap`'s, re-exported here. This module lays the two images out in a JPEG:
//!
//! - [`encode_jpeg`] writes a gain map JPEG: the base with an MPF index (CIPA DC-007), the ISO
//!   21496-1 marker and Adobe/Google container XMP, followed by the gain map JPEG carrying the ISO
//!   21496-1 metadata, the Adobe `hdrgm` XMP (Android, Chrome) and Apple's description of the same
//!   map (what survives an iMessage send).
//! - [`read_jpeg`] finds and decodes the gain map of a gain map JPEG (ours, cameras, Lightroom),
//!   from its ISO 21496-1 metadata or, failing that, its `hdrgm` XMP.

pub use craft_gainmap::{GainImage, GainMapMeta, GainMapOptions, ISO_URN, apply, compute};

use crate::encode::{EncodeImage, EncodeMeta, Samples, jpeg_app_segments, xmp_segment};
use crate::{ChromaSubsampling, Error, Format, Result, jpeg, jpeg_par};

/// Offsets within the MPF payload (from "MPF\0"): the endian field, then the index IFD (3 tags).
const MPF_ENTRIES_FROM_ENDIAN: usize = 8 + 2 + 3 * 12 + 4;

/// The primary image's MPF payload, entry sizes and offset to be patched once the file exists.
fn mpf_placeholder() -> Vec<u8> {
    let mut p = b"MPF\0MM".to_vec();
    p.extend_from_slice(&42u16.to_be_bytes());
    p.extend_from_slice(&8u32.to_be_bytes()); // first IFD
    p.extend_from_slice(&3u16.to_be_bytes());
    let tag = |p: &mut Vec<u8>, tag: u16, ty: u16, count: u32, value: [u8; 4]| {
        p.extend_from_slice(&tag.to_be_bytes());
        p.extend_from_slice(&ty.to_be_bytes());
        p.extend_from_slice(&count.to_be_bytes());
        p.extend_from_slice(&value);
    };
    tag(&mut p, 0xB000, 7, 4, *b"0100"); // MPFVersion
    tag(&mut p, 0xB001, 4, 1, 2u32.to_be_bytes()); // NumberOfImages
    tag(&mut p, 0xB002, 7, 32, (MPF_ENTRIES_FROM_ENDIAN as u32).to_be_bytes()); // MPEntry
    p.extend_from_slice(&0u32.to_be_bytes()); // no next IFD
    // entry 0: baseline primary (0x030000) flagged as the representative image (bit 29)
    for attr in [0x2003_0000u32, jpeg::MPF_GAIN_MAP] {
        p.extend_from_slice(&attr.to_be_bytes());
        p.extend_from_slice(&[0; 12]); // size, offset, two dependent-image entries
    }
    p
}

/// Encode a gain map JPEG: `base` (8-bit SDR, 3 or 4 channels, colour-managed by `meta.icc`) and
/// its gain map, both at `quality`. `meta.xmp` is kept, with the gain map container added.
pub fn encode_jpeg(base: &EncodeImage, gain: &GainImage, m: &GainMapMeta, quality: u8, sub: ChromaSubsampling, meta: &EncodeMeta) -> Result<Vec<u8>> {
    let Samples::U8(data) = base.samples else {
        return Err(Error::Encode("gain map JPEG needs an 8-bit base image".into()));
    };
    let (w, h) = (base.width as usize, base.height as usize);
    if !matches!(base.channels, 3 | 4) || data.len() < w * h * base.channels as usize {
        return Err(Error::Encode("gain map JPEG needs an RGB base image".into()));
    }
    if w == 0 || h == 0 || w > u16::MAX as usize || h > u16::MAX as usize {
        return Err(Error::Encode("JPEG dimensions are limited to 65535".into()));
    }
    if !matches!(gain.channels, 1 | 3) || gain.width == 0 || gain.height == 0 || gain.data.len() < gain.width * gain.height * gain.channels {
        return Err(Error::Encode("gain map image is malformed".into()));
    }
    // (jpeg_par::encode gives an empty JPEG past this, which would be appended as a size-0 gain map)
    if gain.width > u16::MAX as usize || gain.height > u16::MAX as usize {
        return Err(Error::Encode("JPEG dimensions are limited to 65535".into()));
    }
    if !m.is_valid() || m.multichannel != (gain.channels == 3) {
        return Err(Error::Encode("gain map metadata doesn't match the map".into()));
    }
    // jpeg_par writes 4:4:4 or 4:2:0
    let sub = if sub == ChromaSubsampling::S422 { ChromaSubsampling::S420 } else { sub };

    let gain_segs = vec![(0xE1, xmp_segment(&m.to_gain_map_xmp())?), (0xE2, m.to_iso())];
    let gain_jpeg = jpeg_par::encode(&gain.data, gain.width, gain.height, gain.channels, quality, ChromaSubsampling::S444, &gain_segs);

    let xmp = craft_gainmap::primary_xmp(meta.xmp, gain_jpeg.len());
    let mut segs = jpeg_app_segments(&EncodeMeta { xmp: Some(&xmp), ..*meta })?;
    segs.push((0xE2, craft_gainmap::primary_iso_marker()));
    segs.push((0xE2, mpf_placeholder()));
    let mut primary = jpeg_par::encode(data, w, h, base.channels as usize, quality, sub, &segs);

    // patch the MPF index: sizes, and the gain map's offset from the MPF endian field
    let markers = jpeg::parse_markers(&primary).ok_or_else(|| Error::Encode("gain map JPEG: primary image unreadable".into()))?;
    let endian = markers.mpf.as_ref().map(|m| m.0).ok_or_else(|| Error::Encode("gain map JPEG: MPF segment missing".into()))?;
    let entries = endian + MPF_ENTRIES_FROM_ENDIAN;
    let (plen, glen) = (primary.len(), gain_jpeg.len());
    let fields = [(entries + 4, plen), (entries + 8, 0), (entries + 20, glen), (entries + 24, plen - endian)];
    for (at, v) in fields {
        let v = u32::try_from(v).map_err(|_| Error::Encode("gain map JPEG larger than 4 GiB".into()))?;
        let slot = primary.get_mut(at..at + 4).ok_or_else(|| Error::Encode("gain map JPEG: truncated MPF segment".into()))?;
        slot.copy_from_slice(&v.to_be_bytes());
    }
    primary.extend_from_slice(&gain_jpeg);
    Ok(primary)
}

/// The gain map of a gain map JPEG.
#[derive(Clone, Debug, PartialEq)]
pub struct JpegGainMap {
    pub gain: GainImage,
    pub meta: GainMapMeta,
}

/// Find and decode the gain map of a JPEG (`None` when it has none or it's unusable). The
/// primary image is decoded as usual ([`crate::decode`]); combine the two with [`apply`].
pub fn read_jpeg(bytes: &[u8]) -> Option<JpegGainMap> {
    let m = jpeg::parse_markers(bytes)?;
    let marked = m.iso_gainmap.is_some() || m.xmp.as_deref().is_some_and(|x| x.contains("hdrgm:Version"));
    if !marked {
        return None;
    }
    for e in jpeg::mpf_images(&m) {
        let Some(data) = bytes.get(e.offset..e.offset.saturating_add(e.len)) else { continue };
        let Some(gm) = jpeg::parse_markers(data) else { continue };
        let meta = gm.iso_gainmap.as_deref().and_then(GainMapMeta::from_iso).or_else(|| gm.xmp.as_deref().and_then(GainMapMeta::from_hdrgm_xmp));
        let Some(meta) = meta else { continue };
        let Ok((w, h, ch, codes)) = jpeg::decode_codes8(data) else { continue };
        if ch == 3 && !meta.multichannel {
            // a single-channel map stored as RGB: use its first channel
            let g: Vec<u8> = codes.as_chunks::<3>().0.iter().map(|c| c[0]).collect();
            return Some(JpegGainMap { gain: GainImage { width: w, height: h, channels: 1, data: g }, meta });
        }
        let mut meta = meta;
        if ch == 1 && meta.multichannel {
            meta.multichannel = false;
            meta.fill_from_first();
        }
        return Some(JpegGainMap { gain: GainImage { width: w, height: h, channels: ch, data: codes }, meta });
    }
    None
}

/// Whether `bytes` is a gain map JPEG with a usable gain map (cheap: no pixel decode).
pub fn is_gain_map_jpeg(bytes: &[u8]) -> bool {
    crate::sniff(bytes) == Some(Format::Jpeg)
        && jpeg::parse_markers(bytes).is_some_and(|m| {
            (m.iso_gainmap.is_some() || m.xmp.as_deref().is_some_and(|x| x.contains("hdrgm:Version")))
                && jpeg::mpf_images(&m)
                    .iter()
                    .any(|e| bytes.get(e.offset..e.offset.saturating_add(e.len)).and_then(jpeg::parse_markers).is_some_and(|g| g.is_gain_map()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renditions(w: usize, h: usize) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
        // a ramp: SDR rolls off at 1, HDR keeps going to 6
        let hdr: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                let t = (i % w) as f32 / (w - 1) as f32;
                let v = 0.01 + 6.0 * t * t;
                [v, v * 0.8, v * 0.6]
            })
            .collect();
        // a luminance tone map (one scale per pixel, as LightCraft's renders do), so a single
        // luminance gain can rebuild it exactly
        let sdr = hdr
            .iter()
            .map(|p| {
                let y = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
                let k = 1.0 / (1.0 + y * 0.5);
                p.map(|v| v * k)
            })
            .collect();
        (sdr, hdr)
    }

    #[test]
    fn a_gain_map_wider_than_a_jpeg_allows_is_refused() {
        let base = [0u8; 3];
        let gain = GainImage { width: 65_536, height: 1, channels: 1, data: vec![0; 65_536] };
        let r = encode_jpeg(
            &EncodeImage::new(1, 1, 3, Samples::U8(&base)),
            &gain,
            &GainMapMeta::default(),
            90,
            ChromaSubsampling::S444,
            &EncodeMeta::default(),
        );
        assert!(matches!(r, Err(Error::Encode(m)) if m.contains("65535")));
    }

    #[test]
    fn jpeg_round_trip_and_container_structure() {
        let (w, h) = (96, 64);
        let (sdr, hdr) = renditions(w, h);
        let (g, m) = compute(&sdr, &hdr, w, h, [0.2126, 0.7152, 0.0722], &GainMapOptions::default()).unwrap();
        let base: Vec<u8> =
            sdr.iter().flat_map(|p| p.map(|v| (lightcraft_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8)).collect();
        let user_xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/"/></rdf:RDF></x:xmpmeta>"#;
        let meta = EncodeMeta { xmp: Some(user_xmp), ..EncodeMeta::default() };
        let file = encode_jpeg(&EncodeImage::new(w as u32, h as u32, 3, Samples::U8(&base)), &g, &m, 90, ChromaSubsampling::S444, &meta).unwrap();
        assert!(is_gain_map_jpeg(&file));
        // the primary decodes as an ordinary JPEG of the right size, and keeps the user's XMP
        let d = crate::decode(&file, crate::DecodeOptions::default()).unwrap();
        assert_eq!((d.width, d.height), (w as u32, h as u32));
        let x = d.xmp.unwrap();
        assert!(x.contains("xmlns:dc=") && x.contains("hdrgm:Version") && x.contains("Item:Semantic=\"GainMap\""));
        // the MPF index points at the gain map, typed as one
        let pm = jpeg::parse_markers(&file).unwrap();
        let e = jpeg::mpf_images(&pm);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].kind, jpeg::MPF_GAIN_MAP);
        assert_eq!(e[0].offset + e[0].len, file.len());
        // and the gain map reads back
        let r = read_jpeg(&file).unwrap();
        assert_eq!((r.gain.width, r.gain.height, r.gain.channels), (48, 32, 1));
        assert!((r.meta.max[0] - m.max[0]).abs() < 1e-5);
        // the gain map is never a thumbnail
        if let Some(t) = jpeg::embedded_thumbnail(&file, 32, 16) {
            assert_ne!(t.source, crate::ThumbnailSource::MpfPreview);
        }
    }

    #[test]
    fn garbage_never_panics() {
        assert!(read_jpeg(b"").is_none());
        assert!(read_jpeg(&[0xFF, 0xD8, 0xFF, 0xE2, 0x00, 0x04, 0, 0]).is_none());
        assert!(!is_gain_map_jpeg(&[0xFF, 0xD8]));
        let mut b = vec![0xFF, 0xD8, 0xFF, 0xE2, 0, 40];
        b.extend_from_slice(ISO_URN);
        b.extend_from_slice(&[0, 0, 0, 0, 0xFF, 1, 2, 3, 4]);
        assert!(read_jpeg(&b).is_none());
        let g = GainImage { width: 2, height: 2, channels: 1, data: vec![0; 3] };
        assert!(apply(&mut [[0.0; 3]; 4], 2, 2, &g, &GainMapMeta::default(), 1.0).is_err());
        assert!(compute(&[], &[], 0, 0, [0.3, 0.6, 0.1], &GainMapOptions::default()).is_err());
    }
}
