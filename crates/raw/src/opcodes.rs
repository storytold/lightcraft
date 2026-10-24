//! DNG opcode lists (`OpcodeList1/2/3`, DNG 1.7 chapter 7). Opcode lists are always big-endian.
//!
//! Parsed: every opcode (unknown ones are kept as [`Opcode::Unknown`]). Applied: `WarpRectilinear`,
//! `WarpRectilinear2` (opcode 14, DNG 1.6; see [`Opcode::WarpRectilinear2`]), `FixVignetteRadial`,
//! `FixBadPixelsConstant`, `FixBadPixelsList`, `MapTable`, `MapPolynomial`, `GainMap`, `DeltaPerRow/Column`,
//! `ScalePerRow/Column`. `TrimBounds` and `WarpFisheye` are recorded but not applied.
//!
//! Opcodes run in list order, except for the DNG 1.6 skip rule ([`applied_ops`]): a `WarpRectilinear` or
//! `WarpFisheye` that immediately follows an *optional* `WarpRectilinear2` is skipped (the older opcode is the
//! fallback for readers that don't know opcode 14).
//!
//! Value convention: list 1 runs on raw sample values (16-bit scale), lists 2 and 3 on values normalised to
//! [0, 1]; table/polynomial/delta opcodes are defined on the normalised range and are rescaled accordingly.

use crate::{Cfa, Rgb32f};
use serde::{Deserialize, Serialize};

/// Rectangular area + plane selection + pitch shared by the "area" opcodes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Area {
    pub top: u32,
    pub left: u32,
    pub bottom: u32,
    pub right: u32,
    pub plane: u32,
    pub planes: u32,
    pub row_pitch: u32,
    pub col_pitch: u32,
}

/// Values per coefficient set of `WarpRectilinear2`: 15 radial (`kr0..kr14`), 2 tangential (`kt0`, `kt1`),
/// `min_valid_radius`, `max_valid_radius`.
pub const WR2_LEN: usize = 19;
/// Most coefficient sets (planes) a `WarpRectilinear2` may carry.
const WR2_MAX_PLANES: usize = 4;
/// Largest magnitude accepted for a `WarpRectilinear2` coefficient (a real lens fit is far below this; it keeps the
/// polynomial finite for any radius up to 1).
const WR2_MAX_COEF: f64 = 1e9;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Opcode {
    /// Per-plane radial (kr0..kr3) + tangential (kt0, kt1) coefficients and the relative optical centre.
    WarpRectilinear {
        planes: Vec<[f64; 6]>,
        center: [f64; 2],
    },
    WarpFisheye {
        planes: Vec<[f64; 4]>,
        center: [f64; 2],
    },
    /// DNG 1.6 `WarpRectilinear2` (opcode 14; DNG specification 1.6.0.0, chapter 7, "WarpRectilinear2"). Per plane
    /// `[kr0 .. kr14, kt0, kt1, min_valid_radius, max_valid_radius]` ([`WR2_LEN`] values, see [`wr2_factor`]),
    /// the relative optical centre, and `reciprocalRadial`. `optional` is opcode flag bit 0, which turns on the skip
    /// rule for the next warp opcode.
    WarpRectilinear2 {
        planes: Vec<[f64; WR2_LEN]>,
        center: [f64; 2],
        reciprocal: bool,
        optional: bool,
    },
    FixVignetteRadial {
        k: [f64; 5],
        center: [f64; 2],
    },
    FixBadPixelsConstant {
        constant: u32,
        bayer_phase: u32,
    },
    FixBadPixelsList {
        bayer_phase: u32,
        points: Vec<(u32, u32)>,
        rects: Vec<[u32; 4]>,
    },
    TrimBounds {
        top: u32,
        left: u32,
        bottom: u32,
        right: u32,
    },
    MapTable {
        area: Area,
        table: Vec<u16>,
    },
    MapPolynomial {
        area: Area,
        coefficients: Vec<f64>,
    },
    GainMap {
        area: Area,
        points_v: u32,
        points_h: u32,
        spacing: [f64; 2],
        origin: [f64; 2],
        map_planes: u32,
        gains: Vec<f32>,
    },
    DeltaPerRow {
        area: Area,
        deltas: Vec<f32>,
    },
    DeltaPerColumn {
        area: Area,
        deltas: Vec<f32>,
    },
    ScalePerRow {
        area: Area,
        scales: Vec<f32>,
    },
    ScalePerColumn {
        area: Area,
        scales: Vec<f32>,
    },
    Unknown {
        id: u32,
        flags: u32,
        params: Vec<u8>,
    },
}

impl Opcode {
    /// Whether [`apply_list`] / [`apply_list3`] implement this opcode.
    pub fn is_applied(&self) -> bool {
        !matches!(self, Opcode::Unknown { .. } | Opcode::TrimBounds { .. } | Opcode::WarpFisheye { .. })
    }

    /// Whether this is a lens correction (`WarpRectilinear`, `WarpRectilinear2`, `FixVignetteRadial`): from a DNG, or
    /// the equivalent of a camera's own correction recorded in its raw (Panasonic RW2). Developers that apply these
    /// through the "profile corrections" of the develop settings remove them before demosaicing.
    pub fn is_lens_correction(&self) -> bool {
        matches!(self, Opcode::WarpRectilinear { .. } | Opcode::WarpRectilinear2 { .. } | Opcode::FixVignetteRadial { .. })
    }

    /// This lens correction re-expressed for the part `crop` of the `width × height` image it was written for: applied
    /// to the cropped image it gives the crop of the corrected whole. The centre moves into the crop and the
    /// coefficients are rescaled for the crop's farthest corner, the radius every coordinate is normalised by. Centres
    /// use the warps' convention (0 and 1 at the first and last pixel), also for `FixVignetteRadial`, as the develop
    /// pipeline reads them. `None` for other opcodes, an image or crop under 2 × 2, or a crop outside the image.
    pub fn cropped(&self, width: usize, height: usize, crop: crate::Rect) -> Option<Opcode> {
        let fits = crop.x.checked_add(crop.width).is_some_and(|r| r <= width) && crop.y.checked_add(crop.height).is_some_and(|b| b <= height);
        if width < 2 || height < 2 || crop.width < 2 || crop.height < 2 || !fits {
            return None;
        }
        let (aw, ah) = (width as f64 - 1.0, height as f64 - 1.0);
        let (cw, ch) = (crop.width as f64 - 1.0, crop.height as f64 - 1.0);
        // the centre in the crop, and the ratio of the crop's normalising radius to the image's
        let reframe = |c: [f64; 2]| -> Option<([f64; 2], f64)> {
            let (px, py) = (c[0] * aw, c[1] * ah);
            let (qx, qy) = (px - crop.x as f64, py - crop.y as f64);
            let s = max_radius(qx, qy, cw, ch) / max_radius(px, py, aw, ah);
            (s.is_finite() && s > 0.0 && qx.is_finite() && qy.is_finite()).then_some(([qx / cw, qy / ch], s))
        };
        match self {
            // source radius r·(kr0 + kr1 r² + kr2 r⁴ + kr3 r⁶) and tangential terms quadratic in r: with r = r'·s,
            // kr_i scales by s^2i and kt by s
            Opcode::WarpRectilinear { planes, center } => {
                let (center, s) = reframe(*center)?;
                let planes = planes
                    .iter()
                    .map(|k| {
                        let mut k = *k;
                        for (i, v) in k[..4].iter_mut().enumerate() {
                            *v *= s.powi(2 * i as i32);
                        }
                        k[4] *= s;
                        k[5] *= s;
                        k
                    })
                    .collect();
                Some(Opcode::WarpRectilinear { planes, center })
            }
            // radial factor Σ kr_i r^i between the valid radii: kr_i scales by s^i, the radii by 1/s
            Opcode::WarpRectilinear2 { planes, center, reciprocal, optional } => {
                let (center, s) = reframe(*center)?;
                let planes = planes
                    .iter()
                    .map(|k| {
                        let mut k = *k;
                        for (i, v) in k[..15].iter_mut().enumerate() {
                            *v *= s.powi(i as i32);
                        }
                        k[15] *= s;
                        k[16] *= s;
                        k[17] /= s;
                        k[18] /= s;
                        k
                    })
                    .collect();
                Some(Opcode::WarpRectilinear2 { planes, center, reciprocal: *reciprocal, optional: *optional })
            }
            // gain 1 + Σ k_i r^(2i+2)
            Opcode::FixVignetteRadial { k, center } => {
                let (center, s) = reframe(*center)?;
                let mut k = *k;
                for (i, v) in k.iter_mut().enumerate() {
                    *v *= s.powi(2 * i as i32 + 2);
                }
                Some(Opcode::FixVignetteRadial { k, center })
            }
            _ => None,
        }
    }
}

/// The opcodes of a list that a reader applies, in order: all of them, minus the DNG 1.6 skip rule (specification
/// 1.6.0.0, chapter 7) — a `WarpRectilinear` or `WarpFisheye` immediately after a `WarpRectilinear2` that is marked
/// optional is not applied. A `WarpRectilinear2` that could not be parsed ([`Opcode::Unknown`]) triggers no skip.
pub fn applied_ops(list: &[Opcode]) -> impl Iterator<Item = &Opcode> {
    let mut skip = false;
    list.iter().filter(move |op| {
        let skipped = skip && matches!(op, Opcode::WarpRectilinear { .. } | Opcode::WarpFisheye { .. });
        skip = matches!(op, Opcode::WarpRectilinear2 { optional: true, .. });
        !skipped
    })
}

/// `WarpRectilinear2` radial factor `r_real / r_ideal` for one coefficient set `k` at ideal radius `r`
/// (specification 1.6.0.0, "WarpRectilinear2"): `f(r) = Σ kr_p · r^p` (p = 0..14) evaluated at `r` clamped to
/// `[min_valid_radius, max_valid_radius]`, so `r_real = r · f` — or `r / f` when `reciprocalRadial` is non-zero.
/// `None` when the factor is not finite (a degenerate set, or `f = 0` with `reciprocalRadial`).
pub fn wr2_factor(k: &[f64; WR2_LEN], reciprocal: bool, r: f64) -> Option<f64> {
    let rc = r.clamp(k[17].min(k[18]), k[17].max(k[18]));
    let f = k[..15].iter().rev().fold(0.0, |acc, &c| acc * rc + c);
    let f = if reciprocal { 1.0 / f } else { f };
    f.is_finite().then_some(f)
}

/// The three opcode lists of a DNG raw IFD.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpcodeLists {
    pub list1: Vec<Opcode>,
    pub list2: Vec<Opcode>,
    pub list3: Vec<Opcode>,
}

struct Rd<'a> {
    d: &'a [u8],
    p: usize,
}

impl Rd<'_> {
    fn u32(&mut self) -> Option<u32> {
        let v = u32::from_be_bytes(self.d.get(self.p..self.p + 4)?.try_into().ok()?);
        self.p += 4;
        Some(v)
    }
    fn u16(&mut self) -> Option<u16> {
        let v = u16::from_be_bytes(self.d.get(self.p..self.p + 2)?.try_into().ok()?);
        self.p += 2;
        Some(v)
    }
    fn f64(&mut self) -> Option<f64> {
        let v = f64::from_be_bytes(self.d.get(self.p..self.p + 8)?.try_into().ok()?);
        self.p += 8;
        Some(v)
    }
    fn f32(&mut self) -> Option<f32> {
        let v = f32::from_be_bytes(self.d.get(self.p..self.p + 4)?.try_into().ok()?);
        self.p += 4;
        Some(v)
    }
    fn area(&mut self) -> Option<Area> {
        Some(Area {
            top: self.u32()?,
            left: self.u32()?,
            bottom: self.u32()?,
            right: self.u32()?,
            plane: self.u32()?,
            planes: self.u32()?,
            row_pitch: self.u32()?.max(1),
            col_pitch: self.u32()?.max(1),
        })
    }
    fn remaining(&self) -> usize {
        self.d.len().saturating_sub(self.p)
    }
}

fn parse_one(id: u32, flags: u32, params: &[u8]) -> Option<Opcode> {
    let mut r = Rd { d: params, p: 0 };
    let f = |r: &mut Rd| r.f64().filter(|v| v.is_finite());
    Some(match id {
        1 => {
            let n = r.u32()? as usize;
            if n == 0 || n > 4 {
                return None;
            }
            let mut planes = Vec::with_capacity(n);
            for _ in 0..n {
                planes.push([f(&mut r)?, f(&mut r)?, f(&mut r)?, f(&mut r)?, f(&mut r)?, f(&mut r)?]);
            }
            Opcode::WarpRectilinear { planes, center: [f(&mut r)?, f(&mut r)?] }
        }
        2 => {
            let n = r.u32()? as usize;
            if n == 0 || n > 4 {
                return None;
            }
            let mut planes = Vec::with_capacity(n);
            for _ in 0..n {
                planes.push([f(&mut r)?, f(&mut r)?, f(&mut r)?, f(&mut r)?]);
            }
            Opcode::WarpFisheye { planes, center: [f(&mut r)?, f(&mut r)?] }
        }
        14 => {
            let n = r.u32()? as usize;
            // N sets of 19 doubles, then the centre (2 doubles) and reciprocalRadial (LONG); nothing else
            if n == 0 || n > WR2_MAX_PLANES || params.len() != 4 + n * WR2_LEN * 8 + 16 + 4 {
                return None;
            }
            let mut planes = Vec::with_capacity(n);
            for _ in 0..n {
                let mut k = [0.0; WR2_LEN];
                for v in k.iter_mut() {
                    *v = f(&mut r).filter(|v| v.abs() <= WR2_MAX_COEF)?;
                }
                // the spec requires 0 <= min_valid_radius < max_valid_radius <= 1
                if !(k[17] >= 0.0 && k[17] < k[18] && k[18] <= 1.0) {
                    return None;
                }
                planes.push(k);
            }
            let center = [f(&mut r).filter(|v| v.abs() <= 4.0)?, f(&mut r).filter(|v| v.abs() <= 4.0)?];
            Opcode::WarpRectilinear2 { planes, center, reciprocal: r.u32()? != 0, optional: flags & 1 != 0 }
        }
        3 => Opcode::FixVignetteRadial { k: [f(&mut r)?, f(&mut r)?, f(&mut r)?, f(&mut r)?, f(&mut r)?], center: [f(&mut r)?, f(&mut r)?] },
        4 => Opcode::FixBadPixelsConstant { constant: r.u32()?, bayer_phase: r.u32()? },
        5 => {
            let bayer_phase = r.u32()?;
            let (np, nr) = (r.u32()? as usize, r.u32()? as usize);
            if np.saturating_mul(8).saturating_add(nr.saturating_mul(16)) > r.remaining() {
                return None;
            }
            let points = (0..np).map(|_| Some((r.u32()?, r.u32()?))).collect::<Option<_>>()?;
            let rects = (0..nr).map(|_| Some([r.u32()?, r.u32()?, r.u32()?, r.u32()?])).collect::<Option<_>>()?;
            Opcode::FixBadPixelsList { bayer_phase, points, rects }
        }
        6 => Opcode::TrimBounds { top: r.u32()?, left: r.u32()?, bottom: r.u32()?, right: r.u32()? },
        7 => {
            let area = r.area()?;
            let n = r.u32()? as usize;
            if n == 0 || n > 65536 || n * 2 > r.remaining() {
                return None;
            }
            Opcode::MapTable { area, table: (0..n).map(|_| r.u16()).collect::<Option<_>>()? }
        }
        8 => {
            let area = r.area()?;
            let deg = r.u32()? as usize;
            if deg > 8 {
                return None;
            }
            Opcode::MapPolynomial { area, coefficients: (0..=deg).map(|_| f(&mut r)).collect::<Option<_>>()? }
        }
        9 => {
            let area = r.area()?;
            let (pv, ph) = (r.u32()?, r.u32()?);
            let spacing = [f(&mut r)?, f(&mut r)?];
            let origin = [f(&mut r)?, f(&mut r)?];
            let mp = r.u32()?;
            let n = (pv as usize).checked_mul(ph as usize)?.checked_mul(mp as usize)?;
            if n == 0 || n * 4 > r.remaining() {
                return None;
            }
            let gains = (0..n).map(|_| r.f32()).collect::<Option<_>>()?;
            Opcode::GainMap { area, points_v: pv, points_h: ph, spacing, origin, map_planes: mp, gains }
        }
        10..=13 => {
            let area = r.area()?;
            let n = r.u32()? as usize;
            if n * 4 > r.remaining() {
                return None;
            }
            let v: Vec<f32> = (0..n).map(|_| r.f32()).collect::<Option<_>>()?;
            match id {
                10 => Opcode::DeltaPerRow { area, deltas: v },
                11 => Opcode::DeltaPerColumn { area, deltas: v },
                12 => Opcode::ScalePerRow { area, scales: v },
                _ => Opcode::ScalePerColumn { area, scales: v },
            }
        }
        _ => Opcode::Unknown { id, flags, params: params.to_vec() },
    })
}

fn put_area(o: &mut Vec<u8>, a: &Area) {
    for v in [a.top, a.left, a.bottom, a.right, a.plane, a.planes, a.row_pitch, a.col_pitch] {
        o.extend_from_slice(&v.to_be_bytes());
    }
}

fn params(op: &Opcode) -> (u32, u32, Vec<u8>) {
    let mut o = Vec::new();
    let f64s = |o: &mut Vec<u8>, v: &[f64]| v.iter().for_each(|x| o.extend_from_slice(&x.to_be_bytes()));
    let u32s = |o: &mut Vec<u8>, v: &[u32]| v.iter().for_each(|x| o.extend_from_slice(&x.to_be_bytes()));
    let f32s = |o: &mut Vec<u8>, v: &[f32]| v.iter().for_each(|x| o.extend_from_slice(&x.to_be_bytes()));
    let id = match op {
        Opcode::WarpRectilinear { planes, center } => {
            u32s(&mut o, &[planes.len() as u32]);
            planes.iter().for_each(|p| f64s(&mut o, p));
            f64s(&mut o, center);
            1
        }
        Opcode::WarpFisheye { planes, center } => {
            u32s(&mut o, &[planes.len() as u32]);
            planes.iter().for_each(|p| f64s(&mut o, p));
            f64s(&mut o, center);
            2
        }
        Opcode::WarpRectilinear2 { planes, center, reciprocal, optional } => {
            u32s(&mut o, &[planes.len() as u32]);
            planes.iter().for_each(|p| f64s(&mut o, p));
            f64s(&mut o, center);
            u32s(&mut o, &[*reciprocal as u32]);
            return (14, *optional as u32, o);
        }
        Opcode::FixVignetteRadial { k, center } => {
            f64s(&mut o, k);
            f64s(&mut o, center);
            3
        }
        Opcode::FixBadPixelsConstant { constant, bayer_phase } => {
            u32s(&mut o, &[*constant, *bayer_phase]);
            4
        }
        Opcode::FixBadPixelsList { bayer_phase, points, rects } => {
            u32s(&mut o, &[*bayer_phase, points.len() as u32, rects.len() as u32]);
            points.iter().for_each(|&(r, c)| u32s(&mut o, &[r, c]));
            rects.iter().for_each(|r| u32s(&mut o, r));
            5
        }
        Opcode::TrimBounds { top, left, bottom, right } => {
            u32s(&mut o, &[*top, *left, *bottom, *right]);
            6
        }
        Opcode::MapTable { area, table } => {
            put_area(&mut o, area);
            u32s(&mut o, &[table.len() as u32]);
            table.iter().for_each(|v| o.extend_from_slice(&v.to_be_bytes()));
            7
        }
        Opcode::MapPolynomial { area, coefficients } => {
            put_area(&mut o, area);
            u32s(&mut o, &[coefficients.len().saturating_sub(1) as u32]);
            f64s(&mut o, coefficients);
            8
        }
        Opcode::GainMap { area, points_v, points_h, spacing, origin, map_planes, gains } => {
            put_area(&mut o, area);
            u32s(&mut o, &[*points_v, *points_h]);
            f64s(&mut o, spacing);
            f64s(&mut o, origin);
            u32s(&mut o, &[*map_planes]);
            f32s(&mut o, gains);
            9
        }
        Opcode::DeltaPerRow { area, deltas: v }
        | Opcode::DeltaPerColumn { area, deltas: v }
        | Opcode::ScalePerRow { area, scales: v }
        | Opcode::ScalePerColumn { area, scales: v } => {
            put_area(&mut o, area);
            u32s(&mut o, &[v.len() as u32]);
            f32s(&mut o, v);
            match op {
                Opcode::DeltaPerRow { .. } => 10,
                Opcode::DeltaPerColumn { .. } => 11,
                Opcode::ScalePerRow { .. } => 12,
                _ => 13,
            }
        }
        Opcode::Unknown { id, flags, params } => return (*id, *flags, params.clone()),
    };
    (id, 0, o)
}

/// Serialise an opcode list (inverse of [`parse_list`]).
pub fn write_list(list: &[Opcode]) -> Vec<u8> {
    let mut out = (list.len() as u32).to_be_bytes().to_vec();
    for op in list {
        let (id, flags, p) = params(op);
        for v in [id, if id == 14 { 0x0106_0000 } else { 0x0103_0000 }, flags, p.len() as u32] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(&p);
    }
    out
}

/// Parse an opcode list blob. Malformed opcodes become [`Opcode::Unknown`]; a truncated list stops early.
pub fn parse_list(d: &[u8]) -> Vec<Opcode> {
    let mut r = Rd { d, p: 0 };
    let Some(count) = r.u32() else { return vec![] };
    let mut out = Vec::new();
    for _ in 0..count.min(4096) {
        let (Some(id), Some(_ver), Some(flags), Some(size)) = (r.u32(), r.u32(), r.u32(), r.u32()) else { break };
        let Some(params) = d.get(r.p..r.p.saturating_add(size as usize)) else { break };
        r.p += size as usize;
        out.push(parse_one(id, flags, params).unwrap_or(Opcode::Unknown { id, flags, params: params.to_vec() }));
    }
    out
}

fn area_iter(a: &Area, w: usize, h: usize, cpp: usize) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
    let (t, l) = (a.top as usize, a.left as usize);
    let (b, r) = ((a.bottom as usize).min(h), (a.right as usize).min(w));
    let (p0, p1) = (a.plane as usize, (a.plane as usize + a.planes.max(1) as usize).min(cpp));
    (t..b.max(t))
        .step_by(a.row_pitch.max(1) as usize)
        .flat_map(move |y| (l..r.max(l)).step_by(a.col_pitch.max(1) as usize).flat_map(move |x| (p0..p1.max(p0)).map(move |p| (x, y, p))))
}

fn gain_at(points_v: u32, points_h: u32, spacing: [f64; 2], origin: [f64; 2], map_planes: u32, gains: &[f32], rv: f64, rh: f64, plane: usize) -> f32 {
    let (pv, ph) = (points_v as usize, points_h as usize);
    let mp = map_planes as usize;
    let p = plane.min(mp - 1);
    let fy = if spacing[0] > 0.0 { ((rv - origin[0]) / spacing[0]).clamp(0.0, (pv - 1) as f64) } else { 0.0 };
    let fx = if spacing[1] > 0.0 { ((rh - origin[1]) / spacing[1]).clamp(0.0, (ph - 1) as f64) } else { 0.0 };
    let (y0, x0) = (fy.floor() as usize, fx.floor() as usize);
    let (y1, x1) = ((y0 + 1).min(pv - 1), (x0 + 1).min(ph - 1));
    let (ty, tx) = ((fy - y0 as f64) as f32, (fx - x0 as f64) as f32);
    let g = |y: usize, x: usize| gains[(y * ph + x) * mp + p];
    let top = g(y0, x0) + (g(y0, x1) - g(y0, x0)) * tx;
    let bot = g(y1, x0) + (g(y1, x1) - g(y1, x0)) * tx;
    top + (bot - top) * ty
}

/// Radius normaliser: distance from the centre to the farthest image corner.
pub(crate) fn max_radius(cx: f64, cy: f64, w: f64, h: f64) -> f64 {
    [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)].iter().map(|&(x, y)| (x - cx).hypot(y - cy)).fold(0.0, f64::max).max(1e-9)
}

/// Most pixels one bad-pixel opcode repairs (the list is 64 MB, `fix_pixels`' set about twice that). Uncapped, a
/// mosaic equal to `FixBadPixelsConstant`'s constant, or one `FixBadPixelsList` rectangle over the image, lists
/// every pixel: tens of GB at [`crate::MAX_SAMPLES`]. Pixels past the cap, in list order, are left as they are.
const MAX_BAD_PIXELS: usize = 1 << 22;

/// Apply a list to an interleaved `w × h × cpp` buffer. `cfa` (when the data is a mosaic) is used by the
/// bad-pixel opcodes. `scale` is the value of 1.0 in the buffer: 65535 for list 1 (raw 16-bit values), 1 for
/// lists 2 and 3 (normalised values).
pub fn apply_list(list: &[Opcode], buf: &mut [f32], w: usize, h: usize, cpp: usize, cfa: Option<&Cfa>, scale: f32) {
    for op in list {
        match op {
            Opcode::MapTable { area, table } => {
                let n = table.len();
                for (x, y, p) in area_iter(area, w, h, cpp) {
                    let v = &mut buf[(y * w + x) * cpp + p];
                    let idx = ((*v / scale * 65535.0).round().max(0.0) as usize).min(n - 1);
                    *v = table[idx] as f32 / 65535.0 * scale;
                }
            }
            Opcode::MapPolynomial { area, coefficients } => {
                for (x, y, p) in area_iter(area, w, h, cpp) {
                    let v = &mut buf[(y * w + x) * cpp + p];
                    let t = (*v / scale) as f64;
                    let r = coefficients.iter().rev().fold(0.0, |acc, &c| acc * t + c);
                    *v = r as f32 * scale;
                }
            }
            Opcode::GainMap { area, points_v, points_h, spacing, origin, map_planes, gains } => {
                for (x, y, p) in area_iter(area, w, h, cpp) {
                    let rv = y as f64 / h as f64;
                    let rh = x as f64 / w as f64;
                    let g = gain_at(*points_v, *points_h, *spacing, *origin, *map_planes, gains, rv, rh, p - area.plane as usize);
                    buf[(y * w + x) * cpp + p] *= g;
                }
            }
            Opcode::DeltaPerRow { area, deltas } | Opcode::DeltaPerColumn { area, deltas } => {
                let per_row = matches!(op, Opcode::DeltaPerRow { .. });
                for (x, y, p) in area_iter(area, w, h, cpp) {
                    let i =
                        if per_row { (y - area.top as usize) / area.row_pitch as usize } else { (x - area.left as usize) / area.col_pitch as usize };
                    if let Some(d) = deltas.get(i) {
                        buf[(y * w + x) * cpp + p] += d * scale;
                    }
                }
            }
            Opcode::ScalePerRow { area, scales } | Opcode::ScalePerColumn { area, scales } => {
                let per_row = matches!(op, Opcode::ScalePerRow { .. });
                for (x, y, p) in area_iter(area, w, h, cpp) {
                    let i =
                        if per_row { (y - area.top as usize) / area.row_pitch as usize } else { (x - area.left as usize) / area.col_pitch as usize };
                    if let Some(s) = scales.get(i) {
                        buf[(y * w + x) * cpp + p] *= s;
                    }
                }
            }
            Opcode::FixVignetteRadial { k, center } => {
                let (cx, cy) = (center[0] * w as f64, center[1] * h as f64);
                let m = max_radius(cx, cy, w as f64, h as f64);
                for y in 0..h {
                    for x in 0..w {
                        let r2 = ((x as f64 + 0.5 - cx).powi(2) + (y as f64 + 0.5 - cy).powi(2)) / (m * m);
                        let g = 1.0 + r2 * (k[0] + r2 * (k[1] + r2 * (k[2] + r2 * (k[3] + r2 * k[4]))));
                        for p in 0..cpp {
                            buf[(y * w + x) * cpp + p] *= g as f32;
                        }
                    }
                }
            }
            Opcode::FixBadPixelsConstant { constant, .. } => {
                let c = (*constant as f64 / 65535.0 * scale as f64) as f32;
                let bad: Vec<(usize, usize)> =
                    (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| cpp == 1 && buf[y * w + x] == c).take(MAX_BAD_PIXELS).collect();
                fix_pixels(buf, w, h, cpp, cfa, &bad);
            }
            Opcode::FixBadPixelsList { points, rects, .. } => {
                let mut bad: Vec<(usize, usize)> =
                    points.iter().map(|&(r, c)| (c as usize, r as usize)).filter(|&(x, y)| x < w && y < h).take(MAX_BAD_PIXELS).collect();
                'rects: for r in rects {
                    let [t, l, b, rr] = r.map(|v| v as usize);
                    for y in t..b.min(h) {
                        for x in l..rr.min(w) {
                            if bad.len() >= MAX_BAD_PIXELS {
                                break 'rects;
                            }
                            bad.push((x, y));
                        }
                    }
                }
                fix_pixels(buf, w, h, cpp, cfa, &bad);
            }
            _ => {}
        }
    }
}

/// Replace listed pixels with the mean of the nearest same-colour neighbours that are not themselves bad.
fn fix_pixels(buf: &mut [f32], w: usize, h: usize, cpp: usize, cfa: Option<&Cfa>, bad: &[(usize, usize)]) {
    if bad.is_empty() {
        return;
    }
    let mut is_bad = std::collections::HashSet::with_capacity(bad.len());
    for &b in bad {
        is_bad.insert(b);
    }
    let step: isize = match cfa {
        Some(c) if c.width == 2 && c.height == 2 => 2,
        Some(c) => c.width.max(c.height) as isize,
        None => 1,
    };
    for &(x, y) in bad {
        for p in 0..cpp {
            let mut sum = 0.0;
            let mut n = 0;
            for (dx, dy) in [(-step, 0), (step, 0), (0, -step), (0, step), (-step, -step), (step, step), (-step, step), (step, -step)] {
                let (nx, ny) = (x as isize + dx, y as isize + dy);
                if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize || is_bad.contains(&(nx as usize, ny as usize)) {
                    continue;
                }
                sum += buf[(ny as usize * w + nx as usize) * cpp + p];
                n += 1;
            }
            if n > 0 {
                buf[(y * w + x) * cpp + p] = sum / n as f32;
            }
        }
    }
}

/// Most full-image passes of one `OpcodeList3`: every warp record is a full-image allocation and resample
/// pass and every bad-pixel record a full flatten-and-rebuild, and a hostile record may repeat either
/// thousands of times. Real DNGs carry at most a couple; passes past the cap are skipped.
const MAX_LIST3_PASSES: usize = 8;

/// Apply `OpcodeList3` (on the demosaiced RGB image, values normalised to [0, 1]).
pub fn apply_list3(list: &[Opcode], img: &mut Rgb32f) {
    if list.is_empty() {
        return;
    }
    let (w, h) = (img.width, img.height);
    let mut passes = 0usize;
    for op in applied_ops(list) {
        match op {
            Opcode::WarpRectilinear { planes, center } => {
                if passes < MAX_LIST3_PASSES {
                    passes += 1;
                    *img = warp_rectilinear(img, planes, *center);
                }
            }
            Opcode::WarpRectilinear2 { planes, center, reciprocal, .. } => {
                if passes < MAX_LIST3_PASSES {
                    passes += 1;
                    *img = warp_rectilinear2(img, planes, *center, *reciprocal);
                }
            }
            Opcode::WarpFisheye { .. } | Opcode::TrimBounds { .. } | Opcode::Unknown { .. } => {}
            other => {
                if passes >= MAX_LIST3_PASSES {
                    continue;
                }
                passes += 1;
                let mut flat: Vec<f32> = img.data.iter().flat_map(|p| *p).collect();
                apply_list(std::slice::from_ref(other), &mut flat, w, h, 3, None, 1.0);
                for (d, c) in img.data.iter_mut().zip(flat.as_chunks::<3>().0) {
                    *d = [c[0], c[1], c[2]];
                }
            }
        }
    }
}

/// DNG `WarpRectilinear`: for each output pixel, the source position is the radial + tangential model around the
/// optical centre, with coordinates normalised by the distance to the farthest corner.
pub fn warp_rectilinear(img: &Rgb32f, planes: &[[f64; 6]], center: [f64; 2]) -> Rgb32f {
    let (w, h) = (img.width, img.height);
    let (cx, cy) = (center[0] * (w as f64 - 1.0), center[1] * (h as f64 - 1.0));
    let m = max_radius(cx, cy, w as f64 - 1.0, h as f64 - 1.0);
    let mut out = Rgb32f::new(w, h);
    let coef = |p: usize| planes[p.min(planes.len() - 1)];
    lightcraft_raster::par_rows(&mut out.data, w, |y, row| {
        for (x, px) in row.iter_mut().enumerate() {
            for (c, v) in px.iter_mut().enumerate() {
                let [kr0, kr1, kr2, kr3, kt0, kt1] = coef(c);
                let dx = (x as f64 - cx) / m;
                let dy = (y as f64 - cy) / m;
                let r2 = dx * dx + dy * dy;
                let f = kr0 + r2 * (kr1 + r2 * (kr2 + r2 * kr3));
                let sx = dx * f + kt0 * 2.0 * dx * dy + kt1 * (r2 + 2.0 * dx * dx);
                let sy = dy * f + kt1 * 2.0 * dx * dy + kt0 * (r2 + 2.0 * dy * dy);
                let (fx, fy) = (cx + m * sx, cy + m * sy);
                *v = img.sample_bilinear(fx as f32 + 0.5, fy as f32 + 0.5)[c];
            }
        }
    });
    out
}

/// DNG 1.6 `WarpRectilinear2`: the `WarpRectilinear` geometry (centre and normalisation as there), with the radial
/// factor of [`wr2_factor`] in place of the even polynomial in `r²`. A pixel whose factor is not finite stays put.
pub fn warp_rectilinear2(img: &Rgb32f, planes: &[[f64; WR2_LEN]], center: [f64; 2], reciprocal: bool) -> Rgb32f {
    let (w, h) = (img.width, img.height);
    let Some(last) = planes.len().checked_sub(1) else { return img.clone() };
    let mut out = Rgb32f::new(w, h);
    let (cx, cy) = (center[0] * (w as f64 - 1.0), center[1] * (h as f64 - 1.0));
    let m = max_radius(cx, cy, w as f64 - 1.0, h as f64 - 1.0);
    lightcraft_raster::par_rows(&mut out.data, w, |y, row| {
        for (x, px) in row.iter_mut().enumerate() {
            for (c, v) in px.iter_mut().enumerate() {
                let Some(k) = planes.get(c.min(last)) else { continue };
                let dx = (x as f64 - cx) / m;
                let dy = (y as f64 - cy) / m;
                let r2 = dx * dx + dy * dy;
                let (sx, sy) = match wr2_factor(k, reciprocal, r2.sqrt()) {
                    Some(f) => {
                        (dx * f + k[15] * 2.0 * dx * dy + k[16] * (r2 + 2.0 * dx * dx), dy * f + k[16] * 2.0 * dx * dy + k[15] * (r2 + 2.0 * dy * dy))
                    }
                    None => (dx, dy),
                };
                let (fx, fy) = (cx + m * sx, cy + m * sy);
                *v = img.sample_bilinear(fx as f32 + 0.5, fy as f32 + 0.5)[c];
            }
        }
    });
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) struct ListWriter(pub Vec<u8>, pub u32);

    impl ListWriter {
        pub fn new() -> Self {
            ListWriter(Vec::new(), 0)
        }
        pub fn op(&mut self, id: u32, params: &[u8]) -> &mut Self {
            for v in [id, 0x01030000, 0, params.len() as u32] {
                self.0.extend_from_slice(&v.to_be_bytes());
            }
            self.0.extend_from_slice(params);
            self.1 += 1;
            self
        }
        pub fn finish(&self) -> Vec<u8> {
            let mut v = self.1.to_be_bytes().to_vec();
            v.extend_from_slice(&self.0);
            v
        }
    }

    pub(crate) fn be(vals: &[f64]) -> Vec<u8> {
        vals.iter().flat_map(|v| v.to_be_bytes()).collect()
    }
    pub(crate) fn area(a: [u32; 8]) -> Vec<u8> {
        a.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    /// Warp and bad-pixel records each cost a full-image pass, and a hostile `OpcodeList3` repeats them
    /// thousands of times ([`MAX_LIST3_PASSES`] bounds the multiplier); identity warps below the cap must
    /// still apply, exactly, so the same image comes out.
    #[test]
    fn list3_pass_cap_skips_repeated_records() {
        let mut img = Rgb32f::new(8, 6);
        for (i, px) in img.data.iter_mut().enumerate() {
            *px = [i as f32 / 2.0, (i % 8) as f32, (i / 8) as f32];
        }
        let orig: Vec<[f32; 3]> = img.data.clone();
        let list: Vec<Opcode> =
            (0..MAX_LIST3_PASSES * 5).map(|_| Opcode::WarpRectilinear { planes: vec![[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]], center: [0.5, 0.5] }).collect();
        apply_list3(&list, &mut img);
        assert_eq!(img.data, orig, "identity warps change nothing, and the cap skips the repeated rest");

        // a real warp below the cap is still applied
        let one = [Opcode::WarpRectilinear { planes: vec![[2.0, 0.0, 0.0, 0.0, 0.0, 0.0]], center: [0.5, 0.5] }];
        apply_list3(&one, &mut img);
        assert_ne!(img.data, orig, "a genuine warp record is not skipped");
    }

    #[test]
    fn parse_and_apply_area_ops() {
        let (w, h) = (4usize, 3usize);
        let mut gm = area([0, 0, 3, 4, 0, 1, 1, 1]);
        for v in [2u32, 2] {
            gm.extend_from_slice(&v.to_be_bytes());
        }
        gm.extend(be(&[1.0, 1.0, 0.0, 0.0]));
        gm.extend_from_slice(&1u32.to_be_bytes());
        for g in [1.0f32, 2.0, 1.0, 2.0] {
            gm.extend_from_slice(&g.to_be_bytes());
        }
        let mut dpr = area([0, 0, 3, 4, 0, 1, 1, 1]);
        dpr.extend_from_slice(&3u32.to_be_bytes());
        for d in [0.0f32, 0.1, 0.2] {
            dpr.extend_from_slice(&d.to_be_bytes());
        }
        let mut poly = area([0, 0, 3, 4, 0, 1, 1, 2]);
        poly.extend_from_slice(&1u32.to_be_bytes());
        poly.extend(be(&[0.0, 2.0]));
        let blob = ListWriter::new().op(9, &gm).op(10, &dpr).op(8, &poly).op(99, &[1, 2, 3]).finish();
        let list = parse_list(&blob);
        assert_eq!(list.len(), 4);
        assert!(matches!(list[3], Opcode::Unknown { id: 99, .. }));
        assert!(list[..3].iter().all(|o| o.is_applied()));
        let mut buf = vec![0.25f32; w * h];
        apply_list(&list, &mut buf, w, h, 1, None, 1.0);
        // gain map: horizontal ramp 1 → 2 across relative x 0..1 (x / w)
        let expect = |x: usize, y: usize| {
            let g = 1.0 + x as f32 / w as f32;
            let v = 0.25 * g + [0.0, 0.1, 0.2][y];
            if x.is_multiple_of(2) { v * 2.0 } else { v }
        };
        for y in 0..h {
            for x in 0..w {
                assert!((buf[y * w + x] - expect(x, y)).abs() < 1e-6, "{x},{y}: {} vs {}", buf[y * w + x], expect(x, y));
            }
        }
    }

    #[test]
    fn map_table_raw_scale_and_bad_pixels() {
        let mut t = area([0, 0, 2, 2, 0, 1, 1, 1]);
        t.extend_from_slice(&3u32.to_be_bytes());
        for v in [0u16, 30000, 65535] {
            t.extend_from_slice(&v.to_be_bytes());
        }
        let list = parse_list(&ListWriter::new().op(7, &t).finish());
        let mut buf = vec![0.0, 1.0 / 65535.0, 2.0 / 65535.0, 0.5];
        apply_list(&list, &mut buf, 2, 2, 1, None, 1.0);
        assert_eq!(buf[0], 0.0);
        assert!((buf[1] - 30000.0 / 65535.0).abs() < 1e-7);
        assert_eq!(buf[3], 1.0);

        let mut p = vec![];
        for v in [7u32, 0] {
            p.extend_from_slice(&v.to_be_bytes());
        }
        let list = parse_list(&ListWriter::new().op(4, &p).finish());
        let cfa = Cfa::bayer("RGGB").unwrap();
        let mut img: Vec<f32> = (0..36).map(|i| 100.0 + (i % 6) as f32).collect();
        img[2 * 6 + 2] = 7.0;
        apply_list(&list, &mut img, 6, 6, 1, Some(&cfa), 65535.0);
        assert_eq!(img[2 * 6 + 2], 102.0);
    }

    /// A mosaic equal to the bad-pixel constant, or one rectangle over the whole image, used to list every pixel as
    /// bad: w × h entries (plus a set of them), tens of GB at the decoder's size limit. Here the rows between a good
    /// first row (500) and a good last row (900) are bad: 2052 rows of 2048 pixels, four rows more than the 1 << 22
    /// cap. Row 1, within the cap, is repaired from the first row; the second-last row, past the cap, must be left
    /// as it is (uncapped, it was rewritten from the last row).
    fn assert_bad_pixels_capped(op: Opcode) {
        let (w, h) = (2048, 2054);
        let mut buf = vec![0.0f32; w * h];
        buf[..w].fill(500.0);
        buf[(h - 1) * w..].fill(900.0);
        apply_list(&[op], &mut buf, w, h, 1, None, 65535.0);
        assert!(buf[w..2 * w].iter().all(|&v| v == 500.0), "row 1 (within the cap) not repaired: {:?}", &buf[w..w + 4]);
        let past = &buf[(h - 2) * w..(h - 1) * w];
        assert!(past.iter().all(|&v| v == 0.0), "a row past the cap was rewritten: {:?}", &past[..4]);
    }

    #[test]
    fn bad_pixels_constant_is_capped() {
        assert_bad_pixels_capped(Opcode::FixBadPixelsConstant { constant: 0, bayer_phase: 0 });
    }

    #[test]
    fn bad_pixels_list_is_capped_inside_a_rectangle() {
        // top, left, bottom, right: every row but the first and the last
        assert_bad_pixels_capped(Opcode::FixBadPixelsList { bayer_phase: 0, points: vec![], rects: vec![[1, 0, 2053, 2048]] });
    }

    #[test]
    fn vignette_and_warp_identity() {
        let mut img = Rgb32f::from_fn(9, 7, |x, y| [x as f32 / 9.0, y as f32 / 7.0, 0.5]);
        let orig = img.clone();
        let mut p = be(&[0.0; 5]);
        p.extend(be(&[0.5, 0.5]));
        let mut warp = 1u32.to_be_bytes().to_vec();
        warp.extend(be(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5]));
        let list = parse_list(&ListWriter::new().op(3, &p).op(1, &warp).finish());
        assert_eq!(list.len(), 2);
        apply_list3(&list, &mut img);
        for (a, b) in img.data.iter().zip(&orig.data) {
            for c in 0..3 {
                assert!((a[c] - b[c]).abs() < 1e-5);
            }
        }
        // positive k0 brightens the corners more than the centre
        let mut p = be(&[1.0, 0.0, 0.0, 0.0, 0.0]);
        p.extend(be(&[0.5, 0.5]));
        let list = parse_list(&ListWriter::new().op(3, &p).finish());
        let mut flat = Rgb32f::filled(9, 9, [0.5; 3]);
        apply_list3(&list, &mut flat);
        assert!(flat.get(0, 0)[0] > 0.85 && (flat.get(4, 4)[0] - 0.5).abs() < 0.01, "{:?}", flat.get(0, 0));
    }

    #[test]
    fn warp_barrel_moves_pixels_outward() {
        let img = Rgb32f::from_fn(31, 31, |x, y| if x == 24 && y == 15 { [1.0; 3] } else { [0.0; 3] });
        // source radius = r (1 + k r²) with k < 0 → content moves outward
        let out = warp_rectilinear(&img, &[[1.0, -0.6, 0.0, 0.0, 0.0, 0.0]], [0.5, 0.5]);
        let (bx, _) = (0..31).map(|x| (x, out.get(x, 15)[0])).fold((0, 0.0), |a, b| if b.1 > a.1 { b } else { a });
        assert!(bx >= 25, "{bx}");
    }

    #[test]
    fn write_parse_roundtrip() {
        let a = Area { top: 1, left: 2, bottom: 30, right: 40, plane: 0, planes: 3, row_pitch: 1, col_pitch: 2 };
        let list = vec![
            Opcode::WarpRectilinear { planes: vec![[1.0, 0.01, -0.002, 0.0, 1e-4, -2e-4]; 3], center: [0.5, 0.49] },
            Opcode::WarpFisheye { planes: vec![[1.0, 0.1, 0.0, 0.0]], center: [0.5, 0.5] },
            Opcode::FixVignetteRadial { k: [0.1, 0.2, 0.0, 0.0, 0.0], center: [0.5, 0.5] },
            Opcode::FixBadPixelsConstant { constant: 0, bayer_phase: 1 },
            Opcode::FixBadPixelsList { bayer_phase: 0, points: vec![(3, 4)], rects: vec![[1, 1, 2, 2]] },
            Opcode::TrimBounds { top: 1, left: 1, bottom: 9, right: 9 },
            Opcode::MapTable { area: a, table: vec![0, 100, 65535] },
            Opcode::MapPolynomial { area: a, coefficients: vec![0.0, 1.0, -0.1] },
            Opcode::GainMap { area: a, points_v: 2, points_h: 1, spacing: [1.0, 1.0], origin: [0.0, 0.0], map_planes: 1, gains: vec![1.0, 1.5] },
            Opcode::DeltaPerRow { area: a, deltas: vec![0.1, 0.2] },
            Opcode::DeltaPerColumn { area: a, deltas: vec![0.3] },
            Opcode::ScalePerRow { area: a, scales: vec![1.1] },
            Opcode::ScalePerColumn { area: a, scales: vec![0.9, 1.0] },
            Opcode::WarpRectilinear2 {
                planes: vec![wr2_set(&[(0, 0.5), (3, 0.1)]), wr2_set(&[(0, 0.25)])],
                center: [0.5, 0.49],
                reciprocal: true,
                optional: true,
            },
            Opcode::Unknown { id: 77, flags: 1, params: vec![1, 2, 3] },
        ];
        assert_eq!(parse_list(&write_list(&list)), list);
    }

    #[test]
    fn truncated_lists_do_not_panic() {
        let mut gm = area([0, 0, 3, 4, 0, 1, 1, 1]);
        gm.extend_from_slice(&[0xff; 64]);
        let blob = ListWriter::new().op(9, &gm).op(5, &[0xff; 12]).op(1, &[0, 0, 0, 9]).finish();
        for n in 0..blob.len() {
            let l = parse_list(&blob[..n]);
            let mut buf = vec![0.5f32; 12];
            apply_list(&l, &mut buf, 4, 3, 1, None, 1.0);
        }
    }

    /// `WarpRectilinear2` parameter bytes (spec 1.6.0.0: N, then per set kr0..kr14, kt0, kt1, min/max valid radius,
    /// then the centre and `reciprocalRadial`).
    fn wr2_params(sets: &[[f64; WR2_LEN]], center: [f64; 2], reciprocal: u32) -> Vec<u8> {
        let mut p = (sets.len() as u32).to_be_bytes().to_vec();
        for s in sets {
            p.extend(be(s));
        }
        p.extend(be(&center));
        p.extend_from_slice(&reciprocal.to_be_bytes());
        p
    }

    fn wr2_set(kr: &[(usize, f64)]) -> [f64; WR2_LEN] {
        let mut k = [0.0; WR2_LEN];
        for &(i, v) in kr {
            k[i] = v;
        }
        k[18] = 1.0;
        k
    }

    #[test]
    fn warp_rectilinear2_parses_the_spec_layout() {
        let mut k = wr2_set(&[(0, 1.0), (1, 0.25), (14, -2.0)]);
        k[15] = 0.001;
        k[16] = -0.002;
        k[17] = 0.1;
        k[18] = 0.9;
        let blob = ListWriter::new().op(14, &wr2_params(&[k, wr2_set(&[(0, 1.0)])], [0.5, 0.25], 1)).finish();
        let list = parse_list(&blob);
        assert_eq!(
            list,
            vec![Opcode::WarpRectilinear2 { planes: vec![k, wr2_set(&[(0, 1.0)])], center: [0.5, 0.25], reciprocal: true, optional: false }]
        );
        assert!(list[0].is_applied() && list[0].is_lens_correction());
        // written back as opcode 14, version 1.6.0.0, optional flag kept
        let again = vec![Opcode::WarpRectilinear2 { planes: vec![k], center: [0.5, 0.5], reciprocal: false, optional: true }];
        let bytes = write_list(&again);
        assert_eq!(bytes.get(4..8), Some(&14u32.to_be_bytes()[..]));
        assert_eq!(bytes.get(8..12), Some(&0x0106_0000u32.to_be_bytes()[..]));
        assert_eq!(parse_list(&bytes), again);
    }

    #[test]
    fn warp_rectilinear2_identity_leaves_the_image() {
        let img = Rgb32f::from_fn(21, 15, |x, y| [x as f32 / 21.0, y as f32 / 15.0, 0.25]);
        for reciprocal in [false, true] {
            let mut i = img.clone();
            let p = wr2_params(&[wr2_set(&[(0, 1.0)])], [0.5, 0.5], reciprocal as u32);
            apply_list3(&parse_list(&ListWriter::new().op(14, &p).finish()), &mut i);
            for (a, b) in i.data.iter().zip(&img.data) {
                for c in 0..3 {
                    assert!((a[c] - b[c]).abs() < 1e-5, "{a:?} {b:?}");
                }
            }
        }
    }

    #[test]
    fn warp_rectilinear2_matches_warp_rectilinear_on_even_terms() {
        // kr0 + kr2 r^2 + kr4 r^4 is WarpRectilinear's kr0 + kr1 r^2 + kr2 r^4
        let img = Rgb32f::from_fn(31, 31, |x, y| [(x * 7 % 13) as f32 / 13.0, (y * 5 % 11) as f32 / 11.0, ((x + y) % 4) as f32 / 4.0]);
        let a = warp_rectilinear(&img, &[[1.0, -0.3, 0.1, 0.0, 0.01, -0.02]], [0.5, 0.5]);
        let mut k = wr2_set(&[(0, 1.0), (2, -0.3), (4, 0.1)]);
        k[15] = 0.01;
        k[16] = -0.02;
        let b = warp_rectilinear2(&img, &[k], [0.5, 0.5], false);
        for (p, q) in a.data.iter().zip(&b.data) {
            for c in 0..3 {
                assert!((p[c] - q[c]).abs() < 1e-5);
            }
        }
        // odd terms, the reciprocal and the valid-radius clamp give the factor the spec defines
        let k = wr2_set(&[(0, 1.0), (1, 0.5)]);
        assert_eq!(wr2_factor(&k, false, 0.5), Some(1.25));
        assert_eq!(wr2_factor(&k, true, 0.5), Some(0.8));
        let mut k = k;
        k[18] = 0.5; // max_valid_radius
        assert_eq!(wr2_factor(&k, false, 0.9), Some(1.25));
        k[17] = 0.25; // min_valid_radius
        assert_eq!(wr2_factor(&k, false, 0.0), Some(1.125));
        assert_eq!(wr2_factor(&wr2_set(&[]), true, 0.3), None, "1 / 0");
    }

    #[test]
    fn malformed_warp_rectilinear2_is_unknown_not_a_panic() {
        let good = wr2_params(&[wr2_set(&[(0, 1.0)])], [0.5, 0.5], 0);
        let is_wr2 = |p: &[u8]| matches!(parse_list(&ListWriter::new().op(14, p).finish()).as_slice(), [Opcode::WarpRectilinear2 { .. }]);
        assert!(is_wr2(&good));
        // every truncation and any trailing byte
        for n in 0..good.len() {
            assert!(!is_wr2(&good[..n]), "{n}");
        }
        assert!(!is_wr2(&[&good[..], &[0u8][..]].concat()));
        // N = 0, N too big for the data, N beyond the plane limit
        for n in [0u32, 2, 5, u32::MAX] {
            let mut p = good.clone();
            p[..4].copy_from_slice(&n.to_be_bytes());
            assert!(!is_wr2(&p), "N = {n}");
        }
        // non-finite or absurd values, in a coefficient, the valid range and the centre
        let patch = |at: usize, v: f64| {
            let mut p = good.clone();
            p[at..at + 8].copy_from_slice(&v.to_be_bytes());
            p
        };
        for (at, v) in [
            (4, f64::NAN),
            (4 + 8, f64::INFINITY),
            (4 + 8 * 3, 1e300),
            (4 + 8 * 17, -0.5),
            (4 + 8 * 18, 1.5),
            (4 + 8 * 17, 1.0),
            (4 + 19 * 8, f64::NAN),
            (4 + 19 * 8 + 8, 1e30),
        ] {
            assert!(!is_wr2(&patch(at, v)), "{at} {v}");
        }
        // refused ones stay Unknown and are not applied
        let list = parse_list(&ListWriter::new().op(14, &good[..10]).finish());
        assert!(matches!(list.as_slice(), [Opcode::Unknown { id: 14, .. }]));
        let mut img = Rgb32f::filled(5, 5, [0.5; 3]);
        apply_list3(&list, &mut img);
        assert_eq!(img.get(2, 2), [0.5; 3]);
        // a hostile but accepted set (huge radial terms) still gives a finite image
        let k = wr2_set(&[(0, 1.0), (14, 1e9), (13, -1e9)]);
        let img = Rgb32f::from_fn(9, 9, |x, y| [x as f32 / 9.0, y as f32 / 9.0, 0.5]);
        for reciprocal in [false, true] {
            let out = warp_rectilinear2(&img, &[k], [0.5, 0.5], reciprocal);
            assert!(out.data.iter().all(|p| p.iter().all(|v| v.is_finite())));
        }
    }

    #[test]
    fn warp_list_order_and_the_optional_skip_rule() {
        let wr2 = |optional| Opcode::WarpRectilinear2 { planes: vec![wr2_set(&[(0, 1.0)])], center: [0.5, 0.5], reciprocal: false, optional };
        let wr = Opcode::WarpRectilinear { planes: vec![[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]], center: [0.5, 0.5] };
        let fish = Opcode::WarpFisheye { planes: vec![[1.0, 0.0, 0.0, 0.0]], center: [0.5, 0.5] };
        let vig = Opcode::FixVignetteRadial { k: [0.0; 5], center: [0.5, 0.5] };
        let d = std::mem::discriminant::<Opcode>;
        let ids = |l: &[Opcode]| applied_ops(l).map(d).collect::<Vec<_>>();
        // WarpRectilinear2 (optional) then WarpRectilinear / WarpFisheye: the second is skipped
        assert_eq!(ids(&[wr2(true), wr.clone()]), vec![d(&wr2(true))]);
        assert_eq!(ids(&[wr2(true), fish.clone()]), vec![d(&wr2(true))]);
        // not optional: both run, in list order
        assert_eq!(ids(&[wr2(false), wr.clone()]), vec![d(&wr2(false)), d(&wr)]);
        // only the opcode immediately after is skipped
        assert_eq!(ids(&[wr2(true), vig.clone(), wr.clone()]), vec![d(&wr2(true)), d(&vig), d(&wr)]);
        // two in a row: both run, and the WarpRectilinear after the optional second one is skipped
        assert_eq!(ids(&[wr2(true), wr2(true), wr.clone()]), vec![d(&wr2(true)); 2]);
        // a WarpRectilinear before the WarpRectilinear2 runs, in list order
        assert_eq!(ids(&[wr.clone(), wr2(true)]), vec![d(&wr), d(&wr2(true))]);
        // the same from bytes: an optional (flag 1) opcode 14 followed by opcode 1
        let w2 = wr2_params(&[wr2_set(&[(0, 1.0)])], [0.5, 0.5], 0);
        let mut one = 1u32.to_be_bytes().to_vec();
        one.extend(be(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5]));
        let mut blob = 2u32.to_be_bytes().to_vec();
        for (id, flags, p) in [(14u32, 1u32, &w2), (1, 1, &one)] {
            for v in [id, 0x0106_0000, flags, p.len() as u32] {
                blob.extend_from_slice(&v.to_be_bytes());
            }
            blob.extend_from_slice(p);
        }
        let list = parse_list(&blob);
        assert_eq!(list.len(), 2);
        assert_eq!(applied_ops(&list).count(), 1);
        // a refused (Unknown) opcode 14 triggers no skip
        let bad = Opcode::Unknown { id: 14, flags: 1, params: vec![] };
        assert!(applied_ops(&[bad, wr]).any(|o| matches!(o, Opcode::WarpRectilinear { .. })));
    }

    /// `cropped` re-expresses a lens correction for a crop: the corrected crop is the crop of the corrected whole. A
    /// linear ramp makes bilinear sampling exact, so only the borders (where the warp reaches past the crop) differ.
    #[test]
    fn cropped_lens_corrections_correct_the_crop_like_the_whole() {
        let (w, h) = (90usize, 64usize);
        let crop = crate::Rect::new(7, 5, 70, 50);
        let ramp = Rgb32f::from_fn(w, h, |x, y| [0.002 * x as f32 + 0.001 * y as f32 + 0.1, 0.3 + 0.0015 * y as f32, 0.2]);
        let mut wr2 = [0.0; WR2_LEN];
        wr2[..5].copy_from_slice(&[1.0, 0.01, -0.03, 0.0, 0.01]);
        wr2[15] = 0.002;
        wr2[16] = -0.001;
        wr2[17] = 0.05;
        wr2[18] = 0.9;
        let warps = [
            Opcode::WarpRectilinear { planes: vec![[0.97, 0.04, -0.01, 0.002, 0.003, -0.002]], center: [0.47, 0.53] },
            Opcode::WarpRectilinear2 { planes: vec![wr2], center: [0.52, 0.46], reciprocal: false, optional: true },
            Opcode::WarpRectilinear2 { planes: vec![wr2], center: [0.52, 0.46], reciprocal: true, optional: false },
        ];
        for op in &warps {
            let mut whole = ramp.clone();
            apply_list3(std::slice::from_ref(op), &mut whole);
            let re = op.cropped(w, h, crop).unwrap();
            let mut part = ramp.clone().into_crop(crop.x, crop.y, crop.width, crop.height);
            apply_list3(std::slice::from_ref(&re), &mut part);
            let mut worst = 0f32;
            for y in 6..crop.height - 6 {
                for x in 6..crop.width - 6 {
                    let (a, b) = (whole.data[(y + crop.y) * w + x + crop.x], part.data[y * crop.width + x]);
                    worst = (0..3).map(|c| (a[c] - b[c]).abs()).fold(worst, f32::max);
                }
            }
            assert!(worst < 1e-4, "{op:?} → {re:?}: {worst}");
        }
        // the vignette, in the warps' centre convention the develop pipeline reads it with
        let gain = |k: &[f64; 5], c: [f64; 2], w: usize, h: usize, x: f64, y: f64| {
            let (aw, ah) = (w as f64 - 1.0, h as f64 - 1.0);
            let (px, py) = (c[0] * aw, c[1] * ah);
            let r2 = ((x - px).powi(2) + (y - py).powi(2)) / max_radius(px, py, aw, ah).powi(2);
            1.0 + (0..5).map(|i| k[i] * r2.powi(i as i32 + 1)).sum::<f64>()
        };
        let (k, center) = ([-0.3, 0.1, -0.05, 0.02, -0.01], [0.45, 0.55]);
        let Some(Opcode::FixVignetteRadial { k: k2, center: c2 }) = (Opcode::FixVignetteRadial { k, center }).cropped(w, h, crop) else {
            panic!("vignette")
        };
        for (x, y) in [(0.0, 0.0), (69.0, 49.0), (33.5, 20.0), (69.0, 0.0), (12.0, 40.0)] {
            let (a, b) = (gain(&k, center, w, h, x + crop.x as f64, y + crop.y as f64), gain(&k2, c2, crop.width, crop.height, x, y));
            assert!((a - b).abs() < 1e-12, "({x}, {y}): {a} vs {b}");
        }
        // no change for a full-frame "crop"; nothing for other opcodes, tiny images or a crop past the edge
        assert_eq!(warps[0].cropped(w, h, crate::Rect::new(0, 0, w, h)).unwrap(), warps[0]);
        assert!(Opcode::TrimBounds { top: 0, left: 0, bottom: 2, right: 2 }.cropped(w, h, crop).is_none());
        assert!(warps[0].cropped(1, h, crate::Rect::new(0, 0, 1, h)).is_none());
        assert!(warps[0].cropped(w, h, crate::Rect::new(30, 0, 70, 50)).is_none());
        assert!(warps[0].cropped(w, h, crate::Rect::new(usize::MAX, 0, 70, 50)).is_none());
    }
}
