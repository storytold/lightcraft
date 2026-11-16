//! Y′CbCr to RGB for decoded video pictures, written from ITU-T H.273 (code points), ITU-R BT.601,
//! BT.709, BT.2020 and BT.2100 (matrices, ranges, transfer functions).
//!
//! A decoder hands over planar Y′CbCr ([`Picture`]: 8 to 16 bits, 4:2:0, 4:2:2 or 4:4:4, any
//! strides) and the stream's colour description ([`ColourDescription`], H.273 code points as the
//! H.264 / HEVC VUI and the MP4 `colr` box carry them). A [`Converter`] built once per stream turns
//! every picture into either
//!
//! - **display sRGB** ([`Converter::to_srgb8`]): RGBA, 8 bits, sRGB primaries and curve, for a
//!   texture on screen (video playback), or
//! - **linear Rec.2020** ([`Converter::to_linear_rec2020`]): the develop pipeline's working space,
//!   1.0 = SDR white (poster frames rendered like photos).
//!
//! The converter allocates nothing per picture beyond two chroma rows per band of rows; the caller
//! owns the output buffer and can reuse it from frame to frame.
//!
//! Interpretation choices, the same for both outputs:
//!
//! - **SDR transfer curves** (BT.709, BT.601, BT.2020 10/12-bit, sRGB, and BT.709 with extended range:
//!   xvYCC, BT.1361, clipped to SDR) are read with the sRGB curve,
//!   as browsers and most players show such video, so a picture looks the way the clip looks in a
//!   player. An 8-bit BT.709 stream in BT.709 primaries therefore converts to display sRGB without
//!   any linearisation (the fast path).
//! - **HDR** (PQ, HLG) gets a simple tone map to SDR: BT.2408 reference white (203 cd/m²) becomes
//!   SDR white, the range above it is compressed into a shoulder. Good enough for a thumbnail or a
//!   preview on an SDR screen, not a grading tool.
//! - **Unspecified or reserved code points** fall back to what the picture size implies: BT.709 for
//!   HD and larger, BT.601 (625 or 525 lines) for SD. They never make a conversion fail.
//! - **Chroma** is upsampled linearly, with the sample positions H.264 and HEVC use by default
//!   (horizontally co-sited with the left luma sample, vertically between the two rows for 4:2:0).
//! - Matrix 10 (BT.2020 constant luminance) is converted as the non-constant-luminance matrix,
//!   a small error on saturated colours. ICtCp (matrix 14) is refused ([`YuvError::Unsupported`]).

use lightcraft_color::{RgbSpace, Xy};

/// Why a picture could not be converted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum YuvError {
    /// The planes do not hold the picture the sizes and strides describe.
    #[error("bad picture layout: {0}")]
    Layout(String),
    /// A colour description this converter does not handle.
    #[error("unsupported colour description: {0}")]
    Unsupported(String),
}

/// Chroma sampling of a picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chroma {
    /// Chroma planes half the luma width and height (rounded up).
    C420,
    /// Chroma planes half the luma width (rounded up), full height.
    C422,
    /// Chroma planes the luma size.
    C444,
}

impl Chroma {
    /// (horizontal, vertical) subsampling shifts.
    pub fn shifts(self) -> (u32, u32) {
        match self {
            Chroma::C420 => (1, 1),
            Chroma::C422 => (1, 0),
            Chroma::C444 => (0, 0),
        }
    }

    /// Chroma plane size for a `width` × `height` picture.
    pub fn plane_size(self, width: usize, height: usize) -> (usize, usize) {
        let (sx, sy) = self.shifts();
        (width.div_ceil(1 << sx), height.div_ceil(1 << sy))
    }
}

/// The samples of one plane: one byte per sample (8-bit streams) or one `u16` per sample holding a
/// value below `1 << bit_depth` (deeper streams; 8-bit samples in `u16` are fine too).
#[derive(Clone, Copy, Debug)]
pub enum Samples<'a> {
    U8(&'a [u8]),
    U16(&'a [u16]),
}

impl Samples<'_> {
    fn len(&self) -> usize {
        match self {
            Samples::U8(s) => s.len(),
            Samples::U16(s) => s.len(),
        }
    }

    #[inline(always)]
    fn at(&self, i: usize) -> usize {
        match self {
            Samples::U8(s) => s.get(i).map_or(0, |&v| usize::from(v)),
            Samples::U16(s) => s.get(i).map_or(0, |&v| usize::from(v)),
        }
    }
}

/// One plane: its samples and the distance between rows, in samples.
#[derive(Clone, Copy, Debug)]
pub struct Plane<'a> {
    pub samples: Samples<'a>,
    pub stride: usize,
}

/// A decoded picture in planar Y′CbCr, borrowed from the decoder's output.
#[derive(Clone, Copy, Debug)]
pub struct Picture<'a> {
    /// Luma size (the visible, cropped picture).
    pub width: usize,
    pub height: usize,
    /// Bits per sample, 8 to 16.
    pub bit_depth: u8,
    pub chroma: Chroma,
    pub y: Plane<'a>,
    pub cb: Plane<'a>,
    pub cr: Plane<'a>,
}

impl Picture<'_> {
    /// Checks that every plane holds its rows (with checked arithmetic: sizes come from a stream).
    fn check(&self) -> Result<(), YuvError> {
        if self.width == 0 || self.height == 0 {
            return Err(YuvError::Layout(format!("empty picture {}×{}", self.width, self.height)));
        }
        if !(8..=16).contains(&self.bit_depth) {
            return Err(YuvError::Layout(format!("bit depth {} outside 8 to 16", self.bit_depth)));
        }
        let (cw, ch) = self.chroma.plane_size(self.width, self.height);
        for (name, plane, w, h) in [("luma", &self.y, self.width, self.height), ("Cb", &self.cb, cw, ch), ("Cr", &self.cr, cw, ch)] {
            if matches!(plane.samples, Samples::U8(_)) && self.bit_depth != 8 {
                return Err(YuvError::Layout(format!("{name} plane has 8-bit samples for a {}-bit picture", self.bit_depth)));
            }
            if plane.stride < w {
                return Err(YuvError::Layout(format!("{name} stride {} below width {w}", plane.stride)));
            }
            let need = (h - 1).checked_mul(plane.stride).and_then(|n| n.checked_add(w));
            if need.is_none_or(|n| n > plane.samples.len()) {
                return Err(YuvError::Layout(format!(
                    "{name} plane holds {} samples, {w}×{h} at stride {} needs more",
                    plane.samples.len(),
                    plane.stride
                )));
            }
        }
        Ok(())
    }
}

/// A colour description as ITU-T H.273 code points: `colour_primaries`, `transfer_characteristics`,
/// `matrix_coefficients` and the full-range flag, as the H.264 / HEVC VUI and the MP4 `colr` box
/// (`nclx`, QuickTime `nclc`) carry them. Code point 2 is "unspecified".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ColourDescription {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub full_range: bool,
}

/// H.273's "unspecified" code point.
pub const UNSPECIFIED: u8 = 2;

impl ColourDescription {
    /// Nothing known: every code point unspecified, limited range.
    pub const UNSPECIFIED: ColourDescription =
        ColourDescription { primaries: UNSPECIFIED, transfer: UNSPECIFIED, matrix: UNSPECIFIED, full_range: false };

    /// BT.709 SDR, limited range: what most HD cameras and phones write.
    pub const BT709: ColourDescription = ColourDescription { primaries: 1, transfer: 1, matrix: 1, full_range: false };

    /// Whether no code point says anything (all three unspecified).
    pub fn is_unspecified(&self) -> bool {
        self.primaries == UNSPECIFIED && self.transfer == UNSPECIFIED && self.matrix == UNSPECIFIED
    }

    /// Each unspecified code point taken from `other` (the range flag stays this one's).
    pub fn or(self, other: ColourDescription) -> ColourDescription {
        let pick = |a: u8, b: u8| if a == UNSPECIFIED { b } else { a };
        ColourDescription {
            primaries: pick(self.primaries, other.primaries),
            transfer: pick(self.transfer, other.transfer),
            matrix: pick(self.matrix, other.matrix),
            full_range: self.full_range,
        }
    }

    /// Whether the transfer function is HDR (PQ or HLG).
    pub fn is_hdr(&self) -> bool {
        matches!(self.transfer, 16 | 18)
    }
}

/// The transfer function a stream's samples are read with.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Transfer {
    /// The SDR video curves, read as sRGB (see the module documentation).
    Sdr,
    Linear,
    Gamma(f32),
    Pq,
    Hlg,
}

impl Transfer {
    fn from_code(code: u8) -> Option<Transfer> {
        Some(match code {
            // (11 and 12, xvYCC and BT.1361, are the BT.709 curve with room beyond black and white,
            // which an SDR picture clips)
            1 | 6 | 11 | 12 | 13 | 14 | 15 => Transfer::Sdr,
            4 => Transfer::Gamma(2.2),
            5 => Transfer::Gamma(2.8),
            8 => Transfer::Linear,
            16 => Transfer::Pq,
            18 => Transfer::Hlg,
            _ => return None,
        })
    }
}

/// How Y′CbCr becomes R′G′B′.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Matrix {
    /// R′G′B′ stored as G, B, R (H.273 matrix 0).
    Identity,
    /// Luma weights Kr, Kb.
    Kr { kr: f32, kb: f32 },
    /// YCgCo (H.273 matrix 8).
    YCgCo,
}

/// What the picture size implies when a stream says nothing: (matrix, primaries) codes.
fn size_default(width: usize, height: usize) -> (u8, u8) {
    if width >= 1280 || height > 576 {
        (1, 1)
    } else if height > 480 {
        (5, 5)
    } else {
        (6, 6)
    }
}

fn primaries_of(code: u8) -> Option<RgbSpace> {
    let space = |name, r: (f64, f64), g: (f64, f64), b: (f64, f64), w: (f64, f64)| RgbSpace {
        name,
        r: Xy::new(r.0, r.1),
        g: Xy::new(g.0, g.1),
        b: Xy::new(b.0, b.1),
        white: Xy::new(w.0, w.1),
    };
    const D65: (f64, f64) = (0.3127, 0.3290);
    Some(match code {
        1 => lightcraft_color::SRGB,
        4 => space("BT.470 System M", (0.67, 0.33), (0.21, 0.71), (0.14, 0.08), (0.310, 0.316)),
        5 => space("BT.601 625", (0.64, 0.33), (0.29, 0.60), (0.15, 0.06), D65),
        6 | 7 => space("BT.601 525", (0.630, 0.340), (0.310, 0.595), (0.155, 0.070), D65),
        9 => lightcraft_color::REC2020,
        11 => space("DCI-P3", (0.680, 0.320), (0.265, 0.690), (0.150, 0.060), (0.314, 0.351)),
        12 => lightcraft_color::DISPLAY_P3,
        22 => space("EBU Tech 3213", (0.630, 0.340), (0.295, 0.605), (0.155, 0.077), D65),
        _ => return None,
    })
}

fn matrix_of(code: u8, primaries: &RgbSpace) -> Result<Option<Matrix>, YuvError> {
    let kr = |kr: f32, kb: f32| Some(Matrix::Kr { kr, kb });
    Ok(match code {
        0 => Some(Matrix::Identity),
        1 => kr(0.2126, 0.0722),
        4 => kr(0.30, 0.11),
        5 | 6 => kr(0.299, 0.114),
        7 => kr(0.212, 0.087),
        8 => Some(Matrix::YCgCo),
        9 | 10 => kr(0.2627, 0.0593),
        // chromaticity-derived: the luma weights of the primaries (constant luminance read as non-constant)
        12 | 13 => {
            let l = primaries.luma();
            kr(l[0] as f32, l[2] as f32)
        }
        14 => return Err(YuvError::Unsupported("ICtCp (matrix 14)".into())),
        _ => None,
    })
}

/// Entries of the transfer lookup tables (over code values 0..=1).
const CURVE_STEPS: usize = 4096;

/// Peak luminance assumed for HDR content, in cd/m² (BT.2100 HLG's nominal display peak; a common
/// mastering peak for PQ).
const HDR_PEAK_NITS: f32 = 1000.0;
/// BT.2408 HDR reference white, mapped to SDR white.
const REFERENCE_WHITE_NITS: f32 = 203.0;
/// Below this (relative to SDR white) the tone map leaves HDR luminance alone.
const SHOULDER_KNEE: f32 = 0.75;

/// The output a conversion writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Output {
    Srgb8,
    Linear2020,
}

/// Converts the pictures of one stream (one colour description and bit depth). Cheap to keep;
/// building one makes a few small tables.
#[derive(Clone, Debug)]
pub struct Converter {
    bit_depth: u8,
    matrix: Matrix,
    transfer: Transfer,
    /// Luma code value → Y′ (0..1 nominal).
    y_lut: Vec<f32>,
    /// Chroma code value → Cb / Cr (−0.5..0.5 nominal).
    c_lut: Vec<f32>,
    /// Signal → linear (relative to SDR white for SDR; for PQ relative to reference white; for HLG
    /// scene light 0..1).
    eotf: Vec<f32>,
    /// Linear source primaries → linear sRGB, and → linear Rec.2020.
    to_srgb: [[f32; 3]; 3],
    to_2020: [[f32; 3]; 3],
    /// Luma weights of the source primaries (HLG's OOTF and the tone map).
    luma: [f32; 3],
    /// Source is display sRGB already (SDR curve, BT.709 primaries): no linearisation for sRGB output.
    srgb_passthrough: bool,
    /// The resolved colour description.
    resolved: ColourDescription,
}

impl Converter {
    /// A converter for pictures of `bit_depth` bits (8 to 16) described by `desc`. `width` and
    /// `height` (the luma size) pick the defaults for unspecified code points.
    pub fn new(desc: ColourDescription, bit_depth: u8, width: usize, height: usize) -> Result<Converter, YuvError> {
        if !(8..=16).contains(&bit_depth) {
            return Err(YuvError::Layout(format!("bit depth {bit_depth} outside 8 to 16")));
        }
        let (def_matrix, def_primaries) = size_default(width, height);
        let (primaries_code, primaries) = match primaries_of(desc.primaries) {
            Some(p) => (desc.primaries, p),
            None => (def_primaries, primaries_of(def_primaries).unwrap_or(lightcraft_color::SRGB)),
        };
        let (matrix_code, matrix) = match matrix_of(desc.matrix, &primaries)? {
            Some(m) => (desc.matrix, m),
            None => (def_matrix, matrix_of(def_matrix, &primaries)?.unwrap_or(Matrix::Kr { kr: 0.2126, kb: 0.0722 })),
        };
        let (transfer_code, transfer) = match Transfer::from_code(desc.transfer) {
            Some(t) => (desc.transfer, t),
            None => (1, Transfer::Sdr),
        };
        let resolved = ColourDescription { primaries: primaries_code, transfer: transfer_code, matrix: matrix_code, full_range: desc.full_range };
        let to_srgb = primaries.to_space(&lightcraft_color::SRGB).to_f32();
        let to_2020 = primaries.to_space(&lightcraft_color::REC2020).to_f32();
        let l = primaries.luma();
        Ok(Converter {
            bit_depth,
            matrix,
            transfer,
            y_lut: range_lut(bit_depth, desc.full_range, false),
            c_lut: range_lut(bit_depth, desc.full_range, true),
            eotf: eotf_lut(transfer),
            to_srgb,
            to_2020,
            luma: [l[0] as f32, l[1] as f32, l[2] as f32],
            srgb_passthrough: transfer == Transfer::Sdr && is_identity(&to_srgb),
            resolved,
        })
    }

    /// The description actually used: unspecified and reserved code points replaced by defaults.
    pub fn resolved(&self) -> ColourDescription {
        self.resolved
    }

    /// Whether the stream is HDR (PQ or HLG) and is tone mapped.
    pub fn is_hdr(&self) -> bool {
        matches!(self.transfer, Transfer::Pq | Transfer::Hlg)
    }

    /// Bits per sample this converter was built for.
    pub fn bit_depth(&self) -> u8 {
        self.bit_depth
    }

    /// Converts `pic` to display sRGB (RGBA, alpha 255) in `out`, which must hold exactly
    /// `width × height` pixels, row by row.
    pub fn to_srgb8(&self, pic: &Picture<'_>, out: &mut [[u8; 4]]) -> Result<(), YuvError> {
        self.check(pic, out.len())?;
        let enc = srgb_encode_lut();
        for_rows(out, pic.width, |y, row, scratch| {
            self.row(pic, y, scratch, Output::Srgb8, |x, rgb| {
                if let Some(px) = row.get_mut(x) {
                    *px = if self.srgb_passthrough {
                        [unit_u8(rgb[0]), unit_u8(rgb[1]), unit_u8(rgb[2]), 255]
                    } else {
                        [enc_u8(enc, rgb[0]), enc_u8(enc, rgb[1]), enc_u8(enc, rgb[2]), 255]
                    };
                }
            });
        });
        Ok(())
    }

    /// Converts `pic` to linear-light Rec.2020 (1.0 = SDR white) in `out`, which must hold exactly
    /// `width × height` pixels, row by row.
    pub fn to_linear_rec2020(&self, pic: &Picture<'_>, out: &mut [[f32; 3]]) -> Result<(), YuvError> {
        self.check(pic, out.len())?;
        for_rows(out, pic.width, |y, row, scratch| {
            self.row(pic, y, scratch, Output::Linear2020, |x, rgb| {
                if let Some(px) = row.get_mut(x) {
                    *px = rgb;
                }
            });
        });
        Ok(())
    }

    fn check(&self, pic: &Picture<'_>, out_len: usize) -> Result<(), YuvError> {
        pic.check()?;
        if pic.bit_depth != self.bit_depth {
            return Err(YuvError::Layout(format!("a {}-bit picture for a {}-bit converter", pic.bit_depth, self.bit_depth)));
        }
        if pic.width.checked_mul(pic.height) != Some(out_len) {
            return Err(YuvError::Layout(format!("output holds {out_len} pixels, the picture has {}×{}", pic.width, pic.height)));
        }
        Ok(())
    }

    /// Converts luma row `y`, handing each pixel to `put` as linear sRGB (or, on the sRGB
    /// pass-through, the signal itself) or linear Rec.2020.
    fn row(&self, pic: &Picture<'_>, y: usize, scratch: &mut Scratch, output: Output, mut put: impl FnMut(usize, [f32; 3])) {
        let w = pic.width;
        let Scratch { cb, cr } = scratch;
        cb.resize(w, 0.0);
        cr.resize(w, 0.0);
        self.chroma_row(pic, &pic.cb, y, cb);
        self.chroma_row(pic, &pic.cr, y, cr);
        let ybase = y * pic.y.stride;
        let ymax = self.y_lut.len() - 1;
        for x in 0..w {
            let luma = self.y_lut[pic.y.samples.at(ybase + x).min(ymax)];
            let signal = self.rgb_signal(luma, cb[x], cr[x]);
            put(x, self.finish(signal, output));
        }
    }

    /// Chroma row for luma row `y`, upsampled to the luma width (in Cb / Cr units).
    fn chroma_row(&self, pic: &Picture<'_>, plane: &Plane<'_>, y: usize, out: &mut [f32]) {
        let (sx, sy) = pic.chroma.shifts();
        let (cw, ch) = pic.chroma.plane_size(pic.width, pic.height);
        // (GBR stores B and R with luma's offset and scale)
        let lut = if self.matrix == Matrix::Identity { &self.y_lut } else { &self.c_lut };
        let cmax = lut.len() - 1;
        let at = |cx: usize, cy: usize| lut[plane.samples.at(cy.min(ch - 1) * plane.stride + cx.min(cw - 1)).min(cmax)];
        // vertical: 4:2:0 chroma sits between luma rows 2i and 2i+1
        let (r0, r1, w1) = if sy == 0 {
            (y, y, 0.0)
        } else {
            let i = y >> 1;
            if y & 1 == 0 { (i, i.saturating_sub(1), 0.25) } else { (i, i + 1, 0.25) }
        };
        let sample = |cx: usize| at(cx, r0) * (1.0 - w1) + at(cx, r1) * w1;
        for (x, o) in out.iter_mut().enumerate() {
            *o = if sx == 0 {
                sample(x)
            } else if x & 1 == 0 {
                sample(x >> 1)
            } else {
                // horizontally co-sited with the even luma sample: odd ones sit halfway
                (sample(x >> 1) + sample((x >> 1) + 1)) * 0.5
            };
        }
    }

    #[inline(always)]
    fn rgb_signal(&self, y: f32, cb: f32, cr: f32) -> [f32; 3] {
        match self.matrix {
            Matrix::Kr { kr, kb } => {
                let kg = 1.0 - kr - kb;
                let r = y + 2.0 * (1.0 - kr) * cr;
                let b = y + 2.0 * (1.0 - kb) * cb;
                let g = if kg.abs() > 1e-6 { (y - kr * r - kb * b) / kg } else { y };
                [r, g, b]
            }
            // G in the luma plane, B in Cb, R in Cr
            Matrix::Identity => [cr, y, cb],
            Matrix::YCgCo => {
                let t = y - cb;
                [t + cr, y + cb, t - cr]
            }
        }
    }

    /// Signal R′G′B′ → the output's RGB.
    #[inline(always)]
    fn finish(&self, s: [f32; 3], output: Output) -> [f32; 3] {
        if output == Output::Srgb8 && self.srgb_passthrough {
            return s;
        }
        let lin = self.linearise(s);
        match output {
            Output::Srgb8 => apply(&self.to_srgb, lin),
            Output::Linear2020 => apply(&self.to_2020, lin).map(|v| v.max(0.0)),
        }
    }

    /// Signal → linear light relative to SDR white, HDR tone mapped.
    #[inline(always)]
    fn linearise(&self, s: [f32; 3]) -> [f32; 3] {
        let lin = s.map(|v| curve(&self.eotf, v));
        match self.transfer {
            Transfer::Pq => self.tone_map(lin),
            Transfer::Hlg => {
                // BT.2100 OOTF for a 1000 cd/m² display (system gamma 1.2), then relative to reference white
                let ys = dot(self.luma, lin).max(0.0);
                let gain = if ys > 0.0 { ys.powf(0.2) } else { 0.0 } * (HDR_PEAK_NITS / REFERENCE_WHITE_NITS);
                self.tone_map(lin.map(|v| v * gain))
            }
            _ => lin,
        }
    }

    /// Compresses luminance above [`SHOULDER_KNEE`] into a shoulder that approaches SDR white
    /// (keeps the colour's ratios).
    fn tone_map(&self, rgb: [f32; 3]) -> [f32; 3] {
        let l = dot(self.luma, rgb);
        if l.is_nan() || l <= SHOULDER_KNEE {
            return rgb;
        }
        let span = 1.0 - SHOULDER_KNEE;
        let mapped = SHOULDER_KNEE + span * (1.0 - (-(l - SHOULDER_KNEE) / span).exp());
        rgb.map(|v| v * mapped / l)
    }
}

/// Code value → normalised value for one bit depth and range (BT.601 / BT.709 / BT.2100 scaling).
fn range_lut(bit_depth: u8, full: bool, chroma: bool) -> Vec<f32> {
    let n = 1usize << bit_depth;
    let scale = (1u32 << (bit_depth - 8)) as f32;
    let max = (n - 1) as f32;
    let (offset, span) = match (full, chroma) {
        (false, false) => (16.0 * scale, 219.0 * scale),
        (false, true) => (128.0 * scale, 224.0 * scale),
        (true, false) => (0.0, max),
        (true, true) => ((1u32 << (bit_depth - 1)) as f32, max),
    };
    (0..n).map(|v| (v as f32 - offset) / span).collect()
}

fn eotf_lut(t: Transfer) -> Vec<f32> {
    (0..=CURVE_STEPS)
        .map(|i| {
            let v = i as f32 / CURVE_STEPS as f32;
            match t {
                Transfer::Sdr => lightcraft_color::transfer::srgb_to_linear(v),
                Transfer::Linear => v,
                Transfer::Gamma(g) => v.powf(g),
                Transfer::Pq => (lightcraft_color::transfer::pq_decode(f64::from(v)) * 10_000.0 / f64::from(REFERENCE_WHITE_NITS)) as f32,
                Transfer::Hlg => lightcraft_color::transfer::hlg_decode(f64::from(v)) as f32,
            }
        })
        .collect()
}

/// `lut` (over 0..=1) at `v`, linearly interpolated; values outside 0..1 are clipped (the SDR
/// signal range; out-of-gamut Y′CbCr combinations land there).
#[inline(always)]
fn curve(lut: &[f32], v: f32) -> f32 {
    if v.is_nan() || v <= 0.0 {
        return lut.first().copied().unwrap_or(0.0);
    }
    let p = v.min(1.0) * CURVE_STEPS as f32;
    let i = (p as usize).min(CURVE_STEPS - 1);
    let f = p - i as f32;
    let a = lut.get(i).copied().unwrap_or(0.0);
    let b = lut.get(i + 1).copied().unwrap_or(a);
    a + (b - a) * f
}

#[inline(always)]
fn apply(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v)]
}

#[inline(always)]
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn is_identity(m: &[[f32; 3]; 3]) -> bool {
    (0..3).all(|r| (0..3).all(|c| (m[r][c] - if r == c { 1.0 } else { 0.0 }).abs() < 1e-4))
}

#[inline(always)]
fn unit_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Linear light → 8-bit sRGB, by table over 0..=1.
fn srgb_encode_lut() -> &'static [u8] {
    static LUT: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    LUT.get_or_init(|| (0..=CURVE_STEPS).map(|i| unit_u8(lightcraft_color::transfer::linear_to_srgb(i as f32 / CURVE_STEPS as f32))).collect())
}

#[inline(always)]
fn enc_u8(lut: &[u8], v: f32) -> u8 {
    if v.is_nan() || v <= 0.0 {
        return 0;
    }
    lut.get((v.min(1.0) * CURVE_STEPS as f32 + 0.5) as usize).copied().unwrap_or(255)
}

/// Chroma rows upsampled to the luma width, reused across the rows of one band.
#[derive(Default)]
struct Scratch {
    cb: Vec<f32>,
    cr: Vec<f32>,
}

/// Rows per band: each band gets one [`Scratch`] (and, with the `parallel` feature, one task).
const BAND_ROWS: usize = 16;

/// Runs `f(y, row, scratch)` for every output row, in parallel bands when the `parallel` feature is on.
fn for_rows<T: Send>(out: &mut [T], width: usize, f: impl Fn(usize, &mut [T], &mut Scratch) + Sync) {
    if width == 0 {
        return;
    }
    let band = |(b, rows): (usize, &mut [T])| {
        let mut scratch = Scratch::default();
        for (i, row) in rows.chunks_mut(width).enumerate() {
            f(b * BAND_ROWS + i, row, &mut scratch);
        }
    };
    let band_len = width.saturating_mul(BAND_ROWS);
    #[cfg(feature = "parallel")]
    {
        use rayon::prelude::*;
        out.par_chunks_mut(band_len).enumerate().for_each(band);
    }
    #[cfg(not(feature = "parallel"))]
    out.chunks_mut(band_len).enumerate().for_each(band);
}

#[cfg(test)]
#[path = "yuv_tests.rs"]
mod tests;
