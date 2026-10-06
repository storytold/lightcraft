//! JPEG XL tiles and strips (DNG 1.7 compression 52546), decoded with `jxl-oxide` (pure Rust).
//!
//! Per the DNG 1.7 specification a JPEG XL chunk holds N-bit unsigned integers (8 ≤ N ≤ 16) or 16-bit
//! floats, in 1 or 3 planes, as either a bare codestream or an ISO-BMFF container; jxl-oxide reads both.
//!
//! jxl-oxide renders every channel as `f32`: integer samples come out as `value / (2^bits − 1)` with
//! `bits` the codestream's own declared bit depth, so scaling back by that maximum and rounding gives the
//! stored integers exactly (an `f32` holds `v / 65535` to far better than half a code value) — lossless
//! tiles decode bit-exactly, whatever bit depth the encoder declared relative to the IFD's
//! `BitsPerSample`. Float samples are returned as decoded.
//!
//! We never request an output colour encoding, so jxl-oxide renders in the codestream's *original*
//! colour encoding: for lossless (non-XYB) data that is the identity, keeping the values in the raw IFD's
//! sample space. Lossy (XYB) tiles are converted from XYB back to that signalled encoding by jxl-oxide;
//! [INFERENCE] DNG writers signal a linear encoding matching the raw data there, so the result is the raw
//! sample space up to the codec's loss — not verified against a real lossy JXL DNG.

use crate::tiffraw::ChunkPx;
use crate::{RawError, Result};
use jxl_oxide::image::BitDepth;
use jxl_oxide::{AllocTracker, JxlImage, JxlThreadPool};

fn corrupt(e: impl std::fmt::Display) -> RawError {
    RawError::Corrupt(format!("JPEG XL tile: {e}"))
}

/// Parse a chunk's headers (no pixel decoding) and check them against the chunk it must fill:
/// nominal `cw × ch`, or the part of it inside the image `vw × vh` (edge tiles), `cpp` colour planes,
/// a sample type matching the IFD. Returns the decoder and the codestream's integer maximum (0 for float).
#[allow(clippy::too_many_arguments)]
fn open(src: &[u8], cw: usize, ch: usize, vw: usize, vh: usize, cpp: usize, bits: u32, float: bool) -> Result<(JxlImage, f32)> {
    if !matches!(cpp, 1 | 3) {
        return Err(RawError::Unsupported(format!("JPEG XL with {cpp} samples per pixel")));
    }
    if float && bits != 16 || !float && !(8..=16).contains(&bits) {
        return Err(RawError::Unsupported(format!("JPEG XL with {bits}-bit {} samples", if float { "float" } else { "integer" })));
    }
    // jxl-oxide's frame buffers: a few f32/i32 grids per channel; cap them well above that
    let limit = cw.saturating_mul(ch).saturating_mul(cpp).saturating_mul(64).max(1 << 22);
    let img = JxlImage::builder()
        .pool(JxlThreadPool::none()) // chunks are already decoded in parallel
        .alloc_tracker(AllocTracker::with_limit(limit))
        .read(std::io::Cursor::new(src))
        .map_err(corrupt)?;
    let header = img.image_header();
    let (jw, jh) = (img.width() as usize, img.height() as usize);
    if (jw, jh) != (cw, ch) && (jw, jh) != (vw, vh) {
        return Err(RawError::Corrupt(format!("JPEG XL tile is {jw}×{jh}, expected {cw}×{ch} (or {vw}×{vh} at the image edge)")));
    }
    if header.metadata.orientation != 1 {
        return Err(RawError::Corrupt("JPEG XL tile with a non-identity orientation".into()));
    }
    let planes = if header.metadata.grayscale() { 1 } else { 3 };
    if planes != cpp {
        return Err(RawError::Corrupt(format!("JPEG XL tile has {planes} colour planes, the IFD {cpp} samples per pixel")));
    }
    let max = match header.metadata.bit_depth {
        BitDepth::IntegerSample { bits_per_sample } if !float && (1..=16).contains(&bits_per_sample) => ((1u32 << bits_per_sample) - 1) as f32,
        BitDepth::FloatSample { .. } if float => 0.0,
        d => return Err(RawError::Corrupt(format!("JPEG XL tile sample type {d:?} does not match the IFD"))),
    };
    Ok((img, max))
}

/// Check the first chunk's headers ([`open`]) without decoding pixels (header-only probing).
#[allow(clippy::too_many_arguments)]
pub(crate) fn check(src: &[u8], cw: usize, ch: usize, vw: usize, vh: usize, cpp: usize, bits: u32, float: bool) -> Result<()> {
    open(src, cw, ch, vw, vh, cpp, bits, float).map(|_| ())
}

/// Decode one chunk into `cw × ch × cpp` samples (row-major, interleaved); a chunk coded at its
/// in-image size `vw × vh` fills the top-left of that, the rest stays 0.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode(src: &[u8], cw: usize, ch: usize, vw: usize, vh: usize, cpp: usize, bits: u32, float: bool) -> Result<ChunkPx> {
    let (img, max) = open(src, cw, ch, vw, vh, cpp, bits, float)?;
    if img.num_loaded_keyframes() == 0 {
        return Err(corrupt("no complete frame"));
    }
    let render = img.render_frame(0).map_err(corrupt)?;
    let fb = render.image_all_channels();
    let (jw, jh, fc) = (fb.width(), fb.height(), fb.channels());
    if jw == 0 || jh == 0 || jw > cw || jh > ch || fc < cpp {
        return Err(corrupt(format!("rendered {jw}×{jh}×{fc}, expected {cw}×{ch}×{cpp}")));
    }
    let n = cw.saturating_mul(ch).saturating_mul(cpp);
    let rows = fb.buf().chunks_exact(jw * fc);
    Ok(if float {
        let mut out = vec![0f32; n];
        for (dst, src) in out.chunks_exact_mut(cw * cpp).zip(rows) {
            for (d, s) in dst.chunks_exact_mut(cpp).zip(src.chunks_exact(fc)) {
                d.iter_mut().zip(s).for_each(|(d, s)| *d = *s);
            }
        }
        ChunkPx::F32(out)
    } else {
        let mut out = vec![0u16; n];
        for (dst, src) in out.chunks_exact_mut(cw * cpp).zip(rows) {
            for (d, s) in dst.chunks_exact_mut(cpp).zip(src.chunks_exact(fc)) {
                // `as` saturates (NaN → 0): lossy overshoot clips to the integer range
                d.iter_mut().zip(s).for_each(|(d, s)| *d = (s * max).round() as u16);
            }
        }
        ChunkPx::U16(out)
    })
}
