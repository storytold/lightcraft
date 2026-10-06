//! Running an installed face recognition model on the CPU with `tract`, behind the `tract` feature. tract is Rust,
//! but its build assembles hand-written speed kernels (and on Linux aarch64 may compile optional C ones); a build
//! without the feature has none of that.
//!
//! A model is a stranger's file, so loading and running it are guarded: tract's errors become ours, a panic
//! inside tract (it is a large library reading hostile graphs) is caught and reported as an error, the
//! embedding must have the size the manifest promised, every number must be finite, and a model that
//! produces nothing useful fails the [self-test](Embedder::self_test) before it is ever used on a photo.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use tract_onnx::prelude::*;

use crate::manifest::{Colour, ModelManifest, OutputSpec};

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("the model could not be loaded: {0}")]
    Load(String),
    #[error("the model failed while running: {0}")]
    Run(String),
    #[error("{0}")]
    Input(String),
}

/// What `self_test` found.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelfTest {
    pub ok: bool,
    /// Milliseconds to load and optimise the model.
    pub load_ms: f64,
    /// Median milliseconds to embed one face.
    pub embed_ms: f64,
    pub dimension: usize,
    /// Each check: what was checked and whether it held.
    pub checks: Vec<(String, bool)>,
}

type Plan = TypedRunnableModel;

/// A loaded recognition model.
#[derive(Clone)]
pub struct Embedder {
    plan: Arc<Plan>,
    manifest: ModelManifest,
    width: usize,
    height: usize,
    dim: usize,
    load_ms: f64,
}

fn guarded<T>(what: &str, f: impl FnOnce() -> Result<T, RuntimeError>) -> Result<T, RuntimeError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => Err(RuntimeError::Run(format!("the runtime gave up on this model while {what}"))),
    }
}

impl Embedder {
    /// Load the `.onnx` at `path` as described by `manifest` (an embedder).
    pub fn load(path: &Path, manifest: &ModelManifest) -> Result<Embedder, RuntimeError> {
        let OutputSpec::Embedding { dim } = manifest.output else {
            return Err(RuntimeError::Load("this is not a recognition model".into()));
        };
        let (w, h) = (manifest.input.width as usize, manifest.input.height as usize);
        let started = Instant::now();
        let plan = guarded("loading it", || {
            tract_onnx::onnx()
                .model_for_path(path)
                .and_then(|m| m.with_input_fact(0, f32::fact([1, 3, h, w]).into()))
                .and_then(|m| m.into_optimized())
                .and_then(|m| m.into_runnable())
                .map_err(|e| RuntimeError::Load(format!("{e:#}")))
        })?;
        Ok(Embedder { plan, manifest: manifest.clone(), width: w, height: h, dim: dim as usize, load_ms: started.elapsed().as_secs_f64() * 1000.0 })
    }

    /// The size of the aligned face picture the model wants (width, height).
    pub fn input_size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    pub fn manifest(&self) -> &ModelManifest {
        &self.manifest
    }

    /// The face as a unit-length vector. `rgb` is an aligned face at exactly [`Self::input_size`], 8-bit RGB.
    pub fn embed(&self, rgb: &[u8]) -> Result<Vec<f32>, RuntimeError> {
        if rgb.len() != self.width * self.height * 3 {
            return Err(RuntimeError::Input("the face picture is not the size the model wants".into()));
        }
        let (mean, std, bgr) = (self.manifest.input.mean, self.manifest.input.std, self.manifest.input.colour == Colour::Bgr);
        let plane = self.width * self.height;
        let mut data = vec![0.0f32; 3 * plane];
        for (i, px) in rgb.as_chunks::<3>().0.iter().enumerate() {
            for c in 0..3 {
                let src = if bgr { 2 - c } else { c };
                if let (Some(v), Some(m), Some(s), Some(o)) = (px.get(src), mean.get(c), std.get(c), data.get_mut(c * plane + i)) {
                    *o = (f32::from(*v) - m) / s;
                }
            }
        }
        let out = guarded("running it", || {
            let input: Tensor =
                tract_ndarray::Array4::from_shape_vec((1, 3, self.height, self.width), data).map_err(|e| RuntimeError::Input(e.to_string()))?.into();
            let result = self.plan.run(tvec!(input.into())).map_err(|e| RuntimeError::Run(format!("{e:#}")))?;
            let first = result.first().ok_or_else(|| RuntimeError::Run("the model gave no output".into()))?;
            let view = first.to_plain_array_view::<f32>().map_err(|e| RuntimeError::Run(format!("{e:#}")))?;
            Ok(view.iter().copied().collect::<Vec<f32>>())
        })?;
        if out.len() != self.dim {
            return Err(RuntimeError::Run(format!("the model gave {} numbers per face, not the {} its description says", out.len(), self.dim)));
        }
        if out.iter().any(|v| !v.is_finite()) {
            return Err(RuntimeError::Run("the model gave numbers that are not finite".into()));
        }
        let norm = out.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm < 1e-9 {
            return Err(RuntimeError::Run("the model gave an all-zero face".into()));
        }
        Ok(out.into_iter().map(|v| v / norm).collect())
    }

    /// Check the model does something sensible before it is ever used on a photo: the right number of finite
    /// numbers, the same picture twice gives the same face, and two different pictures give different faces.
    pub fn self_test(&self) -> SelfTest {
        let mut checks: Vec<(String, bool)> = Vec::new();
        let n = self.width * self.height * 3;
        // two deterministic, different test pictures: noise, and a diagonal gradient with a colour cast
        let mut seed = 0x2545_f491u32;
        let noise: Vec<u8> = (0..n)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed >> 8) as u8
            })
            .collect();
        let gradient: Vec<u8> = (0..n)
            .map(|i| (((i / 3) % self.width + (i / 3) / self.width) * 255 / (self.width + self.height) + (i % 3) * 40).min(255) as u8)
            .collect();
        let mut times = Vec::new();
        let mut run = |img: &[u8]| {
            let t = Instant::now();
            let r = self.embed(img);
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            r
        };
        let (a, a2, b) = (run(&noise), run(&noise), run(&gradient));
        let dot = |x: &[f32], y: &[f32]| x.iter().zip(y).map(|(p, q)| p * q).sum::<f32>();
        match (&a, &a2, &b) {
            (Ok(a), Ok(a2), Ok(b)) => {
                checks.push((format!("gives {} finite numbers per face", a.len()), true));
                checks.push(("gives the same face for the same picture".into(), dot(a, a2) > 0.9999));
                checks.push(("gives different faces for different pictures".into(), dot(a, b) < 0.9995));
            }
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => checks.push((format!("runs: {e}"), false)),
        }
        times.sort_by(|x, y| x.total_cmp(y));
        SelfTest {
            ok: checks.iter().all(|(_, ok)| *ok),
            load_ms: self.load_ms,
            embed_ms: times.get(times.len() / 2).copied().unwrap_or(0.0),
            dimension: self.dim,
            checks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{InputSpec, Licence, OutputSpec, Role, Thresholds};

    fn manifest(dim: u32) -> ModelManifest {
        ModelManifest {
            id: "tiny".into(),
            name: "Tiny".into(),
            version: "1".into(),
            role: Role::Embedder,
            licence: Licence::default(),
            source: None,
            sha256: None,
            size_bytes: None,
            provenance: String::new(),
            input: InputSpec::default(),
            output: OutputSpec::Embedding { dim },
            thresholds: Thresholds::default(),
        }
    }

    fn temp_model(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("lc-faces-runtime-{name}-{}.onnx", std::process::id()));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn a_working_model_loads_embeds_and_passes_the_self_test() {
        let p = temp_model("ok", &crate::synthetic::tiny_embedder_model(16));
        let e = Embedder::load(&p, &manifest(16)).unwrap();
        assert_eq!(e.input_size(), (112, 112));
        let v = e.embed(&vec![100u8; 112 * 112 * 3]).unwrap();
        assert_eq!(v.len(), 16);
        assert!((v.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-4, "unit length");
        let t = e.self_test();
        assert!(t.ok, "{t:?}");
        assert_eq!(t.dimension, 16);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn wrong_sizes_wrong_dimensions_and_broken_files_are_errors() {
        let p = temp_model("dim", &crate::synthetic::tiny_embedder_model(16));
        let e = Embedder::load(&p, &manifest(16)).unwrap();
        assert!(e.embed(&[0u8; 10]).is_err(), "wrong picture size");
        // the manifest promises 32 numbers, the model gives 16
        let wrong = Embedder::load(&p, &manifest(32)).unwrap();
        assert!(wrong.embed(&vec![9u8; 112 * 112 * 3]).is_err());
        assert!(!wrong.self_test().ok);
        // not a model, an empty file, a graph with no layers, a detector-shaped manifest
        for (name, bytes) in [("junk", b"nonsense".to_vec()), ("empty", Vec::new()), ("nolayers", crate::synthetic::embedder_model(16))] {
            let q = temp_model(name, &bytes);
            assert!(Embedder::load(&q, &manifest(16)).is_err(), "{name}");
            let _ = std::fs::remove_file(q);
        }
        let mut detector = manifest(16);
        detector.output = OutputSpec::Detector { decoder: "yunet-v2".into() };
        assert!(Embedder::load(&p, &detector).is_err());
        assert!(Embedder::load(&std::env::temp_dir().join("does-not-exist.onnx"), &manifest(16)).is_err());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn truncated_and_corrupted_models_never_panic() {
        let good = crate::synthetic::tiny_embedder_model(16);
        for n in (0..good.len()).step_by(7) {
            let q = temp_model("trunc", &good[..n]);
            let _ = Embedder::load(&q, &manifest(16));
            let _ = std::fs::remove_file(q);
        }
        for at in (0..good.len()).step_by(11) {
            let mut bad = good.clone();
            if let Some(b) = bad.get_mut(at) {
                *b = b.wrapping_add(0x55);
            }
            let q = temp_model("flip", &bad);
            if let Ok(e) = Embedder::load(&q, &manifest(16)) {
                let _ = e.embed(&vec![7u8; 112 * 112 * 3]);
            }
            let _ = std::fs::remove_file(q);
        }
    }

    /// Opt-in: `LC_FACE_MODELS=<folder> cargo test -p lightcraft-faces --features tract -- --ignored --nocapture`
    /// self-tests every known recognition model found there.
    #[test]
    #[ignore = "needs real model files: set LC_FACE_MODELS"]
    fn real_models_pass_the_self_test() {
        let Some(dir) = std::env::var_os("LC_FACE_MODELS") else { return };
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let Ok(sha) = crate::hash::sha256_file(&path) else { continue };
            let Some(m) = crate::known::lookup(&sha).filter(|m| m.role == Role::Embedder) else { continue };
            let e = Embedder::load(&path, &m).unwrap();
            let t = e.self_test();
            println!("{}: ok {} | load {:.0} ms | {:.0} ms per face | {} numbers | {:?}", m.id, t.ok, t.load_ms, t.embed_ms, t.dimension, t.checks);
            assert!(t.ok);
        }
    }

    #[test]
    fn bgr_changes_the_input() {
        let p = temp_model("bgr", &crate::synthetic::tiny_embedder_model(16));
        let rgb = Embedder::load(&p, &manifest(16)).unwrap();
        let mut m = manifest(16);
        m.input.colour = crate::manifest::Colour::Bgr;
        let bgr = Embedder::load(&p, &m).unwrap();
        // a picture with different channel levels: swapping the order must change the embedding
        let img: Vec<u8> = (0..112 * 112).flat_map(|_| [200u8, 100, 20]).collect();
        let (a, b) = (rgb.embed(&img).unwrap(), bgr.embed(&img).unwrap());
        let cos: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        assert!(cos < 0.9999, "{cos}");
        let _ = std::fs::remove_file(p);
    }
}
