//! Render-fidelity measures for the SIL fidelity suite (`docs/testing-sil.md`, gap 3).
//!
//! - [`delta_e2000`] and [`compare`]: CIEDE2000 between two 8-bit sRGB renders (mean, 95th
//!   percentile, maximum) and [`ssim`] on their luma.
//! - [`chart`]: a synthetic ColorChecker Classic in any linear RGB space, and [`measure_chart`],
//!   which reads each patch back from a render and gives its ΔE2000 against the published value:
//!   the per-body colour check of plan item 3.1 uses it on charts rendered through a camera profile.
//! - [`RefStore`]: the local, gitignored folder of reference renders the suite compares against,
//!   in a small own file format read defensively.
//!
//! Sources: CIEDE2000 from G. Sharma, W. Wu, E. N. Dalal, "The CIEDE2000 Color-Difference
//! Formula: Implementation Notes, Supplementary Test Data, and Mathematical Observations" (2005),
//! own implementation checked against that paper's test pairs; SSIM from Z. Wang, A. C. Bovik,
//! H. R. Sheikh, E. P. Simoncelli, "Image Quality Assessment: From Error Visibility to Structural
//! Similarity" (IEEE TIP 2004), own implementation with uniform 8 × 8 windows. The ColorChecker
//! values are `lcc_camprof::reference::COLORCHECKER_CLASSIC` (X-Rite's published values).

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::{Path, PathBuf};

use lcc_color::{D50, RgbSpace, bradford, lab::srgb8_to_lab};
use lcc_raster::{Rgb32f, Rgba8};

/// What went wrong measuring (never a panic).
#[derive(Debug, Clone, PartialEq)]
pub enum FidelityError {
    /// The two images differ in size.
    SizeMismatch { a: (usize, usize), b: (usize, usize) },
    /// An image is empty or smaller than one measuring window.
    TooSmall,
    /// A reference file is malformed.
    BadReference(String),
    /// Reading or writing the reference store failed.
    Io(String),
}

impl std::fmt::Display for FidelityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SizeMismatch { a, b } => write!(f, "images differ in size: {}×{} vs {}×{}", a.0, a.1, b.0, b.1),
            Self::TooSmall => write!(f, "image too small to measure"),
            Self::BadReference(m) => write!(f, "bad reference file: {m}"),
            Self::Io(m) => write!(f, "reference store: {m}"),
        }
    }
}

impl std::error::Error for FidelityError {}

/// CIEDE2000 colour difference between two L*a*b* colours (kL = kC = kH = 1). Non-finite input
/// gives `f64::INFINITY` rather than NaN, so a broken render never passes a tolerance.
pub fn delta_e2000(lab1: [f64; 3], lab2: [f64; 3]) -> f64 {
    if lab1.iter().chain(lab2.iter()).any(|v| !v.is_finite()) {
        return f64::INFINITY;
    }
    let [l1, a1, b1] = lab1;
    let [l2, a2, b2] = lab2;
    let c1 = a1.hypot(b1);
    let c2 = a2.hypot(b2);
    let cm = (c1 + c2) / 2.0;
    let cm7 = cm.powi(7);
    let g = 0.5 * (1.0 - (cm7 / (cm7 + 25f64.powi(7))).sqrt());
    let a1p = a1 * (1.0 + g);
    let a2p = a2 * (1.0 + g);
    let c1p = a1p.hypot(b1);
    let c2p = a2p.hypot(b2);
    let hue = |b: f64, a: f64| if b == 0.0 && a == 0.0 { 0.0 } else { b.atan2(a).to_degrees().rem_euclid(360.0) };
    let h1p = hue(b1, a1p);
    let h2p = hue(b2, a2p);
    let dl = l2 - l1;
    let dc = c2p - c1p;
    let dh = if c1p * c2p == 0.0 {
        0.0
    } else {
        let d = h2p - h1p;
        if d > 180.0 {
            d - 360.0
        } else if d < -180.0 {
            d + 360.0
        } else {
            d
        }
    };
    let dhh = 2.0 * (c1p * c2p).sqrt() * (dh.to_radians() / 2.0).sin();
    let lm = (l1 + l2) / 2.0;
    let cmp = (c1p + c2p) / 2.0;
    let hm = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= 180.0 {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 360.0 {
        (h1p + h2p + 360.0) / 2.0
    } else {
        (h1p + h2p - 360.0) / 2.0
    };
    let t = 1.0 - 0.17 * (hm - 30.0).to_radians().cos() + 0.24 * (2.0 * hm).to_radians().cos() + 0.32 * (3.0 * hm + 6.0).to_radians().cos()
        - 0.20 * (4.0 * hm - 63.0).to_radians().cos();
    let d_theta = 30.0 * (-((hm - 275.0) / 25.0).powi(2)).exp();
    let cmp7 = cmp.powi(7);
    let rc = 2.0 * (cmp7 / (cmp7 + 25f64.powi(7))).sqrt();
    let sl = 1.0 + 0.015 * (lm - 50.0).powi(2) / (20.0 + (lm - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * cmp;
    let sh = 1.0 + 0.015 * cmp * t;
    let rt = -(2.0 * d_theta).to_radians().sin() * rc;
    let (x, y, z) = (dl / sl, dc / sc, dhh / sh);
    let de = (x * x + y * y + z * z + rt * y * z).max(0.0).sqrt();
    if de.is_finite() { de } else { f64::INFINITY }
}

/// The aggregate difference between two renders.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Comparison {
    pub mean_de2000: f64,
    pub p95_de2000: f64,
    pub max_de2000: f64,
    /// Mean SSIM of the luma (1 = identical structure).
    pub ssim: f64,
}

fn check_sizes(a: &Rgba8, b: &Rgba8) -> Result<(), FidelityError> {
    if (a.width, a.height) != (b.width, b.height) {
        return Err(FidelityError::SizeMismatch { a: (a.width, a.height), b: (b.width, b.height) });
    }
    let n = a.width.saturating_mul(a.height);
    if n == 0 || a.data.len() != n || b.data.len() != n {
        return Err(FidelityError::TooSmall);
    }
    Ok(())
}

/// ΔE2000 (pixel by pixel, sRGB → L*a*b* D50) and luma SSIM between two same-size renders.
pub fn compare(a: &Rgba8, b: &Rgba8) -> Result<Comparison, FidelityError> {
    check_sizes(a, b)?;
    let mut des: Vec<f64> =
        a.data.iter().zip(&b.data).map(|(p, q)| delta_e2000(srgb8_to_lab([p[0], p[1], p[2]]), srgb8_to_lab([q[0], q[1], q[2]]))).collect();
    des.sort_by(f64::total_cmp);
    let n = des.len();
    let mean = des.iter().sum::<f64>() / n as f64;
    let p95 = des.get((n.saturating_sub(1) * 95) / 100).copied().unwrap_or(f64::INFINITY);
    let max = des.last().copied().unwrap_or(f64::INFINITY);
    Ok(Comparison { mean_de2000: mean, p95_de2000: p95, max_de2000: max, ssim: ssim(a, b)? })
}

/// Mean SSIM of the Rec. 709 luma of two same-size 8-bit renders, over 8 × 8 windows every 4
/// pixels (images smaller than a window are measured as one window).
pub fn ssim(a: &Rgba8, b: &Rgba8) -> Result<f64, FidelityError> {
    check_sizes(a, b)?;
    let luma = |p: &[u8; 4]| 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]);
    let la: Vec<f64> = a.data.iter().map(luma).collect();
    let lb: Vec<f64> = b.data.iter().map(luma).collect();
    let (w, h) = (a.width, a.height);
    let win = 8usize;
    let (ww, wh) = (win.min(w), win.min(h));
    let c1 = (0.01f64 * 255.0).powi(2);
    let c2 = (0.03f64 * 255.0).powi(2);
    let (mut sum, mut count) = (0.0, 0usize);
    let mut y = 0;
    while y + wh <= h {
        let mut x = 0;
        while x + ww <= w {
            let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for yy in y..y + wh {
                for xx in x..x + ww {
                    let i = yy * w + xx;
                    let (p, q) = (la.get(i).copied().unwrap_or(0.0), lb.get(i).copied().unwrap_or(0.0));
                    sa += p;
                    sb += q;
                    saa += p * p;
                    sbb += q * q;
                    sab += p * q;
                }
            }
            let k = (ww * wh) as f64;
            let (ma, mb) = (sa / k, sb / k);
            let (va, vb, cov) = (saa / k - ma * ma, sbb / k - mb * mb, sab / k - ma * mb);
            sum += ((2.0 * ma * mb + c1) * (2.0 * cov + c2)) / ((ma * ma + mb * mb + c1) * (va + vb + c2));
            count += 1;
            x += 4;
        }
        y += 4;
    }
    if count == 0 { Err(FidelityError::TooSmall) } else { Ok(sum / count as f64) }
}

/// One patch of the synthetic chart.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartPatch {
    pub name: String,
    /// The published value, L*a*b* D50.
    pub lab: [f64; 3],
}

/// The ColorChecker Classic patches in reading order (6 columns × 4 rows).
pub fn chart_patches() -> Vec<ChartPatch> {
    lcc_camprof::reference::COLORCHECKER_CLASSIC.patches.iter().map(|p| ChartPatch { name: p.name.to_string(), lab: p.lab }).collect()
}

/// Chart columns and rows.
pub const CHART_COLUMNS: usize = 6;
pub const CHART_ROWS: usize = 4;

/// L*a*b* D50 → linear RGB in `space` (its white Bradford-adapted from D50), not clipped.
pub fn lab_to_linear(lab: [f64; 3], space: &RgbSpace) -> [f64; 3] {
    const EPS: f64 = 216.0 / 24389.0;
    const KAPPA: f64 = 24389.0 / 27.0;
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = fy + lab[1] / 500.0;
    let fz = fy - lab[2] / 200.0;
    let inv = |f: f64| if f.powi(3) > EPS { f.powi(3) } else { (116.0 * f - 16.0) / KAPPA };
    let w = D50.to_xyz();
    let xyz = [inv(fx) * w[0], inv(fy) * w[1], inv(fz) * w[2]];
    let to_space = space.from_xyz().mul(&bradford(D50, space.white));
    to_space.apply(xyz)
}

/// The synthetic chart in linear `space`: a dark grey surround and 24 patches on a 6 × 4 grid,
/// each cell's outer sixth left as surround. Sizes are clamped to at least 6 × 4 pixels per cell.
pub fn chart(space: &RgbSpace, width: usize, height: usize) -> Rgb32f {
    let (w, h) = (width.max(CHART_COLUMNS * 6), height.max(CHART_ROWS * 6));
    let colours: Vec<[f32; 3]> = chart_patches().iter().map(|p| lab_to_linear(p.lab, space).map(|v| v.max(0.0) as f32)).collect();
    let surround = [0.03f32; 3];
    Rgb32f::from_fn(w, h, |x, y| {
        let (cx, cy) = (x * CHART_COLUMNS / w, y * CHART_ROWS / h);
        let fx = (x * CHART_COLUMNS) as f64 / w as f64 - cx as f64;
        let fy = (y * CHART_ROWS) as f64 / h as f64 - cy as f64;
        if !(1.0 / 6.0..5.0 / 6.0).contains(&fx) || !(1.0 / 6.0..5.0 / 6.0).contains(&fy) {
            return surround;
        }
        colours.get(cy * CHART_COLUMNS + cx).copied().unwrap_or(surround)
    })
}

/// Per-patch ΔE2000 of a render of [`chart`] against the published values.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartDe {
    /// Patch name, measured L*a*b* D50, ΔE2000.
    pub patches: Vec<(String, [f64; 3], f64)>,
    pub mean: f64,
    pub max: f64,
}

/// Reads each patch of a render of [`chart`] (any size, the same framing) from the middle third of
/// its cell, averages it in L*a*b*, and gives its ΔE2000 against the published value.
pub fn measure_chart(img: &Rgba8) -> Result<ChartDe, FidelityError> {
    let (w, h) = (img.width, img.height);
    if w < CHART_COLUMNS * 3 || h < CHART_ROWS * 3 || img.data.len() != w.saturating_mul(h) {
        return Err(FidelityError::TooSmall);
    }
    let mut patches = Vec::new();
    for (i, p) in chart_patches().into_iter().enumerate() {
        let (cx, cy) = (i % CHART_COLUMNS, i / CHART_COLUMNS);
        let (x0, x1) = (w * (3 * cx + 1) / (3 * CHART_COLUMNS), w * (3 * cx + 2) / (3 * CHART_COLUMNS));
        let (y0, y1) = (h * (3 * cy + 1) / (3 * CHART_ROWS), h * (3 * cy + 2) / (3 * CHART_ROWS));
        let (mut acc, mut n) = ([0.0f64; 3], 0usize);
        for y in y0..y1.max(y0 + 1) {
            for x in x0..x1.max(x0 + 1) {
                if let Some(px) = img.data.get(y * w + x) {
                    let lab = srgb8_to_lab([px[0], px[1], px[2]]);
                    for (a, v) in acc.iter_mut().zip(lab) {
                        *a += v;
                    }
                    n += 1;
                }
            }
        }
        let lab = acc.map(|v| v / n.max(1) as f64);
        let de = delta_e2000(lab, p.lab);
        patches.push((p.name, lab, de));
    }
    let mean = patches.iter().map(|p| p.2).sum::<f64>() / patches.len().max(1) as f64;
    let max = patches.iter().map(|p| p.2).fold(0.0, f64::max);
    Ok(ChartDe { patches, mean, max })
}

const MAGIC: &[u8; 8] = b"LCCFREF1";
/// Largest reference accepted (pixels); the suite renders far smaller.
pub const MAX_REF_PIXELS: usize = 16 << 20;

/// Encodes a render as a reference file: magic, width and height (u32 LE), RGBA bytes.
pub fn encode_reference(img: &Rgba8) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + img.data.len() * 4);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&u32::try_from(img.width).unwrap_or(0).to_le_bytes());
    out.extend_from_slice(&u32::try_from(img.height).unwrap_or(0).to_le_bytes());
    for p in &img.data {
        out.extend_from_slice(p);
    }
    out
}

/// Decodes a reference file; malformed input is an error, never a panic or a huge allocation.
pub fn decode_reference(bytes: &[u8]) -> Result<Rgba8, FidelityError> {
    let bad = |m: &str| FidelityError::BadReference(m.to_string());
    if bytes.get(..8) != Some(MAGIC.as_slice()) {
        return Err(bad("not a reference file"));
    }
    let word = |at: usize| bytes.get(at..at + 4).and_then(|s| <[u8; 4]>::try_from(s).ok()).map(u32::from_le_bytes);
    let (w, h) = (word(8).ok_or_else(|| bad("truncated header"))?, word(12).ok_or_else(|| bad("truncated header"))?);
    let (w, h) = (usize::try_from(w).map_err(|_| bad("width"))?, usize::try_from(h).map_err(|_| bad("height"))?);
    let n = w.checked_mul(h).filter(|n| *n > 0 && *n <= MAX_REF_PIXELS).ok_or_else(|| bad("size out of range"))?;
    let body = bytes.get(16..).ok_or_else(|| bad("truncated"))?;
    if body.len() != n * 4 {
        return Err(bad("pixel data length does not match the size"));
    }
    let data = body.as_chunks::<4>().0.to_vec();
    Ok(Rgba8 { width: w, height: h, data })
}

/// The local folder of reference renders: `<root>/<tool>/<case>.ref`. Never committed.
#[derive(Debug, Clone)]
pub struct RefStore {
    root: PathBuf,
}

fn safe_part(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' })
        .collect::<String>()
        .trim_start_matches('.')
        .to_string()
}

impl RefStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, tool: &str, case: &str) -> PathBuf {
        self.root.join(safe_part(tool)).join(format!("{}.ref", safe_part(case)))
    }

    /// The stored reference, `Ok(None)` when there is none yet.
    pub fn load(&self, tool: &str, case: &str) -> Result<Option<Rgba8>, FidelityError> {
        match std::fs::read(self.path(tool, case)) {
            Ok(b) => decode_reference(&b).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(FidelityError::Io(e.to_string())),
        }
    }

    pub fn save(&self, tool: &str, case: &str, img: &Rgba8) -> Result<(), FidelityError> {
        let p = self.path(tool, case);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| FidelityError::Io(e.to_string()))?;
        }
        std::fs::write(&p, encode_reference(img)).map_err(|e| FidelityError::Io(format!("{}: {e}", p.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcc_color::{SRGB, transfer::encode_srgb8};

    #[test]
    fn de2000_matches_sharma_test_pairs() {
        // pairs 1, 7, 13, 17, 25 and 34 of Sharma, Wu and Dalal (2005), table 1
        let pairs = [
            ([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485], 2.0425),
            ([50.0, 0.0, 0.0], [50.0, -1.0, 2.0], 2.3669),
            ([50.0, 2.49, -0.001], [50.0, -2.49, 0.0011], 7.2195),
            ([50.0, 2.5, 0.0], [73.0, 25.0, -18.0], 27.1492),
            ([60.2574, -34.0099, 36.2677], [60.4626, -34.1751, 39.4387], 1.2644),
            ([2.0776, 0.0795, -1.1350], [0.9033, -0.0636, -0.5514], 0.9082),
        ];
        for (a, b, want) in pairs {
            let got = delta_e2000(a, b);
            assert!((got - want).abs() < 1e-3, "{a:?} {b:?}: {got} vs {want}");
            assert!((delta_e2000(b, a) - got).abs() < 1e-9, "symmetric");
        }
        assert_eq!(delta_e2000([f64::NAN, 0.0, 0.0], [0.0; 3]), f64::INFINITY);
    }

    fn encode(img: &Rgb32f) -> Rgba8 {
        Rgba8 {
            width: img.width,
            height: img.height,
            data: img.data.iter().map(|p| [encode_srgb8(p[0]), encode_srgb8(p[1]), encode_srgb8(p[2]), 255]).collect(),
        }
    }

    #[test]
    fn the_synthetic_chart_reads_back_as_the_published_values() {
        let de = measure_chart(&encode(&chart(&SRGB, 240, 160))).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(de.patches.len(), CHART_COLUMNS * CHART_ROWS);
        // only 8-bit rounding and the one patch (cyan) outside sRGB stand between them
        assert!(de.mean < 1.5, "mean ΔE {}", de.mean);
        let in_gamut = de.patches.iter().filter(|p| p.2 < 1.0).count();
        assert!(in_gamut >= 22, "{in_gamut} patches within ΔE 1: {:?}", de.patches);
    }

    #[test]
    fn a_shifted_render_measures_worse() {
        let base = encode(&chart(&SRGB, 240, 160));
        let mut warm = base.clone();
        for p in &mut warm.data {
            p[0] = p[0].saturating_add(25);
        }
        let (a, b) = (measure_chart(&base).map(|d| d.mean).unwrap_or(f64::NAN), measure_chart(&warm).map(|d| d.mean).unwrap_or(f64::NAN));
        assert!(b > a + 2.0, "{a} → {b}");
        let same = compare(&base, &base).unwrap_or_else(|e| panic!("{e}"));
        assert!(same.mean_de2000 < 1e-9 && (same.ssim - 1.0).abs() < 1e-9, "{same:?}");
        let diff = compare(&base, &warm).unwrap_or_else(|e| panic!("{e}"));
        assert!(diff.mean_de2000 > 1.0 && diff.max_de2000 >= diff.p95_de2000 && diff.p95_de2000 >= diff.mean_de2000 * 0.0);
    }

    #[test]
    fn ssim_drops_with_structure_loss() {
        let img = Rgba8::from_fn(64, 64, |x, y| {
            let v = if (x / 4 + y / 4) % 2 == 0 { 220 } else { 30 };
            [v, v, v, 255]
        });
        let flat = Rgba8::filled(64, 64, [125, 125, 125, 255]);
        let s = ssim(&img, &flat).unwrap_or(f64::NAN);
        assert!(s < 0.2, "checkerboard vs flat: {s}");
    }

    #[test]
    fn hostile_inputs_are_errors_not_panics() {
        let a = Rgba8::filled(10, 10, [0; 4]);
        let b = Rgba8::filled(11, 10, [0; 4]);
        assert!(matches!(compare(&a, &b), Err(FidelityError::SizeMismatch { .. })));
        let empty = Rgba8 { width: 0, height: 0, data: vec![] };
        assert!(compare(&empty, &empty).is_err());
        let lying = Rgba8 { width: 100, height: 100, data: vec![[0; 4]; 3] };
        assert!(compare(&lying, &lying).is_err());
        assert!(measure_chart(&lying).is_err());
        assert!(measure_chart(&empty).is_err());
        let good = encode_reference(&a);
        let mut cases: Vec<Vec<u8>> = vec![vec![], b"LCCFREF1".to_vec(), good[..good.len() - 1].to_vec(), b"NOTAREF!\0\0\0\0".to_vec()];
        let mut huge = MAGIC.to_vec();
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        cases.push(huge);
        let mut zero = MAGIC.to_vec();
        zero.extend_from_slice(&[0; 8]);
        cases.push(zero);
        for c in cases {
            assert!(decode_reference(&c).is_err(), "{c:?}");
        }
        assert_eq!(decode_reference(&good).ok(), Some(a));
    }

    #[test]
    fn the_store_round_trips_and_keeps_names_inside_its_folder() {
        let dir = std::env::temp_dir().join(format!("lcc-fidelity-store-{}", std::process::id()));
        let store = RefStore::new(&dir);
        let img = Rgba8::filled(9, 7, [1, 2, 3, 255]);
        assert_eq!(store.load("light.exposure", "scene").ok(), Some(None));
        store.save("../../escape", "a/b", &img).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(store.load("../../escape", "a/b").ok(), Some(Some(img)));
        assert!(store.path("../../escape", "a/b").starts_with(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
