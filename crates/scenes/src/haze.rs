//! A hazy valley for measuring Dehaze (issue #1237): ridges that fade with distance into a bright
//! bluish-grey airlight, a sky, a sharp bright/dark edge and a neutral grey patch in the foreground.
//!
//! Every pixel follows the haze model `I = J·t + A·(1 − t)` with a known airlight `A`
//! (`AIRLIGHT`) and a transmission per depth layer, so a dehaze can be judged against the
//! scene it should recover. Not part of the demo library: a test and measurement scene.

use lightcraft_color::transfer::{decode_srgb8, linear_to_srgb};
use lightcraft_color::{REC2020, SRGB};
use lightcraft_raster::{Rgb32f, Rgba8};
use rayon::prelude::*;

use crate::noise::fbm;

/// The airlight every layer blends towards, linear sRGB (a bright bluish grey).
const AIRLIGHT: [f32; 3] = [0.62, 0.68, 0.78];

/// Where the measurement features sit, in normalised image coordinates (`0..1` across and down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HazeLayout {
    /// The bright block spans `bright_x0..edge_x`, the dark block `edge_x..dark_x1`, both
    /// `block_y0..block_y1`: a sharp vertical bright/dark edge at `edge_x`.
    pub bright_x0: f32,
    pub edge_x: f32,
    pub dark_x1: f32,
    pub block_y0: f32,
    pub block_y1: f32,
    /// The neutral grey patch (albedo 0.18, under the foreground's haze): `[x0, y0, x1, y1]`.
    pub grey: [f32; 4],
    /// Transmission of the foreground layer the blocks and the patch sit in.
    pub foreground_t: f32,
}

/// The layout of [`hazy_valley`].
pub const HAZE_LAYOUT: HazeLayout =
    HazeLayout { bright_x0: 0.58, edge_x: 0.71, dark_x1: 0.86, block_y0: 0.74, block_y1: 0.93, grey: [0.12, 0.76, 0.26, 0.92], foreground_t: 0.87 };

/// Ridges from far to near: (crest base `v`, crest amplitude, frequency, transmission, albedo).
const RIDGES: [(f32, f32, f32, f32, [f32; 3]); 4] = [
    (0.34, 0.07, 3.0, 0.22, [0.10, 0.12, 0.09]),
    (0.44, 0.07, 4.0, 0.45, [0.08, 0.11, 0.05]),
    (0.55, 0.06, 5.0, 0.68, [0.12, 0.10, 0.05]),
    (0.66, 0.05, 6.0, 0.87, [0.09, 0.13, 0.04]),
];

const SEED: u32 = 1237;

/// Render the hazy valley at `w × h` (meant for 3:2; any size works), linear Rec.2020.
pub fn hazy_valley(w: usize, h: usize) -> Rgb32f {
    let mut img = Rgb32f::new(w, h);
    if w == 0 || h == 0 {
        return img;
    }
    let to2020 = SRGB.to_space(&REC2020).to_f32();
    img.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, px) in row.iter_mut().enumerate() {
            let u = (x as f32 + 0.5) / w as f32;
            let v = (y as f32 + 0.5) / h as f32;
            let c = shade(u, v);
            let m = &to2020;
            *px = [
                (m[0][0] * c[0] + m[0][1] * c[1] + m[0][2] * c[2]).max(0.0),
                (m[1][0] * c[0] + m[1][1] * c[1] + m[1][2] * c[2]).max(0.0),
                (m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2]).max(0.0),
            ];
        }
    });
    img
}

fn hazed(j: [f32; 3], t: f32) -> [f32; 3] {
    let a = AIRLIGHT;
    [j[0] * t + a[0] * (1.0 - t), j[1] * t + a[1] * (1.0 - t), j[2] * t + a[2] * (1.0 - t)]
}

fn shade(u: f32, v: f32) -> [f32; 3] {
    let l = HAZE_LAYOUT;
    // The measurement features sit in the nearest layer.
    let in_blocks = (l.block_y0..l.block_y1).contains(&v);
    if in_blocks && (l.bright_x0..l.edge_x).contains(&u) {
        return hazed([0.85, 0.85, 0.82], l.foreground_t);
    }
    if in_blocks && (l.edge_x..l.dark_x1).contains(&u) {
        return hazed([0.015, 0.015, 0.02], l.foreground_t);
    }
    let [gx0, gy0, gx1, gy1] = l.grey;
    if (gx0..gx1).contains(&u) && (gy0..gy1).contains(&v) {
        return hazed([0.18, 0.18, 0.18], l.foreground_t);
    }
    // Nearest ridge whose crest is above this point.
    for (k, &(base, amp, freq, t, albedo)) in RIDGES.iter().enumerate().rev() {
        let seed = SEED.wrapping_add(k as u32 * 17);
        let crest = base + amp * (fbm(u * freq, 0.5, seed, 4) * 0.5 + 0.5);
        if v >= crest {
            // Contrasty texture: lit faces and dark gullies.
            let tex = fbm(u * 24.0, v * 24.0, seed ^ 0x7e, 4) * 0.5 + 0.5;
            let light = 0.25 + 2.2 * tex * tex;
            return hazed([albedo[0] * light, albedo[1] * light, albedo[2] * light], t);
        }
    }
    // Sky: the airlight at the horizon, bluer and a little darker towards the zenith.
    let elev = (1.0 - v / RIDGES[0].0).clamp(0.0, 1.0);
    let zenith = [0.30, 0.45, 0.80];
    let a = AIRLIGHT;
    let s = elev.powf(0.8);
    let wisp = 1.0 + 0.04 * fbm(u * 3.0, v * 9.0, SEED ^ 0x5c, 3);
    [(a[0] + (zenith[0] - a[0]) * s) * wisp, (a[1] + (zenith[1] - a[1]) * s) * wisp, (a[2] + (zenith[2] - a[2]) * s) * wisp]
}

// ---- measuring a render of the valley (shared by the Dehaze tests and `examples/dehaze_report.rs`)

/// A pixel is clipped to black when every channel is at most this (8-bit, display encoded).
pub const CLIP_BLACK: u8 = 2;
/// A halo pixel is darker than the interior of its block by more than this (8-bit luma).
pub const HALO_STEP: f32 = 10.0;

/// Linear sRGB of an 8-bit sRGB pixel.
pub fn linear8(p: [u8; 4]) -> [f32; 3] {
    [decode_srgb8(p[0]), decode_srgb8(p[1]), decode_srgb8(p[2])]
}

/// Relative luminance (Rec.709 weights) of a linear sRGB colour.
pub fn luminance709(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Relative luminance (linear) of an 8-bit sRGB pixel.
pub fn luminance8(p: [u8; 4]) -> f32 {
    luminance709(linear8(p))
}

/// Display-encoded luminance of an 8-bit sRGB pixel, 0..255.
pub fn luma8(p: [u8; 4]) -> f32 {
    linear_to_srgb(luminance8(p).clamp(0.0, 1.0)) * 255.0
}

/// The value `pct` % of the way up the sorted `v` (index `len·pct/100`, clamped; 0 when empty).
pub fn percentile(mut v: Vec<f32>, pct: usize) -> f32 {
    v.sort_by(f32::total_cmp);
    let i = (v.len().saturating_mul(pct) / 100).min(v.len().saturating_sub(1));
    v.get(i).copied().unwrap_or(0.0)
}

/// Luminance (linear) of every pixel of `img`.
pub fn lumas(img: &Rgba8) -> Vec<f32> {
    img.data.iter().map(|&p| luminance8(p)).collect()
}

/// Median linear luminance of `img`.
pub fn median_luma(img: &Rgba8) -> f32 {
    percentile(lumas(img), 50)
}

/// Share (%) of the pixels of `img` clipped to black ([`CLIP_BLACK`]).
pub fn black_pct(img: &Rgba8) -> f32 {
    let n = img.data.iter().filter(|p| p[0] <= CLIP_BLACK && p[1] <= CLIP_BLACK && p[2] <= CLIP_BLACK).count();
    100.0 * n as f32 / img.data.len().max(1) as f32
}

/// Width in pixels (mean over the middle rows of the blocks) of the band next to the valley's
/// bright/dark edge ([`HAZE_LAYOUT`]) that is darker than the interior of its block by more than
/// [`HALO_STEP`]: on the dark side and on the bright side.
pub fn halo_widths(img: &Rgba8) -> (f32, f32) {
    let l = HAZE_LAYOUT;
    let (w, h) = (img.width as f32, img.height as f32);
    let px = |u: f32| ((u * w) as usize).min(img.width.saturating_sub(1));
    let (edge, bright0, dark1) = (px(l.edge_x), px(l.bright_x0), px(l.dark_x1));
    let span = l.block_y1 - l.block_y0;
    let (y0, y1) = (((l.block_y0 + span * 0.25) * h) as usize, ((l.block_y1 - span * 0.25) * h) as usize);
    let (mut dark_sum, mut bright_sum, mut rows) = (0.0f32, 0.0f32, 0.0f32);
    for y in y0..y1.min(img.height) {
        let luma = |x: usize| luma8(img.get(x, y));
        // Interiors: the outer half of each block, away from the edge.
        let dark_ref = percentile((dark1 - (dark1 - edge) / 2..dark1.saturating_sub(2)).map(luma).collect(), 50);
        let bright_ref = percentile((bright0 + 2..bright0 + (edge - bright0) / 2).map(luma).collect(), 50);
        dark_sum += (edge..dark1).take_while(|&x| luma(x) < dark_ref - HALO_STEP).count() as f32;
        bright_sum += (bright0..edge).rev().take_while(|&x| luma(x) < bright_ref - HALO_STEP).count() as f32;
        rows += 1.0;
    }
    (dark_sum / rows.max(1.0), bright_sum / rows.max(1.0))
}

/// CIE L*C*h° (D65) of the mean colour of the inner part of the valley's grey patch.
pub fn grey_lch(img: &Rgba8) -> [f32; 3] {
    let [x0, y0, x1, y1] = HAZE_LAYOUT.grey;
    let (w, h) = (img.width as f32, img.height as f32);
    let (mx, my) = ((x1 - x0) * 0.2, (y1 - y0) * 0.2);
    let (xa, xb) = (((x0 + mx) * w) as usize, ((x1 - mx) * w) as usize);
    let (ya, yb) = (((y0 + my) * h) as usize, ((y1 - my) * h) as usize);
    let (mut sum, mut n) = ([0.0f32; 3], 0.0f32);
    for y in ya..yb.min(img.height) {
        for x in xa..xb.min(img.width) {
            let c = linear8(img.get(x, y));
            sum = [sum[0] + c[0], sum[1] + c[1], sum[2] + c[2]];
            n += 1.0;
        }
    }
    let c = sum.map(|v| v / n.max(1.0));
    let [l, a, b] = lab(c);
    [l, a.hypot(b), b.atan2(a).to_degrees().rem_euclid(360.0)]
}

/// CIE L*a*b* (D65) of a linear sRGB colour.
fn lab(c: [f32; 3]) -> [f32; 3] {
    let x = 0.4124 * c[0] + 0.3576 * c[1] + 0.1805 * c[2];
    let y = luminance709(c);
    let z = 0.0193 * c[0] + 0.1192 * c[1] + 0.9505 * c[2];
    let f = |t: f32| if t > 0.008_856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let (fx, fy, fz) = (f(x / 0.950_47), f(y), f(z / 1.088_83));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Signed difference `a − b` of two hues (degrees), in −180..180.
pub fn hue_diff(a: f32, b: f32) -> f32 {
    (a - b + 180.0).rem_euclid(360.0) - 180.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(img: &Rgb32f, u: f32, v: f32) -> [f32; 3] {
        img.get(((u * img.width as f32) as usize).min(img.width - 1), ((v * img.height as f32) as usize).min(img.height - 1))
    }

    #[test]
    fn the_valley_is_deterministic_finite_and_hazy() {
        let a = hazy_valley(150, 100);
        assert_eq!(a, hazy_valley(150, 100));
        assert!(a.data.iter().all(|p| p.iter().all(|v| v.is_finite() && *v >= 0.0)));
        assert!(hazy_valley(0, 4).data.is_empty());
        // Far ridges are brighter (hazier) than near ones on average.
        let row_mean = |v: f32| (0..150).map(|x| at(&a, x as f32 / 150.0, v)[1]).sum::<f32>() / 150.0;
        assert!(row_mean(0.43) > row_mean(0.70), "{} vs {}", row_mean(0.43), row_mean(0.70));
    }

    #[test]
    fn the_measurements_are_sane_and_never_panic() {
        assert_eq!(percentile(vec![], 50), 0.0);
        assert_eq!(percentile(vec![3.0, 1.0, 2.0, 4.0], 50), 3.0);
        assert_eq!(percentile(vec![3.0, 1.0, 2.0], 100), 3.0);
        let empty = Rgba8::new(0, 0);
        assert_eq!((median_luma(&empty), black_pct(&empty), halo_widths(&empty)), (0.0, 0.0, (0.0, 0.0)));
        let black = Rgba8::from_fn(30, 20, |_, _| [0, 0, 0, 255]);
        assert_eq!(black_pct(&black), 100.0);
        assert!((hue_diff(350.0, 10.0) + 20.0).abs() < 1e-4);
    }

    #[test]
    fn the_features_are_where_the_layout_says() {
        let img = hazy_valley(600, 400);
        let l = HAZE_LAYOUT;
        let mid = (l.block_y0 + l.block_y1) / 2.0;
        let bright = at(&img, l.edge_x - 0.02, mid);
        let dark = at(&img, l.edge_x + 0.02, mid);
        assert!(bright[1] > 0.6 && dark[1] < 0.15, "{bright:?} {dark:?}");
        let [x0, y0, x1, y1] = l.grey;
        let g = at(&img, (x0 + x1) / 2.0, (y0 + y1) / 2.0);
        assert!(g[1] > 0.2 && g[1] < 0.3, "{g:?}");
    }
}
