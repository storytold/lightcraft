//! Sony ARW.
//!
//! Sources: H. Dietz, "Sony ARW2 Compression: Artifacts And Credible Repair" (IS&T Electronic Imaging 2016) for
//! the ARW2 ("cRAW") scheme, and the ExifTool Sony tag-name documentation for the meaning of the raw-IFD tags
//! (`0x7010` tone-curve thresholds, `0x7310` black levels, `0x7313` WB levels, `0x74c7/0x74c8` crop).
//!
//! ARW2, as described in the paper:
//! 1. Sensor values are tone-mapped to 11-bit codes by a five-segment piecewise-linear curve whose step doubles at
//!    each threshold; the thresholds are recorded in the file (`0x7010`). We invert it: code `c` indexes the curve at
//!    `2c` in a 12-bit domain whose breakpoints are the recorded thresholds / 4, with slopes 1, 2, 4, 8, 16; the
//!    result is in 14-bit sensor units (black 512, white 16383 per the file's own tags, which we verified against
//!    the decoded data).
//! 2. Each row is coded in 32-pixel groups split into two interleaved 16-pixel sets (even columns, then odd), each
//!    a 128-bit little-endian block: 11-bit max, 11-bit min, 4-bit index of the max, 4-bit index of the min and 14
//!    seven-bit deltas above the min, scaled by the smallest shift that fits `max − min` into 7 bits.
//!
//! White balance: `WB_RGGBLevels` (`0x7313`), the gains the camera applied. Bodies from about 2017 on write them in
//! plain form in the raw IFD; every ARW from the DSLR-A200 on has them in the encrypted `SR2SubIFD` (see
//! [`super::sr2`]), and the two agree on every file that has both. Only when neither can be read do we fall back on
//! the `WB_RGBLevels` of the maker note's enciphered `Tag2010` block (see [`DECIPHER`] and [`TAG2010_WB`]), which
//! can differ widely from what the camera applied (issue #535). Without any, an ARW opened with unit multipliers,
//! i.e. a strong green cast (issue #148).
//!
//! Black level: the raw IFD's `0x7310`, else the level stored in the encrypted `SR2SubIFD` (see [`sr2_black`]; 800
//! rather than the default 512 on 1″-sensor bodies such as the RX100 series), else 512 (14-bit) / 128 (12-bit).
//! Downsized lossless (YCbCr) data sit [`YCBCR_OFFSET`] above that.
//!
//! The Sony DSC-R1 (.SR2, 2005) stores uncompressed 16-bit words big-endian inside a little-endian TIFF
//! ([`word16_order`]). Measured on the CC0 DSC-R1 (raw.pixls.us 3221): StripByteCounts 20780544 = 3984×2608×2; read
//! big-endian the maximum is 16368 and the image is smooth (neighbour roughness 0.012 against 0.249), read
//! little-endian the maximum is 65340 (noise). Black stays the default 512 (the masked columns read 511.4) and white
//! comes from the data. The crop and white balance are NOT addressed here: the file has no maker-note `FullImageSize`
//! and no plain WB tag (they live in its `SR2SubIFD`), so it opens uncropped with unit multipliers.
//!
//! Packed 12-bit ARW (DSLR-A900; Compression 32767 but BitsPerSample 12 and a strip of exactly width × height × 1.5
//! bytes): plain 12-bit samples, two per three bytes, least-significant-bit first ([`unpack_row12`]); linear, black 128
//! (512 on the 14-bit scale), CFA from the file's own pattern tag, crop centred on the frame (see [`default_crop`]).
//!
//! Also: uncompressed 16-bit ARW, and lossless-compressed ARW (Compression 7, ILCE-7M4 and later): LJ92 tiles whose
//! frames hold one 2×2 CFA cell per four-component sample ([`read_quad_tiles`]). Other lossless-JPEG layouts go
//! through the generic TIFF path.

use crate::tiffraw::{Packing, check_image, read_image_in};
use crate::{BlackLevel, Cfa, ColorData, Mode, OpcodeLists, RawData, RawError, RawFormat, RawImage, Rect, Result, ljpeg};
use lightcraft_geom::Orientation;
use lightcraft_tiff::image::{Chunk, ImageInfo, Layout, chunk_bytes};
use lightcraft_tiff::tags::{self as t, photometric};
use lightcraft_tiff::{Ifd, Tiff, makernote};
use rayon::prelude::*;

const TONE_CURVE: u16 = 0x7010;
const BLACK_LEVEL: u16 = 0x7310;
const WB_RGGB: u16 = 0x7313;
const CROP_TOP_LEFT: u16 = 0x74c7;
const CROP_SIZE: u16 = 0x74c8;
const YCBCR_COEFFICIENTS: u16 = 529;
const REFERENCE_BLACK_WHITE: u16 = 532;
/// Maker-note tags (ExifTool Sony tag names): the enciphered `Tag2010` block and `FullImageSize` (height, width).
const MN_TAG2010: u16 = 0x2010;
const MN_FULL_IMAGE_SIZE: u16 = 0xb02b;
/// Maker-note `DynamicRangeOptimizer` (ExifTool Sony tag documentation, int32u): 0 Off, 1 Standard, 2 Advanced
/// Auto, 3 Auto, 8–12 Advanced Lv1–Lv5, 16–23 Lv1–Lv8. Sony writes a second tag of that name, `0xb04f` (0 Off,
/// 1 Standard, 2 Plus), but it adds nothing: on the 241 Sony raws (75 bodies) of 264 checked that carry both, it
/// reads 1 whenever `0xb025` is on, whatever the level, and 0 when off; older bodies (DSLR-A100 to A900,
/// NEX-3/5/C3, SLT-A33/A35/A55) leave it out. `0xb025` was present on all 264 files (98 bodies).
const MN_DYNAMIC_RANGE_OPTIMIZER: u16 = 0xb025;

/// Whether the camera's Dynamic Range Optimizer (DRO) was on for this shot. DRO is a local tone operator that
/// brightens darker regions of the camera's JPEG (and the preview embedded in the ARW) but leaves the raw data
/// alone; the factory default is Auto. `None` when the maker note doesn't record it or holds an undocumented value.
pub(crate) fn dynamic_range_optimizer(note: &makernote::MakerNote) -> Option<bool> {
    match note.ifd.u64(MN_DYNAMIC_RANGE_OPTIMIZER)? {
        0 => Some(false),
        1..=3 | 8..=12 | 16..=23 => Some(true),
        _ => None,
    }
}

/// Inverse of Sony's maker-note byte substitution. ExifTool's Sony tag documentation states that the data of
/// tags `0x2010`, `0x9050` and `0x94xx` "is encrypted by a simple substitution cipher" (no decoder source was
/// consulted). The substitution, checked black-box on CC0 raw.pixls.us samples: every byte `p < 249` is stored as
/// `p³ mod 249` (a bijection on 0..249, as 3 is coprime to φ(249) = 164) and bytes 249–255 are stored unchanged.
/// Verified on 15 files from 13 bodies covering seven `Tag2010` layouts: the deciphered `SonyISO` field encodes
/// the Exif ISO (`100 · 2^(16 − v/256)`), the green level is 255–256 throughout, and on the ILCE-7M3 the deciphered
/// `WB_RGBLevels` equal the plain raw-IFD `0x7313` levels.
const DECIPHER: [u8; 256] = {
    let mut inv = [0u8; 256];
    let mut p = 0usize;
    while p < 256 {
        let c = if p < 249 { p * p % 249 * p % 249 } else { p };
        inv[c] = p as u8;
        p += 1;
    }
    inv
};

/// `Tag2010` layouts (ExifTool "Sony Tag2010a" … "Tag2010i" tables): the models each is documented for and the
/// byte offset of `WB_RGBLevels` (three `u16`, R, G, B gains; Tag2010g and h share the offset). Only a fallback
/// for files whose `WB_RGGBLevels` (`0x7313`, plain or in the `SR2SubIFD`) cannot be read. On the 45 CC0
/// raw.pixls.us ARWs that have both but no plain `0x7313`, the two agree within 5 % on 35 and differ by 8–100 %
/// on the other 10: five of the eight shot with a preset, colour temperature or custom white balance (ILCE-3500 at
/// 5600 K: R −34 %, B +100 %) and five in Auto (SLT-A37: R −32 %, B +53 %). Where the camera JPEG has enough
/// neutral pixels to tell, the gains the camera applied are `WB_RGGBLevels`, not these.
const TAG2010_WB: &[(&[&str], usize)] = &[
    (&["NEX-5N"], 4476),
    (&["SLT-A65", "SLT-A77", "NEX-7", "NEX-VG20E"], 4480),
    (&["SLT-A37", "SLT-A57", "NEX-F3"], 4444),
    (&["DSC-HX10V", "DSC-HX20V", "DSC-HX200V", "DSC-TX66", "DSC-TX200V", "DSC-TX300V", "DSC-WX50", "DSC-WX100", "DSC-WX150"], 4568),
    (
        &[
            "SLT-A58",
            "SLT-A99",
            "ILCE-3000",
            "ILCE-3500",
            "NEX-3N",
            "NEX-5R",
            "NEX-5T",
            "NEX-6",
            "NEX-VG30E",
            "NEX-VG900",
            "DSC-RX100",
            "DSC-RX1",
            "DSC-RX1R",
            "DSC-HX300",
            "DSC-HX50V",
            "DSC-TX30",
            "DSC-WX60",
            "DSC-WX200",
            "DSC-WX300",
        ],
        4532,
    ),
    (&["DSC-RX100M2", "DSC-QX10", "DSC-QX100"], 4204),
    (
        &[
            "DSC-HX60V",
            "DSC-HX350",
            "DSC-HX400V",
            "DSC-QX30",
            "DSC-RX10",
            "DSC-RX100M3",
            "DSC-WX220",
            "DSC-WX350",
            "ILCE-7",
            "ILCE-7R",
            "ILCE-7S",
            "ILCE-7M2",
            "ILCE-5000",
            "ILCE-5100",
            "ILCE-6000",
            "ILCE-QX1",
            "ILCA-68",
            "ILCA-77M2",
            "DSC-HX80",
            "DSC-HX90V",
            "DSC-RX0",
            "DSC-RX1RM2",
            "DSC-RX10M2",
            "DSC-RX10M3",
            "DSC-RX100M4",
            "DSC-RX100M5",
            "DSC-WX500",
            "ILCE-6300",
            "ILCE-6500",
            "ILCE-7RM2",
            "ILCE-7SM2",
            "ILCA-99M2",
        ],
        612,
    ),
    (
        &[
            "ILCE-6100",
            "ILCE-6400",
            "ILCE-6600",
            "ILCE-7C",
            "ILCE-7M3",
            "ILCE-7RM3",
            "ILCE-7RM4",
            "ILCE-9",
            "ILCE-9M2",
            "DSC-RX0M2",
            "DSC-RX10M4",
            "DSC-RX100M6",
            "DSC-RX100M5A",
            "DSC-RX100M7",
            "DSC-HX99",
        ],
        594,
    ),
];

/// White balance from the maker note's enciphered `Tag2010` block, the fallback described at [`TAG2010_WB`].
/// `model` is the Exif model; Sony appends a regional "V" to some names (SLT-A77V), which the documented lists
/// omit.
fn tag2010_wb(model: &str, block: &[u8], order: lightcraft_tiff::ByteOrder) -> Option<[f32; 3]> {
    let model = model.trim();
    let &(_, offset) = TAG2010_WB.iter().find(|(models, _)| models.iter().any(|m| model == *m || model.strip_suffix('V') == Some(*m)))?;
    let bytes: Vec<u8> = block.get(offset..offset + 6)?.iter().map(|&b| DECIPHER[b as usize]).collect();
    let level = |i: usize| order.read_u16(&bytes, 2 * i).map(f32::from);
    let (r, g, b) = (level(0)?, level(1)?, level(2)?);
    // levels are fixed-point gains (green 255–256 on every sample seen); reject implausible values from an unknown layout
    if !(16.0..=16384.0).contains(&g) {
        return None;
    }
    let (r, b) = (r / g, b / g);
    ((0.2..=8.0).contains(&r) && (0.2..=8.0).contains(&b)).then_some([r, 1.0, b])
}

/// Whether a 12-bit, single-strip, Sony-compressed (32767) image is stored as plain packed 12-bit samples: the strip
/// is exactly 1.5 bytes per pixel (width even), so every row is `1.5 · width` bytes with no padding. On the one
/// DSLR-A900 sample this is the whole strip (36 917 760 = 6080 × 4048 × 1.5) and it ends at the end of the file.
fn is_packed12(info: &ImageInfo, strip_count: usize, strip_len: u64) -> bool {
    let (w, h) = (info.width as u64, info.height as u64);
    info.bits() == 12 && strip_count == 1 && w % 2 == 0 && w > 0 && h > 0 && strip_len == w * h * 3 / 2
}

/// Unpack plain 12-bit samples stored least-significant-bit first: two pixels per three bytes, the first being
/// `b0 | (b1 & 0xf) << 8` and the second `b1 >> 4 | b2 << 4` (measured: same-colour neighbours differ by ~16 codes
/// in this order against ~200 in the most-significant-first order). `row` is `1.5 · out.len()` bytes.
pub(crate) fn unpack_row12(row: &[u8], out: &mut [u16]) {
    for (i, b) in row.windows(3).step_by(3).enumerate() {
        let Some(px) = out.get_mut(2 * i..2 * i + 2) else { break };
        px[0] = u16::from(b[0]) | (u16::from(b[1] & 0xf) << 8);
        px[1] = u16::from(b[1] >> 4) | (u16::from(b[2]) << 4);
    }
}

/// White-balance gains `[R/G, 1, B/G]` from `WB_RGGBLevels` (R, G, G, B); `None` unless there are four levels
/// with a positive green and plausible ratios.
fn rggb_gains(levels: &[f64]) -> Option<[f32; 3]> {
    let &[r, g1, g2, b] = levels else { return None };
    let g = (g1 + g2) / 2.0;
    if !g.is_finite() || g <= 0.0 {
        return None;
    }
    let (r, b) = (r / g, b / g);
    ((0.2..=8.0).contains(&r) && (0.2..=8.0).contains(&b)).then_some([r as f32, 1.0, b as f32])
}

/// The inverse tone curve: 11-bit code → 14-bit sensor value.
pub(crate) fn code_curve(thresholds: &[u64]) -> Vec<u16> {
    let mut bp = [0usize, 4095, 4095, 4095, 4095, 4095];
    for (i, &v) in thresholds.iter().take(4).enumerate() {
        bp[i + 1] = ((v >> 2) as usize).min(4095);
    }
    // keep breakpoints monotonic
    for i in 1..6 {
        bp[i] = bp[i].max(bp[i - 1]);
    }
    let mut lut = vec![0u32; 4096];
    let mut seg = 0;
    for i in 1..4096 {
        while seg < 4 && i > bp[seg + 1] {
            seg += 1;
        }
        lut[i] = lut[i - 1] + (1 << seg);
    }
    (0..2048).map(|c| lut[(2 * c).min(4095)].min(16383) as u16).collect()
}

/// Decode one row of ARW2 data (`row.len() == width` bytes) into codes.
pub(crate) fn decode_row(row: &[u8], out: &mut [u16]) {
    let w = out.len();
    let mut x0 = 0;
    while x0 + 32 <= w && x0 + 32 <= row.len() {
        for half in 0..2 {
            let off = x0 + half * 16;
            let Some(bytes) = row.get(off..off + 16).and_then(|b| <[u8; 16]>::try_from(b).ok()) else { return };
            let block = u128::from_le_bytes(bytes);
            let max = (block & 0x7ff) as u16;
            let min = ((block >> 11) & 0x7ff) as u16;
            let imax = ((block >> 22) & 0xf) as usize;
            let imin = ((block >> 26) & 0xf) as usize;
            let range = max.saturating_sub(min);
            let mut sh = 0;
            while sh < 4 && (range >> sh) > 127 {
                sh += 1;
            }
            let mut bit = 30;
            for i in 0..16 {
                let v = if i == imax {
                    max
                } else if i == imin {
                    min
                } else {
                    // a corrupt block with imax == imin would read a 15th delta past bit 127
                    let d = if bit + 7 <= 128 { ((block >> bit) & 0x7f) as u16 } else { 0 };
                    bit += 7;
                    (min + (d << sh)).min(0x7ff)
                };
                out[x0 + half + 2 * i] = v;
            }
        }
        x0 += 32;
    }
}

/// Whether `info` holds Sony's lossless-compressed layout: LJ92 tiles whose frames are half the tile in each
/// direction with four components (checked on the first tile).
fn is_quad_tiled(bytes: &[u8], info: &ImageInfo) -> bool {
    let Layout::Tiles { tile_width, tile_height } = info.layout else { return false };
    info.samples_per_pixel == 1
        && info
            .chunks(bytes.len() as u64)
            .first()
            .and_then(|c| chunk_bytes(bytes, c))
            .and_then(|src| ljpeg::frame_info(src).ok())
            .is_some_and(|(fw, fh, nc, _)| nc == 4 && fw * 2 == tile_width as usize && fh * 2 == tile_height as usize)
}

/// Decode Sony's lossless-compressed raw data. Each tile is one LJ92 frame of half the tile's width and height whose
/// four components are the tile's 2×2 CFA cells in raster order (top-left, top-right, bottom-left, bottom-right), so
/// all four colour planes are predicted separately. Observed in the files' own frame headers (512×512 tiles holding
/// 256×256×4 frames) and checked on decoded images; the generic TIFF path reads a frame as a row-major sample stream
/// (DNG's convention), which would interleave each tile's left and right halves row by row.
fn read_quad_tiles(bytes: &[u8], info: &ImageInfo) -> Result<Vec<u16>> {
    let (w, h) = (info.width as usize, info.height as usize);
    let total = check_image(bytes, info)?;
    let chunks = info.chunks(bytes.len() as u64);
    let decoded: Vec<Result<(Chunk, ljpeg::Frame)>> = chunks
        .par_iter()
        .map(|c| {
            let src = chunk_bytes(bytes, c).ok_or_else(|| RawError::Corrupt("tile offset past end of file".into()))?;
            let f = ljpeg::decode(src, (c.width as usize * c.height as usize).max(1 << 16))?;
            if f.components != 4 || f.data.len() < f.width * f.height * 4 {
                return Err(RawError::Corrupt(format!(
                    "lossless ARW tile: {} samples in a {}×{}×{} frame",
                    f.data.len(),
                    f.width,
                    f.height,
                    f.components
                )));
            }
            Ok((*c, f))
        })
        .collect();
    let mut out = vec![0u16; total];
    let mut ok = 0usize;
    let mut first_err = None;
    for r in decoded {
        let (c, f) = match r {
            Ok(v) => v,
            Err(e) => {
                first_err.get_or_insert(e);
                continue;
            }
        };
        ok += 1;
        let (x0, y0) = (c.x as usize, c.y as usize);
        for fy in 0..f.height {
            for k in 0..4 {
                let y = y0 + 2 * fy + (k >> 1);
                if y >= h {
                    continue;
                }
                for fx in 0..f.width {
                    let x = x0 + 2 * fx + (k & 1);
                    if x < w {
                        out[y * w + x] = f.data[(fy * f.width + fx) * 4 + k];
                    }
                }
            }
        }
    }
    if ok == 0 {
        return Err(first_err.unwrap_or_else(|| RawError::Corrupt("no decodable tiles".into())));
    }
    Ok(out)
}

/// Sony's downsized lossless ARWs (M and S sizes: linear YCbCr, already white-balanced) are stored this far above the
/// sensor black level the raw IFD records (`0x7310`, 512 in every file seen); their reference levels say Y black 0.
/// Measured against a full-size coding of the same scene shot seconds apart, on the pixels where both camera JPEGs
/// agree: YCbCr = k · (full-size − black) · WB + b, with b 1015–1052 and green k 0.98–1.02, on 11 of 12 files from
/// four bodies (ILCE-7M4 M, S and APS-C S; ILCE-7RM5 M, S and S35 S; ILCE-7CR M, S and S35 S; ILCE-9M3 M, S and
/// APS-C S; the twelfth's reference was shot 24 s later, 7 % brighter). Their darkest 0.1 % lies at 1047–1127. So
/// black and white both move up by 512: the scale stays the full-size raw's, and an M/S file renders like its
/// full-size twin. Subtracting only 512 left a pedestal of 3 % of white, lifting the shadows (issue #535).
const YCBCR_OFFSET: f32 = 512.0;
/// White of the YCbCr data before [`YCBCR_OFFSET`]: the reference levels' Y white (required by [`read_ycbcr_tiles`]).
const YCBCR_WHITE: f32 = 16383.0;

/// Downsized M/S ARWs are linear YCbCr 4:2:0 / 4:2:2, not CFA, despite retaining dummy CFA tags.
/// T.81 supplies the lossless coding; TIFF 6.0 supplies the YCbCr coefficients and reference
/// levels. Sony's reference chroma black and white are identical (the neutral offset).
fn read_ycbcr_tiles(bytes: &[u8], info: &ImageInfo, raw: &Ifd, mode: Mode) -> Result<Vec<u16>> {
    let total = check_image(bytes, info)?;
    if info.samples_per_pixel != 3 || info.planar != 1 || !matches!(info.layout, Layout::Tiles { .. }) {
        return Err(RawError::Unsupported("Sony linear YCbCr tile layout".into()));
    }
    let chunks = info.chunks(bytes.len() as u64);
    let first = chunks.first().and_then(|c| chunk_bytes(bytes, c)).ok_or_else(|| RawError::Corrupt("missing YCbCr tile".into()))?;
    ljpeg::frame_info_subsampled(first)?;
    let coefficients = raw.f64s(YCBCR_COEFFICIENTS).unwrap_or_else(|| vec![0.299, 0.587, 0.114]);
    let [kr, kg, kb] = coefficients.as_slice() else { return Err(RawError::Corrupt("YCbCr coefficients".into())) };
    if !coefficients.iter().all(|v| v.is_finite() && *v > 0.0 && *v < 1.0) || (kr + kg + kb - 1.0).abs() > 0.001 {
        return Err(RawError::Corrupt("invalid YCbCr coefficients".into()));
    }
    let references = raw.f64s(REFERENCE_BLACK_WHITE).unwrap_or_else(|| vec![0.0, 16383.0, 16384.0, 16384.0, 16384.0, 16384.0]);
    let [yblack, ywhite, cbzero, cbwhite, crzero, crwhite] = references.as_slice() else {
        return Err(RawError::Corrupt("YCbCr reference levels".into()));
    };
    if !references.iter().all(|v| v.is_finite() && (0.0..=65535.0).contains(v))
        || *yblack != 0.0
        || *ywhite != 16383.0
        || cbzero != cbwhite
        || crzero != crwhite
    {
        return Err(RawError::Unsupported("Sony YCbCr reference levels".into()));
    }
    if mode == Mode::Header {
        return Ok(Vec::new());
    }
    let decoded: Vec<(Chunk, ljpeg::FrameSubsampled)> = chunks
        .par_iter()
        .map(|c| {
            let src = chunk_bytes(bytes, c).ok_or_else(|| RawError::Corrupt("YCbCr tile outside file".into()))?;
            let limit = (c.width as usize).checked_mul(c.height as usize).and_then(|n| n.checked_mul(3)).ok_or(RawError::Limit("tile too large"))?;
            let f = ljpeg::decode_subsampled(src, limit, ljpeg::Prediction::Geometric)?;
            if f.width != c.width as usize || f.height != c.height as usize {
                return Err(RawError::Corrupt("Sony YCbCr tile dimensions".into()));
            }
            Ok((*c, f))
        })
        .collect::<Result<_>>()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let mut out = vec![0u16; total];
    for (c, f) in decoded {
        for y in 0..f.height.min(h.saturating_sub(c.y as usize)) {
            for x in 0..f.width.min(w.saturating_sub(c.x as usize)) {
                let luma = f.planes[0][y * f.width + x] as f64;
                let ci = (y / f.vertical_subsampling) * (f.width / 2) + x / 2;
                let cb = f.planes[1][ci] as f64 - cbzero;
                let cr = f.planes[2][ci] as f64 - crzero;
                let r = luma + (2.0 - 2.0 * kr) * cr;
                let b = luma + (2.0 - 2.0 * kb) * cb;
                let g = (luma - kr * r - kb * b) / kg;
                let dst = (((c.y as usize + y) * w) + c.x as usize + x) * 3;
                for (s, value) in [r, g, b].into_iter().enumerate() {
                    out[dst + s] = value.round().clamp(0.0, 65535.0) as u16;
                }
            }
        }
    }
    Ok(out)
}

/// The black level stored in the encrypted `SR2SubIFD` (`0x7310`; bodies that do not write it in the raw IFD keep
/// it only there), in 14-bit units; `None` unless it is SHORT, count 4, and the four levels are equal and plausible.
///
/// Its value sits at a layout-dependent position (1638 … 2786 bytes into the block), which the directory entry
/// gives. Reading a fixed position instead (2510, right for the 29252, 33210 and 56958-byte layouts) read other data
/// on the DSLR-A450/A500/A550 (27152 bytes: 354–365) and the DSLR-A700 (62112 bytes: 975), whose black is 512 by
/// ExifTool and by the data floor (issue #535).
fn sr2_black(sr2: &super::sr2::SubIfd) -> Option<f32> {
    match sr2.short_bytes(BLACK_LEVEL)? {
        (3, plain) if plain.len() == 8 => sr2_black_levels(plain, sr2.order()),
        _ => None,
    }
}

/// One black level from the four deciphered per-channel levels (their mean), when they are near-equal and plausible.
fn sr2_black_levels(plain: &[u8], order: lightcraft_tiff::ByteOrder) -> Option<f32> {
    let levels: Vec<u16> = (0..4).filter_map(|i| order.read_u16(plain, 2 * i)).collect();
    let (&lo, &hi) = (levels.iter().min()?, levels.iter().max()?);
    (levels.len() == 4 && (64..=4096).contains(&lo) && hi - lo <= 64).then(|| levels.iter().map(|&v| f32::from(v)).sum::<f32>() / 4.0)
}

/// Byte order of 16-bit-word samples: the file's own order unless the words only fit the sample width in the other
/// (see the DSC-R1 note at the top). Sampled across the strip; the order is swapped only when the file order leaves
/// more than 1 % of the sampled words at or above `1 << max(bits, 14)` while the other order leaves none.
fn word16_order(strip: &[u8], bits: u32, file: lightcraft_tiff::ByteOrder) -> lightcraft_tiff::ByteOrder {
    use lightcraft_tiff::ByteOrder::{Big, Little};
    let other = if file == Little { Big } else { Little };
    let limit = 1u32 << bits.clamp(14, 16);
    let (mut n, mut over_file, mut over_other) = (0usize, 0usize, 0usize);
    let words = strip.len() / 2;
    let step = (words / 65536).max(1);
    for &b in strip.as_chunks::<2>().0.iter().step_by(step) {
        n += 1;
        over_file += usize::from(u32::from(file.u16(b)) >= limit);
        over_other += usize::from(u32::from(other.u16(b)) >= limit);
    }
    if n > 0 && over_other == 0 && over_file * 100 > n { other } else { file }
}

/// The raw IFD's DNG-style `DefaultCropOrigin` / `DefaultCropSize`, clipped to the `w × h` frame.
fn dng_default_crop(raw: &Ifd, w: usize, h: usize) -> Option<Rect> {
    if let (Some([x, y]), Some([cw, ch])) = (raw.u64s(t::DEFAULT_CROP_ORIGIN).as_deref(), raw.u64s(t::DEFAULT_CROP_SIZE).as_deref())
        && *cw > 0
        && *ch > 0
    {
        return Some(Rect::new(*x as usize, *y as usize, *cw as usize, *ch as usize).clipped(w, h));
    }
    None
}

/// The image area for files without Sony's crop tags (`0x74c7/0x74c8`, written since about 2017): the DNG-style
/// default crop when the raw IFD has one, else the image size the camera records (see [`recorded_size_crop`]).
///
/// `centred` places that recorded-size window in the middle of the frame (on even offsets, keeping the CFA phase)
/// instead of at the left: the packed 12-bit frame (DSLR-A900) has no padding at either edge, and its camera JPEG
/// is centred on the frame.
fn default_crop(raw: &Ifd, mn: Option<&makernote::MakerNote>, exif_size: Option<(u64, u64)>, w: usize, h: usize, centred: bool) -> Rect {
    let full = Rect::new(0, 0, w, h);
    if let Some(crop) = dng_default_crop(raw, w, h) {
        return crop;
    }
    match mn.and_then(|m| m.ifd.u64s(MN_FULL_IMAGE_SIZE)).as_deref() {
        Some([fh, fw]) => recorded_size_crop((*fw, *fh), exif_size, w, h, centred).unwrap_or(full),
        _ => full,
    }
}

/// The crop for the maker note's `FullImageSize` `full` (width, height: the camera JPEG's size) and the Exif image
/// size `exif` (`PixelXDimension` × `PixelYDimension`) in a `w × h` frame; `None` when they don't describe a crop of
/// it. Older bodies store a few columns of padding at the right edge of the raw frame (constant values, 8–32 columns
/// on the samples we checked) inside a frame 16–48 pixels wider than `FullImageSize`, so the crop is anchored at
/// the left, which removes them while keeping the CFA phase. In the camera's 3:2 mode it is also anchored at the
/// top. An in-camera aspect ratio narrower than the frame (16:9) is centred vertically, as measured by registering
/// each camera JPEG on its raw (the ILCE-7SM2's 4240 × 2384 at y 228–232 of 2848; the DSLR-A580's 4912 × 2760 at
/// y 258–262 of 3280; issue #535): its height comes from `FullImageSize` (ILCE-7SM2) or, when that still says
/// 3:2, from an Exif image size of the same width (DSLR-A580). `centred` centres the window in both directions
/// instead (see [`default_crop`]).
fn recorded_size_crop(full: (u64, u64), exif: Option<(u64, u64)>, w: usize, h: usize, centred: bool) -> Option<Rect> {
    let (fw, fh) = (usize::try_from(full.0).ok()?, usize::try_from(full.1).ok()?);
    // only a plausible trim of the width: never more than 64 pixels, never an enlargement
    if fw == 0 || fh == 0 || fw > w || fw.saturating_add(64) < w || fh > h {
        return None;
    }
    let height = match exif.and_then(|(ew, eh)| Some((usize::try_from(ew).ok()?, usize::try_from(eh).ok()?))) {
        Some((ew, eh)) if ew == fw && eh > 0 && eh < fh => eh,
        _ => fh,
    };
    // even offsets keep the CFA phase
    let x = if centred { ((w - fw) / 2) & !1 } else { 0 };
    if height.saturating_add(64) >= h {
        return Some(Rect::new(x, if centred { ((h - height) / 2) & !1 } else { 0 }, fw, height));
    }
    // an in-camera aspect ratio: at least half the frame's height, centred
    (height * 2 >= h).then(|| Rect::new(x, ((h - height) / 2) & !1, fw, height))
}

fn raw_ifd(tiff: &Tiff) -> Option<&Ifd> {
    tiff.all_ifds()
        .into_iter()
        .filter(|i| i.u16(t::PHOTOMETRIC) == Some(photometric::CFA) || i.contains(TONE_CURVE))
        .max_by_key(|i| i.u64(t::IMAGE_WIDTH).unwrap_or(0).saturating_mul(i.u64(t::IMAGE_LENGTH).unwrap_or(0)))
}

pub(crate) fn decode(bytes: &[u8], mode: Mode) -> Result<RawImage> {
    let tiff = Tiff::parse(bytes)?;
    let ifd0 = &tiff.ifds[0];
    let raw = raw_ifd(&tiff).ok_or_else(|| RawError::Unsupported("ARW without a CFA image IFD (old ARW or SR2)".into()))?;
    let info = raw.image()?;
    let (w, h) = (info.width as usize, info.height as usize);
    if w * h > crate::MAX_SAMPLES {
        return Err(RawError::Limit("image too large"));
    }
    let bits = info.bits() as u32;
    let linear_rgb = info.compression == 7 && info.photometric == photometric::YCBCR;
    let chunks = info.chunks(bytes.len() as u64);
    let strip_len: u64 = chunks.iter().map(|c| c.len).sum();
    // The row-per-w-bytes (ARW2) layout stores exactly one byte per pixel in a single strip; all 108 such files in the test
    // archive measure exactly width x height. A200/A230/A350 files carry a strip 4-23 % larger than that (and A290/A390 one
    // smaller), which is a different packing, so they are refused rather than read with the wrong layout.
    let one_byte_per_sample = chunks.len() == 1 && strip_len == (w * h) as u64;
    let packed12 = info.compression == 32767 && is_packed12(&info, chunks.len(), strip_len);
    let (data, out_bits) = match info.compression {
        7 if linear_rgb => (RawData::U16(read_ycbcr_tiles(bytes, &info, raw, mode)?), 14),
        32767 if one_byte_per_sample && mode == Mode::Header => {
            chunk_bytes(bytes, &chunks[0]).ok_or_else(|| RawError::Corrupt("raw strip outside file".into()))?;
            (RawData::U16(Vec::new()), 14)
        }
        32767 if one_byte_per_sample => {
            let src = chunk_bytes(bytes, &chunks[0]).ok_or_else(|| RawError::Corrupt("raw strip outside file".into()))?;
            let curve = code_curve(&raw.u64s(TONE_CURVE).unwrap_or_else(|| vec![8000, 10400, 12900, 14100]));
            let mut data = vec![0u16; w * h];
            data.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
                let row = src.get(y * w..((y + 1) * w).min(src.len())).unwrap_or(&[]);
                if row.len() == w {
                    decode_row(row, out);
                    out.iter_mut().for_each(|v| *v = curve[*v as usize]);
                }
            });
            (RawData::U16(data), 14)
        }
        32767 if packed12 && mode == Mode::Header => {
            chunk_bytes(bytes, &chunks[0]).ok_or_else(|| RawError::Corrupt("raw strip outside file".into()))?;
            (RawData::U16(Vec::new()), 12)
        }
        32767 if packed12 => {
            let src = chunk_bytes(bytes, &chunks[0]).ok_or_else(|| RawError::Corrupt("raw strip outside file".into()))?;
            let row_bytes = w / 2 * 3;
            let mut data = vec![0u16; w * h];
            data.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
                if let Some(row) = src.get(y * row_bytes..(y + 1) * row_bytes) {
                    unpack_row12(row, out);
                }
            });
            (RawData::U16(data), 12)
        }
        32767 => return Err(RawError::Unsupported("Sony ARW version 1 / packed compressed variant (raw strip is not one byte per pixel)".into())),
        7 if is_quad_tiled(bytes, &info) => match mode {
            Mode::Full => (RawData::U16(read_quad_tiles(bytes, &info)?), bits),
            Mode::Header => {
                check_image(bytes, &info)?;
                (RawData::U16(Vec::new()), bits)
            }
        },
        1 => {
            let word16 = strip_len >= (w * h * 2) as u64;
            let packing = if word16 { Packing::Word16 } else { Packing::Msb };
            let order = match chunks.first().and_then(|c| chunk_bytes(bytes, c)) {
                Some(strip) if word16 => word16_order(strip, bits, tiff.order),
                _ => tiff.order,
            };
            (read_image_in(mode, bytes, &info, order, packing)?, bits)
        }
        _ => (read_image_in(mode, bytes, &info, tiff.order, Packing::Msb)?, bits),
    };
    let RawData::U16(ref samples) = data else { return Err(RawError::Unsupported("float ARW".into())) };
    // "12-bit uncompressed" files (e.g. ILCE-7RM2) say BitsPerSample 12 but store 16-bit words on the 14-bit scale
    // of the other modes (black 512, peaks near 16383): black and white follow the data, `bits` keeps the tag
    let scale_bits = if info.compression == 1 && out_bits < 14 && strip_len >= (w * h * 2) as u64 && samples.iter().any(|&v| v >> (out_bits + 1) != 0)
    {
        14
    } else {
        out_bits
    };

    let cfa = match (raw.u64s(t::CFA_REPEAT_PATTERN_DIM).as_deref(), raw.bytes(t::CFA_PATTERN_EP)) {
        (Some([2, 2]), Some(p)) if p.len() == 4 && p.iter().all(|&c| c <= 2) => Cfa { width: 2, height: 2, pattern: p.to_vec() },
        _ => Cfa::bayer_static("RGGB"),
    };
    let default_black = if scale_bits >= 14 { 512.0 } else { 128.0 };
    // the decrypted SR2SubIFD, read once and only when a plain black level or white balance is missing
    let sr2_cell = std::cell::OnceCell::new();
    let sr2 = || sr2_cell.get_or_init(|| super::sr2::SubIfd::read(bytes, ifd0, tiff.order)).as_ref();
    let black = match raw.f64s(BLACK_LEVEL).as_deref() {
        Some(v) if linear_rgb && !v.is_empty() => BlackLevel::uniform((v.iter().sum::<f64>() / v.len() as f64) as f32 + YCBCR_OFFSET),
        Some([a, b, c, d]) => {
            BlackLevel { repeat_rows: 2, repeat_cols: 2, values: vec![*a as f32, *b as f32, *c as f32, *d as f32], ..Default::default() }
        }
        _ if linear_rgb => BlackLevel::uniform(default_black + YCBCR_OFFSET),
        _ => BlackLevel::uniform((scale_bits >= 14).then(|| sr2().and_then(sr2_black)).flatten().unwrap_or(default_black)),
    };
    let white = if linear_rgb {
        YCBCR_WHITE + YCBCR_OFFSET
    } else {
        raw.f64(t::WHITE_LEVEL).map(|v| v as f32).filter(|v| *v > 0.0).unwrap_or_else(|| super::white_from_data(samples, scale_bits))
    };
    let model = ifd0.string(t::MODEL).unwrap_or_default();
    let mn = tiff
        .exif()
        .and_then(|e| e.get(t::MAKER_NOTE))
        .and_then(|e| makernote::parse_makernote(bytes, e.offset, e.count() as u64, tiff.order, &ifd0.string(t::MAKE).unwrap_or_default()));
    let wb = raw
        .f64s(WB_RGGB)
        .and_then(|v| rggb_gains(&v))
        .or_else(|| rggb_gains(&sr2()?.shorts(WB_RGGB)?))
        .or_else(|| mn.as_ref().and_then(|m| tag2010_wb(&model, m.ifd.bytes(MN_TAG2010)?, m.order)));
    let crop = match (raw.u64s(CROP_TOP_LEFT).as_deref(), raw.u64s(CROP_SIZE).as_deref()) {
        (Some([x, y]), Some([cw, ch])) if *cw > 0 && *ch > 0 => Rect::new(*x as usize, *y as usize, *cw as usize, *ch as usize).clipped(w, h),
        _ => {
            let exif_size = tiff.exif().and_then(|e| Some((e.u64(t::PIXEL_X_DIMENSION)?, e.u64(t::PIXEL_Y_DIMENSION)?)));
            default_crop(raw, mn.as_ref(), exif_size, w, h, packed12)
        }
    };
    let mut metadata = lightcraft_meta::from_tiff(&tiff);
    metadata.width = Some(crop.width as u32);
    metadata.height = Some(crop.height as u32);
    let img = RawImage {
        format: RawFormat::Arw,
        width: w,
        height: h,
        cpp: if linear_rgb { 3 } else { 1 },
        data,
        cfa: if linear_rgb { None } else { Some(cfa) },
        bits: out_bits,
        black,
        white: vec![white],
        active_area: Rect::new(0, 0, w, h),
        crop,
        orientation: Orientation::from_exif(ifd0.u16(t::ORIENTATION).unwrap_or(1)),
        color: ColorData::default(),
        // Sony's linear YCbCr already carries as-shot WB. Applying the CFA gains again
        // makes a neutral surface magenta. WB edits remain relative to this as-shot RGB.
        wb_multipliers: if linear_rgb { Some([1.0; 3]) } else { wb },
        linearized: false,
        // The distortion table is centred on the DNG-style default crop when the file has one: on the ILCE-7RM4A
        // it starts at x = 32 where Sony's crop tags (the image crop, unchanged) start at 0, and the warp was
        // validated against Sony's exports with that centre. The opcode's centre is in active-area coordinates, so
        // it stays on the optical centre whichever crop frames the image.
        opcodes: OpcodeLists {
            list3: if linear_rgb {
                Vec::new()
            } else {
                let geometry = dng_default_crop(raw, w, h).unwrap_or(crop);
                super::arw_lens::distortion(&model, raw, Rect::new(0, 0, w, h), geometry).into_iter().collect()
            },
            ..Default::default()
        },
        metadata,
    };
    img.validate_for(mode)?;
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsized_lossless_is_linear_rgb_not_cfa() {
        use lightcraft_tiff::{IfdBuilder, ImageData, TiffWriter, Value};
        let mut raw = IfdBuilder::new();
        raw.set(t::MAKE, Value::Ascii("SONY".into()));
        raw.set(t::MODEL, Value::Ascii("ILCE-7M4".into()));
        raw.set(t::IMAGE_WIDTH, Value::Long(vec![4]));
        raw.set(t::IMAGE_LENGTH, Value::Long(vec![4]));
        raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![15, 15, 15]));
        raw.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![3]));
        raw.set(t::PHOTOMETRIC, Value::Short(vec![photometric::YCBCR]));
        raw.set(t::COMPRESSION, Value::Short(vec![7]));
        raw.set(TONE_CURVE, Value::Short(vec![0; 4]));
        raw.set(BLACK_LEVEL, Value::Short(vec![512; 4]));
        raw.set(WB_RGGB, Value::Short(vec![2048, 1024, 1024, 2048]));
        raw.set_image(ImageData::Tiles { tile_width: 4, tile_height: 4, tiles: vec![ljpeg::tests::fixture_420()] });
        let file = TiffWriter::default().write(&[raw]).unwrap();
        let header = decode(&file, Mode::Header).unwrap();
        let full = decode(&file, Mode::Full).unwrap();
        assert_eq!(header.info(), full.info());
        assert_eq!((full.width, full.height, full.cpp), (4, 4, 3));
        assert!(full.cfa.is_none());
        assert_eq!(full.data.len(), 48);
        assert_eq!(full.wb_multipliers, Some([1.0; 3]));
        // the YCbCr data sit 512 above the recorded sensor black level; white moves with it
        assert_eq!((full.black.mean(), full.white_at(0)), (1024.0, 16895.0));
        assert_eq!((header.black.mean(), header.white_at(0)), (1024.0, 16895.0));
        let RawData::U16(ref pixels) = full.data else {
            panic!("integer ARW");
        };
        assert_eq!(&pixels[..3], &[1140, 929, 1000]);
        let developed = full.develop(crate::Method::Bilinear).unwrap();
        assert_eq!((developed.width, developed.height), (4, 4));
    }

    #[test]
    fn recorded_size_crops_in_camera_aspect_ratios() {
        // 3:2: anchored at the top-left, as before (ILCE-7S, DSLR-A700 whose Exif size is the whole frame)
        assert_eq!(recorded_size_crop((4240, 2832), Some((4240, 2832)), 4288, 2848, false), Some(Rect::new(0, 0, 4240, 2832)));
        assert_eq!(recorded_size_crop((4272, 2848), Some((4288, 2856)), 4288, 2856, false), Some(Rect::new(0, 0, 4272, 2848)));
        assert_eq!(recorded_size_crop((4240, 2832), None, 4288, 2848, false), Some(Rect::new(0, 0, 4240, 2832)));
        // 16:9 from FullImageSize (ILCE-7SM2) and from the Exif size when FullImageSize says 3:2 (DSLR-A580): centred
        assert_eq!(recorded_size_crop((4240, 2384), Some((4240, 2384)), 4288, 2848, false), Some(Rect::new(0, 232, 4240, 2384)));
        assert_eq!(recorded_size_crop((4912, 3264), Some((4912, 2760)), 4928, 3280, false), Some(Rect::new(0, 260, 4912, 2760)));
        // offsets stay even (CFA phase)
        assert_eq!(recorded_size_crop((4240, 2386), None, 4288, 2848, false).map(|r| r.y), Some(230));
        // not a crop of this frame: wider than it, far narrower, under half its height, empty, hostile values
        assert_eq!(recorded_size_crop((4300, 2832), None, 4288, 2848, false), None);
        assert_eq!(recorded_size_crop((4000, 2832), None, 4288, 2848, false), None);
        assert_eq!(recorded_size_crop((4240, 1000), None, 4288, 2848, false), None);
        assert_eq!(recorded_size_crop((0, 2832), None, 4288, 2848, false), None);
        assert_eq!(recorded_size_crop((u64::MAX, u64::MAX), Some((u64::MAX, 1)), 4288, 2848, false), None);
        assert_eq!(recorded_size_crop((4240, 2832), Some((4240, 0)), 4288, 2848, false), Some(Rect::new(0, 0, 4240, 2832)));
        // centred (packed 12-bit DSLR-A900: 6048 x 4032 in the middle of 6080 x 4048)
        assert_eq!(recorded_size_crop((6048, 4032), Some((6048, 4032)), 6080, 4048, true), Some(Rect::new(16, 8, 6048, 4032)));
        assert_eq!(recorded_size_crop((6046, 4030), None, 6080, 4048, true), Some(Rect::new(16, 8, 6046, 4030)));
    }

    /// A 32767-compressed ARW whose single strip is `strip` bytes for a 32 x 4 image.
    fn arw_with_strip(strip: usize) -> Vec<u8> {
        use lightcraft_tiff::{IfdBuilder, ImageData, TiffWriter, Value};
        let mut raw = IfdBuilder::new();
        raw.set(t::MAKE, Value::Ascii("SONY".into()));
        raw.set(t::MODEL, Value::Ascii("DSLR-A200".into()));
        raw.set(t::IMAGE_WIDTH, Value::Long(vec![32]));
        raw.set(t::IMAGE_LENGTH, Value::Long(vec![4]));
        raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![12]));
        raw.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![1]));
        raw.set(t::PHOTOMETRIC, Value::Short(vec![photometric::CFA]));
        raw.set(t::COMPRESSION, Value::Short(vec![32767]));
        raw.set(TONE_CURVE, Value::Short(vec![8000, 10400, 12900, 14100]));
        raw.set_image(ImageData::Strips { rows_per_strip: 4, strips: vec![vec![0u8; strip]] });
        TiffWriter::default().write(&[raw]).unwrap()
    }

    #[test]
    fn strip_larger_or_smaller_than_one_byte_per_pixel_is_the_packed_variant() {
        for strip in [32 * 4 + 8, 32 * 4 * 5 / 4 - 1, 32 * 4 - 1] {
            for mode in [Mode::Header, Mode::Full] {
                let err = decode(&arw_with_strip(strip), mode).unwrap_err();
                assert!(matches!(err, RawError::Unsupported(ref m) if m.contains("packed compressed variant")), "{strip} {mode:?}: {err:?}");
            }
        }
        for mode in [Mode::Header, Mode::Full] {
            let r = decode(&arw_with_strip(32 * 4), mode);
            assert!(!matches!(r, Err(RawError::Unsupported(ref m)) if m.contains("packed compressed variant")), "{r:?}");
        }
    }

    /// A 32767-compressed, 12-bit ARW of `w` x `h` whose single strip is `strip`.
    fn packed_arw(w: u32, h: u32, strip: Vec<u8>) -> Vec<u8> {
        use lightcraft_tiff::{IfdBuilder, ImageData, TiffWriter, Value};
        let mut raw = IfdBuilder::new();
        raw.set(t::MAKE, Value::Ascii("SONY".into()));
        raw.set(t::MODEL, Value::Ascii("DSLR-A900".into()));
        raw.set(t::IMAGE_WIDTH, Value::Long(vec![w]));
        raw.set(t::IMAGE_LENGTH, Value::Long(vec![h]));
        raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![12]));
        raw.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![1]));
        raw.set(t::PHOTOMETRIC, Value::Short(vec![photometric::CFA]));
        raw.set(t::COMPRESSION, Value::Short(vec![32767]));
        raw.set(t::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![2, 2]));
        raw.set(t::CFA_PATTERN_EP, Value::Byte(vec![0, 1, 1, 2]));
        raw.set(TONE_CURVE, Value::Short(vec![8000, 10400, 12900, 14100]));
        raw.set_image(ImageData::Strips { rows_per_strip: h, strips: vec![strip] });
        TiffWriter::default().write(&[raw]).unwrap()
    }

    /// Pack 12-bit samples as the file does: `b0 = a & 0xff`, `b1 = a >> 8 | b << 4 & 0xf0`, `b2 = b >> 4`.
    fn pack12(samples: &[u16]) -> Vec<u8> {
        samples.chunks(2).flat_map(|p| [p[0] as u8, (p[0] >> 8) as u8 | ((p[1] & 0xf) << 4) as u8, (p[1] >> 4) as u8]).collect()
    }

    #[test]
    fn packed_12_bit_strip_of_one_and_a_half_bytes_per_pixel_decodes() {
        let (w, h) = (8usize, 4usize);
        let samples: Vec<u16> = (0..w * h).map(|i| (i as u16 * 129 + 130) & 0xfff).collect();
        let file = packed_arw(w as u32, h as u32, pack12(&samples));
        let full = decode(&file, Mode::Full).unwrap();
        let RawData::U16(ref got) = full.data else { panic!("integer ARW") };
        assert_eq!(got, &samples);
        assert_eq!((full.width, full.height, full.bits), (w, h, 12));
        assert_eq!(full.cfa.as_ref().map(|c| c.pattern.clone()), Some(vec![0, 1, 1, 2]));
        assert_eq!(full.black.values.first().copied().unwrap_or(0.0), 128.0);
        let header = decode(&file, Mode::Header).unwrap();
        assert_eq!(header.info(), full.info());
        // the first three bytes 0x12 0xA3 0x45 are the samples 0x312 and 0x45A (least-significant-bit first)
        let mut row = vec![0u16; 2];
        unpack_row12(&[0x12, 0xa3, 0x45], &mut row);
        assert_eq!(row, [0x312, 0x45a]);
    }

    #[test]
    fn packed_12_bit_needs_the_exact_strip_size() {
        let (w, h) = (8usize, 4usize);
        let exact = w * h * 3 / 2;
        for strip in [exact - 1, exact + 1, exact + 3 * h] {
            for mode in [Mode::Header, Mode::Full] {
                let err = decode(&packed_arw(w as u32, h as u32, vec![0u8; strip]), mode).unwrap_err();
                assert!(matches!(err, RawError::Unsupported(ref m) if m.contains("packed compressed variant")), "{strip} {mode:?}: {err:?}");
            }
        }
        // an odd width cannot be packed in pairs
        let err = decode(&packed_arw(7, 4, vec![0u8; 7 * 4 * 3 / 2]), Mode::Full).unwrap_err();
        assert!(matches!(err, RawError::Unsupported(_)), "{err:?}");
    }

    /// Encode one 16-value set as an ARW2 block (the paper's scheme, used here to test the decoder).
    pub(crate) fn encode_block(v: &[u16; 16]) -> [u8; 16] {
        let (imax, &max) = v.iter().enumerate().max_by_key(|(i, x)| (**x, usize::MAX - *i)).unwrap();
        let (imin, &min) = v.iter().enumerate().filter(|(i, _)| *i != imax).min_by_key(|(_, x)| **x).unwrap();
        let range = max - min;
        let mut sh = 0;
        while sh < 4 && (range >> sh) > 127 {
            sh += 1;
        }
        let mut b: u128 = max as u128 | (min as u128) << 11 | (imax as u128) << 22 | (imin as u128) << 26;
        let mut bit = 30;
        for (i, &x) in v.iter().enumerate() {
            if i == imax || i == imin {
                continue;
            }
            b |= (((x - min) >> sh) as u128 & 0x7f) << bit;
            bit += 7;
        }
        b.to_le_bytes()
    }

    #[test]
    fn block_roundtrip_exact_when_range_small() {
        let mut row = vec![0u8; 64];
        let vals: Vec<u16> = (0..64).map(|i| 300 + (i * 37 % 100) as u16).collect();
        for g in 0..2 {
            for half in 0..2 {
                let set: [u16; 16] = std::array::from_fn(|i| vals[g * 32 + half + 2 * i]);
                row[g * 32 + half * 16..g * 32 + half * 16 + 16].copy_from_slice(&encode_block(&set));
            }
        }
        let mut out = vec![0u16; 64];
        decode_row(&row, &mut out);
        assert_eq!(out, vals);
    }

    #[test]
    fn block_quantises_large_ranges() {
        let set: [u16; 16] = std::array::from_fn(|i| (i as u16) * 130);
        let mut row = vec![0u8; 32];
        row[..16].copy_from_slice(&encode_block(&set));
        let mut out = vec![0u16; 32];
        decode_row(&row, &mut out);
        for i in 0..16 {
            let got = out[2 * i];
            assert!(got <= set[i] && set[i] - got < 16, "{i}: {got} vs {}", set[i]);
        }
        assert_eq!(out[0], 0);
        assert_eq!(out[30], 1950);
    }

    #[test]
    fn curve_is_monotonic_and_matches_tags() {
        let c = code_curve(&[8000, 10400, 12900, 14100]);
        assert!(c.windows(2).all(|w| w[1] >= w[0]));
        assert_eq!(c[256], 512); // black
        assert_eq!(c[1000], 2000);
        assert_eq!(*c.last().unwrap(), 16383);
        // degenerate thresholds do not panic
        let _ = code_curve(&[]);
        let _ = code_curve(&[60000, 1, 0, 0]);
    }

    #[test]
    fn decipher_inverts_the_cube_substitution() {
        let mut seen = [false; 256];
        for p in 0..=255u8 {
            let c = if p < 249 { (p as u32).pow(3) % 249 } else { p as u32 } as u8;
            assert_eq!(DECIPHER[c as usize], p);
            seen[DECIPHER[p as usize] as usize] = true;
        }
        assert!(seen.iter().all(|&s| s), "not a bijection");
        assert_eq!((DECIPHER[8], DECIPHER[27], DECIPHER[94]), (2, 3, 7)); // 7³ = 343 ≡ 94
    }

    fn encipher(plain: &[u8]) -> Vec<u8> {
        plain.iter().map(|&p| if p < 249 { ((p as u32).pow(3) % 249) as u8 } else { p }).collect()
    }

    #[test]
    fn tag2010_white_balance_by_model() {
        use lightcraft_tiff::ByteOrder::Little;
        let mut plain = vec![0u8; 700];
        for (i, v) in [669u16, 256, 441].iter().enumerate() {
            plain[612 + 2 * i..614 + 2 * i].copy_from_slice(&v.to_le_bytes());
        }
        let block = encipher(&plain);
        let wb = tag2010_wb("DSC-RX100M3", &block, Little).unwrap();
        assert!((wb[0] - 669.0 / 256.0).abs() < 1e-6 && wb[1] == 1.0 && (wb[2] - 441.0 / 256.0).abs() < 1e-6, "{wb:?}");
        // same layout (Tag2010h), regional "V" suffix, other layouts, unknown models, truncated or implausible data
        assert!(tag2010_wb("ILCE-7RM2", &block, Little).is_some());
        assert!(tag2010_wb("DSC-RX100M3V", &block, Little).is_some());
        assert!(tag2010_wb("SLT-A77V", &block, Little).is_none()); // Tag2010b: offset 4480 is past the block
        assert!(tag2010_wb("DSC-RX100M3X", &block, Little).is_none());
        assert!(tag2010_wb("ILCE-1", &block, Little).is_none());
        assert!(tag2010_wb("DSC-RX100M3", &block[..615], Little).is_none());
        assert!(tag2010_wb("DSC-RX100M3", &encipher(&[0u8; 700]), Little).is_none());
        let mut odd = plain.clone();
        odd[612..614].copy_from_slice(&9000u16.to_le_bytes()); // R/G ≈ 35
        assert!(tag2010_wb("DSC-RX100M3", &encipher(&odd), Little).is_none());
    }

    /// A Sony-style file: IFD0 (`make`) → Exif → maker note `SONY DSC \0\0\0` + an IFD of `entries` (tag, type,
    /// count, inline value bytes), the layout of the camera's own notes; `count` overrides the entry count.
    fn sony_file(order: lightcraft_tiff::ByteOrder, make: &str, entries: &[(u16, u16, u32, [u8; 4])], count: Option<u16>) -> Vec<u8> {
        use lightcraft_tiff::{ByteOrder, IfdBuilder, TiffWriter, Value};
        let (u16b, u32b) = match order {
            ByteOrder::Little => (u16::to_le_bytes as fn(u16) -> [u8; 2], u32::to_le_bytes as fn(u32) -> [u8; 4]),
            ByteOrder::Big => (u16::to_be_bytes as fn(u16) -> [u8; 2], u32::to_be_bytes as fn(u32) -> [u8; 4]),
        };
        let mut note = b"SONY DSC \0\0\0".to_vec();
        note.extend(u16b(count.unwrap_or(entries.len() as u16)));
        for &(tag, kind, n, value) in entries {
            note.extend(u16b(tag));
            note.extend(u16b(kind));
            note.extend(u32b(n));
            note.extend(value);
        }
        note.extend(u32b(0));
        let mut exif = IfdBuilder::new();
        exif.set(t::MAKER_NOTE, Value::Undefined(note));
        let mut ifd0 = IfdBuilder::new();
        ifd0.set(t::MAKE, Value::Ascii(make.into()));
        ifd0.set(t::MODEL, Value::Ascii("ILCE-7CR".into()));
        ifd0.set_child(t::EXIF_IFD, exif);
        TiffWriter::new(order, false).write(&[ifd0]).unwrap()
    }

    #[test]
    fn dynamic_range_optimizer_is_read_from_the_sony_note() {
        use lightcraft_tiff::ByteOrder::{Big, Little};
        let long = |order, v: u32| match order {
            Little => v.to_le_bytes(),
            Big => v.to_be_bytes(),
        };
        let short = |order, v: u16| match order {
            Little => [v.to_le_bytes()[0], v.to_le_bytes()[1], 0, 0],
            Big => [v.to_be_bytes()[0], v.to_be_bytes()[1], 0, 0],
        };
        let dro = |bytes: &[u8]| crate::embedded_preview_dynamic_range_optimized(bytes);
        for order in [Little, Big] {
            // every documented value: Off; Standard, Advanced Auto, Auto (the factory default); Advanced Lv1–5; Lv1–8
            let documented: Vec<u32> = [0, 1, 2, 3].into_iter().chain(8..=12).chain(16..=23).collect();
            for &value in &documented {
                let file = sony_file(order, "SONY", &[(MN_DYNAMIC_RANGE_OPTIMIZER, 4, 1, long(order, value))], None);
                assert_eq!(dro(&file), Some(value != 0), "{order:?} {value}");
            }
            // undocumented values say nothing
            for value in (0..=64).filter(|v| !documented.contains(v)).chain([255, 65535, u32::MAX]) {
                let file = sony_file(order, "SONY", &[(MN_DYNAMIC_RANGE_OPTIMIZER, 4, 1, long(order, value))], None);
                assert_eq!(dro(&file), None, "{order:?} {value}");
            }
            // stored as a SHORT: same reading
            assert_eq!(dro(&sony_file(order, "SONY", &[(MN_DYNAMIC_RANGE_OPTIMIZER, 3, 1, short(order, 18))], None)), Some(true));
            // the coarse 0xb04f alone is not used, nor is an ASCII or empty value
            assert_eq!(dro(&sony_file(order, "SONY", &[(0xb04f, 3, 1, short(order, 1))], None)), None);
            assert_eq!(dro(&sony_file(order, "SONY", &[(MN_DYNAMIC_RANGE_OPTIMIZER, 2, 2, *b"3\0\0\0")], None)), None);
            assert_eq!(dro(&sony_file(order, "SONY", &[(MN_DYNAMIC_RANGE_OPTIMIZER, 4, 0, [0; 4])], None)), None);
            // other makers' notes are not read as Sony's
            assert_eq!(dro(&sony_file(order, "NIKON CORPORATION", &[(MN_DYNAMIC_RANGE_OPTIMIZER, 4, 1, long(order, 3))], None)), None);
        }
    }

    #[test]
    fn hostile_sony_notes_are_read_safely() {
        use lightcraft_tiff::ByteOrder::Little;
        let entry = [(MN_DYNAMIC_RANGE_OPTIMIZER, 4, 1, 3u32.to_le_bytes())];
        let good = sony_file(Little, "SONY", &entry, None);
        assert_eq!(crate::embedded_preview_dynamic_range_optimized(&good), Some(true));
        // an entry count far beyond the note: at most the entries actually there are read
        assert_ne!(crate::embedded_preview_dynamic_range_optimized(&sony_file(Little, "SONY", &entry, Some(u16::MAX))), Some(false));
        // an entry pointing outside the file
        let outside = [(MN_DYNAMIC_RANGE_OPTIMIZER, 4, 2, 0xffff_fff0u32.to_le_bytes())];
        assert_eq!(crate::embedded_preview_dynamic_range_optimized(&sony_file(Little, "SONY", &outside, None)), None);
        // every truncation of a good file, and no file at all
        for len in 0..good.len() {
            let _ = crate::embedded_preview_dynamic_range_optimized(&good[..len]);
        }
        assert_eq!(crate::embedded_preview_dynamic_range_optimized(b"not a TIFF"), None);
        assert_eq!(crate::embedded_preview_dynamic_range_optimized(&[]), None);
        // a Sony file without a maker note
        let mut ifd0 = lightcraft_tiff::IfdBuilder::new();
        ifd0.set(t::MAKE, lightcraft_tiff::Value::Ascii("SONY".into()));
        assert_eq!(crate::embedded_preview_dynamic_range_optimized(&lightcraft_tiff::TiffWriter::new(Little, false).write(&[ifd0]).unwrap()), None);
    }

    #[test]
    fn rggb_gains_need_four_plausible_levels() {
        assert_eq!(rggb_gains(&[2048.0, 1024.0, 1024.0, 1536.0]), Some([2.0, 1.0, 1.5]));
        assert_eq!(rggb_gains(&[2048.0, 1000.0, 1048.0, 1536.0]), Some([2.0, 1.0, 1.5]));
        for bad in [
            &[2048.0, 1024.0, 1024.0][..],
            &[2048.0, 1024.0, 1024.0, 1536.0, 1.0],
            &[2048.0, 1024.0, -1024.0, 1536.0],
            &[2048.0, 0.0, 0.0, 1536.0],
            &[2048.0, f64::NAN, 1024.0, 1536.0],
            &[-2048.0, 1024.0, 1024.0, 1536.0],
            &[9000.0, 1024.0, 1024.0, 1536.0],
            &[2048.0, 1024.0, 1024.0, f64::INFINITY],
        ] {
            assert_eq!(rggb_gains(bad), None, "{bad:?}");
        }
    }

    /// A 32 × 4 ARW2 file whose white balance can come from the raw IFD's `0x7313`, an encrypted `SR2SubIFD`
    /// (reached through `DNGPrivateData` and the `SR2Private` IFD, as Sony stores it, with `key`; black level 800)
    /// and a maker note whose `Tag2010` block (RX100 III layout) holds `WB_RGBLevels`.
    fn arw_with_wb(plain: Option<[u16; 4]>, sr2: Option<[u16; 4]>, tag2010: Option<[u16; 3]>, key: [u8; 4]) -> Vec<u8> {
        use lightcraft_tiff::{ByteOrder::Little, IfdBuilder, ImageData, TiffWriter, Value};
        let mut tag2010_block = vec![0u8; 700];
        for (i, v) in tag2010.unwrap_or([0; 3]).iter().enumerate() {
            tag2010_block[612 + 2 * i..614 + 2 * i].copy_from_slice(&v.to_le_bytes());
        }
        let tag2010_block = encipher(&tag2010_block);
        // the maker note: "SONY DSC " header, then an IFD whose offsets are file offsets
        let note = |at: usize| -> Vec<u8> {
            let mut n = b"SONY DSC \0\0\0".to_vec();
            n.extend([1, 0]);
            n.extend(MN_TAG2010.to_le_bytes());
            n.extend(7u16.to_le_bytes());
            n.extend((tag2010_block.len() as u32).to_le_bytes());
            n.extend(((at + n.len() + 8) as u32).to_le_bytes());
            n.extend([0; 4]);
            n.extend(&tag2010_block);
            n
        };
        let mut raw = IfdBuilder::new();
        raw.set(t::MAKE, Value::Ascii("SONY".into()));
        raw.set(t::MODEL, Value::Ascii("DSC-RX100M3".into()));
        raw.set(t::IMAGE_WIDTH, Value::Long(vec![32]));
        raw.set(t::IMAGE_LENGTH, Value::Long(vec![4]));
        raw.set(t::BITS_PER_SAMPLE, Value::Short(vec![12]));
        raw.set(t::SAMPLES_PER_PIXEL, Value::Short(vec![1]));
        raw.set(t::PHOTOMETRIC, Value::Short(vec![photometric::CFA]));
        raw.set(t::COMPRESSION, Value::Short(vec![32767]));
        raw.set(TONE_CURVE, Value::Short(vec![8000, 10400, 12900, 14100]));
        raw.set(t::DNG_PRIVATE_DATA, Value::Byte(vec![0; 4]));
        if let Some(levels) = plain {
            raw.set(WB_RGGB, Value::Short(levels.to_vec()));
        }
        if tag2010.is_some() {
            raw.set_child(t::EXIF_IFD, IfdBuilder::new().with(t::MAKER_NOTE, Value::Undefined(note(0))));
        }
        raw.set_image(ImageData::Strips { rows_per_strip: 4, strips: vec![vec![0u8; 32 * 4]] });
        let mut file = TiffWriter::default().write(&[raw]).unwrap();
        let parsed = Tiff::parse(&file).unwrap();
        if let Some(e) = parsed.exif().and_then(|e| e.get(t::MAKER_NOTE)) {
            let at = e.offset as usize;
            file[at..at + e.count()].copy_from_slice(&note(at));
        }
        if let Some(levels) = sr2 {
            // SR2Private IFD (three entries), then the encrypted SR2SubIFD
            let private = file.len();
            let start = private + 2 + 3 * 12 + 4;
            let block = super::super::sr2::tests::block(start, &[(BLACK_LEVEL, 3, &[800; 4]), (WB_RGGB, 8, &levels)], Little);
            file.extend([3, 0]);
            for (tag, kind, value) in
                [(0x7200u16, 4u16, (start as u32).to_le_bytes()), (0x7201, 4, (block.len() as u32).to_le_bytes()), (0x7221, 7, key)]
            {
                file.extend(tag.to_le_bytes());
                file.extend(kind.to_le_bytes());
                file.extend(if kind == 7 { 4u32 } else { 1 }.to_le_bytes());
                file.extend(value);
            }
            file.extend([0; 4]);
            file.extend(block);
            let at = parsed.ifds[0].get(t::DNG_PRIVATE_DATA).unwrap().offset as usize;
            file[at..at + 4].copy_from_slice(&(private as u32).to_le_bytes());
        }
        file
    }

    #[test]
    fn white_balance_prefers_the_applied_levels() {
        const KEY: [u8; 4] = [0x11, 0x22, 0x33, 0x44];
        let (plain, sr2, tag2010) = ([2048, 1024, 1024, 1536], [2932, 1024, 1024, 1576], [484, 256, 788]);
        let wb = |file: Vec<u8>| {
            let header = decode(&file, Mode::Header).unwrap();
            let full = decode(&file, Mode::Full).unwrap();
            assert_eq!(header.wb_multipliers, full.wb_multipliers);
            assert_eq!(header.black, full.black);
            full.wb_multipliers
        };
        // the same decrypted directory gives the black level, with the known key only
        let black = |file: Vec<u8>| decode(&file, Mode::Full).unwrap().black.mean();
        assert_eq!(black(arw_with_wb(None, Some(sr2), None, KEY)), 800.0);
        assert_eq!(black(arw_with_wb(None, Some(sr2), None, [1, 2, 3, 4])), 512.0);
        assert_eq!(black(arw_with_wb(None, None, None, KEY)), 512.0);
        let sr2_gains = Some([2932.0 / 1024.0, 1.0, 1576.0 / 1024.0]);
        // the encrypted SR2SubIFD's WB_RGGBLevels over Tag2010 (issue #535: ILCE-3500 at 5600 K) and when alone
        assert_eq!(wb(arw_with_wb(None, Some(sr2), Some(tag2010), KEY)), sr2_gains);
        assert_eq!(wb(arw_with_wb(None, Some(sr2), None, KEY)), sr2_gains);
        // the plain levels over both
        assert_eq!(wb(arw_with_wb(Some(plain), Some(sr2), Some(tag2010), KEY)), Some([2.0, 1.0, 1.5]));
        // Tag2010 when the SR2SubIFD has another key, implausible levels or none at all
        let fallback = Some([484.0 / 256.0, 1.0, 788.0 / 256.0]);
        assert_eq!(wb(arw_with_wb(None, Some(sr2), Some(tag2010), [1, 2, 3, 4])), fallback);
        assert_eq!(wb(arw_with_wb(None, Some([9000, 1024, 1024, 1576]), Some(tag2010), KEY)), fallback);
        assert_eq!(wb(arw_with_wb(None, None, Some(tag2010), KEY)), fallback);
        assert_eq!(wb(arw_with_wb(None, None, None, KEY)), None);
    }

    #[test]
    fn broken_sr2_private_data_is_ignored() {
        const KEY: [u8; 4] = [0x11, 0x22, 0x33, 0x44];
        let file = arw_with_wb(None, Some([2932, 1024, 1024, 1576]), None, KEY);
        let at = Tiff::parse(&file).unwrap().ifds[0].get(t::DNG_PRIVATE_DATA).unwrap().offset as usize;
        let private = u32::from_le_bytes(file[at..at + 4].try_into().unwrap()) as usize;
        let patched = |pos: usize, bytes: &[u8]| {
            let mut f = file.clone();
            f[pos..pos + bytes.len()].copy_from_slice(bytes);
            decode(&f, Mode::Full).map(|img| img.wb_multipliers)
        };
        // DNGPrivateData pointing nowhere or into the image; SR2SubIFD offset or length past the end, or enormous
        for (pos, bytes) in [
            (at, u32::MAX.to_le_bytes()),
            (at, 8u32.to_le_bytes()),
            (private + 2 + 8, u32::MAX.to_le_bytes()),
            (private + 2 + 12 + 8, u32::MAX.to_le_bytes()),
            (private + 2 + 12 + 8, (file.len() as u32).to_le_bytes()),
            (private + 2 + 12 + 8, (1u32 << 21).to_le_bytes()),
        ] {
            assert_eq!(patched(pos, &bytes).unwrap(), None, "{pos} {bytes:?}");
        }
        // the file cut anywhere inside the SR2 data still decodes (the image comes first), without white balance
        for len in [private, private + 20, file.len() - 30, file.len() - 1] {
            let img = decode(&file[..len], Mode::Full).unwrap();
            assert_eq!(img.wb_multipliers, None, "{len}");
        }
    }

    #[test]
    fn sr2_black_level_decodes_and_rejects_garbage() {
        use lightcraft_tiff::ByteOrder::Little;
        let plain = |levels: [u16; 4]| -> Vec<u8> { levels.iter().flat_map(|v| v.to_le_bytes()).collect() };
        assert_eq!(sr2_black_levels(&plain([800; 4]), Little), Some(800.0));
        assert_eq!(sr2_black_levels(&plain([512, 512, 514, 512]), Little), Some(512.5));
        assert_eq!(sr2_black_levels(&plain([65320, 64928, 65260, 488]), Little), None);
        assert_eq!(sr2_black_levels(&plain([0; 4]), Little), None);
        assert_eq!(sr2_black_levels(&plain([800; 4])[..5], Little), None);
        assert_eq!(sr2_black_levels(&[], Little), None);
    }

    /// An encrypted `SR2SubIFD` of `len` bytes at file offset `start` whose first entry is a black level stored at
    /// `value_pos` (relative), holding `levels`; `other` puts four more levels at position 2510 (where the old
    /// fixed read looked).
    fn sr2_block(start: usize, len: usize, tag: u16, value_pos: usize, levels: [u16; 4], other: Option<[u16; 4]>) -> Vec<u8> {
        let mut plain = vec![0u8; len];
        plain[0..2].copy_from_slice(&180u16.to_le_bytes());
        plain[2..4].copy_from_slice(&tag.to_le_bytes());
        plain[4..6].copy_from_slice(&3u16.to_le_bytes());
        plain[6..10].copy_from_slice(&4u32.to_le_bytes());
        plain[10..14].copy_from_slice(&((start + value_pos) as u32).to_le_bytes());
        for (i, v) in levels.iter().enumerate() {
            plain[value_pos + 2 * i..value_pos + 2 * i + 2].copy_from_slice(&v.to_le_bytes());
        }
        if let Some(o) = other {
            for (i, v) in o.iter().enumerate() {
                plain[2510 + 2 * i..2510 + 2 * i + 2].copy_from_slice(&v.to_le_bytes());
            }
        }
        super::super::sr2::tests::encrypt(&mut plain);
        plain
    }

    /// [`sr2_black`] on the encrypted `SR2SubIFD` bytes `block`, which start at file offset `start`.
    fn sr2_black_in(block: &[u8], start: usize, order: lightcraft_tiff::ByteOrder) -> Option<f32> {
        sr2_black(&super::super::sr2::SubIfd::decrypt(block, start, order))
    }

    #[test]
    fn sr2_black_level_follows_the_first_entry_to_its_layout() {
        use lightcraft_tiff::ByteOrder::Little;
        let start = 37584;
        // the value position of every layout seen, e.g. the DSLR-A700's (62112 bytes, value at 1638) and the
        // DSLR-A450/A500/A550's (27152 bytes, value at 2166); the first four are those of bodies without a plain 0x7310
        for pos in [1638, 2166, 2418, 2510, 2030, 2102, 2186, 2558, 2786] {
            for level in [512, 800] {
                let block = sr2_block(start, 27152, BLACK_LEVEL, pos, [level; 4], None);
                assert_eq!(sr2_black_in(&block, start, Little), Some(f32::from(level)), "value at {pos}");
            }
        }
        // the A700's layout holds four other equal values at 2510 (975 each); the old fixed read took those
        let a700 = sr2_block(start, 62112, BLACK_LEVEL, 1638, [512; 4], Some([975; 4]));
        assert_eq!(sr2_black_in(&a700, start, Little), Some(512.0));
        // not a black-level entry, another type, a value outside the block, before it
        assert_eq!(sr2_black_in(&sr2_block(start, 27152, 0x7311, 2166, [512; 4], None), start, Little), None);
        let mut long = sr2_block(start, 27152, BLACK_LEVEL, 2166, [512; 4], None);
        long[4] ^= 3 ^ 4; // SHORT -> LONG (type field of the first entry)
        assert_eq!(sr2_black_in(&long, start, Little), None);
        assert_eq!(sr2_black_in(&sr2_block(start, 27152, BLACK_LEVEL, 2166, [512; 4], None)[..2170], start, Little), None);
        let block = sr2_block(start, 27152, BLACK_LEVEL, 2166, [512; 4], None);
        assert_eq!(sr2_black_in(&block, start + 4000, Little), None, "value offset before the block");
        assert_eq!(sr2_black_in(&block[..10], start, Little), None, "truncated directory");
        assert_eq!(sr2_black_in(&[], start, Little), None);
        // implausible levels at a known position
        assert_eq!(sr2_black_in(&sr2_block(start, 27152, BLACK_LEVEL, 2166, [4; 4], None), start, Little), None);
    }

    #[test]
    fn word16_byte_order_follows_the_sample_range() {
        use lightcraft_tiff::ByteOrder::{Big, Little};
        let be: Vec<u8> = (0..4096u16).flat_map(|i| (512 + i * 3).to_be_bytes()).collect(); // 512..12800 big-endian words
        assert_eq!(word16_order(&be, 14, Little), Big);
        let le: Vec<u8> = (0..4096u16).flat_map(|i| (512 + i * 3).to_le_bytes()).collect();
        assert_eq!(word16_order(&le, 14, Little), Little);
        // values whose bytes are all below 0x40 fit both ways: keep the file order
        let amb: Vec<u8> = (0..4096usize).flat_map(|i| [(i % 60) as u8, (i % 50) as u8]).collect();
        assert_eq!(word16_order(&amb, 14, Little), Little);
        assert_eq!(word16_order(&[], 14, Big), Big);
        // any `bits` value (even a corrupt one) clamps to 14..=16 instead of overflowing the shift
        for bits in [0, 1, 12, 14, 16, 17, 31, 32, 33, u32::MAX] {
            let _ = word16_order(&be, bits, Little);
        }
    }

    #[test]
    fn quad_tiles_place_cfa_cells() {
        // 14×12 mosaic in 8×8 tiles (the right and bottom tiles overhang), each tile a 4×4 frame of 2×2 cells,
        // stored column by column in the file as Sony does while TileOffsets stay in raster order
        let (w, h, tw) = (14usize, 12usize, 8usize);
        let mosaic: Vec<u16> = (0..w * h).map(|i| 100 + (i * 37 % 4000) as u16).collect();
        let at = |x: usize, y: usize| if x < w && y < h { mosaic[y * w + x] } else { 0 };
        let mut file = vec![0u8; 16];
        let mut offsets = vec![0u64; 4];
        let mut counts = vec![0u64; 4];
        for tx in 0..2 {
            for ty in 0..2 {
                let mut frame = Vec::new();
                for fy in 0..tw / 2 {
                    for fx in 0..tw / 2 {
                        let (x, y) = (tx * tw + 2 * fx, ty * tw + 2 * fy);
                        frame.extend([at(x, y), at(x + 1, y), at(x, y + 1), at(x + 1, y + 1)]);
                    }
                }
                let enc = ljpeg::encode(&frame, tw / 2, tw / 2, 4, 14, 1, 0);
                offsets[ty * 2 + tx] = file.len() as u64;
                counts[ty * 2 + tx] = enc.len() as u64;
                file.extend(enc);
            }
        }
        let info = ImageInfo {
            width: w as u32,
            height: h as u32,
            bits_per_sample: vec![14],
            samples_per_pixel: 1,
            compression: 7,
            photometric: photometric::CFA,
            planar: 1,
            predictor: 1,
            sample_format: 1,
            new_subfile_type: 0,
            layout: Layout::Tiles { tile_width: tw as u32, tile_height: tw as u32 },
            offsets,
            byte_counts: counts,
        };
        assert!(is_quad_tiled(&file, &info));
        assert_eq!(read_quad_tiles(&file, &info).unwrap(), mosaic);
    }
}
