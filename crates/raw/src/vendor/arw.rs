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
//! White balance: the raw IFD's `0x7313` levels when present (bodies from about 2017 on); otherwise the
//! `WB_RGBLevels` of the maker note's enciphered `Tag2010` block (see [`DECIPHER`] and [`TAG2010_WB`]). Without
//! either, an ARW opened with unit multipliers, i.e. a strong green cast (issue #148). Black level: the raw IFD's
//! `0x7310`, else the level stored in the encrypted `SR2SubIFD` (see [`SR2_BLACK_AT`]; 800 rather than the
//! default 512 on 1″-sensor bodies such as the RX100 series), else 512 (14-bit) / 128 (12-bit).
//!
//! ARW6 (Compressed RAW 2, compression 32766): `arw6.rs`, output in 2 × 14-bit units.
//!
//! Also: uncompressed 16-bit ARW, and lossless-compressed ARW (Compression 7, ILCE-7M4 and later): LJ92 tiles whose
//! frames hold one 2×2 CFA cell per four-component sample ([`read_quad_tiles`]). Other lossless-JPEG layouts go
//! through the generic TIFF path.

use super::arw6;
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
/// byte offset of `WB_RGBLevels` (three `u16`, the as-shot white balance as R, G, B gains; Tag2010g and h share
/// the offset). Newer bodies (Tag2010i) also write the levels in plain form in the raw IFD (`0x7313`), which takes
/// precedence.
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

/// As-shot white balance from the maker note's enciphered `Tag2010` block (bodies that do not write `0x7313` in
/// the raw IFD: most ARWs before 2017). `model` is the Exif model; Sony appends a regional "V" to some names
/// (SLT-A77V), which the documented lists omit.
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
            let f = ljpeg::decode_subsampled(src, limit)?;
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

/// `SR2Private` tags (ExifTool "Sony SR2Private" table; the IFD is referenced by IFD0's `DNGPrivateData`).
const SR2_SUBIFD_OFFSET: u16 = 0x7200;
const SR2_SUBIFD_LENGTH: u16 = 0x7201;
const SR2_SUBIFD_KEY: u16 = 0x7221;
/// Black level inside the encrypted `SR2SubIFD` (bodies that do not write `0x7310` in the raw IFD), recovered by
/// black-box known-plaintext analysis of CC0 raw.pixls.us samples; no description of Sony's cipher was used.
///
/// Every Sony ARW we examined (16 files, 15 bodies, 2012–2021) stores the same key (`11 22 33 44`), so its
/// keystream is the same in every file. The DSC-RX100M6 and ILCE-7M3 also write their black levels in plain form
/// (`0x7310`: 800 and 512); XOR-ing their `SR2SubIFD`s against each other matched `pack(512×4) ^ pack(800×4)` at
/// exactly one position, byte 2510, which gave the eight keystream bytes there. Applied to the other files at that
/// position they decode to four equal levels that agree with each sensor's dark-pixel floor: 800 on every 1″-sensor
/// body (RX10, RX100 … RX100M5), 512 on the APS-C/full-frame ones (NEX-5N/6, SLT-A77, ILCE-6000/7/7M2/7RM2), in
/// both `SR2SubIFD` sizes seen (29252 and 56958 bytes). Newer layouts (ILCE-7M4, 35398 bytes) put other data there
/// and are rejected by the plausibility checks; those bodies write `0x7310` anyway.
const SR2_BLACK_AT: usize = 2510;
const SR2_BLACK_KEYSTREAM: [u8; 8] = [0x43, 0xcb, 0x86, 0xb6, 0x11, 0xd2, 0x1a, 0x73];
const SR2_KEY: [u8; 4] = [0x11, 0x22, 0x33, 0x44];

/// The per-channel black level from the encrypted `SR2SubIFD` (see [`SR2_BLACK_AT`]), in 14-bit units; `None`
/// unless the file uses the known key and the four decoded levels are equal and plausible.
fn sr2_black(bytes: &[u8], ifd0: &Ifd, order: lightcraft_tiff::ByteOrder) -> Option<f32> {
    let private = ifd0.bytes(t::DNG_PRIVATE_DATA)?;
    let at = order.read_u32(private, 0)? as u64;
    let opts = lightcraft_tiff::ParseOptions { max_ifds: 1, max_depth: 0, follow_children: false, ..Default::default() };
    let (sr2, _) = lightcraft_tiff::parse_ifd_at(bytes, at, order, 0, false, &opts).ok()?;
    if sr2.bytes(SR2_SUBIFD_KEY)? != SR2_KEY {
        return None;
    }
    let (start, len) = (usize::try_from(sr2.u64(SR2_SUBIFD_OFFSET)?).ok()?, usize::try_from(sr2.u64(SR2_SUBIFD_LENGTH)?).ok()?);
    if len < SR2_BLACK_AT + 8 {
        return None;
    }
    sr2_black_levels(bytes.get(start.checked_add(SR2_BLACK_AT)?..start.checked_add(SR2_BLACK_AT + 8)?)?, order)
}

/// Decipher the eight `SR2SubIFD` bytes at [`SR2_BLACK_AT`] into one black level (the mean of four near-equal,
/// plausible per-channel levels).
fn sr2_black_levels(cipher: &[u8], order: lightcraft_tiff::ByteOrder) -> Option<f32> {
    let plain: Vec<u8> = cipher.iter().zip(SR2_BLACK_KEYSTREAM).map(|(c, k)| c ^ k).collect();
    let levels: Vec<u16> = (0..4).filter_map(|i| order.read_u16(&plain, 2 * i)).collect();
    let (&lo, &hi) = (levels.iter().min()?, levels.iter().max()?);
    (levels.len() == 4 && (64..=4096).contains(&lo) && hi - lo <= 64).then(|| levels.iter().map(|&v| f32::from(v)).sum::<f32>() / 4.0)
}

/// The image area for files without Sony's crop tags (`0x74c7/0x74c8`, written since about 2017): the DNG-style
/// default crop when the raw IFD has one, else the maker note's `FullImageSize` (the camera JPEG's size) anchored
/// at the top-left. Older bodies store a few columns of padding at the right edge of the raw frame (constant
/// values, 8–32 columns on the samples we checked) inside a frame 16–48 pixels wider than `FullImageSize`, so
/// the anchored crop removes them while keeping the CFA phase.
fn default_crop(raw: &Ifd, mn: Option<&makernote::MakerNote>, w: usize, h: usize) -> Rect {
    let full = Rect::new(0, 0, w, h);
    if let (Some([x, y]), Some([cw, ch])) = (raw.u64s(t::DEFAULT_CROP_ORIGIN).as_deref(), raw.u64s(t::DEFAULT_CROP_SIZE).as_deref())
        && *cw > 0
        && *ch > 0
    {
        return Rect::new(*x as usize, *y as usize, *cw as usize, *ch as usize).clipped(w, h);
    }
    match mn.and_then(|m| m.ifd.u64s(MN_FULL_IMAGE_SIZE)).as_deref() {
        // only a plausible trim: never more than 64 pixels per side, never an enlargement
        Some([fh, fw]) if *fw as usize <= w && *fh as usize <= h && *fw as usize + 64 >= w && *fh as usize + 64 >= h => {
            Rect::new(0, 0, *fw as usize, *fh as usize)
        }
        _ => full,
    }
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
    let (data, out_bits) = match info.compression {
        7 if linear_rgb => (RawData::U16(read_ycbcr_tiles(bytes, &info, raw, mode)?), 14),
        32767 if chunks.len() == 1 && strip_len >= (w * h) as u64 && strip_len < (w * h) as u64 * 5 / 4 && mode == Mode::Header => {
            chunk_bytes(bytes, &chunks[0]).ok_or_else(|| RawError::Corrupt("raw strip outside file".into()))?;
            (RawData::U16(Vec::new()), 14)
        }
        32767 if chunks.len() == 1 && strip_len >= (w * h) as u64 && strip_len < (w * h) as u64 * 5 / 4 => {
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
        32767 => return Err(RawError::Unsupported("Sony ARW version 1 / packed compressed variant".into())),
        7 if is_quad_tiled(bytes, &info) => match mode {
            Mode::Full => (RawData::U16(read_quad_tiles(bytes, &info)?), bits),
            Mode::Header => {
                check_image(bytes, &info)?;
                (RawData::U16(Vec::new()), bits)
            }
        },
        32766 if chunks.len() == 1 => {
            let src = chunk_bytes(bytes, &chunks[0]).ok_or_else(|| RawError::Corrupt("raw strip outside file".into()))?;
            (RawData::U16(arw6::decode(src, w, h, mode)?), 16)
        }
        32766 => return Err(RawError::Unsupported("ARW6 with more than one strip".into())),
        1 => {
            let packing = if strip_len >= (w * h * 2) as u64 { Packing::Word16 } else { Packing::Msb };
            (read_image_in(mode, bytes, &info, tiff.order, packing)?, bits)
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
    let black = match raw.f64s(BLACK_LEVEL).as_deref() {
        Some(v) if linear_rgb && !v.is_empty() => BlackLevel::uniform((v.iter().sum::<f64>() / v.len() as f64) as f32),
        Some([a, b, c, d]) => {
            BlackLevel { repeat_rows: 2, repeat_cols: 2, values: vec![*a as f32, *b as f32, *c as f32, *d as f32], ..Default::default() }
        }
        _ => BlackLevel::uniform(sr2_black(bytes, ifd0, tiff.order).filter(|_| scale_bits >= 14).unwrap_or(default_black)),
    };
    let white = if linear_rgb {
        16383.0
    } else {
        raw.f64(t::WHITE_LEVEL).map(|v| v as f32).filter(|v| *v > 0.0).unwrap_or_else(|| super::white_from_data(samples, scale_bits))
    };
    // ARW6 samples are in 2 x 14-bit units, so the file's 14-bit levels double
    let unit = if info.compression == 32766 { 2.0 } else { 1.0 };
    let black = BlackLevel { values: black.values.iter().map(|v| v * unit).collect(), ..black };
    let white = white * unit;
    let model = ifd0.string(t::MODEL).unwrap_or_default();
    let mn = tiff
        .exif()
        .and_then(|e| e.get(t::MAKER_NOTE))
        .and_then(|e| makernote::parse_makernote(bytes, e.offset, e.count() as u64, tiff.order, &ifd0.string(t::MAKE).unwrap_or_default()));
    let wb = raw
        .f64s(WB_RGGB)
        .filter(|v| v.len() == 4 && v[1] > 0.0 && v[0] > 0.0 && v[3] > 0.0)
        .map(|v| {
            let g = (v[1] + v[2]) / 2.0;
            [(v[0] / g) as f32, 1.0, (v[3] / g) as f32]
        })
        .or_else(|| mn.as_ref().and_then(|m| tag2010_wb(&model, m.ifd.bytes(MN_TAG2010)?, m.order)));
    let crop = match (raw.u64s(CROP_TOP_LEFT).as_deref(), raw.u64s(CROP_SIZE).as_deref()) {
        (Some([x, y]), Some([cw, ch])) if *cw > 0 && *ch > 0 => Rect::new(*x as usize, *y as usize, *cw as usize, *ch as usize).clipped(w, h),
        _ => default_crop(raw, mn.as_ref(), w, h),
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
        opcodes: OpcodeLists::default(),
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
        let RawData::U16(ref pixels) = full.data else {
            panic!("integer ARW");
        };
        assert_eq!(&pixels[..3], &[1140, 929, 1000]);
        let developed = full.develop(crate::Method::Bilinear).unwrap();
        assert_eq!((developed.width, developed.height), (4, 4));
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

    #[test]
    fn sr2_black_level_decodes_and_rejects_garbage() {
        use lightcraft_tiff::ByteOrder::Little;
        let cipher =
            |levels: [u16; 4]| -> Vec<u8> { levels.iter().flat_map(|v| v.to_le_bytes()).zip(SR2_BLACK_KEYSTREAM).map(|(p, k)| p ^ k).collect() };
        assert_eq!(sr2_black_levels(&cipher([800; 4]), Little), Some(800.0));
        assert_eq!(sr2_black_levels(&cipher([512, 512, 514, 512]), Little), Some(512.5));
        assert_eq!(sr2_black_levels(&cipher([65320, 64928, 65260, 488]), Little), None); // ILCE-7M4's layout
        assert_eq!(sr2_black_levels(&cipher([0; 4]), Little), None);
        assert_eq!(sr2_black_levels(&cipher([800; 4])[..5], Little), None);
        assert_eq!(sr2_black_levels(&[], Little), None);
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
