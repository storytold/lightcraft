//! HEIF / HEIC (the iPhone and Mac photo format), read-only.
//!
//! Decoding lives in the optional `dac-heif` crate (heic-rs, a pure-Rust HEVC
//! still-picture decoder), enabled by this crate's `heif` feature — off by default because HEVC
//! is patent-encumbered and whether a build carries an HEVC decoder is the distributor's call
//! (the same policy as the sibling PhotoCraft). Without the feature, HEIF files are still
//! detected and opening one is an [`Error::Unsupported`] error, never a panic.
//!
//! HEIF records orientation in the container (`irot`/`imir`, plus a `clap` crop), not in EXIF,
//! so the decoder applies them and `orientation` is reported as 1: the EXIF Orientation tag only
//! mirrors what the container says, and applying it as well would turn the photo twice.
//! The samples come back in the container's own primaries with its ICC profile (if any); like
//! every decoder here, the colour interpretation happens in [`crate::convert::finish`].

#[cfg(feature = "heif")]
use crate::convert::{Buf, Meta, Model, Raw, finish};
use crate::{DecodeOptions, Decoded, Error, Format, Result};

const F: Format = Format::Heif;

/// The reason given when this build has no HEIF decoder.
#[cfg(not(feature = "heif"))]
pub(crate) const NOT_IN_BUILD: &str = "HEIC/HEIF support isn't included in this build of LightCraft";

/// The reason a HEIF file that no build can open is refused (this crate decodes HEVC stills
/// only; the wrapper names the exact construct in its own errors).
#[cfg(feature = "heif")]
const NOT_DECODED: &str = "the image is not a decodable HEVC still (image sequence, overlay, alpha-less grid or AVIF-coded item)";

#[cfg(not(feature = "heif"))]
pub(crate) fn decode(_bytes: &[u8], _opts: &DecodeOptions) -> Result<Decoded> {
    Err(Error::Unsupported(F, NOT_IN_BUILD))
}

#[cfg(feature = "heif")]
fn err(e: dac_heif::Error) -> Error {
    match e {
        // This crate's Unsupported takes a fixed reason; the dynamic wording lives in the
        // dac-heif error itself.
        dac_heif::Error::Unsupported(_) => Error::Unsupported(F, NOT_DECODED),
        dac_heif::Error::Limit(m) | dac_heif::Error::Malformed(m) => Error::Malformed(F, m),
    }
}

#[cfg(feature = "heif")]
pub(crate) fn decode(bytes: &[u8], opts: &DecodeOptions) -> Result<Decoded> {
    // The container alone: the declared size is checked before any pixel is decoded.
    let info = dac_heif::probe(bytes).map_err(err)?;
    if (info.width as u64).saturating_mul(info.height as u64) > opts.max_pixels {
        return Err(Error::TooLarge(info.width as u64, info.height as u64));
    }
    let options = dac_heif::Options { max_pixels: opts.max_pixels, apply_transforms: true };
    let decoded = dac_heif::decode(bytes, &options).map_err(err)?;
    // The decoder re-reads the container; trust its output's shape, not the probe's.
    let buf = if decoded.sixteen_bit {
        let mut v = Vec::with_capacity(decoded.data.len() / 2);
        for pair in decoded.data.as_chunks::<2>().0 {
            v.push(u16::from_ne_bytes(*pair));
        }
        Buf::U16(v)
    } else {
        Buf::U8(decoded.data)
    };
    let raw = Raw {
        width: decoded.width as usize,
        height: decoded.height as usize,
        model: Model::Rgb,
        alpha: decoded.has_alpha,
        premultiplied: false,
        buf,
        bit_depth: if decoded.sixteen_bit { 16 } else { 8 },
    };
    let meta = Meta { icc: decoded.icc, exif: decoded.exif, xmp: decoded.xmp, orientation: Some(1), ..Default::default() };
    finish(F, raw, meta, (decoded.width, decoded.height), opts)
}

#[cfg(test)]
mod tests {
    use crate::{Format, sniff};

    /// The container header both builds recognise, whatever the feature says.
    const HEIC_HEADER: &[u8] = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic";

    #[test]
    fn heif_is_always_recognised() {
        assert_eq!(sniff(HEIC_HEADER), Some(Format::Heif));
    }

    #[test]
    fn can_decode_follows_the_feature() {
        assert_eq!(Format::Heif.can_decode(), cfg!(feature = "heif"));
    }

    #[cfg(not(feature = "heif"))]
    #[test]
    fn without_the_feature_heif_is_a_clear_unsupported_error() {
        let r = crate::decode(HEIC_HEADER, crate::DecodeOptions::default());
        assert!(matches!(&r, Err(crate::Error::Unsupported(Format::Heif, why)) if why.contains("isn't included in this build")), "{r:?}");
    }

    /// Found by PhotoCraft's `decode_heif` fuzz target: a malformed box makes heic-rs 0.1.1 slice
    /// out of range (`boxes.rs:130`). It must be an error, not a crash.
    #[cfg(feature = "heif")]
    const HEIC_RS_BOX_PANIC: [u8; 72] = [
        0x00, 0x00, 0x00, 0x24, 0x66, 0x74, 0x79, 0x70, 0x68, 0x65, 0x69, 0x63, 0x00, 0x00, 0x00, 0x00, 0x6d, 0x69, 0x66, 0x31, 0x4d, 0x69, 0x50,
        0x72, 0x6d, 0x69, 0x61, 0x66, 0x4d, 0x69, 0x48, 0x42, 0x72, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6d, 0x65, 0x74, 0x61, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x75, 0x75, 0x69, 0x64, 0x00, 0x00, 0x00, 0x00, 0x03, 0x08, 0x08, 0x08, 0x00, 0x03, 0x01, 0x03, 0x70,
        0x00, 0xa8, 0x00,
    ];

    #[cfg(feature = "heif")]
    #[test]
    fn a_heic_rs_panic_is_a_malformed_error() {
        let r = crate::decode(&HEIC_RS_BOX_PANIC, crate::DecodeOptions::default());
        assert!(matches!(&r, Err(crate::Error::Malformed(Format::Heif, _))), "{r:?}");
    }

    #[cfg(feature = "heif")]
    #[test]
    fn an_image_sequence_is_unsupported() {
        // An ftyp with no meta box: a video track, not a still.
        let r = crate::decode(b"\0\0\0\x18ftypmsf1\0\0\0\0msf1hevc", crate::DecodeOptions::default());
        assert!(matches!(&r, Err(crate::Error::Unsupported(Format::Heif, _))), "{r:?}");
    }
}
