//! A camera look's hue/saturation table (the DNG `ProfileHueSatMap` layout) applied to linear Rec.2020 pixels with
//! their luminance restored. The engine fits the table to a camera JPEG and applies it when a raw is decoded; the
//! measured looks ([`crate::looks`]) undo it again at render time to recover the camera's colour.

use lightcraft_color::{D50, D65, Mat3, PROPHOTO, REC2020, bradford, luminance_2020};
use lightcraft_raw::profile::HsvTable;

/// Linear Rec.2020 D65 → linear ProPhoto RGB D50, the space DNG hue/saturation tables work in.
pub fn to_prophoto() -> Mat3 {
    PROPHOTO.from_xyz().mul(&bradford(D65, D50)).mul(&REC2020.to_xyz())
}

/// [`HueSat::apply`] restores the luminance in full only if the table kept at least this share of it. A colour
/// the table darkens to [`NO_LUMINANCE_KEPT`] or less (only near-black noise, whose channels go negative) keeps
/// its colour; in between the two are blended, so noisy shadows show no step.
pub const MIN_LUMINANCE_KEPT: f32 = 0.25;
pub const NO_LUMINANCE_KEPT: f32 = 0.15;

/// How much of the table's colour a pixel gets when the table kept `share` of its luminance: 0 up to
/// [`NO_LUMINANCE_KEPT`], 1 from [`MIN_LUMINANCE_KEPT`], a smoothstep between.
pub fn restore_weight(share: f32) -> f32 {
    if !share.is_finite() {
        return 0.0;
    }
    let t = ((share - NO_LUMINANCE_KEPT) / (MIN_LUMINANCE_KEPT - NO_LUMINANCE_KEPT)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A camera look's fitted hue/saturation table (the engine's `CameraLook::hue_sat`), ready to apply to linear Rec.2020 pixels. It changes
/// hue and saturation only: luminance is restored, the camera tone curve owns it.
pub struct HueSat<'a> {
    table: &'a HsvTable,
    to: [[f32; 3]; 3],
    from: [[f32; 3]; 3],
}

impl<'a> HueSat<'a> {
    /// `table` ready to apply; `None` when the colour space conversion can't be inverted.
    pub fn new(table: &'a HsvTable) -> Option<HueSat<'a>> {
        let to = to_prophoto();
        Some(HueSat { table, to: to.to_f32(), from: to.inverse()?.to_f32() })
    }

    /// The table applied to linear Rec.2020 `rgb`, its luminance restored ([`restore_weight`] near black).
    #[inline]
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let mul = |m: &[[f32; 3]; 3], v: [f32; 3]| -> [f32; 3] { std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2]) };
        let out = mul(&self.from, self.table.apply(mul(&self.to, rgb)));
        let (before, after) = (luminance_2020(rgb), luminance_2020(out));
        // restoring the luminance divides by the table's: near black, where noise makes some channels
        // negative, the table can bring it close to 0, and the gain would turn the pixel white (#820)
        if before <= 0.0 || after <= 0.0 || !out.iter().all(|v| v.is_finite()) {
            return rgb;
        }
        let w = restore_weight(after / before);
        if w >= 1.0 {
            return out.map(|v| v * before / after);
        }
        if w <= 0.0 {
            return rgb;
        }
        std::array::from_fn(|i| rgb[i] + (out[i] * before / after - rgb[i]) * w)
    }

    /// [`Self::apply`] to `N` pixels at once (`rgb[channel][lane]`), bit for bit, through
    /// [`HsvTable::apply_lanes`], with the guards as selects.
    #[inline]
    pub fn apply_lanes<const N: usize>(&self, rgb: [[f32; N]; 3]) -> [[f32; N]; 3] {
        let mul = |m: &[[f32; 3]; 3], v: &[[f32; N]; 3]| {
            let mut out = [[0.0f32; N]; 3];
            for (row, out) in m.iter().zip(&mut out) {
                for l in 0..N {
                    out[l] = row[0] * v[0][l] + row[1] * v[1][l] + row[2] * v[2][l];
                }
            }
            out
        };
        let out = mul(&self.from, &self.table.apply_lanes(mul(&self.to, &rgb)));
        let mut restored = rgb;
        for l in 0..N {
            let (input, out) = ([rgb[0][l], rgb[1][l], rgb[2][l]], [out[0][l], out[1][l], out[2][l]]);
            let (before, after) = (luminance_2020(input), luminance_2020(out));
            let untouched = (before <= 0.0) | (after <= 0.0) | !(out[0].is_finite() & out[1].is_finite() & out[2].is_finite());
            let w = restore_weight(after / before);
            for (c, restored) in restored.iter_mut().enumerate() {
                let full = out[c] * before / after;
                let blend = input[c] + (full - input[c]) * w;
                // (`w >= 1` and `w <= 0` exclude each other)
                restored[l] = if untouched | (w <= 0.0) {
                    input[c]
                } else if w >= 1.0 {
                    full
                } else {
                    blend
                };
            }
        }
        restored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Black and non-finite pixels come out unchanged.
    #[test]
    fn hue_sat_passes_black_and_non_finite_pixels_through() {
        let table = HsvTable { hue_divisions: 4, sat_divisions: 2, val_divisions: 1, data: vec![[10.0, 1.5, 1.0]; 8], srgb_value: false };
        let hue_sat = HueSat::new(&table).unwrap();
        assert_eq!(hue_sat.apply([0.0; 3]), [0.0; 3]);
        let nan = hue_sat.apply([f32::NAN, 0.2, 0.1]);
        assert!(nan[0].is_nan() && nan[1] == 0.2 && nan[2] == 0.1);
    }

    /// Near-black noise with mixed-sign channels is never blown up by the luminance restore (#820).
    #[test]
    fn hue_sat_never_turns_near_black_noise_white() {
        // a saturating table, as fitted to high-ISO Sony files; near-black noise with mixed-sign channels
        let table = HsvTable { hue_divisions: 4, sat_divisions: 2, val_divisions: 1, data: vec![[10.0, 1.5, 1.0]; 8], srgb_value: false };
        let hue_sat = HueSat::new(&table).unwrap();
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f32 / (1u64 << 53) as f32 * 2.0 - 1.0
        };
        let mut worst = 0.0f32;
        for _ in 0..200_000 {
            let rgb = [0.002 + 0.004 * next(), 0.002 + 0.004 * next(), 0.002 + 0.004 * next()];
            if luminance_2020(rgb) <= 0.0 {
                continue;
            }
            let out = hue_sat.apply(rgb);
            let scale = rgb.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            worst = worst.max(out.iter().fold(0.0f32, |m, v| m.max(v.abs())) / scale);
        }
        assert!(worst < 20.0, "a near-black pixel came out {worst}× its largest channel");
    }

    /// `apply_lanes` gives `apply`'s bits for any colour: random, near-black noise (the restore's blend), grey,
    /// negative, tiny, above 1, huge, NaN and infinite components; on a fitted look's shape and a saturating table.
    #[test]
    fn hue_sat_lanes_match_one_by_one_bit_for_bit() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let unit = |r: u64, i: usize| ((r >> (16 * i)) & 0xffff) as f32 / 65535.0;
        let fitted = (0..72 * 5 * 5)
            .map(|_| {
                let r = next();
                [unit(r, 0) * 60.0 - 30.0, 0.5 + unit(r, 1), 1.0]
            })
            .collect();
        let fitted = HsvTable { hue_divisions: 72, sat_divisions: 5, val_divisions: 5, data: fitted, srgb_value: true };
        let saturating = HsvTable { hue_divisions: 4, sat_divisions: 2, val_divisions: 1, data: vec![[10.0, 1.5, 1.0]; 8], srgb_value: false };
        let special = [0.0, -0.0, 1e-30, -1e-6, 0.002, 0.5, 1.0, 3.0, 1e30, f32::INFINITY, f32::NEG_INFINITY, f32::NAN];
        let mut colours = Vec::new();
        for a in special {
            for b in special {
                colours.extend(special.map(|c| [a, b, c]));
            }
        }
        for _ in 0..60_000 {
            let r = next();
            let (offset, scale) = [(0.15, 1.3), (0.15e-3, 1.3e-3), (0.6, 5.0), (-0.002, 0.008)][(r >> 62) as usize];
            colours.push(std::array::from_fn(|i| unit(r, i) * scale - offset));
        }
        for table in [fitted, saturating] {
            let hue_sat = HueSat::new(&table).unwrap();
            for chunk in colours.chunks(16) {
                let rgb: [[f32; 16]; 3] = std::array::from_fn(|c| std::array::from_fn(|l| chunk.get(l).map_or(0.0, |p| p[c])));
                let out = hue_sat.apply_lanes(rgb);
                for (l, p) in chunk.iter().enumerate() {
                    let (got, want) = ([out[0][l], out[1][l], out[2][l]], hue_sat.apply(*p));
                    assert_eq!(got.map(f32::to_bits), want.map(f32::to_bits), "{p:?}: {got:?} vs {want:?}");
                }
            }
        }
    }

    /// The restore weight rises from 0 to 1 between the two thresholds without a step.
    #[test]
    fn hue_sat_restore_weight_blends_smoothly_between_the_thresholds() {
        assert_eq!(restore_weight(0.0), 0.0);
        assert_eq!(restore_weight(NO_LUMINANCE_KEPT), 0.0);
        assert_eq!(restore_weight(MIN_LUMINANCE_KEPT), 1.0);
        assert_eq!(restore_weight(1.0), 1.0);
        assert_eq!(restore_weight(f32::NAN), 0.0);
        // monotonic and without a step: neighbouring shares 0.001 apart differ by little
        let mut last = 0.0f32;
        for i in 0..=400 {
            let w = restore_weight(i as f32 * 0.001);
            assert!(w >= last && w - last < 0.02, "step at {}: {last} → {w}", i as f32 * 0.001);
            last = w;
        }
    }

    /// Colours whose luminance the table keeps get the full restore, exactly as before the blend existed.
    #[test]
    fn hue_sat_keeps_colours_the_table_leaves_bright_exactly_as_before() {
        // the blend only touches colours whose luminance the table cut below MIN_LUMINANCE_KEPT
        let table = HsvTable { hue_divisions: 4, sat_divisions: 2, val_divisions: 1, data: vec![[10.0, 1.5, 1.0]; 8], srgb_value: false };
        let hue_sat = HueSat::new(&table).unwrap();
        let mul = |m: &[[f32; 3]; 3], v: [f32; 3]| -> [f32; 3] { std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2]) };
        for rgb in [[0.4, 0.2, 0.1], [0.05, 0.3, 0.2], [0.18, 0.18, 0.18], [0.6, 0.1, 0.5]] {
            let out = mul(&hue_sat.from, table.apply(mul(&hue_sat.to, rgb)));
            let (before, after) = (luminance_2020(rgb), luminance_2020(out));
            assert!(after >= MIN_LUMINANCE_KEPT * before);
            assert_eq!(hue_sat.apply(rgb), out.map(|v| v * before / after));
        }
    }
}
