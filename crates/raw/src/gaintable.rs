//! DNG `ProfileGainTableMap` (tag 52525, DNG 1.6) and `ProfileGainTableMap2` (tag 52544, DNG 1.7):
//! a coarse grid of 1D gain tables carrying the maker's local tone mapping. Apple ProRAW has one;
//! it lifts shadows and compresses highlights region by region. Without it a ProRAW renders
//! with a very dark foreground.
//!
//! - The grid has `MapPointsV × MapPointsH` tables of `MapPointsN` gains. Its origin and spacing
//!   are relative to the active area (1.0 = the active area's height or width) and may lie
//!   partly outside it. Pixels are sampled at their centres. Between grid points the four
//!   surrounding tables are blended bilinearly; outside the grid the edge tables repeat.
//! - The table input is the dot product of (R, G, B, min, max) with the tag's five weights,
//!   clamped to 0..=1. Version 2 then raises it to the tag's `Gamma`. The table is read at
//!   index `input · MapPointsN` with linear interpolation, clamped to the last entry. The gain
//!   multiplies R, G and B, and values above 1 are kept.
//! - It is applied in linear ProPhoto (RIMM) RGB, D50, after `BaselineExposure` and before
//!   `ProfileToneCurve` (see [`crate::profile::ProfileTables`] for where it sits relative to the
//!   hue/saturation map and the look table).
//! - Version 2 stores gains as u8 or u16 (mapped linearly onto `GainMin..=GainMax`), f16 or f32.
//!   It takes precedence over version 1 when both are present.
//!
//! Multi-byte fields use the file's byte order. Malformed tags are ignored and the file renders
//! without the map: a wrong size, zero or huge dimensions, non-finite or negative gains,
//! non-positive spacing, or gamma outside 0.25..=4.

use crate::Rect;
use lightcraft_tiff::ByteOrder;
use serde::{Deserialize, Serialize};

/// Upper bound on `MapPointsV × MapPointsH` (Apple uses 6 × 8).
const MAX_TABLES: usize = 1 << 12;
/// Upper bound on the total number of gains (Apple: 6 × 8 × 257 = 12 336).
const MAX_GAINS: usize = 1 << 22;

/// A parsed `ProfileGainTableMap` / `ProfileGainTableMap2`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GainTableMap {
    /// Tables vertically (`MapPointsV`) and horizontally (`MapPointsH`), and points per table (`MapPointsN`).
    pub points_v: usize,
    pub points_h: usize,
    pub points_n: usize,
    /// Grid spacing and origin, relative to the active area's height (`_v`) and width (`_h`).
    pub spacing_v: f64,
    pub spacing_h: f64,
    pub origin_v: f64,
    pub origin_h: f64,
    /// Weights of (R, G, B, min, max) for the table input.
    pub weights: [f32; 5],
    /// Exponent for the table input (1.0 for the version-1 tag).
    pub gamma: f32,
    /// `points_v × points_h × points_n` gains, row-major with the table points innermost. Integer
    /// storage has already been mapped onto `GainMin..=GainMax`. Mapping before interpolating is
    /// the same as mapping after, because the mapping is linear.
    pub gains: Vec<f32>,
}

impl GainTableMap {
    /// Parse the tag's bytes (`version2`: `ProfileGainTableMap2`). `None` when malformed.
    pub fn parse(b: &[u8], order: ByteOrder, version2: bool) -> Option<GainTableMap> {
        let take = |o: usize, n: usize| b.get(o..o.checked_add(n)?);
        let u32_at = |o: usize| take(o, 4).and_then(|s| s.try_into().ok()).map(|a: [u8; 4]| order.u32(a));
        let f32_at = |o: usize| u32_at(o).map(f32::from_bits);
        let f64_at = |o: usize| take(o, 8).and_then(|s| s.try_into().ok()).map(|a: [u8; 8]| f64::from_bits(order.u64(a)));
        let (points_v, points_h) = (u32_at(0)? as usize, u32_at(4)? as usize);
        let (spacing_v, spacing_h, origin_v, origin_h) = (f64_at(8)?, f64_at(16)?, f64_at(24)?, f64_at(32)?);
        let points_n = u32_at(40)? as usize;
        let mut weights = [0f32; 5];
        for (i, w) in weights.iter_mut().enumerate() {
            *w = f32_at(44 + 4 * i)?;
        }
        let tables = points_v.checked_mul(points_h).filter(|&t| t > 0 && t <= MAX_TABLES)?;
        let count = tables.checked_mul(points_n).filter(|&c| c > 0 && c <= MAX_GAINS)?;
        let spacing_ok = |s: f64, points: usize| s.is_finite() && (s > 0.0 || points == 1);
        if !spacing_ok(spacing_v, points_v)
            || !spacing_ok(spacing_h, points_h)
            || !origin_v.is_finite()
            || !origin_h.is_finite()
            || !weights.iter().all(|w| w.is_finite())
        {
            return None;
        }
        let (header, data_type, gamma, gain_min, gain_max) =
            if version2 { (80, u32_at(64)?, f32_at(68)?, f32_at(72)?, f32_at(76)?) } else { (64, 3, 1.0, 0.0, 1.0) };
        if !(0.25..=4.0).contains(&gamma) {
            return None;
        }
        let width = match data_type {
            0 => 1,
            1 | 2 => 2,
            3 => 4,
            _ => return None,
        };
        if b.len() != count.checked_mul(width)?.checked_add(header)? {
            return None;
        }
        let data = b.get(header..)?;
        let scaled = |q: f32, max: f32| gain_min + q / max * (gain_max - gain_min);
        let gains: Vec<f32> = match data_type {
            0 => data.iter().map(|&q| scaled(q as f32, 255.0)).collect(),
            1 => data.as_chunks::<2>().0.iter().map(|c| scaled(order.u16(*c) as f32, 65535.0)).collect(),
            2 => data.as_chunks::<2>().0.iter().map(|c| crate::unpack::f16_to_f32(order.u16(*c))).collect(),
            _ => data.as_chunks::<4>().0.iter().map(|c| f32::from_bits(order.u32(*c))).collect(),
        };
        if gains.len() != count || !gains.iter().all(|g| g.is_finite() && *g >= 0.0) {
            return None;
        }
        Some(GainTableMap { points_v, points_h, points_n, spacing_v, spacing_h, origin_v, origin_h, weights, gamma, gains })
    }

    /// The tag's bytes in `order`: version 1 (DNG 1.6, f32 gains) when the gamma is 1, else
    /// version 2 with f32 gains. Returns `(version2, bytes)`.
    pub fn to_bytes(&self, order: ByteOrder) -> (bool, Vec<u8>) {
        let version2 = self.gamma != 1.0;
        let mut out = Vec::with_capacity(80 + 4 * self.gains.len());
        let u32b = |v: u32| match order {
            ByteOrder::Big => v.to_be_bytes(),
            ByteOrder::Little => v.to_le_bytes(),
        };
        let f64b = |v: f64| match order {
            ByteOrder::Big => v.to_be_bytes(),
            ByteOrder::Little => v.to_le_bytes(),
        };
        let dim = |v: usize| u32b(u32::try_from(v).unwrap_or(u32::MAX));
        out.extend_from_slice(&dim(self.points_v));
        out.extend_from_slice(&dim(self.points_h));
        for v in [self.spacing_v, self.spacing_h, self.origin_v, self.origin_h] {
            out.extend_from_slice(&f64b(v));
        }
        out.extend_from_slice(&dim(self.points_n));
        for w in self.weights {
            out.extend_from_slice(&u32b(w.to_bits()));
        }
        if version2 {
            // DataType 3 (f32); GainMin/GainMax are ignored for float data
            for v in [3, self.gamma.to_bits(), 0f32.to_bits(), 1f32.to_bits()] {
                out.extend_from_slice(&u32b(v));
            }
        }
        for g in &self.gains {
            out.extend_from_slice(&u32b(g.to_bits()));
        }
        (version2, out)
    }

    /// The gain for linear RIMM `rgb` at relative active-area position `pos` (`[u, v]`: across,
    /// down; see [`MapPlacement`]).
    #[inline]
    pub fn gain(&self, rgb: [f32; 3], pos: [f32; 2]) -> f32 {
        let w = &self.weights;
        let lo = rgb[0].min(rgb[1]).min(rgb[2]);
        let hi = rgb[0].max(rgb[1]).max(rgb[2]);
        let t = w[0] * rgb[0] + w[1] * rgb[1] + w[2] * rgb[2] + w[3] * lo + w[4] * hi;
        // NaN-safe clamp to 0..=1
        let mut t = if t > 0.0 { t.min(1.0) } else { 0.0 };
        if self.gamma != 1.0 {
            t = t.powf(self.gamma);
        }
        let last = self.points_n.saturating_sub(1);
        let x = (t * self.points_n as f32).min(last as f32);
        let i0 = (x as usize).min(last);
        let (i1, fi) = ((i0 + 1).min(last), x - i0 as f32);
        let row = axis(pos[1], self.origin_v, self.spacing_v, self.points_v);
        let col = axis(pos[0], self.origin_h, self.spacing_h, self.points_h);
        let n = self.points_n;
        let table = |r: usize, c: usize| -> f32 {
            let base = (r * self.points_h + c) * n;
            let a = self.gains.get(base + i0).copied().unwrap_or(1.0);
            let b = self.gains.get(base + i1).copied().unwrap_or(a);
            a + (b - a) * fi
        };
        let top = lerp(table(row.0, col.0), table(row.0, col.1), col.2);
        let bottom = if row.2 > 0.0 { lerp(table(row.1, col.0), table(row.1, col.1), col.2) } else { top };
        lerp(top, bottom, row.2)
    }
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Grid cell along one axis for relative position `p`: (lower index, upper index, fraction),
/// clamped to the grid (edge replication).
#[inline]
fn axis(p: f32, origin: f64, spacing: f64, points: usize) -> (usize, usize, f32) {
    let last = points.saturating_sub(1);
    if last == 0 {
        return (0, 0, 0.0);
    }
    let x = ((f64::from(p) - origin) / spacing) as f32;
    let x = if x > 0.0 { x.min(last as f32) } else { 0.0 };
    let i0 = (x as usize).min(last);
    (i0, (i0 + 1).min(last), x - i0 as f32)
}

/// Where the pixels of a developed image sit on a gain table map. The developed image is the
/// default crop of the active area, possibly binned. Positions are relative to the active area,
/// at pixel centres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapPlacement {
    u0: f32,
    du: f32,
    v0: f32,
    dv: f32,
}

impl MapPlacement {
    /// For a `width × height` image of the default `crop` (relative to `active`). As in
    /// [`crate::RawInfo::developed_size`], a degenerate crop means the whole active area.
    pub fn new(active: Rect, crop: Rect, width: usize, height: usize) -> MapPlacement {
        let (aw, ah) = (active.width.max(1), active.height.max(1));
        let c = crop.clipped(aw, ah);
        let c = if c.width > 1 && c.height > 1 { c } else { Rect::new(0, 0, aw, ah) };
        let sx = c.width as f64 / width.max(1) as f64;
        let sy = c.height as f64 / height.max(1) as f64;
        MapPlacement {
            u0: ((c.x as f64 + 0.5 * sx) / aw as f64) as f32,
            du: (sx / aw as f64) as f32,
            v0: ((c.y as f64 + 0.5 * sy) / ah as f64) as f32,
            dv: (sy / ah as f64) as f32,
        }
    }

    /// Relative active-area position (`[u, v]`) of the centre of pixel (`x`, `y`).
    #[inline]
    pub fn at(&self, x: usize, y: usize) -> [f32; 2] {
        [self.u0 + x as f32 * self.du, self.v0 + y as f32 * self.dv]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tag bytes: the header fields (`v2`: data type, gamma, gain min, gain max), then `data`.
    fn tag(
        order: ByteOrder,
        dims: [u32; 3],
        spacing: [f64; 2],
        origin: [f64; 2],
        weights: [f32; 5],
        v2: Option<(u32, f32, f32, f32)>,
        data: &[u8],
    ) -> Vec<u8> {
        let be = order == ByteOrder::Big;
        let u = |v: u32| if be { v.to_be_bytes() } else { v.to_le_bytes() };
        let d = |v: f64| if be { v.to_be_bytes() } else { v.to_le_bytes() };
        let mut b = Vec::new();
        b.extend_from_slice(&u(dims[0]));
        b.extend_from_slice(&u(dims[1]));
        for v in [spacing[0], spacing[1], origin[0], origin[1]] {
            b.extend_from_slice(&d(v));
        }
        b.extend_from_slice(&u(dims[2]));
        for w in weights {
            b.extend_from_slice(&u(w.to_bits()));
        }
        if let Some((dt, gamma, min, max)) = v2 {
            for v in [dt, gamma.to_bits(), min.to_bits(), max.to_bits()] {
                b.extend_from_slice(&u(v));
            }
        }
        b.extend_from_slice(data);
        b
    }

    fn floats(order: ByteOrder, g: &[f32]) -> Vec<u8> {
        g.iter().flat_map(|v| if order == ByteOrder::Big { v.to_be_bytes() } else { v.to_le_bytes() }).collect()
    }

    const GREEN: [f32; 5] = [0.0, 1.0, 0.0, 0.0, 0.0];

    /// 2 × 2 tables of N = 4 points: table (r, c) = base · [1, 2, 3, 4] with base 1, 2, 3, 4.
    fn grid() -> Vec<f32> {
        (0..4).flat_map(|t| (1..=4).map(move |i| (t + 1) as f32 * i as f32)).collect()
    }

    #[test]
    fn gain_interpolates_tables_and_positions() {
        for order in [ByteOrder::Big, ByteOrder::Little] {
            // grid points at u, v ∈ {0.25, 0.75}
            let b = tag(order, [2, 2, 4], [0.5, 0.5], [0.25, 0.25], GREEN, None, &floats(order, &grid()));
            let m = GainTableMap::parse(&b, order, false).unwrap();
            let g = |green: f32, u: f32, v: f32| m.gain([0.3, green, 0.9], [u, v]);
            // input 0.25 → index 1.0 exactly; at grid point (0, 0) the table is [1, 2, 3, 4]
            assert_eq!(g(0.25, 0.25, 0.25), 2.0);
            // input 0.3 → index 1.2: 2 + 0.2 · (3 − 2)
            assert!((g(0.3, 0.25, 0.25) - 2.2).abs() < 1e-5);
            // input 1 → index 4, clamped to the last entry
            assert_eq!(g(1.0, 0.25, 0.25), 4.0);
            assert_eq!(g(7.0, 0.25, 0.25), 4.0, "input clamps to 1");
            assert_eq!(g(-1.0, 0.25, 0.25), 1.0, "input clamps to 0");
            assert_eq!(g(f32::NAN, 0.25, 0.25), 1.0);
            // other grid points: table (0, 1) has base 2, (1, 0) base 3, (1, 1) base 4
            assert_eq!(g(0.25, 0.75, 0.25), 4.0);
            assert_eq!(g(0.25, 0.25, 0.75), 6.0);
            assert_eq!(g(0.25, 0.75, 0.75), 8.0);
            // bilinear between the four: centre = mean of bases 1..4 = 2.5 → 2.5 · 2
            assert!((g(0.25, 0.5, 0.5) - 5.0).abs() < 1e-5);
            // a quarter of the way across the top row: base 1.25
            assert!((g(0.25, 0.375, 0.25) - 2.5).abs() < 1e-5);
            // outside the grid the edge tables repeat
            assert_eq!(g(0.25, 0.0, 0.0), 2.0);
            assert_eq!(g(0.25, 1.0, 0.0), 4.0);
            assert_eq!(g(0.25, 1.0, 1.0), 8.0);
            assert!((g(0.25, 0.5, -3.0) - 3.0).abs() < 1e-5, "top edge, half-way across");
        }
    }

    #[test]
    fn input_weights_use_min_and_max() {
        let order = ByteOrder::Big;
        // one table, N = 2: gain 1 at input 0 and 3 at input 0.5 (index 1), clamped beyond
        let b = tag(order, [1, 1, 2], [1.0, 1.0], [0.0, 0.0], [0.0, 0.0, 0.0, 0.5, 0.5], None, &floats(order, &[1.0, 3.0]));
        let m = GainTableMap::parse(&b, order, false).unwrap();
        // (min + max) / 2 of (0.1, 0.5, 0.3) = 0.3 → index 0.6 → 1 + 0.6 · 2
        assert!((m.gain([0.1, 0.5, 0.3], [0.5, 0.5]) - 2.2).abs() < 1e-5);
    }

    #[test]
    fn version_2_storage_types_and_gamma() {
        let order = ByteOrder::Little;
        let v2 = |dt: u32, gamma: f32, data: &[u8]| {
            GainTableMap::parse(&tag(order, [1, 1, 4], [1.0, 1.0], [0.0, 0.0], GREEN, Some((dt, gamma, 0.5, 2.5)), data), order, true)
        };
        // u8: 0 → GainMin, 255 → GainMax
        let m = v2(0, 1.0, &[0, 51, 255, 255]).unwrap();
        let want = [0.5, 0.9, 2.5, 2.5];
        assert!(m.gains.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-6), "{:?}", m.gains);
        // u16
        let m = v2(1, 1.0, &[0, 0, 0xff, 0xff, 0, 0, 0xff, 0xff]).unwrap();
        assert_eq!(m.gains, vec![0.5, 2.5, 0.5, 2.5]);
        // f16 ignores GainMin/GainMax: 1.0, 2.0, 0.5, 0
        let m = v2(2, 1.0, &[0x00, 0x3c, 0x00, 0x40, 0x00, 0x38, 0, 0]).unwrap();
        assert_eq!(m.gains, vec![1.0, 2.0, 0.5, 0.0]);
        // f32 with gamma 2: input 0.5 → 0.25 → index 1
        let m = v2(3, 2.0, &floats(order, &[1.0, 2.0, 3.0, 4.0])).unwrap();
        assert_eq!(m.gain([0.0, 0.5, 0.0], [0.5, 0.5]), 2.0);
        // unknown data type, gamma out of range, size for another type: ignored
        assert!(v2(4, 1.0, &[0; 4]).is_none());
        assert!(v2(0, 0.1, &[0; 4]).is_none());
        assert!(v2(0, 5.0, &[0; 4]).is_none());
        assert!(v2(1, 1.0, &[0; 4]).is_none());
        // negative integer range
        let neg = tag(order, [1, 1, 4], [1.0, 1.0], [0.0, 0.0], GREEN, Some((0, 1.0, -1.0, 1.0)), &[0; 4]);
        assert!(GainTableMap::parse(&neg, order, true).is_none());
    }

    #[test]
    fn malformed_tags_are_rejected() {
        let order = ByteOrder::Big;
        let good = tag(order, [2, 2, 4], [0.5, 0.5], [0.25, 0.25], GREEN, None, &floats(order, &grid()));
        assert!(GainTableMap::parse(&good, order, false).is_some());
        // every truncation (and an extra byte)
        for n in 0..good.len() {
            assert!(GainTableMap::parse(&good[..n], order, false).is_none(), "{n} bytes");
        }
        let mut long = good.clone();
        long.push(0);
        assert!(GainTableMap::parse(&long, order, false).is_none());
        // the wrong byte order reads absurd dimensions
        assert!(GainTableMap::parse(&good, ByteOrder::Little, false).is_none());
        // read as version 2 the header is too short for the data
        assert!(GainTableMap::parse(&good, order, true).is_none());
        let bad = |dims: [u32; 3], spacing: [f64; 2], origin: [f64; 2], gains: &[f32]| {
            GainTableMap::parse(&tag(order, dims, spacing, origin, GREEN, None, &floats(order, gains)), order, false)
        };
        let mut g = grid();
        g[5] = f32::NAN;
        assert!(bad([2, 2, 4], [0.5, 0.5], [0.0, 0.0], &g).is_none(), "NaN gain");
        g[5] = -1.0;
        assert!(bad([2, 2, 4], [0.5, 0.5], [0.0, 0.0], &g).is_none(), "negative gain");
        g[5] = f32::INFINITY;
        assert!(bad([2, 2, 4], [0.5, 0.5], [0.0, 0.0], &g).is_none(), "infinite gain");
        assert!(bad([2, 2, 4], [0.0, 0.5], [0.0, 0.0], &grid()).is_none(), "zero spacing");
        assert!(bad([2, 2, 4], [0.5, f64::NAN], [0.0, 0.0], &grid()).is_none(), "NaN spacing");
        assert!(bad([2, 2, 4], [0.5, 0.5], [f64::INFINITY, 0.0], &grid()).is_none(), "infinite origin");
        assert!(bad([0, 2, 4], [0.5, 0.5], [0.0, 0.0], &[]).is_none(), "no rows");
        assert!(bad([1, 1, 0], [0.5, 0.5], [0.0, 0.0], &[]).is_none(), "no points");
        // huge dimensions (whose product overflows or exceeds the cap) are rejected before allocating
        assert!(bad([u32::MAX, u32::MAX, u32::MAX], [0.5, 0.5], [0.0, 0.0], &grid()).is_none());
        assert!(bad([4096, 4096, 1], [0.5, 0.5], [0.0, 0.0], &grid()).is_none());
        assert!(bad([1, 1, u32::MAX], [0.5, 0.5], [0.0, 0.0], &grid()).is_none());
        // a single table needs no spacing
        assert!(bad([1, 1, 2], [0.0, 0.0], [0.0, 0.0], &[1.0, 2.0]).is_some());
        let mut w = GREEN;
        w[3] = f32::NAN;
        assert!(GainTableMap::parse(&tag(order, [1, 1, 1], [1.0, 1.0], [0.0, 0.0], w, None, &floats(order, &[1.0])), order, false).is_none());
    }

    #[test]
    fn writes_what_it_reads() {
        for order in [ByteOrder::Big, ByteOrder::Little] {
            let m =
                GainTableMap::parse(&tag(order, [2, 2, 4], [0.5, 0.5], [0.25, 0.25], GREEN, None, &floats(order, &grid())), order, false).unwrap();
            let (v2, b) = m.to_bytes(order);
            assert!(!v2);
            assert_eq!(GainTableMap::parse(&b, order, false).unwrap(), m);
            let g = GainTableMap { gamma: 2.0, ..m };
            let (v2, b) = g.to_bytes(order);
            assert!(v2);
            assert_eq!(GainTableMap::parse(&b, order, true).unwrap(), g);
        }
    }

    #[test]
    fn placement_is_pixel_centred_in_the_active_area() {
        // 4000 × 3000 active area, full-size image of a 3000 × 2000 crop at (500, 400)
        let p = MapPlacement::new(Rect::new(8, 10, 4000, 3000), Rect::new(500, 400, 3000, 2000), 3000, 2000);
        let close = |a: [f32; 2], b: [f64; 2]| (a[0] as f64 - b[0]).abs() < 1e-6 && (a[1] as f64 - b[1]).abs() < 1e-6;
        assert!(close(p.at(0, 0), [500.5 / 4000.0, 400.5 / 3000.0]));
        assert!(close(p.at(2999, 1999), [3499.5 / 4000.0, 2399.5 / 3000.0]));
        // the same crop binned 4× covers the same area with 4-pixel steps
        let b = MapPlacement::new(Rect::new(8, 10, 4000, 3000), Rect::new(500, 400, 3000, 2000), 750, 500);
        assert!(close(b.at(0, 0), [502.0 / 4000.0, 402.0 / 3000.0]));
        // no default crop: the whole active area
        let f = MapPlacement::new(Rect::new(0, 0, 4000, 3000), Rect::default(), 4000, 3000);
        assert!(close(f.at(3999, 2999), [3999.5 / 4000.0, 2999.5 / 3000.0]));
    }
}
