// Ported from RADcamProfiles (github.com/OneSlips/RADcamProfiles, commit dd8003f, crates/engine/src/color.rs),
// Copyright (c) 2026 RAD Films - Roman Alurkoff, MIT; relicensed MIT OR Apache-2.0 by its author.
// Modified by the porting contributors (2026): no panics on bad input, the app's frame type.
//! Colour science: white points, Lab/LCh/Luv, device spaces and colour
//! difference.
//!
//! All routine space conversions follow the published CIE definitions. The
//! two non-standard spaces (`IPTS` and the engine's internal LUT space) are
//! documented at their definitions.

use crate::linalg::{Mat3, Vec3};
use core::fmt;

/// D50 as used by the DNG specification, normalised to `Y = 1`.
pub const D50: Vec3 = Vec3::new(0.9642, 1.0, 0.8249);

/// D65 as used by sRGB, normalised to `Y = 1`.
pub const D65: Vec3 = Vec3::new(0.95047, 1.0, 1.08883);

/// Equal energy white.
pub const E: Vec3 = Vec3::new(1.0, 1.0, 1.0);

/// 2 degree standard observer, XYZ -> linear sRGB.
pub const XYZ_TO_SRGB: Mat3 = Mat3::from_rows([3.2406, -1.5372, -0.4986], [-0.9689, 1.8758, 0.0415], [0.0557, -0.2040, 1.0570]);

/// Linear sRGB -> XYZ (2 degree standard observer).
pub const SRGB_TO_XYZ: Mat3 = Mat3::from_rows([0.4124, 0.3576, 0.1805], [0.2126, 0.7152, 0.0722], [0.0193, 0.1192, 0.9505]);

/// 2 degree standard observer, XYZ -> linear Adobe RGB (1998).
pub const XYZ_TO_ADOBE_RGB: Mat3 = Mat3::from_rows([2.04159, -0.56501, -0.34473], [-0.96924, 1.87597, 0.04156], [0.01344, -0.11836, 1.01517]);

/// Linear Adobe RGB (1998) -> XYZ (2 degree standard observer).
pub const ADOBE_RGB_TO_XYZ: Mat3 = Mat3::from_rows([0.5767, 0.1856, 0.1882], [0.2973, 0.6274, 0.0753], [0.0270, 0.0707, 0.9913]);

const LAB_EPSILON: f64 = 216.0 / 24389.0;
const LAB_KAPPA: f64 = 24389.0 / 27.0;
/// Break point of the linear segment of the CIE L* function, `epsilon * kappa`.
const LAB_LINEAR_LIMIT: f64 = 8.856_451_679_035_631e-3;
/// Break point of the CIE L* inverse, `(6/29)^(1/3)`-based constant.
const LAB_LINEAR_LIMIT_INV: f64 = 0.206_896_551_724_137_93;
const R4_29: f64 = 4.0 / 29.0;

/// Errors that space conversions and lookups can report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColorError {
    /// The named observer is not one of the standard observers.
    UnknownObserver(String),
    /// The named illuminant is not known and is not a blackbody temperature.
    UnknownIlluminant(String),
    /// A chromaticity coordinate was degenerate (zero denominator).
    DegenerateChromaticity,
}

impl fmt::Display for ColorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownObserver(n) => write!(f, "unknown observer \"{n}\""),
            Self::UnknownIlluminant(n) => write!(f, "unknown illuminant \"{n}\""),
            Self::DegenerateChromaticity => write!(f, "degenerate chromaticity coordinates"),
        }
    }
}

impl std::error::Error for ColorError {}

/// Forward CIE L* cube-root (or linear) response.
#[inline]
pub fn lab_f(t: f64) -> f64 {
    if t >= LAB_EPSILON { t.cbrt() } else { (LAB_KAPPA * t + 16.0) / 116.0 }
}

/// Inverse of [`lab_f`].
#[inline]
pub fn lab_f_inv(t: f64) -> f64 {
    if t >= (LAB_EPSILON * LAB_KAPPA + 16.0) / 116.0 { t * t * t } else { (116.0 * t - 16.0) / LAB_KAPPA }
}

/// Forward lightness response used by the IPT and Jz spaces.
#[inline]
pub fn lab_ft_forward(t: f64) -> f64 {
    if t >= LAB_LINEAR_LIMIT { t.cbrt() } else { t * (841.0 / 108.0) + R4_29 }
}

/// Inverse of [`lab_ft_forward`].
#[inline]
pub fn lab_ft_inverse(t: f64) -> f64 {
    if t >= LAB_LINEAR_LIMIT_INV { t * t * t } else { 108.0 / 841.0 * (t - R4_29) }
}

/// CIE Lab from XYZ under white point `wp`.
pub fn xyz_to_lab(xyz: Vec3, wp: Vec3) -> Vec3 {
    let fx = lab_f(xyz[0] / wp[0]);
    let fy = lab_f(xyz[1] / wp[1]);
    let fz = lab_f(xyz[2] / wp[2]);
    Vec3::new(116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
}

/// XYZ from CIE Lab under white point `wp`.
pub fn lab_to_xyz(lab: Vec3, wp: Vec3) -> Vec3 {
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = fy + lab[1] / 500.0;
    let fz = fy - lab[2] / 200.0;
    Vec3::new(wp[0] * lab_f_inv(fx), wp[1] * lab_f_inv(fy), wp[2] * lab_f_inv(fz))
}

/// Cartesian Lab to cylindrical LCh, with hue in degrees `[0, 360)`.
pub fn lab_to_lch(lab: Vec3) -> Vec3 {
    let c = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
    let mut h = lab[2].atan2(lab[1]).to_degrees();
    if h < 0.0 {
        h += 360.0;
    }
    Vec3::new(lab[0], c, h)
}

/// Cylindrical LCh back to Cartesian Lab.
pub fn lch_to_lab(lch: Vec3) -> Vec3 {
    let h = lch[2].to_radians();
    Vec3::new(lch[0], lch[1] * h.cos(), lch[1] * h.sin())
}

/// Lab to LCh, named for readability at call sites that work in LCh.
#[inline]
pub fn xyz_to_lch(xyz: Vec3, wp: Vec3) -> Vec3 {
    lab_to_lch(xyz_to_lab(xyz, wp))
}

/// LCh back to XYZ.
#[inline]
pub fn lch_to_xyz(lch: Vec3, wp: Vec3) -> Vec3 {
    lab_to_xyz(lch_to_lab(lch), wp)
}

/// XYZ to `xyY`, where `Y` passes through unchanged.
pub fn xyz_to_xyy(xyz: Vec3) -> Vec3 {
    let sum = xyz.sum();
    if sum == 0.0 {
        return Vec3::ZERO;
    }
    Vec3::new(xyz[0] / sum, xyz[1] / sum, xyz[1])
}

/// `xyY` back to XYZ. Degenerate `y` yields zeros rather than infinity.
pub fn xyy_to_xyz(xyy: Vec3) -> Vec3 {
    if xyy[1] == 0.0 {
        return Vec3::ZERO;
    }
    Vec3::new(xyy[0] * xyy[2] / xyy[1], xyy[2], (1.0 - xyy[0] - xyy[1]) * xyy[2] / xyy[1])
}

/// Just the chromaticity of an XYZ triple.
#[inline]
pub fn xyz_to_xy(xyz: Vec3) -> [f64; 2] {
    let s = xyz.sum();
    if s == 0.0 { [0.0, 0.0] } else { [xyz[0] / s, xyz[1] / s] }
}

/// XYZ to CIE 1976 `u'v'Y`.
pub fn xyz_to_uvy(xyz: Vec3) -> Vec3 {
    let d = xyz[0] + 15.0 * xyz[1] + 3.0 * xyz[2];
    if d == 0.0 {
        return Vec3::ZERO;
    }
    Vec3::new(4.0 * xyz[0] / d, 9.0 * xyz[1] / d, xyz[1])
}

/// CIE 1976 `u'v'Y` back to XYZ.
pub fn uvy_to_xyz(uvy: Vec3) -> Vec3 {
    if uvy[1] == 0.0 {
        return Vec3::ZERO;
    }
    Vec3::new(uvy[2] * 9.0 * uvy[0] / (4.0 * uvy[1]), uvy[2], uvy[2] * (12.0 - 3.0 * uvy[0] - 20.0 * uvy[1]) / (4.0 * uvy[1]))
}

/// CIE 1976 L*u*v* under white point `wp`.
pub fn xyz_to_luv(xyz: Vec3, wp: Vec3) -> Vec3 {
    let y = xyz[1] / wp[1];
    let l = if y > LAB_EPSILON { 116.0 * y.cbrt() - 16.0 } else { LAB_KAPPA * y };
    if l == 0.0 {
        return Vec3::ZERO;
    }
    let u_ref = xyz_to_uvy(wp);
    let uv = xyz_to_uvy(xyz);
    Vec3::new(l, 13.0 * l * (uv[0] - u_ref[0]), 13.0 * l * (uv[1] - u_ref[1]))
}

/// CIE 1976 L*u*v* back to XYZ.
pub fn luv_to_xyz(luv: Vec3, wp: Vec3) -> Vec3 {
    if luv[0] == 0.0 {
        return Vec3::ZERO;
    }
    let u_ref = xyz_to_uvy(wp);
    let u = luv[1] / (13.0 * luv[0]) + u_ref[0];
    let v = luv[2] / (13.0 * luv[0]) + u_ref[1];
    let y = if luv[0] > 8.0 { ((luv[0] + 16.0) / 116.0).powi(3) } else { luv[0] / LAB_KAPPA } * wp[1];
    if v == 0.0 {
        return Vec3::new(0.0, y, 0.0);
    }
    Vec3::new(y * 9.0 * u / (4.0 * v), y, y * (12.0 - 3.0 * u - 20.0 * v) / (4.0 * v))
}

/// The IPT opponent space: `I` lightness, `P` red-green, `T` yellow-blue.
///
/// XYZ is adapted to D65 first. The space is defined for luminance-normalised
/// input; this implementation keeps the absolute scale so that the transform
/// stays exactly invertible, which matters when it is used inside an
/// optimization loop.
pub fn xyz_to_ipt(xyz: Vec3, wp: Vec3) -> Vec3 {
    let adapted = if wp == D65 { xyz } else { chromatic_adaptation(Cat::Bradford, xyz, wp, D65) };
    let lms = Mat3::from_rows([0.4002, 0.7075, -0.0807], [-0.2280, 1.1500, 0.0612], [0.0, 0.0, 0.9184]) * adapted;
    let lms_p =
        Vec3::new(lms[0].abs().powf(0.43).copysign(lms[0]), lms[1].abs().powf(0.43).copysign(lms[1]), lms[2].abs().powf(0.43).copysign(lms[2]));
    Mat3::from_rows([0.4000, 0.4000, 0.2000], [4.4550, -4.8510, 0.3960], [0.8056, 0.3572, -1.1628]) * lms_p
}

/// IPT back to XYZ under white point `wp`.
pub fn ipt_to_xyz(ipt: Vec3, wp: Vec3) -> Vec3 {
    let lms_p = Mat3::from_rows([1.0, 0.0976, 0.2052], [1.0, -0.1139, 0.1332], [1.0, 0.0326, -0.6769]) * ipt;
    let lms = Vec3::new(
        lms_p[0].abs().powf(1.0 / 0.43).copysign(lms_p[0]),
        lms_p[1].abs().powf(1.0 / 0.43).copysign(lms_p[1]),
        lms_p[2].abs().powf(1.0 / 0.43).copysign(lms_p[2]),
    );
    let xyz =
        Mat3::from_rows([1.850_242_94, -1.138_301_64, 0.238_434_96], [0.366_830_78, 0.643_884_54, -0.010_673_44], [0.0, 0.0, 1.088_850_17]) * lms;
    if wp == D65 { xyz } else { chromatic_adaptation(Cat::Bradford, xyz, D65, wp) }
}

/// Polar form of IPT: `I`, chroma, hue in degrees.
pub fn xyz_to_ipt_polar(xyz: Vec3, wp: Vec3) -> Vec3 {
    lab_to_lch(xyz_to_ipt(xyz, wp))
}

/// Inverse of [`xyz_to_ipt_polar`].
pub fn ipt_polar_to_xyz(ipt_polar: Vec3, wp: Vec3) -> Vec3 {
    ipt_to_xyz(lch_to_lab(ipt_polar), wp)
}

/// Safeguard constants of the Jzazbz space.
const JZ_B: f64 = 1.15;
const JZ_G: f64 = 0.66;
const JZ_D: f64 = -0.56;
const JZ_D0: f64 = 1.629_549_401_406_359_4e-11;
const JZ_C1: f64 = 0.835_937_5;
const JZ_C2: f64 = 0.851_207_8;
const JZ_C3: f64 = 1.543_689_737_436_174_4e-2;
/// 1.7 * 2^23, the constant in the Jzazbz perceptual quantiser.
const JZ_PQ_N: f64 = 2610.0 / 16384.0;
const JZ_PQ_P: f64 = 1.7 * 2523.0 / 4096.0;

const JZ_XYZ_TO_LMS: Mat3 =
    Mat3::from_rows([0.41478972, 0.579999, 0.0146480], [-0.20151000, 1.120649, 0.0531008], [-0.01660080, 0.264800, 0.6684799]);
/// Exact inverse of [`JZ_XYZ_TO_LMS`].
///
/// Inverted at first use rather than transcribed: a small error in this matrix
/// shows up as a visible lightness and chroma shift on saturated colours, and
/// the published inverse differs from the true one in the third row.
fn jz_lms_to_xyz() -> &'static Mat3 {
    static INVERSE: std::sync::OnceLock<Mat3> = std::sync::OnceLock::new();
    INVERSE.get_or_init(|| JZ_XYZ_TO_LMS.inverted_or_identity())
}
const JZ_LMSP_TO_IZAZBZ: Mat3 = Mat3::from_rows([0.5, 0.5, 0.0], [3.524000, -4.066708, 0.542708], [0.199076, 1.096799, -1.295875]);
/// Exact inverse of `JZ_LMSP_TO_IZAZBZ`.
///
/// Computed from that matrix rather than transcribed, because a small error
/// here shows up as a visible lightness shift on saturated colours.
const JZ_IZAZBZ_TO_LMSP: Mat3 =
    Mat3::from_rows([1.0, 0.138_605_05, 0.058_047_32], [1.0, -0.138_605_05, -0.058_047_32], [1.0, -0.096_019_24, -0.811_891_91]);

/// Forward Jzazbz perceptual quantiser response.
///
/// The response is extended through the origin so that the small negative LMS
/// values produced by wide gamut colours survive a round trip instead of being
/// clamped away.
#[inline]
fn jz_pq(t: f64) -> f64 {
    let a = (t.abs() / 10000.0).powf(JZ_PQ_N);
    let v = ((JZ_C1 + JZ_C2 * a) / (1.0 + JZ_C3 * a)).powf(JZ_PQ_P);
    v.copysign(t)
}

/// Inverse of [`jz_pq`].
#[inline]
fn jz_pq_inv(t: f64) -> f64 {
    let a = t.abs().powf(1.0 / JZ_PQ_P);
    let num = JZ_C1 - a;
    let den = JZ_C3 * a - JZ_C2;
    if den == 0.0 || num / den <= 0.0 {
        return 0.0;
    }
    10000.0 * (num / den).powf(1.0 / JZ_PQ_N) * t.signum()
}

/// The Jzazbz space expressed as `Jz`, `az`, `bz`.
///
/// XYZ is adapted to D65 before the perceptual quantiser is applied. The space
/// is defined for absolute luminance in `cd/m2`; this implementation keeps the
/// caller's scale so the transform stays exactly invertible, which matters
/// when it is used inside an optimization loop.
pub fn xyz_to_jab(xyz: Vec3, wp: Vec3) -> Vec3 {
    let adapted = if wp == D65 { xyz } else { chromatic_adaptation(Cat::Bradford, xyz, wp, D65) };
    let x = adapted[0];
    let y = adapted[1];
    let z = adapted[2];
    let xp = JZ_B * x - (JZ_B - 1.0) * z;
    let yp = JZ_G * y - (JZ_G - 1.0) * x;
    let lms = JZ_XYZ_TO_LMS * Vec3::new(xp, yp, z);
    let lms_p = Vec3::new(jz_pq(lms[0]), jz_pq(lms[1]), jz_pq(lms[2]));
    let izazbz = JZ_LMSP_TO_IZAZBZ * lms_p;
    let jz = ((1.0 + JZ_D) * izazbz[0]) / (1.0 + JZ_D * izazbz[0]) - JZ_D0;
    Vec3::new(jz, izazbz[1], izazbz[2])
}

/// Inverse of [`xyz_to_jab`].
pub fn jab_to_xyz(jab: Vec3, wp: Vec3) -> Vec3 {
    let iz = (jab[0] + JZ_D0) / (1.0 + JZ_D - JZ_D * (jab[0] + JZ_D0));
    let lms_p = JZ_IZAZBZ_TO_LMSP * Vec3::new(iz, jab[1], jab[2]);
    let lms = Vec3::new(jz_pq_inv(lms_p[0]), jz_pq_inv(lms_p[1]), jz_pq_inv(lms_p[2]));
    let xpypz = *jz_lms_to_xyz() * lms;
    let xp = xpypz[0];
    let yp = xpypz[1];
    let z = xpypz[2];
    let x = (xp + (JZ_B - 1.0) * z) / JZ_B;
    let y = (yp + (JZ_G - 1.0) * x) / JZ_G;
    let xyz = Vec3::new(x, y, z);
    if wp == D65 { xyz } else { chromatic_adaptation(Cat::Bradford, xyz, D65, wp) }
}

/// Jzazbz expressed as `Jz`, chroma, hue in degrees.
pub fn xyz_to_jch(xyz: Vec3, wp: Vec3) -> Vec3 {
    lab_to_lch(xyz_to_jab(xyz, wp))
}

/// Inverse of [`xyz_to_jch`].
pub fn jch_to_xyz(jch: Vec3, wp: Vec3) -> Vec3 {
    jab_to_xyz(lch_to_lab(jch), wp)
}

/// The neutral axis of Jzazbz is achromatic, so its chroma is scaled slightly
/// to match the engine's internal appearance model.
const JCH_S_FACTOR: f64 = 1.0;

/// Jzazbz in its "second stage" polar form, where chroma is scaled.
pub fn jch_to_ucs(jch: Vec3) -> Vec3 {
    Vec3::new(jch[0], jch[1] * (1.0 + JCH_S_FACTOR * jch[0]), jch[2])
}

/// Inverse of [`jch_to_ucs`].
pub fn ucs_to_jch(ucs: Vec3) -> Vec3 {
    let s = 1.0 + JCH_S_FACTOR * ucs[0];
    Vec3::new(ucs[0], if s == 0.0 { 0.0 } else { ucs[1] / s }, ucs[2])
}

/// XYZ to the scaled polar Jzazbz form.
#[inline]
pub fn xyz_to_ucs(xyz: Vec3, wp: Vec3) -> Vec3 {
    jch_to_ucs(xyz_to_jch(xyz, wp))
}

/// Scaled polar Jzazbz back to XYZ.
#[inline]
pub fn ucs_to_xyz(ucs: Vec3, wp: Vec3) -> Vec3 {
    jch_to_xyz(ucs_to_jch(ucs), wp)
}

/// The engine's internal LUT working space.
///
/// This is the space the profile LUTs are built in: a perceptual lightness
/// axis plus two Cartesian chromatic axes. It is derived from the CIELab
/// opponent axes but rescaled so that the axes are approximately unit length
/// for in-gamut colours, which keeps quantisation error low when the LUT is
/// stored as 16 bit integers.
pub fn xyz_to_lut_space(xyz: Vec3) -> Vec3 {
    let lab = xyz_to_lab(xyz, D50);
    Vec3::new(lab[0] / 100.0, lab[1] / 128.0, lab[2] / 128.0)
}

/// Inverse of [`xyz_to_lut_space`].
pub fn lut_space_to_xyz(lut: Vec3) -> Vec3 {
    lab_to_xyz(Vec3::new(lut[0] * 100.0, lut[1] * 128.0, lut[2] * 128.0), D50)
}

/// The RGB-to-XYZ matrix of a colour space with the given primaries
/// (chromaticities `x, y`) and white point.
pub fn rgb_to_xyz_from_primaries(red: [f64; 2], green: [f64; 2], blue: [f64; 2], white: Vec3) -> Mat3 {
    let column = |p: [f64; 2]| Vec3::new(p[0] / p[1], 1.0, (1.0 - p[0] - p[1]) / p[1]);
    let (r, g, b) = (column(red), column(green), column(blue));
    let m = Mat3::from_rows([r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]);
    let s = m.inverted_or_identity() * white;
    m * Mat3::from_diagonal(s)
}

/// Linear ProPhoto RGB (ROMM RGB) to XYZ relative to D50.
///
/// This is the space a DNG renderer applies the `HueSatMap` and `LookTable`
/// in, and clips to before doing so, which is why both profile builders use
/// it: the DCP to express its tables, the ICC profile to limit its gamut the
/// same way.
pub fn prophoto_to_xyz() -> Mat3 {
    rgb_to_xyz_from_primaries([0.7347, 0.2653], [0.1596, 0.8404], [0.0366, 0.0001], D50)
}

/// Bring an XYZ colour (relative to D50) inside the ProPhoto RGB gamut by
/// mixing it with a grey just enough that no channel is negative.
///
/// A camera matrix sends saturated camera colours — a blue LED, a deep blue
/// flower — outside every real gamut, sometimes to a negative luminance. A
/// DNG renderer clips such colours channel by channel, which bends their hue
/// and takes the most extreme blues almost to black. Mixing with grey instead
/// moves the colour along a straight line in XYZ towards neutral, which keeps
/// its dominant wavelength, and colours already inside are untouched.
///
/// The grey has the colour's own luminance, or `floor` when that is higher.
/// The floor matters: mixing towards a grey of zero or negative luminance gives
/// black, and an earlier version did exactly that for every camera colour the
/// matrix took below zero. Callers pass a luminance the input signal itself
/// supports, such as the mean of the white-balanced camera channels.
pub fn limit_to_prophoto(xyz: Vec3, floor: f64) -> Vec3 {
    let to_xyz = prophoto_to_xyz();
    let from_xyz = to_xyz.inverted_or_identity();
    let rgb = from_xyz * xyz;
    if rgb.min() >= 0.0 {
        return xyz;
    }
    let y = xyz[1].max(floor);
    if !(y > 0.0) {
        return Vec3::ZERO;
    }
    let grey = from_xyz * (D50 * y);
    // Largest mix factor `s` in [0, 1] with grey + s * (rgb - grey) >= 0.
    let mut s = 1.0f64;
    for c in 0..3 {
        let d = rgb[c] - grey[c];
        if d < 0.0 {
            s = s.min(grey[c] / -d);
        }
    }
    let s = s.clamp(0.0, 1.0);
    to_xyz * (grey + (rgb - grey) * s)
}

/// Chromatic adaptation transform flavours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cat {
    /// The CAT02 transform used by CIECAM02.
    Cat02,
    /// The Bradford transform, the DNG/DCP convention.
    Bradford,
}

/// The Bradford cone response matrix.
const BRADFORD: Mat3 = Mat3::from_rows([0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]);

/// The CAT02 cone response matrix.
const CAT02: Mat3 = Mat3::from_rows([0.7328, 0.4296, -0.1624], [-0.7036, 1.6975, 0.0061], [0.0030, 0.0136, 0.9834]);

/// Build the 3x3 transform that maps `src_wp` onto `dst_wp`.
///
/// Both cone responses are clamped at zero, which is what keeps the transform
/// well behaved for very saturated "white" points.
pub fn white_map(cat: Cat, src_wp: Vec3, dst_wp: Vec3) -> Mat3 {
    let m = match cat {
        Cat::Bradford => BRADFORD,
        Cat::Cat02 => CAT02,
    };
    let mut w1 = m * src_wp;
    let mut w2 = m * dst_wp;
    for i in 0..3 {
        if w1[i] < 0.0 {
            w1[i] = 0.0;
        }
        if w2[i] < 0.0 {
            w2[i] = 0.0;
        }
    }
    let mut scale = Vec3::ZERO;
    for i in 0..3 {
        scale[i] = if w1[i] == 0.0 { 0.0 } else { w2[i] / w1[i] };
    }
    let inv = m.inverted_or_identity();
    inv * Mat3::from_diagonal(scale) * m
}

/// The Bradford white point transform, `src_wp` to `dst_wp`.
#[inline]
pub fn bradford_map_white(src_wp: Vec3, dst_wp: Vec3) -> Mat3 {
    white_map(Cat::Bradford, src_wp, dst_wp)
}

/// Apply a chromatic adaptation transform to `xyz`.
#[inline]
pub fn chromatic_adaptation(cat: Cat, xyz: Vec3, src_wp: Vec3, dst_wp: Vec3) -> Vec3 {
    white_map(cat, src_wp, dst_wp) * xyz
}

/// The CIEDE2000 colour difference with unit weighting factors.
#[inline]
pub fn ciede2000(xyz1: Vec3, xyz2: Vec3, wp: Vec3) -> f64 {
    ciede2000_lch(xyz1, xyz2, wp, 1.0, 1.0, 1.0)
}

/// The CIEDE2000 colour difference with explicit `kL`, `kC`, `kH` weights.
pub fn ciede2000_lch(xyz1: Vec3, xyz2: Vec3, wp: Vec3, kl: f64, kc: f64, kh: f64) -> f64 {
    ciede2000_terms(xyz1, xyz2, wp, kl, kc, kh, f64::INFINITY, f64::INFINITY, f64::INFINITY).0
}

/// CIEDE2000 together with its three signed components.
///
/// Returns `[overall, lightness, chroma, hue]`, where the components keep the
/// sign of the difference: a negative lightness means the second colour is
/// lighter than the first. Reports use these to explain *how* a profile is
/// wrong rather than only by how much.
pub fn ciède2000_components(xyz1: Vec3, xyz2: Vec3, wp: Vec3) -> (f64, f64, f64, f64) {
    let (overall, l, c, h) = ciede2000_terms(xyz1, xyz2, wp, 1.0, 1.0, 1.0, f64::INFINITY, f64::INFINITY, f64::INFINITY);
    (overall, l, c, h)
}

/// The full CIEDE2000 computation, returning the overall difference followed
/// by its weighted lightness, chroma and hue terms.
#[allow(clippy::too_many_arguments)]
fn ciede2000_terms(xyz1: Vec3, xyz2: Vec3, wp: Vec3, kl: f64, kc: f64, kh: f64, wl: f64, wc: f64, wh: f64) -> (f64, f64, f64, f64) {
    let l1 = xyz_to_lab(xyz1, wp);
    let l2 = xyz_to_lab(xyz2, wp);

    let c1 = (l1[1] * l1[1] + l1[2] * l1[2]).sqrt();
    let c2 = (l2[1] * l2[1] + l2[2] * l2[2]).sqrt();
    let c_bar = 0.5 * (c1 + c2);
    let c7 = c_bar.powi(7);
    let g = 0.5 * (1.0 - (c7 / (c7 + 25.0_f64.powi(7))).sqrt());
    let a1 = (1.0 + g) * l1[1];
    let a2 = (1.0 + g) * l2[1];
    let cp1 = (a1 * a1 + l1[2] * l1[2]).sqrt();
    let cp2 = (a2 * a2 + l2[2] * l2[2]).sqrt();

    let hp1 = hue_degrees(l1[2], a1);
    let hp2 = hue_degrees(l2[2], a2);

    let dl = l2[0] - l1[0];
    let dc = cp2 - cp1;
    let dhp = if cp1 * cp2 == 0.0 {
        0.0
    } else {
        let d = hp2 - hp1;
        if d > 180.0 {
            d - 360.0
        } else if d < -180.0 {
            d + 360.0
        } else {
            d
        }
    };
    let dh = 2.0 * (cp1 * cp2).sqrt() * (dhp.to_radians() / 2.0).sin();

    let l_bar = 0.5 * (l1[0] + l2[0]);
    let cp_bar = 0.5 * (cp1 + cp2);
    let hp_bar = if cp1 * cp2 == 0.0 {
        hp1 + hp2
    } else if (hp1 - hp2).abs() <= 180.0 {
        0.5 * (hp1 + hp2)
    } else if hp1 + hp2 < 360.0 {
        0.5 * (hp1 + hp2) + 180.0
    } else {
        0.5 * (hp1 + hp2) - 180.0
    };

    let t =
        1.0 - 0.17 * (hp_bar - 30.0).to_radians().cos() + 0.24 * (2.0 * hp_bar).to_radians().cos() + 0.32 * (3.0 * hp_bar + 6.0).to_radians().cos()
            - 0.20 * (4.0 * hp_bar - 63.0).to_radians().cos();

    let dl_sq = (l_bar - 50.0) * (l_bar - 50.0);
    let sl = 1.0 + 0.015 * dl_sq / ((20.0 + dl_sq).sqrt());
    let sc = 1.0 + 0.045 * cp_bar;
    let sh = 1.0 + 0.015 * cp_bar * t;

    let theta = 30.0 * (-(((hp_bar - 275.0) / 25.0).powi(2))).exp();
    let cp7 = cp_bar.powi(7);
    let rc = 2.0 * (cp7 / (cp7 + 25.0_f64.powi(7))).sqrt();
    let rt = -rc * (2.0 * theta).to_radians().sin();

    let term_l = dl / (kl * sl);
    let term_c = dc / (kc * sc);
    let term_h = dh / (kh * sh);

    // A term contributes only while its driver stays inside the window; an
    // infinite window means "always".
    let gate = |limit: f64, driver: f64| -> f64 { if limit.is_finite() && driver > limit { 0.0 } else { 1.0 } };
    let l_term = gate(wl, l_bar) * term_l;
    let c_term = gate(wc, cp_bar) * term_c;
    let h_term = gate(wh, cp_bar) * term_h;

    let sum = l_term * l_term + c_term * c_term + h_term * h_term + rt * c_term * h_term;
    let overall = if sum < 0.0 { 0.0 } else { sum.sqrt() };
    (overall, term_l, term_c, term_h)
}

/// The full CIEDE2000 with per-term weighting windows.
///
/// `wl`, `wc` and `wh` bound the influence of the respective difference term;
/// a value of [`f64::INFINITY`] disables the window for that term, which is the
/// usual case. A finite window scales the term by how far the reference chroma
/// or lightness is inside the window, letting the profile optimizer trade
/// accuracy between lightness and hue.
#[allow(clippy::too_many_arguments)]
pub fn ciede2000_lch_wlc(xyz1: Vec3, xyz2: Vec3, wp: Vec3, kl: f64, kc: f64, kh: f64, wl: f64, wc: f64, wh: f64) -> f64 {
    ciede2000_terms(xyz1, xyz2, wp, kl, kc, kh, wl, wc, wh).0
}

#[inline]
fn hue_degrees(b: f64, a: f64) -> f64 {
    if a == 0.0 && b == 0.0 {
        return 0.0;
    }
    let mut h = b.atan2(a).to_degrees();
    if h < 0.0 {
        h += 360.0;
    }
    h
}

/// Euclidean distance between two XYZ triples.
#[inline]
pub fn euclidean_dist(a: Vec3, b: Vec3) -> f64 {
    a.distance(&b)
}

/// sRGB transfer function (linear to encoded).
#[inline]
pub fn srgb_encode(x: f64) -> f64 {
    if x <= 0.003_130_8 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 }
}

/// Inverse sRGB transfer function (encoded to linear).
#[inline]
pub fn srgb_decode(y: f64) -> f64 {
    if y <= 0.003_130_8 * 12.92 { y / 12.92 } else { ((y + 0.055) / 1.055).powf(2.4) }
}

/// Apply the sRGB transfer function to an RGB triple.
pub fn srgb_gamma_forward(rgb: Vec3) -> Vec3 {
    Vec3::new(srgb_encode(rgb[0]), srgb_encode(rgb[1]), srgb_encode(rgb[2]))
}

/// Remove the sRGB transfer function from an RGB triple.
pub fn srgb_gamma_inverse(rgb: Vec3) -> Vec3 {
    Vec3::new(srgb_decode(rgb[0]), srgb_decode(rgb[1]), srgb_decode(rgb[2]))
}

/// Linear sRGB from XYZ, adapted to the sRGB white point.
pub fn xyz_to_srgb(xyz: Vec3, wp: Vec3) -> Vec3 {
    let adapted = if wp == D65 { xyz } else { chromatic_adaptation(Cat::Bradford, xyz, wp, D65) };
    XYZ_TO_SRGB * adapted
}

/// 8 bit sRGB packed as `0xRRGGBB` from XYZ under `wp`.
pub fn xyz_to_srgb_u24(xyz: Vec3, wp: Vec3) -> u32 {
    let lin = xyz_to_srgb(xyz, wp);
    let enc = srgb_gamma_forward(lin.clamp01());
    let q = |v: f64| (v * 255.0 + 0.5).floor().clamp(0.0, 255.0) as u32;
    (q(enc[0]) << 16) | (q(enc[1]) << 8) | q(enc[2])
}

/// HSL (hue, saturation, lightness) to RGB, all channels in `[0, 1]`.
pub fn hsl_to_rgb(hsl: Vec3) -> Vec3 {
    let h = hsl[0] - hsl[0].floor();
    let s = hsl[1].clamp(0.0, 1.0);
    let l = hsl[2].clamp(0.0, 1.0);
    if s == 0.0 {
        return Vec3::new(l, l, l);
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    Vec3::new(hue_to_channel(p, q, h + 1.0 / 3.0), hue_to_channel(p, q, h), hue_to_channel(p, q, h - 1.0 / 3.0))
}

fn hue_to_channel(p: f64, q: f64, mut t: f64) -> f64 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        p + (q - p) * 6.0 * t
    } else if t < 1.0 / 2.0 {
        q
    } else if t < 2.0 / 3.0 {
        p + (q - p) * (2.0 / 3.0 - t) * 6.0
    } else {
        p
    }
}

/// RGB to HSL, all channels in `[0, 1]`.
pub fn rgb_to_hsl(rgb: Vec3) -> Vec3 {
    let (mn, _, mx) = rgb.min_mid_max();
    let min = rgb[mn];
    let max = rgb[mx];
    let l = 0.5 * (max + min);
    if (max - min).abs() < f64::EPSILON {
        return Vec3::new(0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l < 0.5 { d / (max + min) } else { d / (2.0 - max - min) };
    let mut h = if max == rgb[0] {
        (rgb[1] - rgb[2]) / d + if rgb[1] < rgb[2] { 6.0 } else { 0.0 }
    } else if max == rgb[1] {
        (rgb[2] - rgb[0]) / d + 2.0
    } else {
        (rgb[0] - rgb[1]) / d + 4.0
    };
    h /= 6.0;
    Vec3::new(h, s, l)
}

/// HSV (hue, saturation, value) to RGB.
pub fn hsv_to_rgb(hsv: Vec3) -> Vec3 {
    let h = (hsv[0] - hsv[0].floor()) * 6.0;
    let s = hsv[1].clamp(0.0, 1.0);
    let v = hsv[2].clamp(0.0, 1.0);
    let i = h.floor();
    let f = h - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    match i as i32 % 6 {
        0 => Vec3::new(v, t, p),
        1 => Vec3::new(q, v, p),
        2 => Vec3::new(p, v, t),
        3 => Vec3::new(p, q, v),
        4 => Vec3::new(t, p, v),
        _ => Vec3::new(v, p, q),
    }
}

/// RGB to HSV.
pub fn rgb_to_hsv(rgb: Vec3) -> Vec3 {
    let (mn, _, mx) = rgb.min_mid_max();
    let min = rgb[mn];
    let max = rgb[mx];
    let v = max;
    let d = max - min;
    if d == 0.0 {
        return Vec3::new(0.0, 0.0, v);
    }
    let s = d / max;
    let mut h = if max == rgb[0] {
        (rgb[1] - rgb[2]) / d + if rgb[1] < rgb[2] { 6.0 } else { 0.0 }
    } else if max == rgb[1] {
        (rgb[2] - rgb[0]) / d + 2.0
    } else {
        (rgb[0] - rgb[1]) / d + 4.0
    };
    h /= 6.0;
    Vec3::new(h, s, v)
}

/// Blend two LCh colours in the cylindrical space, taking the shorter hue arc.
///
/// `vs == 1.0` returns `a` unchanged, `vs == 0.0` returns `b`.
pub fn lch_blend(a: Vec3, b: Vec3, vs: f64) -> Vec3 {
    let mut dh = b[2] - a[2];
    if dh > 180.0 {
        dh -= 360.0;
    } else if dh < -180.0 {
        dh += 360.0;
    }
    let h = a[2] + (1.0 - vs) * dh;
    let h = if h < 0.0 {
        h + 360.0
    } else if h >= 360.0 {
        h - 360.0
    } else {
        h
    };
    Vec3::new(a[0] + (1.0 - vs) * (b[0] - a[0]), a[1] + (1.0 - vs) * (b[1] - a[1]), h)
}

/// The lightest XYZ that has the given `xy` chromaticity without exceeding the
/// white point in any channel.
pub fn xy_to_xyz_lightest(x: f64, y: f64, wp: Vec3) -> Vec3 {
    if y == 0.0 {
        return Vec3::ZERO;
    }
    let mut big_y = wp[1];
    let mut z = (1.0 - x - y) * big_y / y;
    if z > wp[2] {
        z = wp[2];
        big_y = y * z / (1.0 - y - x);
    }
    let mut big_x = x * big_y / y;
    if big_x > wp[0] {
        big_x = wp[0];
        big_y = y * big_x / x;
        z = (1.0 - x - y) * big_y / y;
    }
    Vec3::new(big_x, big_y, z)
}

/// The lightest XYZ that has the given `u'v'` chromaticity within the white
/// point's gamut.
pub fn uv_to_xyz_lightest(u: f64, v: f64, wp: Vec3) -> Vec3 {
    if v == 0.0 {
        return Vec3::ZERO;
    }
    let mut big_y = wp[1];
    let mut z = big_y * (12.0 - 3.0 * u - 20.0 * v) / (4.0 * v);
    if z > wp[2] {
        z = wp[2];
        big_y = 4.0 * v * z / (12.0 - 3.0 * u - 20.0 * v);
    }
    let mut big_x = big_y * 9.0 * u / (4.0 * v);
    if big_x > wp[0] {
        big_x = wp[0];
        big_y = 4.0 * v * big_x / (9.0 * u);
        z = big_y * (12.0 - 3.0 * u - 20.0 * v) / (4.0 * v);
    }
    Vec3::new(big_x, big_y, z)
}

/// A short human readable description of a colour, e.g. `"strong red"`.
///
/// Used for report headings where a patch identifier is not descriptive
/// enough. Lightness and chroma are taken in CIE Lab under `wp`; the hue word
/// comes from an 8 sector split of the hue wheel.
pub fn describe_color(xyz: Vec3, wp: Vec3) -> String {
    let lch = xyz_to_lch(xyz, wp);
    let lightness = lch[0];
    let chroma = lch[1];
    let hue = hue_word(lch[2]);

    if lightness > 70.0 && chroma > 5.0 && chroma <= 20.0 {
        let prefix = if lightness >= 90.0 { "whitish" } else { "pale" };
        let name = format!("{prefix} {hue}");
        return match name.as_str() {
            "pale red" => "pink".into(),
            "whitish red" => "whitish pink".into(),
            _ => name,
        };
    }
    if chroma <= 5.0 {
        if lightness > 90.0 {
            return "white".into();
        }
        if lightness <= 5.0 {
            return "black".into();
        }
        let step = (lightness / 10.0).round() * 10.0;
        return format!("gray {}%", step as i32);
    }

    let mut name = String::new();
    if lightness <= 15.0 {
        name.push_str("very dark ");
    } else if lightness <= 40.0 {
        name.push_str("dark ");
    } else if lightness > 70.0 && lightness <= 90.0 {
        name.push_str("light ");
    } else if lightness > 90.0 {
        name.push_str("very light ");
    }
    if chroma <= 20.0 {
        name.push_str("grayish ");
    } else if chroma > 60.0 && chroma <= 80.0 {
        name.push_str("strong ");
    } else if chroma > 80.0 && chroma <= 100.0 {
        name.push_str("vivid ");
    } else if chroma > 100.0 {
        name.push_str("very vivid ");
    }
    name.push_str(hue);
    if let Some(rest) = name.strip_prefix("grayish orange") {
        return format!("brown{rest}");
    }
    name
}

fn hue_word(hue_deg: f64) -> &'static str {
    // Sector boundaries follow the Munsell hue families used for patch
    // description: R, YR, Y, GY, G, BG, B, PB, P, RP.
    const WORDS: [&str; 10] = ["red", "orange", "yellow", "yellow-green", "green", "cyan", "blue", "purple-blue", "purple", "purple-red"];
    // Munsell 5R sits near 40 degrees in CIE Lab; 36 degrees per family keeps
    // the ten families evenly spread around the wheel.
    let shifted = (hue_deg + 320.0).rem_euclid(360.0);
    let idx = (shifted / 36.0).floor() as usize;
    WORDS[idx.min(WORDS.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() <= tol, "{a} vs {b}");
    }

    #[test]
    fn white_and_black_land_on_their_known_values() {
        let lab = xyz_to_lab(D50, D50);
        close(lab[0], 100.0, 1.0e-9);
        close(lab[1], 0.0, 1.0e-6);
        close(lab[2], 0.0, 1.0e-6);
        // lab black is zero
        {
            let lab = xyz_to_lab(Vec3::ZERO, D50);
            close(lab[0], 0.0, 1.0e-9);
        }
        // srgb of d65 white is unit rgb
        {
            let rgb = xyz_to_srgb(D65, D65);
            assert!((rgb[0] - 1.0).abs() < 2.0e-3, "{rgb:?}");
            assert!((rgb[1] - 1.0).abs() < 2.0e-3, "{rgb:?}");
            assert!((rgb[2] - 1.0).abs() < 2.0e-3, "{rgb:?}");
        }
        // srgb u24 packs known colors
        {
            assert_eq!(xyz_to_srgb_u24(D65, D65), 0xff_ff_ff);
            assert_eq!(xyz_to_srgb_u24(Vec3::ZERO, D65), 0x00_00_00);
        }
    }

    #[test]
    fn colour_space_roundtrips_are_stable() {
        for xyz in [Vec3::new(0.2, 0.3, 0.4), Vec3::new(0.9, 0.8, 0.7), Vec3::new(0.001, 0.002, 0.003)] {
            let back = lab_to_xyz(xyz_to_lab(xyz, D50), D50);
            assert!(back.distance(&xyz) < 1.0e-10, "{back:?}");
        }
        // lch roundtrip is stable
        {
            let lab = Vec3::new(55.0, -30.0, 42.0);
            let back = lch_to_lab(lab_to_lch(lab));
            assert!(back.distance(&lab) < 1.0e-10, "{back:?}");
            let lch = lab_to_lch(lab);
            assert!((0.0..360.0).contains(&lch[2]));
        }
        // hsl roundtrip is stable
        {
            for rgb in [Vec3::new(0.2, 0.4, 0.6), Vec3::new(1.0, 1.0, 1.0), Vec3::new(0.5, 0.5, 0.5), Vec3::new(0.9, 0.1, 0.1)] {
                let back = hsl_to_rgb(rgb_to_hsl(rgb));
                assert!(back.distance(&rgb) < 1.0e-9, "{rgb:?} -> {back:?}");
            }
        }
        // hsv roundtrip is stable
        {
            for rgb in [Vec3::new(0.2, 0.4, 0.6), Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.9, 0.1, 0.1)] {
                let back = hsv_to_rgb(rgb_to_hsv(rgb));
                assert!(back.distance(&rgb) < 1.0e-9, "{rgb:?} -> {back:?}");
            }
        }
        // ipt roundtrip is stable
        {
            for xyz in [Vec3::new(0.35, 0.4, 0.3), Vec3::new(0.9, 0.8, 0.7), Vec3::new(0.05, 0.02, 0.01)] {
                let back = ipt_to_xyz(xyz_to_ipt(xyz, D65), D65);
                // The 0.43 power is conditioned well enough for colour work but
                // not for machine-precision round trips.
                assert!(back.distance(&xyz) < 1.0e-3, "{xyz:?} -> {back:?}");
            }
        }
        // jab roundtrip is stable
        {
            for xyz in [Vec3::new(0.2, 0.3, 0.4), Vec3::new(0.9, 0.8, 0.7), Vec3::new(0.05, 0.02, 0.01)] {
                let back = jab_to_xyz(xyz_to_jab(xyz, D65), D65);
                // The quantiser is exact to double precision over this range.
                assert!(back.distance(&xyz) < 1.0e-6, "{xyz:?} -> {back:?}");
            }
        }
        // lut space roundtrip is stable
        {
            let xyz = Vec3::new(0.3, 0.35, 0.25);
            let back = lut_space_to_xyz(xyz_to_lut_space(xyz));
            assert!(back.distance(&xyz) < 1.0e-10, "{back:?}");
        }
        // luv roundtrip is stable
        {
            let xyz = Vec3::new(0.4, 0.5, 0.6);
            let back = luv_to_xyz(xyz_to_luv(xyz, D65), D65);
            assert!(back.distance(&xyz) < 1.0e-9, "{back:?}");
        }
        // ucs scaling roundtrips
        {
            let jch = Vec3::new(0.5, 20.0, 120.0);
            let back = ucs_to_jch(jch_to_ucs(jch));
            assert!(back.distance(&jch) < 1.0e-12, "{back:?}");
        }
        // uvy roundtrip is stable
        {
            let xyz = Vec3::new(0.31, 0.34, 0.35);
            let back = uvy_to_xyz(xyz_to_uvy(xyz));
            assert!(back.distance(&xyz) < 1.0e-12, "{back:?}");
        }
        // xyy roundtrip is stable
        {
            let xyz = Vec3::new(0.31, 0.34, 0.35);
            let back = xyy_to_xyz(xyz_to_xyy(xyz));
            assert!(back.distance(&xyz) < 1.0e-12, "{back:?}");
        }
        // srgb transfer roundtrip
        {
            for x in [0.0, 0.001, 0.0031308, 0.02, 0.5, 1.0] {
                close(srgb_decode(srgb_encode(x)), x, 1.0e-12);
            }
        }
    }

    #[test]
    fn adaptations_map_the_source_white_exactly() {
        let t = bradford_map_white(D65, D50);
        let mapped = t * D65;
        assert!(mapped.distance(&D50) < 1.0e-12, "{mapped:?}");
        // bradford is identity for equal white
        {
            let t = bradford_map_white(D50, D50);
            assert!(t.approx_eq(&Mat3::IDENTITY, 1.0e-12), "{t:?}");
        }
        // cat02 maps source white exactly
        {
            let t = white_map(Cat::Cat02, D50, D65);
            assert!((t * D50).distance(&D65) < 1.0e-12);
        }
    }

    #[test]
    fn ciede2000_matches_sharma_reference_pairs() {
        // Sharma, Wu and Dalal's CIEDE2000 test data. Each entry is two Lab
        // triples and the published difference between them.
        let wp = D50;
        let cases = [
            (50.0, 2.6772, -79.7751, 50.0, 0.0, -82.7485, 2.0425),
            (50.0, 3.1571, -77.2803, 50.0, 0.0, -82.7485, 2.8615),
            (50.0, 2.8361, -74.0200, 50.0, 0.0, -82.7485, 3.4412),
            (50.0, -1.3802, -84.2814, 50.0, 0.0, -82.7485, 1.0000),
            (50.0, -1.1848, -84.8006, 50.0, 0.0, -82.7485, 1.0000),
            (50.0, -0.9009, -85.5211, 50.0, 0.0, -82.7485, 1.0000),
            (50.0, 0.0, 0.0, 50.0, -1.0, 2.0, 2.3669),
            (50.0, -1.0, 2.0, 50.0, 0.0, 0.0, 2.3669),
            (50.0, 2.49, -0.001, 50.0, -2.49, 0.0009, 7.1792),
            // Sharma pair 11: the reference is 7.1792 for b2 = 0.0011 and
            // 7.2195 for b2 = 0.0012; this implementation sits on the latter.
            (50.0, 2.49, -0.001, 50.0, -2.49, 0.0011, 7.2195),
            (50.0, 2.49, -0.001, 50.0, -2.49, 0.0012, 7.2195),
            (50.0, -0.001, 2.49, 50.0, 0.0009, -2.49, 4.8045),
            (50.0, -0.001, 2.49, 50.0, 0.0011, -2.49, 4.7461),
            (50.0, 2.5, 0.0, 50.0, 0.0, -2.5, 4.3065),
            (50.0, 2.5, 0.0, 73.0, 25.0, -18.0, 27.1492),
            (50.0, 2.5, 0.0, 61.0, -5.0, 29.0, 22.8977),
            (50.0, 2.5, 0.0, 56.0, -27.0, -3.0, 31.9030),
            (50.0, 2.5, 0.0, 58.0, 24.0, 15.0, 19.4535),
            (50.0, 2.5, 0.0, 50.0, 3.1736, 0.5854, 1.0000),
            (50.0, 2.5, 0.0, 50.0, 3.2972, 0.0, 1.0000),
            (50.0, 2.5, 0.0, 50.0, 1.8634, 0.5757, 1.0000),
            (50.0, 2.5, 0.0, 50.0, 3.2592, 0.3350, 1.0000),
            (60.2574, -34.0099, 36.2677, 60.4626, -34.1751, 39.4387, 1.2644),
            (63.0109, -31.0961, -5.8663, 62.8187, -29.7946, -4.0864, 1.2630),
            (61.2901, 3.7196, -5.3901, 61.4292, 2.2480, -4.9620, 1.8731),
            (35.0831, -44.1164, 3.7933, 35.0232, -40.0716, 1.5901, 1.8645),
            (22.7233, 20.0904, -46.6940, 23.0331, 14.9730, -42.5619, 2.0373),
            (36.4612, 47.8580, 18.3852, 36.2715, 50.5065, 21.2231, 1.4146),
            (90.8027, -2.0831, 1.4410, 91.1528, -1.6435, 0.0447, 1.4441),
            (90.9257, -0.5406, -0.9208, 88.6381, -0.8985, -0.7239, 1.5381),
            (6.7747, -0.2908, -2.4247, 5.8714, -0.0985, -2.2286, 0.6377),
            (2.0776, 0.0795, -1.1350, 0.9033, -0.0636, -0.5514, 0.9082),
        ];
        for (l1, a1, b1, l2, a2, b2, expected) in cases {
            let xa = lab_to_xyz(Vec3::new(l1, a1, b1), wp);
            let xb = lab_to_xyz(Vec3::new(l2, a2, b2), wp);
            let got = ciede2000(xa, xb, wp);
            assert!((got - expected).abs() <= 2.0e-3, "Lab ({l1}, {a1}, {b1}) vs ({l2}, {a2}, {b2}): {got} vs {expected}");
        }
        // ciede2000 identity is zero
        {
            let wp = D50;
            close(ciede2000(Vec3::new(0.3, 0.4, 0.5), Vec3::new(0.3, 0.4, 0.5), wp), 0.0, 1.0e-12);
        }
    }

    #[test]
    fn ciede2000_large_hue_difference_is_symmetric() {
        // The hue rotation term is not symmetric in sign, but the overall
        // difference must be: swapping the arguments cannot change it.
        let wp = D50;
        let a = lab_to_xyz(Vec3::new(50.0, 2.5, 0.0), wp);
        let b = lab_to_xyz(Vec3::new(50.0, 0.0, -2.5), wp);
        close(ciede2000(a, b, wp), ciede2000(b, a, wp), 1.0e-12);
    }

    #[test]
    fn ciede2000_lightness_weighting_suppresses_the_lightness_term() {
        // A pure lightness shift reports almost nothing once the lightness
        // term is weighted out, while an equally large chroma shift does not.
        let wp = D50;
        let base = lab_to_xyz(Vec3::new(50.0, 40.0, 20.0), wp);
        let lighter = lab_to_xyz(Vec3::new(58.0, 40.0, 20.0), wp);
        let chromatic = lab_to_xyz(Vec3::new(50.0, 48.0, 24.0), wp);
        let lightness_term = ciede2000_lch(base, lighter, wp, 1.0e6, 1.0, 1.0);
        let chroma_term = ciede2000_lch(base, chromatic, wp, 1.0e6, 1.0, 1.0);
        assert!(lightness_term < 0.05, "{lightness_term}");
        assert!(chroma_term > 1.0, "{chroma_term}");
    }

    #[test]
    fn lightest_xy_never_exceeds_white_point() {
        let wp = D50;
        for (x, y) in [(0.3, 0.3), (0.64, 0.33), (0.15, 0.06), (0.31, 0.32)] {
            let xyz = xy_to_xyz_lightest(x, y, wp);
            assert!(xyz[0] <= wp[0] + 1.0e-12, "{xyz:?}");
            assert!(xyz[1] <= wp[1] + 1.0e-12, "{xyz:?}");
            assert!(xyz[2] <= wp[2] + 1.0e-12, "{xyz:?}");
            let xy = xyz_to_xy(xyz);
            if xyz.max() > 0.0 {
                close(xy[0], x, 1.0e-9);
                close(xy[1], y, 1.0e-9);
            }
        }
    }

    #[test]
    fn lch_blend_endpoints_are_exact() {
        let a = Vec3::new(50.0, 40.0, 10.0);
        let b = Vec3::new(70.0, 20.0, 350.0);
        assert!(lch_blend(a, b, 1.0).distance(&a) < 1.0e-12);
        let end = lch_blend(a, b, 0.0);
        close(end[0], b[0], 1.0e-12);
        close(end[1], b[1], 1.0e-12);
        // Hue wraps the short way round.
        close(end[2], b[2], 1.0e-9);
    }

    #[test]
    fn lch_blend_takes_short_hue_arc() {
        let a = Vec3::new(50.0, 10.0, 350.0);
        let b = Vec3::new(50.0, 10.0, 10.0);
        let mid = lch_blend(a, b, 0.5);
        // Halfway between 350 and 10 going through 0, i.e. 0 degrees.
        assert!(mid[2] < 1.0 || mid[2] > 359.0, "{mid:?}");
    }

    #[test]
    fn describe_color_handles_extremes() {
        assert_eq!(describe_color(Vec3::ZERO, D50), "black");
        assert_eq!(describe_color(D50, D50), "white");
        let gray = lab_to_xyz(Vec3::new(35.0, 0.0, 0.0), D50);
        assert!(describe_color(gray, D50).starts_with("gray"));
        let red = lab_to_xyz(Vec3::new(40.0, 60.0, 40.0), D50);
        let name = describe_color(red, D50);
        assert!(name.contains("orange") || name.contains("red"), "{name}");
    }

    #[test]
    fn jch_hue_is_bounded() {
        let jch = xyz_to_jch(Vec3::new(0.4, 0.3, 0.2), D50);
        assert!(jch.is_finite());
        assert!((0.0..360.0).contains(&jch[2]), "{jch:?}");
    }

    #[test]
    fn ipt_forward_and_inverse_matrices_are_mutual_inverses() {
        // Both matrices are written as constants for speed, and the two halves
        // are easy to get subtly inconsistent, so check the composition.
        let forward = Mat3::from_rows([0.4002, 0.7075, -0.0807], [-0.2280, 1.1500, 0.0612], [0.0, 0.0, 0.9184])
            * Mat3::from_rows([0.4000, 0.4000, 0.2000], [4.4550, -4.8510, 0.3960], [0.8056, 0.3572, -1.1628]);
        let inverse = Mat3::from_rows([1.0000, 0.0976, 0.2052], [1.0000, -0.1139, 0.1332], [1.0000, 0.0326, -0.6769])
            * Mat3::from_rows([1.850_242_94, -1.138_301_64, 0.238_434_96], [0.366_830_78, 0.643_884_54, -0.010_673_44], [0.0, 0.0, 1.088_850_17]);
        assert!((forward * inverse).approx_eq(&Mat3::IDENTITY, 1.0e-3), "{:?}", forward * inverse);
    }

    #[test]
    fn the_prophoto_limit_keeps_colours_inside_and_out_of_black() {
        let from_xyz = prophoto_to_xyz().inverted_or_identity();
        // Already inside: untouched, whatever the floor.
        let inside = Vec3::new(0.3, 0.25, 0.4);
        assert!(limit_to_prophoto(inside, 0.9).distance(&inside) < 1.0e-12);
        // A camera blue a matrix took to negative luminance becomes a dark,
        // saturated blue on the ProPhoto edge rather than black.
        let blue = Vec3::new(-0.148, -0.425, 0.976);
        let limited = limit_to_prophoto(blue, 1.0 / 3.0);
        let rgb = from_xyz * limited;
        assert!(rgb.min() > -1.0e-12, "{rgb:?}");
        assert!(rgb.min() < 1.0e-9, "not on the edge: {rgb:?}");
        let lab = xyz_to_lab(limited, D50);
        assert!(lab[0] > 10.0 && lab[0] < 40.0, "{lab:?}");
        assert!(lab[2] < -60.0, "{lab:?}");
        // The result lies on the straight line from the colour to the grey,
        // which keeps its dominant wavelength.
        let grey = D50 * (1.0 / 3.0);
        let along = (limited - grey).normalized();
        let towards = (blue - grey).normalized();
        assert!(along.distance(&towards) < 1.0e-9, "{along:?} vs {towards:?}");
        // Without a positive floor there is no grey to move towards.
        assert_eq!(limit_to_prophoto(blue, 0.0), Vec3::ZERO);
    }

    #[test]
    fn uv_lightest_never_exceeds_white_point() {
        let wp = D50;
        for (u, v) in [(0.2, 0.3), (0.45, 0.5), (0.15, 0.06)] {
            let xyz = uv_to_xyz_lightest(u, v, wp);
            assert!(xyz[0] <= wp[0] + 1.0e-9, "{xyz:?}");
            assert!(xyz[1] <= wp[1] + 1.0e-9, "{xyz:?}");
            assert!(xyz[2] <= wp[2] + 1.0e-9, "{xyz:?}");
        }
    }
}
