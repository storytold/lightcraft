//! Compare two renders of the same photo: register them, then report the CIEDE2000 colour difference overall, by
//! tone band and by hue sector, the signed lightness / chroma / hue bias, and optionally a false-colour ΔE00 map.
//!
//! The renders can come from any editor (the app, Lightroom, Capture One, darktable, a camera JPEG): each file is
//! decoded through its own colour space (embedded ICC, else the format's default) to XYZ D50, so the numbers measure
//! what a person would see, not file values.
//!
//! Sources: own design; CIEDE2000 (Sharma, Wu, Dalal 2005); registration in [`crate::dataset`].
use crate::dataset::grid::{Grid, bilinear, fit_long, reduce_rgb};
use crate::dataset::register;
use crate::deltae::{de2000, quantile};
use crate::{Error, Result};
use serde::Serialize;
use std::path::Path;

/// Weakest registration accepted before the comparison is refused (normalised cross-correlation).
const MIN_SCORE: f32 = 0.35;
/// Largest aspect-ratio mismatch between the two renders.
const MAX_ASPECT: f64 = 0.03;
/// Chroma (C*ab) below which a colour counts as neutral.
const NEUTRAL_CHROMA: f64 = 8.0;
/// Border left out of the comparison (fraction of each edge): registration and lens corrections differ there.
const BORDER: f64 = 0.03;

/// Tone bands by the reference's L*.
const TONES: [(&str, f64, f64); 4] = [("shadows", 0.0, 25.0), ("darks", 25.0, 50.0), ("lights", 50.0, 75.0), ("highlights", 75.0, f64::INFINITY)];
/// Hue sectors by the reference's CIELAB hue angle (degrees, start inclusive); names are approximate.
const HUES: [(&str, f64, f64); 8] = [
    ("red", 15.0, 50.0),
    ("orange", 50.0, 80.0),
    ("yellow", 80.0, 110.0),
    ("green", 110.0, 170.0),
    ("cyan", 170.0, 230.0),
    ("blue", 230.0, 290.0),
    ("purple", 290.0, 330.0),
    ("magenta", 330.0, 375.0),
];

#[derive(Clone, Debug)]
pub struct CompareOptions {
    /// Long edge of the comparison grid (pixels); both renders are area-reduced to it (never enlarged).
    pub long_edge: usize,
    /// Register the test render against the reference (scale + shift); off compares pixel for pixel.
    pub align: bool,
}

impl Default for CompareOptions {
    fn default() -> Self {
        CompareOptions { long_edge: 512, align: true }
    }
}

/// ΔE00 statistics of a set of pixels, plus the test render's mean signed bias against the reference.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Stats {
    pub pixels: usize,
    pub mean: f64,
    pub median: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
    /// Mean L* of the test minus the reference (positive: the test is lighter).
    pub lightness_bias: f64,
    /// Mean C*ab of the test minus the reference (positive: the test is more saturated).
    pub chroma_bias: f64,
    /// Chroma-weighted mean hue angle of the test minus the reference, degrees (positive: anticlockwise in a*b*).
    pub hue_bias_deg: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Group {
    pub name: String,
    #[serde(flatten)]
    pub stats: Stats,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Alignment {
    pub scale: f32,
    /// Shift in comparison-grid pixels.
    pub dx: f32,
    pub dy: f32,
    /// Normalised cross-correlation at the optimum (−1..1).
    pub score: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct CompareReport {
    pub width: usize,
    pub height: usize,
    pub alignment: Option<Alignment>,
    pub overall: Stats,
    pub tones: Vec<Group>,
    /// Neutrals (C* below 8) first, then the hue sectors.
    pub hues: Vec<Group>,
    /// Per-pixel ΔE00 on the comparison grid (row-major, `width × height`; NaN where not compared).
    #[serde(skip)]
    pub map: Vec<f32>,
}

/// Decode an image to linear XYZ D50, oriented, area-reduced to fit `long` (or, with `width`, to that width).
fn decode_xyz(name: &str, bytes: &[u8], long: usize, width: Option<usize>) -> Result<Grid<[f32; 3]>> {
    let edge = u32::try_from(long.saturating_mul(4)).unwrap_or(u32::MAX);
    let d = lcc_codecs::decode(bytes, lcc_codecs::DecodeOptions::fit(edge, edge)).map_err(|e| Error(format!("{name}: cannot decode: {e}")))?;
    let m = d.space.to_xyz_d50;
    let img = d.image.into_oriented(lcc_raw::Orientation::from_exif(d.orientation));
    if img.width < 16 || img.height < 16 {
        return Err(Error(format!("{name}: image too small to compare ({}×{})", img.width, img.height)));
    }
    let src = Grid { w: img.width, h: img.height, data: img.data.iter().map(|p| m.apply_f32(*p)).collect() };
    let (w, h) = match width {
        Some(w) => (w.max(1), ((src.h as f64 * w as f64 / src.w as f64).round() as usize).max(1)),
        // never upscale: it adds nothing to compare
        None => fit_long(src.w, src.h, long.min(src.w.max(src.h))),
    };
    Ok(reduce_rgb(&src, w, h, |p| p[1]).0)
}

fn read(path: &Path) -> Result<(String, Vec<u8>)> {
    let name = crate::dataset::file_name(path);
    let bytes = crate::dataset::read_file(path).map_err(|e| Error(format!("{name}: {e}")))?;
    Ok((name, bytes))
}

fn lab(xyz: [f32; 3]) -> [f64; 3] {
    crate::deltae::lab([xyz[0] as f64, xyz[1] as f64, xyz[2] as f64])
}

/// Running sums for one group.
#[derive(Clone, Default)]
struct Acc {
    de: Vec<f64>,
    dl: f64,
    dc: f64,
    dh_w: f64,
    w: f64,
}

impl Acc {
    fn add(&mut self, de: f64, r: [f64; 3], t: [f64; 3]) {
        self.de.push(de);
        self.dl += t[0] - r[0];
        let (cr, ct) = (r[1].hypot(r[2]), t[1].hypot(t[2]));
        self.dc += ct - cr;
        // hue difference only where both have colour, weighted by chroma
        let w = cr.min(ct);
        if w > 1.0 {
            let d = (t[2].atan2(t[1]) - r[2].atan2(r[1])).to_degrees();
            let d = (d + 180.0).rem_euclid(360.0) - 180.0;
            self.dh_w += d * w;
            self.w += w;
        }
    }

    fn stats(&self) -> Stats {
        let n = self.de.len();
        if n == 0 {
            return Stats::default();
        }
        let nf = n as f64;
        Stats {
            pixels: n,
            mean: self.de.iter().sum::<f64>() / nf,
            median: quantile(&self.de, 0.5),
            p95: quantile(&self.de, 0.95),
            p99: quantile(&self.de, 0.99),
            max: self.de.iter().copied().fold(0.0, f64::max),
            lightness_bias: self.dl / nf,
            chroma_bias: self.dc / nf,
            hue_bias_deg: if self.w > 0.0 { self.dh_w / self.w } else { 0.0 },
        }
    }
}

/// Compare `test` against `reference` (both image files: JPEG, TIFF, PNG, …).
pub fn compare(reference: &Path, test: &Path, opts: &CompareOptions) -> Result<CompareReport> {
    let (rn, rb) = read(reference)?;
    let (tn, tb) = read(test)?;
    compare_bytes((&rn, &rb), (&tn, &tb), opts)
}

/// [`compare`] on encoded images in memory, each given as `(name for messages, bytes)`.
pub fn compare_bytes(reference: (&str, &[u8]), test: (&str, &[u8]), opts: &CompareOptions) -> Result<CompareReport> {
    let long = opts.long_edge.clamp(32, 4096);
    let a = decode_xyz(reference.0, reference.1, long, None)?;
    let b = decode_xyz(test.0, test.1, long, Some(a.w))?;
    let (ra, rb) = (a.h as f64 / a.w as f64, b.h as f64 / b.w as f64);
    if (ra / rb - 1.0).abs() > MAX_ASPECT {
        return Err(Error(format!(
            "the renders have different shapes ({:.3} vs {:.3}): one is cropped or rotated; export both uncropped",
            1.0 / ra,
            1.0 / rb
        )));
    }
    let t = if opts.align {
        let la = register::feature(&a.map(|p| p[1]));
        let lb = register::feature(&b.map(|p| p[1]));
        let t = register::register(&la, &lb, 0.04, 0.04);
        if !(t.score.is_finite() && t.score >= MIN_SCORE) {
            return Err(Error(format!(
                "could not line the renders up (correlation {:.2}): different photos, or a large crop? Try --no-align for same-size renders",
                t.score
            )));
        }
        t
    } else {
        register::Transform { scale: 1.0, tx: 0.0, ty: 0.0, score: 1.0 }
    };

    let mut all = Acc::default();
    let mut tones = vec![Acc::default(); TONES.len()];
    let mut neutral = Acc::default();
    let mut hues = vec![Acc::default(); HUES.len()];
    let mut map = vec![f32::NAN; a.w.saturating_mul(a.h)];
    let (bx, by) = ((a.w as f64 * BORDER).ceil() as usize, (a.h as f64 * BORDER).ceil() as usize);
    for y in by..a.h.saturating_sub(by) {
        for x in bx..a.w.saturating_sub(bx) {
            let (u, v) = t.map(x as f32 + 0.5, y as f32 + 0.5, (a.w, a.h), (b.w, b.h), 1.0);
            let Some(pb) = bilinear(&b, u, v) else { continue };
            let pa = a.at(x, y);
            if !pa.iter().chain(&pb).all(|c| c.is_finite()) {
                continue;
            }
            let (r, s) = (lab(pa), lab(pb));
            let de = de2000(r, s);
            if !de.is_finite() {
                continue;
            }
            if let Some(m) = map.get_mut(y * a.w + x) {
                *m = de as f32;
            }
            all.add(de, r, s);
            let tone = TONES.iter().position(|(_, lo, hi)| r[0] >= *lo && r[0] < *hi).or((r[0] < 0.0).then_some(0));
            if let Some(g) = tone.and_then(|i| tones.get_mut(i)) {
                g.add(de, r, s);
            }
            if r[1].hypot(r[2]) < NEUTRAL_CHROMA {
                neutral.add(de, r, s);
            } else {
                let h = r[2].atan2(r[1]).to_degrees().rem_euclid(360.0);
                let h = if h < HUES[0].1 { h + 360.0 } else { h };
                if let Some(g) = HUES.iter().position(|(_, lo, hi)| h >= *lo && h < *hi).and_then(|i| hues.get_mut(i)) {
                    g.add(de, r, s);
                }
            }
        }
    }
    if all.de.is_empty() {
        return Err(Error("no pixels to compare (the renders do not overlap)".into()));
    }
    let group = |name: &str, acc: &Acc| Group { name: name.to_string(), stats: acc.stats() };
    Ok(CompareReport {
        width: a.w,
        height: a.h,
        alignment: opts.align.then_some(Alignment { scale: t.scale, dx: t.tx, dy: t.ty, score: t.score }),
        overall: all.stats(),
        tones: TONES.iter().zip(&tones).map(|((n, ..), acc)| group(n, acc)).collect(),
        hues: std::iter::once(group("neutral", &neutral)).chain(HUES.iter().zip(&hues).map(|((n, ..), acc)| group(n, acc))).collect(),
        map,
    })
}

/// The ΔE00 map as 8-bit sRGB: black (0) → blue (1) → green (2) → yellow (5) → red (10 and above); grey where
/// not compared.
pub fn map_rgb8(report: &CompareReport) -> Vec<u8> {
    const STOPS: [(f32, [f32; 3]); 5] =
        [(0.0, [0.0, 0.0, 0.0]), (1.0, [0.1, 0.2, 0.9]), (2.0, [0.1, 0.8, 0.2]), (5.0, [1.0, 0.9, 0.1]), (10.0, [0.95, 0.1, 0.1])];
    let mut out = Vec::with_capacity(report.map.len().saturating_mul(3));
    for &d in &report.map {
        let c = if !d.is_finite() {
            [0.5, 0.5, 0.5]
        } else {
            let mut c = STOPS[STOPS.len() - 1].1;
            for w in STOPS.windows(2) {
                if let [(d0, c0), (d1, c1)] = w
                    && d < *d1
                {
                    let k = ((d - d0) / (d1 - d0)).clamp(0.0, 1.0);
                    c = [c0[0] + (c1[0] - c0[0]) * k, c0[1] + (c1[1] - c0[1]) * k, c0[2] + (c1[2] - c0[2]) * k];
                    break;
                }
            }
            c
        };
        out.extend(c.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8));
    }
    out
}

/// Plain-text table of a report, for the terminal.
pub fn text(report: &CompareReport) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "compared {}×{} pixels", report.width, report.height);
    if let Some(a) = report.alignment {
        let _ = writeln!(s, "aligned: scale {:.4}, shift {:+.1}/{:+.1} px, correlation {:.2}", a.scale, a.dx, a.dy, a.score);
    }
    let _ = writeln!(
        s,
        "\n{:<12} {:>7} {:>6} {:>6} {:>6} {:>6}   {:>6} {:>6} {:>7}",
        "ΔE00", "pixels", "mean", "p50", "p95", "p99", "ΔL*", "ΔC*", "Δhue°"
    );
    let mut row = |name: &str, st: &Stats| {
        if st.pixels == 0 {
            return;
        }
        let _ = writeln!(
            s,
            "{:<12} {:>7} {:>6.2} {:>6.2} {:>6.2} {:>6.2}   {:>+6.2} {:>+6.2} {:>+7.1}",
            name, st.pixels, st.mean, st.median, st.p95, st.p99, st.lightness_bias, st.chroma_bias, st.hue_bias_deg
        );
    };
    row("overall", &report.overall);
    for g in report.tones.iter().chain(&report.hues) {
        row(&g.name, &g.stats);
    }
    s.push_str("\nΔL* > 0: test lighter · ΔC* > 0: test more saturated · Δhue° > 0: test hue rotated anticlockwise in a*b*\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test's folder in Temp, removed when the test ends.
    fn scratch(name: &str) -> lcc_testkit::TempDir {
        lcc_testkit::TempDir::new(&format!("lcc-colourfit-compare-{name}"))
    }

    /// A textured, colourful sRGB test card (8-bit), `gain` applied in linear light and shifted by `shift` pixels.
    fn card(w: usize, h: usize, gain: f32, shift: (i32, i32)) -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h * 3);
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let (xs, ys) = ((x - shift.0) as f32, (y - shift.1) as f32);
                let hue = xs / w as f32 * std::f32::consts::TAU;
                let v = 0.15 + 0.6 * (ys / h as f32).clamp(0.0, 1.0);
                // non-repeating texture: hashed 3 × 3 pixel blocks
                let (bx, by) = ((x - shift.0).div_euclid(3) as u32, (y - shift.1).div_euclid(3) as u32);
                let hash = bx.wrapping_mul(0x9E37_79B1) ^ by.wrapping_mul(0x85EB_CA77);
                let hash = (hash ^ (hash >> 15)).wrapping_mul(0x2C1B_3C6D);
                let tex = 0.08 * ((hash >> 8) as f32 / (1u32 << 24) as f32 - 0.5);
                let rgb = [hue.cos(), (hue + 2.1).cos(), (hue + 4.2).cos()].map(|c| ((0.5 + 0.35 * c) * v + tex).clamp(0.0, 1.0));
                for c in rgb {
                    let lin = lcc_color::transfer::srgb_to_linear(c) * gain;
                    out.push((lcc_color::transfer::linear_to_srgb(lin.clamp(0.0, 1.0)) * 255.0).round() as u8);
                }
            }
        }
        out
    }

    fn write_png(path: &Path, w: usize, h: usize, rgb: &[u8]) {
        let img = lcc_codecs::EncodeImage::new(w as u32, h as u32, 3, lcc_codecs::Samples::U8(rgb));
        std::fs::write(path, lcc_codecs::encode_png(&img, &lcc_codecs::EncodeMeta::default()).unwrap()).unwrap();
    }

    #[test]
    fn identical_renders_have_no_difference() {
        let d = scratch("same");
        let p = d.join("a.png");
        write_png(&p, 320, 200, &card(320, 200, 1.0, (0, 0)));
        let r = compare(&p, &p, &CompareOptions::default()).unwrap();
        assert!(r.overall.pixels > 1000);
        assert!(r.overall.p99 < 0.05, "{:?}", r.overall);
        assert!(r.hues.iter().filter(|g| g.stats.pixels > 0).count() >= 4, "the card spans many hues");
    }

    #[test]
    fn a_brighter_render_reads_lighter() {
        let d = scratch("bright");
        let (a, b) = (d.join("a.png"), d.join("b.png"));
        write_png(&a, 320, 200, &card(320, 200, 1.0, (0, 0)));
        write_png(&b, 320, 200, &card(320, 200, 1.3, (0, 0)));
        let r = compare(&a, &b, &CompareOptions::default()).unwrap();
        assert!(r.overall.lightness_bias > 1.0, "{:?}", r.overall);
        assert!(r.overall.median > 1.0);
        // the map marks compared pixels, and the border as not compared
        assert!(r.map.iter().any(|v| v.is_finite()) && r.map.iter().any(|v| v.is_nan()));
        assert_eq!(map_rgb8(&r).len(), r.width * r.height * 3);
    }

    #[test]
    fn a_shifted_render_is_registered() {
        let d = scratch("shift");
        let (a, b) = (d.join("a.png"), d.join("b.png"));
        write_png(&a, 320, 200, &card(320, 200, 1.0, (0, 0)));
        write_png(&b, 320, 200, &card(320, 200, 1.0, (4, -3)));
        let aligned = compare(&a, &b, &CompareOptions::default()).unwrap();
        let unaligned = compare(&a, &b, &CompareOptions { align: false, ..Default::default() }).unwrap();
        let t = aligned.alignment.unwrap();
        assert!((t.dx - 4.0).abs() < 1.0 && (t.dy + 3.0).abs() < 1.0, "{t:?}");
        assert!(aligned.overall.p95 < unaligned.overall.p95, "{:?} vs {:?}", aligned.overall, unaligned.overall);
    }

    #[test]
    fn hostile_inputs_are_errors_not_panics() {
        let d = scratch("hostile");
        let good = d.join("good.png");
        write_png(&good, 320, 200, &card(320, 200, 1.0, (0, 0)));
        let garbage = d.join("garbage.jpg");
        std::fs::write(&garbage, b"\xff\xd8\xff\xe0 not really a jpeg").unwrap();
        let empty = d.join("empty.tif");
        std::fs::write(&empty, b"").unwrap();
        let tiny = d.join("tiny.png");
        write_png(&tiny, 4, 4, &[128; 48]);
        let tall = d.join("tall.png");
        write_png(&tall, 200, 320, &card(200, 320, 1.0, (0, 0)));
        let flat = d.join("flat.png");
        write_png(&flat, 320, 200, &vec![0u8; 320 * 200 * 3]);
        for (a, b) in
            [(&good, &d.join("missing.png")), (&good, &garbage), (&empty, &good), (&tiny, &good), (&good, &tall), (&good, &flat), (&flat, &flat)]
        {
            assert!(compare(a, b, &CompareOptions::default()).is_err(), "{} vs {}", a.display(), b.display());
        }
        // extreme sizes are clamped, not trusted
        assert!(compare(&good, &good, &CompareOptions { long_edge: 0, align: true }).is_ok());
        assert!(compare(&good, &good, &CompareOptions { long_edge: usize::MAX, align: false }).is_ok());
    }
}
