//! X-Trans demosaic benchmark: re-mosaic linear RGB scenes in simulated camera space, demosaic, score.
//!
//! Scenes are `.rgbf` files (u32 width, u32 height, then width × height × 3 little-endian f32, linear light,
//! white balanced; NaN marks pixels to skip). For each illuminant the scene is divided by that illuminant's
//! white-balance gains (the camera sees less red and blue than the balanced image shows), mosaiced with the X-Trans
//! pattern, optionally given signal-dependent Gaussian noise, demosaiced by each variant, white balanced back and
//! compared with the clean scene after sRGB encoding:
//!
//! - CPSNR (dB, all three channels, 8 px border excluded): overall fidelity.
//! - chroma RMSE (8-bit levels of R − G and B − G): false colour and fringing, which CPSNR mixes with luminance.
//!
//! Scene groups, scored separately and averaged over illuminants:
//! - `real`: held-out scenes made by `xtrans_truth` from real X-Trans raws (centre 768 px); the others train the
//!   LMMSE filters.
//! - `photo`: the natural images of `<scene dir>` (test only).
//! - `synth`: zone plate, fine lines, colour edges, colour wheel (test only).
//!
//! `cargo run --release -p lightcraft-raw --example xtrans_bench -- <scene dir> <truth dir> [csv out]`
//!
//! Environment: `XB_ONLY=a,b` runs only the named variants; `XB_SCENES=1` prints per-scene scores (clean,
//! daylight); `XB_QUICK=1` skips the neutral illuminant and ISO 800; `XB_AUG=n` trains on `n` of the 8
//! rotations / reflections (default 2).

mod lmmse;
mod proto;

use lightcraft_raw::demosaic::{Options, demosaic_with, mosaic_from_rgb};
use lightcraft_raw::{Cfa, Method, Normalized, Rgb32f};
use rayon::prelude::*;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

const BORDER: usize = 8;
const SYNTHETIC: [&str; 4] = ["zoneplate", "fine_lines", "colour_edges", "colorwheel"];
/// Held-out real scenes (near-duplicate pairs stay on the same side of the split).
const TEST_REAL: [&str; 13] = [
    "raf-fuji-xe1-12bit",
    "raf-fuji-xt20-14bit",
    "raf-fuji-xt5-6122",
    "raf-fuji-xt5-6123",
    "raf-fujifilm-x-e3",
    "raf-fujifilm-x-e4",
    "raf-fujifilm-x-pro2",
    "raf-fujifilm-x-pro3",
    "raf-fujifilm-x-t10",
    "raf-fujifilm-x-t3",
    "raf-fujifilm-x-t30-iii",
    "raf-fujifilm-x100vi",
    "raf-fujifilm-xq1",
];
const CROP: usize = 768;
const GROUPS: [&str; 3] = ["real", "photo", "synth"];

fn load(p: &Path) -> Rgb32f {
    let b = std::fs::read(p).unwrap();
    let w = u32::from_le_bytes(b[0..4].try_into().unwrap()) as usize;
    let h = u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize;
    let f: Vec<f32> = b[8..].as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
    assert_eq!(f.len(), w * h * 3, "{}", p.display());
    Rgb32f { width: w, height: h, data: f.as_chunks::<3>().0.to_vec() }
}

/// The centre `size` × `size` (6-pixel aligned, so the X-Trans phase is kept), or the image if smaller.
fn centre(img: Rgb32f, size: usize) -> Rgb32f {
    if img.width <= size && img.height <= size {
        return img;
    }
    let (cw, ch) = (size.min(img.width), size.min(img.height));
    let (x0, y0) = ((img.width - cw) / 12 * 6, (img.height - ch) / 12 * 6);
    Rgb32f::from_fn(cw, ch, |x, y| img.get(x0 + x, y0 + y))
}

fn srgb(v: f32) -> f64 {
    let v = v.clamp(0.0, 1.0) as f64;
    if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

/// (CPSNR dB, chroma RMSE in 8-bit levels), both on sRGB-encoded values; NaN truth pixels are skipped.
fn score(out: &Rgb32f, truth: &Rgb32f) -> (f64, f64) {
    let (mut se, mut ce, mut n) = (0.0f64, 0.0f64, 0usize);
    for y in BORDER..truth.height - BORDER {
        for x in BORDER..truth.width - BORDER {
            let (p, q) = (out.get(x, y), truth.get(x, y));
            if !q.iter().all(|v| v.is_finite()) {
                continue;
            }
            let p = p.map(srgb);
            let q = q.map(srgb);
            for c in 0..3 {
                se += (p[c] - q[c]).powi(2);
            }
            ce += ((p[0] - p[1]) - (q[0] - q[1])).powi(2) + ((p[2] - p[1]) - (q[2] - q[1])).powi(2);
            n += 1;
        }
    }
    // capped: an exact result (an oracle on an achromatic scene) would make every mean infinite
    ((10.0 * (3.0 * n as f64 / se).log10()).min(60.0), 255.0 * (ce / (2 * n) as f64).sqrt())
}

/// Deterministic Gaussian (Box–Muller over an LCG).
struct Rng(u64);
impl Rng {
    fn uniform(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f32 {
        let (a, b) = (self.uniform(), self.uniform());
        ((-2.0 * a.ln()).sqrt() * (std::f64::consts::TAU * b).cos()) as f32
    }
}

/// What a variant sees: the camera mosaic, the illuminant's gains and (for oracles only) the camera-space truth.
struct Ctx<'a> {
    n: &'a Normalized,
    cfa: &'a Cfa,
    gains: [f32; 3],
    truth_cam: &'a Rgb32f,
    filters: &'a [Trained],
}

impl Ctx<'_> {
    fn lib(&self, method: Method, wb: bool) -> Rgb32f {
        demosaic_with(self.n, method, Options { balance: wb.then_some(self.gains) })
    }
    fn m(&self) -> proto::M {
        proto::M::new(self.n, self.cfa, self.gains)
    }
    /// Red and blue by `rb` from the green of LMMSE set `set`.
    fn with_green(&self, set: &str, rb: impl Fn(&proto::M, &[f32]) -> Rgb32f) -> Rgb32f {
        let m = self.m();
        let g: Vec<f32> = self.lmmse_bal(&m, set).data.iter().map(|p| p[1]).collect();
        proto::unbalance(rb(&m, &g), self.gains)
    }
    /// Red and blue by `rb` from the true green (scores the red/blue stage alone).
    fn oracle_rb(&self, rb: impl Fn(&proto::M, &[f32]) -> Rgb32f) -> Rgb32f {
        let m = self.m();
        proto::unbalance(rb(&m, &proto::green_oracle(self.truth_cam, self.gains)), self.gains)
    }
    /// Balanced LMMSE output of filter set `name` (stage 2 runs on its stage 1's green).
    fn lmmse_bal(&self, m: &proto::M, name: &str) -> Rgb32f {
        lmmse_run(self.filters, m, name)
    }
    /// Balanced green from LMMSE filters.
    fn lmmse_green(&self, name: &str) -> Vec<f32> {
        self.lmmse_bal(&self.m(), name).data.iter().map(|p| p[1]).collect()
    }
    /// Camera RGB with the true red and blue and the balanced green `g` (scores green error alone).
    fn green_only(&self, g: &[f32]) -> Rgb32f {
        let data = self.truth_cam.data.iter().zip(g).map(|(t, &g)| [t[0], g / self.gains[1], t[2]]).collect();
        Rgb32f { width: self.truth_cam.width, height: self.truth_cam.height, data }
    }
    fn lmmse(&self, name: &str) -> Rgb32f {
        proto::unbalance(self.lmmse_bal(&self.m(), name), self.gains)
    }
}

struct Variant {
    name: &'static str,
    /// The LMMSE filter set it needs ("" for none).
    uses: &'static str,
    run: fn(&Ctx, &'static str) -> Rgb32f,
}

const fn v(name: &'static str, run: fn(&Ctx, &'static str) -> Rgb32f) -> Variant {
    Variant { name, uses: "", run }
}

const fn l(name: &'static str, uses: &'static str, run: fn(&Ctx, &'static str) -> Rgb32f) -> Variant {
    Variant { name, uses, run }
}

/// LMMSE output.
fn full(c: &Ctx, set: &str) -> Rgb32f {
    c.lmmse(set)
}

/// LMMSE green, edge-aware colour-difference red and blue.
fn g_cd(c: &Ctx, set: &str) -> Rgb32f {
    let m = c.m();
    let g: Vec<f32> = c.lmmse_bal(&m, set).data.iter().map(|p| p[1]).collect();
    proto::unbalance(proto::rb_cd(&m, &g, 2, true), c.gains)
}

/// Green-only score of LMMSE green.
fn g_only(c: &Ctx, set: &str) -> Rgb32f {
    c.green_only(&c.lmmse_green(set))
}

const VARIANTS: &[Variant] = &[
    v("bilinear", |c, _| c.lib(Method::Bilinear, false)),
    // X-Trans: Ppg is the directional interpolation (unbalanced: the old default), Ahd what ships
    v("directional", |c, _| c.lib(Method::Ppg, false)),
    v("lib", |c, _| c.lib(Method::Ahd, true)),
    // oracle: true green, so only the red/blue stage errs
    v("O:g+cd", |c, _| {
        let m = c.m();
        proto::unbalance(proto::rb_cd(&m, &proto::green_oracle(c.truth_cam, c.gains), 2, true), c.gains)
    }),
    v("O:cd2-r3", |c, _| c.oracle_rb(|m, g| proto::rb_cd2(m, g, 3, 2.0, 1.0, 1e-3, false))),
    v("O:cd2-rel", |c, _| c.oracle_rb(|m, g| proto::rb_cd2(m, g, 2, 2.0, 1.0, 0.01, true))),
    v("O:cd2-p2", |c, _| c.oracle_rb(|m, g| proto::rb_cd2(m, g, 2, 2.0, 2.0, 0.01, true))),
    v("O:cd2-d1", |c, _| c.oracle_rb(|m, g| proto::rb_cd2(m, g, 2, 1.0, 1.0, 0.01, true))),
    v("O:cd2-d3", |c, _| c.oracle_rb(|m, g| proto::rb_cd2(m, g, 3, 3.0, 1.0, 0.01, true))),
    v("O:ri2", |c, _| c.oracle_rb(|m, g| proto::rb_ri(m, g, 2, 1e-4))),
    v("O:ri3", |c, _| c.oracle_rb(|m, g| proto::rb_ri(m, g, 3, 1e-4))),
    v("O:ri3-l3", |c, _| c.oracle_rb(|m, g| proto::rb_ri(m, g, 3, 1e-3))),
    v("O:ri3-l5", |c, _| c.oracle_rb(|m, g| proto::rb_ri(m, g, 3, 1e-5))),
    v("gdir+cd", |c, _| {
        let m = c.m();
        proto::unbalance(proto::rb_cd(&m, &proto::green_dir(&m, 1e-3), 2, true), c.gains)
    }),
    l("lmmse7", "7", full),
    l("lmmse7d", "7d", full),
    l("lmmse7t", "7t", full),
    l("lmmse7tn", "7tn", full),
    l("Ld+cd", "7d", g_cd),
    l("Lt+cd", "7t", g_cd),
    l("Ltn+cd", "7tn", g_cd),
    l("Lt+ri3", "7t", |c, set| {
        let m = c.m();
        let g: Vec<f32> = c.lmmse_bal(&m, set).data.iter().map(|p| p[1]).collect();
        proto::unbalance(proto::rb_ri(&m, &g, 3, 1e-5), c.gains)
    }),
    l("Lt+cd-r3", "7t", |c, set| {
        let m = c.m();
        let g: Vec<f32> = c.lmmse_bal(&m, set).data.iter().map(|p| p[1]).collect();
        proto::unbalance(proto::rb_cd2(&m, &g, 3, 2.0, 1.0, 1e-3, false), c.gains)
    }),
    l("Lt+ri-l4", "7t", |c, set| c.with_green(set, |m, g| proto::rb_ri2(m, g, 3, 1e-4, f32::INFINITY, 2))),
    l("Lt+ri-a2", "7t", |c, set| c.with_green(set, |m, g| proto::rb_ri2(m, g, 3, 1e-5, 2.0, 2))),
    l("Lt+ri-a15", "7t", |c, set| c.with_green(set, |m, g| proto::rb_ri2(m, g, 3, 1e-5, 1.5, 2))),
    l("Lt+ri-rr3", "7t", |c, set| c.with_green(set, |m, g| proto::rb_ri2(m, g, 3, 1e-5, f32::INFINITY, 3))),
    l("Lt+ri-a2rr3", "7t", |c, set| c.with_green(set, |m, g| proto::rb_ri2(m, g, 3, 1e-4, 2.0, 3))),
    v("gdir+ri3", |c, _| {
        let m = c.m();
        proto::unbalance(proto::rb_ri(&m, &proto::green_dir(&m, 1e-3), 3, 1e-5), c.gains)
    }),
    l("L2t", "7t2", full),
    l("L2t-g1", "7t2", |c, set| {
        // stage 2's red and blue with stage 1's green
        let m = c.m();
        let mut out = c.lmmse_bal(&m, set);
        let g1 = c.lmmse_bal(&m, "7t");
        out.data.iter_mut().zip(&g1.data).for_each(|(p, q)| p[1] = q[1]);
        proto::unbalance(out, c.gains)
    }),
    // green-only scores: true red and blue with each method's green
    v("G:gdir", |c, _| c.green_only(&proto::green_dir(&c.m(), 1e-3))),
    l("G:7", "7", g_only),
    l("G:7d", "7d", g_only),
    l("G:7t", "7t", g_only),
    l("G:7tn", "7tn", g_only),
    l("G:7t2", "7t2", g_only),
    l("G:7tc", "7tc", g_only),
    l("G:9t", "9t", g_only),
    l("G:prod", "prod", g_only),
    // what crates/raw ships, prototyped: production green, residual interpolation
    l("proto", "prod", |c, set| c.with_green(set, |m, g| proto::rb_ri2(m, g, 3, 1e-5, f32::INFINITY, 3))),
];

/// A trained filter set: (name, filters, the stage-1 set whose green it reads).
type Trained = (&'static str, lmmse::Filters, Option<&'static str>);

/// LMMSE filter sets: (name, layout, classifier, train with noise, stage-1 set it reads, slots). Stage-1 sets
/// come before the stage-2 sets that read them. `prod` is what `crates/raw` ships (`XB_WRITE`).
type Set = (&'static str, lmmse::Layout, lmmse::Classifier, bool, Option<&'static str>, lmmse::Slots);
const LMMSE_SETS: &[Set] = &[
    ("7", lmmse::Layout { r: 3, q: None }, lmmse::Classifier::None, false, None, lmmse::Slots::Phase),
    ("7d", lmmse::Layout { r: 3, q: None }, lmmse::Classifier::Dir4 { split: 0.7 }, false, None, lmmse::Slots::Phase),
    ("7t", lmmse::Layout { r: 3, q: None }, TENSOR, false, None, lmmse::Slots::Phase),
    ("7tn", lmmse::Layout { r: 3, q: None }, TENSOR, true, None, lmmse::Slots::Phase),
    ("7t2", lmmse::Layout { r: 3, q: Some(2) }, TENSOR, false, Some("7t"), lmmse::Slots::Phase),
    ("7tc", lmmse::Layout { r: 3, q: None }, TENSOR_CHEAP, false, None, lmmse::Slots::Phase),
    ("9t", lmmse::Layout { r: 4, q: None }, TENSOR, false, None, lmmse::Slots::Phase),
    ("prod", lmmse::Layout { r: 3, q: None }, TENSOR, false, None, lmmse::Slots::Positions),
];

const TENSOR: lmmse::Classifier = lmmse::Classifier::Tensor { s: [f32::NAN; 2], k: [f32::NAN; 2], cheap: false };
const TENSOR_CHEAP: lmmse::Classifier = lmmse::Classifier::Tensor { s: [f32::NAN; 2], k: [f32::NAN; 2], cheap: true };

/// Run filter set `name` on `m` (balanced), with its stage 1 first when it has one.
fn lmmse_run(filters: &[Trained], m: &proto::M, name: &str) -> Rgb32f {
    let (_, f, dep) = filters.iter().find(|(n, ..)| *n == name).expect("filters trained");
    match dep {
        None => lmmse::apply(m, None, f),
        Some(d) => {
            let g: Vec<f32> = lmmse_run(filters, m, d).data.iter().map(|p| p[1]).collect();
            lmmse::apply(m, Some(&g), f)
        }
    }
}

/// Noise levels mixed into training with noise (same model as `NOISE`).
const TRAIN_NOISE: [(f32, f32); 3] = [(0.0, 0.0), (1.0e-4, 1.0e-6), (4.0e-4, 6.0e-6)];

/// White-balance gains (camera → balanced, green = 1), roughly those of an X-Trans body.
const ILLUMINANTS: [(&str, [f32; 3]); 3] = [("neutral", [1.0, 1.0, 1.0]), ("daylight", [1.9, 1.0, 1.6]), ("tungsten", [1.15, 1.0, 2.8])];

/// Noise variance = a · signal + b (camera units, white = 1).
const NOISE: [(&str, f32, f32); 3] = [("clean", 0.0, 0.0), ("iso800", 1.0e-4, 1.0e-6), ("iso6400", 1.6e-3, 2.5e-5)];

fn rgbf_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<_> =
        std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "rgbf")).collect();
    v.sort();
    v
}

fn stem(p: &Path) -> String {
    p.file_stem().unwrap().to_string_lossy().to_string()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = Path::new(args.get(1).map(String::as_str).unwrap_or("bench_data"));
    let truth_dir = Path::new(args.get(2).map(String::as_str).unwrap_or("xtrans_truth"));
    let csv_path = args.get(3);
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let only: Option<Vec<String>> = env("XB_ONLY").map(|v| v.split(',').map(str::to_owned).collect());
    let per_scene = env("XB_SCENES").is_some();
    let quick = env("XB_QUICK").is_some();
    let aug: usize = env("XB_AUG").and_then(|v| v.parse().ok()).unwrap_or(2).clamp(1, 8);
    let variants: Vec<&Variant> = VARIANTS.iter().filter(|v| only.as_ref().is_none_or(|o| o.iter().any(|n| n == v.name))).collect();

    let cfa = Cfa::xtrans();
    let real = rgbf_in(truth_dir);
    let (test_real, train): (Vec<PathBuf>, Vec<PathBuf>) = real.into_iter().partition(|p| TEST_REAL.contains(&stem(p).as_str()));
    let filters = train_filters(&train, &cfa, &variants, aug);
    // (name, group, truth)
    let mut scenes: Vec<(String, usize, Rgb32f)> = test_real.iter().map(|p| (stem(p), 0, centre(load(p), CROP))).collect();
    for p in rgbf_in(dir) {
        let name = stem(&p);
        let group = if SYNTHETIC.contains(&name.as_str()) { 2 } else { 1 };
        scenes.push((name, group, load(&p)));
    }

    let mut csv = String::from("scene,illuminant,noise,variant,cpsnr_db,chroma_rmse,ms\n");
    // [group][noise][variant] → (cpsnr, chroma, count)
    let mut tot = vec![[0.0f64; 3]; GROUPS.len() * NOISE.len() * variants.len()];
    // variant → (ms, megapixels)
    let mut time = vec![(0.0f64, 0.0f64); variants.len()];
    for (scene, group, truth) in &scenes {
        let mp = (truth.width * truth.height) as f64 / 1e6;
        for (ii, (iname, gain)) in ILLUMINANTS.iter().enumerate() {
            if quick && ii == 0 {
                continue;
            }
            // NaN (clipped) truth pixels become mid grey in the mosaic and are skipped by the score
            let cam = Rgb32f {
                width: truth.width,
                height: truth.height,
                data: truth
                    .data
                    .iter()
                    .map(|p| if p.iter().all(|v| v.is_finite()) { [p[0] / gain[0], p[1] / gain[1], p[2] / gain[2]] } else { [0.18; 3] })
                    .collect(),
            };
            for (ni, (nname, a, b)) in NOISE.iter().enumerate() {
                if quick && ni == 1 {
                    continue;
                }
                let mut n = mosaic_from_rgb(&cam, &cfa);
                if *a > 0.0 || *b > 0.0 {
                    let mut rng = Rng(0x5eed ^ (ii as u64) << 8 ^ ni as u64);
                    for v in &mut n.data {
                        *v += (a * v.max(0.0) + b).sqrt() * rng.gauss();
                    }
                }
                let ctx = Ctx { n: &n, cfa: &cfa, gains: *gain, truth_cam: &cam, filters: &filters };
                for (vi, v) in variants.iter().enumerate() {
                    let t0 = Instant::now();
                    let out = (v.run)(&ctx, v.uses);
                    let ms = t0.elapsed().as_secs_f64() * 1e3;
                    let shown = Rgb32f {
                        width: out.width,
                        height: out.height,
                        data: out.data.iter().map(|p| [p[0] * gain[0], p[1] * gain[1], p[2] * gain[2]]).collect(),
                    };
                    let (ps, ch) = score(&shown, truth);
                    csv.push_str(&format!("{scene},{iname},{nname},{},{ps:.3},{ch:.3},{ms:.1}\n", v.name));
                    let t = &mut tot[(group * NOISE.len() + ni) * variants.len() + vi];
                    t[0] += ps;
                    t[1] += ch;
                    t[2] += 1.0;
                    time[vi].0 += ms;
                    time[vi].1 += mp;
                    if per_scene && ni == 0 && *iname == "daylight" {
                        eprintln!("{scene:<24} {:<14} {ps:6.2} dB {ch:6.2}", v.name);
                    }
                }
            }
        }
    }
    println!("CPSNR dB ↑ / chroma RMSE ↓ (sRGB 8-bit levels), mean over illuminants; ms per MP (single image, all cores)\n");
    print!("{:<12}", "variant");
    for g in GROUPS {
        for (ni, (nname, _, _)) in NOISE.iter().enumerate() {
            if quick && ni == 1 {
                continue;
            }
            print!(" | {:>13}", format!("{g} {}", nname.trim_start_matches("iso")));
        }
    }
    println!(" | {:>6}", "ms/MP");
    for (vi, v) in variants.iter().enumerate() {
        print!("{:<12}", v.name);
        for g in 0..GROUPS.len() {
            for ni in 0..NOISE.len() {
                if quick && ni == 1 {
                    continue;
                }
                let t = tot[(g * NOISE.len() + ni) * variants.len() + vi];
                print!(" | {:>5.2} / {:>5.2}", t[0] / t[2], t[1] / t[2]);
            }
        }
        println!(" | {:>6.1}", time[vi].0 / time[vi].1);
    }
    if let Some(p) = csv_path {
        std::fs::File::create(p).unwrap().write_all(csv.as_bytes()).unwrap();
    }
}

/// LMMSE filters trained on the balanced mosaics of the training scenes (and `aug` of their 8 rotations /
/// reflections), clean or with a mix of noise levels.
fn train_filters(train: &[PathBuf], cfa: &Cfa, variants: &[&Variant], aug: usize) -> Vec<Trained> {
    let mut out = Vec::new();
    // the sets the variants use, and the stage-1 sets those read
    let used = |set: &str| variants.iter().any(|v| v.uses == set);
    let sets: Vec<_> =
        LMMSE_SETS.iter().filter(|(set, ..)| used(set) || LMMSE_SETS.iter().any(|(s2, .., dep, _)| *dep == Some(*set) && used(s2))).collect();
    if sets.is_empty() {
        return out;
    }
    let truths: Vec<Rgb32f> = train.iter().map(|p| load(p)).collect();
    let mp: f64 = truths.iter().map(|t| (t.width * t.height) as f64 / 1e6).sum();
    for &&(set, layout, classifier, noisy, dep, slots) in &sets {
        let t0 = Instant::now();
        // the training mosaic of job (image, transform)
        let mosaic = |i: usize, k: usize| -> (Rgb32f, proto::M) {
            let t = lmmse::dihedral(&truths[i], k);
            let mut n = mosaic_from_rgb(&t, cfa);
            let (a, b) = if noisy { TRAIN_NOISE[(i + k) % TRAIN_NOISE.len()] } else { (0.0, 0.0) };
            if a > 0.0 || b > 0.0 {
                let mut rng = Rng(0x7a1 ^ (i as u64) << 8 ^ k as u64);
                for v in &mut n.data {
                    *v += (a * v.max(0.0) + b).sqrt() * rng.gauss();
                }
            }
            let m = proto::M::new(&n, cfa, [1.0; 3]);
            (t, m)
        };
        // thresholds from the training data's own feature quantiles
        let classifier = if let lmmse::Classifier::Tensor { .. } = classifier {
            let feats: Vec<[f32; 4]> = (0..truths.len())
                .step_by(3)
                .flat_map(|i| lmmse::tensor_features(&mosaic(i, 0).1, matches!(classifier, lmmse::Classifier::Tensor { cheap: true, .. })))
                .collect();
            let fitted = classifier.fit(&feats);
            eprintln!("{set}: {fitted:?}");
            fitted
        } else {
            classifier
        };
        let jobs: Vec<(usize, usize)> = (0..truths.len()).flat_map(|i| (0..aug).map(move |k| (i, k))).collect();
        let t = jobs
            .par_iter()
            .fold(
                || lmmse::Trainer::new(layout, classifier, slots),
                |mut tr, &(i, k)| {
                    let (t, m) = mosaic(i, k);
                    let g: Option<Vec<f32>> = dep.map(|d| lmmse_run(&out, &m, d).data.iter().map(|p| p[1]).collect());
                    tr.add(&m, g.as_deref(), &t);
                    tr
                },
            )
            .reduce(|| lmmse::Trainer::new(layout, classifier, slots), lmmse::Trainer::merge);
        let solved = t.solve(1e-6, 200);
        if set == "prod"
            && let Ok(path) = std::env::var("XB_WRITE")
        {
            let provenance = format!(
                "XB_AUG={aug} XB_WRITE=… cargo run --release -p lightcraft-raw --example xtrans_bench -- <scenes> <xtrans_truth>; {} training scenes",
                truths.len()
            );
            lmmse::write_rust(&solved, Path::new(&path), &provenance).unwrap();
            eprintln!("wrote {path}");
        }
        out.push((set, solved, dep));
        eprintln!("trained {set} on {} scenes ({mp:.0} MP × {aug}) in {:.1} s", truths.len(), t0.elapsed().as_secs_f64());
    }
    out
}
