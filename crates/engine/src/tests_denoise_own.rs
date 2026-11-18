//! The upstream project's own AI Denoise model (`known::own_bayer`, trained by OneSlips with `tools/denoise-train`),
//! run for real. Each test skips unless `<PREFIX>_TEST_OWN_DENOISE_MODEL` names its `.onnx` (31 MB, downloaded from the
//! address `known::own_bayer_urls` gives, or with `ai.models` / Settings ▸ AI Denoise):
//!
//! ```text
//! <PREFIX>_TEST_OWN_DENOISE_MODEL=<…>/<upstream>-bayer-0.1.0.onnx \
//!   cargo test --release -p lcc-engine tests_denoise_own -- --nocapture
//! ```
//!
//! - end to end, through a library: a noisy raw developed with AI Denoise is closer to the clean raw than the same
//!   raw with the best manual Noise Reduction (the Rust form of `tools/denoise-train/eval_app.py`);
//! - the graphics card and the processor give the same tile;
//! - a damaged copy of the file is an error, never a panic;
//! - `time_a_24mp_denoise` (ignored: run it with `--ignored`) times a 24 MP mosaic against the built-in denoiser.

use std::path::{Path, PathBuf};

use lcc_denoise::bayer::Layout;
use lcc_denoise::manifest::{DenoiserManifest, Domain, Gain};
use lcc_denoise::run::{Control, Params, TileRunner, denoise_bayer};
use lcc_denoise::runtime::CpuRunner;
use serde_json::json;

use crate::Session;
use crate::tests_xmp::{synthetic_dng_of, temp_dir};

/// The model file the tests run, or `None` (and the test skips).
fn model_path() -> Option<PathBuf> {
    lcc_brand::env("TEST_OWN_DENOISE_MODEL").map(PathBuf::from).filter(|p| p.is_file())
}

macro_rules! need_model {
    () => {
        match model_path() {
            Some(p) => p,
            None => {
                lcc_testkit::skip!(Model, "set <PREFIX>_TEST_OWN_DENOISE_MODEL to the own denoise model's .onnx");
                return;
            }
        }
    };
}

/// The manifest the app gives that model (`known::own_bayer`), without the pins: a damaged copy must fail on its
/// contents, not only on its hash.
fn manifest() -> DenoiserManifest {
    DenoiserManifest {
        id: "own-bayer-test".into(),
        name: "Own denoise model (test)".into(),
        version: "0.1.0".into(),
        licence: Default::default(),
        source: None,
        sha256: None,
        size_bytes: None,
        provenance: "the own model, installed by a test".into(),
        domain: Domain::BayerToRgb,
        tile: 512,
        overlap: 64,
        gain: Gain::None,
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        ((self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f64 {
        (-2.0 * self.next().ln()).sqrt() * (std::f64::consts::TAU * self.next()).cos()
    }
}

/// A dim scene (signal above black, in DN of a 14-bit raw whose white is 16000): smooth shading, hard-edged patches
/// of colour and a fine stripe texture, so noise reduction can't win by blurring everything.
fn scene(w: usize, h: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f64 / w as f64, y as f64 / h as f64);
            let colour = [(x % 2 == 0) && (y % 2 == 0), (x + y) % 2 == 1, (x % 2 == 1) && (y % 2 == 1)];
            let mut s = 120.0 + 700.0 * u * (1.0 - 0.4 * v);
            if ((x / 96) + (y / 96)) % 3 == 0 {
                s += if colour[0] {
                    600.0
                } else if colour[2] {
                    150.0
                } else {
                    300.0
                };
            }
            if (200..320).contains(&y) {
                s += 120.0 * (x as f64 * 0.35).sin();
            }
            out.push(s);
        }
    }
    out
}

const BLACK: f64 = 256.0;

/// The scene as raw samples, with Poisson-Gaussian noise (variance `a · signal + b`) when `noise` is given.
fn raw(scene: &[f64], noise: Option<(f64, f64)>, seed: u64) -> Vec<u16> {
    let mut rng = Rng(seed);
    scene
        .iter()
        .map(|&s| {
            let n = noise.map_or(0.0, |(a, b)| (a * s + b).sqrt() * rng.gauss());
            (BLACK + s + n).round().clamp(0.0, 16000.0) as u16
        })
        .collect()
}

fn psnr(a: &lcc_raster::Rgba8, b: &lcc_raster::Rgba8) -> f64 {
    let (mut err, mut n) = (0f64, 0f64);
    for (p, q) in a.data.iter().zip(&b.data) {
        for c in 0..3 {
            err += (f64::from(p[c]) - f64::from(q[c])).powi(2);
            n += 1.0;
        }
    }
    10.0 * (255f64.powi(2) / (err / n.max(1.0)).max(1e-12)).log10()
}

/// The own model installed into a session whose library holds the clean and the noisy raw.
/// (Fields drop in order: the session, with its library open in `dir`, before the folder goes.)
struct Setup {
    s: Session,
    dir: lcc_testkit::TempDir,
    clean: lcc_catalog::PhotoId,
    noisy: lcc_catalog::PhotoId,
}

const W: usize = 768;
const H: usize = 512;

fn setup(model: &Path) -> Setup {
    let dir = temp_dir("own-denoise");
    let originals = dir.join("originals");
    std::fs::create_dir_all(&originals).unwrap();
    let sc = scene(W, H);
    std::fs::write(originals.join("a-clean.dng"), synthetic_dng_of(W, H, "RGGB", raw(&sc, None, 1), None, Default::default())).unwrap();
    std::fs::write(originals.join("b-noisy.dng"), synthetic_dng_of(W, H, "RGGB", raw(&sc, Some((3.0, 64.0)), 7), None, Default::default())).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(dir.join("library"), false).unwrap();
    let paths: Vec<String> = ["a-clean.dng", "b-noisy.dng"].iter().map(|f| originals.join(f).to_string_lossy().into_owned()).collect();
    s.execute("library.import", &json!({"paths": paths})).unwrap();
    let by_name = |s: &Session, name: &str| s.catalog.photos().find(|p| p.file_name == name).unwrap().id;
    let (clean, noisy) = (by_name(&s, "a-clean.dng"), by_name(&s, "b-noisy.dng"));
    s.set_denoise_models_dir(Some(dir.join("denoise-models")));
    let src = dir.join("model");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::copy(model, src.join("model.onnx")).unwrap();
    std::fs::write(src.join("denoise-model.json"), serde_json::to_vec(&manifest()).unwrap()).unwrap();
    s.execute("denoise.models.install", &json!({"path": src.join("model.onnx").to_string_lossy(), "acknowledged": true})).unwrap();
    Setup { dir, s, clean, noisy }
}

/// The same develop settings for every version: exposure brought up to a normal picture, no sharpening, the noise
/// reduction given.
fn develop(s: &mut Session, id: lcc_catalog::PhotoId, luminance: f64, colour: f64, ai: f64) {
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    for (control, value) in [
        ("light.exposure", 2.5),
        ("detail.sharpenAmount", 0.0),
        ("detail.nrLuminance", luminance),
        ("detail.nrColor", colour),
        ("enhance.denoise", ai),
    ] {
        s.execute("develop.set", &json!({"control": control, "value": value})).unwrap();
    }
}

#[test]
fn the_own_model_beats_manual_noise_reduction_end_to_end() {
    let model = need_model!();
    let mut x = setup(&model);
    let (clean, noisy) = (x.clean, x.noisy);
    develop(&mut x.s, clean, 0.0, 0.0, 0.0);
    let reference = x.s.render_now(clean, W, H).unwrap().image;
    develop(&mut x.s, noisy, 0.0, 0.0, 0.0);
    let none = psnr(&x.s.render_now(noisy, W, H).unwrap().image, &reference);
    let mut manual = f64::MIN;
    for l in [10.0, 25.0, 40.0, 60.0, 80.0] {
        develop(&mut x.s, noisy, l, 25.0, 0.0);
        let got = psnr(&x.s.render_now(noisy, W, H).unwrap().image, &reference);
        eprintln!("manual noise reduction {l}: {got:.2} dB");
        manual = manual.max(got);
    }
    develop(&mut x.s, noisy, 0.0, 0.0, 100.0);
    let started = std::time::Instant::now();
    let mut ready = false;
    for _ in 0..2400 {
        let r = x.s.execute("denoise.pump", &json!({"pace": "full"})).unwrap();
        if r["ready"].as_u64() >= Some(1) {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(ready, "the AI picture was made: {}", x.s.execute("denoise.status", &json!({})).unwrap());
    let ai = psnr(&x.s.render_now(noisy, W, H).unwrap().image, &reference);
    eprintln!("no noise reduction {none:.2} dB, best manual {manual:.2} dB, AI Denoise {ai:.2} dB ({:.1} s)", started.elapsed().as_secs_f64());
    assert!(ai > none + 3.0, "AI Denoise {ai:.2} dB vs none {none:.2} dB");
    assert!(ai > manual, "AI Denoise {ai:.2} dB vs the best manual noise reduction {manual:.2} dB");
    drop(x.s);
    let _ = std::fs::remove_dir_all(&x.dir);
}

#[test]
fn the_own_model_gives_the_same_tile_on_the_card_and_the_processor() {
    let model = need_model!();
    if !lcc_gpu::available() {
        lcc_testkit::skip!(Gpu, "no graphics card to compare with");
        return;
    }
    let cpu = CpuRunner::load(&model, &manifest()).unwrap();
    let tile = 512;
    let input = lcc_denoise::runtime::check_tile(tile);
    let on_cpu = cpu.run(&input).unwrap();
    let card = match lcc_gpu::nn::runner(cpu.net(), tile) {
        Ok(c) => c,
        Err(e) => {
            lcc_testkit::skip!(Gpu, "the card can't run the model: {e}");
            return;
        }
    };
    let on_card = card.run(&input).unwrap();
    assert_eq!(on_cpu.len(), on_card.len());
    let worst = on_cpu.iter().zip(&on_card).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    let mean = on_cpu.iter().zip(&on_card).map(|(a, b)| f64::from((a - b).abs())).sum::<f64>() / on_cpu.len() as f64;
    eprintln!("card vs processor: worst {worst:.2e}, mean {mean:.2e}");
    assert!(worst < 2e-3 && mean < 1e-4, "worst {worst}, mean {mean}");
}

#[test]
fn a_damaged_own_model_is_an_error_not_a_panic() {
    let model = need_model!();
    let bytes = std::fs::read(&model).unwrap();
    let dir = temp_dir("own-denoise-damaged");
    let damaged: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("first 100 bytes", bytes[..100].to_vec()),
        ("first half", bytes[..bytes.len() / 2].to_vec()),
        ("all but the last byte", bytes[..bytes.len() - 1].to_vec()),
        ("its header scrambled", {
            let mut b = bytes.clone();
            b.iter_mut().take(4096).enumerate().for_each(|(i, v)| *v ^= (i as u8).wrapping_mul(37) | 1);
            b
        }),
    ];
    for (what, data) in damaged {
        let path = dir.join("model.onnx");
        std::fs::write(&path, &data).unwrap();
        let loaded = std::panic::catch_unwind(|| CpuRunner::load(&path, &manifest()).map(|_| ()));
        let Ok(loaded) = loaded else { panic!("a model file with {what} panicked") };
        assert!(loaded.is_err(), "a model file with {what} loaded");
    }
    // through the app: the install says no, and the pinned download would not keep it (its SHA-256 differs)
    let mut s = Session::new().with_fs();
    s.set_denoise_models_dir(Some(dir.join("denoise-models")));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("model.onnx"), &bytes[..bytes.len() / 2]).unwrap();
    std::fs::write(src.join("denoise-model.json"), serde_json::to_vec(&manifest()).unwrap()).unwrap();
    let r = s.execute("denoise.models.install", &json!({"path": src.join("model.onnx").to_string_lossy(), "acknowledged": true}));
    assert!(r.is_err(), "a cut-off model installed: {r:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A 24 MP mosaic (6000 × 4000) through the own model and the built-in denoiser on the processor, every core.
#[test]
#[ignore = "takes minutes: run with --ignored --nocapture"]
fn time_a_24mp_denoise() {
    let model = need_model!();
    let (w, h) = (6000, 4000);
    let mut rng = Rng(3);
    let mosaic: Vec<f32> = (0..w * h).map(|i| (0.05 + 0.3 * (i % w) as f32 / w as f32 + 0.01 * rng.gauss() as f32).clamp(0.0, 1.0)).collect();
    let layout = Layout::from_cell([0, 1, 1, 2]).unwrap();
    let parallel = std::thread::available_parallelism().map_or(4, |n| n.get());
    let ctl = Control::default();
    let own = CpuRunner::load(&model, &manifest()).unwrap();
    let started = std::time::Instant::now();
    denoise_bayer(&mosaic, w, h, layout, &own, &Params { tile: 512, overlap: 64, gain: Gain::None, clip: Some(1.0), parallel }, &ctl).unwrap();
    let own_s = started.elapsed().as_secs_f64();
    let builtin = lcc_denoise::builtin::Builtin { noise: None, tuning: Default::default() };
    let (tile, overlap) = (lcc_denoise::builtin::TILE as usize, lcc_denoise::builtin::OVERLAP as usize);
    let started = std::time::Instant::now();
    denoise_bayer(&mosaic, w, h, layout, &builtin, &Params { tile, overlap, gain: Gain::None, clip: Some(1.0), parallel }, &ctl).unwrap();
    let builtin_s = started.elapsed().as_secs_f64();
    eprintln!("24 MP on {parallel} threads: own model {own_s:.1} s, built-in denoiser {builtin_s:.1} s");
    if lcc_gpu::available()
        && let Ok(card) = lcc_gpu::nn::runner(own.net(), 512)
    {
        let started = std::time::Instant::now();
        denoise_bayer(&mosaic, w, h, layout, &card, &Params { tile: 512, overlap: 64, gain: Gain::None, clip: Some(1.0), parallel: 1 }, &ctl)
            .unwrap();
        eprintln!("24 MP, own model on the graphics card: {:.1} s", started.elapsed().as_secs_f64());
    }
}
